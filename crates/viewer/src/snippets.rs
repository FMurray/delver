//! Discover-mode snippet generation (slice V2, DV-012): pure functions that
//! turn a clicked page element (or a palette entry) into DocQL source ready
//! to insert at the editor cursor.
//!
//! Everything here is target-independent (no DOM, no DB) so the generators
//! run identically on the server (palette column specs) and in the WASM
//! client (side-panel insert actions), and are unit-testable with
//! `cargo test -p viewer --lib`.

use serde::{Deserialize, Serialize};

/// Max characters of element text carried into a generated `Text("…")`
/// pattern (fuzzy matching tolerates the truncation; an ellipsis would not).
pub const MATCH_TEXT_MAX_CHARS: usize = 80;

// ───────────────────────── snippet specs ─────────────────────────

/// What to insert, decided at the click site; rendered against the current
/// editor buffer at insertion time so generated names stay unique.
#[derive(Debug, Clone, PartialEq)]
pub enum SnippetSpec {
    /// `Match<Section> SectionN { Text("…", threshold=0.6) }`
    TextMatch { text: String },
    /// TextMatch plus a wrapping `Section(match=…) { TextChunk(…) }`.
    SectionScaffold { text: String },
    /// TextMatch plus a wrapping `Section(match=…) { Table(as="…") }`.
    SectionWithTable { text: String },
    /// `Table(as="table_p<page>")`.
    TableRef { page: i32 },
    /// `TYPE TableP<page> AS TABLE ( … );` + `Table(as=…, type=…)`.
    TypedTable { page: i32, columns: Vec<ColumnSpec> },
    /// `Annotation(as="…")` / `Figure(as="…")`.
    AuxRef { kind: AuxRefKind, page: i32 },
    /// `TextChunk(chunkSize=500, chunkOverlap=150)`.
    PlainChunks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxRefKind {
    Annotation,
    Figure,
}

impl AuxRefKind {
    fn element(self) -> &'static str {
        match self {
            AuxRefKind::Annotation => "Annotation",
            AuxRefKind::Figure => "Figure",
        }
    }

    fn as_prefix(self) -> &'static str {
        match self {
            AuxRefKind::Annotation => "annotation",
            AuxRefKind::Figure => "figure",
        }
    }
}

/// One usable (non-filler) table column, in left-to-right order: the header
/// cell text (when a header row was detected) and whether the body cells are
/// numeric-ish. Produced by [`column_specs`] on either side of the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnSpec {
    /// Trimmed header-cell text; `None` when absent/empty.
    pub header: Option<String>,
    /// 1-based original column index (fallback field name `col<index>`).
    pub index: usize,
    /// True when the column's non-empty body cells are mostly numeric-ish
    /// (D-021 coercion conventions) → field type DECIMAL, else TEXT.
    pub numeric: bool,
}

/// Render a spec into DocQL source. `buffer` is the current editor text;
/// every generated identifier is made unique against it (and within the
/// snippet itself).
pub fn render_snippet(spec: &SnippetSpec, buffer: &str) -> String {
    match spec {
        SnippetSpec::TextMatch { text } => {
            let (match_name, _) = numbered_section_names(buffer);
            text_match_block(&match_name, text)
        }
        SnippetSpec::SectionScaffold { text } => {
            let (match_name, as_name) = numbered_section_names(buffer);
            format!(
                "{}\n\nSection(match={match_name}, as=\"{as_name}\") {{\n  TextChunk(chunkSize=500, chunkOverlap=150)\n}}",
                text_match_block(&match_name, text),
            )
        }
        SnippetSpec::SectionWithTable { text } => {
            let (match_name, as_name) = numbered_section_names(buffer);
            format!(
                "{}\n\nSection(match={match_name}, as=\"{as_name}\") {{\n  Table(as=\"{as_name}_tables\")\n}}",
                text_match_block(&match_name, text),
            )
        }
        SnippetSpec::TableRef { page } => {
            let name = uniquify(buffer, &format!("table_p{page}"));
            format!("Table(as=\"{name}\")")
        }
        SnippetSpec::TypedTable { page, columns } => typed_table_snippet(buffer, *page, columns),
        SnippetSpec::AuxRef { kind, page } => {
            let name = uniquify(buffer, &format!("{}_p{page}", kind.as_prefix()));
            format!("{}(as=\"{name}\")", kind.element())
        }
        SnippetSpec::PlainChunks => "TextChunk(chunkSize=500, chunkOverlap=150)".to_string(),
    }
}

fn text_match_block(match_name: &str, text: &str) -> String {
    let pattern = escape_docql_string(&elide(text, MATCH_TEXT_MAX_CHARS));
    format!("Match<Section> {match_name} {{\n  Text(\"{pattern}\", threshold=0.6)\n}}")
}

/// `TYPE TableP<page> AS TABLE ( … );` + a `Table` element using it. Field
/// names are slugified header texts (fallback `col<index>`), deduplicated;
/// field types DECIMAL for numeric-ish columns, else TEXT.
fn typed_table_snippet(buffer: &str, page: i32, columns: &[ColumnSpec]) -> String {
    let (type_name, as_name) = paired_unique(
        buffer,
        &format!("TableP{page}"),
        &format!("table_p{page}"),
    );
    let mut fields: Vec<String> = Vec::new();
    let mut lines = String::new();
    for col in columns {
        let base = col
            .header
            .as_deref()
            .map(slug_identifier)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("col{}", col.index));
        let mut name = base.clone();
        let mut n = 2;
        while fields.iter().any(|f| f == &name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        fields.push(name.clone());
        let ty = if col.numeric { "DECIMAL" } else { "TEXT" };
        lines.push_str(&format!("  {name} {ty},\n"));
    }
    format!(
        "TYPE {type_name} AS TABLE (\n{lines});\n\nTable(as=\"{as_name}\", type=\"{type_name}\")"
    )
}

// ───────────────────────── text helpers ─────────────────────────

/// Collapse all whitespace runs (incl. newlines) to single spaces, trim, and
/// truncate to `max` characters (char-boundary safe, no ellipsis — the
/// truncated prefix still fuzzy-matches; an added ellipsis would not).
pub fn elide(text: &str, max: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(max).collect::<String>().trim_end().to_string()
}

/// Escape for a DocQL string literal (grammar `char` rule): backslash and
/// double quote. Other characters — including non-ASCII like `’` or `—` —
/// are valid raw, and [`elide`] has already removed control whitespace.
pub fn escape_docql_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Drop LSP snippet placeholders (`$0`, `$1`, …) from an insert text —
/// CodeMirror 5 hints insert plain strings. A `$` not followed by digits is
/// kept verbatim.
pub fn strip_snippet_placeholders(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            let mut saw_digit = false;
            while chars.peek().is_some_and(|d| d.is_ascii_digit()) {
                chars.next();
                saw_digit = true;
            }
            if !saw_digit {
                out.push('$');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Lowercased identifier slug: alphanumerics kept, everything else collapses
/// to single underscores; digit-leading slugs get a `c` prefix (keeps the
/// D-021 fuzzy header match within its 0.8 similarity boundary, e.g.
/// `c2015` ↔ "2015"); capped at 30 chars; may return "" (caller falls back).
pub fn slug_identifier(text: &str) -> String {
    let mut slug = String::new();
    let mut pending_sep = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_sep && !slug.is_empty() {
                slug.push('_');
            }
            pending_sep = false;
            slug.push(ch.to_ascii_lowercase());
            if slug.len() >= 30 {
                break;
            }
        } else {
            pending_sep = true;
        }
    }
    if slug.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        slug.insert(0, 'c');
    }
    slug
}

/// True when `ident` occurs in `buffer` as a whole word (identifier-char
/// boundaries on both sides).
pub fn buffer_contains_ident(buffer: &str, ident: &str) -> bool {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut start = 0;
    while let Some(pos) = buffer[start..].find(ident) {
        let at = start + pos;
        let end = at + ident.len();
        let before_ok = at == 0 || !buffer[..at].chars().next_back().is_some_and(is_word);
        let after_ok = end == buffer.len() || !buffer[end..].chars().next().is_some_and(is_word);
        if before_ok && after_ok {
            return true;
        }
        start = at + 1;
    }
    false
}

/// `base` when free in `buffer`, else `base_2`, `base_3`, ….
pub fn uniquify(buffer: &str, base: &str) -> String {
    if !buffer_contains_ident(buffer, base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}_{n}");
        if !buffer_contains_ident(buffer, &candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Smallest N ≥ 1 such that both `SectionN` (match name) and `sectionN`
/// (`as=` name) are free in the buffer.
pub fn numbered_section_names(buffer: &str) -> (String, String) {
    let mut n = 1;
    loop {
        let upper = format!("Section{n}");
        let lower = format!("section{n}");
        if !buffer_contains_ident(buffer, &upper) && !buffer_contains_ident(buffer, &lower) {
            return (upper, lower);
        }
        n += 1;
    }
}

/// Two related names (e.g. TYPE name + `as=` name) sharing one uniqueness
/// suffix so they stay visually paired.
fn paired_unique(buffer: &str, base_a: &str, base_b: &str) -> (String, String) {
    if !buffer_contains_ident(buffer, base_a) && !buffer_contains_ident(buffer, base_b) {
        return (base_a.to_string(), base_b.to_string());
    }
    let mut n = 2;
    loop {
        let a = format!("{base_a}_{n}");
        let b = format!("{base_b}_{n}");
        if !buffer_contains_ident(buffer, &a) && !buffer_contains_ident(buffer, &b) {
            return (a, b);
        }
        n += 1;
    }
}

// ───────────────────────── table column inference ─────────────────────────

/// Minimal cell shape shared by the client DTO (`CellOverlay`) and the
/// server's `table_cells` rows.
#[derive(Debug, Clone)]
pub struct CellLite {
    pub row: i32,
    pub col: i32,
    pub text: Option<String>,
    pub is_header: bool,
}

/// True when the cell text coerces under the D-021 INT/DECIMAL conventions:
/// strip a trailing `%`, treat surrounding parens as negative, then drop
/// `$`/`,`/whitespace and parse the remainder as a number. Empty → false.
pub fn is_numericish(text: &str) -> bool {
    let mut s = text.trim();
    if let Some(stripped) = s.strip_suffix('%') {
        s = stripped.trim_end();
    }
    if s.starts_with('(') && s.ends_with(')') && s.len() >= 2 {
        s = &s[1..s.len() - 1];
    }
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '$' && *c != ',')
        .collect();
    !cleaned.is_empty() && cleaned.parse::<f64>().is_ok()
}

/// True when every body cell of the column is empty or consists solely of
/// `$ % ( )` / whitespace — the SEC `$`-filler pattern D-021 excludes from
/// column mapping; generated TYPEs skip them too so every declared field can
/// claim a real column.
fn is_filler(body_texts: &[&str]) -> bool {
    body_texts
        .iter()
        .all(|t| t.chars().all(|c| c.is_whitespace() || "$%()".contains(c)))
}

/// Per-column specs for a table's cells: filler columns dropped, header text
/// from `is_header` cells (first header row wins), numeric-ness by strict
/// majority over non-empty body cells.
pub fn column_specs(n_cols: usize, cells: &[CellLite]) -> Vec<ColumnSpec> {
    let mut specs = Vec::new();
    for col in 0..n_cols {
        let col_cells: Vec<&CellLite> = cells
            .iter()
            .filter(|c| c.col >= 0 && c.col as usize == col)
            .collect();
        let header = col_cells
            .iter()
            .filter(|c| c.is_header)
            .filter_map(|c| c.text.as_deref())
            .map(str::trim)
            .find(|t| !t.is_empty())
            .map(str::to_string);
        let body_texts: Vec<&str> = col_cells
            .iter()
            .filter(|c| !c.is_header)
            .map(|c| c.text.as_deref().unwrap_or("").trim())
            .collect();
        if is_filler(&body_texts) && header.is_none() {
            continue;
        }
        let non_empty: Vec<&&str> = body_texts.iter().filter(|t| !t.is_empty()).collect();
        let numeric_count = non_empty.iter().filter(|t| is_numericish(t)).count();
        let numeric = !non_empty.is_empty() && 2 * numeric_count > non_empty.len();
        specs.push(ColumnSpec {
            header,
            index: col + 1,
            numeric,
        });
    }
    specs
}

// ───────────────────────── heading heuristic ─────────────────────────

/// One candidate line for the heading heuristic (already length-filtered by
/// the caller or not — [`select_headings`] re-checks).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadingInput {
    /// 1-based store page.
    pub page: i32,
    pub order_idx: i32,
    pub text: String,
    pub font_size: f32,
    pub font_name: Option<String>,
}

/// Heading-candidate selection (DV-012, documented heuristic):
/// short text lines (3..=80 chars, ≥1 ASCII letter) that are either
/// - **size-prominent**: font_size ≥ body_size + 1.5pt, or
/// - **bold-emphasis**: bold font while the document's dominant (modal) style
///   is not bold, font_size ≥ body_size − 0.5pt, and ALL-CAPS text — the SEC
///   HTML-to-PDF convention where headings share the body size (the 3M 10-K's
///   "PERFORMANCE BY BUSINESS SEGMENT" is Times-Bold at body 13pt).
/// Ordered by font_size desc, then (page, order_idx); deduplicated on
/// lowercased text (first occurrence wins); capped at `cap`.
pub fn select_headings(
    pool: &[HeadingInput],
    body_size: f32,
    body_is_bold: bool,
    cap: usize,
) -> Vec<HeadingInput> {
    let mut picked: Vec<HeadingInput> = pool
        .iter()
        .filter(|h| {
            let text = h.text.trim();
            let len = text.chars().count();
            if !(3..=80).contains(&len) || !text.chars().any(|c| c.is_ascii_alphabetic()) {
                return false;
            }
            let size_prominent = h.font_size >= body_size + 1.5;
            let bold = h
                .font_name
                .as_deref()
                .is_some_and(|f| f.to_ascii_lowercase().contains("bold"));
            let all_caps = text == text.to_uppercase();
            let bold_emphasis =
                !body_is_bold && bold && h.font_size >= body_size - 0.5 && all_caps;
            size_prominent || bold_emphasis
        })
        .cloned()
        .collect();
    picked.sort_by(|a, b| {
        b.font_size
            .partial_cmp(&a.font_size)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.page.cmp(&b.page))
            .then(a.order_idx.cmp(&b.order_idx))
    });
    let mut seen = std::collections::HashSet::new();
    picked.retain(|h| seen.insert(h.text.trim().to_lowercase()));
    picked.truncate(cap);
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── escaping + elision ──

    #[test]
    fn elide_collapses_whitespace_and_truncates_on_char_boundary() {
        assert_eq!(elide("  a\n b\tc  ", 80), "a b c");
        let long = "x".repeat(100);
        assert_eq!(elide(&long, 80).chars().count(), 80);
        // Multi-byte chars: truncation counts chars, not bytes.
        let uni = "é".repeat(100);
        assert_eq!(elide(&uni, 80).chars().count(), 80);
    }

    #[test]
    fn escape_handles_quotes_and_backslashes() {
        assert_eq!(escape_docql_string(r#"a "b" c\d"#), r#"a \"b\" c\\d"#);
        // Non-ASCII passes through raw (valid per the grammar char rule).
        assert_eq!(escape_docql_string("em — dash ’"), "em — dash ’");
    }

    #[test]
    fn strip_placeholders_removes_dollar_digit_markers() {
        assert_eq!(
            strip_snippet_placeholders("Section(match=$1, as=\"$2\") {\n  $0\n}"),
            "Section(match=, as=\"\") {\n  \n}"
        );
        assert_eq!(strip_snippet_placeholders("price in $ USD"), "price in $ USD");
        assert_eq!(strip_snippet_placeholders("TEXT"), "TEXT");
    }

    #[test]
    fn text_match_snippet_escapes_and_elides() {
        let text = format!("Heading \"quoted\" {}", "y".repeat(100));
        let out = render_snippet(&SnippetSpec::TextMatch { text }, "");
        assert!(out.starts_with("Match<Section> Section1 {"));
        assert!(out.contains(r#"Text("Heading \"quoted\" yyy"#));
        assert!(out.contains("threshold=0.6"));
        // Pattern is elided to ≤ MATCH_TEXT_MAX_CHARS chars (escaping may
        // add bytes but not source chars beyond the escape prefix).
        let pattern = out.split('"').nth(1).unwrap_or_default();
        assert!(pattern.chars().count() <= MATCH_TEXT_MAX_CHARS + 2); // +2 for the \" escapes
    }

    // ── identifier slugging + uniqueness ──

    #[test]
    fn slug_identifier_rules() {
        assert_eq!(slug_identifier("Sales (millions)"), "sales_millions");
        assert_eq!(slug_identifier("2015"), "c2015"); // digit-leading prefix
        assert_eq!(slug_identifier("  %$  "), ""); // nothing usable
        assert_eq!(slug_identifier("Net—income"), "net_income");
        assert!(slug_identifier(&"long header ".repeat(20)).len() <= 31);
    }

    #[test]
    fn uniquify_suffixes_against_buffer_words() {
        assert_eq!(uniquify("", "table_p26"), "table_p26");
        assert_eq!(
            uniquify("Table(as=\"table_p26\")", "table_p26"),
            "table_p26_2"
        );
        // Substring occurrences do not count as collisions.
        assert_eq!(uniquify("table_p261", "table_p26"), "table_p26");
        let buf = "table_p26 table_p26_2";
        assert_eq!(uniquify(buf, "table_p26"), "table_p26_3");
    }

    #[test]
    fn numbered_section_names_skip_taken_pairs() {
        assert_eq!(
            numbered_section_names(""),
            ("Section1".to_string(), "section1".to_string())
        );
        let buf = "Match<Section> Section1 { Text(\"x\") }\nSection(match=Section1, as=\"section1\") {}";
        assert_eq!(
            numbered_section_names(buf),
            ("Section2".to_string(), "section2".to_string())
        );
        // Either half being taken burns the number.
        assert_eq!(
            numbered_section_names("section1"),
            ("Section2".to_string(), "section2".to_string())
        );
    }

    // ── numeric inference + column specs ──

    #[test]
    fn numericish_follows_d021_conventions() {
        for ok in ["10,328", "(7.3)", "(6.0)%", "21.9 %", "$1,234", "0.6", "-3"] {
            assert!(is_numericish(ok), "{ok:?} should be numeric-ish");
        }
        for bad in ["", "—", "$", "Sales (millions)", "N/A"] {
            assert!(!is_numericish(bad), "{bad:?} should not be numeric-ish");
        }
    }

    fn cell(row: i32, col: i32, text: &str, is_header: bool) -> CellLite {
        CellLite {
            row,
            col,
            text: if text.is_empty() { None } else { Some(text.to_string()) },
            is_header,
        }
    }

    /// The p26 segment-table shape: label column without header, `$` filler
    /// columns, year columns numeric with one em-dash nil.
    fn p26_like_cells() -> Vec<CellLite> {
        vec![
            cell(0, 0, "", true),
            cell(0, 1, "", true),
            cell(0, 2, "2015", true),
            cell(0, 3, "", true),
            cell(0, 4, "2014", true),
            cell(1, 0, "Sales (millions)", false),
            cell(1, 1, "$", false),
            cell(1, 2, "10,328", false),
            cell(1, 3, "$", false),
            cell(1, 4, "10,990", false),
            cell(2, 0, "Acquisitions", false),
            cell(2, 1, "", false),
            cell(2, 2, "0.6", false),
            cell(2, 3, "", false),
            cell(2, 4, "—", false),
            cell(3, 0, "Total sales change", false),
            cell(3, 1, "", false),
            cell(3, 2, "(6.0)%", false),
            cell(3, 3, "", false),
            cell(3, 4, "3.1 %", false),
        ]
    }

    #[test]
    fn column_specs_drop_filler_and_infer_types() {
        let specs = column_specs(5, &p26_like_cells());
        assert_eq!(specs.len(), 3); // $ columns 1 and 3 dropped
        assert_eq!(specs[0], ColumnSpec { header: None, index: 1, numeric: false });
        assert_eq!(
            specs[1],
            ColumnSpec { header: Some("2015".to_string()), index: 3, numeric: true }
        );
        // Majority rule: em-dash nil does not flip a numeric column.
        assert_eq!(
            specs[2],
            ColumnSpec { header: Some("2014".to_string()), index: 5, numeric: true }
        );
    }

    #[test]
    fn typed_table_snippet_from_p26_like_columns() {
        let columns = column_specs(5, &p26_like_cells());
        let out = render_snippet(&SnippetSpec::TypedTable { page: 26, columns }, "");
        assert_eq!(
            out,
            "TYPE TableP26 AS TABLE (\n  col1 TEXT,\n  c2015 DECIMAL,\n  c2014 DECIMAL,\n);\n\n\
             Table(as=\"table_p26\", type=\"TableP26\")"
        );
    }

    #[test]
    fn typed_table_dedupes_field_names_and_uniquifies_type_name() {
        let columns = vec![
            ColumnSpec { header: Some("Total".into()), index: 1, numeric: true },
            ColumnSpec { header: Some("total".into()), index: 2, numeric: true },
            ColumnSpec { header: None, index: 3, numeric: false },
        ];
        let buffer = "TYPE TableP26 AS TABLE ( x TEXT );\nTable(as=\"table_p26\", type=\"TableP26\")";
        let out = render_snippet(&SnippetSpec::TypedTable { page: 26, columns }, buffer);
        assert!(out.contains("TYPE TableP26_2 AS TABLE ("));
        assert!(out.contains("  total DECIMAL,\n  total_2 DECIMAL,\n  col3 TEXT,\n"));
        assert!(out.contains("Table(as=\"table_p26_2\", type=\"TableP26_2\")"));
    }

    // ── other snippet shapes ──

    #[test]
    fn section_scaffold_emits_match_plus_section() {
        let out = render_snippet(
            &SnippetSpec::SectionScaffold { text: "OVERVIEW".into() },
            "",
        );
        assert_eq!(
            out,
            "Match<Section> Section1 {\n  Text(\"OVERVIEW\", threshold=0.6)\n}\n\n\
             Section(match=Section1, as=\"section1\") {\n  TextChunk(chunkSize=500, chunkOverlap=150)\n}"
        );
    }

    #[test]
    fn section_with_table_and_aux_and_table_ref() {
        let out = render_snippet(
            &SnippetSpec::SectionWithTable { text: "RESULTS".into() },
            "",
        );
        assert!(out.contains("Table(as=\"section1_tables\")"));
        assert_eq!(
            render_snippet(&SnippetSpec::TableRef { page: 26 }, ""),
            "Table(as=\"table_p26\")"
        );
        assert_eq!(
            render_snippet(
                &SnippetSpec::AuxRef { kind: AuxRefKind::Annotation, page: 3 },
                ""
            ),
            "Annotation(as=\"annotation_p3\")"
        );
        assert_eq!(
            render_snippet(
                &SnippetSpec::AuxRef { kind: AuxRefKind::Figure, page: 7 },
                "figure_p7"
            ),
            "Figure(as=\"figure_p7_2\")"
        );
        assert_eq!(
            render_snippet(&SnippetSpec::PlainChunks, "anything"),
            "TextChunk(chunkSize=500, chunkOverlap=150)"
        );
    }

    // ── heading heuristic ──

    fn h(page: i32, order: i32, text: &str, size: f32, font: &str) -> HeadingInput {
        HeadingInput {
            page,
            order_idx: order,
            text: text.to_string(),
            font_size: size,
            font_name: Some(font.to_string()),
        }
    }

    #[test]
    fn headings_pick_size_prominent_and_bold_caps_lines() {
        let pool = vec![
            h(1, 0, "3M COMPANY", 21.0, "Times-Roman"),          // size-prominent
            h(5, 1, "body sentence fragment here", 13.0, "Times-Roman"), // body
            h(24, 2, "PERFORMANCE BY BUSINESS SEGMENT", 13.0, "Times-Bold"), // bold caps
            h(30, 3, "Bold but not caps", 13.0, "Times-Bold"),   // rejected (not caps)
            h(40, 4, "BOLD CAPS TINY", 9.0, "Times-Bold"),       // rejected (below body-0.5)
        ];
        let picked = select_headings(&pool, 13.0, false, 20);
        let texts: Vec<&str> = picked.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(texts, vec!["3M COMPANY", "PERFORMANCE BY BUSINESS SEGMENT"]);
    }

    #[test]
    fn headings_dedupe_cap_and_filter_lengths() {
        let mut pool = vec![
            h(2, 0, "OVERVIEW", 13.0, "Times-Bold"),
            h(16, 1, "Overview", 21.0, "Times-Roman"), // same text (case-insensitive)
            h(3, 2, "AB", 21.0, "Times-Roman"),        // too short
            h(3, 3, "1234", 21.0, "Times-Roman"),      // no letters
        ];
        for i in 0..30 {
            pool.push(h(50 + i, i, &format!("HEADING NUMBER {i}"), 18.0, "Times-Roman"));
        }
        let picked = select_headings(&pool, 13.0, false, 20);
        assert_eq!(picked.len(), 20);
        // The 21pt duplicate wins over the 13pt bold one; only one survives.
        assert_eq!(
            picked.iter().filter(|h| h.text.eq_ignore_ascii_case("overview")).count(),
            1
        );
        assert_eq!(picked[0].text, "Overview");
        // When the dominant body style is itself bold, bold-emphasis is off.
        let bold_doc = vec![h(1, 0, "SOME BOLD LINE", 13.0, "Helvetica-Bold")];
        assert!(select_headings(&bold_doc, 13.0, true, 20).is_empty());
    }
}
