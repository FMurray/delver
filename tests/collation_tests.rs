use delver_pdf::dom::{Element, ElementType, MatchConfig, MatchType, Value};
use delver_pdf::layout::MatchContext;
use delver_pdf::matcher::{
    align_template_with_content, MatchedContent, SectionBoundaries, TemplateContentMatch,
};
use delver_pdf::parse::{ImageElement, PageContent, TextElement};
use delver_pdf::search_index::PdfIndex;

use lopdf::Object;
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

// Reusing and adapting helper functions from search_index_tests.rs
// Helper function to create mock TextElement
fn create_mock_text_element(
    id: Uuid,
    text: &str,
    font_name: &str,
    font_size: f32,
    page: u32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> PageContent {
    PageContent::Text(TextElement {
        id,
        text: text.to_string(),
        font_name: Some(font_name.to_string()),
        font_size,
        bbox: (x, y, x + width, y + height),
        page_number: page,
        operators: Vec::new(), // Add missing field
    })
}

// Helper function to create mock ImageElement
fn create_mock_image_element(
    id: Uuid,
    page: u32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> PageContent {
    PageContent::Image(ImageElement {
        id,
        page_number: page,
        bbox: delver_pdf::geo::Rect {
            x0: x,
            y0: y,
            x1: x + width,
            y1: y + height,
        }, // Use Rect
        image_object: Object::Null, // Placeholder for tests
    })
}

// Helper to create a template Element with optional end_match and children
fn create_template_element(
    name: &str,
    element_type: ElementType,
    match_pattern: Option<&str>,
    end_match_pattern: Option<&str>,
    children: Vec<Element>,
) -> Element {
    let mut attributes = HashMap::new();
    if let Some(pattern) = match_pattern {
        attributes.insert("match".to_string(), Value::String(pattern.to_string()));
    }
    if let Some(end_pattern) = end_match_pattern {
        attributes.insert(
            "end_match".to_string(),
            Value::String(end_pattern.to_string()),
        );
    }
    // Add other attributes as needed for different element types later
    Element {
        name: name.to_string(),
        element_type,
        attributes,
        children,
        parent: None,
        prev_sibling: None,
        next_sibling: None,
    }
}

#[cfg(test)]
mod collation_flow_tests {
    use super::*;

    #[test]
    fn test_simple_section_match_with_explicit_end() {
        // 1. Template Definition
        let section_template = create_template_element(
            "Introduction",
            ElementType::Section,
            Some("Introduction Heading"),
            Some("Next Section Starts Here"),
            Vec::new(),
        );
        let template_elements = vec![section_template];

        // 2. Document Content (PdfIndex Setup)
        let mut page_map: BTreeMap<u32, Vec<PageContent>> = BTreeMap::new();
        let heading_id = Uuid::new_v4();
        let para1_id = Uuid::new_v4();
        let image_id = Uuid::new_v4();
        let para2_id = Uuid::new_v4();
        let next_section_heading_id = Uuid::new_v4(); // Renamed for clarity

        page_map.insert(
            1,
            vec![
                create_mock_text_element(
                    heading_id,
                    "Introduction Heading",
                    "Arial",
                    16.0,
                    1,
                    50.0,
                    700.0,
                    200.0,
                    16.0,
                ),
                create_mock_text_element(
                    para1_id,
                    "This is the first paragraph.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    680.0,
                    300.0,
                    12.0,
                ),
                create_mock_image_element(image_id, 1, 50.0, 600.0, 100.0, 80.0),
                create_mock_text_element(
                    para2_id,
                    "This is the second paragraph.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    580.0,
                    300.0,
                    12.0,
                ),
                create_mock_text_element(
                    next_section_heading_id,
                    "Next Section Starts Here",
                    "Arial",
                    16.0,
                    1,
                    50.0,
                    550.0,
                    250.0,
                    16.0,
                ),
            ],
        );

        let mock_match_context = MatchContext::default();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // 3. Call align_template_with_content
        let results_option = align_template_with_content(&template_elements, &index, None, None);

        // 4. Assertions
        assert!(
            results_option.is_some(),
            "align_template_with_content should return Some"
        );
        let results = results_option.unwrap();
        assert_eq!(
            results.len(),
            1,
            "Should find one match for the section template"
        );

        let section_match = &results[0];
        assert_eq!(section_match.template_element.name, "Introduction");

        assert!(
            section_match.section_boundaries.is_some(),
            "Section boundaries should be set"
        );
        let boundaries = section_match.section_boundaries.as_ref().unwrap();

        // Assert Start Marker
        match boundaries.start_marker {
            PageContent::Text(text_elem) => {
                assert_eq!(text_elem.id, heading_id, "Start marker ID mismatch");
                assert_eq!(
                    text_elem.text, "Introduction Heading",
                    "Start marker text mismatch"
                );
            }
            _ => panic!("Start marker should be a TextElement"),
        }

        // Assert End Marker (Now we expect it to be next_section_heading_id)
        assert!(
            boundaries.end_marker.is_some(),
            "End marker should now be Some due to end_match attribute"
        );
        match boundaries.end_marker.unwrap() {
            PageContent::Text(text_elem) => {
                assert_eq!(
                    text_elem.id, next_section_heading_id,
                    "End marker ID should be next_section_heading_id"
                );
                assert_eq!(
                    text_elem.text, "Next Section Starts Here",
                    "End marker text mismatch"
                );
            }
            _ => panic!("End marker should be a TextElement"),
        }

        // Assert Matched Content (Order matters)
        // Expected content: heading_id, para1, image1, para2 (because start_marker is inclusive, end_marker is exclusive)
        assert!(
            !section_match.matched_content.is_empty(),
            "Matched content should not be empty"
        );

        // Note: heading_id, para1_id, image_id, para2_id, next_section_heading_id are defined above in the test
        let expected_content_ids = vec![heading_id, para1_id, image_id, para2_id];
        let actual_content_ids: Vec<Uuid> = section_match
            .matched_content
            .iter()
            .map(|mc| match mc {
                MatchedContent::Text(te) => te.id,
                MatchedContent::Image(im) => im.id,
                MatchedContent::None => panic!("MatchedContent::None not expected here"),
            })
            .collect();

        assert_eq!(
            actual_content_ids, expected_content_ids,
            "Matched content IDs do not match expected order or content."
        );

        assert!(
            section_match.children.is_empty(),
            "Section children should be empty for this template"
        );
    }

    #[test]
    fn test_nested_sections() {
        // 1. Template Definition
        let child_section_1_1 = create_template_element(
            "Section 1.1",
            ElementType::Section,
            Some("Heading Section 1.1"),
            Some("Heading Section 1.2"), // Ends before next sibling
            Vec::new(),
        );
        let child_section_1_2 = create_template_element(
            "Section 1.2",
            ElementType::Section,
            Some("Heading Section 1.2"),
            Some("Heading Chapter 2"), // Ends before next chapter
            Vec::new(),
        );
        let parent_section_chapter_1 = create_template_element(
            "Chapter 1",
            ElementType::Section,
            Some("Heading Chapter 1"),
            Some("Heading Chapter 2"), // Chapter 1 ends before Chapter 2
            vec![child_section_1_1, child_section_1_2],
        );

        let template_elements = vec![parent_section_chapter_1];

        // 2. Document Content (PdfIndex Setup)
        let mut page_map: BTreeMap<u32, Vec<PageContent>> = BTreeMap::new();
        let chap1_h_id = Uuid::new_v4();
        let chap1_p1_id = Uuid::new_v4();
        let sec1_1_h_id = Uuid::new_v4();
        let sec1_1_p1_id = Uuid::new_v4();
        let sec1_2_h_id = Uuid::new_v4();
        let sec1_2_p1_id = Uuid::new_v4();
        let chap2_h_id = Uuid::new_v4();

        page_map.insert(
            1,
            vec![
                create_mock_text_element(
                    chap1_h_id,
                    "Heading Chapter 1",
                    "Arial",
                    20.0,
                    1,
                    50.0,
                    750.0,
                    200.0,
                    20.0,
                ),
                create_mock_text_element(
                    chap1_p1_id,
                    "Content for Chapter 1, before subsections.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    730.0,
                    400.0,
                    12.0,
                ),
                create_mock_text_element(
                    sec1_1_h_id,
                    "Heading Section 1.1",
                    "Arial",
                    16.0,
                    1,
                    70.0,
                    700.0,
                    180.0,
                    16.0,
                ),
                create_mock_text_element(
                    sec1_1_p1_id,
                    "Content for Section 1.1.",
                    "Arial",
                    12.0,
                    1,
                    70.0,
                    680.0,
                    300.0,
                    12.0,
                ),
                create_mock_text_element(
                    sec1_2_h_id,
                    "Heading Section 1.2",
                    "Arial",
                    16.0,
                    1,
                    70.0,
                    650.0,
                    180.0,
                    16.0,
                ),
                create_mock_text_element(
                    sec1_2_p1_id,
                    "Content for Section 1.2.",
                    "Arial",
                    12.0,
                    1,
                    70.0,
                    630.0,
                    300.0,
                    12.0,
                ),
                create_mock_text_element(
                    chap2_h_id,
                    "Heading Chapter 2",
                    "Arial",
                    20.0,
                    1,
                    50.0,
                    600.0,
                    200.0,
                    20.0,
                ),
            ],
        );

        let mock_match_context = MatchContext::default();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // 3. Call align_template_with_content
        let results_option = align_template_with_content(&template_elements, &index, None, None);

        // 4. Assertions
        assert!(
            results_option.is_some(),
            "align_template_with_content should return Some for nested structure"
        );
        let results = results_option.unwrap();
        assert_eq!(
            results.len(),
            1,
            "Should find one top-level match (Chapter 1)"
        );

        let chapter1_match = &results[0];
        assert_eq!(chapter1_match.template_element.name, "Chapter 1");
        assert_eq!(
            chapter1_match
                .section_boundaries
                .as_ref()
                .unwrap()
                .start_marker
                .id(),
            chap1_h_id
        );
        assert_eq!(
            chapter1_match
                .section_boundaries
                .as_ref()
                .unwrap()
                .end_marker
                .unwrap()
                .id(),
            chap2_h_id
        );
        // Content of Chapter 1 (direct) currently means all content between its own start/end markers.
        // Child content is also part of this list if we don't explicitly subtract it.
        // The `children` field separately details the matched child structures.
        let chapter1_all_inclusive_content_ids: Vec<Uuid> = chapter1_match.matched_content.iter().map(|mc| mc.id()).collect();
        assert_eq!(
            chapter1_all_inclusive_content_ids, 
            vec![chap1_h_id, chap1_p1_id, sec1_1_h_id, sec1_1_p1_id, sec1_2_h_id, sec1_2_p1_id], 
            "Chapter 1 matched_content should include its own heading, its direct paragraph, and all content up to its end_marker, which encompasses child section elements currently."
        );

        assert_eq!(chapter1_match.children.len(), 2, "Chapter 1 should have two child sections");

        // Assert Child Section 1.1
        let section1_1_match = &chapter1_match.children[0];
        assert_eq!(section1_1_match.template_element.name, "Section 1.1");
        assert_eq!(
            section1_1_match
                .section_boundaries
                .as_ref()
                .unwrap()
                .start_marker
                .id(),
            sec1_1_h_id
        );
        assert_eq!(
            section1_1_match
                .section_boundaries
                .as_ref()
                .unwrap()
                .end_marker
                .unwrap()
                .id(),
            sec1_2_h_id
        );
        let section1_1_content_ids: Vec<Uuid> = section1_1_match
            .matched_content
            .iter()
            .map(|mc| mc.id())
            .collect();
        assert_eq!(
            section1_1_content_ids,
            vec![sec1_1_h_id, sec1_1_p1_id],
            "Section 1.1 content mismatch"
        );

        // Assert Child Section 1.2
        let section1_2_match = &chapter1_match.children[1];
        assert_eq!(section1_2_match.template_element.name, "Section 1.2");
        assert_eq!(
            section1_2_match
                .section_boundaries
                .as_ref()
                .unwrap()
                .start_marker
                .id(),
            sec1_2_h_id
        );
        assert_eq!(
            section1_2_match
                .section_boundaries
                .as_ref()
                .unwrap()
                .end_marker
                .unwrap()
                .id(),
            chap2_h_id
        );
        let section1_2_content_ids: Vec<Uuid> = section1_2_match
            .matched_content
            .iter()
            .map(|mc| mc.id())
            .collect();
        assert_eq!(
            section1_2_content_ids,
            vec![sec1_2_h_id, sec1_2_p1_id],
            "Section 1.2 content mismatch"
        );
    }
}
