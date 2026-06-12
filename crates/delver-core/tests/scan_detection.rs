//! Scanned-PDF detection over synthetic lopdf documents (slice P1; D-009:
//! fixtures are generated in-test, no binary files, no network).
//!
//! Covers the three canonical page shapes — full-page image (raw scan),
//! normal text (born digital), invisible text over an image (scan with OCR
//! layer) — the document aggregate, and the producer-fingerprint escalation.

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};

use delver_core::parse::parse_document;

const BODY: &str =
    "Revenue grew steadily across all reporting segments during the fiscal year under review.";

/// What to put on each synthetic page.
enum PageSpec {
    /// One visible 12pt text paragraph (well over the sparse-text floor).
    Text,
    /// One CCITT-encoded image XObject drawn over the whole page.
    FullImage,
    /// The full-page image plus the paragraph drawn with `3 Tr`
    /// (invisible) — the Acrobat/ocrmypdf "searchable image" shape.
    ImageWithInvisibleText,
    /// Nothing at all (used by the producer-escalation test).
    Empty,
}

fn build_pdf(specs: &[PageSpec], producer: Option<&str>) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    // Image stream bytes are never decoded by the parser; junk is fine.
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

    let draw_image = |ops: &mut Vec<Operation>| {
        ops.push(Operation::new("q", vec![]));
        ops.push(Operation::new(
            "cm",
            vec![612.into(), 0.into(), 0.into(), 792.into(), 0.into(), 0.into()],
        ));
        ops.push(Operation::new("Do", vec!["Im0".into()]));
        ops.push(Operation::new("Q", vec![]));
    };
    let draw_text = |ops: &mut Vec<Operation>, invisible: bool| {
        ops.push(Operation::new("BT", vec![]));
        ops.push(Operation::new("Tf", vec!["F1".into(), 12.into()]));
        if invisible {
            ops.push(Operation::new("Tr", vec![3.into()]));
        }
        ops.push(Operation::new("Td", vec![72.into(), 700.into()]));
        ops.push(Operation::new("Tj", vec![Object::string_literal(BODY)]));
        ops.push(Operation::new("ET", vec![]));
    };

    let mut page_ids = Vec::new();
    for spec in specs {
        let mut ops = Vec::new();
        match spec {
            PageSpec::Text => draw_text(&mut ops, false),
            PageSpec::FullImage => draw_image(&mut ops),
            PageSpec::ImageWithInvisibleText => {
                draw_image(&mut ops);
                draw_text(&mut ops, true);
            }
            PageSpec::Empty => {}
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

    if let Some(producer) = producer {
        let info_id = doc.add_object(dictionary! {
            "Producer" => Object::string_literal(producer),
        });
        doc.trailer.set("Info", info_id);
    }

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize pdf");
    bytes
}

fn scan_block(specs: &[PageSpec], producer: Option<&str>) -> serde_json::Value {
    let bytes = build_pdf(specs, producer);
    let doc = Document::load_mem(&bytes).expect("load synthetic pdf");
    let parsed = parse_document(&doc).expect("parse synthetic pdf");
    parsed
        .metadata
        .get("scan")
        .cloned()
        .expect("parse_document must attach a scan block")
}

#[test]
fn full_page_image_page_is_scanned_no_text() {
    let scan = scan_block(&[PageSpec::FullImage], None);
    assert_eq!(scan["class"], "scanned_no_text");
    assert_eq!(scan["scanned_page_ratio"], 1.0);
    let page = &scan["pages"][0];
    assert_eq!(page["class"], "scanned_no_text");
    assert!(
        page["image_coverage"].as_f64().unwrap() > 0.95,
        "full-page image must cover the page: {page}"
    );
    assert_eq!(page["text_chars"], 0);
    assert_eq!(page["image_filters"], serde_json::json!(["CCITTFaxDecode"]));
}

#[test]
fn text_page_is_native() {
    let scan = scan_block(&[PageSpec::Text], None);
    assert_eq!(scan["class"], "native");
    assert_eq!(scan["scanned_page_ratio"], 0.0);
    let page = &scan["pages"][0];
    assert_eq!(page["class"], "native");
    assert_eq!(page["image_coverage"], 0.0);
    assert_eq!(page["text_chars"], BODY.chars().count() as i64);
    assert_eq!(page["invisible_text_ratio"], 0.0);
}

#[test]
fn invisible_text_over_image_is_scanned_ocr() {
    let scan = scan_block(&[PageSpec::ImageWithInvisibleText], None);
    assert_eq!(scan["class"], "scanned_ocr");
    let page = &scan["pages"][0];
    assert_eq!(page["class"], "scanned_ocr");
    assert_eq!(
        page["invisible_text_ratio"], 1.0,
        "every char is drawn with 3 Tr: {page}"
    );
    assert_eq!(page["text_chars"], BODY.chars().count() as i64);
}

#[test]
fn document_aggregate_over_disagreeing_pages_is_mixed() {
    let scan = scan_block(
        &[
            PageSpec::FullImage,
            PageSpec::ImageWithInvisibleText,
            PageSpec::Text,
        ],
        None,
    );
    // 2 of 3 pages scanned: between the native and scanned verdicts.
    assert_eq!(scan["class"], "mixed");
    let ratio = scan["scanned_page_ratio"].as_f64().unwrap();
    assert!((ratio - 2.0 / 3.0).abs() < 1e-6, "ratio: {ratio}");
    let classes: Vec<&str> = scan["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["class"].as_str().unwrap())
        .collect();
    assert_eq!(classes, ["scanned_no_text", "scanned_ocr", "native"]);
}

#[test]
fn all_scanned_document_aggregates_to_scanned_ocr() {
    let scan = scan_block(
        &[
            PageSpec::ImageWithInvisibleText,
            PageSpec::ImageWithInvisibleText,
            PageSpec::FullImage,
        ],
        None,
    );
    assert_eq!(scan["class"], "scanned_ocr", "OCR flavor dominates: {scan}");
    assert_eq!(scan["scanned_page_ratio"], 1.0);
    assert_eq!(scan["confidence"], 1.0);
}

#[test]
fn zero_text_doc_with_scanner_producer_escalates() {
    // Pages the walk cannot see into (no text, no image XObjects — stand-in
    // for inline-image scans) escalate on the producer fingerprint alone.
    let scan = scan_block(
        &[PageSpec::Empty, PageSpec::Empty],
        Some("ABBYY FineReader 15"),
    );
    assert_eq!(scan["class"], "scanned_no_text");
    assert_eq!(scan["confidence"], 0.5, "escalation is low-confidence");
    assert_eq!(scan["producer"], "ABBYY FineReader 15");
    assert_eq!(scan["producer_hint"], "abbyy");

    // The same empty document with a word-processor producer stays native.
    let scan = scan_block(&[PageSpec::Empty, PageSpec::Empty], Some("Microsoft Word"));
    assert_eq!(scan["class"], "native");
}
