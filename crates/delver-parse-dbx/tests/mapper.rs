//! Mapper tests against the canned ai_parse_document response fixture
//! (hand-written from the published 2.0 output schema — contains no real
//! document data; see docs/DECISIONS-aiparse.md). No network anywhere.

use delver_core::parse::{AuxKind, PageContent};
use delver_core::table::TableStrategy;
use delver_parse_dbx::map_ai_parse_response;
use serde_json::{json, Value};

const FIXTURE: &str = include_str!("fixtures/ai_parse_response.json");

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).expect("fixture is valid JSON")
}

#[test]
fn maps_fixture_to_parsed_document() {
    let parsed = map_ai_parse_response(&fixture()).expect("fixture maps");

    // 3 pages, all present even where elements are sparse (0-based -> 1-based).
    assert_eq!(parsed.page_count(), 3);
    assert!(parsed.pages.contains_key(&1) && parsed.pages.contains_key(&3));

    // Page 1: title + section_header + text + page_footer, all text rows.
    let page1 = &parsed.pages[&1];
    let texts: Vec<_> = page1.text_store.iter().collect();
    assert_eq!(texts.len(), 4);
    assert_eq!(texts[0].text, "Annual Report on Form 10-K");
    assert_eq!(texts[0].page_number, 1);
    assert_eq!(texts[0].bbox, (120.0, 80.0, 1100.0, 150.0));
    assert!(texts[1].text.starts_with("Item 7."));
    // ai_parse output has no font information.
    assert_eq!(texts[0].font_size, 0.0);
    assert_eq!(texts[0].font_name, None);

    // Page 2: one table + one figure.
    let page2 = &parsed.pages[&2];
    let aux: Vec<_> = page2.aux_store.iter().collect();
    assert_eq!(aux.len(), 2);

    let table = aux.iter().find(|a| a.kind == AuxKind::Table).expect("table");
    let structure = table.table.as_ref().expect("table structure");
    assert_eq!(structure.strategy, TableStrategy::AiParse);
    assert_eq!((structure.n_rows, structure.n_cols), (4, 3));
    assert!((structure.confidence - 0.89).abs() < 1e-9);
    assert_eq!(structure.header_texts(), vec!["Metric", "2015", "2014"]);
    let body = structure.body_rows();
    assert_eq!(body[0], vec!["Sales", "10,328", "10,990"]);
    // The colspan=3 note row keeps its anchor cell and span.
    let note = structure
        .cells
        .iter()
        .find(|c| c.text.starts_with("Amounts in millions"))
        .expect("note cell");
    assert_eq!((note.row, note.col, note.col_span), (3, 0, 3));
    assert_eq!(table.metadata["source"], "ai_parse_document");
    assert_eq!(table.metadata["strategy"], "ai-parse");

    let figure = aux.iter().find(|a| a.kind == AuxKind::Figure).expect("figure");
    assert_eq!(
        figure.text.as_deref(),
        Some("Bar chart comparing segment revenue for 2014 and 2015."),
        "figure description must be searchable element text"
    );
    assert_eq!(figure.metadata["confidence"], json!(0.84));

    // Page 3: the footnote text row.
    let page3 = &parsed.pages[&3];
    assert_eq!(page3.text_store.iter().count(), 1);

    // Parser provenance (DA-008).
    assert_eq!(parsed.metadata["parser"], "ai_parse_document");
    assert_eq!(parsed.metadata["parser_version"], "2.0");
    assert_eq!(parsed.metadata["parser_run_id"], "01ef-fixture-run-0001");
    assert_eq!(parsed.metadata["bbox_space"], "pixels");
    assert!(parsed.refs.is_empty());
}

#[test]
fn document_order_is_preserved_within_pages() {
    let parsed = map_ai_parse_response(&fixture()).expect("fixture maps");
    let page1 = &parsed.pages[&1];
    let ordered: Vec<String> = page1
        .iter_ordered()
        .filter_map(|c| match c {
            PageContent::Text(t) => Some(t.text),
            _ => None,
        })
        .collect();
    assert!(ordered[0].starts_with("Annual Report"));
    assert!(ordered[3].starts_with("Page 1 of 3"));
}

#[test]
fn error_status_entries_fail_loud() {
    let mut bad = fixture();
    bad["error_status"] = json!([
        {"error_message": "Could not rasterize page", "page_id": 1}
    ]);
    let err = map_ai_parse_response(&bad).expect_err("page errors must fail");
    assert!(
        err.0.contains("Could not rasterize page") && err.0.contains("page 1"),
        "error must carry the page detail: {err}"
    );
}

#[test]
fn unknown_schema_version_fails_loud() {
    let mut bad = fixture();
    bad["metadata"]["version"] = json!("3.0");
    let err = map_ai_parse_response(&bad).expect_err("major bump must fail");
    assert!(err.0.contains("3.0") && err.0.contains("2.x"), "got: {err}");
}

#[test]
fn unknown_element_type_fails_loud() {
    let mut bad = fixture();
    bad["document"]["elements"][0]["type"] = json!("hologram");
    let err = map_ai_parse_response(&bad).expect_err("unknown type must fail");
    assert!(err.0.contains("hologram"), "got: {err}");
}

#[test]
fn missing_bbox_fails_loud() {
    let mut bad = fixture();
    bad["document"]["elements"][0]["bbox"] = json!([]);
    let err = map_ai_parse_response(&bad).expect_err("missing bbox must fail");
    assert!(err.0.contains("bbox"), "got: {err}");
}

#[test]
fn unparseable_table_html_fails_loud() {
    let mut bad = fixture();
    bad["document"]["elements"][4]["content"] = json!("not a table at all");
    let err = map_ai_parse_response(&bad).expect_err("bad table HTML must fail");
    assert!(err.0.contains("table element 4"), "got: {err}");
}
