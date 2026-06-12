//! Structural query model for the no-code builder (slice V4, DV-016).
//!
//! [`parse_query_tree`] turns DocQL source into a span-carrying node tree
//! (elements with attributes + children, Match / TYPE declarations with
//! their rules / fields) plus the first compile diagnostic attributed to a
//! node. It runs the REAL delver-core pipeline — the pest grammar for
//! structure + spans, then `parse_template` for the D-006 fail-loud compile
//! surface — and is pure and target-independent: delver-core is a
//! non-optional viewer dependency, so the same code runs in the wasm client
//! (the builder parses synchronously in-process; no server round trip, see
//! DV-016) and in `cargo test -p viewer --lib`.
//!
//! The second half is the pure edit surface the builder UI drives: byte
//! splices computed from node spans (header regeneration, slot insertion,
//! node deletion, match-rule / TYPE rewrites). Every edit rewrites only the
//! spans it owns; untouched source — including the user's hand formatting —
//! is preserved verbatim.

use crate::snippets::{escape_docql_string, slug_identifier, uniquify};
use delver_core::docql::{parse_template, Rule, TemplateParser};
use pest::iterators::Pair;
use pest::Parser;

// ───────────────────────── model ─────────────────────────

/// Byte span (start, end) into the parsed source.
pub type Span = (usize, usize);

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// `Name(attrs) { children }` — Name kept verbatim (unknown elements
    /// still appear in the tree; they just get no form).
    Element,
    /// `Match<Target> Name { rules }`
    MatchDef,
    /// `TYPE Name AS TABLE ( fields );`
    TypeDef,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryNode {
    pub kind: NodeKind,
    /// Element identifier (`Section`, `Table`, …), match-definition name, or
    /// TYPE name.
    pub name: String,
    /// Whole node, including any body.
    pub span: Span,
    /// Element: identifier + attribute list (everything before the body) —
    /// the splice target for attribute-form edits. Declarations: == `span`.
    pub header_span: Span,
    /// Interior of the `{ … }` body (exclusive of the braces), when present.
    pub body_span: Option<Span>,
    /// MatchDef / TypeDef: span of the declaration's name identifier (rename
    /// splice target).
    pub name_span: Option<Span>,
    /// Element attributes in source order.
    pub attrs: Vec<AttrNode>,
    /// Element children in source order.
    pub children: Vec<QueryNode>,
    /// MatchDef: the `<Target>` type.
    pub match_target: Option<String>,
    /// MatchDef: rule clauses in source order.
    pub rules: Vec<MatchRuleNode>,
    /// TypeDef: `(name, type)` field declarations in source order.
    pub fields: Vec<TypeFieldNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttrNode {
    pub name: String,
    /// Verbatim source slice of the value (quotes included for strings).
    pub value_raw: String,
    pub value: AttrValue,
    /// Whole `name=value`.
    pub span: Span,
    pub value_span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    /// String literal, unescaped.
    Str(String),
    Num(f64),
    Bool(bool),
    Ident(String),
    /// Arrays and anything else: carried raw only.
    Other,
}

impl AttrValue {
    /// The value as display text (string contents unquoted, identifiers and
    /// numbers verbatim); `None` for `Other`.
    pub fn display(&self) -> Option<String> {
        match self {
            AttrValue::Str(s) => Some(s.clone()),
            AttrValue::Num(n) => Some(fmt_num(*n)),
            AttrValue::Bool(b) => Some(b.to_string()),
            AttrValue::Ident(i) => Some(i.clone()),
            AttrValue::Other => None,
        }
    }
}

/// One clause of a Match definition body. `function`/`source` always carry
/// the verbatim shape; the typed fields are best-effort extraction for the
/// rule form (unsupported shapes render read-only from `source`).
#[derive(Debug, Clone, PartialEq)]
pub struct MatchRuleNode {
    /// `Text` / `Regex` / `Heuristic` / `EmbeddingSim` / `FirstMatch` / other.
    pub function: String,
    pub span: Span,
    /// Verbatim source slice of the clause.
    pub source: String,
    /// First positional string argument, unescaped (Text/Regex/EmbeddingSim).
    pub pattern: Option<String>,
    pub threshold: Option<f64>,
    pub endpoint: Option<String>,
    /// Heuristic comparisons as raw `(property, op, value)` texts.
    pub comparisons: Vec<HeuristicRow>,
    /// FirstMatch alternatives.
    pub nested: Vec<MatchRuleNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeuristicRow {
    pub property: String,
    pub op: String,
    /// Verbatim right-hand side (quotes included for strings).
    pub value_raw: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeFieldNode {
    pub name: String,
    pub field_type: String,
}

/// Index path through `QueryTree::nodes` and `QueryNode::children`.
pub type NodePath = Vec<usize>;

#[derive(Debug, Clone, PartialEq)]
pub struct CompileDiag {
    pub message: String,
    /// Node the message names, when one could be attributed (heuristic:
    /// first DFS node whose match/TYPE name, `as=` value, or element
    /// identifier appears single-quoted in the message).
    pub node_path: Option<NodePath>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueryTree {
    /// Top-level items in source order.
    pub nodes: Vec<QueryNode>,
    /// First compile error from `parse_template` (its fail-loud checks stop
    /// at the first problem), when the source parses but does not compile.
    pub compile: Option<CompileDiag>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParseOutcome {
    Tree(QueryTree),
    /// pest syntax error: the tree cannot be built; the UI keeps the last
    /// good tree and banners this.
    SyntaxError { line: usize, col: usize, message: String },
}

// ───────────────────────── parsing ─────────────────────────

/// Parse DocQL source into the span-carrying builder tree (see module docs).
pub fn parse_query_tree(text: &str) -> ParseOutcome {
    let template = match TemplateParser::parse(Rule::template, text) {
        Ok(mut pairs) => match pairs.next() {
            Some(p) => p,
            None => return ParseOutcome::Tree(QueryTree::default()),
        },
        Err(e) => {
            let (line, col) = match e.line_col {
                pest::error::LineColLocation::Pos((l, c)) => (l, c),
                pest::error::LineColLocation::Span((l, c), _) => (l, c),
            };
            return ParseOutcome::SyntaxError {
                line,
                col,
                message: e.variant.message().to_string(),
            };
        }
    };

    let mut nodes = Vec::new();
    for pair in template.into_inner() {
        match pair.as_rule() {
            Rule::expression => {
                if let Some(node) = build_element(pair, text) {
                    nodes.push(node);
                }
            }
            Rule::match_definition => nodes.push(build_match_def(pair, text)),
            Rule::type_definition => nodes.push(build_type_def(pair, text)),
            _ => {}
        }
    }

    let compile = match parse_template(text) {
        Ok(_) => None,
        Err(e) => {
            let message = e.to_string();
            let node_path = attribute_diag(&nodes, &message);
            Some(CompileDiag { message, node_path })
        }
    };

    ParseOutcome::Tree(QueryTree { nodes, compile })
}

fn span_of(pair: &Pair<Rule>) -> Span {
    let s = pair.as_span();
    (s.start(), s.end())
}

/// pest spans absorb the whitespace consumed while attempting a trailing
/// optional rule (e.g. an element's missing `element_body`); shrink the end
/// back to the last non-whitespace byte so spans slice exact source text.
fn trim_span(text: &str, (start, mut end): Span) -> Span {
    let bytes = text.as_bytes();
    while end > start && matches!(bytes[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
        end -= 1;
    }
    (start, end)
}

fn build_element(pair: Pair<Rule>, text: &str) -> Option<QueryNode> {
    let element = if pair.as_rule() == Rule::expression {
        pair.into_inner().next()?
    } else {
        pair
    };
    let span = trim_span(text, span_of(&element));

    let mut name = String::new();
    let mut header_end = span.0;
    let mut attrs = Vec::new();
    let mut body_span = None;
    let mut children = Vec::new();

    for inner in element.into_inner() {
        match inner.as_rule() {
            Rule::identifier => {
                name = inner.as_str().to_string();
                header_end = span_of(&inner).1;
            }
            Rule::attributes => {
                header_end = span_of(&inner).1;
                attrs = build_attrs(inner, text);
            }
            Rule::element_body => {
                let (b0, b1) = span_of(&inner);
                body_span = Some((b0 + 1, b1 - 1));
                for expr in inner.into_inner() {
                    if expr.as_rule() == Rule::expression {
                        if let Some(child) = build_element(expr, text) {
                            children.push(child);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Some(QueryNode {
        kind: NodeKind::Element,
        name,
        span,
        header_span: (span.0, header_end),
        body_span,
        name_span: None,
        attrs,
        children,
        match_target: None,
        rules: Vec::new(),
        fields: Vec::new(),
    })
}

fn build_attrs(attributes: Pair<Rule>, text: &str) -> Vec<AttrNode> {
    let mut out = Vec::new();
    for list in attributes.into_inner() {
        if list.as_rule() != Rule::attribute_list {
            continue;
        }
        for attr in list.into_inner() {
            if attr.as_rule() != Rule::attribute {
                continue;
            }
            let span = span_of(&attr);
            let mut inner = attr.into_inner();
            let Some(key) = inner.next() else { continue };
            let Some(value) = inner.next() else { continue };
            let value_span = span_of(&value);
            out.push(AttrNode {
                name: key.as_str().to_string(),
                value_raw: text[value_span.0..value_span.1].to_string(),
                value: build_value(value),
                span,
                value_span,
            });
        }
    }
    out
}

fn build_value(pair: Pair<Rule>) -> AttrValue {
    let inner = if pair.as_rule() == Rule::value {
        match pair.into_inner().next() {
            Some(p) => p,
            None => return AttrValue::Other,
        }
    } else {
        pair
    };
    match inner.as_rule() {
        Rule::string => AttrValue::Str(unescape_docql_string(inner.as_str())),
        Rule::number => inner
            .as_str()
            .parse::<f64>()
            .map(AttrValue::Num)
            .unwrap_or(AttrValue::Other),
        Rule::boolean => AttrValue::Bool(inner.as_str() == "true"),
        Rule::identifier => AttrValue::Ident(inner.as_str().to_string()),
        _ => AttrValue::Other,
    }
}

fn build_match_def(pair: Pair<Rule>, text: &str) -> QueryNode {
    let span = trim_span(text, span_of(&pair));
    let mut target = None;
    let mut name = String::new();
    let mut name_span = None;
    let mut body_span = None;
    let mut rules = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier => {
                if target.is_none() {
                    target = Some(inner.as_str().to_string());
                } else {
                    name = inner.as_str().to_string();
                    name_span = Some(span_of(&inner));
                }
            }
            Rule::match_body => {
                let (b0, b1) = span_of(&inner);
                body_span = Some((b0 + 1, b1 - 1));
                for expr in inner.into_inner() {
                    if expr.as_rule() == Rule::match_expression {
                        if let Some(rule) = build_rule(expr, text) {
                            rules.push(rule);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    QueryNode {
        kind: NodeKind::MatchDef,
        name,
        span,
        header_span: span,
        body_span,
        name_span,
        attrs: Vec::new(),
        children: Vec::new(),
        match_target: target,
        rules,
        fields: Vec::new(),
    }
}

/// `match_expression` (or a bare `function_call` from FirstMatch nesting) →
/// rule node. Bare values become `function: "(value)"` carrying source only.
fn build_rule(pair: Pair<Rule>, text: &str) -> Option<MatchRuleNode> {
    let span = trim_span(text, span_of(&pair));
    let source = text[span.0..span.1].to_string();
    let inner = if pair.as_rule() == Rule::match_expression {
        pair.into_inner().next()?
    } else {
        pair
    };
    if inner.as_rule() != Rule::function_call {
        return Some(MatchRuleNode {
            function: "(value)".to_string(),
            span,
            source,
            pattern: None,
            threshold: None,
            endpoint: None,
            comparisons: Vec::new(),
            nested: Vec::new(),
        });
    }

    let mut function = String::new();
    let mut pattern = None;
    let mut threshold = None;
    let mut endpoint = None;
    let mut comparisons = Vec::new();
    let mut nested = Vec::new();
    let mut positional_idx = 0usize;

    for part in inner.into_inner() {
        match part.as_rule() {
            Rule::identifier => function = part.as_str().to_string(),
            Rule::function_args => {
                for arg in part.into_inner() {
                    if arg.as_rule() != Rule::function_arg {
                        continue;
                    }
                    // function_arg = (identifier "=" function_arg_value) | function_arg_value
                    let parts: Vec<Pair<Rule>> = arg.into_inner().collect();
                    let (arg_name, value_pair) = match parts.len() {
                        2 => (Some(parts[0].as_str().to_string()), parts[1].clone()),
                        1 => (None, parts[0].clone()),
                        _ => continue,
                    };
                    let value_inner = if value_pair.as_rule() == Rule::function_arg_value {
                        match value_pair.into_inner().next() {
                            Some(p) => p,
                            None => continue,
                        }
                    } else {
                        value_pair
                    };
                    match (arg_name.as_deref(), value_inner.as_rule()) {
                        (None, Rule::value) => {
                            if positional_idx == 0 {
                                if let AttrValue::Str(s) = build_value(value_inner) {
                                    pattern = Some(s);
                                }
                            } else if threshold.is_none() {
                                if let AttrValue::Num(n) = build_value(value_inner) {
                                    threshold = Some(n);
                                }
                            }
                            positional_idx += 1;
                        }
                        (Some("threshold"), Rule::value) => {
                            if let AttrValue::Num(n) = build_value(value_inner) {
                                threshold = Some(n);
                            }
                        }
                        (Some("endpoint"), Rule::value) => {
                            if let AttrValue::Str(s) = build_value(value_inner) {
                                endpoint = Some(s);
                            }
                        }
                        (_, Rule::comparison_expr) => {
                            if let Some(row) = build_comparison(value_inner, text) {
                                comparisons.push(row);
                            }
                        }
                        (_, Rule::function_call) => {
                            if let Some(sub) = build_rule(value_inner, text) {
                                nested.push(sub);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Some(MatchRuleNode {
        function,
        span,
        source,
        pattern,
        threshold,
        endpoint,
        comparisons,
        nested,
    })
}

fn build_comparison(pair: Pair<Rule>, text: &str) -> Option<HeuristicRow> {
    let mut property = None;
    let mut op = None;
    let mut value_raw = None;
    for part in pair.into_inner() {
        match part.as_rule() {
            Rule::identifier => property = Some(part.as_str().to_string()),
            Rule::comparison_op => op = Some(part.as_str().to_string()),
            Rule::comparison_value => {
                let (s0, s1) = span_of(&part);
                value_raw = Some(text[s0..s1].to_string());
            }
            _ => {}
        }
    }
    Some(HeuristicRow {
        property: property?,
        op: op?,
        value_raw: value_raw?,
    })
}

fn build_type_def(pair: Pair<Rule>, text: &str) -> QueryNode {
    let span = trim_span(text, span_of(&pair));
    let mut name = String::new();
    let mut name_span = None;
    let mut fields = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::identifier => {
                name = inner.as_str().to_string();
                name_span = Some(span_of(&inner));
            }
            Rule::field_list => {
                for field in inner.into_inner() {
                    if field.as_rule() != Rule::field_decl {
                        continue;
                    }
                    let mut parts = field.into_inner();
                    let (Some(fname), Some(ftype)) = (parts.next(), parts.next()) else {
                        continue;
                    };
                    fields.push(TypeFieldNode {
                        name: fname.as_str().to_string(),
                        field_type: ftype.as_str().to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    QueryNode {
        kind: NodeKind::TypeDef,
        name,
        span,
        header_span: span,
        body_span: None,
        name_span,
        attrs: Vec::new(),
        children: Vec::new(),
        match_target: None,
        rules: Vec::new(),
        fields,
    }
}

/// Attribute a compile message to a node: collect the `'…'`-quoted tokens
/// and DFS for the first node whose declaration name, `as=` value, or
/// element identifier matches one. `parse_template` errors carry no spans,
/// so this name heuristic is the best available anchor (documented DV-016).
fn attribute_diag(nodes: &[QueryNode], message: &str) -> Option<NodePath> {
    let tokens: Vec<&str> = message
        .split('\'')
        .enumerate()
        .filter_map(|(i, part)| (i % 2 == 1).then_some(part))
        .collect();
    if tokens.is_empty() {
        return None;
    }

    fn matches(node: &QueryNode, tokens: &[&str]) -> bool {
        if tokens.iter().any(|t| *t == node.name) {
            return true;
        }
        node.attrs.iter().any(|a| {
            a.name == "as"
                && matches!(&a.value, AttrValue::Str(s) if tokens.iter().any(|t| t == s))
        })
    }

    fn dfs(nodes: &[QueryNode], tokens: &[&str], base: &mut NodePath) -> Option<NodePath> {
        for (i, node) in nodes.iter().enumerate() {
            base.push(i);
            if matches(node, tokens) {
                return Some(base.clone());
            }
            if let Some(found) = dfs(&node.children, tokens, base) {
                return Some(found);
            }
            base.pop();
        }
        None
    }

    dfs(nodes, &tokens, &mut Vec::new())
}

// ───────────────────────── lookups ─────────────────────────

pub fn node_at<'t>(tree: &'t QueryTree, path: &[usize]) -> Option<&'t QueryNode> {
    let (&first, rest) = path.split_first()?;
    let mut node = tree.nodes.get(first)?;
    for &idx in rest {
        node = node.children.get(idx)?;
    }
    Some(node)
}

/// The attribute value of `name` on `node`, if present.
pub fn attr<'n>(node: &'n QueryNode, name: &str) -> Option<&'n AttrNode> {
    node.attrs.iter().find(|a| a.name == name)
}

/// Top-level Match definition by name.
pub fn match_def_by_name<'t>(tree: &'t QueryTree, name: &str) -> Option<(usize, &'t QueryNode)> {
    tree.nodes
        .iter()
        .enumerate()
        .find(|(_, n)| n.kind == NodeKind::MatchDef && n.name == name)
}

/// Names of all top-level TYPE declarations, in source order.
pub fn type_names(tree: &QueryTree) -> Vec<String> {
    tree.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::TypeDef)
        .map(|n| n.name.clone())
        .collect()
}

// ───────────────────────── slot legality ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    Section,
    TextChunk,
    Table,
    Paragraph,
    Annotation,
    Figure,
    Image,
    MatchDef,
    TypeDef,
    SubCorpus,
}

impl SlotKind {
    pub fn label(self) -> &'static str {
        match self {
            SlotKind::Section => "Section",
            SlotKind::TextChunk => "TextChunk",
            SlotKind::Table => "Table",
            SlotKind::Paragraph => "Paragraph",
            SlotKind::Annotation => "Annotation",
            SlotKind::Figure => "Figure",
            SlotKind::Image => "Image",
            SlotKind::MatchDef => "Match",
            SlotKind::TypeDef => "TYPE",
            SlotKind::SubCorpus => "SubCorpus",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            SlotKind::Section => "a document slice between two headings",
            SlotKind::TextChunk => "split the text in scope into chunks",
            SlotKind::Table => "collect detected tables in scope",
            SlotKind::Paragraph => "paragraph-level text element",
            SlotKind::Annotation => "PDF page annotations in scope",
            SlotKind::Figure => "image + caption groupings in scope",
            SlotKind::Image => "images in scope",
            SlotKind::MatchDef => "a reusable named matcher",
            SlotKind::TypeDef => "a typed table schema",
            SlotKind::SubCorpus => "a corpus-context description",
        }
    }
}

/// The slot-legality table (DV-016): what a slot may insert, by parent.
/// Top level (`None`) takes every element plus the three declarations;
/// Section bodies take the element subset; every other node is a leaf.
pub fn slot_menu(parent_element: Option<&str>) -> &'static [SlotKind] {
    const TOP: &[SlotKind] = &[
        SlotKind::Section,
        SlotKind::TextChunk,
        SlotKind::Table,
        SlotKind::Paragraph,
        SlotKind::Annotation,
        SlotKind::Figure,
        SlotKind::Image,
        SlotKind::MatchDef,
        SlotKind::TypeDef,
        SlotKind::SubCorpus,
    ];
    const SECTION: &[SlotKind] = &[
        SlotKind::Section,
        SlotKind::TextChunk,
        SlotKind::Table,
        SlotKind::Paragraph,
        SlotKind::Annotation,
        SlotKind::Figure,
        SlotKind::Image,
    ];
    match parent_element {
        None => TOP,
        Some("Section") => SECTION,
        Some(_) => &[],
    }
}

/// Whether the builder offers child slots inside this node.
pub fn has_child_slots(node: &QueryNode) -> bool {
    node.kind == NodeKind::Element && !slot_menu(Some(node.name.as_str())).is_empty()
}

// ───────────────────────── text generation ─────────────────────────

/// Byte offset → UTF-16 code-unit offset (CodeMirror 5's `posFromIndex`
/// space). `byte` is clamped to the text length.
pub fn byte_to_utf16(text: &str, byte: usize) -> usize {
    let mut clamped = byte.min(text.len());
    while clamped > 0 && !text.is_char_boundary(clamped) {
        clamped -= 1;
    }
    text[..clamped].encode_utf16().count()
}

/// Render an f64 without trailing noise (whole numbers as integers).
pub fn fmt_num(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

pub fn quote_str(s: &str) -> String {
    format!("\"{}\"", escape_docql_string(s))
}

/// Undo [`quote_str`]: strip the quotes and the two escapes the grammar
/// requires. Other escape sequences are left verbatim (display-only).
pub fn unescape_docql_string(quoted: &str) -> String {
    let inner = quoted
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(quoted);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Default insertion text for a slot menu pick. Generated names are
/// uniquified against the whole buffer (DV-012 conventions).
pub fn insertion_text(kind: SlotKind, buffer: &str) -> String {
    match kind {
        SlotKind::Section => {
            let name = uniquify(buffer, "section1");
            format!("Section(as=\"{name}\") {{\n}}")
        }
        SlotKind::TextChunk => "TextChunk(chunkSize=500, chunkOverlap=150)".to_string(),
        SlotKind::Table => format!("Table(as=\"{}\")", uniquify(buffer, "table1")),
        SlotKind::Paragraph => format!("Paragraph(as=\"{}\")", uniquify(buffer, "paragraph1")),
        SlotKind::Annotation => {
            format!("Annotation(as=\"{}\")", uniquify(buffer, "annotation1"))
        }
        SlotKind::Figure => format!("Figure(as=\"{}\")", uniquify(buffer, "figure1")),
        SlotKind::Image => format!("Image(as=\"{}\")", uniquify(buffer, "image1")),
        SlotKind::MatchDef => {
            let name = uniquify(buffer, "Match1");
            format!("Match<Section> {name} {{\n  Text(\"\", threshold=0.6)\n}}")
        }
        SlotKind::TypeDef => {
            let name = uniquify(buffer, "Type1");
            format!("TYPE {name} AS TABLE (\n  field1 TEXT,\n);")
        }
        SlotKind::SubCorpus => {
            let name = uniquify(buffer, "subcorpus1");
            format!("SubCorpus(description=\"\", as=\"{name}\")")
        }
    }
}

/// A rule as the form edits it (the supported subset; FirstMatch and exotic
/// shapes stay source-only).
#[derive(Debug, Clone, PartialEq)]
pub enum RuleSpec {
    Text { pattern: String, threshold: f64 },
    Regex { pattern: String },
    Heuristic { rows: Vec<HeuristicRow> },
    EmbeddingSim { pattern: String, threshold: f64, endpoint: Option<String> },
}

pub fn render_rule(spec: &RuleSpec) -> String {
    match spec {
        RuleSpec::Text { pattern, threshold } => format!(
            "Text({}, threshold={})",
            quote_str(pattern),
            fmt_num(*threshold)
        ),
        RuleSpec::Regex { pattern } => format!("Regex({})", quote_str(pattern)),
        RuleSpec::Heuristic { rows } => {
            let parts: Vec<String> = rows
                .iter()
                .map(|r| format!("{} {} {}", r.property, r.op, r.value_raw))
                .collect();
            format!("Heuristic({})", parts.join(", "))
        }
        RuleSpec::EmbeddingSim { pattern, threshold, endpoint } => {
            let mut out = format!(
                "EmbeddingSim({}, threshold={}",
                quote_str(pattern),
                fmt_num(*threshold)
            );
            if let Some(e) = endpoint.as_deref().filter(|e| !e.trim().is_empty()) {
                out.push_str(&format!(", endpoint={}", quote_str(e)));
            }
            out.push(')');
            out
        }
    }
}

pub fn render_match_def(name: &str, target: &str, rule: &RuleSpec) -> String {
    format!("Match<{target}> {name} {{\n  {}\n}}", render_rule(rule))
}

pub fn render_type_def(name: &str, fields: &[(String, String)]) -> String {
    let mut lines = String::new();
    for (fname, ftype) in fields {
        lines.push_str(&format!("  {fname} {ftype},\n"));
    }
    format!("TYPE {name} AS TABLE (\n{lines});")
}

/// Auto names for a Section's match definition + `as=` from a picked
/// pattern: `Overview` / `overview` style, uniquified against `buffer`
/// (existing values that already equal the slug are kept stable).
pub fn section_names_for(pattern: &str, buffer: &str, current_as: Option<&str>) -> (String, String) {
    let slug = slug_identifier(pattern);
    let (def_base, as_base) = if slug.is_empty() {
        ("Section1".to_string(), "section1".to_string())
    } else {
        let mut cap = slug.clone();
        if let Some(first) = cap.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        (cap, slug)
    };
    let as_name = if current_as == Some(as_base.as_str()) {
        as_base
    } else {
        uniquify(buffer, &as_base)
    };
    (uniquify(buffer, &def_base), as_name)
}

// ───────────────────────── splicing ─────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Splice {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Apply non-overlapping splices (any order) to `text`.
pub fn apply_splices(text: &str, mut splices: Vec<Splice>) -> String {
    splices.sort_by_key(|s| s.start);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for s in splices {
        if s.start < cursor || s.end > text.len() {
            // Overlapping/out-of-range edit: bail out conservatively.
            return text.to_string();
        }
        out.push_str(&text[cursor..s.start]);
        out.push_str(&s.text);
        cursor = s.end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// One attribute write for header regeneration.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrWrite {
    /// Rendered value text (already quoted/escaped as needed).
    Set(String),
    Remove,
}

/// Regenerated element header (`Name(attrs)`): existing attributes keep
/// their source order and raw values; updates overwrite or remove; new
/// attributes append in `updates` order.
pub fn updated_header(node: &QueryNode, updates: &[(String, AttrWrite)]) -> String {
    let mut attrs: Vec<(String, Option<String>)> = node
        .attrs
        .iter()
        .map(|a| (a.name.clone(), Some(a.value_raw.clone())))
        .collect();
    for (name, write) in updates {
        let existing = attrs.iter_mut().find(|(n, _)| n == name);
        match (existing, write) {
            (Some(slot), AttrWrite::Set(v)) => slot.1 = Some(v.clone()),
            (Some(slot), AttrWrite::Remove) => slot.1 = None,
            (None, AttrWrite::Set(v)) => attrs.push((name.clone(), Some(v.clone()))),
            (None, AttrWrite::Remove) => {}
        }
    }
    let rendered: Vec<String> = attrs
        .into_iter()
        .filter_map(|(n, v)| v.map(|v| format!("{n}={v}")))
        .collect();
    if rendered.is_empty() {
        node.name.clone()
    } else {
        format!("{}({})", node.name, rendered.join(", "))
    }
}

/// Splice for a header update on `node`.
pub fn header_splice(node: &QueryNode, updates: &[(String, AttrWrite)]) -> Splice {
    Splice {
        start: node.header_span.0,
        end: node.header_span.1,
        text: updated_header(node, updates),
    }
}

/// Indentation (spaces/tabs) of the line `offset` sits on.
fn line_indent(text: &str, offset: usize) -> String {
    let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    text[line_start..offset]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Re-indent every line after the first by `indent`.
fn indent_block(block: &str, indent: &str) -> String {
    block.replace('\n', &format!("\n{indent}"))
}

/// Splice inserting `new_text` as top-level node number `index`.
pub fn top_insert_splice(text: &str, tree: &QueryTree, index: usize, new_text: &str) -> Splice {
    if tree.nodes.is_empty() {
        return Splice {
            start: 0,
            end: text.len(),
            text: format!("{new_text}\n"),
        };
    }
    if index == 0 {
        let at = tree.nodes[0].span.0;
        return Splice {
            start: at,
            end: at,
            text: format!("{new_text}\n\n"),
        };
    }
    let prev_end = tree.nodes[index.min(tree.nodes.len()) - 1].span.1;
    Splice {
        start: prev_end,
        end: prev_end,
        text: format!("\n\n{new_text}"),
    }
}

/// Splice inserting `new_text` as child number `index` of `parent`
/// (an element; a body is added when the source has none).
pub fn child_insert_splice(
    text: &str,
    parent: &QueryNode,
    index: usize,
    new_text: &str,
) -> Splice {
    let parent_indent = line_indent(text, parent.span.0);
    let child_indent = format!("{parent_indent}  ");
    let block = indent_block(new_text, &child_indent);

    let Some((b0, b1)) = parent.body_span else {
        // No body in the source: append one after the node.
        return Splice {
            start: parent.span.1,
            end: parent.span.1,
            text: format!(" {{\n{child_indent}{block}\n{parent_indent}}}"),
        };
    };
    if parent.children.is_empty() {
        return Splice {
            start: b0,
            end: b1,
            text: format!("\n{child_indent}{block}\n{parent_indent}"),
        };
    }
    if index == 0 {
        let at = parent.children[0].span.0;
        return Splice {
            start: at,
            end: at,
            text: format!("{block}\n{child_indent}"),
        };
    }
    let prev_end = parent.children[index.min(parent.children.len()) - 1].span.1;
    Splice {
        start: prev_end,
        end: prev_end,
        text: format!("\n{child_indent}{block}"),
    }
}

/// Splice deleting `node`: the span plus its line indentation and one
/// trailing newline (plus one following blank line, absorbing the top-level
/// `\n\n` separator).
pub fn delete_splice(text: &str, node: &QueryNode) -> Splice {
    let mut start = node.span.0;
    let bytes = text.as_bytes();
    while start > 0 && (bytes[start - 1] == b' ' || bytes[start - 1] == b'\t') {
        start -= 1;
    }
    let mut end = node.span.1;
    while end < bytes.len() && (bytes[end] == b' ' || bytes[end] == b'\t') {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b'\n' {
        end += 1;
        let mut probe = end;
        while probe < bytes.len() && (bytes[probe] == b' ' || bytes[probe] == b'\t') {
            probe += 1;
        }
        if probe < bytes.len() && bytes[probe] == b'\n' {
            end = probe + 1;
        }
    }
    Splice {
        start,
        end,
        text: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(text: &str) -> QueryTree {
        match parse_query_tree(text) {
            ParseOutcome::Tree(t) => t,
            ParseOutcome::SyntaxError { message, .. } => {
                panic!("expected tree, got syntax error: {message}")
            }
        }
    }

    const FULL: &str = r#"TYPE Seg AS TABLE (
  metric TEXT,
  y2015 DECIMAL,
);

Match<Section> MDA {
  Text("Management Discussion", threshold=0.6)
}

SubCorpus(description="California auto loans", as="ca_loans")

Section(match=MDA, as="mda") {
  TextChunk(chunkSize=500, chunkOverlap=150)
  Table(as="t", type="Seg")
}"#;

    // ── tree building ──

    #[test]
    fn builds_kinds_names_and_spans() {
        let t = tree(FULL);
        assert_eq!(t.nodes.len(), 4);
        assert_eq!(t.nodes[0].kind, NodeKind::TypeDef);
        assert_eq!(t.nodes[0].name, "Seg");
        assert_eq!(
            t.nodes[0].fields,
            vec![
                TypeFieldNode { name: "metric".into(), field_type: "TEXT".into() },
                TypeFieldNode { name: "y2015".into(), field_type: "DECIMAL".into() },
            ]
        );
        assert_eq!(t.nodes[1].kind, NodeKind::MatchDef);
        assert_eq!(t.nodes[1].name, "MDA");
        assert_eq!(t.nodes[1].match_target.as_deref(), Some("Section"));
        assert_eq!(t.nodes[2].name, "SubCorpus");
        assert_eq!(t.nodes[3].name, "Section");
        assert_eq!(t.nodes[3].children.len(), 2);
        // Spans slice back to the exact source.
        let (s0, s1) = t.nodes[1].span;
        assert!(FULL[s0..s1].starts_with("Match<Section> MDA {"));
        assert!(FULL[s0..s1].ends_with('}'));
        let section = &t.nodes[3];
        let (h0, h1) = section.header_span;
        assert_eq!(&FULL[h0..h1], "Section(match=MDA, as=\"mda\")");
        let chunk = &section.children[0];
        let (c0, c1) = chunk.span;
        assert_eq!(&FULL[c0..c1], "TextChunk(chunkSize=500, chunkOverlap=150)");
        assert!(t.compile.is_none(), "FULL should compile: {:?}", t.compile);
    }

    #[test]
    fn attrs_carry_values_and_value_spans() {
        let t = tree(FULL);
        let table = &t.nodes[3].children[1];
        let as_attr = attr(table, "as").unwrap();
        assert_eq!(as_attr.value, AttrValue::Str("t".into()));
        assert_eq!(as_attr.value_raw, "\"t\"");
        let (v0, v1) = as_attr.value_span;
        assert_eq!(&FULL[v0..v1], "\"t\"");
        let section = &t.nodes[3];
        assert_eq!(
            attr(section, "match").unwrap().value,
            AttrValue::Ident("MDA".into())
        );
        let chunk = &section.children[0];
        assert_eq!(
            attr(chunk, "chunkSize").unwrap().value,
            AttrValue::Num(500.0)
        );
    }

    #[test]
    fn rules_extract_pattern_threshold_endpoint_and_heuristics() {
        let src = r#"Match<Section> M {
  Text("Heading \"X\"", threshold=0.7)
  Regex("M.*A")
  Heuristic(font_size > 14, font_name == "Times-Bold")
  EmbeddingSim("risk factors", threshold=0.8, endpoint="bge")
  FirstMatch(Text("a"), Regex("b"))
}"#;
        let t = tree(src);
        let rules = &t.nodes[0].rules;
        assert_eq!(rules.len(), 5);
        assert_eq!(rules[0].function, "Text");
        assert_eq!(rules[0].pattern.as_deref(), Some("Heading \"X\""));
        assert_eq!(rules[0].threshold, Some(0.7));
        assert_eq!(rules[1].function, "Regex");
        assert_eq!(rules[1].pattern.as_deref(), Some("M.*A"));
        assert_eq!(
            rules[2].comparisons,
            vec![
                HeuristicRow { property: "font_size".into(), op: ">".into(), value_raw: "14".into() },
                HeuristicRow {
                    property: "font_name".into(),
                    op: "==".into(),
                    value_raw: "\"Times-Bold\"".into()
                },
            ]
        );
        assert_eq!(rules[3].endpoint.as_deref(), Some("bge"));
        assert_eq!(rules[4].function, "FirstMatch");
        assert_eq!(rules[4].nested.len(), 2);
        assert_eq!(rules[4].nested[1].function, "Regex");
        // Every rule's source slice round-trips.
        for rule in rules {
            assert_eq!(&src[rule.span.0..rule.span.1], rule.source);
        }
    }

    #[test]
    fn empty_buffer_parses_to_zero_nodes() {
        let t = tree("");
        assert!(t.nodes.is_empty());
        assert!(t.compile.is_none());
        let t = tree("  \n\n");
        assert!(t.nodes.is_empty());
    }

    #[test]
    fn syntax_errors_report_position() {
        match parse_query_tree("Section(match=) {}") {
            ParseOutcome::SyntaxError { line, col, .. } => {
                assert_eq!(line, 1);
                assert!(col > 1);
            }
            other => panic!("expected syntax error, got {other:?}"),
        }
    }

    #[test]
    fn compile_diag_attributes_to_named_node() {
        // type="Nope" is a compile error naming the Table element.
        let t = tree("TextChunk(chunkSize=500)\n\nTable(as=\"t\", type=\"Nope\")");
        let diag = t.compile.expect("compile error expected");
        assert!(diag.message.contains("Nope"));
        assert_eq!(diag.node_path, Some(vec![1]));
        // Unknown match definition referenced from a nested element.
        let t = tree("Section(match=Ghost, as=\"s\") {\n  TextChunk(chunkSize=500)\n}");
        let diag = t.compile.expect("compile error expected");
        assert_eq!(diag.node_path, Some(vec![0]));
    }

    // ── slot legality ──

    #[test]
    fn slot_legality_table() {
        let top = slot_menu(None);
        assert_eq!(top.len(), 10);
        assert!(top.contains(&SlotKind::MatchDef));
        assert!(top.contains(&SlotKind::TypeDef));
        assert!(top.contains(&SlotKind::SubCorpus));
        let section = slot_menu(Some("Section"));
        assert_eq!(section.len(), 7);
        assert!(!section.contains(&SlotKind::MatchDef));
        assert!(!section.contains(&SlotKind::SubCorpus));
        for leaf in ["TextChunk", "Table", "Annotation", "Figure", "Image", "Paragraph", "Whatever"] {
            assert!(slot_menu(Some(leaf)).is_empty(), "{leaf} must be a leaf");
        }
        let t = tree(FULL);
        assert!(has_child_slots(&t.nodes[3]));
        assert!(!has_child_slots(&t.nodes[3].children[0]));
        assert!(!has_child_slots(&t.nodes[1])); // Match def
    }

    // ── insertion text ──

    #[test]
    fn insertion_texts_parse_and_uniquify() {
        for kind in slot_menu(None) {
            let text = insertion_text(*kind, "");
            let t = tree(&text);
            assert_eq!(t.nodes.len(), 1, "{kind:?} inserts one node: {text}");
        }
        // Names uniquify against the buffer.
        let text = insertion_text(SlotKind::Table, "Table(as=\"table1\")");
        assert_eq!(text, "Table(as=\"table1_2\")");
        let text = insertion_text(SlotKind::Section, "section1");
        assert!(text.contains("as=\"section1_2\""));
    }

    // ── header regeneration ──

    #[test]
    fn header_updates_set_remove_and_append() {
        let t = tree("TextChunk(chunkSize=500, chunkOverlap=150)");
        let node = &t.nodes[0];
        let header = updated_header(
            node,
            &[
                ("chunkSize".into(), AttrWrite::Set("800".into())),
                ("method".into(), AttrWrite::Set("\"semantic\"".into())),
                ("chunkOverlap".into(), AttrWrite::Remove),
            ],
        );
        assert_eq!(header, "TextChunk(chunkSize=800, method=\"semantic\")");
        // Removing everything drops the parens.
        let header = updated_header(
            node,
            &[
                ("chunkSize".into(), AttrWrite::Remove),
                ("chunkOverlap".into(), AttrWrite::Remove),
            ],
        );
        assert_eq!(header, "TextChunk");
    }

    #[test]
    fn header_splice_round_trips_through_reparse() {
        let src = "Section(match=MDA, as=\"mda\") {\n  TextChunk(chunkSize=500)\n}\n\nMatch<Section> MDA {\n  Text(\"x\")\n}";
        let t = tree(src);
        let splice = header_splice(
            &t.nodes[0],
            &[("as".into(), AttrWrite::Set("\"renamed\"".into()))],
        );
        let out = apply_splices(src, vec![splice]);
        let t2 = tree(&out);
        assert_eq!(
            attr(&t2.nodes[0], "as").unwrap().value,
            AttrValue::Str("renamed".into())
        );
        // The child and the match definition are untouched.
        assert!(out.contains("TextChunk(chunkSize=500)"));
        assert!(out.contains("Match<Section> MDA {\n  Text(\"x\")\n}"));
    }

    // ── insert / delete splices ──

    #[test]
    fn top_insert_replaces_empty_and_separates_nodes() {
        let empty = tree("");
        let s = top_insert_splice("", &empty, 0, "Table(as=\"t\")");
        assert_eq!(apply_splices("", vec![s]), "Table(as=\"t\")\n");

        let src = "TextChunk(chunkSize=500)";
        let t = tree(src);
        let s = top_insert_splice(src, &t, 1, "Table(as=\"t\")");
        let out = apply_splices(src, vec![s]);
        assert_eq!(out, "TextChunk(chunkSize=500)\n\nTable(as=\"t\")");
        let s = top_insert_splice(src, &t, 0, "Table(as=\"t\")");
        let out = apply_splices(src, vec![s]);
        assert_eq!(out, "Table(as=\"t\")\n\nTextChunk(chunkSize=500)");
        tree(&out); // still parses
    }

    #[test]
    fn child_insert_into_empty_body_and_between_children() {
        let src = "Section(as=\"s\") {\n}";
        let t = tree(src);
        let s = child_insert_splice(src, &t.nodes[0], 0, "TextChunk(chunkSize=500)");
        let out = apply_splices(src, vec![s]);
        assert_eq!(out, "Section(as=\"s\") {\n  TextChunk(chunkSize=500)\n}");
        let t2 = tree(&out);
        assert_eq!(t2.nodes[0].children.len(), 1);

        // Append after the existing child.
        let s = child_insert_splice(&out, &t2.nodes[0], 1, "Table(as=\"t\")");
        let out2 = apply_splices(&out, vec![s]);
        assert_eq!(
            out2,
            "Section(as=\"s\") {\n  TextChunk(chunkSize=500)\n  Table(as=\"t\")\n}"
        );
        let t3 = tree(&out2);
        assert_eq!(t3.nodes[0].children[1].name, "Table");

        // Prepend.
        let s = child_insert_splice(&out2, &t3.nodes[0], 0, "Paragraph(as=\"p\")");
        let out3 = apply_splices(&out2, vec![s]);
        let t4 = tree(&out3);
        assert_eq!(t4.nodes[0].children[0].name, "Paragraph");
        assert_eq!(t4.nodes[0].children.len(), 3);
    }

    #[test]
    fn child_insert_adds_missing_body_and_indents_nested() {
        let src = "Section(as=\"s\")";
        let t = tree(src);
        let s = child_insert_splice(src, &t.nodes[0], 0, "TextChunk(chunkSize=500)");
        let out = apply_splices(src, vec![s]);
        assert_eq!(out, "Section(as=\"s\") {\n  TextChunk(chunkSize=500)\n}");

        // Nested section: multi-line insert re-indents its continuation lines.
        let src = "Section(as=\"outer\") {\n  Section(as=\"inner\") {\n  }\n}";
        let t = tree(src);
        let inner = &t.nodes[0].children[0];
        let s = child_insert_splice(src, inner, 0, "Section(as=\"deep\") {\n}");
        let out = apply_splices(src, vec![s]);
        assert!(out.contains("    Section(as=\"deep\") {\n    }"), "got: {out}");
        tree(&out);
    }

    #[test]
    fn delete_splice_removes_node_and_separator() {
        let src = "TextChunk(chunkSize=500)\n\nTable(as=\"t\")\n\nParagraph(as=\"p\")";
        let t = tree(src);
        let out = apply_splices(src, vec![delete_splice(src, &t.nodes[1])]);
        assert_eq!(out, "TextChunk(chunkSize=500)\n\nParagraph(as=\"p\")");
        // Child deletion keeps the body shape.
        let src = "Section(as=\"s\") {\n  TextChunk(chunkSize=500)\n  Table(as=\"t\")\n}";
        let t = tree(src);
        let out = apply_splices(src, vec![delete_splice(src, &t.nodes[0].children[0])]);
        assert_eq!(out, "Section(as=\"s\") {\n  Table(as=\"t\")\n}");
        tree(&out);
    }

    // ── rule + declaration rendering ──

    #[test]
    fn rendered_rules_parse_back() {
        let specs = vec![
            RuleSpec::Text { pattern: "OVER\"VIEW".into(), threshold: 0.75 },
            RuleSpec::Regex { pattern: "M.*A".into() },
            RuleSpec::Heuristic {
                rows: vec![
                    HeuristicRow { property: "font_size".into(), op: ">=".into(), value_raw: "14".into() },
                    HeuristicRow { property: "text".into(), op: "==".into(), value_raw: "\"X\"".into() },
                ],
            },
            RuleSpec::EmbeddingSim {
                pattern: "risk".into(),
                threshold: 0.8,
                endpoint: Some("bge".into()),
            },
        ];
        for spec in &specs {
            let def = render_match_def("M1", "Section", spec);
            let t = tree(&def);
            assert_eq!(t.nodes[0].kind, NodeKind::MatchDef);
            assert_eq!(t.nodes[0].rules.len(), 1, "spec {spec:?} → {def}");
        }
        // Text round-trip preserves pattern + threshold through the parser.
        let def = render_match_def("M1", "Section", &specs[0]);
        let t = tree(&def);
        assert_eq!(t.nodes[0].rules[0].pattern.as_deref(), Some("OVER\"VIEW"));
        assert_eq!(t.nodes[0].rules[0].threshold, Some(0.75));
    }

    #[test]
    fn rendered_type_defs_parse_and_compile() {
        let def = render_type_def(
            "TableP26",
            &[("metric".into(), "TEXT".into()), ("c2015".into(), "DECIMAL".into())],
        );
        let src = format!("{def}\n\nTable(as=\"t\", type=\"TableP26\")");
        let t = tree(&src);
        assert!(t.compile.is_none(), "should compile: {:?}", t.compile);
        assert_eq!(t.nodes[0].fields.len(), 2);
    }

    #[test]
    fn section_names_derive_from_pattern_and_stay_stable() {
        let (def, as_name) = section_names_for("PERFORMANCE BY BUSINESS SEGMENT", "", None);
        assert_eq!(def, "Performance_by_business_segmen"); // 30-char slug cap
        assert_eq!(as_name, "performance_by_business_segmen");
        // Re-picking the same heading keeps the existing as= name.
        let buffer = "Section(as=\"overview\")";
        let (_, as_name) = section_names_for("Overview", buffer, Some("overview"));
        assert_eq!(as_name, "overview");
        // Without the current marker it would uniquify away from the buffer.
        let (_, as_name) = section_names_for("Overview", buffer, None);
        assert_eq!(as_name, "overview_2");
        let (def, as_name) = section_names_for("—", "", None);
        assert_eq!((def.as_str(), as_name.as_str()), ("Section1", "section1"));
    }

    #[test]
    fn lookups_find_defs_and_types() {
        let t = tree(FULL);
        assert_eq!(match_def_by_name(&t, "MDA").map(|(i, _)| i), Some(1));
        assert!(match_def_by_name(&t, "Ghost").is_none());
        assert_eq!(type_names(&t), vec!["Seg".to_string()]);
        assert_eq!(node_at(&t, &[3, 1]).map(|n| n.name.as_str()), Some("Table"));
        assert!(node_at(&t, &[9]).is_none());
    }
}
