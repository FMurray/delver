//! Query-builder state + edit controller (slice V4, DV-016).
//!
//! [`QueryBuilder`] is the app-level context the no-code palette and the
//! editor share: ONE buffer signal (the DocQL source of truth), the last
//! good parse snapshot, the current syntax error, and the selected tree
//! node. Edits are span splices computed against the snapshot (which the
//! guard requires to match the buffer byte-for-byte), so untouched source —
//! including hand formatting — survives form edits verbatim.
//!
//! Sync loop: buffer change (editor keystroke, form edit, slot insert, chip
//! insert) → [`BuilderSync`]'s client-side effect reparses in-process
//! (delver-core compiles to wasm; parsing is synchronous and sub-millisecond
//! at palette scale, so there is no debounce and no staleness window — see
//! DV-016) → snapshot signal → tree UI re-renders. On a syntax error the
//! snapshot keeps the last good tree and `syntax_error` banners the palette;
//! form edits no-op until the source parses again.

use leptos::prelude::*;

use crate::query_tree::{
    self, apply_splices, attr, child_insert_splice, delete_splice, header_splice,
    match_def_by_name, node_at, parse_query_tree, quote_str, render_match_def, render_rule,
    render_type_def, section_names_for, top_insert_splice, AttrValue, AttrWrite, NodeKind,
    NodePath, ParseOutcome, QueryNode, QueryTree, RuleSpec, SlotKind, Splice,
};
use crate::snippets::{render_snippet, SnippetSpec};

/// Last good parse: the exact text it was built from plus its tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub text: String,
    pub tree: QueryTree,
}

/// A slot between/inside nodes: `parent` None = top level, `index` = the
/// child position the insert lands at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotKey {
    pub parent: Option<NodePath>,
    pub index: usize,
}

#[derive(Clone, Copy)]
pub struct QueryBuilder {
    /// The DocQL source of truth (editor and forms both write it).
    pub buffer: RwSignal<String>,
    /// Last good parse of the buffer (kept across syntax errors).
    pub snapshot: RwSignal<Option<Snapshot>>,
    /// Set while the buffer fails the pest parse (message + position).
    pub syntax_error: RwSignal<Option<String>>,
    /// Selected tree node (its form is expanded).
    pub selected: RwSignal<Option<NodePath>>,
}

impl QueryBuilder {
    pub fn new() -> Self {
        Self {
            buffer: RwSignal::new(String::new()),
            snapshot: RwSignal::new(None),
            syntax_error: RwSignal::new(None),
            selected: RwSignal::new(None),
        }
    }

    /// Parse `text` and update snapshot / syntax-error signals.
    pub fn reparse(&self, text: &str) {
        match parse_query_tree(text) {
            ParseOutcome::Tree(tree) => {
                self.snapshot.set(Some(Snapshot {
                    text: text.to_string(),
                    tree,
                }));
                self.syntax_error.set(None);
            }
            ParseOutcome::SyntaxError { line, col, message } => {
                self.syntax_error
                    .set(Some(format!("line {line}:{col}: {message}")));
            }
        }
    }

    /// The snapshot when it matches the buffer exactly — the precondition
    /// for span splices. `None` while the buffer is syntactically broken.
    pub fn fresh(&self) -> Option<Snapshot> {
        let snap = self.snapshot.get_untracked()?;
        (snap.text == self.buffer.get_untracked()).then_some(snap)
    }

    fn apply(&self, snap: &Snapshot, splices: Vec<Splice>) {
        let new_text = apply_splices(&snap.text, splices);
        self.buffer.set(new_text);
    }

    // ── element attribute forms ──

    /// Apply attribute writes to the element at `path` (header splice).
    pub fn set_attrs(&self, path: &[usize], updates: Vec<(String, AttrWrite)>) {
        let Some(snap) = self.fresh() else { return };
        let Some(node) = node_at(&snap.tree, path) else { return };
        let splice = header_splice(node, &updates);
        self.apply(&snap, vec![splice]);
    }

    // ── Section match / end_match rules ──

    /// Write the `match=` / `end_match=` rule of the Section at `path`.
    ///
    /// Existing `attr=Name` references rewrite the named definition's first
    /// clause in place (extra clauses are preserved). Inline strings and
    /// missing attributes are converted to a named `Match<Section>` block
    /// inserted right before the section's top-level ancestor (auto-named
    /// from the pattern, DV-012 style). `set_as` additionally rewrites the
    /// `as=` attribute (heading picks auto-slug it).
    pub fn set_section_match(
        &self,
        path: &[usize],
        attr_key: &str,
        rule: RuleSpec,
        set_as: Option<String>,
    ) {
        let Some(snap) = self.fresh() else { return };
        let Some(node) = node_at(&snap.tree, path) else { return };
        if node.kind != NodeKind::Element {
            return;
        }
        let mut splices = Vec::new();
        let mut inserted_def = false;
        let mut header_updates: Vec<(String, AttrWrite)> = Vec::new();
        if let Some(as_name) = &set_as {
            header_updates.push(("as".to_string(), AttrWrite::Set(quote_str(as_name))));
        }

        let existing_ref = attr(node, attr_key).and_then(|a| match &a.value {
            AttrValue::Ident(name) => Some(name.clone()),
            _ => None,
        });

        match existing_ref {
            Some(def_name) => {
                if let Some((_, def)) = match_def_by_name(&snap.tree, &def_name) {
                    splices.push(rule_splice(def, &rule));
                } else {
                    // Dangling reference: define it (fixes the compile error
                    // the diagnostics badge is showing).
                    let def_text = render_match_def(&def_name, "Section", &rule);
                    splices.push(top_insert_splice(&snap.text, &snap.tree, path[0], &def_text));
                    inserted_def = true;
                }
            }
            None => {
                let current_as = attr(node, "as").and_then(|a| match &a.value {
                    AttrValue::Str(s) => Some(s.clone()),
                    _ => None,
                });
                let pattern = match &rule {
                    RuleSpec::Text { pattern, .. }
                    | RuleSpec::Regex { pattern }
                    | RuleSpec::EmbeddingSim { pattern, .. } => pattern.clone(),
                    RuleSpec::Heuristic { .. } => String::new(),
                };
                let (def_name, auto_as) =
                    section_names_for(&pattern, &snap.text, current_as.as_deref());
                let def_text = render_match_def(&def_name, "Section", &rule);
                splices.push(top_insert_splice(&snap.text, &snap.tree, path[0], &def_text));
                inserted_def = true;
                header_updates.push((attr_key.to_string(), AttrWrite::Set(def_name)));
                if set_as.is_none() && current_as.is_none() && attr_key == "match" {
                    header_updates.push(("as".to_string(), AttrWrite::Set(quote_str(&auto_as))));
                }
            }
        }

        if !header_updates.is_empty() {
            splices.push(header_splice(node, &header_updates));
        }
        self.apply(&snap, splices);
        if inserted_def {
            // The new definition sits before the section at top level: shift
            // any selection at/after the insertion index so the form stays
            // on the node being edited.
            self.shift_top_selection(path[0]);
        }
    }

    /// Bump the selected path's top-level index when a node was inserted at
    /// `inserted_at` (selection at or after it moves down by one).
    fn shift_top_selection(&self, inserted_at: usize) {
        if let Some(mut selected) = self.selected.get_untracked() {
            if let Some(first) = selected.first_mut() {
                if *first >= inserted_at {
                    *first += 1;
                    self.selected.set(Some(selected));
                }
            }
        }
    }

    /// Remove `end_match=` (its definition is left in place; it may be
    /// shared and deleting it is one click on its tree node).
    pub fn remove_attr(&self, path: &[usize], attr_key: &str) {
        self.set_attrs(
            path,
            vec![(attr_key.to_string(), AttrWrite::Remove)],
        );
    }

    // ── declaration forms ──

    /// Rewrite the first clause of the Match definition at `path`.
    pub fn set_def_rule(&self, path: &[usize], rule: RuleSpec) {
        let Some(snap) = self.fresh() else { return };
        let Some(def) = node_at(&snap.tree, path) else { return };
        if def.kind != NodeKind::MatchDef {
            return;
        }
        let splice = rule_splice(def, &rule);
        self.apply(&snap, vec![splice]);
    }

    /// Rename a Match/TYPE declaration, rewriting every reference
    /// (`match=`/`end_match=` identifiers, `type=` values).
    pub fn rename_declaration(&self, path: &[usize], new_name: &str) {
        let Some(snap) = self.fresh() else { return };
        let Some(def) = node_at(&snap.tree, path) else { return };
        let Some(name_span) = def.name_span else { return };
        if new_name.is_empty()
            || !new_name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            || !new_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return; // not a valid identifier
        }
        let old_name = def.name.clone();
        let is_type = def.kind == NodeKind::TypeDef;
        let mut splices = vec![Splice {
            start: name_span.0,
            end: name_span.1,
            text: new_name.to_string(),
        }];
        collect_reference_splices(
            &snap.tree.nodes,
            &old_name,
            is_type,
            new_name,
            &mut splices,
        );
        self.apply(&snap, splices);
    }

    /// Replace the field rows of the TYPE declaration at `path`.
    pub fn set_type_fields(&self, path: &[usize], fields: Vec<(String, String)>) {
        let Some(snap) = self.fresh() else { return };
        let Some(def) = node_at(&snap.tree, path) else { return };
        if def.kind != NodeKind::TypeDef || fields.is_empty() {
            return;
        }
        let text = render_type_def(&def.name, &fields);
        self.apply(
            &snap,
            vec![Splice {
                start: def.span.0,
                end: def.span.1,
                text,
            }],
        );
    }

    /// "New TYPE…" flow: emit a TYPE declaration at the top of the buffer
    /// and point the Table at `table_path` at it (`type="Name"`).
    pub fn add_type_for_table(
        &self,
        table_path: &[usize],
        name: &str,
        fields: Vec<(String, String)>,
    ) {
        let Some(snap) = self.fresh() else { return };
        let Some(node) = node_at(&snap.tree, table_path) else { return };
        if fields.is_empty() || name.is_empty() {
            return;
        }
        let def_text = render_type_def(name, &fields);
        let splices = vec![
            top_insert_splice(&snap.text, &snap.tree, 0, &def_text),
            header_splice(
                node,
                &[("type".to_string(), AttrWrite::Set(quote_str(name)))],
            ),
        ];
        self.apply(&snap, splices);
        // The TYPE landed at top level index 0: shift the selected path.
        self.shift_top_selection(0);
    }

    // ── structural edits ──

    /// Insert a default node of `kind` at `slot` and select it.
    pub fn insert_at_slot(&self, slot: &SlotKey, kind: SlotKind) {
        let Some(snap) = self.fresh() else { return };
        let text = query_tree::insertion_text(kind, &snap.text);
        let splice = match &slot.parent {
            None => top_insert_splice(&snap.text, &snap.tree, slot.index, &text),
            Some(parent_path) => {
                let Some(parent) = node_at(&snap.tree, parent_path) else { return };
                child_insert_splice(&snap.text, parent, slot.index, &text)
            }
        };
        self.apply(&snap, vec![splice]);
        let mut new_path = slot.parent.clone().unwrap_or_default();
        new_path.push(slot.index);
        self.selected.set(Some(new_path));
    }

    /// Delete the node at `path` (and clear the selection).
    pub fn delete_node(&self, path: &[usize]) {
        let Some(snap) = self.fresh() else { return };
        let Some(node) = node_at(&snap.tree, path) else { return };
        let splice = delete_splice(&snap.text, node);
        self.apply(&snap, vec![splice]);
        self.selected.set(None);
    }

    // ── inspector-chip routing (DV-012 insert bus → V4 machinery) ──

    /// Insert a snippet spec: single-element specs land in the selected
    /// node's child slot when legal, everything else (and every spec while
    /// the buffer is broken) appends at top level.
    pub fn insert_spec(&self, spec: &SnippetSpec) {
        let buffer = self.buffer.get_untracked();
        let text = render_snippet(spec, &buffer);

        if single_element_spec(spec) {
            if let Some(snap) = self.fresh() {
                if let Some(path) = self.selected.get_untracked() {
                    if let Some(node) = node_at(&snap.tree, &path) {
                        let legal = query_tree::slot_menu(Some(node.name.as_str()))
                            .iter()
                            .any(|k| k.label() == spec_element(spec));
                        if node.kind == NodeKind::Element && legal {
                            let index = node.children.len();
                            let splice = child_insert_splice(&snap.text, node, index, &text);
                            self.apply(&snap, vec![splice]);
                            return;
                        }
                    }
                }
            }
        }

        // Top-level append (no spans needed — works even mid-syntax-error).
        let trimmed = buffer.trim_end();
        let new_text = if trimmed.is_empty() {
            format!("{text}\n")
        } else {
            format!("{trimmed}\n\n{text}\n")
        };
        self.buffer.set(new_text);
    }
}

impl Default for QueryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Splice rewriting a Match definition's first clause (or seeding an empty
/// body) with `rule`. Extra clauses keep their source verbatim.
fn rule_splice(def: &QueryNode, rule: &RuleSpec) -> Splice {
    if let Some(first) = def.rules.first() {
        Splice {
            start: first.span.0,
            end: first.span.1,
            text: render_rule(rule),
        }
    } else if let Some((b0, b1)) = def.body_span {
        Splice {
            start: b0,
            end: b1,
            text: format!("\n  {}\n", render_rule(rule)),
        }
    } else {
        // A match definition always has a body per the grammar; degenerate
        // fallback: rewrite nothing.
        Splice { start: def.span.1, end: def.span.1, text: String::new() }
    }
}

/// Collect value-span splices for every reference to a renamed declaration:
/// `match=`/`end_match=` identifier values (match defs) or `type=` values
/// (TYPEs, string or identifier form).
fn collect_reference_splices(
    nodes: &[QueryNode],
    old_name: &str,
    is_type: bool,
    new_name: &str,
    out: &mut Vec<Splice>,
) {
    for node in nodes {
        for a in &node.attrs {
            let hit = if is_type {
                a.name == "type"
                    && matches!(
                        &a.value,
                        AttrValue::Str(s) if s == old_name
                    )
                    || (a.name == "type"
                        && matches!(&a.value, AttrValue::Ident(i) if i == old_name))
            } else {
                (a.name == "match" || a.name == "end_match")
                    && matches!(&a.value, AttrValue::Ident(i) if i == old_name)
            };
            if hit {
                let text = if is_type && matches!(&a.value, AttrValue::Str(_)) {
                    quote_str(new_name)
                } else {
                    new_name.to_string()
                };
                out.push(Splice {
                    start: a.value_span.0,
                    end: a.value_span.1,
                    text,
                });
            }
        }
        collect_reference_splices(&node.children, old_name, is_type, new_name, out);
    }
}

/// Specs that render to exactly one element expression (child-slot legal).
fn single_element_spec(spec: &SnippetSpec) -> bool {
    matches!(
        spec,
        SnippetSpec::TableRef { .. } | SnippetSpec::AuxRef { .. } | SnippetSpec::PlainChunks
    )
}

fn spec_element(spec: &SnippetSpec) -> &'static str {
    match spec {
        SnippetSpec::TableRef { .. } => "Table",
        SnippetSpec::PlainChunks => "TextChunk",
        SnippetSpec::AuxRef { kind, .. } => match kind {
            crate::snippets::AuxRefKind::Annotation => "Annotation",
            crate::snippets::AuxRefKind::Figure => "Figure",
        },
        _ => "",
    }
}

/// Client-side sync host: reparses the buffer into the tree on every change
/// and consumes the DV-012 insert bus through the builder. Mounted once in
/// `App`; effects only run post-hydration (DV-009/DV-013 discipline — the
/// SSR tree renders the empty placeholder and the client fills it in).
#[component]
pub fn BuilderSync() -> impl IntoView {
    let builder = expect_context::<QueryBuilder>();

    Effect::new(move |_| {
        let text = builder.buffer.get();
        builder.reparse(&text);
    });

    if let Some(bus) = use_context::<crate::components::insert::InsertBus>() {
        Effect::new(move |_| {
            let Some((spec, _nonce)) = bus.0.get() else {
                return;
            };
            builder.insert_spec(&spec);
            bus.0.set(None); // consumed — re-runs must not re-insert
        });
    }

    ().into_view()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leptos::prelude::Owner;

    /// Signals need an active reactive Owner under the ssr feature's
    /// sandboxed arenas; keep it alive for the test body.
    fn fresh_builder(text: &str) -> (Owner, QueryBuilder) {
        let owner = Owner::new();
        owner.set();
        let b = QueryBuilder::new();
        b.buffer.set(text.to_string());
        b.reparse(text);
        (owner, b)
    }

    #[test]
    fn reparse_keeps_last_good_tree_across_syntax_errors() {
        let (_owner, b) = fresh_builder("Table(as=\"t\")");
        assert_eq!(b.snapshot.get_untracked().unwrap().tree.nodes.len(), 1);
        assert!(b.syntax_error.get_untracked().is_none());
        b.buffer.set("Table(as=".to_string());
        b.reparse("Table(as=");
        assert!(b.syntax_error.get_untracked().is_some());
        // Snapshot still holds the last good tree…
        assert_eq!(b.snapshot.get_untracked().unwrap().tree.nodes.len(), 1);
        // …but no longer matches the buffer, so edits are gated off.
        assert!(b.fresh().is_none());
    }

    #[test]
    fn set_section_match_creates_named_def_and_sets_as() {
        let src = "Section(as=\"section1\") {\n}";
        let (_owner, b) = fresh_builder(src);
        b.selected.set(Some(vec![0]));
        b.set_section_match(
            &[0],
            "match",
            RuleSpec::Text { pattern: "OVERVIEW".into(), threshold: 0.6 },
            Some("overview".into()),
        );
        // The new definition was inserted before the section: the selection
        // follows the section to its new index.
        assert_eq!(b.selected.get_untracked(), Some(vec![1]));
        let out = b.buffer.get_untracked();
        // Existing attributes keep their source position; new ones append
        // (as= was already present, match= is new).
        assert_eq!(
            out,
            "Match<Section> Overview {\n  Text(\"OVERVIEW\", threshold=0.6)\n}\n\n\
             Section(as=\"overview\", match=Overview) {\n}"
        );
        b.reparse(&out);
        assert!(b.snapshot.get_untracked().unwrap().tree.compile.is_none());
    }

    #[test]
    fn set_section_match_rewrites_existing_def_in_place() {
        let src = "Match<Section> MDA {\n  Text(\"Management\", threshold=0.6)\n  Regex(\"keepme\")\n}\n\nSection(match=MDA, as=\"mda\") {\n}";
        let (_owner, b) = fresh_builder(src);
        b.set_section_match(
            &[1],
            "match",
            RuleSpec::Regex { pattern: "M.*A".into() },
            None,
        );
        let out = b.buffer.get_untracked();
        assert!(out.contains("Match<Section> MDA {\n  Regex(\"M.*A\")\n  Regex(\"keepme\")\n}"), "got: {out}");
        assert!(out.contains("Section(match=MDA, as=\"mda\")"));
    }

    #[test]
    fn end_match_gets_its_own_def_without_touching_as() {
        let src = "Match<Section> Overview {\n  Text(\"OVERVIEW\", threshold=0.6)\n}\n\nSection(match=Overview, as=\"overview\") {\n}";
        let (_owner, b) = fresh_builder(src);
        b.set_section_match(
            &[1],
            "end_match",
            RuleSpec::Text { pattern: "RESULTS".into(), threshold: 0.6 },
            None,
        );
        let out = b.buffer.get_untracked();
        assert!(out.contains("Match<Section> Results {\n  Text(\"RESULTS\", threshold=0.6)\n}"), "got: {out}");
        assert!(out.contains("end_match=Results"));
        assert!(out.contains("as=\"overview\""));
        b.reparse(&out);
        assert!(b.snapshot.get_untracked().unwrap().tree.compile.is_none(), "{out}");
    }

    #[test]
    fn rename_declaration_rewrites_references() {
        let src = "TYPE Seg AS TABLE (\n  metric TEXT,\n);\n\nMatch<Section> MDA {\n  Text(\"x\")\n}\n\nSection(match=MDA, as=\"s\") {\n  Table(as=\"t\", type=\"Seg\")\n}";
        let (_owner, b) = fresh_builder(src);
        b.rename_declaration(&[1], "Discussion");
        let out = b.buffer.get_untracked();
        assert!(out.contains("Match<Section> Discussion {"));
        assert!(out.contains("match=Discussion"));
        b.reparse(&out);
        b.rename_declaration(&[0], "Segments");
        let out = b.buffer.get_untracked();
        assert!(out.contains("TYPE Segments AS TABLE"));
        assert!(out.contains("type=\"Segments\""));
        b.reparse(&out);
        assert!(b.snapshot.get_untracked().unwrap().tree.compile.is_none(), "{out}");
        // Invalid identifiers are rejected.
        b.rename_declaration(&[0], "9bad");
        assert!(b.buffer.get_untracked().contains("TYPE Segments"));
    }

    #[test]
    fn insert_at_slot_and_delete_round_trip() {
        let (_owner, b) = fresh_builder("");
        b.insert_at_slot(&SlotKey { parent: None, index: 0 }, SlotKind::Section);
        let out = b.buffer.get_untracked();
        assert_eq!(out, "Section(as=\"section1\") {\n}\n");
        assert_eq!(b.selected.get_untracked(), Some(vec![0]));
        b.reparse(&out);

        b.insert_at_slot(
            &SlotKey { parent: Some(vec![0]), index: 0 },
            SlotKind::TextChunk,
        );
        let out = b.buffer.get_untracked();
        assert_eq!(
            out,
            "Section(as=\"section1\") {\n  TextChunk(chunkSize=500, chunkOverlap=150)\n}\n"
        );
        assert_eq!(b.selected.get_untracked(), Some(vec![0, 0]));
        b.reparse(&out);

        b.delete_node(&[0, 0]);
        let out = b.buffer.get_untracked();
        assert_eq!(out, "Section(as=\"section1\") {\n}\n");
    }

    #[test]
    fn add_type_for_table_emits_decl_at_top_and_sets_type() {
        let src = "Table(as=\"table_p26\")";
        let (_owner, b) = fresh_builder(src);
        b.selected.set(Some(vec![0]));
        b.add_type_for_table(
            &[0],
            "TableP26",
            vec![("metric".into(), "TEXT".into()), ("c2015".into(), "DECIMAL".into())],
        );
        let out = b.buffer.get_untracked();
        assert!(out.starts_with("TYPE TableP26 AS TABLE (\n  metric TEXT,\n  c2015 DECIMAL,\n);"), "got: {out}");
        assert!(out.contains("Table(as=\"table_p26\", type=\"TableP26\")"));
        // Selection shifted past the inserted declaration.
        assert_eq!(b.selected.get_untracked(), Some(vec![1]));
        b.reparse(&out);
        assert!(b.snapshot.get_untracked().unwrap().tree.compile.is_none());
    }

    #[test]
    fn insert_spec_routes_to_selected_section_else_top() {
        let src = "Section(as=\"s\") {\n}";
        let (_owner, b) = fresh_builder(src);
        b.selected.set(Some(vec![0]));
        b.insert_spec(&SnippetSpec::TableRef { page: 26 });
        let out = b.buffer.get_untracked();
        assert_eq!(out, "Section(as=\"s\") {\n  Table(as=\"table_p26\")\n}");
        b.reparse(&out);

        // Declaration-bearing specs always go top level.
        b.insert_spec(&SnippetSpec::TextMatch { text: "OVERVIEW".into() });
        let out = b.buffer.get_untracked();
        assert!(out.ends_with("Match<Section> Section1 {\n  Text(\"OVERVIEW\", threshold=0.6)\n}\n"), "got: {out}");
        b.reparse(&out);

        // No selection → top level.
        b.selected.set(None);
        b.insert_spec(&SnippetSpec::PlainChunks);
        let out = b.buffer.get_untracked();
        assert!(out.ends_with("TextChunk(chunkSize=500, chunkOverlap=150)\n"));
        b.reparse(&out);
        assert_eq!(b.snapshot.get_untracked().unwrap().tree.nodes.len(), 3);
    }

    #[test]
    fn stale_buffer_gates_all_edits() {
        let (_owner, b) = fresh_builder("Table(as=\"t\")");
        // Simulate an unparsed manual edit.
        b.buffer.set("Table(as=\"t\") garbage(".to_string());
        b.set_attrs(&[0], vec![("as".into(), AttrWrite::Set("\"x\"".into()))]);
        assert_eq!(b.buffer.get_untracked(), "Table(as=\"t\") garbage(");
    }
}
