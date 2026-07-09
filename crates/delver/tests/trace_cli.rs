//! T1 execution-tracing contract through the real `delver` binary
//! (docs/DECISIONS.md D-027).
//!
//! The load-bearing assertions:
//! * default = EXACTLY the pre-T1 behavior — stdout unchanged AND stderr
//!   0 bytes for `index`/`query`/`search`;
//! * with tracing enabled, stdout stays BYTE-IDENTICAL (trace output rides
//!   on stderr / a file / the collector, never stdout);
//! * the span vocabulary actually shows up (pass1, boundary_candidate,
//!   end_boundary, ingest, text_search, ...);
//! * `--trace` with an unreachable collector warns and falls back to the
//!   stderr tree — it never silently does nothing.
//!
//! DB-backed tests follow the D-009 pattern: synthetic in-memory PDF, skip
//! with an explicit message when Postgres is unreachable. The PDF builder is
//! duplicated from store_cli.rs by design (see D-012: no shared test-util
//! crate for ~60 lines).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use uuid::Uuid;

use delver_store::blocking::DelverStoreBlocking;

const HEADING_1: &str = "Management Discussion and Analysis";
const BODY_1A: &str = "Revenue grew steadily across all reporting segments during the fiscal year.";
const BODY_1B: &str = "Operating expenses stayed flat thanks to disciplined cost control programs.";
const HEADING_2: &str = "Quantitative and Qualitative Disclosures";
const BODY_2A: &str = "Interest rate exposure remains hedged through a portfolio of fixed rate swaps.";

/// Section with an explicit end_match plus chunking — exercises pass 1
/// (start/end boundary candidates) and pass 2 (chunk assignment).
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
    // The local dev default lives in one place (D-002); reusing the exported
    // constant keeps this file free of connection-string literals.
    std::env::var("DATABASE_URL").unwrap_or_else(|_| delver::DEFAULT_DB_URL.to_string())
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
    let dir = std::env::temp_dir().join(format!("delver-trace-cli-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Run the binary hermetically: trace-related env stripped so an ambient
/// DELVER_TRACE/RUST_LOG (or a collector running on :4318) cannot leak into
/// the default-off assertions. `envs` re-adds what a test needs.
fn run(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_delver"));
    cmd.args(args)
        .env_remove("DELVER_TRACE")
        .env_remove("RUST_LOG")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT");
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("spawn delver binary");
    assert!(
        output.status.success(),
        "`delver {}` failed with {}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn assert_contains_all(haystack: &str, needles: &[&str], context: &str) {
    for needle in needles {
        assert!(
            haystack.contains(needle),
            "{context}: expected {needle:?} in trace stderr; got:\n{haystack}"
        );
    }
}

/// Default off + stdout invariance + span vocabulary, end to end over a
/// synthetic document: index (ingest + dedup), query --doc (hydrate →
/// compile → pass1 boundaries → pass2 → chunk), search (text_search).
#[test]
fn trace_default_off_and_stdout_byte_identical_when_tracing() {
    let url = db_url();
    if !db_available(&url, "trace_default_off_and_stdout_byte_identical_when_tracing") {
        return;
    }

    let dir = scratch_dir();
    let pdf_path = dir.join("synthetic.pdf");
    fs::write(&pdf_path, build_test_pdf()).expect("write synthetic pdf");
    let template_path = dir.join("synthetic.tmpl");
    fs::write(&template_path, TEMPLATE).expect("write template");
    let pdf = pdf_path.to_str().expect("utf8 pdf path");
    let template = template_path.to_str().expect("utf8 template path");
    let corpus = format!("trace-cli-{}", Uuid::new_v4());

    // --- index: default run must keep stderr at exactly 0 bytes ---
    let indexed = run(&["index", pdf, "--corpus", &corpus, "--db", &url], &[]);
    assert!(
        indexed.stderr.is_empty(),
        "default index must write 0 stderr bytes, got:\n{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&indexed.stdout).expect("index receipt is JSON");
    let doc_id = receipt["document_id"].as_str().expect("document_id").to_string();

    // --- re-index (dedup) with tracing: stdout identical, `ingest` span +
    // dedup narration on stderr ---
    let replay_default = run(&["index", pdf, "--corpus", &corpus, "--db", &url], &[]);
    let replay_traced = run(
        &["index", pdf, "--corpus", &corpus, "--db", &url, "--trace-stderr"],
        &[],
    );
    assert_eq!(
        replay_default.stdout, replay_traced.stdout,
        "index stdout must be byte-identical with --trace-stderr"
    );
    let index_trace = String::from_utf8_lossy(&replay_traced.stderr);
    assert_contains_all(
        &index_trace,
        &["cli.index", "ingest", "dedup hit", "ensure_corpus"],
        "index trace",
    );

    // --- query --doc: default stderr is 0 bytes ---
    let query_args = [
        "query",
        "--template",
        template,
        "--doc",
        &doc_id,
        "--db",
        &url,
        "--tokenizer-model",
        "none",
    ];
    let query_default = run(&query_args, &[]);
    assert!(
        query_default.stderr.is_empty(),
        "default query must write 0 stderr bytes, got:\n{}",
        String::from_utf8_lossy(&query_default.stderr)
    );

    // --- query --doc with tracing: stdout byte-identical, full pass-1/2
    // narration on stderr ---
    let mut traced_args = query_args.to_vec();
    traced_args.push("--trace-stderr");
    let query_traced = run(&traced_args, &[]);
    assert_eq!(
        query_default.stdout, query_traced.stdout,
        "query stdout must be byte-identical with --trace-stderr"
    );
    let query_trace = String::from_utf8_lossy(&query_traced.stderr);
    assert_contains_all(
        &query_trace,
        &[
            "cli.query",
            "load_document",
            "hydrate",
            "compile_template",
            "build_index",
            "pass1",
            "start_boundary",
            "boundary_candidate",
            "end_boundary",
            "explicit_end_match",
            "pass2",
            "chunk",
        ],
        "query trace",
    );

    // --- DELVER_TRACE=1 with an unreachable collector: stdout still
    // byte-identical; one warning + tree fallback (never silently nothing) ---
    let query_env_traced = run(
        &query_args,
        &[
            ("DELVER_TRACE", "1"),
            // Discard-port endpoint that cannot accept connections.
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9"),
        ],
    );
    assert_eq!(
        query_default.stdout, query_env_traced.stdout,
        "query stdout must be byte-identical under DELVER_TRACE=1"
    );
    let fallback_trace = String::from_utf8_lossy(&query_env_traced.stderr);
    assert_contains_all(
        &fallback_trace,
        &["unreachable", "falling back", "pass1"],
        "OTLP-unreachable fallback",
    );

    // --- search: default stderr 0; traced stdout identical + text_search ---
    let search_args = ["search", HEADING_1, "--corpus", &corpus, "--db", &url];
    let search_default = run(&search_args, &[]);
    assert!(
        search_default.stderr.is_empty(),
        "default search must write 0 stderr bytes, got:\n{}",
        String::from_utf8_lossy(&search_default.stderr)
    );
    let mut search_traced_args = search_args.to_vec();
    search_traced_args.push("--trace-stderr");
    let search_traced = run(&search_traced_args, &[]);
    assert_eq!(
        search_default.stdout, search_traced.stdout,
        "search stdout must be byte-identical with --trace-stderr"
    );
    assert_contains_all(
        &String::from_utf8_lossy(&search_traced.stderr),
        &["cli.search", "text_search", "hits"],
        "search trace",
    );
}

/// `--trace-json` writes structured JSON-lines spans/events to the file;
/// stdout stays byte-identical and stderr stays at 0 bytes (no tree unless
/// asked for).
#[test]
fn trace_json_writes_span_lines_and_keeps_stderr_empty() {
    let url = db_url();
    if !db_available(&url, "trace_json_writes_span_lines_and_keeps_stderr_empty") {
        return;
    }

    let dir = scratch_dir();
    let pdf_path = dir.join("synthetic.pdf");
    fs::write(&pdf_path, build_test_pdf()).expect("write synthetic pdf");
    let pdf = pdf_path.to_str().expect("utf8 pdf path");
    let corpus = format!("trace-json-{}", Uuid::new_v4());
    let trace_path = dir.join("trace.jsonl");
    let trace_file = trace_path.to_str().expect("utf8 trace path");

    // First ingest creates; the two runs compared below are both idempotent
    // replays (identical receipts by D-008).
    run(&["index", pdf, "--corpus", &corpus, "--db", &url], &[]);
    let default_run = run(&["index", pdf, "--corpus", &corpus, "--db", &url], &[]);
    let traced_run = run(
        &[
            "index", pdf, "--corpus", &corpus, "--db", &url, "--trace-json", trace_file,
        ],
        &[],
    );
    assert_eq!(
        default_run.stdout, traced_run.stdout,
        "index stdout must be byte-identical with --trace-json"
    );
    assert!(
        traced_run.stderr.is_empty(),
        "--trace-json alone must keep stderr at 0 bytes, got:\n{}",
        String::from_utf8_lossy(&traced_run.stderr)
    );

    let lines = fs::read_to_string(&trace_path).expect("read trace file");
    let mut span_names = Vec::new();
    for line in lines.lines() {
        let value: serde_json::Value =
            serde_json::from_str(line).expect("every trace line is a JSON document");
        if let Some(name) = value["span"]["name"].as_str() {
            span_names.push(name.to_string());
        }
    }
    for expected in ["cli.index", "connect", "ingest"] {
        assert!(
            span_names.iter().any(|n| n == expected),
            "expected span {expected:?} in {trace_file}; saw {span_names:?}"
        );
    }
}

/// Minimal in-test OTLP/HTTP endpoint: accepts POSTs on a loopback port,
/// answers `200 {}`, and hands every request body to the caller. Verifies
/// the hand-rolled exporter end to end (probe + export) without external
/// collector binaries.
fn spawn_otlp_sink() -> (String, std::sync::mpsc::Receiver<(String, String)>) {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let endpoint = format!("http://{}", listener.local_addr().expect("local addr"));
    let (sender, receiver) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let mut reader = BufReader::new(stream);

            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let mut content_length = 0usize;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).is_err() {
                    break;
                }
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                if let Some(value) = header
                    .to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(str::trim)
                    .and_then(|v| v.parse::<usize>().ok())
                {
                    content_length = value;
                }
            }
            let mut body = vec![0u8; content_length];
            if reader.read_exact(&mut body).is_err() {
                continue;
            }
            let _ = reader.get_mut().write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
            );
            let _ = sender.send((
                request_line.trim_end().to_string(),
                String::from_utf8_lossy(&body).into_owned(),
            ));
        }
    });

    (endpoint, receiver)
}

/// `--trace` against a reachable collector: stdout byte-identical, the
/// exporter POSTs spec-shaped OTLP JSON to /v1/traces (service.name=delver,
/// one trace rooted at cli.query, pass1 parented under it, events attached),
/// and reports the export on stderr.
#[test]
fn trace_exports_otlp_json_to_a_reachable_collector() {
    let url = db_url();
    if !db_available(&url, "trace_exports_otlp_json_to_a_reachable_collector") {
        return;
    }

    let dir = scratch_dir();
    let pdf_path = dir.join("synthetic.pdf");
    fs::write(&pdf_path, build_test_pdf()).expect("write synthetic pdf");
    let template_path = dir.join("synthetic.tmpl");
    fs::write(&template_path, TEMPLATE).expect("write template");
    let pdf = pdf_path.to_str().expect("utf8 pdf path");
    let template = template_path.to_str().expect("utf8 template path");
    let corpus = format!("trace-otlp-{}", Uuid::new_v4());

    let indexed = run(&["index", pdf, "--corpus", &corpus, "--db", &url], &[]);
    let receipt: serde_json::Value =
        serde_json::from_slice(&indexed.stdout).expect("index receipt is JSON");
    let doc_id = receipt["document_id"].as_str().expect("document_id").to_string();

    let query_args = [
        "query",
        "--template",
        template,
        "--doc",
        &doc_id,
        "--db",
        &url,
        "--tokenizer-model",
        "none",
    ];
    let default_run = run(&query_args, &[]);

    let (endpoint, bodies) = spawn_otlp_sink();
    let mut traced_args = query_args.to_vec();
    traced_args.push("--trace");
    let traced_run = run(&traced_args, &[("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint)]);

    assert_eq!(
        default_run.stdout, traced_run.stdout,
        "query stdout must be byte-identical with --trace"
    );
    let stderr = String::from_utf8_lossy(&traced_run.stderr);
    assert!(
        stderr.contains("trace: exported"),
        "expected export confirmation on stderr, got:\n{stderr}"
    );

    // First request is the reachability probe (empty resourceSpans), second
    // is the export. Both hit POST /v1/traces.
    let (probe_line, probe_body) = bodies
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("probe request");
    assert!(probe_line.starts_with("POST /v1/traces"), "probe: {probe_line}");
    assert_eq!(probe_body, r#"{"resourceSpans":[]}"#);

    let (export_line, export_body) = bodies
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("export request");
    assert!(export_line.starts_with("POST /v1/traces"), "export: {export_line}");
    let export: serde_json::Value =
        serde_json::from_str(&export_body).expect("export body is JSON");

    let resource_attrs = &export["resourceSpans"][0]["resource"]["attributes"];
    assert!(
        resource_attrs
            .as_array()
            .expect("resource attributes")
            .iter()
            .any(|a| a["key"] == "service.name" && a["value"]["stringValue"] == "delver"),
        "service.name=delver missing: {resource_attrs}"
    );

    let spans = export["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .expect("spans array");
    let find = |name: &str| {
        spans
            .iter()
            .find(|s| s["name"] == name)
            .unwrap_or_else(|| panic!("span {name:?} missing from export"))
    };
    let root = find("cli.query");
    assert_eq!(root["parentSpanId"], "", "cli.query must be the trace root");
    assert_eq!(root["traceId"].as_str().map(str::len), Some(32));
    let pass1 = find("pass1");
    assert_eq!(
        pass1["traceId"], root["traceId"],
        "one trace per CLI run"
    );
    // pass1's parent chain leads to the root (direct parent is match_template).
    assert_ne!(pass1["parentSpanId"], "");
    // Candidate events attach to the boundary-search span they fired in.
    let start_boundary = find("start_boundary");
    assert!(
        start_boundary["events"]
            .as_array()
            .is_some_and(|events| events.iter().any(|e| e["name"] == "boundary_candidate")),
        "start_boundary span must carry boundary_candidate events; got {}",
        start_boundary["events"]
    );
    assert_eq!(
        start_boundary["parentSpanId"], pass1["spanId"],
        "start_boundary nests under pass1"
    );
    for expected in ["connect", "load_document", "hydrate", "end_boundary", "pass2"] {
        find(expected);
    }
}

/// Byte-exact regression baselines against the shared dev database (the two
/// real 3M 10-K documents). Skips — with a message — when those documents
/// are not present in the reachable database or the HF tokenizer cache is
/// unavailable (the baselines were captured with the default tokenizer).
#[test]
fn shared_dev_db_query_baselines_hold_with_and_without_trace() {
    let url = db_url();
    if !db_available(&url, "shared_dev_db_query_baselines_hold_with_and_without_trace") {
        return;
    }

    let template = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/10k.tmpl");
    for (doc, expected_bytes) in [
        ("1129cea2-a617-429a-baec-a9739af9ddfe", 414_534usize),
        ("56e30967-eff1-4c0f-acdb-3fa13b30d4ef", 466_678usize),
    ] {
        let args = ["query", "--template", template, "--doc", doc, "--db", &url];
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_delver"));
        cmd.args(args)
            .env_remove("DELVER_TRACE")
            .env_remove("RUST_LOG")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT");
        let output = cmd.output().expect("spawn delver binary");
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() && stderr.contains("has no stored elements") {
            eprintln!(
                "SKIP shared_dev_db_query_baselines: document {doc} not in this database"
            );
            return;
        }
        if stderr.contains("tokenizer") {
            eprintln!(
                "SKIP shared_dev_db_query_baselines: default tokenizer unavailable \
                 (baselines were captured with it)"
            );
            return;
        }
        assert!(output.status.success(), "query --doc {doc} failed: {stderr}");
        assert!(
            output.stderr.is_empty(),
            "default query must write 0 stderr bytes, got:\n{stderr}"
        );
        assert_eq!(
            output.stdout.len(),
            expected_bytes,
            "regression baseline for {doc} moved (D-013/D-018)"
        );

        // Same run with tracing: stdout byte-identical.
        let mut traced = Command::new(env!("CARGO_BIN_EXE_delver"));
        traced
            .args(args)
            .arg("--trace-stderr")
            .env_remove("DELVER_TRACE")
            .env_remove("RUST_LOG")
            .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT");
        let traced = traced.output().expect("spawn delver binary");
        assert!(traced.status.success());
        assert_eq!(
            output.stdout, traced.stdout,
            "query stdout for {doc} must be byte-identical with --trace-stderr"
        );
        let trace = String::from_utf8_lossy(&traced.stderr);
        assert_contains_all(
            &trace,
            &["pass1", "boundary_candidate", "end_boundary", "style_similarity"],
            "10-K query trace",
        );
    }
}
