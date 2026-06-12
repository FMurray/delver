//! LIVE end-to-end test against a real Databricks workspace.
//!
//! SKIPPED by default: requires `DELVER_DBX_LIVE=1` plus the full engine
//! configuration. Never run in CI; it uploads a tiny synthetic PDF (built
//! in-test, no real data) to the configured UC volume, runs
//! ai_parse_document on the configured warehouse, and deletes the upload.

use delver_parse_dbx::{map_ai_parse_response, DbxConfig, DbxParseClient};

const REQUIRED: &[&str] = &[
    "DELVER_DBX_LIVE=1",
    "DATABRICKS_HOST + DATABRICKS_TOKEN (or DELVER_DBX_PROFILE)",
    "DELVER_DBX_WAREHOUSE_ID",
    "DELVER_DBX_VOLUME",
];

#[test]
fn live_ai_parse_document_end_to_end() {
    if std::env::var("DELVER_DBX_LIVE").as_deref() != Ok("1") {
        eprintln!(
            "SKIP live_ai_parse_document_end_to_end: set {} to run this live test",
            REQUIRED.join(", ")
        );
        return;
    }
    let config = match DbxConfig::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!(
                "SKIP live_ai_parse_document_end_to_end: DELVER_DBX_LIVE=1 but \
                 configuration is incomplete ({e}); required: {}",
                REQUIRED.join(", ")
            );
            return;
        }
    };

    let pdf = synthetic_pdf();
    let client = DbxParseClient::new(config);
    let response = client
        .parse_document_bytes(&pdf, "delver-live-test.pdf")
        .expect("live ai_parse_document call");
    let parsed = map_ai_parse_response(&response).expect("live response maps");
    assert!(
        parsed.page_count() >= 1,
        "live parse should see at least one page"
    );
    let texts: usize = parsed
        .pages
        .values()
        .map(|p| p.text_store.iter().count())
        .sum();
    assert!(texts > 0, "live parse should extract some text");
}

/// One-page text PDF built in memory (D-009: synthetic fixtures only).
fn synthetic_pdf() -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let ops = vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec!["F1".into(), 24.into()]),
        Operation::new("Td", vec![72.into(), 700.into()]),
        Operation::new(
            "Tj",
            vec![Object::string_literal("Delver live ai_parse_document check")],
        ),
        Operation::new("ET", vec![]),
    ];
    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations: ops }.encode().expect("encode"),
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
    doc.save_to(&mut bytes).expect("serialize");
    bytes
}
