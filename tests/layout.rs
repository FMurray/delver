use lopdf::Document;
use std::path::Path;

use crate::{extract_text_fragments, identify_headings};

#[test]
fn test_detect_headings() {
    // Create the test PDF
    assert!(create_test_pdf().is_ok());

    // Load the PDF document
    let doc = Document::load("tests/example.pdf").unwrap();

    // Extract text fragments
    let fragments = extract_text_fragments(&doc);

    // Identify headings
    let nodes = identify_headings(&fragments);

    // Check for expected headings
    let expected_headings = vec!["Hello World!", "Subheading 1", "Subheading 2"];
    let detected_headings: Vec<&str> = nodes
        .iter()
        .filter(|node| node.is_heading)
        .map(|node| node.text.as_str())
        .collect();

    assert_eq!(expected_headings, detected_headings);
}
