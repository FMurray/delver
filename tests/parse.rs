use std::path::PathBuf;

pub mod setup;
use crate::setup::create_test_pdf;
use delver::parse::{get_pdf_text, get_refs, load_pdf, TextElement};

#[test]
fn test_load_pdf() {
    // Create the test PDF
    create_test_pdf().expect("Failed to create test PDF");

    let test_pdf_path = PathBuf::from("tests/example.pdf");
    let result = load_pdf(&test_pdf_path);
    assert!(result.is_ok(), "Should successfully load the test PDF");
}

#[test]
fn test_get_pdf_text() {
    create_test_pdf().expect("Failed to create test PDF");
    let doc = load_pdf("tests/example.pdf").unwrap();

    let text_elements = get_pdf_text(&doc).unwrap();

    assert!(
        !text_elements.is_empty(),
        "Should extract text elements from PDF"
    );

    // Test specific content from setup.rs
    let expected_texts = vec![
        "Hello World!",
        "Subheading 1",
        "This is the first section text.",
        "Subheading 2",
        "This is the second section text.",
    ];

    let extracted_texts: Vec<&str> = text_elements
        .iter()
        .map(|element| element.text.as_str())
        .collect();

    for expected in expected_texts {
        assert!(
            extracted_texts.contains(&expected),
            "Missing expected text: {}",
            expected
        );
    }

    // Test font properties - note that we're not checking font_name as it might vary
    for element in text_elements {
        match element.text.as_str() {
            "Hello World!" => assert_eq!(element.font_size, 48.0),
            text if text.starts_with("Subheading") => assert_eq!(element.font_size, 24.0),
            _ => assert_eq!(element.font_size, 12.0),
        }
    }
}

#[test]
fn test_get_refs() {
    // Create a fresh PDF for this test
    create_test_pdf().expect("Failed to create test PDF");

    // Now load and test it
    let doc = match load_pdf("tests/example.pdf") {
        Ok(doc) => doc,
        Err(e) => panic!("Failed to load PDF: {}", e),
    };

    let context = get_refs(&doc).unwrap();

    // The test PDF created in setup.rs doesn't have any destinations
    assert!(context.destinations.is_empty());
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
        page_id: (1, 0),
        font_size: 12.0,
        font_name: Some(String::from("Courier")),
        position: (100.0, 200.0),
    }
}

#[test]
fn test_text_element_display() {
    let element = create_sample_text_element();
    let display_string = format!("{}", element);

    assert!(display_string.contains("Sample text"));
    assert!(display_string.contains("Page Number: 1"));
    assert!(display_string.contains("Font Size: 12.00"));
    assert!(display_string.contains("Courier"));
    assert!(display_string.contains("100.00, 200.00"));
}
