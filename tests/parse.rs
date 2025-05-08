use std::path::PathBuf;

pub mod setup;
use crate::setup::create_test_pdf;
use delver_pdf::parse::{get_page_content, get_refs, load_pdf, TextElement, PageContent};

mod common;

#[test]
fn test_load_pdf() {
    common::setup();

    // Create the test PDF
    create_test_pdf().expect("Failed to create test PDF");

    let test_pdf_path = PathBuf::from("tests/example.pdf");
    let result = load_pdf(&test_pdf_path);
    assert!(result.is_ok(), "Should successfully load the test PDF");
    common::cleanup_all();
}

#[test]
fn test_get_pdf_text() {
    common::setup();

    // Create new test PDF
    create_test_pdf().expect("Failed to create test PDF");

    // Run test
    let doc = load_pdf("tests/example.pdf").unwrap();
    let pages_map = get_page_content(&doc).unwrap();

    assert!(
        !pages_map.is_empty(),
        "Should extract content from PDF"
    );

    // Test specific content from setup.rs
    let expected_texts = vec![
        "Hello World!",
        "Subheading 1",
        "This is the first section text.",
        "Subheading 2",
        "This is the second section text.",
    ];

    let extracted_texts: Vec<&str> = pages_map
        .values()
        .flatten()
        .filter_map(|content| {
            if let PageContent::Text(text_elem) = content {
                Some(text_elem.text.as_str())
            } else {
                None
            }
        })
        .collect();

    for expected in expected_texts {
        assert!(
            extracted_texts.contains(&expected),
            "Missing expected text: {}",
            expected
        );
    }

    // Test font properties
    for content in pages_map.values().flatten() {
        if let PageContent::Text(element) = content {
            match element.text.as_str() {
                "Hello World!" => assert_eq!(element.font_size, 48.0),
                text if text.starts_with("Subheading") => assert_eq!(element.font_size, 24.0),
                _ => assert_eq!(element.font_size, 12.0),
            }
        }
    }

    // Clean up after test
    common::cleanup_all();
}

#[test]
fn test_get_refs() {
    common::setup();
    create_test_pdf().expect("Failed to create test PDF");

    // Now load and test it
    let doc = match load_pdf("tests/example.pdf") {
        Ok(doc) => doc,
        Err(e) => panic!("Failed to load PDF: {}", e),
    };

    let context = get_refs(&doc).unwrap();

    // The test PDF created in setup.rs doesn't have any destinations
    assert!(context.destinations.is_empty());

    common::cleanup_all();
}

#[test]
fn test_load_pdf_invalid_path() {
    let invalid_path = PathBuf::from("nonexistent.pdf");
    let result = load_pdf(&invalid_path);
    assert!(result.is_err(), "Should fail when loading non-existent PDF");
}

// Helper function to create a sample TextElement for testing
fn create_sample_text_element() -> TextElement {
    TextElement {
        text: String::from("Sample text"),
        page_number: 1,
        font_size: 12.0,
        font_name: Some(String::from("Courier")),
        bbox: (100.0, 200.0, 150.0, 210.0),
        id: uuid::Uuid::new_v4(),
        operators: Vec::new(),
    }
}

#[test]
fn test_text_element_display() {
    let element = create_sample_text_element();
    let display_string = format!("{}", element);

    assert!(display_string.contains("Sample text"));
    assert!(display_string.contains("12pt"));
    assert!(display_string.contains("Courier"));
    assert!(display_string.contains("100.0, 200.0, 150.0, 210.0"));
}
