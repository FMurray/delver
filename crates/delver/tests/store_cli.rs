//! End-to-end tests for the store-backed CLI subcommands (Stage A slice 2,
//! docs/DECISIONS.md D-012): index -> query --doc -> search, through the
//! actual `delver` binary.
//!
//! Per D-009 the test PDF is generated in-test via lopdf (no binary fixtures)
//! and DB-backed tests skip with an explicit message when Postgres is not
//! reachable (default dev DB: postgres://delver:delver@localhost:5433/delver).
//!
//! Equivalence between `query --pdf` (fresh parse) and `query --doc`
//! (hydrated from Postgres) is asserted on STABLE fields only: separate
//! parses of the same bytes assign different element UUIDs (D-011), so
//! per-run ids are stripped before comparison. Chunk outputs carry no ids;
//! image outputs do (none in this synthetic document).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use serde_json::Value;
use uuid::Uuid;

use delver_store::blocking::DelverStoreBlocking;

const HEADING_1: &str = "Management Discussion and Analysis";
const BODY_1A: &str = "Revenue grew steadily across all reporting segments during the fiscal year.";
const BODY_1B: &str = "Operating expenses stayed flat thanks to disciplined cost control programs.";
const HEADING_2: &str = "Quantitative and Qualitative Disclosures";
const BODY_2A: &str = "Interest rate exposure remains hedged through a portfolio of fixed rate swaps.";

/// DocQL template over the synthetic document: one named section bounded by
/// the two headings, chunked small enough to produce multiple chunks.
const TEMPLATE: &str = r#"Section(
  threshold=0.8,
  match="Management Discussion and Analysis",
  end_match="Quantitative and Qualitative Disclosures",
  as="MD&A"
) {
  TextChunk(
    chunkSize=120,
    chunkOverlap=20,
  )
}
"#;

fn db_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://delver:delver@localhost:5433/delver".to_string())
}

fn db_available(url: &str, test_name: &str) -> bool {
    match DelverStoreBlocking::connect(url) {
        Ok(_) => true,
        Err(e) => {
            eprintln!(
                "SKIP {test_name}: Postgres unreachable at {url} ({e}); \
                 set DATABASE_URL or run scripts/dev-db.sh"
            );
            false
        }
    }
}

fn push_text_ops(ops: &mut Vec<Operation>, text: &str, size: f32, x: f32, y: f32) {
    ops.push(Operation::new("BT", vec![]));
    ops.push(Operation::new("Tf", vec!["F1".into(), size.into()]));
    ops.push(Operation::new("Td", vec![x.into(), y.into()]));
    ops.push(Operation::new("Tj", vec![Object::string_literal(text)]));
    ops.push(Operation::new("ET", vec![]));
}

/// Build a small 2-page PDF entirely in memory (same shape as
/// delver-store/tests/roundtrip.rs): page 1 has a 24pt heading and two 11pt
/// body paragraphs, page 2 has a 24pt heading and one body paragraph.
fn build_test_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut p1_ops = Vec::new();
    push_text_ops(&mut p1_ops, HEADING_1, 24.0, 72.0, 700.0);
    push_text_ops(&mut p1_ops, BODY_1A, 11.0, 72.0, 660.0);
    push_text_ops(&mut p1_ops, BODY_1B, 11.0, 72.0, 640.0);

    let mut p2_ops = Vec::new();
    push_text_ops(&mut p2_ops, HEADING_2, 24.0, 72.0, 700.0);
    push_text_ops(&mut p2_ops, BODY_2A, 11.0, 72.0, 660.0);

    let mut page_ids = Vec::new();
    for ops in [p1_ops, p2_ops] {
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            Content { operations: ops }.encode().expect("encode content"),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        page_ids.push(page_id);
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

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("delver-store-cli-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Run the delver binary (path provided by cargo for integration tests) and
/// parse its stdout as a single JSON document — which also enforces the
/// machine-readable stdout contract of D-012.
fn run_json(args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_delver"))
        .args(args)
        .output()
        .expect("spawn delver binary");
    assert!(
        output.status.success(),
        "`delver {}` failed with {}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout of `delver {}` is not a single JSON document ({e}):\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// Strip per-run fields (element UUIDs on image outputs) so fresh and
/// hydrated query outputs can be compared on stable content only.
fn stable_view(outputs: &Value) -> Value {
    let mut stable = outputs.clone();
    if let Some(items) = stable.as_array_mut() {
        for item in items {
            if let Some(map) = item.as_object_mut() {
                map.remove("id");
            }
        }
    }
    stable
}

#[test]
fn cli_index_query_search_flow() {
    let url = db_url();
    if !db_available(&url, "cli_index_query_search_flow") {
        return;
    }

    let dir = scratch_dir();
    let pdf_path = dir.join("synthetic.pdf");
    fs::write(&pdf_path, build_test_pdf()).expect("write synthetic pdf");
    let template_path = dir.join("synthetic.tmpl");
    fs::write(&template_path, TEMPLATE).expect("write template");
    let pdf = pdf_path.to_str().expect("utf8 pdf path");
    let template = template_path.to_str().expect("utf8 template path");

    let corpus = format!("cli-slice2-{}", Uuid::new_v4());

    // --- index: parse + persist, JSON receipt on stdout ---
    let indexed = run_json(&[
        "index", pdf,
        "--corpus", &corpus,
        "--uri", "mem://synthetic.pdf",
        "--db", &url,
    ]);
    assert_eq!(indexed["created"], Value::Bool(true), "first ingest must create");
    assert_eq!(indexed["corpus"].as_str(), Some(corpus.as_str()));
    let element_count = indexed["element_count"].as_i64().expect("element_count");
    assert!(
        element_count >= 5,
        "expected at least the 5 synthetic text elements, got {element_count}"
    );
    let doc_id = indexed["document_id"]
        .as_str()
        .expect("document_id string")
        .to_string();
    Uuid::parse_str(&doc_id).expect("document_id must be a uuid");

    // --- re-index is idempotent (D-008) ---
    let again = run_json(&["index", pdf, "--corpus", &corpus, "--db", &url]);
    assert_eq!(again["created"], Value::Bool(false), "re-ingest must not create");
    assert_eq!(again["document_id"].as_str(), Some(doc_id.as_str()));
    assert_eq!(again["element_count"].as_i64(), Some(element_count));

    // --- query: hydrated (--doc) vs fresh (--pdf), same template ---
    let hydrated = run_json(&[
        "query",
        "--template", template,
        "--doc", &doc_id,
        "--db", &url,
        "--tokenizer-model", "none",
    ]);
    let fresh = run_json(&[
        "query",
        "--template", template,
        "--pdf", pdf,
        "--tokenizer-model", "none",
    ]);

    let fresh_outputs = fresh.as_array().expect("fresh outputs array");
    assert!(!fresh_outputs.is_empty(), "fresh query produced no outputs");
    assert_eq!(
        fresh_outputs.len(),
        hydrated.as_array().expect("hydrated outputs array").len(),
        "chunk count differs between fresh and hydrated query"
    );
    assert_eq!(
        stable_view(&fresh),
        stable_view(&hydrated),
        "hydrated query must equal fresh query on stable fields"
    );

    // Chunks must cover the section body and carry stable page metadata.
    let all_text: String = fresh_outputs
        .iter()
        .filter_map(|o| o["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        all_text.contains("Revenue grew steadily"),
        "chunks missing section body text: {all_text}"
    );
    assert!(
        fresh_outputs
            .iter()
            .all(|o| o["metadata"]["page_numbers"] == serde_json::json!([1])),
        "section chunks must come from page 1: {fresh}"
    );

    // --- search: corpus scope ---
    let hits = run_json(&[
        "search", "disciplined cost control",
        "--corpus", &corpus,
        "--limit", "5",
        "--db", &url,
    ]);
    let hits = hits.as_array().expect("search hits array");
    assert!(!hits.is_empty(), "corpus search returned no hits");
    for key in ["element_id", "document_id", "page", "rank", "snippet"] {
        assert!(hits[0].get(key).is_some(), "hit missing {key}: {}", hits[0]);
    }
    assert!(
        hits.iter().any(|h| h["snippet"]
            .as_str()
            .unwrap_or_default()
            .contains("disciplined cost control")),
        "expected a hit on the cost-control paragraph: {hits:?}"
    );
    assert!(
        hits.iter()
            .any(|h| h["document_id"].as_str() == Some(doc_id.as_str())),
        "expected hits from the ingested document"
    );

    // --- search: document scope, page-2 content ---
    let doc_hits = run_json(&[
        "search", "hedged interest rate exposure",
        "--corpus", &corpus,
        "--doc", &doc_id,
        "--limit", "5",
        "--db", &url,
    ]);
    let doc_hits = doc_hits.as_array().expect("doc search hits array");
    assert!(!doc_hits.is_empty(), "document-scoped search returned no hits");
    assert!(
        doc_hits
            .iter()
            .all(|h| h["document_id"].as_str() == Some(doc_id.as_str())),
        "document scope must not leak other documents: {doc_hits:?}"
    );
    assert!(
        doc_hits.iter().any(|h| h["page"].as_i64() == Some(2)),
        "expected the page-2 swaps paragraph: {doc_hits:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}
