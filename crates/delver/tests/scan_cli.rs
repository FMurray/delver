//! CLI tests for slice P1: the scan block in the index receipt, the
//! `--where scan.class=...` filter, and the `--engine` fail-loud paths.
//!
//! DB-gated like store_cli.rs (skip with a message when Postgres is
//! unreachable). No network, no Databricks: every `--engine ai-parse` /
//! `auto` invocation here runs with the Databricks variables removed from
//! the child environment and must fail before any HTTP would happen.
//! Synthetic PDFs are built in-test (D-009); the builders are duplicated
//! from store_cli.rs / delver-core tests by design (D-012 precedent).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use serde_json::Value;
use uuid::Uuid;

use delver_store::blocking::DelverStoreBlocking;

const BODY: &str =
    "Revenue grew steadily across all reporting segments during the fiscal year under review.";

/// Env vars that configure the ai-parse engine; removed from every child
/// process so the tests are hermetic regardless of the developer's shell.
const DBX_ENV: &[&str] = &[
    "DATABRICKS_HOST",
    "DATABRICKS_TOKEN",
    "DELVER_DBX_PROFILE",
    "DELVER_DBX_WAREHOUSE_ID",
    "DELVER_DBX_VOLUME",
];

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

/// Build a one-page PDF: born-digital text page, or a full-page CCITT image
/// page (a raw scan).
fn build_pdf(scanned: bool) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let image_id = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1700,
            "Height" => 2200,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 1,
            "Filter" => "CCITTFaxDecode",
        },
        vec![0u8; 64],
    ));
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
        "XObject" => dictionary! { "Im0" => image_id },
    });

    let mut ops = Vec::new();
    if scanned {
        ops.push(Operation::new("q", vec![]));
        ops.push(Operation::new(
            "cm",
            vec![612.into(), 0.into(), 0.into(), 792.into(), 0.into(), 0.into()],
        ));
        ops.push(Operation::new("Do", vec!["Im0".into()]));
        ops.push(Operation::new("Q", vec![]));
    } else {
        ops.push(Operation::new("BT", vec![]));
        ops.push(Operation::new("Tf", vec!["F1".into(), 12.into()]));
        ops.push(Operation::new("Td", vec![72.into(), 700.into()]));
        ops.push(Operation::new("Tj", vec![Object::string_literal(BODY)]));
        ops.push(Operation::new("ET", vec![]));
    }

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
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize pdf");
    bytes
}

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("delver-scan-cli-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn delver(args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_delver"));
    for var in DBX_ENV {
        cmd.env_remove(var);
    }
    cmd.args(args).output().expect("spawn delver binary")
}

fn run_json(args: &[&str]) -> Value {
    let output = delver(args);
    assert!(
        output.status.success(),
        "`delver {}` failed with {}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON document")
}

fn expect_failure(args: &[&str]) -> String {
    let output = delver(args);
    assert!(
        !output.status.success(),
        "`delver {}` unexpectedly succeeded:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stdout.is_empty(),
        "failed commands must not write to stdout (D-014): {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

struct Fixture {
    url: String,
    corpus: String,
    native_pdf: String,
    scanned_pdf: String,
    template: String,
    _dir: PathBuf,
}

fn setup(test_name: &str) -> Option<Fixture> {
    let url = db_url();
    if !db_available(&url, test_name) {
        return None;
    }
    let dir = scratch_dir();
    let native_pdf = dir.join("native.pdf");
    let scanned_pdf = dir.join("scanned.pdf");
    fs::write(&native_pdf, build_pdf(false)).expect("write native pdf");
    fs::write(&scanned_pdf, build_pdf(true)).expect("write scanned pdf");
    let template = dir.join("chunks.tmpl");
    fs::write(&template, "TextChunk(chunkSize=400)\n").expect("write template");
    Some(Fixture {
        url,
        corpus: format!("scan-cli-{}", Uuid::new_v4()),
        native_pdf: native_pdf.to_str().unwrap().to_string(),
        scanned_pdf: scanned_pdf.to_str().unwrap().to_string(),
        template: template.to_str().unwrap().to_string(),
        _dir: dir,
    })
}

#[test]
fn index_receipt_carries_scan_block_and_engine() {
    let Some(fx) = setup("index_receipt_carries_scan_block_and_engine") else {
        return;
    };

    let scanned = run_json(&[
        "index", &fx.scanned_pdf, "--corpus", &fx.corpus, "--db", &fx.url,
    ]);
    assert_eq!(scanned["created"], Value::Bool(true));
    assert_eq!(scanned["engine"], "native");
    assert_eq!(scanned["scan"]["class"], "scanned_no_text");
    assert_eq!(scanned["scan"]["scanned_page_ratio"], 1.0);
    let page = &scanned["scan"]["pages"][0];
    assert_eq!(page["class"], "scanned_no_text");
    assert!(page["image_coverage"].as_f64().unwrap() > 0.95);

    let native = run_json(&[
        "index", &fx.native_pdf, "--corpus", &fx.corpus, "--db", &fx.url,
    ]);
    assert_eq!(native["scan"]["class"], "native");

    // load_document exposes the same block (via documents.metadata): re-query
    // the receipt path on idempotent re-ingest, which reads stored metadata.
    let again = run_json(&[
        "index", &fx.scanned_pdf, "--corpus", &fx.corpus, "--db", &fx.url,
    ]);
    assert_eq!(again["created"], Value::Bool(false));
    assert_eq!(again["scan"]["class"], "scanned_no_text", "stored metadata");
}

#[test]
fn where_scan_class_filters_search_and_query() {
    let Some(fx) = setup("where_scan_class_filters_search_and_query") else {
        return;
    };
    let scanned = run_json(&[
        "index", &fx.scanned_pdf, "--corpus", &fx.corpus, "--db", &fx.url,
    ]);
    let native = run_json(&[
        "index", &fx.native_pdf, "--corpus", &fx.corpus, "--db", &fx.url,
    ]);
    let scanned_id = scanned["document_id"].as_str().unwrap();
    let native_id = native["document_id"].as_str().unwrap();

    // search: the body text only exists in the native document; filtering on
    // the native class finds it, filtering on the scanned class finds nothing.
    let hits = run_json(&[
        "search", "Revenue", "--corpus", &fx.corpus, "--db", &fx.url,
        "--where", "scan.class=native",
    ]);
    let hits = hits.as_array().expect("hits array");
    assert!(!hits.is_empty(), "native-class search must hit the body");
    assert!(hits.iter().all(|h| h["document_id"] == native_id));

    let none = run_json(&[
        "search", "Revenue", "--corpus", &fx.corpus, "--db", &fx.url,
        "--where", "scan.class=scanned_no_text",
    ]);
    assert_eq!(none, serde_json::json!([]), "scanned doc has no text");

    // query --corpus: the scan-class filter selects exactly the scanned doc.
    let by_doc = run_json(&[
        "query", "--corpus", &fx.corpus, "--template", &fx.template,
        "--db", &fx.url, "--tokenizer-model", "none",
        "--where", "scan.class=scanned_no_text",
    ]);
    let keys: Vec<&String> = by_doc.as_object().expect("object by doc id").keys().collect();
    assert_eq!(keys, vec![scanned_id], "only the scanned document matches");

    let by_doc = run_json(&[
        "query", "--corpus", &fx.corpus, "--template", &fx.template,
        "--db", &fx.url, "--tokenizer-model", "none",
        "--where", "scan.class=native",
    ]);
    let keys: Vec<&String> = by_doc.as_object().expect("object by doc id").keys().collect();
    assert_eq!(keys, vec![native_id]);
}

#[test]
fn where_scan_rejects_bad_classes_and_unknown_keys() {
    let Some(fx) = setup("where_scan_rejects_bad_classes_and_unknown_keys") else {
        return;
    };
    let stderr = expect_failure(&[
        "search", "x", "--corpus", &fx.corpus, "--db", &fx.url,
        "--where", "scan.class=bogus",
    ]);
    for class in ["native", "scanned_no_text", "scanned_ocr", "mixed"] {
        assert!(stderr.contains(class), "error must list {class}: {stderr}");
    }

    let stderr = expect_failure(&[
        "search", "x", "--corpus", &fx.corpus, "--db", &fx.url,
        "--where", "scan.confidence=0.9",
    ]);
    assert!(
        stderr.contains("scan.confidence") && stderr.contains("scan.class"),
        "error must name the unsupported key and the supported one: {stderr}"
    );
}

#[test]
fn engine_ai_parse_without_config_fails_loud_listing_vars() {
    let Some(fx) = setup("engine_ai_parse_without_config_fails_loud_listing_vars") else {
        return;
    };
    let stderr = expect_failure(&[
        "index", &fx.native_pdf, "--corpus", &fx.corpus, "--db", &fx.url,
        "--engine", "ai-parse",
    ]);
    for var in [
        "DATABRICKS_HOST",
        "DATABRICKS_TOKEN",
        "DELVER_DBX_PROFILE",
        "DELVER_DBX_WAREHOUSE_ID",
        "DELVER_DBX_VOLUME",
    ] {
        assert!(stderr.contains(var), "error must name {var}: {stderr}");
    }
}

#[test]
fn engine_auto_routes_scanned_to_failure_and_native_to_native() {
    let Some(fx) = setup("engine_auto_routes_scanned_to_failure_and_native_to_native") else {
        return;
    };

    // auto + scanned + no config: the error explains the classification AND
    // the missing configuration.
    let stderr = expect_failure(&[
        "index", &fx.scanned_pdf, "--corpus", &fx.corpus, "--db", &fx.url,
        "--engine", "auto",
    ]);
    assert!(
        stderr.contains("scanned_no_text"),
        "error must state the classification: {stderr}"
    );
    assert!(
        stderr.contains("scanned_page_ratio"),
        "error must carry the scan evidence: {stderr}"
    );
    assert!(
        stderr.contains("DELVER_DBX_WAREHOUSE_ID") && stderr.contains("DELVER_DBX_VOLUME"),
        "error must list the missing config: {stderr}"
    );

    // auto + native document needs no Databricks config at all.
    let receipt = run_json(&[
        "index", &fx.native_pdf, "--corpus", &fx.corpus, "--db", &fx.url,
        "--engine", "auto",
    ]);
    assert_eq!(receipt["created"], Value::Bool(true));
    assert_eq!(receipt["engine"], "native");
    assert_eq!(receipt["scan"]["class"], "native");
}
