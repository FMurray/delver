//! Stage C tests (docs/DECISIONS.md D-021/D-022): user-defined TABLE types
//! (`TYPE ... AS TABLE`), typed coercion via `Table(type="...")`, positional
//! column fallback, SubCorpus interpolation into TextChunk `template=`, and
//! the fail-loud template-compile errors. No database required.
//!
//! Per D-009 fixture PDFs are generated in-test via lopdf (builder helpers
//! deliberately duplicated from table_spec.rs — no shared test-util crate).

use delver_core::docql::parse_template;
use delver_core::process_pdf;
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use serde_json::{json, Value};

// ───────────────────────────── PDF builders ─────────────────────────────

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

fn build_pdf(pages: Vec<Vec<Operation>>) -> Vec<u8> {
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
    for ops in pages {
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

/// Fully ruled 4x3 table with a bold header row Account|Year|Amount and three
/// data rows exercising the coercion conventions: `$1,234`, `(56)` negative,
/// `7.8 %` percent, and one uncoercible INT cell ("abc"). Grid (PDF coords):
/// x boundaries 100/220/340/460, y boundaries 570/600/630/660/690.
fn typed_table_pdf() -> Vec<u8> {
    let mut ops = Vec::new();
    push_text(&mut ops, "F1", "Account Ledger", 24.0, 72.0, 750.0);

    for y in [570.0, 600.0, 630.0, 660.0, 690.0] {
        push_line(&mut ops, 100.0, y, 460.0, y);
    }
    for x in [100.0, 220.0, 340.0, 460.0] {
        push_line(&mut ops, x, 570.0, x, 690.0);
    }

    push_text(&mut ops, "F2", "Account", 10.0, 105.0, 668.0);
    push_text(&mut ops, "F2", "Year", 10.0, 225.0, 668.0);
    push_text(&mut ops, "F2", "Amount", 10.0, 345.0, 668.0);
    push_text(&mut ops, "F1", "Cash", 10.0, 105.0, 638.0);
    push_text(&mut ops, "F1", "2015", 10.0, 225.0, 638.0);
    push_text(&mut ops, "F1", "$1,234", 10.0, 345.0, 638.0);
    push_text(&mut ops, "F1", "Debt", 10.0, 105.0, 608.0);
    push_text(&mut ops, "F1", "2014", 10.0, 225.0, 608.0);
    push_text(&mut ops, "F1", "(56)", 10.0, 345.0, 608.0);
    push_text(&mut ops, "F1", "Growth", 10.0, 105.0, 578.0);
    push_text(&mut ops, "F1", "abc", 10.0, 225.0, 578.0);
    push_text(&mut ops, "F1", "7.8 %", 10.0, 345.0, 578.0);

    build_pdf(vec![ops])
}

/// Borderless headerless table (aligned strategy, uniform font => no header
/// row): label column, an all-`$` filler column, and a value column. Tests
/// the positional fallback and the filler-column skip.
fn headerless_table_pdf() -> Vec<u8> {
    let mut ops = Vec::new();
    push_text(&mut ops, "F1", "Fee Schedule", 24.0, 72.0, 750.0);
    for (i, (label, value)) in [
        ("Revenue", "1,000"),
        ("Cost", "2,000"),
        ("Margin", "(3,500)"),
        ("Total", "4.25"),
    ]
    .iter()
    .enumerate()
    {
        let y = 500.0 - 15.0 * i as f32;
        push_text(&mut ops, "F1", label, 10.0, 72.0, y);
        push_text(&mut ops, "F1", "$", 10.0, 250.0, y);
        push_text(&mut ops, "F1", value, 10.0, 330.0, y);
    }
    build_pdf(vec![ops])
}

/// Plain text page for SubCorpus interpolation tests.
fn text_pdf() -> Vec<u8> {
    let mut ops = Vec::new();
    push_text(
        &mut ops,
        "F1",
        "Delinquency rates trended lower across the portfolio this quarter.",
        11.0,
        72.0,
        700.0,
    );
    push_text(
        &mut ops,
        "F1",
        "Prepayment speeds were stable relative to the prior year.",
        11.0,
        72.0,
        680.0,
    );
    build_pdf(vec![ops])
}

fn run_template(pdf: &[u8], template: &str) -> Vec<Value> {
    let (json, _blocks, _doc) = process_pdf(pdf, template, None, None).expect("process_pdf");
    serde_json::from_str::<Vec<Value>>(&json).expect("outputs JSON array")
}

fn typed_outputs(outputs: &[Value]) -> Vec<&Value> {
    outputs.iter().filter(|o| o["type"] == "TypedTable").collect()
}

// ───────────────────────────── typed extraction ─────────────────────────────

#[test]
fn typed_table_coercion_happy_path() {
    let pdf = typed_table_pdf();
    let outputs = run_template(
        &pdf,
        r#"TYPE Ledger AS TABLE ( account TEXT, year INT, amount DECIMAL );
Table(as="ledger", type="Ledger")"#,
    );
    let typed = typed_outputs(&outputs);
    assert_eq!(typed.len(), 1, "outputs: {outputs:?}");
    let t = typed[0];

    assert_eq!(t["type"], "TypedTable");
    assert_eq!(t["type_name"], "Ledger");
    assert_eq!(t["name"], "ledger");
    // Header columns Account/Year/Amount map by (normalized) header match;
    // each cell coerces per the conventions; "abc" is the designed DATA
    // failure: null + errors entry, run continues (D-021).
    assert_eq!(
        t["records"],
        json!([
            {"account": "Cash",   "year": 2015, "amount": 1234.0},
            {"account": "Debt",   "year": 2014, "amount": -56.0},
            {"account": "Growth", "year": null, "amount": 7.8},
        ])
    );
    assert_eq!(t["coerced_ok"], 8);
    assert_eq!(t["coerced_err"], 1);
    let errors = t["errors"].as_array().expect("errors array");
    assert_eq!(errors.len(), 1, "errors: {errors:?}");
    assert_eq!(errors[0]["row"], 3);
    assert_eq!(errors[0]["col"], 1);
    assert_eq!(errors[0]["raw"], "abc");
    assert!(
        errors[0]["reason"].as_str().unwrap().contains("INT"),
        "reason: {}",
        errors[0]["reason"]
    );
    // Trailing % is stripped and recorded per cell.
    assert_eq!(
        t["metadata"]["percent_cells"],
        json!([{"row": 3, "field": "amount"}])
    );
    // Provenance: table element id + grid rows of each record (header is
    // grid row 0).
    assert_eq!(t["provenance"]["page"], 1);
    assert_eq!(t["provenance"]["source_rows"], json!([1, 2, 3]));
    uuid::Uuid::parse_str(t["provenance"]["element_id"].as_str().unwrap())
        .expect("element_id is a uuid");
}

#[test]
fn untyped_table_still_emits_plain_table_output() {
    let pdf = typed_table_pdf();
    let outputs = run_template(&pdf, r#"Table(as="plain")"#);
    assert!(typed_outputs(&outputs).is_empty());
    let tables: Vec<&Value> = outputs.iter().filter(|o| o["type"] == "Table").collect();
    assert_eq!(tables.len(), 1, "outputs: {outputs:?}");
    assert_eq!(tables[0]["header"], json!(["Account", "Year", "Amount"]));
    assert_eq!(tables[0]["rows"][0], json!(["Cash", "2015", "$1,234"]));
}

#[test]
fn positional_fallback_headerless_table_skips_filler_column() {
    let pdf = headerless_table_pdf();
    let outputs = run_template(
        &pdf,
        r#"TYPE Fees AS TABLE ( label TEXT, amount DECIMAL );
Table(as="fees", type="Fees")"#,
    );
    let typed = typed_outputs(&outputs);
    assert_eq!(typed.len(), 1, "outputs: {outputs:?}");
    let t = typed[0];
    // No header row: fields map positionally left-to-right; the middle
    // column is all "$" => filler, skipped.
    assert_eq!(
        t["records"],
        json!([
            {"label": "Revenue", "amount": 1000.0},
            {"label": "Cost",    "amount": 2000.0},
            {"label": "Margin",  "amount": -3500.0},
            {"label": "Total",   "amount": 4.25},
        ])
    );
    assert_eq!(t["coerced_ok"], 8);
    assert_eq!(t["coerced_err"], 0);
    assert_eq!(t["errors"], json!([]));
    assert_eq!(t["provenance"]["source_rows"], json!([0, 1, 2, 3]));
}

// ───────────────────────────── SubCorpus interpolation ─────────────────────────────

#[test]
fn subcorpus_interpolation_prefixes_chunk_text() {
    let pdf = text_pdf();
    let outputs = run_template(
        &pdf,
        r#"SubCorpus(description="California auto loan portfolio", as="CA_auto_loans")
TextChunk(chunkSize=2000, chunkOverlap=0, template="{CA_auto_loans} {text}")"#,
    );
    let chunks: Vec<&Value> = outputs.iter().filter(|o| o["type"] == "Text").collect();
    assert!(!chunks.is_empty(), "outputs: {outputs:?}");
    for chunk in &chunks {
        let text = chunk["text"].as_str().unwrap();
        assert!(
            text.starts_with("California auto loan portfolio "),
            "chunk not prefixed: {text}"
        );
    }
    assert!(
        chunks[0]["text"]
            .as_str()
            .unwrap()
            .contains("Delinquency rates trended lower"),
        "{text}",
        text = chunks[0]["text"]
    );
}

#[test]
fn textchunk_without_template_attr_is_unchanged_by_subcorpus() {
    let pdf = text_pdf();
    let with_decl = run_template(
        &pdf,
        r#"SubCorpus(description="ignored", as="unused")
TextChunk(chunkSize=2000, chunkOverlap=0)"#,
    );
    let without_decl = run_template(&pdf, "TextChunk(chunkSize=2000, chunkOverlap=0)");
    assert_eq!(with_decl, without_decl);
}

// ───────────────────────────── fail-loud compile errors ─────────────────────────────

fn compile_err(template: &str) -> String {
    parse_template(template)
        .expect_err("template must fail to compile")
        .to_string()
}

#[test]
fn duplicate_type_is_a_compile_error() {
    let err = compile_err(
        "TYPE T AS TABLE ( a TEXT );\nTYPE T AS TABLE ( b INT );\nTable(type=\"T\")",
    );
    assert!(err.contains("duplicate TYPE definition 'T'"), "{err}");
}

#[test]
fn unsupported_field_type_is_a_compile_error() {
    let err = compile_err("TYPE T AS TABLE ( a FLOAT );\nTable(type=\"T\")");
    assert!(
        err.contains("unsupported type 'FLOAT'") && err.contains("TEXT, INT, DECIMAL"),
        "{err}"
    );
}

#[test]
fn undefined_type_reference_is_a_compile_error() {
    let err = compile_err("TYPE Known AS TABLE ( a TEXT );\nTable(as=\"x\", type=\"Missing\")");
    assert!(
        err.contains("undefined TYPE") && err.contains("Known"),
        "{err}"
    );
}

#[test]
fn type_on_textchunk_is_a_compile_error() {
    let err = compile_err("TYPE T AS TABLE ( a TEXT );\nTextChunk(type=\"T\")");
    assert!(err.contains("only supported on Table"), "{err}");
}

#[test]
fn unknown_interpolation_var_is_a_compile_error() {
    let err = compile_err(
        r#"SubCorpus(description="d", as="CA_auto_loans")
TextChunk(template="{nope} {text}")"#,
    );
    assert!(
        err.contains("unknown variable '{nope}'")
            && err.contains("CA_auto_loans")
            && err.contains("text"),
        "{err}"
    );
}

#[test]
fn duplicate_subcorpus_is_a_compile_error() {
    let err = compile_err(
        r#"SubCorpus(description="a", as="dup")
SubCorpus(description="b", as="dup")"#,
    );
    assert!(err.contains("duplicate SubCorpus definition 'dup'"), "{err}");
}

#[test]
fn subcorpus_requires_description_and_name() {
    let err = compile_err(r#"SubCorpus(as="x")"#);
    assert!(err.contains("description"), "{err}");
    let err = compile_err(r#"SubCorpus(description="x")"#);
    assert!(err.contains("as="), "{err}");
}

#[test]
fn template_attr_on_section_is_a_compile_error() {
    let err = compile_err(r#"Section(match="x", template="{text}")"#);
    assert!(err.contains("only supported on TextChunk"), "{err}");
}
