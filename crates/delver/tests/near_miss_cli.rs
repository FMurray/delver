//! D-024 CLI surface: substring-aware `Text(...)` matching and near-miss
//! warnings through the real `delver` binary.
//!
//! * The stderr contract: a match that yields zero candidates prints one
//!   `warning:` line naming the match, with the top-3 closest candidates —
//!   while stdout stays a single pure-JSON document (`[]`) and the exit code
//!   stays 0 (matched-nothing is a data condition, D-013/D-023 precedent).
//! * The user's exact fragment query runs against the real 10-K when the
//!   local database holds it (D-009 skip pattern otherwise).
//!
//! Per D-009 the synthetic PDF is generated in-test via lopdf (builder
//! copied from crates/delver/tests/store_cli.rs by design).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use serde_json::Value;
use uuid::Uuid;

use delver_store::blocking::DelverStoreBlocking;
use delver_store::DocumentId;

/// The 3M 2015 10-K of the user report (indexed locally at parse_version 3).
const TEN_K_DOC_ID: &str = "56e30967-eff1-4c0f-acdb-3fa13b30d4ef";

/// The user's exact failing query, verbatim.
const USER_FRAGMENT_TEMPLATE: &str = r#"
Match<Section> M {
  Text("Management's Discussion", threshold=0.6)
}

Section(match=M) {
  TextChunk(chunkSize=500, chunkOverlap=150)
}
"#;

const NO_MATCH_TEMPLATE: &str = r#"
Match<Section> Missing_Section {
  Text("Zebra Hovercraft Manifest", threshold=0.6)
}

Section(match=Missing_Section) {
  TextChunk(chunkSize=500, chunkOverlap=0)
}
"#;

const FULL_HEADING: &str =
    "Item 7. Management's Discussion and Analysis of Financial Condition and Results of Operations";
const MDA_BODY: &str =
    "Net sales grew across every operating segment while currency headwinds persisted.";

fn db_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://delver:delver@localhost:5433/delver".to_string())
}

fn push_text_ops(ops: &mut Vec<Operation>, text: &str, size: f32, x: f32, y: f32) {
    ops.push(Operation::new("BT", vec![]));
    ops.push(Operation::new("Tf", vec!["F1".into(), size.into()]));
    ops.push(Operation::new("Td", vec![x.into(), y.into()]));
    ops.push(Operation::new("Tj", vec![Object::string_literal(text)]));
    ops.push(Operation::new("ET", vec![]));
}

/// One page: full MD&A heading (24pt) plus one body paragraph.
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

    let mut ops = Vec::new();
    push_text_ops(&mut ops, FULL_HEADING, 24.0, 72.0, 700.0);
    push_text_ops(&mut ops, MDA_BODY, 11.0, 72.0, 660.0);

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
    let dir = std::env::temp_dir().join(format!("delver-near-miss-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn run_delver(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_delver"))
        .args(args)
        .output()
        .expect("spawn delver binary")
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not a single JSON document ({e}):\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

// ── near-miss warning on stderr; stdout stays pure ──────────────────────────

#[test]
fn no_match_query_warns_on_stderr_and_prints_empty_outputs() {
    let dir = scratch_dir();
    let pdf_path = dir.join("synthetic.pdf");
    let template_path = dir.join("no-match.tmpl");
    fs::write(&pdf_path, build_test_pdf()).expect("write pdf");
    fs::write(&template_path, NO_MATCH_TEMPLATE).expect("write template");

    let output = run_delver(&[
        "query",
        "--pdf",
        pdf_path.to_str().unwrap(),
        "--template",
        template_path.to_str().unwrap(),
        "--tokenizer-model",
        "none",
    ]);

    // Matched-nothing is a data condition: exit 0, stdout exactly `[]`.
    assert!(
        output.status.success(),
        "expected exit 0, got {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let outs = stdout_json(&output);
    assert_eq!(outs, serde_json::json!([]), "stdout must be the pure [] payload");

    // The warning names the match, a closest candidate, and its score.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("match 'Missing_Section' matched nothing at threshold 0.6"),
        "stderr must name the missed match and threshold, got:\n{stderr}"
    );
    // (The PDF parser decodes the ASCII apostrophe as U+2019 — quoteright in
    // StandardEncoding — so the excerpt assertion avoids it.)
    assert!(
        stderr.contains("closest:") && stderr.contains("Discussion and Analysis of Financial"),
        "stderr must list a closest candidate, got:\n{stderr}"
    );
    assert!(
        stderr.contains("(0.") && stderr.contains(", p1)"),
        "stderr must carry a sub-threshold score and a page, got:\n{stderr}"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn matching_query_keeps_stderr_empty() {
    let dir = scratch_dir();
    let pdf_path = dir.join("synthetic.pdf");
    let template_path = dir.join("user-mda.tmpl");
    fs::write(&pdf_path, build_test_pdf()).expect("write pdf");
    fs::write(&template_path, USER_FRAGMENT_TEMPLATE).expect("write template");

    let output = run_delver(&[
        "query",
        "--pdf",
        pdf_path.to_str().unwrap(),
        "--template",
        template_path.to_str().unwrap(),
        "--tokenizer-model",
        "none",
    ]);

    assert!(output.status.success());
    let outs = stdout_json(&output);
    let outs = outs.as_array().expect("outputs array");
    assert!(
        !outs.is_empty(),
        "the fragment pattern must match the full-title heading"
    );
    assert_eq!(outs[0]["metadata"]["section"], "M");
    assert!(
        output.stderr.is_empty(),
        "an all-matching run must keep stderr at 0 bytes (D-017), got:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    fs::remove_dir_all(&dir).ok();
}

// ── the user's exact query against the real 10-K (DB-gated, D-009) ──────────

#[test]
fn user_fragment_query_matches_mda_on_real_10k() {
    let url = db_url();
    let store = match DelverStoreBlocking::connect(&url) {
        Ok(store) => store,
        Err(e) => {
            eprintln!(
                "SKIP user_fragment_query_matches_mda_on_real_10k: Postgres unreachable \
                 at {url} ({e}); set DATABASE_URL or run scripts/dev-db.sh"
            );
            return;
        }
    };
    let doc = DocumentId(Uuid::parse_str(TEN_K_DOC_ID).expect("constant uuid"));
    match store.element_count(doc) {
        Ok(n) if n > 0 => {}
        _ => {
            eprintln!(
                "SKIP user_fragment_query_matches_mda_on_real_10k: document {TEN_K_DOC_ID} \
                 not in the local index (index the 3M 2015 10-K at --parse-version 3 first)"
            );
            return;
        }
    }

    let dir = scratch_dir();
    let template_path = dir.join("user-mda.tmpl");
    fs::write(&template_path, USER_FRAGMENT_TEMPLATE).expect("write template");

    let output = run_delver(&[
        "query",
        "--doc",
        TEN_K_DOC_ID,
        "--template",
        template_path.to_str().unwrap(),
        "--db",
        &url,
        "--tokenizer-model",
        "none",
    ]);

    assert!(
        output.status.success(),
        "query failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let outs = stdout_json(&output);
    let outs = outs.as_array().expect("outputs array");
    // The reported bug: this exact query returned [] with zero stderr.
    assert!(
        !outs.is_empty(),
        "the user's fragment query must produce outputs on the real 10-K"
    );
    // Section attribution: every output belongs to match definition M.
    for out in outs {
        assert_eq!(
            out["metadata"]["section"], "M",
            "every output must be attributed to section M"
        );
    }
    // No spurious near-miss warnings once the match succeeds.
    assert!(
        output.stderr.is_empty(),
        "matching run must keep stderr empty, got:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    fs::remove_dir_all(&dir).ok();
}
