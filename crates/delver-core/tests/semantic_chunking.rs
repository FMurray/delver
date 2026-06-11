//! Stage B slice 4 (docs/DECISIONS.md D-020): `TextChunk(method="semantic")`
//! is real — sentence-ish segmentation, embedding-driven valley breakpoints,
//! chunkSize budget cap, chunkOverlap segment carry — and every config that
//! cannot execute fails loud (D-006).
//!
//! Per D-009 the test PDF is generated in-test via lopdf (builder copied from
//! tests/match_exec.rs by design — no shared test-util crate for ~60 lines).
//! The embedder is delver-embed's deterministic MockEmbedder: two designed
//! topics with high within-topic similarity and a deep valley between them.

use std::collections::HashMap;
use std::sync::Arc;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use serde_json::Value;

use delver_core::docql::parse_template;
use delver_core::embed::SharedEmbedder;
use delver_core::layout::MatchContext;
use delver_core::parse::get_page_content;
use delver_core::process_parsed;
use delver_embed::MockEmbedder;

// One sentence per text run, every text ends with '.', so each element is
// exactly one sentence-ish segment. Char costs (no tokenizer in these tests):
// A1=50, A2=45, A3=46, B1=47, B2=50, B3=48.
const A1: &str = "Solar panels convert sunlight into electric power.";
const A2: &str = "Wind turbines harvest energy from moving air.";
const A3: &str = "Hydroelectric dams draw power from river flow.";
const B1: &str = "Sourdough bread needs a lively starter culture.";
const B2: &str = "Croissants get flaky layers from laminated butter.";
const B3: &str = "Bagels are boiled before baking for chewy crust.";

fn push_text_ops(ops: &mut Vec<Operation>, text: &str, size: f32, x: f32, y: f32) {
    ops.push(Operation::new("BT", vec![]));
    ops.push(Operation::new("Tf", vec!["F1".into(), size.into()]));
    ops.push(Operation::new("Td", vec![x.into(), y.into()]));
    ops.push(Operation::new("Tj", vec![Object::string_literal(text)]));
    ops.push(Operation::new("ET", vec![]));
}

/// Build a 1-page PDF with the six topic sentences as six 11pt text runs.
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
    for (i, text) in [A1, A2, A3, B1, B2, B3].iter().enumerate() {
        push_text_ops(&mut ops, text, 11.0, 72.0, 700.0 - 20.0 * i as f32);
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

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize pdf to memory");
    bytes
}

/// Run a template over the synthetic PDF with an optional embedder; returns
/// the raw `process_parsed` result (outputs JSON, or the processing error).
fn run(template: &str, embedder: SharedEmbedder) -> anyhow::Result<String> {
    let doc = Document::load_mem(&build_test_pdf()).expect("load synthetic pdf");
    let pages = get_page_content(&doc).expect("extract page content");
    let match_context = MatchContext {
        destinations: Default::default(),
        embedder,
    };
    process_parsed(&pages, &match_context, template, None)
}

/// Two designed topics: within-topic adjacent cosines ~0.99, the A3->B1
/// boundary ~0.20 — a single deep valley at the topic break.
fn topic_embedder() -> SharedEmbedder {
    let seed: HashMap<String, Vec<f32>> = [
        (A1, vec![1.0, 0.0]),
        (A2, vec![0.99, 0.14]),
        (A3, vec![0.98, 0.2]),
        (B1, vec![0.0, 1.0]),
        (B2, vec![0.14, 0.99]),
        (B3, vec![0.2, 0.98]),
    ]
    .into_iter()
    .map(|(text, vector)| (text.to_string(), vector))
    .collect();
    SharedEmbedder::new(Arc::new(MockEmbedder::new(seed)))
}

fn chunks_of(outputs_json: &str) -> Vec<Value> {
    let outputs: Value = serde_json::from_str(outputs_json).expect("outputs must be JSON");
    outputs.as_array().expect("outputs must be an array").clone()
}

fn texts_of(chunks: &[Value]) -> Vec<String> {
    chunks
        .iter()
        .map(|c| c["text"].as_str().expect("chunk has text").to_string())
        .collect()
}

fn segment_counts_of(chunks: &[Value]) -> Vec<u64> {
    chunks
        .iter()
        .map(|c| {
            c["metadata"]["segment_count"]
                .as_u64()
                .expect("semantic chunk metadata has segment_count")
        })
        .collect()
}

// ── (a) valley split with the default breakpoint percentile ────────────────

#[test]
fn semantic_chunking_splits_at_designed_valley() {
    let template = r#"TextChunk(method="semantic", chunkSize=1000, chunkOverlap=0)"#;
    let outputs = run(template, topic_embedder()).expect("semantic template must execute");
    let chunks = chunks_of(&outputs);

    assert_eq!(
        texts_of(&chunks),
        vec![[A1, A2, A3].join(" "), [B1, B2, B3].join(" ")],
        "chunk boundary must land exactly at the topic break"
    );
    for chunk in &chunks {
        assert_eq!(
            chunk["metadata"]["method"].as_str(),
            Some("semantic"),
            "semantic chunks must note their method: {chunk}"
        );
    }
    assert_eq!(segment_counts_of(&chunks), vec![3, 3]);
}

// ── (b) chunkSize budget cap (char-based here: no tokenizer configured) ─────

#[test]
fn semantic_chunking_respects_chunk_size_budget() {
    // Segment char costs: 50/45/46 | 47/50/48. Budget 120 fits two segments
    // of a topic but never three, so the cap closes mid-topic and the valley
    // closes at the topic break: [A1 A2] [A3] [B1 B2] [B3].
    let template = r#"TextChunk(method="semantic", chunkSize=120, chunkOverlap=0)"#;
    let outputs = run(template, topic_embedder()).expect("semantic template must execute");
    let chunks = chunks_of(&outputs);

    assert_eq!(
        texts_of(&chunks),
        vec![
            [A1, A2].join(" "),
            A3.to_string(),
            [B1, B2].join(" "),
            B3.to_string(),
        ],
    );
    assert_eq!(segment_counts_of(&chunks), vec![2, 1, 2, 1]);
}

// ── (c) chunkOverlap carries trailing segments forward ─────────────────────

#[test]
fn semantic_chunking_overlap_carries_trailing_segment() {
    // Overlap 10 chars < any segment cost, so exactly one trailing segment
    // (A3) is carried into the next chunk — and the consumed valley must not
    // immediately re-split the overlap-carried tail.
    let template = r#"TextChunk(method="semantic", chunkSize=1000, chunkOverlap=10)"#;
    let outputs = run(template, topic_embedder()).expect("semantic template must execute");
    let chunks = chunks_of(&outputs);

    assert_eq!(
        texts_of(&chunks),
        vec![[A1, A2, A3].join(" "), [A3, B1, B2, B3].join(" ")],
        "second chunk must start with the overlap-carried trailing segment"
    );
    assert_eq!(segment_counts_of(&chunks), vec![3, 4]);
}

// ── (d) fail-loud: semantic without an embedder (D-006) ────────────────────

#[test]
fn semantic_without_embedder_errors_naming_element() {
    let template = r#"TextChunk(method="semantic", as="risk_chunks")"#;
    let err = run(template, SharedEmbedder::default())
        .expect_err("method=\"semantic\" without an embedder must be a hard error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("risk_chunks"),
        "error must name the TextChunk element: {msg}"
    );
    assert!(
        msg.contains("--embed-endpoint") && msg.contains("DELVER_EMBED_ENDPOINT"),
        "error must cite the remedies: {msg}"
    );
}

// ── (e) fail-loud: unknown method values (D-006) ───────────────────────────

#[test]
fn unknown_method_errors_listing_supported_values() {
    let template = r#"TextChunk(method="vibes")"#;
    let err = parse_template(template).expect_err("unknown method must fail at compile");
    let msg = err.to_string();
    assert!(
        msg.contains("vibes"),
        "error must echo the unknown value: {msg}"
    );
    assert!(
        msg.contains("\"tokens\"") && msg.contains("\"semantic\""),
        "error must list the supported values: {msg}"
    );
}

#[test]
fn breakpoint_percentile_requires_semantic_method() {
    let template = r#"TextChunk(breakpointPercentile=25)"#;
    let err =
        parse_template(template).expect_err("percentile without method=semantic must fail loud");
    let msg = err.to_string();
    assert!(
        msg.contains("breakpointPercentile") && msg.contains("semantic"),
        "error must explain the attribute is semantic-only: {msg}"
    );
}

// ── (f) default / explicit tokens path unchanged ────────────────────────────

#[test]
fn default_and_explicit_tokens_method_are_identical_and_unannotated() {
    let default_outputs = run(
        r#"TextChunk(chunkSize=400, chunkOverlap=0)"#,
        SharedEmbedder::default(),
    )
    .expect("default template must execute");
    let tokens_outputs = run(
        r#"TextChunk(method="tokens", chunkSize=400, chunkOverlap=0)"#,
        SharedEmbedder::default(),
    )
    .expect("explicit tokens template must execute");

    let default_json: Value = serde_json::from_str(&default_outputs).expect("JSON");
    let tokens_json: Value = serde_json::from_str(&tokens_outputs).expect("JSON");
    assert_eq!(
        default_json, tokens_json,
        "method=\"tokens\" must be exactly the default behavior"
    );

    for chunk in chunks_of(&default_outputs) {
        let metadata = chunk["metadata"]
            .as_object()
            .expect("chunk metadata is an object");
        assert!(
            !metadata.contains_key("method") && !metadata.contains_key("segment_count"),
            "default path must not gain semantic metadata keys: {chunk}"
        );
    }
}
