// use delver_pdf::layout::*;
// use delver_pdf::matcher::{extract_section_content, perform_matching};
// use delver_pdf::parse::{get_page_content, get_refs, PageContent, TextElement};
// use delver_pdf::search_index::PdfIndex;
// use lopdf::Document;

// pub mod setup;
// use crate::setup::{create_test_pdf_with_config, PdfConfig, Section};

// mod common;

// #[test]
// fn test_detect_headings() {
//     common::setup();

//     let pdf_path = "tests/heading_test.pdf";
//     let config = PdfConfig {
//         title: "Test Heading Detection".to_string(),
//         sections: vec![
//             Section {
//                 heading: "First Heading".to_string(),
//                 content: "This is content under the first heading.".to_string(),
//             },
//             Section {
//                 heading: "Second Heading".to_string(),
//                 content: "This is content under the second heading.".to_string(),
//             },
//             Section {
//                 heading: "Third Heading".to_string(),
//                 content: "This is content under the third heading.".to_string(),
//             },
//         ],
//         font_name: "Helvetica".to_string(),
//         title_font_size: 48.0,
//         heading_font_size: 24.0,
//         body_font_size: 12.0,
//         output_path: pdf_path.to_string(),
//     };

//     create_test_pdf_with_config(config.clone()).expect("Failed to create test PDF");

//     let doc = Document::load(pdf_path).unwrap();
//     let pages_map = get_page_content(&doc).unwrap();

//     let mock_match_context = MatchContext::default();
//     let index = PdfIndex::new(&pages_map, &mock_match_context);

//     // 1. Test Title Detection (Optional)
//     let title_text = "Test Heading Detection";
//     let title_candidates = index.find_text_matches(title_text, 0.95, None);
//     let found_title = title_candidates.iter().find(|(content, _score)| {
//         content.as_text().map_or(false, |text_elem| {
//             text_elem.text == title_text
//                 && (text_elem.font_size - config.title_font_size).abs() < 0.1
//         })
//     });
//     assert!(
//         found_title.is_some(),
//         "Failed to find the document title correctly with size {}",
//         config.title_font_size
//     );

//     // 2. Identify Heading Levels (Focus on the 24pt headings)
//     // We expect one dominant heading level from our config (24pt)
//     let heading_levels = index.identify_heading_levels(1); // Look for top 1 level for simplicity

//     assert_eq!(
//         heading_levels.len(),
//         1,
//         "Should identify one primary heading level (24pt)"
//     );

//     // Destructure the ((font_name, font_size), usage_count) tuple
//     let ((level_font_name, level_font_size), level_usage_count) = &heading_levels[0];

//     let expected_heading_font_name =
//         delver_pdf::fonts::canonicalize::canonicalize_font_name(&config.font_name);
//     assert_eq!(
//         *level_font_name, expected_heading_font_name,
//         "Identified heading font name mismatch"
//     );
//     assert!(
//         (*level_font_size - config.heading_font_size).abs() < 0.1,
//         "Identified heading font size mismatch. Expected {}, got {}",
//         config.heading_font_size,
//         level_font_size
//     );
//     assert_eq!(
//         *level_usage_count,
//         config.sections.len() as u32,
//         "Identified heading usage count mismatch"
//     );

//     // 3. Find Elements at the Identified Heading Level
//     let found_heading_elements = index.find_elements_at_heading_level(0);
//     assert_eq!(
//         found_heading_elements.len(),
//         config.sections.len(),
//         "Did not find all expected heading elements for level 0"
//     );

//     let expected_headings_texts: Vec<String> =
//         config.sections.iter().map(|s| s.heading.clone()).collect();

//     for expected_text in &expected_headings_texts {
//         let matching_found_heading = found_heading_elements.iter().find(|pc| {
//             pc.as_text().map_or(false, |te| {
//                 te.text == *expected_text && (te.font_size - config.heading_font_size).abs() < 0.1
//             })
//         });
//         assert!(
//             matching_found_heading.is_some(),
//             "Expected heading '{}' with size {} not found or has wrong font size",
//             expected_text,
//             config.heading_font_size
//         );
//     }

//     // TODO: Test content extraction between headings

//     common::cleanup_all();
// }
