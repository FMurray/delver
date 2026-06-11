//! Parse- and template-level tests for TABLE structure detection (Stage B
//! slice 3, docs/DECISIONS.md D-018): the three detection strategies (ruled,
//! row-ruled, aligned), the negative control, and the `Table(as="…")`
//! template selector with section scoping. No database required; the store
//! round-trip lives in delver-store/tests/roundtrip.rs.
//!
//! Per D-009 the fixture PDF is generated in-test via lopdf (builder
//! deliberately duplicated from the other suites — no shared test-util crate).

use delver_core::parse::{parse_document, AuxKind, ParsedDocument};
use delver_core::process_pdf;
use delver_core::table::{TableStrategy, TableStructure};
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use serde_json::Value;

const HEADING_P1: &str = "Segment Results Overview";
const HEADING_P2: &str = "Appendix Materials";
const HEADING_P3: &str = "Schedule of Fees";
const HEADING_P4: &str = "Notes and Commentary";

/// One positioned text run (`font` selects F1=Helvetica / F2=Helvetica-Bold).
fn push_text(ops: &mut Vec<Operation>, font: &str, text: &str, size: f32, x: f32, y: f32) {
    ops.push(Operation::new("BT", vec![]));
    ops.push(Operation::new("Tf", vec![font.into(), size.into()]));
    ops.push(Operation::new("Td", vec![x.into(), y.into()]));
    ops.push(Operation::new("Tj", vec![Object::string_literal(text)]));
    ops.push(Operation::new("ET", vec![]));
}

/// One stroked line from (x0,y0) to (x1,y1).
fn push_line(ops: &mut Vec<Operation>, x0: f32, y0: f32, x1: f32, y1: f32) {
    ops.push(Operation::new("m", vec![x0.into(), y0.into()]));
    ops.push(Operation::new("l", vec![x1.into(), y1.into()]));
    ops.push(Operation::new("S", vec![]));
}

/// Page 1: a fully ruled 3x3 table (4 horizontal + 4 vertical stroked rules,
/// one filled row-background rect, bold header row) below a 24pt heading.
/// Grid (PDF coords): x boundaries 100/220/340/460, y boundaries
/// 600/630/660/690.
fn page1_ops() -> Vec<Operation> {
    let mut ops = Vec::new();
    push_text(&mut ops, "F1", HEADING_P1, 24.0, 72.0, 750.0);

    for y in [600.0, 630.0, 660.0, 690.0] {
        push_line(&mut ops, 100.0, y, 460.0, y);
    }
    for x in [100.0, 220.0, 340.0, 460.0] {
        push_line(&mut ops, x, 600.0, x, 690.0);
    }
    // Shaded middle row (exercises the filled cell-box rule source).
    ops.push(Operation::new(
        "re",
        vec![100.into(), 630.into(), 360.into(), 30.into()],
    ));
    ops.push(Operation::new("f", vec![]));

    // Header row (bold) + two body rows.
    push_text(&mut ops, "F2", "Name", 10.0, 105.0, 668.0);
    push_text(&mut ops, "F2", "Q1", 10.0, 225.0, 668.0);
    push_text(&mut ops, "F2", "Q2", 10.0, 345.0, 668.0);
    push_text(&mut ops, "F1", "Alpha", 10.0, 105.0, 638.0);
    push_text(&mut ops, "F1", "10", 10.0, 225.0, 638.0);
    push_text(&mut ops, "F1", "20", 10.0, 345.0, 638.0);
    push_text(&mut ops, "F1", "Beta", 10.0, 105.0, 608.0);
    push_text(&mut ops, "F1", "30", 10.0, 225.0, 608.0);
    push_text(&mut ops, "F1", "40", 10.0, 345.0, 608.0);
    ops
}

/// Page 2: a borderless 4x2 table — four consecutive lines with two
/// left-aligned columns (labels at x=72, values at x=300), no rules at all.
fn page2_ops() -> Vec<Operation> {
    let mut ops = Vec::new();
    push_text(&mut ops, "F1", HEADING_P2, 24.0, 72.0, 750.0);
    for (i, (label, value)) in [
        ("Revenue", "1,000"),
        ("Cost", "2,000"),
        ("Margin", "3,000"),
        ("Total", "4,000"),
    ]
    .iter()
    .enumerate()
    {
        let y = 500.0 - 15.0 * i as f32;
        push_text(&mut ops, "F1", label, 10.0, 72.0, y);
        push_text(&mut ops, "F1", value, 10.0, 300.0, y);
    }
    ops
}

/// Page 3: a row-ruled table — four evenly stacked horizontal rules sharing
/// one x-range (no verticals), a header line above the first rule, and three
/// data rows of three columns each.
fn page3_ops() -> Vec<Operation> {
    let mut ops = Vec::new();
    push_text(&mut ops, "F1", HEADING_P3, 24.0, 72.0, 750.0);

    for y in [398.0, 380.0, 362.0, 344.0] {
        push_line(&mut ops, 72.0, y, 400.0, y);
    }
    let rows = [
        ("Item", "Amount", "Pct", 402.0),
        ("Setup", "125", "10%", 384.0),
        ("Service", "250", "20%", 366.0),
        ("Support", "375", "30%", 348.0),
    ];
    for (a, b, c, y) in rows {
        push_text(&mut ops, "F1", a, 10.0, 72.0, y);
        push_text(&mut ops, "F1", b, 10.0, 200.0, y);
        push_text(&mut ops, "F1", c, 10.0, 320.0, y);
    }
    ops
}

/// Page 4 (negative control): a paragraph of full-width single-run lines plus
/// scattered two-piece lines with no shared alignment — none of it may become
/// a table.
fn page4_ops() -> Vec<Operation> {
    let mut ops = Vec::new();
    push_text(&mut ops, "F1", HEADING_P4, 24.0, 72.0, 750.0);
    push_text(
        &mut ops,
        "F1",
        "The company continued to invest in research programs across regions.",
        10.0,
        72.0,
        700.0,
    );
    push_text(
        &mut ops,
        "F1",
        "Cash generation remained strong throughout the period under review.",
        10.0,
        72.0,
        688.0,
    );
    push_text(
        &mut ops,
        "F1",
        "Management expects continued progress against stated priorities.",
        10.0,
        72.0,
        676.0,
    );
    // Scattered fragments: two pieces per line, deliberately unaligned.
    push_text(&mut ops, "F1", "alpha", 10.0, 80.0, 600.0);
    push_text(&mut ops, "F1", "beta", 10.0, 300.0, 600.0);
    push_text(&mut ops, "F1", "gamma", 10.0, 150.0, 585.0);
    push_text(&mut ops, "F1", "delta", 10.0, 420.0, 585.0);
    push_text(&mut ops, "F1", "epsilon", 10.0, 95.0, 570.0);
    push_text(&mut ops, "F1", "zeta", 10.0, 510.0, 570.0);
    ops
}

fn build_table_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let regular = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let bold = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica-Bold",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => regular, "F2" => bold },
    });

    let mut page_ids = Vec::new();
    for ops in [page1_ops(), page2_ops(), page3_ops(), page4_ops()] {
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            Content { operations: ops }
                .encode()
                .expect("encode content"),
        ));
        page_ids.push(doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }));
    }

    let kids: Vec<Object> = page_ids.iter().map(|id| (*id).into()).collect();
    let n_pages = page_ids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => n_pages,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize pdf to memory");
    bytes
}

fn parse_fixture() -> ParsedDocument {
    let bytes = build_table_pdf();
    let doc = Document::load_mem(&bytes).expect("load fixture pdf");
    parse_document(&doc).expect("parse fixture pdf")
}

fn tables_on_page(parsed: &ParsedDocument, page: u32) -> Vec<TableStructure> {
    parsed
        .pages
        .get(&page)
        .into_iter()
        .flat_map(|p| p.aux_store.iter())
        .filter(|aux| aux.kind == AuxKind::Table)
        .map(|aux| aux.table.clone().expect("table element carries structure"))
        .collect()
}

// ───────────────────────────── parse level ─────────────────────────────

#[test]
fn detects_fully_ruled_table() {
    let parsed = parse_fixture();
    let tables = tables_on_page(&parsed, 1);
    assert_eq!(tables.len(), 1, "expected exactly one table on page 1");
    let t = &tables[0];
    assert_eq!(t.strategy, TableStrategy::Ruled);
    assert_eq!((t.n_rows, t.n_cols), (3, 3), "cells: {:?}", t.cells);
    assert_eq!(t.header_texts(), vec!["Name", "Q1", "Q2"]);
    assert_eq!(
        t.body_rows(),
        vec![vec!["Alpha", "10", "20"], vec!["Beta", "30", "40"]]
    );
    // Bold header differs from the body font → flagged.
    assert!(t.cells.iter().filter(|c| c.row == 0).all(|c| c.is_header));
    assert!(t.cells.iter().filter(|c| c.row > 0).all(|c| !c.is_header));
    // Fully occupied lattice → maximum ruled confidence.
    assert!(
        (t.confidence - 1.0).abs() < 1e-9,
        "confidence: {}",
        t.confidence
    );
    // Lattice bbox: x 100..460, y flipped 792-690=102 .. 792-600=192.
    assert!((t.bbox.x0 - 100.0).abs() < 2.5, "bbox: {:?}", t.bbox);
    assert!((t.bbox.y0 - 102.0).abs() < 2.5, "bbox: {:?}", t.bbox);
    assert!((t.bbox.x1 - 460.0).abs() < 2.5, "bbox: {:?}", t.bbox);
    assert!((t.bbox.y1 - 192.0).abs() < 2.5, "bbox: {:?}", t.bbox);
    // Spans default to 1 and the grid is complete.
    assert_eq!(t.cells.len(), 9);
    assert!(t
        .cells
        .iter()
        .all(|c| c.row_span == 1 && c.col_span == 1));
}

#[test]
fn detects_borderless_aligned_table() {
    let parsed = parse_fixture();
    let tables = tables_on_page(&parsed, 2);
    assert_eq!(tables.len(), 1, "expected exactly one table on page 2");
    let t = &tables[0];
    assert_eq!(t.strategy, TableStrategy::Aligned);
    assert_eq!((t.n_rows, t.n_cols), (4, 2), "cells: {:?}", t.cells);
    // Uniform font → no header detected; all four rows are body rows.
    assert!(t.header_texts().is_empty());
    assert_eq!(
        t.body_rows(),
        vec![
            vec!["Revenue", "1,000"],
            vec!["Cost", "2,000"],
            vec!["Margin", "3,000"],
            vec!["Total", "4,000"],
        ]
    );
    assert!(t.confidence > 0.0 && t.confidence <= 0.9);
}

#[test]
fn detects_row_ruled_table() {
    let parsed = parse_fixture();
    let tables = tables_on_page(&parsed, 3);
    assert_eq!(tables.len(), 1, "expected exactly one table on page 3");
    let t = &tables[0];
    assert_eq!(t.strategy, TableStrategy::RowRuled);
    assert_eq!((t.n_rows, t.n_cols), (4, 3), "cells: {:?}", t.cells);
    // The line above the first rule is the rule-separated header.
    assert_eq!(t.header_texts(), vec!["Item", "Amount", "Pct"]);
    assert_eq!(
        t.body_rows(),
        vec![
            vec!["Setup", "125", "10%"],
            vec!["Service", "250", "20%"],
            vec!["Support", "375", "30%"],
        ]
    );
}

#[test]
fn paragraph_and_scattered_text_are_not_tables() {
    let parsed = parse_fixture();
    assert!(
        tables_on_page(&parsed, 4).is_empty(),
        "negative control page produced a table"
    );
}

#[test]
fn detection_is_deterministic() {
    let a = parse_fixture();
    let b = parse_fixture();
    for page in 1..=4 {
        assert_eq!(
            tables_on_page(&a, page),
            tables_on_page(&b, page),
            "page {page} structures differ between parses"
        );
    }
}

// ─────────────────────────── template level ───────────────────────────

fn run_template(template: &str) -> Vec<Value> {
    let bytes = build_table_pdf();
    let (json, _blocks, _doc) =
        process_pdf(&bytes, template, None, None).expect("process_pdf with Table selector");
    serde_json::from_str::<Vec<Value>>(&json).expect("outputs JSON array")
}

fn table_outputs(outputs: &[Value]) -> Vec<&Value> {
    outputs.iter().filter(|o| o["type"] == "Table").collect()
}

#[test]
fn table_selector_top_level_collects_all_tables() {
    let outputs = run_template(r#"Table(as="all-tables")"#);
    let tables = table_outputs(&outputs);
    assert_eq!(tables.len(), 3, "outputs: {outputs:?}");
    let strategies: Vec<&str> = tables
        .iter()
        .map(|t| t["strategy"].as_str().unwrap())
        .collect();
    assert_eq!(strategies, vec!["ruled", "aligned", "row-ruled"]);
    assert!(tables.iter().all(|t| t["name"] == "all-tables"));
}

#[test]
fn table_selector_inside_section() {
    let outputs = run_template(&format!(
        r#"Section(
  threshold=0.8,
  match="{HEADING_P1}",
  end_match="{HEADING_P2}",
  as="S1"
) {{
  TextChunk(chunkSize=200, chunkOverlap=20)
  Table(as="seg-table")
}}
"#
    ));
    let tables = table_outputs(&outputs);
    assert_eq!(tables.len(), 1, "outputs: {outputs:?}");
    let t = tables[0];
    assert_eq!(t["strategy"], "ruled");
    assert_eq!(t["page"], 1);
    assert_eq!(t["n_rows"], 3);
    assert_eq!(t["n_cols"], 3);
    assert_eq!(t["name"], "seg-table");
    assert_eq!(t["metadata"]["section"], "S1");
    assert_eq!(
        t["header"],
        serde_json::json!(["Name", "Q1", "Q2"]),
        "table: {t:?}"
    );
    assert_eq!(t["rows"][0], serde_json::json!(["Alpha", "10", "20"]));
    assert_eq!(t["cells"].as_array().map(Vec::len), Some(9));
    // The section still produced its text chunks alongside.
    assert!(outputs.iter().any(|o| o["type"] == "Text"));
}

#[test]
fn table_selector_respects_section_boundaries() {
    // Section covers page 2 only: exactly the aligned table, not the ruled
    // or row-ruled ones.
    let outputs = run_template(&format!(
        r#"Section(
  threshold=0.8,
  match="{HEADING_P2}",
  end_match="{HEADING_P3}",
  as="S2"
) {{
  Table(as="appendix-table")
}}
"#
    ));
    let tables = table_outputs(&outputs);
    assert_eq!(tables.len(), 1, "outputs: {outputs:?}");
    assert_eq!(tables[0]["strategy"], "aligned");
    assert_eq!(tables[0]["page"], 2);

    // Section over the negative-control page yields no Table outputs.
    let outputs = run_template(&format!(
        r#"Section(
  threshold=0.8,
  match="{HEADING_P4}",
  as="S4"
) {{
  Table(as="none")
}}
"#
    ));
    assert!(
        table_outputs(&outputs).is_empty(),
        "negative-control section produced tables: {outputs:?}"
    );
}

#[test]
fn table_model_attribute_warns_and_proceeds() {
    // The 10k.tmpl shape: model=/targetSchema= are unimplemented enrichment
    // attributes — a documented D-006 exception. Extraction must proceed
    // (no error) and still emit the structural TableOutput.
    let outputs = run_template(
        r#"Table(
  as="enriched",
  model="databricksmodel",
  targetSchema="{...}"
)"#,
    );
    let tables = table_outputs(&outputs);
    assert_eq!(tables.len(), 3, "outputs: {outputs:?}");
    assert!(tables.iter().all(|t| t["name"] == "enriched"));
}
