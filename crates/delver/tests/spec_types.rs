//! Parse- and template-level tests for the Stage B slice-2 element kinds
//! (docs/DECISIONS.md D-016): ANNOTATION, PATH, FIGURE (+ ref edges), BLOB,
//! and DOCUMENT metadata, plus the `Annotation(...)` / `Figure(...)` template
//! selectors. No database required; the store round-trip lives in
//! delver-store/tests/roundtrip.rs.
//!
//! Per D-009 the fixture PDF is generated in-test via lopdf (builder
//! deliberately duplicated from the store tests — no shared test-util crate).

use delver_core::parse::{parse_document, AuxKind, ParsedDocument};
use delver_core::process_pdf;
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use serde_json::Value;

const HEADING_1: &str = "Management Discussion and Analysis";
const BODY_1A: &str = "Revenue grew steadily across all reporting segments during the fiscal year.";
const HEADING_2: &str = "Quantitative and Qualitative Disclosures";
const BODY_2A: &str =
    "Interest rate exposure remains hedged through a portfolio of fixed rate swaps.";
const CAPTION: &str = "Figure 1: Test diagram";

fn push_text_ops(ops: &mut Vec<Operation>, text: &str, size: f32, x: f32, y: f32) {
    ops.push(Operation::new("BT", vec![]));
    ops.push(Operation::new("Tf", vec!["F1".into(), size.into()]));
    ops.push(Operation::new("Td", vec![x.into(), y.into()]));
    ops.push(Operation::new("Tj", vec![Object::string_literal(text)]));
    ops.push(Operation::new("ET", vec![]));
}

/// Two-page PDF with one of each slice-2 artifact: a captioned image on
/// page 1 (figure grouping), a Link annotation on page 1, one stroked
/// rectangle path on page 1, a document-level embedded file, and an Info
/// dictionary.
fn build_rich_pdf() -> Vec<u8> {
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
            "Width" => 4,
            "Height" => 4,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![128u8; 16],
    ));
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
        "XObject" => dictionary! { "Im1" => image_id },
    });

    let mut p1_ops = Vec::new();
    push_text_ops(&mut p1_ops, HEADING_1, 24.0, 72.0, 700.0);
    push_text_ops(&mut p1_ops, BODY_1A, 11.0, 72.0, 660.0);
    // Image at pdf (100,450)-(200,550); caption just below it.
    p1_ops.push(Operation::new("q", vec![]));
    p1_ops.push(Operation::new(
        "cm",
        vec![
            1.into(),
            0.into(),
            0.into(),
            1.into(),
            100.into(),
            450.into(),
        ],
    ));
    p1_ops.push(Operation::new("Do", vec!["Im1".into()]));
    p1_ops.push(Operation::new("Q", vec![]));
    push_text_ops(&mut p1_ops, CAPTION, 10.0, 100.0, 435.0);
    // One painted (stroked) rectangle path.
    p1_ops.push(Operation::new(
        "re",
        vec![300.into(), 100.into(), 150.into(), 40.into()],
    ));
    p1_ops.push(Operation::new("S", vec![]));

    let mut p2_ops = Vec::new();
    push_text_ops(&mut p2_ops, HEADING_2, 24.0, 72.0, 700.0);
    push_text_ops(&mut p2_ops, BODY_2A, 11.0, 72.0, 660.0);

    let annot_id = doc.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Link",
        "Rect" => vec![72.into(), 750.into(), 200.into(), 770.into()],
        "Contents" => Object::string_literal("See appendix"),
        "A" => dictionary! {
            "Type" => "Action",
            "S" => "URI",
            "URI" => Object::string_literal("https://example.com/spec"),
        },
    });

    let mut page_ids = Vec::new();
    for (ops, annots) in [(p1_ops, Some(annot_id)), (p2_ops, None)] {
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            Content { operations: ops }
                .encode()
                .expect("encode content"),
        ));
        let mut page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        };
        if let Some(annot_id) = annots {
            page_dict.set("Annots", vec![annot_id.into()]);
        }
        page_ids.push(doc.add_object(page_dict));
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

    let ef_stream = doc.add_object(Stream::new(
        dictionary! { "Type" => "EmbeddedFile" },
        b"alpha,beta\n1,2\n".to_vec(),
    ));
    let filespec = doc.add_object(dictionary! {
        "Type" => "Filespec",
        "F" => Object::string_literal("report.csv"),
        "UF" => Object::string_literal("report.csv"),
        "EF" => dictionary! { "F" => ef_stream },
    });
    let embedded_files = doc.add_object(dictionary! {
        "Names" => vec![Object::string_literal("report.csv"), filespec.into()],
    });

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "Names" => dictionary! { "EmbeddedFiles" => embedded_files },
    });
    doc.trailer.set("Root", catalog_id);

    let info_id = doc.add_object(dictionary! {
        "Title" => Object::string_literal("Spec Types Fixture"),
        "Author" => Object::string_literal("Delver Tests"),
        "Subject" => Object::string_literal("Round-trip fixture"),
        "CreationDate" => Object::string_literal("D:20260611000000Z"),
    });
    doc.trailer.set("Info", info_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize pdf to memory");
    bytes
}

fn parse_fixture() -> ParsedDocument {
    let bytes = build_rich_pdf();
    let doc = Document::load_mem(&bytes).expect("load fixture pdf");
    parse_document(&doc).expect("parse fixture pdf")
}

fn aux_of_kind(parsed: &ParsedDocument, kind: AuxKind) -> Vec<delver_core::parse::AuxElement> {
    parsed
        .pages
        .values()
        .flat_map(|page| page.aux_store.iter())
        .filter(|aux| aux.kind == kind)
        .cloned()
        .collect()
}

// ───────────────────────────── parse level ─────────────────────────────

#[test]
fn parse_extracts_annotation() {
    let parsed = parse_fixture();
    let annots = aux_of_kind(&parsed, AuxKind::Annotation);
    assert_eq!(annots.len(), 1, "expected exactly one annotation");
    let annot = &annots[0];
    assert_eq!(annot.page_number, 1);
    assert_eq!(annot.text.as_deref(), Some("See appendix"));
    assert_eq!(annot.metadata["subtype"], serde_json::json!("Link"));
    assert_eq!(
        annot.metadata["uri"],
        serde_json::json!("https://example.com/spec")
    );
    // Rect [72 750 200 770] flipped to top-left coordinates on a 792pt page.
    assert!(
        (annot.bbox.x0 - 72.0).abs() < 1e-3,
        "bbox: {:?}",
        annot.bbox
    );
    assert!(
        (annot.bbox.y0 - 22.0).abs() < 1e-3,
        "bbox: {:?}",
        annot.bbox
    );
    assert!(
        (annot.bbox.x1 - 200.0).abs() < 1e-3,
        "bbox: {:?}",
        annot.bbox
    );
    assert!(
        (annot.bbox.y1 - 42.0).abs() < 1e-3,
        "bbox: {:?}",
        annot.bbox
    );
}

#[test]
fn parse_extracts_painted_path() {
    let parsed = parse_fixture();
    let paths = aux_of_kind(&parsed, AuxKind::Path);
    assert_eq!(paths.len(), 1, "expected exactly one painted path");
    let path = &paths[0];
    assert_eq!(path.page_number, 1);
    assert_eq!(path.metadata["op_count"], serde_json::json!(1));
    assert_eq!(path.metadata["stroke"], serde_json::json!(true));
    assert_eq!(path.metadata["fill"], serde_json::json!(false));
    assert_eq!(path.metadata["point_count"], serde_json::json!(4));
    assert_eq!(path.metadata["points"].as_array().map(Vec::len), Some(4));
    // re [300 100 150 40] flipped: (300, 652)-(450, 692).
    assert!((path.bbox.x0 - 300.0).abs() < 1e-3, "bbox: {:?}", path.bbox);
    assert!((path.bbox.y0 - 652.0).abs() < 1e-3, "bbox: {:?}", path.bbox);
    assert!((path.bbox.x1 - 450.0).abs() < 1e-3, "bbox: {:?}", path.bbox);
    assert!((path.bbox.y1 - 692.0).abs() < 1e-3, "bbox: {:?}", path.bbox);
}

#[test]
fn parse_groups_figure_with_caption_and_edges() {
    let parsed = parse_fixture();
    let figures = aux_of_kind(&parsed, AuxKind::Figure);
    assert_eq!(figures.len(), 1, "expected exactly one figure grouping");
    let figure = &figures[0];
    assert_eq!(figure.page_number, 1);
    assert_eq!(figure.metadata["caption"], serde_json::json!(CAPTION));
    assert_eq!(
        figure.metadata["caption_position"],
        serde_json::json!("below")
    );

    let page1 = &parsed.pages[&1];
    let image_id = page1.image_store.id[0];
    let caption_elem = page1
        .text_store
        .iter()
        .find(|t| t.text == CAPTION)
        .expect("caption text element");
    assert_eq!(
        figure.metadata["image_id"],
        serde_json::json!(image_id),
        "figure metadata must reference the grouped image"
    );

    // Union bbox covers both the image and the caption.
    let image_bbox = page1.image_store.bbox[0];
    assert!(figure.bbox.x0 <= image_bbox.x0 && figure.bbox.x1 >= image_bbox.x1);
    assert!(figure.bbox.y0 <= image_bbox.y0);
    assert!(figure.bbox.y1 >= caption_elem.bbox.3 - 1e-3);

    // Typed edges: figure→image "contains", figure→caption "caption-of".
    assert_eq!(parsed.refs.len(), 2, "refs: {:?}", parsed.refs);
    let contains = parsed
        .refs
        .iter()
        .find(|r| r.kind == "contains")
        .expect("contains edge");
    assert_eq!(contains.from, figure.id);
    assert_eq!(contains.to, image_id);
    let caption_of = parsed
        .refs
        .iter()
        .find(|r| r.kind == "caption-of")
        .expect("caption-of edge");
    assert_eq!(caption_of.from, figure.id);
    assert_eq!(caption_of.to, caption_elem.id);
}

#[test]
fn parse_extracts_embedded_file_blob() {
    let parsed = parse_fixture();
    let blobs = aux_of_kind(&parsed, AuxKind::Blob);
    assert_eq!(blobs.len(), 1, "expected exactly one embedded-file blob");
    let blob_elem = &blobs[0];
    assert_eq!(
        blob_elem.page_number, 0,
        "document-level blobs live on synthetic page 0"
    );
    let payload = blob_elem.blob.as_ref().expect("blob payload");
    assert_eq!(payload.data, b"alpha,beta\n1,2\n");
    assert_eq!(payload.filename.as_deref(), Some("report.csv"));
    assert_eq!(
        blob_elem.metadata["filename"],
        serde_json::json!("report.csv")
    );
    // Real page count is unaffected by the synthetic page 0.
    assert_eq!(parsed.page_count(), 2);
}

#[test]
fn parse_reads_info_dict_document_metadata() {
    let parsed = parse_fixture();
    assert_eq!(
        parsed.metadata["title"],
        serde_json::json!("Spec Types Fixture")
    );
    assert_eq!(parsed.metadata["author"], serde_json::json!("Delver Tests"));
    assert_eq!(
        parsed.metadata["subject"],
        serde_json::json!("Round-trip fixture")
    );
    assert_eq!(
        parsed.metadata["creation_date"],
        serde_json::json!("D:20260611000000Z")
    );
}

#[test]
fn image_without_matching_caption_stays_standalone() {
    // Same fixture but the caption text does not match the caption regex:
    // no figure element, no edges (figures are additive, never destructive).
    let mut doc = Document::load_mem(&build_rich_pdf()).expect("load fixture");
    let parsed_with = parse_document(&doc).expect("parse");
    assert_eq!(aux_of_kind(&parsed_with, AuxKind::Figure).len(), 1);
    drop(parsed_with);

    // Rebuild with a non-caption label.
    let bytes = {
        let mut ops_pdf = build_rich_pdf();
        // Cheap textual swap inside the uncompressed content stream: the
        // caption prefix "Figure 1:" becomes "Legend 1:" (same length).
        let needle = b"Figure 1: Test diagram";
        let replacement = b"Legend 1: Test diagram";
        let pos = ops_pdf
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("caption bytes present in uncompressed stream");
        ops_pdf[pos..pos + needle.len()].copy_from_slice(replacement);
        ops_pdf
    };
    doc = Document::load_mem(&bytes).expect("load modified fixture");
    let parsed_without = parse_document(&doc).expect("parse modified");
    assert_eq!(
        aux_of_kind(&parsed_without, AuxKind::Figure).len(),
        0,
        "non-caption text must not produce a figure"
    );
    assert!(parsed_without.refs.is_empty());
    // The image itself is still there, standalone.
    assert_eq!(parsed_without.pages[&1].image_store.id.len(), 1);
}

// ─────────────────────────── template level ───────────────────────────

fn run_template(template: &str) -> Vec<Value> {
    let bytes = build_rich_pdf();
    let (json, _blocks, _doc) =
        process_pdf(&bytes, template, None, None).expect("process_pdf with selectors");
    serde_json::from_str::<Vec<Value>>(&json).expect("outputs JSON array")
}

#[test]
fn annotation_and_figure_selectors_top_level() {
    let outputs = run_template(
        r#"Annotation(as="links")
Figure(as="figures")
"#,
    );

    let annotations: Vec<&Value> = outputs
        .iter()
        .filter(|o| o["type"] == "Annotation")
        .collect();
    assert_eq!(annotations.len(), 1, "outputs: {outputs:?}");
    let annot = annotations[0];
    assert_eq!(annot["text"], "See appendix");
    assert_eq!(annot["page_number"], 1);
    assert_eq!(annot["metadata"]["subtype"], "Link");
    assert_eq!(annot["metadata"]["uri"], "https://example.com/spec");
    assert_eq!(annot["metadata"]["name"], "links");

    let figures: Vec<&Value> = outputs.iter().filter(|o| o["type"] == "Figure").collect();
    assert_eq!(figures.len(), 1, "outputs: {outputs:?}");
    let figure = figures[0];
    assert_eq!(figure["caption"], CAPTION);
    assert_eq!(figure["page_number"], 1);
    assert_eq!(figure["metadata"]["name"], "figures");
    assert!(figure["image_id"].is_string(), "figure: {figure:?}");
}

#[test]
fn annotation_and_figure_selectors_inside_section() {
    let outputs = run_template(&format!(
        r#"Section(
  threshold=0.8,
  match="{HEADING_1}",
  end_match="{HEADING_2}",
  as="S1"
) {{
  TextChunk(chunkSize=200, chunkOverlap=20)
  Annotation(as="sec-annots")
  Figure(as="sec-figs")
}}
"#
    ));

    let annotations: Vec<&Value> = outputs
        .iter()
        .filter(|o| o["type"] == "Annotation")
        .collect();
    assert_eq!(
        annotations.len(),
        1,
        "annotation inside section bounds expected: {outputs:?}"
    );
    assert_eq!(annotations[0]["metadata"]["section"], "S1");
    assert_eq!(annotations[0]["metadata"]["name"], "sec-annots");

    let figures: Vec<&Value> = outputs.iter().filter(|o| o["type"] == "Figure").collect();
    assert_eq!(figures.len(), 1, "figure inside section bounds expected");
    assert_eq!(figures[0]["caption"], CAPTION);
    assert_eq!(figures[0]["metadata"]["section"], "S1");

    // The section still produced its text chunks alongside.
    assert!(
        outputs.iter().any(|o| o["type"] == "Text"),
        "section text chunks missing: {outputs:?}"
    );
}

#[test]
fn figure_selector_respects_section_boundaries() {
    // Section starts at the page-2 heading; the page-1 figure/annotation are
    // outside it, so the selectors must match nothing.
    let outputs = run_template(&format!(
        r#"Section(
  threshold=0.8,
  match="{HEADING_2}",
  as="S2"
) {{
  Annotation(as="none-a")
  Figure(as="none-f")
}}
"#
    ));

    assert!(
        outputs
            .iter()
            .all(|o| o["type"] != "Figure" && o["type"] != "Annotation"),
        "page-1 figure/annotation leaked into the page-2 section: {outputs:?}"
    );
}
