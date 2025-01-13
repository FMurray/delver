use lopdf::Document;

pub mod setup;
use crate::setup::{create_test_pdf_with_config, PdfConfig, Section};
use delver::layout::*;
use delver::parse::{get_pdf_text, get_refs, TextElement};

mod common;

#[test]
fn test_detect_headings() {
    common::setup();

    // Create a PDF with specific headings
    let config = PdfConfig {
        title: "Test Heading Detection".to_string(),
        sections: vec![
            Section {
                heading: "First Heading".to_string(),
                content: "This is content under the first heading.".to_string(),
            },
            Section {
                heading: "Second Heading".to_string(),
                content: "This is content under the second heading.".to_string(),
            },
            Section {
                heading: "Third Heading".to_string(),
                content: "This is content under the third heading.".to_string(),
            },
        ],
        font_name: "Helvetica".to_string(),
        title_font_size: 48.0,
        heading_font_size: 24.0,
        body_font_size: 12.0,
        output_path: "tests/heading_test.pdf".to_string(),
    };

    create_test_pdf_with_config(config).expect("Failed to create test PDF");

    // Load the PDF and extract text elements
    let doc = Document::load("tests/heading_test.pdf").unwrap();
    let text_elements = get_pdf_text(&doc).unwrap().clone();

    // Create a match context
    let context = get_refs(&doc).unwrap();

    // Test finding the title
    let title_matches = perform_matching(&text_elements, "Test Heading Detection", 0.9);
    let title = select_best_match(title_matches, &context);
    assert!(title.is_some());
    assert_eq!(title.unwrap().font_size, 48.0);

    // Test finding each heading sequentially
    let expected_headings = vec!["First Heading", "Second Heading", "Third Heading"];

    let mut last_match = None;
    for expected_heading in expected_headings {
        let matches = perform_matching(&text_elements, expected_heading, 0.9);
        println!("Found {} matches for '{}'", matches.len(), expected_heading);

        for m in &matches {
            println!(
                "Match: '{}' at y={}, font={}",
                m.text, m.position.1, m.font_size
            );
        }

        let heading = select_best_match(matches, &context);

        if heading.is_none() {
            println!(
                "No heading found after {:?}",
                last_match.map(|m: TextElement| m.text)
            );
        }

        assert!(
            heading.is_some(),
            "Failed to find heading: {}",
            expected_heading
        );
        let heading = heading.unwrap();

        // Verify heading properties
        assert_eq!(heading.text, expected_heading);
        assert_eq!(heading.font_size, 24.0);
        assert!(
            heading.position.0 <= 100.0,
            "Heading should be left-aligned"
        );

        last_match = Some(heading);
    }

    // Test extracting content between headings
    if let (Some(first_heading), Some(second_heading)) = (
        perform_matching(&text_elements, "First Heading", 0.9)
            .first()
            .cloned(),
        perform_matching(&text_elements, "Second Heading", 0.9)
            .first()
            .cloned(),
    ) {
        let content =
            extract_section_content(&text_elements, &first_heading, Some(&second_heading));

        // Should find the content text
        assert!(content
            .iter()
            .any(|e| e.text == "This is content under the first heading."));

        // Should have correct font size
        for element in content {
            assert_eq!(element.font_size, 12.0, "Content should use body font size");
        }
    }

    common::cleanup_all();
}
