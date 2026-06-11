//! Stage B slice 1 (docs/DECISIONS.md D-005/D-006/D-014): the formerly inert
//! match rules — EmbeddingSim, Regex, Heuristic — execute for real, and every
//! unexecutable configuration fails loud.
//!
//! Per D-009 the test PDF is generated in-test via lopdf (builder copied from
//! crates/delver/tests/store_cli.rs by design — no shared test-util crate for
//! ~60 lines). EmbeddingSim runs against delver-embed's MockEmbedder through
//! the core `Embedder` trait; no network, no database.

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

const HEADING_1: &str = "Management Discussion and Analysis";
const BODY_1A: &str = "Revenue grew steadily across all reporting segments during the fiscal year.";
const BODY_1B: &str = "Operating expenses stayed flat thanks to disciplined cost control programs.";
const HEADING_2: &str = "Quantitative and Qualitative Disclosures";
const BODY_2A: &str = "Interest rate exposure remains hedged through a portfolio of fixed rate swaps.";

fn push_text_ops(ops: &mut Vec<Operation>, text: &str, size: f32, x: f32, y: f32) {
    ops.push(Operation::new("BT", vec![]));
    ops.push(Operation::new("Tf", vec!["F1".into(), size.into()]));
    ops.push(Operation::new("Td", vec![x.into(), y.into()]));
    ops.push(Operation::new("Tj", vec![Object::string_literal(text)]));
    ops.push(Operation::new("ET", vec![]));
}

/// Build a small 2-page PDF entirely in memory: page 1 has a 24pt heading and
/// two 11pt body paragraphs, page 2 has a 24pt heading and one body paragraph.
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

/// Run a template over the synthetic PDF with an optional embedder; returns
/// the raw `process_parsed` result (outputs JSON, or the match error).
fn run(template: &str, embedder: SharedEmbedder) -> anyhow::Result<String> {
    let doc = Document::load_mem(&build_test_pdf()).expect("load synthetic pdf");
    let pages = get_page_content(&doc).expect("extract page content");
    let match_context = MatchContext {
        destinations: Default::default(),
        embedder,
    };
    process_parsed(&pages, &match_context, template, None)
}

/// Concatenated chunk text of all outputs (panics on non-array output).
fn all_chunk_text(outputs_json: &str) -> String {
    let outputs: Value = serde_json::from_str(outputs_json).expect("outputs must be JSON");
    outputs
        .as_array()
        .expect("outputs must be an array")
        .iter()
        .filter_map(|o| o["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn mock_with(seed: &[(&str, Vec<f32>)]) -> SharedEmbedder {
    let map: HashMap<String, Vec<f32>> = seed
        .iter()
        .map(|(text, vector)| (text.to_string(), vector.clone()))
        .collect();
    SharedEmbedder::new(Arc::new(MockEmbedder::new(map)))
}

const EMBEDDING_TEMPLATE: &str = r#"
Match<Section> RevenueHeading {
  EmbeddingSim("financial results overview heading", threshold=0.6, endpoint="databricks-bge")
}
Section(match=RevenueHeading, as="MD&A") {
  TextChunk(chunkSize=400, chunkOverlap=0)
}
"#;

// ── (a) EmbeddingSim execution ──────────────────────────────────────────────

#[test]
fn embedding_sim_matches_heading_via_mock_embedder() {
    // Query and HEADING_1 are near-parallel (cosine ≈ 0.994); HEADING_2 is a
    // below-threshold control (cosine ≈ 0.45 < 0.6); body texts are unseeded
    // and embed to the orthogonal default (cosine 0).
    let embedder = mock_with(&[
        ("financial results overview heading", vec![1.0, 0.0]),
        (HEADING_1, vec![0.9, 0.1]),
        (HEADING_2, vec![0.45, 0.89]),
    ]);

    let outputs = run(EMBEDDING_TEMPLATE, embedder).expect("embedding template must execute");
    let text = all_chunk_text(&outputs);
    assert!(
        text.contains("Revenue grew steadily"),
        "section starting at the embedding-matched heading must include page-1 body: {text}"
    );
    assert!(
        !text.contains("Interest rate exposure"),
        "below-threshold control heading must not start the section: {text}"
    );
}

#[test]
fn embedding_sim_respects_threshold() {
    // Best candidate similarity ≈ 0.45, below the 0.6 threshold: the section
    // must not match at all (empty outputs), rather than matching weakly.
    let embedder = mock_with(&[
        ("financial results overview heading", vec![1.0, 0.0]),
        (HEADING_1, vec![0.45, 0.89]),
    ]);

    let outputs = run(EMBEDDING_TEMPLATE, embedder).expect("embedding template must execute");
    let outputs: Value = serde_json::from_str(&outputs).expect("outputs must be JSON");
    assert_eq!(
        outputs.as_array().map(Vec::len),
        Some(0),
        "no candidate reaches the threshold, so nothing may match: {outputs}"
    );
}

// ── (b) Regex execution ─────────────────────────────────────────────────────

#[test]
fn regex_matches_heading_pattern() {
    let template = r#"
Match<Section> MdaHeading {
  Regex("^Management .* Analysis$")
}
Section(match=MdaHeading, as="MD&A") {
  TextChunk(chunkSize=400, chunkOverlap=0)
}
"#;
    let outputs = run(template, SharedEmbedder::default()).expect("regex template must execute");
    let text = all_chunk_text(&outputs);
    assert!(
        text.contains("Revenue grew steadily"),
        "regex-matched section must include page-1 body: {text}"
    );
    assert!(
        !text.contains("Interest rate exposure"),
        "regex must anchor on heading 1, not page 2: {text}"
    );
}

// ── (c) Heuristic execution ─────────────────────────────────────────────────

#[test]
fn heuristic_font_size_selects_larger_font_heading() {
    // Headings are 24pt, bodies 11pt: fontSize > 14 must anchor the section
    // at the first heading (bodies are not candidates at all).
    let template = r#"
Match<Section> BigText {
  Heuristic(fontSize > 14)
}
Section(match=BigText, as="big") {
  TextChunk(chunkSize=400, chunkOverlap=0)
}
"#;
    let outputs =
        run(template, SharedEmbedder::default()).expect("heuristic template must execute");
    let text = all_chunk_text(&outputs);
    assert!(
        text.contains("Revenue grew steadily"),
        "heuristic-matched section must include page-1 body: {text}"
    );
    assert!(
        !text.contains("Interest rate exposure"),
        "section must start at the first large-font heading, not page 2: {text}"
    );
}

#[test]
fn heuristic_multiple_comparisons_and_together() {
    // fontSize > 14 AND page == 2 leaves exactly HEADING_2 as the anchor.
    let template = r#"
Match<Section> Page2Heading {
  Heuristic(fontSize > 14, page == 2)
}
Section(match=Page2Heading, as="page2") {
  TextChunk(chunkSize=400, chunkOverlap=0)
}
"#;
    let outputs =
        run(template, SharedEmbedder::default()).expect("heuristic template must execute");
    let text = all_chunk_text(&outputs);
    assert!(
        text.contains("Interest rate exposure"),
        "ANDed heuristic must anchor on the page-2 heading: {text}"
    );
    assert!(
        !text.contains("Revenue grew steadily"),
        "page-1 content must be excluded: {text}"
    );
}

// ── FirstMatch combinator (real semantics, D-014) ───────────────────────────

#[test]
fn first_match_falls_through_to_first_matching_alternative() {
    let template = r#"
Match<Section> Fallback {
  FirstMatch(Text("No Such Heading Anywhere In This Document", threshold=0.95), Regex("^Management"))
}
Section(match=Fallback, as="fallback") {
  TextChunk(chunkSize=400, chunkOverlap=0)
}
"#;
    let outputs =
        run(template, SharedEmbedder::default()).expect("FirstMatch template must execute");
    let text = all_chunk_text(&outputs);
    assert!(
        text.contains("Revenue grew steadily"),
        "FirstMatch must fall through to the Regex alternative: {text}"
    );
}

// ── (d) fail-loud (D-006) ───────────────────────────────────────────────────

#[test]
fn embedding_sim_without_embedder_errors_naming_match_block() {
    let err = run(EMBEDDING_TEMPLATE, SharedEmbedder::default())
        .expect_err("EmbeddingSim without an embedder must be a hard error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("RevenueHeading"),
        "error must name the match block: {msg}"
    );
    assert!(
        msg.contains("EmbeddingSim") && msg.contains("--embed-endpoint"),
        "error must say what is missing and how to fix it: {msg}"
    );
}

#[test]
fn unknown_heuristic_property_errors_listing_supported_properties() {
    let template = r#"
Match<Section> Bad {
  Heuristic(fontWeight > 600)
}
Section(match=Bad) { TextChunk() }
"#;
    let err = parse_template(template).expect_err("unknown property must fail at compile");
    let msg = err.to_string();
    assert!(
        msg.contains("fontWeight"),
        "error must name the unknown property: {msg}"
    );
    assert!(
        msg.contains("fontSize") && msg.contains("y_position") && msg.contains("text_length"),
        "error must list the supported properties: {msg}"
    );
}

#[test]
fn invalid_regex_errors_at_template_compile() {
    let template = r#"
Match<Section> Broken {
  Regex("([unclosed")
}
Section(match=Broken) { TextChunk() }
"#;
    let err = parse_template(template).expect_err("invalid regex must fail at compile");
    let msg = err.to_string();
    assert!(
        msg.contains("([unclosed"),
        "error must include the offending pattern: {msg}"
    );
    assert!(
        msg.contains("Broken"),
        "error must name the match block: {msg}"
    );
}

#[test]
fn optional_combinator_errors_not_yet_implemented() {
    let template = r#"
Match<Section> WithOptional {
  Text("Management Discussion and Analysis")
  Optional(Text("Quantitative"))
}
Section(match=WithOptional) { TextChunk() }
"#;
    let err = parse_template(template).expect_err("Optional must not pass through silently");
    let msg = err.to_string();
    assert!(
        msg.contains("Optional") && msg.contains("not yet implemented"),
        "Optional must error explicitly (D-006): {msg}"
    );
}

#[test]
fn unknown_match_definition_reference_errors() {
    let template = r#"Section(match=NoSuchDefinition) { TextChunk() }"#;
    let err = parse_template(template).expect_err("unknown definition must fail at compile");
    let msg = err.to_string();
    assert!(
        msg.contains("NoSuchDefinition"),
        "error must name the missing definition: {msg}"
    );
}
