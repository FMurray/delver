//! Word-gap inference tests (slice TW0, docs/DECISIONS.md D-029).
//!
//! TeX-family PDFs encode inter-word spacing as glyph positioning (TJ kern
//! adjustments, Td pen moves, Tc letterspacing) instead of space glyphs;
//! text-run assembly must infer those gaps or phrase search fails
//! ("locationvector"). Synthetic content-stream tests run everywhere (D-009);
//! the real-corpus regression uses ~/datasets/papers/EECS-2025-77.pdf
//! (READ-ONLY, never committed — its copyright notice restricts
//! redistribution) and skips with a message when absent, like the DB gating
//! pattern. The store-level FTS check additionally requires Postgres.
//!
//! Per the D-014/D-018 precedent the PDF builder is duplicated from the
//! sibling suites rather than shared through a test-util crate.

use std::path::PathBuf;
use std::process::Command;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use serde_json::Value;
use uuid::Uuid;

use delver_core::parse::get_page_content;
use delver_store::blocking::DelverStoreBlocking;

// ───────────────────────── synthetic-stream harness ─────────────────────────

/// Build a one-page PDF whose content stream is exactly `ops` (F1 =
/// Helvetica, so glyph metrics are the real FONT_METRICS table).
fn pdf_with_ops(ops: Vec<Operation>) -> Vec<u8> {
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
    let content = Content { operations: ops };
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        content.encode().expect("encode content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });
    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("save pdf");
    bytes
}

/// Parse and join every text element's text with `\n`.
fn extracted_text(pdf_bytes: &[u8]) -> String {
    let doc = Document::load_mem(pdf_bytes).expect("load pdf");
    let pages = get_page_content(&doc).expect("parse pages");
    let mut out = Vec::new();
    for contents in pages.values() {
        for elem in contents.text_store.iter() {
            out.push(elem.text);
        }
    }
    out.join("\n")
}

/// `BT /F1 <size> Tf 72 700 Td <ops…> ET` around the text-showing ops.
fn text_object(size: f32, inner: Vec<Operation>) -> Vec<Operation> {
    let mut ops = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), size.into()]),
        Operation::new("Td", vec![72.into(), 700.into()]),
    ];
    ops.extend(inner);
    ops.push(Operation::new("ET", vec![]));
    ops
}

// ─────────────────────────── synthetic-stream tests ─────────────────────────

/// A TJ numeric adjustment of -400 (0.4 em rightward) between two fragments
/// is a word gap: exactly one space is inserted.
#[test]
fn tj_kern_gap_inserts_space() {
    let ops = text_object(
        12.0,
        vec![Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal("Hello"),
                (-400).into(),
                Object::string_literal("world"),
            ])],
        )],
    );
    let text = extracted_text(&pdf_with_ops(ops));
    assert!(text.contains("Hello world"), "text: {text:?}");
    assert!(!text.contains("Hello  world"), "text: {text:?}");
}

/// Kerning jitter and ligature adjustments are small offsets — no space. A
/// -100 kern (0.1 em) stays intra-word; a positive number (leftward) too.
#[test]
fn small_tj_kern_no_space() {
    let ops = text_object(
        12.0,
        vec![Operation::new(
            "TJ",
            vec![Object::Array(vec![
                Object::string_literal("Hel"),
                (-100).into(),
                Object::string_literal("lo"),
                100.into(),
                Object::string_literal("!"),
            ])],
        )],
    );
    let text = extracted_text(&pdf_with_ops(ops));
    assert!(text.contains("Hello!"), "text: {text:?}");
}

/// A Td move to the next word on the same line (large horizontal gap, no
/// vertical drift) inserts a space through the same position math as TJ.
#[test]
fn td_move_to_next_word_inserts_space() {
    let ops = text_object(
        12.0,
        vec![
            Operation::new("Tj", vec![Object::string_literal("Hello")]),
            Operation::new("Td", vec![40.into(), 0.into()]),
            Operation::new("Tj", vec![Object::string_literal("world")]),
        ],
    );
    let text = extracted_text(&pdf_with_ops(ops));
    assert!(text.contains("Hello world"), "text: {text:?}");
    assert!(!text.contains("Hello  world"), "text: {text:?}");
}

/// No leading space at run start, even when the run opens with a big kern.
#[test]
fn run_start_gets_no_leading_space() {
    let ops = text_object(
        12.0,
        vec![Operation::new(
            "TJ",
            vec![Object::Array(vec![
                (-3000).into(),
                Object::string_literal("Hello"),
            ])],
        )],
    );
    let text = extracted_text(&pdf_with_ops(ops));
    assert!(text.contains("Hello"), "text: {text:?}");
    assert!(!text.contains(" Hello"), "leading space: {text:?}");
}

/// Positions compose, numbers don't: with 0.5 em letterspacing (Tc), a +500
/// TJ number cancels to zero net gap (intra-word), while a +1 number leaves
/// ~0.5 em (word gap) — the EECS-2025-77 letterspaced-heading pattern.
#[test]
fn tc_letterspacing_composes_with_kerns() {
    let ops = text_object(
        12.0,
        vec![
            Operation::new("Tc", vec![6.into()]),
            Operation::new(
                "TJ",
                vec![Object::Array(vec![
                    Object::string_literal("a"),
                    500.into(),
                    Object::string_literal("b"),
                    500.into(),
                    Object::string_literal("c"),
                    1.into(),
                    Object::string_literal("d"),
                ])],
            ),
        ],
    );
    let text = extracted_text(&pdf_with_ops(ops));
    assert!(text.contains("abc d"), "text: {text:?}");
}

/// A Td to the next line (vertical drift) never gets an inferred space, even
/// when the x displacement alone would clear the threshold: line-wrap joins
/// are hyphenation-sensitive and stay out of scope (documented in D-029).
#[test]
fn newline_td_gets_no_space() {
    let ops = text_object(
        12.0,
        vec![
            Operation::new("Tj", vec![Object::string_literal("line1")]),
            Operation::new("Td", vec![60.into(), (-14).into()]),
            Operation::new("Tj", vec![Object::string_literal("line2")]),
        ],
    );
    let text = extracted_text(&pdf_with_ops(ops));
    assert!(text.contains("line1line2"), "text: {text:?}");
}

/// Real space glyphs are never doubled: gaps adjacent to existing whitespace
/// are skipped, so the SEC-style multi-space column separator (the D-018
/// cell-fragment signal) survives byte-for-byte even under a wide Tw.
#[test]
fn real_spaces_never_doubled() {
    let ops = text_object(
        12.0,
        vec![
            Operation::new("Tw", vec![4.into()]),
            Operation::new("Tj", vec![Object::string_literal("A  B")]),
        ],
    );
    let text = extracted_text(&pdf_with_ops(ops));
    assert!(text.contains("A  B"), "text: {text:?}");
    assert!(!text.contains("A   B"), "extra space injected: {text:?}");
}

// ──────────────────────── real-corpus regression (gated) ────────────────────

/// The gap-encoded regression fixture (D-007: datasets live outside the
/// repo; `DELVER_TESTDATA` overrides `~/datasets`). Skip-if-absent.
fn fixture_pdf(test_name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("DELVER_TESTDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("datasets")))?;
    let path = base.join("papers").join("EECS-2025-77.pdf");
    if path.exists() {
        Some(path)
    } else {
        eprintln!(
            "SKIP {test_name}: fixture not present at {} (fetch locally; never commit it)",
            path.display()
        );
        None
    }
}

/// Parse-level regression: the TeX-encoded word gaps of EECS-2025-77 come
/// back as single spaces ("locationvector" → "location vector").
#[test]
fn fixture_word_gaps_parse_level() {
    let Some(path) = fixture_pdf("fixture_word_gaps_parse_level") else {
        return;
    };
    let bytes = std::fs::read(&path).expect("read fixture");
    let text = extracted_text(&bytes);
    for phrase in ["location vector", "TWIX System Architecture", "from Templatized"] {
        assert!(text.contains(phrase), "missing {phrase:?} in extracted text");
    }
    assert!(
        !text.contains("locationvector"),
        "gap-encoded join survived parsing"
    );
}

fn db_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| delver::DEFAULT_DB_URL.to_string())
}

fn run_json(args: &[&str]) -> Value {
    let exe = env!("CARGO_BIN_EXE_delver");
    let output = Command::new(exe).args(args).output().expect("run delver");
    assert!(
        output.status.success(),
        "delver {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON document")
}

/// Store-level FTS regression: fresh-ingest the fixture into a scratch
/// corpus and confirm `text_search "location vector"` finds it (the joined
/// "locationvector" is a single lexeme, invisible to the query). DB-gated
/// per D-009 on top of the fixture gate.
#[test]
fn fixture_word_gaps_fts_search() {
    let Some(path) = fixture_pdf("fixture_word_gaps_fts_search") else {
        return;
    };
    let url = db_url();
    if DelverStoreBlocking::connect(&url).is_err() {
        eprintln!(
            "SKIP fixture_word_gaps_fts_search: Postgres unreachable at {url}; \
             set DATABASE_URL or run scripts/dev-db.sh"
        );
        return;
    }

    // Unique corpus name = fresh ingest (the D-008 dedup key is
    // (corpus, sha256, parse_version)), so the pre-fix `papers` corpus
    // ingest of the same bytes cannot satisfy this test.
    let corpus = format!("wordgap-tw0-{}", Uuid::new_v4());
    let pdf = path.to_str().expect("fixture path is utf-8");
    let receipt = run_json(&["index", pdf, "--corpus", &corpus, "--db", &url]);
    assert_eq!(receipt["created"], Value::Bool(true), "receipt: {receipt}");

    let hits = run_json(&[
        "search",
        "location vector",
        "--corpus",
        &corpus,
        "--db",
        &url,
    ]);
    let hits = hits.as_array().expect("search returns a JSON array");
    assert!(
        !hits.is_empty(),
        "text_search \"location vector\" found nothing in the fresh ingest"
    );
}
