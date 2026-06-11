use crate::chunker::{chunk_semantic, chunk_text_elements, ChunkMethod, ChunkingStrategy};
use crate::matcher::{MatchedContent, TemplateContentMatch};
use crate::parse::{PageContent, TextElement};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use image;
use log::{error, info, warn};
use lopdf::Object;
use pest::iterators::Pair;
use pest::Parser as PestParser;
use pest_derive::Parser as PestParserDerive;
use serde::Serialize;
use serde_json;
use std::io::ErrorKind;
use std::sync::{Arc, Weak};
use std::{collections::HashMap, io::Error}; // Add image crate import
use tokenizers::Tokenizer;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum EmbeddingModel {
    Clip,
    Unknown(String),
}

impl From<&str> for EmbeddingModel {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "clip" => EmbeddingModel::Clip,
            _ => EmbeddingModel::Unknown(s.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LLMConfig {
    pub model: String,
    pub prompt: String,
    pub target_schema: Option<String>,
}

#[derive(PestParserDerive)]
#[grammar = "docql.pest"]
pub struct TemplateParser;

#[derive(Debug)]
pub struct Root {
    pub elements: Vec<Element>,
    pub match_definitions: HashMap<String, MatchDefinition>,
    /// User-defined table types (Stage C, D-021): `TYPE Name AS TABLE (...);`
    /// declarations, validated (duplicates, field types) at template compile.
    pub types: HashMap<String, Arc<crate::udt::TableTypeDef>>,
    /// SubCorpus registry (Stage C, D-022): `as` name -> `description`.
    /// SubCorpus declarations are extracted from the element tree at template
    /// compile; they never participate in matching.
    pub sub_corpora: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Element {
    pub name: String,
    pub element_type: ElementType,
    pub attributes: HashMap<String, Value>,
    pub children: Vec<Element>,
    pub parent: Option<Weak<Element>>,
    pub prev_sibling: Option<Weak<Element>>,
    pub next_sibling: Option<Weak<Element>>,
    pub match_config: Option<MatchConfig>,
    pub end_match_config: Option<MatchConfig>,
    /// Resolved `type="Name"` attribute of a Table element (Stage C, D-021):
    /// the user-defined type the matched tables coerce into. Resolved at
    /// template compile (undefined name / non-Table carrier are hard errors),
    /// mirroring the `match_config` resolved-at-compile pattern.
    pub table_type: Option<Arc<crate::udt::TableTypeDef>>,
}

impl Element {
    pub fn new(name: String, element_type: ElementType) -> Self {
        Element {
            name,
            element_type,
            attributes: HashMap::new(),
            children: Vec::new(),
            parent: None,
            prev_sibling: None,
            next_sibling: None,
            match_config: None,
            end_match_config: None,
            table_type: None,
        }
    }

    pub fn previous_sibling(&self) -> Option<Arc<Element>> {
        self.prev_sibling.as_ref().and_then(|w| w.upgrade())
    }

    pub fn next_sibling(&self) -> Option<Arc<Element>> {
        self.next_sibling.as_ref().and_then(|w| w.upgrade())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Value {
    String(String),
    Number(i64),
    Boolean(bool),
    Array(Vec<Value>),
    Identifier(String),
}

#[derive(Debug)]
pub struct DocumentElement {
    pub element_type: ElementType,
    pub text: Option<String>,
    pub children: Vec<DocumentElement>,
    pub metadata: HashMap<String, String>, // Additional info like font size, position
}

/// Types of elements that can be matched against the document
/// Implements PartialEq and PartialOrd for sorting so that
/// elements are matched in a deterministic order.
/// Elements which can contain other elements should be enumerated first.
#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub enum ElementType {
    Section,
    Paragraph,
    TextChunk,
    Table,
    Image,
    /// PDF page annotation selector (D-016)
    Annotation,
    /// Image + caption grouping selector (D-016)
    Figure,
    // Image-specific processing children
    ImageSummary,
    ImageBytes,
    ImageCaption,
    ImageEmbedding,
    /// Corpus-context declaration (Stage C, D-022). Extracted into
    /// `Root::sub_corpora` at template compile; never matched.
    SubCorpus,
    Unknown,
    // Add other types as needed
}

#[derive(Debug)]
pub struct MatchedElement {
    pub template_element: Element,
    pub document_element: DocumentElement,
    pub children: Vec<MatchedElement>,
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, serde::Serialize, Clone)]
pub struct ChunkOutput {
    pub text: String,
    pub metadata: HashMap<String, serde_json::Value>,
    pub chunk_index: usize,
    pub parent_name: Option<String>, // Immediate containing section name
    pub parent_index: Option<usize>, // Immediate containing section index
}

#[derive(Debug, serde::Serialize, Clone)]
pub struct ImageOutput {
    pub id: String, // Use UUID as String for JSON compatibility
    pub page_number: u32,
    pub bbox: (f32, f32, f32, f32),
    pub caption: Option<String>,
    pub bytes_base64: Option<String>,
    pub summary: Option<String>,
    pub embedding: Option<Vec<f32>>, // Assuming embedding is Vec<f32>
    pub metadata: HashMap<String, serde_json::Value>,
    pub parent_name: Option<String>, // Immediate containing section name
    pub parent_index: Option<usize>, // Immediate containing section index
}

/// Output for a matched PDF page annotation (D-016); follows ImageOutput's
/// shape: type tag, element data, page/bbox, merged metadata, parent link.
#[derive(Debug, serde::Serialize, Clone)]
pub struct AnnotationOutput {
    pub id: String,
    pub page_number: u32,
    pub bbox: (f32, f32, f32, f32),
    /// Annotation `Contents` text, when present.
    pub text: Option<String>,
    /// Inherited template metadata merged with the annotation's own
    /// metadata (subtype, uri/dest).
    pub metadata: HashMap<String, serde_json::Value>,
    pub parent_name: Option<String>,
    pub parent_index: Option<usize>,
}

/// Output for a matched figure grouping (D-016): an image plus its caption.
#[derive(Debug, serde::Serialize, Clone)]
pub struct FigureOutput {
    pub id: String,
    pub page_number: u32,
    /// Union of the grouped image and caption bboxes.
    pub bbox: (f32, f32, f32, f32),
    pub caption: Option<String>,
    /// Element id of the grouped image (the figure→image "contains" edge).
    pub image_id: Option<String>,
    pub metadata: HashMap<String, serde_json::Value>,
    pub parent_name: Option<String>,
    pub parent_index: Option<usize>,
}

/// Output for a matched detected table (Stage B slice 3, D-018).
/// `header`/`rows` are the convenience text view (header row when one was
/// detected, body rows otherwise); `cells` carries the full per-cell
/// structure. `name` is the `as="..."` value, falling back to the template
/// element name. Field `page` (not `page_number`) per the slice spec.
#[derive(Debug, serde::Serialize, Clone)]
pub struct TableOutput {
    pub name: String,
    pub page: u32,
    pub bbox: (f32, f32, f32, f32),
    pub n_rows: u32,
    pub n_cols: u32,
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub cells: Vec<crate::table::TableCell>,
    pub strategy: crate::table::TableStrategy,
    pub confidence: f64,
    /// Inherited template metadata (section attribution etc.), as for the
    /// sibling output types.
    pub metadata: HashMap<String, serde_json::Value>,
    pub parent_name: Option<String>,
    pub parent_index: Option<usize>,
}

/// Where a typed table's records came from (Stage C, D-021): the detected
/// table element (id/page/bbox) plus the grid row index of every record
/// (parallel to `records`; rows address the same grid `table_cells` uses).
#[derive(Debug, serde::Serialize, Clone)]
pub struct TypedTableProvenance {
    pub element_id: String,
    pub page: u32,
    pub bbox: (f32, f32, f32, f32),
    pub source_rows: Vec<u32>,
}

/// Output for a matched table coerced into a user-defined type (Stage C,
/// D-021): `Table(as="...", type="TypeName")`. `records` are `{field: value}`
/// objects (fields in declared order, null where the cell was empty or
/// uncoercible); `errors` + `coerced_ok`/`coerced_err` carry the data-quality
/// signal (a failed cell never aborts the run — template misuse does, at
/// compile time). Cells whose trailing `%` was stripped are listed in
/// metadata `percent_cells` as `{row, field}`. Tables without `type=` keep
/// emitting plain [`TableOutput`], byte-identically.
#[derive(Debug, serde::Serialize, Clone)]
pub struct TypedTableOutput {
    pub name: String,
    pub type_name: String,
    pub records: Vec<serde_json::Map<String, serde_json::Value>>,
    pub errors: Vec<crate::udt::CellError>,
    pub coerced_ok: u32,
    pub coerced_err: u32,
    pub provenance: TypedTableProvenance,
    pub metadata: HashMap<String, serde_json::Value>,
    pub parent_name: Option<String>,
    pub parent_index: Option<usize>,
}

#[derive(Debug, serde::Serialize, Clone)]
#[serde(tag = "type")] // Add type field for distinguishing in JSON
pub enum ProcessedOutput {
    Text(ChunkOutput),
    Image(ImageOutput),
    Annotation(AnnotationOutput),
    Figure(FigureOutput),
    Table(TableOutput),
    TypedTable(TypedTableOutput),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatchType {
    /// Simple fuzzy text matching (normalized Levenshtein)
    Text,
    /// Vector embedding cosine similarity. Canonical syntax is
    /// `EmbeddingSim("query", threshold=0.6, endpoint="databricks-bge")`;
    /// `Cosine(...)` and `Semantic(...)` parse as aliases (D-014).
    EmbeddingSim,
    /// Regular expression matching
    Regex,
    /// Element-property comparisons (e.g. `fontSize > 14`), ANDed together
    Heuristic(Vec<ComparisonExpr>),
    /// Try alternatives in order; the first that yields any candidates wins
    FirstMatch(Vec<MatchConfig>),
    /// Unknown type from the array form; rejected at template compile (D-006)
    Custom(String),
}

#[derive(Debug, Clone)]
pub struct MatchConfig {
    pub match_type: MatchType,
    pub pattern: String,                 // Text to match, regex pattern, or embedding query
    pub threshold: f64,                  // Similarity threshold (0.0-1.0)
    pub endpoint: Option<String>,        // EmbeddingSim: serving endpoint name or URL
    pub model: Option<String>,           // EmbeddingSim: optional model hint
    pub name: Option<String>,            // Source match-definition name (diagnostics)
    pub compiled_regex: Option<regex::Regex>, // Regex: compiled at template compile time
    pub options: HashMap<String, Value>, // Additional match-specific options
}

/// `compiled_regex` is a cache of `pattern` and intentionally excluded.
impl PartialEq for MatchConfig {
    fn eq(&self, other: &Self) -> bool {
        self.match_type == other.match_type
            && self.pattern == other.pattern
            && self.threshold == other.threshold
            && self.endpoint == other.endpoint
            && self.model == other.model
            && self.name == other.name
            && self.options == other.options
    }
}

impl Default for MatchConfig {
    fn default() -> Self {
        Self {
            match_type: MatchType::Text,
            pattern: String::new(),
            threshold: 0.6,
            endpoint: None,
            model: None,
            name: None,
            compiled_regex: None,
            options: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MatchDefinition {
    pub target_type: String,           // e.g., "Section"
    pub name: String,                  // e.g., "MDandA"
    pub clauses: Vec<MatchExpression>, // Ordered list of match expressions
}

#[derive(Debug, Clone)]
pub enum MatchExpression {
    FunctionCall(FunctionCall),
    MatchConfig(MatchConfig),
    Value(Value),
}

#[derive(Debug, Clone)]
pub struct FunctionCall {
    pub name: String,           // e.g., "FirstMatch", "Text", "Cosine"
    pub args: Vec<FunctionArg>, // Positional and named arguments
}

#[derive(Debug, Clone)]
pub enum FunctionArg {
    Positional(FunctionArgValue),
    Named {
        name: String,
        value: FunctionArgValue,
    },
}

#[derive(Debug, Clone)]
pub enum FunctionArgValue {
    Value(Value),
    Comparison(ComparisonExpr),
    FunctionCall(FunctionCall),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ComparisonOp {
    GreaterThan,
    LessThan,
    GreaterThanOrEqual,
    LessThanOrEqual,
    Equal,
    NotEqual,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ComparisonValue {
    String(String),
    Number(f64),
    Boolean(bool),
    Identifier(String),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ComparisonExpr {
    pub left: String, // identifier (e.g., "fontSize")
    pub op: ComparisonOp,
    pub right: ComparisonValue,
}

/// Element properties that `Heuristic(...)` comparisons may reference
/// (D-014). Derived from what the index actually stores per text element:
/// font size/name, page number, bbox corners, and text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeuristicProperty {
    /// `fontSize` / `font_size`
    FontSize,
    /// `fontName` / `font_name` (string; equality compares canonicalized names)
    FontName,
    /// `page` / `page_number`
    Page,
    /// `x0` / `x` / `x_position` (left edge of bbox)
    X0,
    /// `y0` / `y` / `y_position` (bottom edge of bbox)
    Y0,
    /// `x1` (right edge of bbox)
    X1,
    /// `y1` (top edge of bbox)
    Y1,
    /// `textLength` / `text_length` (character count)
    TextLength,
    /// `text` (string; exact equality)
    Text,
}

impl HeuristicProperty {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "fontSize" | "font_size" => Some(Self::FontSize),
            "fontName" | "font_name" => Some(Self::FontName),
            "page" | "page_number" => Some(Self::Page),
            "x0" | "x" | "x_position" => Some(Self::X0),
            "y0" | "y" | "y_position" => Some(Self::Y0),
            "x1" => Some(Self::X1),
            "y1" => Some(Self::Y1),
            "textLength" | "text_length" => Some(Self::TextLength),
            "text" => Some(Self::Text),
            _ => None,
        }
    }

    /// String properties only support `==` / `!=`; the rest are numeric.
    pub fn is_string(self) -> bool {
        matches!(self, Self::FontName | Self::Text)
    }

    /// Human-readable list of every accepted property name, for error
    /// messages (D-006: unknown property is a hard error listing these).
    pub fn supported_list() -> &'static str {
        "fontSize/font_size, fontName/font_name, page/page_number, \
         x0/x/x_position, y0/y/y_position, x1, y1, textLength/text_length, text"
    }
}

impl Value {
    // Add a helper to extract match config from attributes
    pub fn as_match_config(&self) -> Option<MatchConfig> {
        if let Value::String(s) = self {
            Some(MatchConfig {
                pattern: s.clone(),
                ..MatchConfig::default()
            })
        } else if let Value::Array(values) = self {
            if values.len() >= 2 {
                let pattern = values[0].as_string()?;
                let threshold = values[1].as_number().map_or(600, |n| n) as f64 / 1000.0;

                let match_type = if values.len() >= 3 {
                    match values[2].as_string() {
                        Some(t) if t == "semantic" || t == "cosine" || t == "embedding_sim" => {
                            MatchType::EmbeddingSim
                        }
                        Some(t) if t == "regex" => MatchType::Regex,
                        Some(t) => MatchType::Custom(t),
                        None => MatchType::Text,
                    }
                } else {
                    MatchType::Text
                };

                let mut options = HashMap::new();
                if values.len() >= 4 {
                    if let Value::Array(opts) = &values[3] {
                        for i in (0..opts.len()).step_by(2) {
                            if i + 1 < opts.len() {
                                if let (Some(key), value) = (opts[i].as_string(), &opts[i + 1]) {
                                    options.insert(key, (*value).clone());
                                }
                            }
                        }
                    }
                }

                Some(MatchConfig {
                    match_type,
                    pattern,
                    threshold,
                    options,
                    ..MatchConfig::default()
                })
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn as_string(&self) -> Option<String> {
        match self {
            Value::String(s) => Some(s.clone()),
            Value::Identifier(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<i64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_boolean(&self) -> Option<bool> {
        match self {
            Value::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<Vec<Value>> {
        match self {
            Value::Array(a) => Some(a.clone()),
            _ => None,
        }
    }
}

pub fn parse_template(template_str: &str) -> Result<Root, Error> {
    info!("Parsing template: {}", template_str);
    let pairs = match TemplateParser::parse(Rule::template, template_str) {
        Ok(mut pairs) => pairs.next().unwrap(),
        Err(e) => {
            error!("Failed to parse template: {}", e);
            return Err(Error::new(ErrorKind::InvalidData, e.to_string()));
        }
    };
    let (mut root, raw_types) = _parse_template(pairs);
    // Template-compile-time fail-loud (D-006/D-014/D-021/D-022): every
    // declared match clause must be executable, every referenced definition
    // must exist, regex patterns / heuristic properties are checked (and
    // regexes cached), TYPE declarations are validated and resolved onto
    // their Table elements, and SubCorpus declarations are extracted and
    // interpolated into TextChunk template attributes — all before any
    // matching starts.
    root.types = compile_type_definitions(raw_types)?;
    collect_sub_corpora(&mut root)?;
    validate_match_definitions(&root.match_definitions)?;
    resolve_match_configs(&mut root.elements, &root.match_definitions)?;
    resolve_table_types(&mut root.elements, &root.types)?;
    resolve_chunk_templates(&mut root.elements, &root.sub_corpora)?;
    compile_match_configs(&mut root.elements)?;
    Ok(root)
}

/// Raw `TYPE Name AS TABLE ( field TYPE, ... );` declaration as parsed;
/// validated into a [`crate::udt::TableTypeDef`] by
/// [`compile_type_definitions`].
#[derive(Debug)]
struct RawTypeDef {
    name: String,
    fields: Vec<(String, String)>,
}

/// Validate TYPE declarations (Stage C, D-021): duplicate names and
/// unsupported field types are hard template-compile errors (D-006).
fn compile_type_definitions(
    raw: Vec<RawTypeDef>,
) -> Result<HashMap<String, Arc<crate::udt::TableTypeDef>>, Error> {
    let mut types = HashMap::new();
    for def in raw {
        let mut fields = Vec::with_capacity(def.fields.len());
        let mut seen_fields = std::collections::HashSet::new();
        for (field_name, type_name) in def.fields {
            let Some(field_type) = crate::udt::FieldType::parse(&type_name) else {
                return Err(template_error(format!(
                    "TYPE {}: field '{}' has unsupported type '{}'; supported types: {}",
                    def.name,
                    field_name,
                    type_name,
                    crate::udt::FieldType::SUPPORTED
                )));
            };
            if !seen_fields.insert(field_name.clone()) {
                return Err(template_error(format!(
                    "TYPE {}: duplicate field name '{}'",
                    def.name, field_name
                )));
            }
            fields.push(crate::udt::FieldDef {
                name: field_name,
                field_type,
            });
        }
        if types
            .insert(
                def.name.clone(),
                Arc::new(crate::udt::TableTypeDef {
                    name: def.name.clone(),
                    fields,
                }),
            )
            .is_some()
        {
            return Err(template_error(format!(
                "duplicate TYPE definition '{}'",
                def.name
            )));
        }
    }
    Ok(types)
}

/// Extract `SubCorpus(description="...", as="name")` declarations into
/// `Root::sub_corpora` (Stage C, D-022). Declarations must sit at template
/// top level, carry both attributes, take no body, and use unique names —
/// anything else is a template-compile error (D-006).
fn collect_sub_corpora(root: &mut Root) -> Result<(), Error> {
    let mut kept = Vec::with_capacity(root.elements.len());
    for element in root.elements.drain(..) {
        if element.element_type != ElementType::SubCorpus {
            // Nested SubCorpus declarations are scope creep with unclear
            // semantics; reject them where they hide (D-006).
            fn find_nested(children: &[Element]) -> Option<&Element> {
                children.iter().find_map(|c| {
                    if c.element_type == ElementType::SubCorpus {
                        Some(c)
                    } else {
                        find_nested(&c.children)
                    }
                })
            }
            if find_nested(std::slice::from_ref(&element)).is_some() {
                return Err(template_error(
                    "SubCorpus(...) must be declared at template top level".to_string(),
                ));
            }
            kept.push(element);
            continue;
        }
        if !element.children.is_empty() {
            return Err(template_error(
                "SubCorpus(...) does not take a body".to_string(),
            ));
        }
        let name = element
            .attributes
            .get("as")
            .and_then(|v| v.as_string())
            .ok_or_else(|| {
                template_error(
                    "SubCorpus(...) requires an as=\"name\" attribute".to_string(),
                )
            })?;
        let description = element
            .attributes
            .get("description")
            .and_then(|v| v.as_string())
            .ok_or_else(|| {
                template_error(format!(
                    "SubCorpus '{name}': requires a description=\"...\" attribute"
                ))
            })?;
        if root.sub_corpora.insert(name.clone(), description).is_some() {
            return Err(template_error(format!(
                "duplicate SubCorpus definition '{name}'"
            )));
        }
    }
    root.elements = kept;
    Ok(())
}

/// Resolve `type="Name"` attributes onto Table elements (Stage C, D-021).
/// Both `type="Name"` and the bare-identifier form `type=Name` are accepted
/// (the grammar's attributes are strictly `key=value`, so a positional
/// identifier cannot carry the type — documented choice). `type=` on any
/// non-Table element and references to undefined TYPEs are template misuse:
/// hard compile-time errors (D-006), as opposed to per-cell coercion
/// failures, which are data-quality issues handled at extraction time.
fn resolve_table_types(
    elements: &mut [Element],
    types: &HashMap<String, Arc<crate::udt::TableTypeDef>>,
) -> Result<(), Error> {
    for element in elements {
        if let Some(value) = element.attributes.get("type") {
            if element.element_type != ElementType::Table {
                return Err(template_error(format!(
                    "element '{}': type= is only supported on Table elements",
                    element.name
                )));
            }
            let name = value.as_string().ok_or_else(|| {
                template_error(format!(
                    "Table '{}': type= must be a TYPE name (string or identifier)",
                    element.name
                ))
            })?;
            let def = types.get(&name).ok_or_else(|| {
                let mut known: Vec<&str> = types.keys().map(String::as_str).collect();
                known.sort_unstable();
                template_error(format!(
                    "Table '{}': type=\"{}\" references undefined TYPE; defined types: {}",
                    element.name,
                    name,
                    if known.is_empty() {
                        "(none)".to_string()
                    } else {
                        known.join(", ")
                    }
                ))
            })?;
            element.table_type = Some(Arc::clone(def));
        }
        resolve_table_types(&mut element.children, types)?;
    }
    Ok(())
}

/// Validate and pre-resolve `template="..."` interpolation on TextChunk
/// elements (Stage C, D-022). `{name}` placeholders referencing a SubCorpus
/// resolve to its description **at template compile** (they are constants);
/// `{text}` stays for chunk time. Unknown `{var}`s and `template=` on any
/// other element type are hard compile errors (D-006). Scope is deliberately
/// TextChunk-only.
fn resolve_chunk_templates(
    elements: &mut [Element],
    sub_corpora: &HashMap<String, String>,
) -> Result<(), Error> {
    for element in elements {
        if let Some(value) = element.attributes.get("template") {
            if element.element_type != ElementType::TextChunk {
                return Err(template_error(format!(
                    "element '{}': template= interpolation is only supported on TextChunk \
                     elements",
                    element.name
                )));
            }
            let Value::String(template) = value else {
                return Err(template_error(format!(
                    "{}: template= must be a string",
                    chunk_owner_label(element)
                )));
            };
            let resolved =
                interpolate_subcorpus_vars(&chunk_owner_label(element), template, sub_corpora)?;
            element
                .attributes
                .insert("template".to_string(), Value::String(resolved));
        }
        resolve_chunk_templates(&mut element.children, sub_corpora)?;
    }
    Ok(())
}

/// Substitute `{name}` -> SubCorpus description, leaving `{text}` verbatim
/// for chunk-time substitution. Unknown variables and unterminated `{` are
/// compile errors listing the known variables (D-006). No escaping in v1: a
/// literal `{` always opens a placeholder.
fn interpolate_subcorpus_vars(
    owner: &str,
    template: &str,
    sub_corpora: &HashMap<String, String>,
) -> Result<String, Error> {
    let known_vars = || {
        let mut known: Vec<&str> = sub_corpora.keys().map(String::as_str).collect();
        known.push("text");
        known.sort_unstable();
        known.join(", ")
    };
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Err(template_error(format!(
                "{owner}: template= has an unterminated '{{' placeholder"
            )));
        };
        let var = &after[..close];
        if var == "text" {
            out.push_str("{text}");
        } else if let Some(description) = sub_corpora.get(var) {
            out.push_str(description);
        } else {
            return Err(template_error(format!(
                "{owner}: template= references unknown variable '{{{var}}}'; known \
                 variables: {}",
                known_vars()
            )));
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn template_error(msg: String) -> Error {
    Error::new(ErrorKind::InvalidData, msg)
}

/// Reject any match-definition clause that cannot execute (D-006). After
/// `process_match_expression`, executable clauses are `MatchConfig`s; any
/// clause still parked as a `FunctionCall` (unknown name, bad arguments,
/// `Optional`) or a bare value would previously be dropped silently.
fn validate_match_definitions(defs: &HashMap<String, MatchDefinition>) -> Result<(), Error> {
    for def in defs.values() {
        for clause in &def.clauses {
            match clause {
                MatchExpression::MatchConfig(config) => {
                    validate_match_config(&def.name, config)?;
                }
                MatchExpression::FunctionCall(fc) => {
                    return Err(unsupported_match_function(&def.name, fc));
                }
                MatchExpression::Value(v) => {
                    return Err(template_error(format!(
                        "match definition '{}': bare value {:?} is not an executable match \
                         clause; use Text(\"...\"), Regex(\"...\"), EmbeddingSim(\"...\"), or \
                         Heuristic(...)",
                        def.name, v
                    )));
                }
            }
        }
    }
    Ok(())
}

fn unsupported_match_function(owner: &str, fc: &FunctionCall) -> Error {
    let msg = match fc.name.as_str() {
        "Optional" => format!(
            "match definition '{owner}': Optional(...) is not yet implemented; \
             remove it or use an executable matcher (D-006: no silent pass-through)"
        ),
        "Text" | "Regex" | "EmbeddingSim" | "Cosine" | "Semantic" => format!(
            "match definition '{owner}': {}(...) requires a quoted string pattern as its \
             first argument",
            fc.name
        ),
        "Heuristic" => format!(
            "match definition '{owner}': Heuristic(...) arguments must be property \
             comparisons like fontSize > 14 (supported properties: {})",
            HeuristicProperty::supported_list()
        ),
        "FirstMatch" => format!(
            "match definition '{owner}': FirstMatch(...) arguments must be nested match \
             functions, e.g. FirstMatch(Text(\"a\"), Regex(\"b\"))"
        ),
        other => format!(
            "match definition '{owner}': unknown match function '{other}'; supported: \
             Text, Regex, EmbeddingSim (aliases: Cosine, Semantic), Heuristic, FirstMatch"
        ),
    };
    template_error(msg)
}

/// Compile-time checks for a single config (no caching; see
/// `compile_match_config` for the element-side pass that also caches).
fn validate_match_config(owner: &str, config: &MatchConfig) -> Result<(), Error> {
    match &config.match_type {
        MatchType::Text | MatchType::EmbeddingSim => Ok(()),
        MatchType::Regex => regex::Regex::new(&config.pattern)
            .map(|_| ())
            .map_err(|e| invalid_regex_error(owner, &config.pattern, &e)),
        MatchType::Heuristic(comps) => {
            for comp in comps {
                validate_comparison(owner, comp)?;
            }
            Ok(())
        }
        MatchType::FirstMatch(subs) => {
            for sub in subs {
                validate_match_config(owner, sub)?;
            }
            Ok(())
        }
        MatchType::Custom(t) => Err(template_error(format!(
            "match '{owner}': unsupported match type '{t}'; supported types: text, regex, \
             semantic/cosine/embedding_sim (D-006: no silent skip)"
        ))),
    }
}

fn invalid_regex_error(owner: &str, pattern: &str, e: &regex::Error) -> Error {
    template_error(format!(
        "match '{owner}': invalid regex pattern '{pattern}': {e}"
    ))
}

fn validate_comparison(owner: &str, comp: &ComparisonExpr) -> Result<(), Error> {
    let Some(prop) = HeuristicProperty::parse(&comp.left) else {
        return Err(template_error(format!(
            "match '{owner}': Heuristic(...) references unknown property '{}'; supported \
             properties: {}",
            comp.left,
            HeuristicProperty::supported_list()
        )));
    };
    match &comp.right {
        ComparisonValue::Number(_) if !prop.is_string() => Ok(()),
        ComparisonValue::String(_) | ComparisonValue::Identifier(_) if prop.is_string() => {
            if matches!(comp.op, ComparisonOp::Equal | ComparisonOp::NotEqual) {
                Ok(())
            } else {
                Err(template_error(format!(
                    "match '{owner}': property '{}' is a string; only == and != are supported",
                    comp.left
                )))
            }
        }
        ComparisonValue::Boolean(_) => Err(template_error(format!(
            "match '{owner}': property '{}' cannot be compared to a boolean (no boolean \
             properties exist; supported properties: {})",
            comp.left,
            HeuristicProperty::supported_list()
        ))),
        other => Err(template_error(format!(
            "match '{owner}': type mismatch comparing {} property '{}' with {:?}",
            if prop.is_string() { "string" } else { "numeric" },
            comp.left,
            other
        ))),
    }
}

/// Validate-and-cache pass over the configs that elements actually carry
/// after resolution: compiles `Regex` patterns into `compiled_regex` and
/// re-checks inline (non-definition) configs that skipped
/// `validate_match_definitions`.
fn compile_match_configs(elements: &mut [Element]) -> Result<(), Error> {
    for element in elements {
        if let Some(config) = element.match_config.as_mut() {
            let owner = config.name.clone().unwrap_or_else(|| element.name.clone());
            compile_match_config(&owner, config)?;
        }
        if let Some(config) = element.end_match_config.as_mut() {
            let owner = config.name.clone().unwrap_or_else(|| element.name.clone());
            compile_match_config(&owner, config)?;
        }
        // Documented D-006 exception (D-018): `Table(model=..., targetSchema=...)`
        // attributes are output *enrichment*, not match/selection semantics —
        // selection correctness is unaffected, so this warns (once, at
        // template compile) instead of erroring; structural extraction
        // proceeds. Stage C wires LLM enrichment.
        if element.element_type == ElementType::Table {
            let unimplemented: Vec<&str> = ["model", "targetSchema"]
                .into_iter()
                .filter(|key| element.attributes.contains_key(*key))
                .collect();
            if !unimplemented.is_empty() {
                tracing::warn!(
                    "Table '{}': attribute(s) {} are not implemented yet; proceeding with \
                     structural extraction only (model= is output enrichment, not selection \
                     semantics — Stage C wires LLM enrichment)",
                    element.name,
                    unimplemented.join(", ")
                );
            }
        }
        // Stage B slice 4 (D-020): chunking attributes fail loud at template
        // compile. `method` selects the chunking strategy wherever text gets
        // chunked — TextChunk/Paragraph elements and a Section's own direct
        // content all route through process_text_chunk_elements_simple.
        if matches!(
            element.element_type,
            ElementType::TextChunk | ElementType::Paragraph | ElementType::Section
        ) {
            validate_chunking_attributes(element)?;
        }
        compile_match_configs(&mut element.children)?;
    }
    Ok(())
}

fn compile_match_config(owner: &str, config: &mut MatchConfig) -> Result<(), Error> {
    match &mut config.match_type {
        MatchType::Regex => {
            let re = regex::Regex::new(&config.pattern)
                .map_err(|e| invalid_regex_error(owner, &config.pattern, &e))?;
            config.compiled_regex = Some(re);
            Ok(())
        }
        MatchType::Heuristic(comps) => {
            for comp in comps.iter() {
                validate_comparison(owner, comp)?;
            }
            Ok(())
        }
        MatchType::FirstMatch(subs) => {
            for sub in subs {
                compile_match_config(owner, sub)?;
            }
            Ok(())
        }
        MatchType::Custom(t) => Err(template_error(format!(
            "match '{owner}': unsupported match type '{t}'; supported types: text, regex, \
             semantic/cosine/embedding_sim (D-006: no silent skip)"
        ))),
        MatchType::Text | MatchType::EmbeddingSim => Ok(()),
    }
}

/// Display label for chunking diagnostics: `TextChunk 'risk'` when an
/// `as="risk"` name is present, else just the element identifier (the
/// TableOutput naming convention, D-018).
fn chunk_owner_label(element: &Element) -> String {
    match element.attributes.get("as").and_then(|v| v.as_string()) {
        Some(name) => format!("{} '{}'", element.name, name),
        None => element.name.clone(),
    }
}

/// Attribute value as the user wrote it, for error messages.
fn format_attr_value(value: &Value) -> String {
    match value {
        Value::String(s) => format!("\"{s}\""),
        Value::Identifier(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Array(_) => "[...]".to_string(),
    }
}

/// Template-compile-time validation of chunking attributes (D-006/D-020):
/// `method` must be a supported strategy name, and `breakpointPercentile`
/// (semantic-only) must be an integer in 0..=100. An integer percentile is
/// deliberate: attribute floats still parse via the legacy x1000 fixed-point
/// encoding, so a fractional threshold attribute would be ambiguous.
fn validate_chunking_attributes(element: &Element) -> Result<(), Error> {
    let method = match element.attributes.get("method") {
        None => None,
        Some(value) => match value.as_string().as_deref().and_then(ChunkMethod::parse) {
            Some(method) => Some(method),
            None => {
                return Err(template_error(format!(
                    "{}: unknown method {}; supported values: {} (D-006: no silent skip)",
                    chunk_owner_label(element),
                    format_attr_value(value),
                    ChunkMethod::SUPPORTED,
                )))
            }
        },
    };
    if let Some(value) = element.attributes.get("breakpointPercentile") {
        if method != Some(ChunkMethod::Semantic) {
            return Err(template_error(format!(
                "{}: breakpointPercentile is only meaningful with method=\"semantic\" \
                 (D-006: no silent no-op)",
                chunk_owner_label(element)
            )));
        }
        match value.as_number() {
            Some(p) if (0..=100).contains(&p) => {}
            _ => {
                return Err(template_error(format!(
                    "{}: breakpointPercentile must be an integer percentile in 0..=100, got {}",
                    chunk_owner_label(element),
                    format_attr_value(value),
                )))
            }
        }
    }
    Ok(())
}

fn _parse_template(pair: Pair<Rule>) -> (Root, Vec<RawTypeDef>) {
    let mut elements = Vec::new();
    let mut match_definitions = HashMap::new();
    let mut raw_types = Vec::new();

    match pair.as_rule() {
        Rule::template => {
            for inner_pair in pair.into_inner() {
                match inner_pair.as_rule() {
                    Rule::expression => {
                        let element = process_element(inner_pair);
                        elements.push(element);
                    }
                    Rule::match_definition => {
                        let match_def = process_match_definition(inner_pair);
                        match_definitions.insert(match_def.name.clone(), match_def);
                    }
                    Rule::type_definition => {
                        raw_types.push(process_type_definition(inner_pair));
                    }
                    Rule::EOI => {}
                    rule => {
                        error!("Unexpected rule in template: {:?}", rule);
                    }
                }
            }
        }
        rule => {
            error!("Expected template rule, got: {:?}", rule);
        }
    }

    (
        Root {
            elements,
            match_definitions,
            types: HashMap::new(),
            sub_corpora: HashMap::new(),
        },
        raw_types,
    )
}

/// `TYPE Name AS TABLE ( field TYPE, ... );` — collect name and raw field
/// pairs; semantic validation happens in [`compile_type_definitions`].
fn process_type_definition(pair: Pair<Rule>) -> RawTypeDef {
    let mut inner = pair.into_inner();
    let name = inner.next().unwrap().as_str().to_string();
    let mut fields = Vec::new();
    if let Some(field_list) = inner.next() {
        for field_decl in field_list.into_inner() {
            if field_decl.as_rule() == Rule::field_decl {
                let mut parts = field_decl.into_inner();
                let field_name = parts.next().unwrap().as_str().to_string();
                let type_name = parts.next().unwrap().as_str().to_string();
                fields.push((field_name, type_name));
            }
        }
    }
    RawTypeDef { name, fields }
}

fn process_element(pair: Pair<Rule>) -> Element {
    // If we receive an expression, get the element inside it
    let element_pair = if pair.as_rule() == Rule::expression {
        pair.into_inner().next().unwrap()
    } else {
        pair
    };

    let mut inner_rules = element_pair.into_inner();
    let identifier = inner_rules.next().unwrap().as_str().to_string();

    // Determine element type based on identifier
    let element_type = match identifier.as_str() {
        "Section" => ElementType::Section,
        "Paragraph" => ElementType::Paragraph,
        "TextChunk" => ElementType::TextChunk,
        "Table" => ElementType::Table,
        "Image" => ElementType::Image,
        "Annotation" => ElementType::Annotation,
        "Figure" => ElementType::Figure,
        "SubCorpus" => ElementType::SubCorpus,
        "ImageSummary" => ElementType::ImageSummary,
        "ImageBytes" => ElementType::ImageBytes,
        "ImageCaption" => ElementType::ImageCaption,
        "ImageEmbedding" => ElementType::ImageEmbedding,
        _ => ElementType::Unknown,
    };

    let mut attributes = HashMap::new();
    let mut children = Vec::new();

    // Process remaining rules
    for inner_pair in inner_rules {
        match inner_pair.as_rule() {
            Rule::attributes => {
                attributes = process_attributes(inner_pair);
            }
            Rule::element_body => {
                for expr in inner_pair.into_inner() {
                    if expr.as_rule() == Rule::expression {
                        children.push(process_element(expr));
                    }
                }
            }
            _ => {}
        }
    }

    Element {
        name: identifier,
        element_type,
        attributes,
        children,
        parent: None,
        prev_sibling: None,
        next_sibling: None,
        match_config: None,
        end_match_config: None,
        table_type: None,
    }
}

fn process_attributes(pair: Pair<Rule>) -> HashMap<String, Value> {
    let mut attributes = HashMap::new();

    for inner_pair in pair.into_inner() {
        if inner_pair.as_rule() == Rule::attribute_list {
            for attr_pair in inner_pair.into_inner() {
                if attr_pair.as_rule() == Rule::attribute {
                    let mut attr_inner = attr_pair.into_inner();
                    let key = attr_inner.next().unwrap().as_str().to_string();
                    let value = process_value(attr_inner.next().unwrap());
                    attributes.insert(key, value);
                }
            }
        }
    }
    attributes
}

fn process_value(pair: Pair<Rule>) -> Value {
    // Get the inner value if this is a value wrapper
    let inner_pair = if pair.as_rule() == Rule::value {
        pair.into_inner().next().unwrap()
    } else {
        pair
    };

    match inner_pair.as_rule() {
        Rule::string => {
            let s = inner_pair.as_str();
            // Remove the surrounding quotes
            Value::String(s[1..s.len() - 1].to_string())
        }
        Rule::number => {
            // Parse as f64 first, then convert to i64 if it's a whole number
            let n = inner_pair.as_str().parse::<f64>().unwrap();
            if n.fract() == 0.0 {
                Value::Number(n as i64)
            } else {
                Value::Number((n * 1000.0) as i64) // Store float as fixed-point with 3 decimal places
            }
        }
        Rule::boolean => {
            let b = inner_pair.as_str().parse::<bool>().unwrap();
            Value::Boolean(b)
        }
        Rule::identifier => Value::Identifier(inner_pair.as_str().to_string()),
        Rule::array => {
            let values: Vec<Value> = inner_pair
                .into_inner()
                .filter(|p| p.as_rule() != Rule::array_values)
                .map(process_value)
                .collect();
            Value::Array(values)
        }

        rule => {
            error!("Unexpected value rule: {:?}", rule);
            Value::String(inner_pair.as_str().to_string())
        }
    }
}

fn process_match_definition(pair: Pair<Rule>) -> MatchDefinition {
    let mut inner_rules = pair.into_inner();

    // Skip "Match" keyword, get target type between < >
    let target_type = inner_rules.next().unwrap().as_str().to_string();
    let name = inner_rules.next().unwrap().as_str().to_string();

    // Process the match body
    let body_pair = inner_rules.next().unwrap(); // match_body
    let mut clauses = Vec::new();

    for expression_pair in body_pair.into_inner() {
        if expression_pair.as_rule() == Rule::match_expression {
            let clause = process_match_expression(expression_pair);
            clauses.push(clause);
        }
    }

    MatchDefinition {
        target_type,
        name,
        clauses,
    }
}

fn resolve_match_configs(
    elements: &mut [Element],
    match_definitions: &HashMap<String, MatchDefinition>,
) -> Result<(), Error> {
    for element in elements {
        // Resolve match_config
        if let Some(match_value) = element.attributes.get("match") {
            element.match_config = Some(resolve_attr_to_match_config(
                &element.name,
                "match",
                match_value,
                match_definitions,
            )?);
        }

        // Resolve end_match_config
        if let Some(end_match_value) = element.attributes.get("end_match") {
            element.end_match_config = Some(resolve_attr_to_match_config(
                &element.name,
                "end_match",
                end_match_value,
                match_definitions,
            )?);
        }

        // Recurse for children
        if !element.children.is_empty() {
            resolve_match_configs(&mut element.children, match_definitions)?;
        }
    }
    Ok(())
}

/// Resolve a `match=` / `end_match=` attribute value to a config, failing
/// loud (D-006) where the old code silently left the element configless.
fn resolve_attr_to_match_config(
    element_name: &str,
    attr: &str,
    value: &Value,
    match_definitions: &HashMap<String, MatchDefinition>,
) -> Result<MatchConfig, Error> {
    match value {
        Value::Identifier(id) => {
            let def = match_definitions.get(id).ok_or_else(|| {
                template_error(format!(
                    "element '{element_name}': {attr}={id} references unknown match \
                     definition '{id}'"
                ))
            })?;
            definition_to_match_config(def)
        }
        other => other.as_match_config().ok_or_else(|| {
            template_error(format!(
                "element '{element_name}': {attr} must be a string, a \
                 [pattern, threshold(, type)] array, or a match-definition identifier"
            ))
        }),
    }
}

/// One executable clause resolves to that config; several resolve to a
/// `FirstMatch` over them in order (D-014 — previously every clause after
/// the first was silently dropped). The definition name is recorded on the
/// config for diagnostics ("naming the match block", D-006).
fn definition_to_match_config(def: &MatchDefinition) -> Result<MatchConfig, Error> {
    let mut configs: Vec<MatchConfig> = def
        .clauses
        .iter()
        .filter_map(|clause| match clause {
            MatchExpression::MatchConfig(config) => Some(config.clone()),
            _ => None,
        })
        .collect();

    let mut config = match configs.len() {
        0 => {
            return Err(template_error(format!(
                "match definition '{}' has no executable match clause (D-006)",
                def.name
            )))
        }
        1 => configs.pop().expect("len checked"),
        _ => MatchConfig {
            match_type: MatchType::FirstMatch(configs),
            threshold: 1.0,
            ..MatchConfig::default()
        },
    };
    config.name = Some(def.name.clone());
    Ok(config)
}

fn function_call_to_match_config(function_call: &FunctionCall) -> Option<MatchConfig> {
    match function_call.name.as_str() {
        "Text" => {
            // Text(pattern, threshold=0.8)
            if function_call.args.is_empty() {
                return None;
            }

            let pattern = match &function_call.args[0] {
                FunctionArg::Positional(FunctionArgValue::Value(Value::String(s))) => s.clone(),
                _ => return None,
            };

            let mut threshold = 0.8; // Default threshold for text matching
            let mut options = HashMap::new();

            // Process additional arguments
            for arg in &function_call.args[1..] {
                match arg {
                    FunctionArg::Named { name, value } => {
                        match name.as_str() {
                            "threshold" => {
                                if let FunctionArgValue::Value(Value::Number(n)) = value {
                                    threshold = *n as f64 / 1000.0; // Convert from fixed-point
                                }
                            }
                            _ => {
                                // Store other options as-is
                                if let FunctionArgValue::Value(v) = value {
                                    options.insert(name.clone(), v.clone());
                                }
                            }
                        }
                    }
                    FunctionArg::Positional(FunctionArgValue::Value(Value::Number(n))) => {
                        // Second positional argument is threshold
                        threshold = *n as f64 / 1000.0;
                    }
                    _ => {}
                }
            }

            Some(MatchConfig {
                match_type: MatchType::Text,
                pattern,
                threshold,
                options,
                ..MatchConfig::default()
            })
        }
        // Canonical: EmbeddingSim("query", threshold=0.6, endpoint="databricks-bge",
        // model="..."). Cosine/Semantic are aliases for backwards compatibility (D-014).
        "EmbeddingSim" | "Cosine" | "Semantic" => {
            if function_call.args.is_empty() {
                return None;
            }

            let pattern = match &function_call.args[0] {
                FunctionArg::Positional(FunctionArgValue::Value(Value::String(s))) => s.clone(),
                _ => return None,
            };

            let mut threshold = 0.7; // Default threshold for embedding similarity
            let mut endpoint = None;
            let mut model = None;
            let mut options = HashMap::new();

            // Process additional arguments
            for arg in &function_call.args[1..] {
                match arg {
                    FunctionArg::Named { name, value } => match (name.as_str(), value) {
                        ("threshold", FunctionArgValue::Value(Value::Number(n))) => {
                            threshold = *n as f64 / 1000.0;
                        }
                        ("endpoint", FunctionArgValue::Value(Value::String(s))) => {
                            endpoint = Some(s.clone());
                        }
                        ("model", FunctionArgValue::Value(Value::String(s))) => {
                            model = Some(s.clone());
                        }
                        (_, FunctionArgValue::Value(v)) => {
                            options.insert(name.clone(), v.clone());
                        }
                        _ => {}
                    },
                    FunctionArg::Positional(FunctionArgValue::Value(Value::Number(n))) => {
                        threshold = *n as f64 / 1000.0;
                    }
                    _ => {}
                }
            }

            Some(MatchConfig {
                match_type: MatchType::EmbeddingSim,
                pattern,
                threshold,
                endpoint,
                model,
                options,
                ..MatchConfig::default()
            })
        }
        "Regex" => {
            // Regex(pattern); the pattern is compiled (and cached) at template
            // compile time — see compile_match_config (D-014).
            if function_call.args.is_empty() {
                return None;
            }

            let pattern = match &function_call.args[0] {
                FunctionArg::Positional(FunctionArgValue::Value(Value::String(s))) => s.clone(),
                _ => return None,
            };

            let mut options = HashMap::new();

            // Process additional arguments
            for arg in &function_call.args[1..] {
                if let FunctionArg::Named { name, value } = arg {
                    if let FunctionArgValue::Value(v) = value {
                        options.insert(name.clone(), v.clone());
                    }
                }
            }

            Some(MatchConfig {
                match_type: MatchType::Regex,
                pattern,
                threshold: 1.0, // Regex is exact match
                options,
                ..MatchConfig::default()
            })
        }
        // Heuristic(fontSize > 14, y_position >= 700, ...): every argument
        // must be a comparison; multiple comparisons AND together (D-014).
        "Heuristic" => {
            let mut comparisons = Vec::new();
            for arg in &function_call.args {
                match arg {
                    FunctionArg::Positional(FunctionArgValue::Comparison(comp)) => {
                        comparisons.push(comp.clone());
                    }
                    // Anything else (named args, bare values) is malformed;
                    // leave as FunctionCall so validation reports it (D-006).
                    _ => return None,
                }
            }
            if comparisons.is_empty() {
                return None;
            }
            Some(MatchConfig {
                match_type: MatchType::Heuristic(comparisons),
                threshold: 1.0,
                ..MatchConfig::default()
            })
        }
        // FirstMatch(Text("a"), Regex("b"), ...): alternatives tried in
        // order; first that yields candidates wins (D-014).
        "FirstMatch" => {
            let mut subs = Vec::new();
            for arg in &function_call.args {
                match arg {
                    FunctionArg::Positional(FunctionArgValue::FunctionCall(fc)) => {
                        subs.push(function_call_to_match_config(fc)?);
                    }
                    _ => return None,
                }
            }
            if subs.is_empty() {
                return None;
            }
            Some(MatchConfig {
                match_type: MatchType::FirstMatch(subs),
                threshold: 1.0,
                ..MatchConfig::default()
            })
        }
        _ => None, // Unknown function types remain as FunctionCall (rejected in validation)
    }
}

fn process_match_expression(pair: Pair<Rule>) -> MatchExpression {
    let inner_pair = if pair.as_rule() == Rule::match_expression {
        pair.into_inner().next().unwrap()
    } else {
        pair
    };

    match inner_pair.as_rule() {
        Rule::function_call => {
            let function_call = process_function_call(inner_pair);
            // Try to convert to MatchConfig if it's a known match type
            if let Some(match_config) = function_call_to_match_config(&function_call) {
                MatchExpression::MatchConfig(match_config)
            } else {
                MatchExpression::FunctionCall(function_call)
            }
        }
        Rule::value => MatchExpression::Value(process_value(inner_pair)),
        _ => {
            // Fallback for other value types
            MatchExpression::Value(process_value(inner_pair))
        }
    }
}

fn process_function_call(pair: Pair<Rule>) -> FunctionCall {
    let mut inner_rules = pair.into_inner();
    let name = inner_rules.next().unwrap().as_str().to_string();

    let mut args = Vec::new();

    // Process function arguments if they exist
    if let Some(args_pair) = inner_rules.next() {
        if args_pair.as_rule() == Rule::function_args {
            for arg_pair in args_pair.into_inner() {
                if arg_pair.as_rule() == Rule::function_arg {
                    args.push(process_function_arg(arg_pair));
                }
            }
        }
    }

    FunctionCall { name, args }
}

fn process_function_arg(pair: Pair<Rule>) -> FunctionArg {
    let mut inner_rules = pair.into_inner();

    // Check if this is a named argument (identifier = value) or positional (just value)
    let first_pair = inner_rules.next().unwrap();

    if let Some(second_pair) = inner_rules.next() {
        // This is a named argument: identifier = function_arg_value
        let name = first_pair.as_str().to_string();
        let value = process_function_arg_value(second_pair);
        FunctionArg::Named { name, value }
    } else {
        // This is a positional argument: just function_arg_value
        let value = process_function_arg_value(first_pair);
        FunctionArg::Positional(value)
    }
}

fn process_function_arg_value(pair: Pair<Rule>) -> FunctionArgValue {
    let inner_pair = if pair.as_rule() == Rule::function_arg_value {
        pair.into_inner().next().unwrap()
    } else {
        pair
    };

    match inner_pair.as_rule() {
        Rule::comparison_expr => FunctionArgValue::Comparison(process_comparison_expr(inner_pair)),
        Rule::function_call => FunctionArgValue::FunctionCall(process_function_call(inner_pair)),
        _ => FunctionArgValue::Value(process_value(inner_pair)),
    }
}

fn process_comparison_expr(pair: Pair<Rule>) -> ComparisonExpr {
    let mut inner_rules = pair.into_inner();
    let left = inner_rules.next().unwrap().as_str().to_string();
    let op_pair = inner_rules.next().unwrap();
    let comparison_value_pair = inner_rules.next().unwrap(); // This should be Rule::comparison_value

    // Process the comparison_value rule which contains the actual value
    let right_pair = comparison_value_pair.into_inner().next().unwrap();

    // Convert from the restricted comparison value to ComparisonValue
    let right = match right_pair.as_rule() {
        Rule::string => {
            let s = right_pair.as_str();
            ComparisonValue::String(s[1..s.len() - 1].to_string()) // Remove quotes
        }
        Rule::number => {
            // Stored as the literal f64 (no fixed-point encoding): heuristic
            // comparisons need the real value at evaluation time (D-014).
            let n = right_pair.as_str().parse::<f64>().unwrap();
            ComparisonValue::Number(n)
        }
        Rule::boolean => {
            let b = right_pair.as_str().parse::<bool>().unwrap();
            ComparisonValue::Boolean(b)
        }
        Rule::identifier => ComparisonValue::Identifier(right_pair.as_str().to_string()),
        _ => {
            error!(
                "Unexpected comparison value rule: {:?}",
                right_pair.as_rule()
            );
            ComparisonValue::String(right_pair.as_str().to_string()) // Fallback
        }
    };

    let op = match op_pair.as_str() {
        ">" => ComparisonOp::GreaterThan,
        "<" => ComparisonOp::LessThan,
        ">=" => ComparisonOp::GreaterThanOrEqual,
        "<=" => ComparisonOp::LessThanOrEqual,
        "==" => ComparisonOp::Equal,
        "!=" => ComparisonOp::NotEqual,
        _ => {
            error!("Unknown comparison operator: {}", op_pair.as_str());
            ComparisonOp::Equal // Default fallback
        }
    };

    ComparisonExpr { left, op, right }
}

// Process the matched content to generate chunks or image data
/// Errors when a matched element cannot be processed (D-006) — today that is
/// `method="semantic"` chunking without a configured embedder, or a semantic
/// embedding batch failing.
pub fn process_matched_content(
    matched_items: &Vec<TemplateContentMatch>,
    index: &crate::search_index::PdfIndex, // Add index parameter to resolve handles
    tokenizer: Option<&Tokenizer>,
) -> Result<Vec<ProcessedOutput>, Error> {
    let mut global_chunk_counter = 0;
    let mut all_outputs = Vec::new();
    let mut deferred_tables = Vec::new();

    // Process all content and then fix parent references
    process_matched_content_recursive(
        matched_items,
        index,
        tokenizer,
        &mut all_outputs,
        &mut deferred_tables,
        &mut global_chunk_counter,
        None, // parent_info: Option<(String, usize)>
        &HashMap::new(),
    )?;

    // TableOutputs append after all positional outputs (D-018): chunk
    // parent_index links are positions in this array, so inserting tables
    // inline would shift the recorded indices of every later section. The
    // table outputs themselves carry parent_name/parent_index captured at
    // match time, which stay valid because earlier positions never move.
    all_outputs.extend(deferred_tables);

    Ok(all_outputs)
}

// Recursive function that builds the output list and tracks parent relationships
#[allow(clippy::too_many_arguments)]
fn process_matched_content_recursive(
    matched_items: &Vec<TemplateContentMatch>,
    index: &crate::search_index::PdfIndex,
    tokenizer: Option<&Tokenizer>,
    all_outputs: &mut Vec<ProcessedOutput>,
    deferred_tables: &mut Vec<ProcessedOutput>,
    global_chunk_counter: &mut usize,
    parent_info: Option<(String, usize)>, // (parent_name, parent_output_index)
    parent_metadata: &HashMap<String, Value>,
) -> Result<(), Error> {
    for match_item in matched_items {
        // Combine parent metadata with the current item's metadata.
        // The current item's metadata will overwrite the parent's if keys conflict.
        let mut current_metadata = parent_metadata.clone();
        current_metadata.extend(
            match_item
                .metadata
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );

        match &match_item.template_element.element_type {
            ElementType::TextChunk | ElementType::Paragraph => {
                let text_elements_to_chunk: Vec<TextElement> = match_item
                    .matched_content
                    .iter()
                    .filter_map(|mc_ref| match mc_ref {
                        MatchedContent::Index(doc_idx) => {
                            // Resolve index to get actual content
                            if let Some(content) = index.content_at(*doc_idx) {
                                match content {
                                    PageContent::Text(text_elem) => Some(text_elem),
                                    _ => None,
                                }
                            } else {
                                None
                            }
                        }
                        MatchedContent::None => None,
                    })
                    .collect();

                if !text_elements_to_chunk.is_empty() {
                    let chunk_outputs = process_text_chunk_elements_simple(
                        &text_elements_to_chunk,
                        &match_item.template_element,
                        &current_metadata,
                        parent_info.clone(),
                        global_chunk_counter,
                        tokenizer,
                        index.embedder(),
                    )?;
                    tracing::debug!("chunk outputs: {:?}", chunk_outputs);
                    for chunk_output in chunk_outputs {
                        all_outputs.push(ProcessedOutput::Text(chunk_output));
                    }
                }
            }
            ElementType::Section => {
                let current_section_name = match_item.template_element.name.clone();

                // First, process the section's own content if it has any
                let section_text_elements: Vec<TextElement> = match_item
                    .matched_content
                    .iter()
                    .filter_map(|mc_ref| match mc_ref {
                        MatchedContent::Index(doc_idx) => {
                            // Resolve index to get actual content
                            if let Some(content) = index.content_at(*doc_idx) {
                                match content {
                                    PageContent::Text(text_elem) => Some(text_elem),
                                    _ => None,
                                }
                            } else {
                                None
                            }
                        }
                        MatchedContent::None => None,
                    })
                    .collect();

                // Track where this section's content will be in the output
                let section_output_index = all_outputs.len();
                let mut section_has_content = false;

                if !section_text_elements.is_empty() {
                    let chunk_outputs = process_text_chunk_elements_simple(
                        &section_text_elements,
                        &match_item.template_element,
                        &current_metadata,
                        parent_info.clone(), // FIXED: Section's own content gets the parent info passed to this section
                        global_chunk_counter,
                        tokenizer,
                        index.embedder(),
                    )?;
                    for chunk_output in chunk_outputs {
                        all_outputs.push(ProcessedOutput::Text(chunk_output));
                        section_has_content = true;
                    }
                }

                // Determine parent info for children - children should reference THIS section
                let child_parent_info = if section_has_content {
                    // If this section itself has a parent, then its children should reference this section
                    // If this section has no parent (root-level), then its immediate TextChunk children should also have no parent
                    // but nested Sections should reference this section
                    if parent_info.is_some() {
                        // This is a nested section, so children reference this section
                        Some((current_section_name.clone(), section_output_index))
                    } else {
                        // This is a root-level section, so immediate TextChunk children get no parent
                        // but nested Sections should reference this section
                        // We'll handle this distinction in the child processing
                        Some((current_section_name.clone(), section_output_index))
                    }
                } else {
                    // Section has no content, children inherit the same parent
                    parent_info.clone()
                };

                // Process any children with updated parent info
                if !match_item.children.is_empty() {
                    // Special handling: if this is a root-level section (parent_info is None),
                    // then TextChunk children should get None as parent, but Section children should get this section as parent
                    for child_match in &match_item.children {
                        match &child_match.template_element.element_type {
                            ElementType::TextChunk | ElementType::Paragraph => {
                                // TextChunk children of root-level sections get no parent
                                let textchunk_parent_info = if parent_info.is_none() {
                                    None // Root-level section's TextChunk children have no parent
                                } else {
                                    child_parent_info.clone() // Nested section's TextChunk children have parent
                                };

                                process_matched_content_recursive(
                                    &vec![child_match.clone()],
                                    index,
                                    tokenizer,
                                    all_outputs,
                                    deferred_tables,
                                    global_chunk_counter,
                                    textchunk_parent_info,
                                    &current_metadata,
                                )?;
                            }
                            ElementType::Section => {
                                // Section children always get this section as parent (regardless of nesting level)
                                process_matched_content_recursive(
                                    &vec![child_match.clone()],
                                    index,
                                    tokenizer,
                                    all_outputs,
                                    deferred_tables,
                                    global_chunk_counter,
                                    child_parent_info.clone(),
                                    &current_metadata,
                                )?;
                            }
                            _ => {
                                // Other types use the default logic
                                process_matched_content_recursive(
                                    &vec![child_match.clone()],
                                    index,
                                    tokenizer,
                                    all_outputs,
                                    deferred_tables,
                                    global_chunk_counter,
                                    child_parent_info.clone(),
                                    &current_metadata,
                                )?;
                            }
                        }
                    }
                }
            }
            ElementType::Image => {
                let mut image_processed = false;
                for mc_ref in &match_item.matched_content {
                    if let MatchedContent::Index(doc_idx) = mc_ref {
                        // Resolve index to get actual content
                        if let Some(content) = index.content_at(*doc_idx) {
                            if let PageContent::Image(image_elem) = content {
                                all_outputs.push(process_image_element_simple(
                                    &image_elem,
                                    &match_item.template_element,
                                    &current_metadata,
                                    parent_info.clone(),
                                ));
                                image_processed = true;
                                break;
                            }
                        }
                    }
                }
                if !image_processed {
                    warn!(
                        "Image template element '{}' did not find any MatchedContent::Index with Image content.",
                        match_item.template_element.name
                    );
                }
            }
            // Table selector (D-018): every matched kind=table aux element in
            // the assigned range produces one TableOutput. Outputs are
            // deferred to the tail of the array (see process_matched_content)
            // so positional parent_index links of later sections stay stable.
            ElementType::Table => {
                for mc_ref in &match_item.matched_content {
                    let MatchedContent::Index(doc_idx) = mc_ref else {
                        continue;
                    };
                    let Some(aux) = index
                        .aux_at(*doc_idx)
                        .filter(|aux| aux.kind == crate::parse::AuxKind::Table)
                    else {
                        continue;
                    };
                    let Some(table) = aux.table.as_ref() else {
                        warn!(
                            "kind=table element {} carries no TableStructure; skipping output",
                            aux.id
                        );
                        continue;
                    };
                    // type="Name" coerces into the user-defined type (Stage
                    // C, D-021); without it the plain TableOutput path is
                    // untouched, byte-identically.
                    let output = match match_item.template_element.table_type.as_deref() {
                        Some(type_def) => process_typed_table_element(
                            aux,
                            table,
                            type_def,
                            &match_item.template_element,
                            &current_metadata,
                            parent_info.clone(),
                        ),
                        None => process_table_element_simple(
                            table,
                            &match_item.template_element,
                            &current_metadata,
                            parent_info.clone(),
                        ),
                    };
                    deferred_tables.push(output);
                }
            }
            // Annotation / Figure selectors (D-016): every matched element of
            // the right aux kind in the assigned range produces one output.
            ElementType::Annotation | ElementType::Figure => {
                let want_kind = match match_item.template_element.element_type {
                    ElementType::Annotation => crate::parse::AuxKind::Annotation,
                    _ => crate::parse::AuxKind::Figure,
                };
                for mc_ref in &match_item.matched_content {
                    let MatchedContent::Index(doc_idx) = mc_ref else {
                        continue;
                    };
                    let Some(aux) = index.aux_at(*doc_idx).filter(|aux| aux.kind == want_kind)
                    else {
                        continue;
                    };
                    all_outputs.push(process_aux_element_simple(
                        aux,
                        &match_item.template_element,
                        &current_metadata,
                        parent_info.clone(),
                    ));
                }
            }
            ElementType::ImageSummary
            | ElementType::ImageBytes
            | ElementType::ImageCaption
            | ElementType::ImageEmbedding => {
                warn!(
                    "Encountered {:?} as a top-level matched element. These are typically processed as children of an Image element.",
                    match_item.template_element.element_type
                );
            }
            ElementType::SubCorpus => {
                // Stripped from the element tree at template compile
                // (collect_sub_corpora); only programmatically built trees
                // can reach here.
                warn!(
                    "SubCorpus element '{}' reached matching; declarations are registry-only",
                    match_item.template_element.name
                );
            }
            ElementType::Unknown => {
                warn!(
                    "Encountered ElementType::Unknown for template element: {}",
                    match_item.template_element.name
                );
            }
        }
    }
    Ok(())
}

/// Convert one template attribute `Value` into JSON. Shared by the image,
/// chunk, annotation, and figure output builders (identical semantics to the
/// previously-inlined conversions: nested non-primitive array entries fall
/// back to their Debug rendering).
fn template_value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Number(n) => serde_json::Value::Number(serde_json::Number::from(*n)),
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Array(arr) => {
            let json_arr: Vec<serde_json::Value> = arr
                .iter()
                .map(|v| match v {
                    Value::String(s) => serde_json::Value::String(s.clone()),
                    Value::Number(n) => serde_json::Value::Number(serde_json::Number::from(*n)),
                    Value::Boolean(b) => serde_json::Value::Bool(*b),
                    _ => serde_json::Value::String(format!("{:?}", v)),
                })
                .collect();
            serde_json::Value::Array(json_arr)
        }
        Value::Identifier(s) => serde_json::Value::String(s.clone()),
    }
}

/// Build the output for a matched aux element (Annotation / Figure
/// selectors, D-016). Output metadata = inherited template metadata,
/// overlaid with the element's own metadata (subtype/uri, caption ids, ...);
/// `as="..."` surfaces as metadata key "name" (sections use "section").
fn process_aux_element_simple(
    aux: &crate::parse::AuxElement,
    template_element: &Element,
    metadata: &HashMap<String, Value>,
    parent_info: Option<(String, usize)>,
) -> ProcessedOutput {
    let mut json_metadata: HashMap<String, serde_json::Value> = metadata
        .iter()
        .map(|(k, v)| (k.clone(), template_value_to_json(v)))
        .collect();
    if let Some(own) = aux.metadata.as_object() {
        for (k, v) in own {
            json_metadata.insert(k.clone(), v.clone());
        }
    }
    if let Some(as_value) = template_element.attributes.get("as") {
        json_metadata.insert("name".to_string(), template_value_to_json(as_value));
    }

    let (parent_name, parent_index) = match parent_info {
        Some((name, index)) => (Some(name), Some(index)),
        None => (None, None),
    };
    let bbox = (aux.bbox.x0, aux.bbox.y0, aux.bbox.x1, aux.bbox.y1);

    match aux.kind {
        crate::parse::AuxKind::Figure => ProcessedOutput::Figure(FigureOutput {
            id: aux.id.to_string(),
            page_number: aux.page_number,
            bbox,
            caption: aux
                .metadata
                .get("caption")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            image_id: aux
                .metadata
                .get("image_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            metadata: json_metadata,
            parent_name,
            parent_index,
        }),
        _ => ProcessedOutput::Annotation(AnnotationOutput {
            id: aux.id.to_string(),
            page_number: aux.page_number,
            bbox,
            text: aux.text.clone(),
            metadata: json_metadata,
            parent_name,
            parent_index,
        }),
    }
}

/// Build the output for a matched table (D-018). `name` is the `as="..."`
/// value, falling back to the template element name ("Table"); inherited
/// template metadata rides along like the other output types.
fn process_table_element_simple(
    table: &crate::table::TableStructure,
    template_element: &Element,
    metadata: &HashMap<String, Value>,
    parent_info: Option<(String, usize)>,
) -> ProcessedOutput {
    let json_metadata: HashMap<String, serde_json::Value> = metadata
        .iter()
        .map(|(k, v)| (k.clone(), template_value_to_json(v)))
        .collect();
    let name = match template_element.attributes.get("as") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => format!("{other:?}"),
        None => template_element.name.clone(),
    };
    let (parent_name, parent_index) = match parent_info {
        Some((name, index)) => (Some(name), Some(index)),
        None => (None, None),
    };

    ProcessedOutput::Table(TableOutput {
        name,
        page: table.page,
        bbox: (
            table.bbox.x0,
            table.bbox.y0,
            table.bbox.x1,
            table.bbox.y1,
        ),
        n_rows: table.n_rows,
        n_cols: table.n_cols,
        header: table.header_texts(),
        rows: table.body_rows(),
        cells: table.cells.clone(),
        strategy: table.strategy,
        confidence: table.confidence,
        metadata: json_metadata,
        parent_name,
        parent_index,
    })
}

/// Build the output for a matched table carrying a `type="Name"` attribute
/// (Stage C, D-021): coerce the detected grid into typed records. Naming,
/// inherited metadata, and parent links follow `process_table_element_simple`;
/// the table element id rides in `provenance` (typed extraction is the first
/// consumer that needs it). Cells whose trailing `%` was stripped surface as
/// metadata `percent_cells`.
fn process_typed_table_element(
    aux: &crate::parse::AuxElement,
    table: &crate::table::TableStructure,
    type_def: &crate::udt::TableTypeDef,
    template_element: &Element,
    metadata: &HashMap<String, Value>,
    parent_info: Option<(String, usize)>,
) -> ProcessedOutput {
    let extraction = crate::udt::extract_typed_records(table, type_def);

    let mut json_metadata: HashMap<String, serde_json::Value> = metadata
        .iter()
        .map(|(k, v)| (k.clone(), template_value_to_json(v)))
        .collect();
    if !extraction.percent_cells.is_empty() {
        json_metadata.insert(
            "percent_cells".to_string(),
            serde_json::Value::Array(
                extraction
                    .percent_cells
                    .iter()
                    .map(|(row, field)| serde_json::json!({ "row": row, "field": field }))
                    .collect(),
            ),
        );
    }
    let name = match template_element.attributes.get("as") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => format!("{other:?}"),
        None => template_element.name.clone(),
    };
    let (parent_name, parent_index) = match parent_info {
        Some((name, index)) => (Some(name), Some(index)),
        None => (None, None),
    };

    ProcessedOutput::TypedTable(TypedTableOutput {
        name,
        type_name: type_def.name.clone(),
        records: extraction.records,
        errors: extraction.errors,
        coerced_ok: extraction.coerced_ok,
        coerced_err: extraction.coerced_err,
        provenance: TypedTableProvenance {
            element_id: aux.id.to_string(),
            page: table.page,
            bbox: (
                table.bbox.x0,
                table.bbox.y0,
                table.bbox.x1,
                table.bbox.y1,
            ),
            source_rows: extraction.source_rows,
        },
        metadata: json_metadata,
        parent_name,
        parent_index,
    })
}

// Helper function to process a matched Image element based on its children
fn process_image_element_simple(
    image_element: &crate::parse::ImageElement,
    template_element: &Element,
    metadata: &HashMap<String, Value>,
    parent_info: Option<(String, usize)>,
) -> ProcessedOutput {
    // Convert existing metadata from Value to serde_json::Value
    let mut json_metadata: HashMap<String, serde_json::Value> = HashMap::new();
    for (key, value) in metadata {
        json_metadata.insert(key.clone(), template_value_to_json(value));
    }

    // Extract parent information
    let (parent_name, parent_index) = if let Some((name, index)) = parent_info {
        (Some(name), Some(index))
    } else {
        (None, None)
    };

    let mut image_output = ImageOutput {
        id: image_element.id.to_string(),
        page_number: image_element.page_number,
        bbox: (
            image_element.bbox.x0,
            image_element.bbox.y0,
            image_element.bbox.x1,
            image_element.bbox.y1,
        ),
        caption: None,
        bytes_base64: None,
        summary: None,
        embedding: None,
        metadata: json_metadata,
        parent_name,
        parent_index,
    };

    // Attempt to decode image bytes once if needed by any child
    let needs_bytes = template_element.children.iter().any(|child| {
        matches!(
            child.element_type,
            ElementType::ImageBytes | ElementType::ImageSummary | ElementType::ImageEmbedding
        )
    });

    let image_bytes_result = if needs_bytes {
        decode_image_object(&image_element.image_object)
    } else {
        Err("Bytes not needed".to_string()) // Indicate bytes weren't requested
    };

    // Iterate through children of the Image template element
    for child_template in &template_element.children {
        match child_template.element_type {
            ElementType::ImageBytes => {
                match &image_bytes_result {
                    Ok(bytes) => {
                        image_output.bytes_base64 = Some(BASE64_STANDARD.encode(bytes));
                        tracing::debug!(
                            "Successfully decoded and encoded image bytes for ImageBytes."
                        );
                    }
                    Err(e) => {
                        if e != "Bytes not needed" {
                            // Don't warn if bytes weren't requested
                            warn!("Could not get image bytes for ImageBytes: {}", e);
                        }
                        image_output.bytes_base64 = None; // Ensure it's None on error
                    }
                }
            }
            ElementType::ImageCaption => {
                // TODO: Implement actual caption finding logic
                // This likely involves searching nearby TextElements in the PdfIndex
                // based on the image_element.bbox and page_number.
                tracing::debug!("Placeholder: Need to implement caption finding for ImageCaption");
                image_output.caption = Some("PLACEHOLDER_IMAGE_CAPTION".to_string());
            }
            ElementType::ImageSummary => {
                let model = child_template
                    .attributes
                    .get("model")
                    .and_then(|v| v.as_string())
                    .unwrap_or_default();
                let prompt = child_template
                    .attributes
                    .get("prompt")
                    .and_then(|v| v.as_string())
                    .unwrap_or_default();
                let target_schema = child_template
                    .attributes
                    .get("targetSchema")
                    .and_then(|v| v.as_string());

                let config = LLMConfig {
                    model,
                    prompt,
                    target_schema,
                };

                match &image_bytes_result {
                    Ok(_bytes) => {
                        // TODO: Implement actual call to external LLM for summary
                        // let summary = call_llm_summary(&config, bytes);
                        tracing::debug!(
                            "Placeholder: Call external summary model ('{:?}')",
                            config
                        );
                        image_output.summary =
                            Some(format!("PLACEHOLDER_SUMMARY_FROM_{}", config.model));
                    }
                    Err(e) => {
                        if e != "Bytes not needed" {
                            warn!("Could not get image bytes for ImageSummary: {}", e);
                        }
                        image_output.summary = None;
                    }
                }
            }
            ElementType::ImageEmbedding => {
                let model_str = child_template
                    .attributes
                    .get("model")
                    .and_then(|v| v.as_string())
                    .unwrap_or("clip".to_string());
                let embedding_model = EmbeddingModel::from(model_str.as_str());

                match &image_bytes_result {
                    Ok(bytes) => {
                        // Call the placeholder embedding function
                        match generate_embedding(&embedding_model, bytes) {
                            Ok(embedding) => {
                                image_output.embedding = Some(embedding);
                                tracing::debug!(
                                    "Successfully generated placeholder embedding using {:?}.",
                                    embedding_model
                                );
                            }
                            Err(e) => {
                                warn!(
                                    "Embedding generation failed for model {:?}: {}",
                                    embedding_model, e
                                );
                                image_output.embedding = None;
                            }
                        }
                    }
                    Err(e) => {
                        if e != "Bytes not needed" {
                            warn!("Could not get image bytes for ImageEmbedding: {}", e);
                        }
                        image_output.embedding = None;
                    }
                }
            }
            _ => {}
        }
    }

    ProcessedOutput::Image(image_output)
}

#[allow(clippy::too_many_arguments)]
fn process_text_chunk_elements_simple(
    elements: &[TextElement],
    template_element: &Element,
    metadata: &HashMap<String, Value>,
    parent_info: Option<(String, usize)>,
    global_chunk_counter: &mut usize,
    tokenizer: Option<&Tokenizer>,
    embedder: Option<&dyn crate::embed::Embedder>,
) -> Result<Vec<ChunkOutput>, Error> {
    // -------- 1. resolve parameters once --------
    let chunk_size = template_element
        .attributes
        .get("chunkSize")
        .and_then(|v| v.as_number())
        .unwrap_or(500) as usize;

    let chunk_overlap = template_element
        .attributes
        .get("chunkOverlap")
        .and_then(|v| v.as_number())
        .unwrap_or(150) as usize;

    // method= selects the chunking strategy (D-020). Validated at template
    // compile; re-checked here because Elements can also be built
    // programmatically (D-006: an unknown method must never silently chunk).
    let method = match template_element.attributes.get("method") {
        None => ChunkMethod::Tokens,
        Some(value) => value
            .as_string()
            .as_deref()
            .and_then(ChunkMethod::parse)
            .ok_or_else(|| {
                template_error(format!(
                    "{}: unknown method {}; supported values: {} (D-006: no silent skip)",
                    chunk_owner_label(template_element),
                    format_attr_value(value),
                    ChunkMethod::SUPPORTED,
                ))
            })?,
    };

    // template="..." interpolation (Stage C, D-022, TextChunk only):
    // SubCorpus {name} placeholders were already substituted at template
    // compile, so the only chunk-time variable left is {text}.
    let chunk_template = if template_element.element_type == ElementType::TextChunk {
        template_element
            .attributes
            .get("template")
            .and_then(|v| v.as_string())
    } else {
        None
    };

    // -------- 2. static conversion of template‑level metadata --------
    // Transform once rather than for every chunk.
    let base_metadata: std::sync::Arc<HashMap<String, serde_json::Value>> = {
        let mut out = HashMap::new();
        for (k, v) in metadata {
            out.insert(k.clone(), template_value_to_json(v));
        }
        std::sync::Arc::new(out)
    };

    // -------- 3. chunk the elements --------
    // Chunks are contiguous element slices under every method; Semantic
    // additionally reports how many sentence-ish segments each chunk holds.
    let chunks: Vec<(&[TextElement], Option<usize>)> = match method {
        ChunkMethod::Tokens => {
            let strategy = if let Some(tokenizer) = tokenizer {
                ChunkingStrategy::Tokens {
                    max_tokens: chunk_size,
                    chunk_overlap,
                    tokenizer: tokenizer.clone(),
                }
            } else {
                ChunkingStrategy::Characters {
                    max_chars: chunk_size,
                }
            };
            chunk_text_elements(elements, &strategy, chunk_overlap)
                .into_iter()
                .map(|chunk| (chunk, None))
                .collect()
        }
        ChunkMethod::Semantic => {
            // Fail loud without an embedder, mirroring the EmbeddingSim
            // matcher's error shape (D-006/D-014).
            let Some(embedder) = embedder else {
                return Err(template_error(format!(
                    "{}: method=\"semantic\" requires an embedder but none is configured; \
                     pass --embed-endpoint <name-or-url> or set DELVER_EMBED_ENDPOINT \
                     (D-006: no silent skip)",
                    chunk_owner_label(template_element)
                )));
            };
            let percentile = template_element
                .attributes
                .get("breakpointPercentile")
                .and_then(|v| v.as_number())
                .unwrap_or(25);
            if !(0..=100).contains(&percentile) {
                return Err(template_error(format!(
                    "{}: breakpointPercentile must be an integer percentile in 0..=100, got {}",
                    chunk_owner_label(template_element),
                    percentile,
                )));
            }
            chunk_semantic(
                elements,
                embedder,
                chunk_size,
                chunk_overlap,
                percentile as u8,
                tokenizer,
            )
            .map_err(|e| {
                template_error(format!(
                    "{}: semantic chunking failed: {e}",
                    chunk_owner_label(template_element)
                ))
            })?
            .into_iter()
            .map(|chunk| (chunk.elements, Some(chunk.segment_count)))
            .collect()
        }
    };

    // -------- 4. Extract parent information --------
    let (parent_name, parent_index) = if let Some((name, index)) = parent_info {
        (Some(name), Some(index))
    } else {
        (None, None)
    };

    // -------- 5. build outputs --------
    let mut outputs = Vec::with_capacity(chunks.len());

    for (chunk, segment_count) in &chunks {
        // a) pre‑compute char capacity
        let est_chars: usize = chunk.iter().map(|e| e.text.len()).sum();
        let mut chunk_text = String::with_capacity(est_chars + chunk.len()); // +spaces

        // b) page statistics (we almost never have more than a handful of pages per chunk)
        let mut page_char_counts: Vec<(u32, usize)> = Vec::new();

        for (i, elem) in chunk.iter().enumerate() {
            chunk_text.push_str(elem.text.as_str());
            if i + 1 != chunk.len() {
                chunk_text.push(' ');
            }

            // accumulate char counts per page without HashMap
            match page_char_counts
                .iter_mut()
                .find(|(p, _)| *p == elem.page_number)
            {
                Some((_, cnt)) => *cnt += elem.text.len(),
                None => page_char_counts.push((elem.page_number, elem.text.len())),
            }
        }

        // derive page metadata
        let primary_page = page_char_counts
            .iter()
            .max_by_key(|(_, cnt)| *cnt)
            .map(|(p, _)| *p)
            .unwrap_or(1);

        // stable order
        page_char_counts.sort_by_key(|(p, _)| *p);
        let page_numbers: Vec<u32> = page_char_counts.iter().map(|(p, _)| *p).collect();

        // c) assemble metadata – start with shared reference, then extend
        let mut meta = (*base_metadata).clone(); // shallow clone of small map
        meta.insert(
            "primary_page".into(),
            serde_json::Value::Number(serde_json::Number::from(primary_page)),
        );
        meta.insert(
            "page_numbers".into(),
            serde_json::Value::Array(
                page_numbers
                    .iter()
                    .map(|p| serde_json::Value::Number(serde_json::Number::from(*p)))
                    .collect(),
            ),
        );
        meta.insert(
            "chunk_element_count".into(),
            serde_json::Value::Number(serde_json::Number::from(chunk.len())),
        );
        meta.insert(
            "chunk_char_count".into(),
            serde_json::Value::Number(serde_json::Number::from(chunk_text.len())),
        );

        // Semantic-only metadata (D-020). The default/tokens path stays
        // byte-identical to its pre-slice output, so these keys exist only
        // when method="semantic".
        if let Some(segment_count) = segment_count {
            meta.insert(
                "method".into(),
                serde_json::Value::String("semantic".to_string()),
            );
            meta.insert(
                "segment_count".into(),
                serde_json::Value::Number(serde_json::Number::from(*segment_count)),
            );
        }

        // Note: We've simplified the parent tracking - no longer storing full ancestry path

        // {text} substitution happens last: metadata char/page statistics
        // describe the source chunk, independent of interpolation (D-022).
        let text = match &chunk_template {
            Some(template) => template.replace("{text}", &chunk_text),
            None => chunk_text,
        };

        outputs.push(ChunkOutput {
            text,
            metadata: meta,
            chunk_index: *global_chunk_counter,
            parent_name: parent_name.clone(),
            parent_index,
        });

        *global_chunk_counter += 1;
    }

    Ok(outputs)
}

fn decode_image_object(image_object: &Object) -> Result<Vec<u8>, String> {
    if let Ok(stream) = image_object.as_stream() {
        Ok(stream.content.clone())
    } else {
        Err("Image object is not a stream".to_string())
    }
}

// Placeholder function for generating embeddings
fn generate_embedding(model: &EmbeddingModel, image_bytes: &[u8]) -> Result<Vec<f32>, String> {
    match model {
        EmbeddingModel::Clip => {
            // --- Placeholder Logic ---
            tracing::debug!(
                "Placeholder: Simulating CLIP embedding generation for image ({} bytes)",
                image_bytes.len()
            );
            // Basic validation: Try to guess format and check dimensions (optional)
            match image::guess_format(image_bytes) {
                Ok(format) => {
                    tracing::debug!("Placeholder: Detected image format: {:?}", format);
                    match image::load_from_memory(image_bytes) {
                        Ok(img) => {
                            tracing::debug!(
                                "Placeholder: Image dimensions {}x{}",
                                img.width(),
                                img.height()
                            );
                            // In real implementation, send `image_bytes` or processed image to CLIP API
                            // Here, return a dummy vector
                            Ok(vec![0.5; 512]) // Example: Return a 512-dimension vector of 0.5s
                        }
                        Err(e) => Err(format!("Placeholder: Failed to load image: {}", e)),
                    }
                }
                Err(_) => Err("Placeholder: Could not guess image format".to_string()),
            }
            // --- End Placeholder Logic ---
        }
        EmbeddingModel::Unknown(name) => Err(format!("Embedding model '{}' not implemented", name)),
    }
}
