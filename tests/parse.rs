use std::path::PathBuf;

pub mod setup;
use crate::setup::{create_test_pdf, create_test_pdf_with_config, PdfConfig, Section};
use delver_pdf::geo::Rect;
use delver_pdf::parse::{get_page_content, get_refs, load_pdf, PageContent, TextElement};
use lopdf::Document;

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

    assert!(!pages_map.is_empty(), "Should extract content from PDF");

    // Test specific content from setup.rs
    let expected_texts = vec![
        "Hello World!",
        "Subheading 1",
        "This is the first section text.",
        "Subheading 2",
        "This is the second section text.",
    ];

    let extracted_texts: Vec<String> = pages_map
        .values()
        .flat_map(|contents| contents.text_elements())
        .map(|text_elem| text_elem.text)
        .collect();

    for expected in expected_texts {
        assert!(
            extracted_texts.iter().any(|t| t == expected),
            "Missing expected text: {}",
            expected
        );
    }

    // Test font properties
    for content in pages_map.values() {
        for text_elem in content.text_elements() {
            match text_elem.text.as_str() {
                "Hello World!" => assert_eq!(text_elem.font_size, 48.0),
                text if text.starts_with("Subheading") => assert_eq!(text_elem.font_size, 24.0),
                _ => assert_eq!(text_elem.font_size, 12.0),
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

#[test]
fn test_coordinate_transformations() {
    common::setup();

    // Create a PDF with specific text positions and transformations
    let config = PdfConfig {
        title: "Coordinate Test".to_string(),
        sections: vec![Section {
            heading: "Test Heading".to_string(),
            content: "This is a test of coordinate transformations.".to_string(),
        }],
        font_name: "Helvetica".to_string(),
        title_font_size: 48.0,
        heading_font_size: 24.0,
        body_font_size: 12.0,
        output_path: "tests/coordinate_test.pdf".to_string(),
    };

    create_test_pdf_with_config(config).expect("Failed to create test PDF");

    // Load the PDF and extract content
    let doc = Document::load("tests/coordinate_test.pdf").unwrap();
    let pages_map = get_page_content(&doc).unwrap();

    // Get all text elements
    let text_elements: Vec<TextElement> = pages_map
        .values()
        .flat_map(|page_contents| page_contents.iter_ordered())
        .filter_map(|content| {
            if let PageContent::Text(text_elem) = content {
                Some(text_elem)
            } else {
                None
            }
        })
        .collect();

    // Verify we have text elements
    assert!(
        !text_elements.is_empty(),
        "Should have extracted text elements"
    );

    // Test coordinate system properties
    for element in &text_elements {
        let (x0, y0, x1, y1) = element.bbox;

        // Verify coordinates are in top-left based system
        assert!(
            y0 <= y1,
            "y0 should be less than or equal to y1 (top-left system)"
        );

        // Verify x coordinates are ordered
        assert!(x0 <= x1, "x0 should be less than or equal to x1");

        // Verify coordinates are within page bounds (assuming standard US Letter size)
        assert!(
            x0 >= 0.0 && x1 <= 612.0,
            "x coordinates should be within page width"
        );
        assert!(
            y0 >= 0.0 && y1 <= 792.0,
            "y coordinates should be within page height"
        );

        // Verify text elements have reasonable dimensions
        let width = x1 - x0;
        let height = y1 - y0;
        assert!(width > 0.0, "Text element should have positive width");
        assert!(height > 0.0, "Text element should have positive height");

        // Verify font size is reflected in height
        let expected_min_height = element.font_size * 0.8; // Approximate minimum height
        assert!(
            height >= expected_min_height,
            "Text height should be at least 80% of font size. Got height={}, font_size={}",
            height,
            element.font_size
        );
    }

    // Test specific text positions
    let title = text_elements
        .iter()
        .find(|e| e.text == "Coordinate Test")
        .expect("Should find title text");

    // Title should be centered and at the top
    let (x0, y0, x1, y1) = title.bbox;
    let title_width = x1 - x0;
    let page_center = 612.0 / 2.0;
    let title_center = x0 + (title_width / 2.0);

    // Allow a wider margin for centering - some PDF renderers may interpret the positioning differently
    assert!(
        (title_center - page_center).abs() < 100.0,
        "Title should be roughly centered. Center offset: {}",
        (title_center - page_center).abs()
    );

    // Title should be near the top of the page
    assert!(
        y0 < 100.0,
        "Title should be near the top of the page. y0: {}",
        y0
    );

    // Test heading position
    let heading = text_elements
        .iter()
        .find(|e| e.text == "Test Heading")
        .expect("Should find heading text");

    // Heading should be below title
    assert!(
        heading.bbox.1 > title.bbox.3,
        "Heading should be below title. Heading y0: {}, Title y1: {}",
        heading.bbox.1,
        title.bbox.3
    );

    // Test content position
    let content = text_elements
        .iter()
        .find(|e| e.text == "This is a test of coordinate transformations.")
        .expect("Should find content text");

    // Content should be below heading
    assert!(
        content.bbox.1 > heading.bbox.3,
        "Content should be below heading. Content y0: {}, Heading y1: {}",
        content.bbox.1,
        heading.bbox.3
    );

    common::cleanup_all();
}
