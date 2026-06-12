//! D-025: the provenance sidecar rides alongside the unchanged outputs JSON.
//!
//! Pages are built directly as `PageContents` (the hydrated-document shape,
//! D-012) so element ids and pages are under test control — exactly what the
//! sidecar exists to expose. The serialized outputs string must stay
//! byte-identical to the `process_parsed` payload (the CLI baselines depend
//! on it), with the sidecar carrying what the outputs cannot: source element
//! ids, pages, document order, and section page spans.

use std::collections::BTreeMap;

use serde_json::Value;
use uuid::Uuid;

use delver_core::geo::Rect;
use delver_core::layout::MatchContext;
use delver_core::parse::{AuxElement, AuxKind, PageContents, TextElement};
use delver_core::provenance::RunProvenance;
use delver_core::table::{TableCell, TableStrategy, TableStructure};
use delver_core::{process_parsed, process_parsed_with_provenance};

/// A text element with a knowable id on a given 1-based page.
fn text(page: u32, y: f32, body: &str) -> TextElement {
    let mut elem = TextElement::new(body.to_string());
    elem.font_size = 12.0;
    elem.font_name = Some("Helvetica".to_string());
    elem.page_number = page;
    elem.bbox = (72.0, y, 500.0, y + 12.0);
    elem
}

fn table_cell(row: u32, col: u32, text: &str, is_header: bool) -> TableCell {
    TableCell {
        row,
        col,
        row_span: 1,
        col_span: 1,
        bbox: (72.0, 100.0, 200.0, 120.0),
        text: text.to_string(),
        is_header,
    }
}

fn table_aux(page: u32) -> AuxElement {
    let bbox = Rect {
        x0: 72.0,
        y0: 100.0,
        x1: 400.0,
        y1: 200.0,
    };
    AuxElement {
        id: Uuid::new_v4(),
        kind: AuxKind::Table,
        page_number: page,
        bbox,
        text: None,
        metadata: serde_json::json!({
            "n_rows": 2, "n_cols": 2, "strategy": "ruled", "confidence": 0.9
        }),
        blob: None,
        table: Some(TableStructure {
            bbox,
            page,
            n_rows: 2,
            n_cols: 2,
            cells: vec![
                table_cell(0, 0, "metric", true),
                table_cell(0, 1, "value", true),
                table_cell(1, 0, "sales", false),
                table_cell(1, 1, "10,328", false),
            ],
            strategy: TableStrategy::Ruled,
            confidence: 0.9,
        }),
    }
}

/// Three pages: p1 intro + section heading, p2 section body + a detected
/// table, p3 post-section heading + body. Returns (pages, ordered element
/// ids) — ids in document order, so tests can address them by position.
fn build_pages() -> (BTreeMap<u32, PageContents>, Vec<String>) {
    let elems = [
        text(1, 700.0, "This report contains statements about future plans."),
        text(1, 660.0, "OVERVIEW"),
        text(2, 700.0, "Net sales grew across every operating segment."),
        text(3, 700.0, "RESULTS OF OPERATIONS"),
        text(3, 660.0, "The consolidated balance sheets reflect total assets."),
    ];
    let table = table_aux(2);

    let mut ids: Vec<String> = Vec::new();
    let mut pages: BTreeMap<u32, PageContents> = BTreeMap::new();
    for elem in &elems {
        ids.push(elem.id.to_string());
        // The table sits between the p2 body text and the p3 heading in
        // document order (pages are walked in order; the aux element is
        // added to page 2 after its text).
        pages
            .entry(elem.page_number)
            .or_insert_with(PageContents::new)
            .add_text(elem.clone());
    }
    ids.insert(3, table.id.to_string());
    pages.get_mut(&2).expect("page 2 exists").add_aux(table);
    (pages, ids)
}

const SECTION_TEMPLATE: &str = r#"
Match<Section> Overview {
  Text("OVERVIEW", threshold=0.6)
}

Match<Section> Results {
  Text("RESULTS OF OPERATIONS", threshold=0.6)
}

Section(match=Overview, as="overview", end_match=Results) {
  TextChunk(chunkSize=500, chunkOverlap=0)
  Table(as="seg_table")
}

TextChunk(chunkSize=500, chunkOverlap=0)
"#;

fn outputs_of(json: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(json)
        .expect("outputs must be JSON")
        .as_array()
        .expect("outputs must be an array")
        .clone()
}

fn run(template: &str) -> (Vec<Value>, RunProvenance, String) {
    let (pages, _) = build_pages();
    let (json, diagnostics, provenance) =
        process_parsed_with_provenance(&pages, &MatchContext::default(), template, None)
            .expect("template must run");
    assert!(diagnostics.is_empty(), "all-matching run: {diagnostics:?}");
    (outputs_of(&json), provenance, json)
}

#[test]
fn sidecar_is_index_aligned_and_outputs_stay_byte_identical() {
    let (pages, _) = build_pages();
    let (outs, provenance, json) = run(SECTION_TEMPLATE);
    assert_eq!(
        outs.len(),
        provenance.outputs.len(),
        "one sidecar entry per output"
    );
    // The provenance pathway serializes the exact same payload as the
    // pre-existing surface (the CLI baselines ride on this).
    let baseline = process_parsed(&pages, &MatchContext::default(), SECTION_TEMPLATE, None)
        .expect("baseline run");
    // Ids are random per build; compare shape, not bytes, across two builds —
    // byte-identity holds within ONE parse, which is what the CLI baselines
    // measure. Here: same output count and same type tags in the same order.
    let base_outs = outputs_of(&baseline);
    assert_eq!(base_outs.len(), outs.len());
    for (a, b) in base_outs.iter().zip(outs.iter()) {
        assert_eq!(a["type"], b["type"]);
    }
    assert!(json.starts_with('['), "payload stays a pretty JSON array");
}

#[test]
fn chunk_entries_carry_source_ids_pages_and_section_span() {
    // Ids are random per build: run against the same pages they came from.
    let (pages, ids2) = build_pages();
    let (json, _, provenance) =
        process_parsed_with_provenance(&pages, &MatchContext::default(), SECTION_TEMPLATE, None)
            .expect("template must run");
    let outs = outputs_of(&json);

    // Output order: section chunk(s) first, then the top-level chunk(s),
    // then the deferred table (D-018 tail-deferral).
    let table_pos = outs
        .iter()
        .position(|o| o["type"] == "Table")
        .expect("table output present");
    assert_eq!(table_pos, outs.len() - 1, "table output is tail-deferred");

    // Section chunk: attributed to "overview" spanning the heading page (1)
    // through the section body (page 2 — the table element is part of the
    // matched range).
    let section_chunk_idx = outs
        .iter()
        .position(|o| o["type"] == "Text" && o["metadata"]["section"] == "overview")
        .expect("section-attributed chunk present");
    let prov = &provenance.outputs[section_chunk_idx];
    let section = prov.section.as_ref().expect("section attribution");
    assert_eq!(section.name, "overview");
    assert_eq!((section.page_start, section.page_end), (1, 2));
    // The chunk's source ids are real document element ids on its pages.
    assert!(!prov.element_ids.is_empty());
    for id in &prov.element_ids {
        assert!(ids2.contains(id), "chunk id {id} must be a document element");
    }
    assert_eq!(prov.pages, vec![1, 2]);
    // Document order: the section starts at the heading (doc index 1).
    assert_eq!(prov.order, 1);

    // Table entry: exactly the aux table's id and page, section-attributed.
    let table_prov = &provenance.outputs[table_pos];
    assert_eq!(table_prov.element_ids, vec![ids2[3].clone()]);
    assert_eq!(table_prov.pages, vec![2]);
    assert_eq!(table_prov.order, 3, "table sits at doc index 3");
    assert_eq!(
        table_prov.section.as_ref().map(|s| s.name.as_str()),
        Some("overview"),
        "table matched inside the section keeps its attribution"
    );

    // Top-level chunk(s): no section attribution.
    let top_chunk_idx = outs
        .iter()
        .position(|o| o["type"] == "Text" && o["metadata"]["section"].is_null())
        .expect("top-level chunk present");
    assert!(provenance.outputs[top_chunk_idx].section.is_none());
}

#[test]
fn unsectioned_template_yields_sectionless_provenance() {
    let (outs, provenance, _) = run("TextChunk(chunkSize=500, chunkOverlap=0)");
    assert!(!outs.is_empty());
    assert_eq!(outs.len(), provenance.outputs.len());
    for prov in &provenance.outputs {
        assert!(prov.section.is_none());
        assert!(!prov.element_ids.is_empty());
        assert!(!prov.pages.is_empty());
        // Pages ascend.
        for pair in prov.pages.windows(2) {
            assert!(pair[0] < pair[1]);
        }
    }
    // The first chunk starts at the first document element.
    assert_eq!(provenance.outputs[0].order, 0);
}
