//! Stage C partition tests (docs/DECISIONS.md D-023), through the real
//! `delver` binary: `index --partition` + path inference, `--where` filters
//! on `search` and `query`, and the multi-document `query --corpus` output
//! keyed by document id.
//!
//! Per D-009 the PDFs are generated in-test via lopdf and everything skips
//! with an explicit message when Postgres is unreachable (default dev DB:
//! postgres://delver:delver@localhost:5433/delver). Builder helpers
//! duplicated from store_cli.rs by design (no shared test-util crate).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use serde_json::{json, Value};
use uuid::Uuid;

use delver_store::blocking::DelverStoreBlocking;

const TEMPLATE: &str = "TextChunk(chunkSize=500, chunkOverlap=0)\n";
const BODY_CA: &str = "Sunny California auto loans performed well across the coastal branches.";
const BODY_NY: &str = "New York lending portfolio expanded rapidly during the spring season.";

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

fn build_text_pdf(body: &str) -> Vec<u8> {
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

    let mut ops = Vec::new();
    ops.push(Operation::new("BT", vec![]));
    ops.push(Operation::new("Tf", vec!["F1".into(), 11.into()]));
    ops.push(Operation::new("Td", vec![72.into(), 700.into()]));
    ops.push(Operation::new("Tj", vec![Object::string_literal(body)]));
    ops.push(Operation::new("ET", vec![]));

    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations: ops }
            .encode()
            .expect("encode content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
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
    let dir = std::env::temp_dir().join(format!("delver-partition-cli-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

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

fn run_expect_failure(args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_delver"))
        .args(args)
        .output()
        .expect("spawn delver binary");
    assert!(
        !output.status.success(),
        "`delver {}` unexpectedly succeeded:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn partition_capture_where_filters_and_multi_doc_query() {
    let url = db_url();
    if !db_available(&url, "partition_capture_where_filters_and_multi_doc_query") {
        return;
    }

    let dir = scratch_dir();
    // Hive-style layout: partitions are inferred from directory components.
    let ca_path = dir.join("state=CA").join("type=Auto").join("ca_loans.pdf");
    let ny_path = dir.join("state=NY").join("type=Auto").join("ny_loans.pdf");
    fs::create_dir_all(ca_path.parent().unwrap()).expect("mkdir CA");
    fs::create_dir_all(ny_path.parent().unwrap()).expect("mkdir NY");
    fs::write(&ca_path, build_text_pdf(BODY_CA)).expect("write CA pdf");
    fs::write(&ny_path, build_text_pdf(BODY_NY)).expect("write NY pdf");
    let template_path = dir.join("chunks.tmpl");
    fs::write(&template_path, TEMPLATE).expect("write template");
    let template = template_path.to_str().unwrap();

    let corpus = format!("partition-stage-c-{}", Uuid::new_v4());

    // --- index: inferred partitions land in the receipt and the store ---
    let ca = run_json(&[
        "index", ca_path.to_str().unwrap(),
        "--corpus", &corpus,
        "--db", &url,
    ]);
    assert_eq!(ca["created"], Value::Bool(true));
    assert_eq!(
        ca["partitions"],
        json!({"state": "CA", "type": "Auto"}),
        "inferred partitions: {ca}"
    );
    let ca_id = ca["document_id"].as_str().expect("CA document id").to_string();

    // Explicit --partition merges over inference (and can add new keys);
    // an explicit flag overrides the inferred value for the same key.
    let ny = run_json(&[
        "index", ny_path.to_str().unwrap(),
        "--corpus", &corpus,
        "--partition", "region=East",
        "--partition", "type=Loan",
        "--db", &url,
    ]);
    assert_eq!(
        ny["partitions"],
        json!({"state": "NY", "type": "Loan", "region": "East"}),
        "merged partitions: {ny}"
    );
    let ny_id = ny["document_id"].as_str().expect("NY document id").to_string();
    assert_ne!(ca_id, ny_id);

    // --- search --where: jsonb containment over metadata.partitions ---
    let hits = run_json(&[
        "search", "loans", "--corpus", &corpus,
        "--where", "state=CA",
        "--db", &url,
    ]);
    let hits = hits.as_array().expect("hits array");
    assert!(!hits.is_empty(), "CA-filtered search returned no hits");
    assert!(
        hits.iter().all(|h| h["document_id"].as_str() == Some(ca_id.as_str())),
        "--where state=CA must only hit the CA document: {hits:?}"
    );
    // Multiple --where pairs must all match (CA doc has type=Auto).
    let hits = run_json(&[
        "search", "loans", "--corpus", &corpus,
        "--where", "state=CA", "--where", "type=Auto",
        "--db", &url,
    ]);
    assert!(!hits.as_array().unwrap().is_empty());
    // A non-matching filter yields no hits (not an error).
    let hits = run_json(&[
        "search", "loans", "--corpus", &corpus,
        "--where", "state=TX",
        "--db", &url,
    ]);
    assert_eq!(hits, json!([]));
    // --where + --doc is rejected (clap conflict).
    run_expect_failure(&[
        "search", "loans", "--corpus", &corpus,
        "--doc", &ca_id,
        "--where", "state=CA",
        "--db", &url,
    ]);

    // --- query --corpus: one object keyed by document id ---
    let all = run_json(&[
        "query", "--template", template,
        "--corpus", &corpus,
        "--db", &url,
        "--tokenizer-model", "none",
    ]);
    let all_map = all.as_object().expect("corpus query returns an object");
    assert_eq!(all_map.len(), 2, "expected both documents: {all}");
    for id in [&ca_id, &ny_id] {
        let outputs = all_map
            .get(id.as_str())
            .unwrap_or_else(|| panic!("missing document key {id}: {all}"))
            .as_array()
            .expect("per-document outputs array");
        assert!(!outputs.is_empty(), "document {id} produced no outputs");
    }

    // Filtered: only the CA document, and its outputs equal `query --doc`.
    let ca_only = run_json(&[
        "query", "--template", template,
        "--corpus", &corpus,
        "--where", "state=CA",
        "--db", &url,
        "--tokenizer-model", "none",
    ]);
    let ca_map = ca_only.as_object().expect("object");
    assert_eq!(
        ca_map.keys().collect::<Vec<_>>(),
        vec![&ca_id],
        "--where state=CA must select exactly the CA document: {ca_only}"
    );
    let direct = run_json(&[
        "query", "--template", template,
        "--doc", &ca_id,
        "--db", &url,
        "--tokenizer-model", "none",
    ]);
    assert_eq!(
        ca_map.get(ca_id.as_str()),
        Some(&direct),
        "corpus-query outputs must equal the direct --doc query"
    );
    assert!(
        direct.as_array().unwrap()[0]["text"]
            .as_str()
            .unwrap()
            .contains("Sunny California"),
        "{direct}"
    );

    // Non-matching filter: empty object, exit 0 (data condition, D-023).
    let none = run_json(&[
        "query", "--template", template,
        "--corpus", &corpus,
        "--where", "state=TX",
        "--db", &url,
        "--tokenizer-model", "none",
    ]);
    assert_eq!(none, json!({}));

    // --where without --corpus is rejected (clap `requires`).
    run_expect_failure(&[
        "query", "--template", template,
        "--doc", &ca_id,
        "--where", "state=CA",
        "--db", &url,
    ]);
    // Malformed --partition fails loud.
    run_expect_failure(&[
        "index", ca_path.to_str().unwrap(),
        "--corpus", &corpus,
        "--partition", "noequals",
        "--db", &url,
    ]);

    let _ = fs::remove_dir_all(&dir);
}
