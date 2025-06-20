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

    #[test]
    fn test_section_with_text_chunks_boundary_limiting_and_metadata() {
        // Test scenario: Section with end markers contains TextChunk children
        // This verifies two key behaviors:
        // 1. TextChunk respects parent section boundaries (doesn't process content beyond end marker)
        // 2. Metadata (like "as" attribute) gets propagated from parent sections to children
        
        // Create a template that mimics: 
        // Section(match="Management's Discussion...", end_match="Quantitative...", as="MD&A") {
        //   TextChunk(chunkSize=500, chunkOverlap=150)
        // }
        
        // First create the TextChunk child element
        let mut textchunk_attributes = HashMap::new();
        textchunk_attributes.insert("chunkSize".to_string(), Value::Number(500));
        textchunk_attributes.insert("chunkOverlap".to_string(), Value::Number(150));
        
        let textchunk_element = Element {
            name: "TextChunk".to_string(),
            element_type: ElementType::TextChunk,
            attributes: textchunk_attributes,
            children: Vec::new(),
            parent: None,
            prev_sibling: None,
            next_sibling: None,
        };
        
        // Create the Section parent element with "as" metadata
        let mut section_attributes = HashMap::new();
        section_attributes.insert(
            "match".to_string(), 
            Value::Array(vec![
                Value::String("Management's Discussion and Analysis of Financial".to_string()),
                Value::Number(400) // 0.4 * 1000 for more lenient matching
            ])
        );
        section_attributes.insert(
            "end_match".to_string(), 
            Value::String("Quantitative and Qualitative Disclosures About Market".to_string())
        );
        section_attributes.insert(
            "as".to_string(), 
            Value::String("MD&A".to_string())
        );
        
        let section_element = Element {
            name: "MDandA".to_string(),
            element_type: ElementType::Section,
            attributes: section_attributes,
            children: vec![textchunk_element],
            parent: None,
            prev_sibling: None,
            next_sibling: None,
        };
        
        let template_elements = vec![section_element];

        // Create document content with multiple sections
        let mut page_map: BTreeMap<u32, Vec<PageContent>> = BTreeMap::new();
        
        let mda_start_id = Uuid::new_v4();
        let mda_content1_id = Uuid::new_v4();
        let mda_content2_id = Uuid::new_v4();
        let mda_content3_id = Uuid::new_v4();
        let risk_section_id = Uuid::new_v4();
        let after_risk_id = Uuid::new_v4();

        page_map.insert(
            1,
            vec![
                // MD&A section start
                create_mock_text_element(
                    mda_start_id,
                    "Management's Discussion and Analysis of Financial Condition and Results of Operations",
                    "Arial",
                    14.0,
                    1,
                    50.0,
                    700.0,
                    500.0,
                    14.0,
                ),
                // MD&A content that should be included in chunks
                create_mock_text_element(
                    mda_content1_id,
                    "This is the first paragraph of the MD&A section. It contains important financial analysis and discussion about the company's performance during the fiscal year. This content should be included in the text chunks since it's between the start and end markers.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    680.0,
                    500.0,
                    12.0,
                ),
                create_mock_text_element(
                    mda_content2_id,
                    "This is the second paragraph of the MD&A section. It continues the financial analysis and provides more detailed discussion about various aspects of the business operations and financial condition.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    660.0,
                    500.0,
                    12.0,
                ),
                create_mock_text_element(
                    mda_content3_id,
                    "This is the third paragraph of the MD&A section. It concludes the discussion and analysis before the next section begins. This should also be included in the text chunks.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    640.0,
                    500.0,
                    12.0,
                ),
                // End marker - this should NOT be included in MD&A content
                create_mock_text_element(
                    risk_section_id,
                    "Quantitative and Qualitative Disclosures About Market Risk",
                    "Arial",
                    14.0,
                    1,
                    50.0,
                    620.0,
                    450.0,
                    14.0,
                ),
                // Content after end marker - should NOT be included
                create_mock_text_element(
                    after_risk_id,
                    "This content comes after the risk disclosures section and should not be included in the MD&A text chunks because it's beyond the end marker.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    600.0,
                    500.0,
                    12.0,
                ),
            ],
        );

        let mock_match_context = MatchContext::default();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // Call align_template_with_content
        let results_option = align_template_with_content(&template_elements, &index, None, None);

        // Verify results
        assert!(
            results_option.is_some(),
            "align_template_with_content should return Some for section with text chunks"
        );
        let results = results_option.unwrap();
        assert_eq!(
            results.len(),
            1,
            "Should find one match for the MD&A section template"
        );

        let section_match = &results[0];
        assert_eq!(section_match.template_element.name, "MDandA");

        // Verify section boundaries
        assert!(
            section_match.section_boundaries.is_some(),
            "Section boundaries should be set"
        );
        let boundaries = section_match.section_boundaries.as_ref().unwrap();

        // Check start marker
        assert_eq!(
            boundaries.start_marker.id(),
            mda_start_id,
            "Start marker should be the MD&A heading"
        );

        // Check end marker  
        assert!(
            boundaries.end_marker.is_some(),
            "End marker should be found"
        );
        assert_eq!(
            boundaries.end_marker.as_ref().unwrap().id(),
            risk_section_id,
            "End marker should be the risk disclosures heading"
        );

        // Verify section content (should include everything between markers, excluding end marker)
        let section_content_ids: Vec<Uuid> = section_match
            .matched_content
            .iter()
            .map(|mc| mc.id())
            .collect();
        assert_eq!(
            section_content_ids,
            vec![mda_start_id, mda_content1_id, mda_content2_id, mda_content3_id],
            "Section content should include start marker and all content up to (but not including) end marker"
        );

        // Verify that content after end marker is NOT included
        assert!(
            !section_content_ids.contains(&after_risk_id),
            "Content after end marker should not be included in section"
        );

        // Verify text chunk children
        assert_eq!(
            section_match.children.len(),
            1,
            "Section should have one TextChunk child"
        );
        
        let textchunk_match = &section_match.children[0];
        assert_eq!(
            textchunk_match.template_element.element_type,
            ElementType::TextChunk,
            "Child should be a TextChunk element"
        );

        // Verify that TextChunk content is limited to section boundaries
        // (i.e., it should not include content after the end marker)
        let textchunk_content_ids: Vec<Uuid> = textchunk_match
            .matched_content
            .iter()
            .map(|mc| mc.id())
            .collect();
        
        // TextChunk should only process content within the parent section boundaries
        // This means it should include the MD&A content but NOT the risk section or content after
        let expected_textchunk_content = vec![mda_start_id, mda_content1_id, mda_content2_id, mda_content3_id];
        
        // Check that all expected content is present
        for expected_id in &expected_textchunk_content {
            assert!(
                textchunk_content_ids.contains(expected_id),
                "TextChunk should include content with ID {} within section boundaries",
                expected_id
            );
        }
        
        // Check that unwanted content is NOT present
        assert!(
            !textchunk_content_ids.contains(&risk_section_id),
            "TextChunk should not include end marker content (risk section)"
        );
        assert!(
            !textchunk_content_ids.contains(&after_risk_id),
            "TextChunk should not include content after end marker"
        );

        // Verify metadata propagation: TextChunk should inherit metadata from parent Section
        // The system propagates the 'as' attribute as 'section' and section name as 'section_name'
        let textchunk_metadata = &textchunk_match.metadata;
        
        // Verify section metadata propagation
        assert!(
            textchunk_metadata.contains_key("section"),
            "TextChunk should inherit section metadata (from parent 'as' attribute)"
        );
        assert_eq!(
            textchunk_metadata.get("section").unwrap(),
            &Value::String("MD&A".to_string()),
            "TextChunk should inherit the correct section value from parent 'as' attribute"
        );
        
        assert!(
            textchunk_metadata.contains_key("section_name"),
            "TextChunk should inherit section_name metadata from parent"
        );
        assert_eq!(
            textchunk_metadata.get("section_name").unwrap(),
            &Value::String("MDandA".to_string()),
            "TextChunk should inherit the correct section_name from parent"
        );

        // Also verify that the section itself has the metadata
        let section_metadata = &section_match.metadata;
        assert_eq!(
            section_metadata.get("section").unwrap(),
            &Value::String("MD&A".to_string()),
            "Section should have its own section metadata (transformed from 'as' attribute)"
        );

        println!("✓ Section with TextChunk test passed:");
        println!("  - Section boundaries correctly identified");
        println!("  - TextChunk content limited to section boundaries");
        println!("  - Metadata properly propagated from Section to TextChunk");
        println!("  - Content after end marker correctly excluded");
    }

    #[test]
    fn test_textchunk_before_section_boundary_limiting() {
        // Test scenario: TextChunk appears before a Section as siblings
        // This verifies that TextChunk extracts content from start up to (but not including) the Section's start marker
        
        // Create a template that mimics:
        // TextChunk(chunkSize=300, chunkOverlap=50)
        // Section(match="Management's Discussion...", as="MD&A")
        
        // First create the TextChunk element (appears first)
        let mut textchunk_attributes = HashMap::new();
        textchunk_attributes.insert("chunkSize".to_string(), Value::Number(300));
        textchunk_attributes.insert("chunkOverlap".to_string(), Value::Number(50));
        
        let textchunk_element = Element {
            name: "IntroductionChunk".to_string(),
            element_type: ElementType::TextChunk,
            attributes: textchunk_attributes,
            children: Vec::new(),
            parent: None,
            prev_sibling: None,
            next_sibling: None,
        };
        
        // Create the Section element (appears second)
        let mut section_attributes = HashMap::new();
        section_attributes.insert(
            "match".to_string(), 
            Value::Array(vec![
                Value::String("Management's Discussion and Analysis of Financial".to_string()),
                Value::Number(400) // 0.4 * 1000 for matching
            ])
        );
        section_attributes.insert(
            "as".to_string(), 
            Value::String("MD&A".to_string())
        );
        
        let section_element = Element {
            name: "MDandA".to_string(),
            element_type: ElementType::Section,
            attributes: section_attributes,
            children: Vec::new(),
            parent: None,
            prev_sibling: None,
            next_sibling: None,
        };
        
        // Template has TextChunk first, then Section
        let template_elements = vec![textchunk_element, section_element];

        // Create document content with introduction content before MD&A section
        let mut page_map: BTreeMap<u32, Vec<PageContent>> = BTreeMap::new();
        
        let intro_para1_id = Uuid::new_v4();
        let intro_para2_id = Uuid::new_v4();
        let intro_para3_id = Uuid::new_v4();
        let mda_start_id = Uuid::new_v4();
        let mda_content_id = Uuid::new_v4();

        page_map.insert(
            1,
            vec![
                // Introduction content that should be captured by TextChunk
                create_mock_text_element(
                    intro_para1_id,
                    "This is the first paragraph of introduction content that appears before the MD&A section. This content should be captured by the TextChunk element since it comes before any section markers.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    750.0,
                    500.0,
                    12.0,
                ),
                create_mock_text_element(
                    intro_para2_id,
                    "This is the second paragraph of introduction content. It continues the preliminary discussion and should also be included in the TextChunk since it's still before the section boundary.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    730.0,
                    500.0,
                    12.0,
                ),
                create_mock_text_element(
                    intro_para3_id,
                    "This is the third and final paragraph of introduction content. It concludes the introductory material before the formal MD&A section begins.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    710.0,
                    500.0,
                    12.0,
                ),
                // MD&A section start - this should NOT be included in TextChunk
                create_mock_text_element(
                    mda_start_id,
                    "Management's Discussion and Analysis of Financial Condition and Results of Operations",
                    "Arial",
                    14.0,
                    1,
                    50.0,
                    690.0,
                    500.0,
                    14.0,
                ),
                // MD&A content - should NOT be included in TextChunk
                create_mock_text_element(
                    mda_content_id,
                    "This content is part of the MD&A section and should not be included in the TextChunk because it comes after the section start marker.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    670.0,
                    500.0,
                    12.0,
                ),
            ],
        );

        let mock_match_context = MatchContext::default();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // Call align_template_with_content
        let results_option = align_template_with_content(&template_elements, &index, None, None);

        // Verify results
        assert!(
            results_option.is_some(),
            "align_template_with_content should return Some for TextChunk before Section"
        );
        let results = results_option.unwrap();
        assert_eq!(
            results.len(),
            2,
            "Should find two matches: one for TextChunk and one for Section"
        );

        // Verify TextChunk match (should be first due to processing order)
        let textchunk_match = &results[0];
        assert_eq!(
            textchunk_match.template_element.element_type,
            ElementType::TextChunk,
            "First match should be the TextChunk element"
        );
        assert_eq!(textchunk_match.template_element.name, "IntroductionChunk");

        // Verify TextChunk content is limited to before Section start marker
        let textchunk_content_ids: Vec<Uuid> = textchunk_match
            .matched_content
            .iter()
            .map(|mc| mc.id())
            .collect();
        
        // TextChunk should include all introduction content but NOT the MD&A content
        let expected_textchunk_content = vec![intro_para1_id, intro_para2_id, intro_para3_id];
        
        // Check that all expected introduction content is present
        for expected_id in &expected_textchunk_content {
            assert!(
                textchunk_content_ids.contains(expected_id),
                "TextChunk should include introduction content with ID {} before section boundary",
                expected_id
            );
        }
        
        // Check that section content is NOT present in TextChunk
        assert!(
            !textchunk_content_ids.contains(&mda_start_id),
            "TextChunk should not include section start marker"
        );
        assert!(
            !textchunk_content_ids.contains(&mda_content_id),
            "TextChunk should not include content after section start marker"
        );

        // Verify Section match (should be second)
        let section_match = &results[1];
        assert_eq!(
            section_match.template_element.element_type,
            ElementType::Section,
            "Second match should be the Section element"
        );
        assert_eq!(section_match.template_element.name, "MDandA");

        // Verify Section boundaries
        assert!(
            section_match.section_boundaries.is_some(),
            "Section should have boundaries"
        );
        let boundaries = section_match.section_boundaries.as_ref().unwrap();
        assert_eq!(
            boundaries.start_marker.id(),
            mda_start_id,
            "Section start marker should be the MD&A heading"
        );

        // Verify Section content
        let section_content_ids: Vec<Uuid> = section_match.matched_content.iter().map(|mc| mc.id()).collect();
        assert_eq!(
            section_content_ids,
            vec![mda_start_id, mda_content_id],
            "Section should contain its start marker and internal content, but not end marker"
        );

        println!("✓ TextChunk before Section test passed:");
        println!("  - TextChunk correctly limited to content before Section start marker");
        println!("  - Section correctly identified its own boundaries");
        println!("  - No content overlap between TextChunk and Section");
        println!("  - Both elements successfully matched as siblings");
    }

    #[test]
    fn test_simple_textchunk_section_textchunk_pattern() {
        // Test the simple pattern: TextChunk A, Section { TextChunk B }, TextChunk C
        // This verifies that:
        // - TextChunk A processes content before the Section 
        // - Section processes its boundaries and contains TextChunk B
        // - TextChunk C processes content after the Section
        
        // Create template elements in order: TextChunk A, Section with child TextChunk B, TextChunk C
        let mut textchunk_a_attributes = HashMap::new();
        textchunk_a_attributes.insert("chunkSize".to_string(), Value::Number(200));
        textchunk_a_attributes.insert("chunkOverlap".to_string(), Value::Number(25));
        
        let textchunk_a = Element {
            name: "TextChunk_A".to_string(),
            element_type: ElementType::TextChunk,
            attributes: textchunk_a_attributes,
            children: Vec::new(),
            parent: None,
            prev_sibling: None,
            next_sibling: None,
        };
        
        // Create TextChunk B (child of Section)
        let mut textchunk_b_attributes = HashMap::new();
        textchunk_b_attributes.insert("chunkSize".to_string(), Value::Number(300));
        textchunk_b_attributes.insert("chunkOverlap".to_string(), Value::Number(50));
        
        let textchunk_b = Element {
            name: "TextChunk_B".to_string(),
            element_type: ElementType::TextChunk,
            attributes: textchunk_b_attributes,
            children: Vec::new(),
            parent: None,
            prev_sibling: None,
            next_sibling: None,
        };
        
        // Create Section with TextChunk B as child
        let mut section_attributes = HashMap::new();
        section_attributes.insert(
            "match".to_string(), 
            Value::Array(vec![
                Value::String("Main Section Heading".to_string()),
                Value::Number(500) // 0.5 threshold
            ])
        );
        section_attributes.insert(
            "end_match".to_string(), 
            Value::String("End of Main Section".to_string())
        );
        section_attributes.insert(
            "as".to_string(), 
            Value::String("MainSection".to_string())
        );
        
        let section = Element {
            name: "MainSection".to_string(),
            element_type: ElementType::Section,
            attributes: section_attributes,
            children: vec![textchunk_b],
            parent: None,
            prev_sibling: None,
            next_sibling: None,
        };
        
        // Create TextChunk C
        let mut textchunk_c_attributes = HashMap::new();
        textchunk_c_attributes.insert("chunkSize".to_string(), Value::Number(250));
        textchunk_c_attributes.insert("chunkOverlap".to_string(), Value::Number(30));
        
        let textchunk_c = Element {
            name: "TextChunk_C".to_string(),
            element_type: ElementType::TextChunk,
            attributes: textchunk_c_attributes,
            children: Vec::new(),
            parent: None,
            prev_sibling: None,
            next_sibling: None,
        };
        
        // Template: [TextChunk A, Section, TextChunk C]
        let template_elements = vec![textchunk_a, section, textchunk_c];

        // Create document content: introduction, section content, conclusion
        let mut page_map: BTreeMap<u32, Vec<PageContent>> = BTreeMap::new();
        
        // Introduction content (should go to TextChunk A)
        let intro1_id = Uuid::new_v4();
        let intro2_id = Uuid::new_v4();
        let intro3_id = Uuid::new_v4();
        
        // Section content
        let section_start_id = Uuid::new_v4();
        let section_content1_id = Uuid::new_v4();
        let section_content2_id = Uuid::new_v4();
        let section_end_id = Uuid::new_v4();
        
        // Conclusion content (should go to TextChunk C)
        let conclusion1_id = Uuid::new_v4();
        let conclusion2_id = Uuid::new_v4();

        page_map.insert(
            1,
            vec![
                // Introduction content (elements 0-2)
                create_mock_text_element(
                    intro1_id,
                    "This is the first paragraph of introduction content. It should be captured by TextChunk A.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    800.0,
                    500.0,
                    12.0,
                ),
                create_mock_text_element(
                    intro2_id,
                    "This is the second paragraph of introduction. It continues the preliminary discussion.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    780.0,
                    500.0,
                    12.0,
                ),
                create_mock_text_element(
                    intro3_id,
                    "This concludes the introduction before the main section begins.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    760.0,
                    500.0,
                    12.0,
                ),
                // Section content (elements 3-5)
                create_mock_text_element(
                    section_start_id,
                    "Main Section Heading",
                    "Arial",
                    16.0,
                    1,
                    50.0,
                    740.0,
                    300.0,
                    16.0,
                ),
                create_mock_text_element(
                    section_content1_id,
                    "This is content within the main section. It should be captured by TextChunk B as a child of the Section.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    720.0,
                    500.0,
                    12.0,
                ),
                create_mock_text_element(
                    section_content2_id,
                    "More content within the main section that should also be processed by TextChunk B.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    700.0,
                    500.0,
                    12.0,
                ),
                create_mock_text_element(
                    section_end_id,
                    "End of Main Section",
                    "Arial",
                    14.0,
                    1,
                    50.0,
                    680.0,
                    300.0,
                    14.0,
                ),
                // Conclusion content (elements 6-7)
                create_mock_text_element(
                    conclusion1_id,
                    "This is conclusion content that comes after the main section. It should be captured by TextChunk C.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    660.0,
                    500.0,
                    12.0,
                ),
                create_mock_text_element(
                    conclusion2_id,
                    "Final paragraph of the document that wraps up the discussion.",
                    "Arial",
                    12.0,
                    1,
                    50.0,
                    640.0,
                    500.0,
                    12.0,
                ),
            ],
        );

        let mock_match_context = MatchContext::default();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // Call align_template_with_content
        let results_option = align_template_with_content(&template_elements, &index, None, None);

        // Verify results
        assert!(
            results_option.is_some(),
            "align_template_with_content should return Some for TextChunk-Section-TextChunk pattern"
        );
        let results = results_option.unwrap();
        assert_eq!(
            results.len(),
            3,
            "Should find three matches: TextChunk A, Section, TextChunk C"
        );

        // Verify results are in template order: TextChunk A, Section, TextChunk C
        let textchunk_a_match = &results[0];
        assert_eq!(
            textchunk_a_match.template_element.element_type,
            ElementType::TextChunk,
            "First match should be TextChunk A (template order preserved)"
        );
        assert_eq!(textchunk_a_match.template_element.name, "TextChunk_A");

        let section_match = &results[1];
        assert_eq!(
            section_match.template_element.element_type,
            ElementType::Section,
            "Second match should be the Section (template order preserved)"
        );
        assert_eq!(section_match.template_element.name, "MainSection");

        let textchunk_c_match = &results[2];
        assert_eq!(
            textchunk_c_match.template_element.element_type,
            ElementType::TextChunk,
            "Third match should be TextChunk C (template order preserved)"
        );
        assert_eq!(textchunk_c_match.template_element.name, "TextChunk_C");

        // Verify Section match
        assert!(section_match.section_boundaries.is_some(), "Section should have boundaries");
        
        let section_boundaries = section_match.section_boundaries.as_ref().unwrap();
        assert_eq!(section_boundaries.start_marker.id(), section_start_id, "Section start should be the heading");
        assert!(section_boundaries.end_marker.is_some(), "Section should have end marker");
        assert_eq!(section_boundaries.end_marker.unwrap().id(), section_end_id, "Section end should be the end marker");

        // Verify Section content
        let section_content_ids: Vec<Uuid> = section_match.matched_content.iter().map(|mc| mc.id()).collect();
        assert_eq!(
            section_content_ids,
            vec![section_start_id, section_content1_id, section_content2_id],
            "Section should contain its start marker and internal content, but not end marker"
        );

        // Verify text chunk children
        assert_eq!(section_match.children.len(), 1, "Section should have one child");
        let textchunk_b_match = &section_match.children[0];
        assert_eq!(textchunk_b_match.template_element.name, "TextChunk_B");
        
        // Verify TextChunk B content (should be same as section content since it's processing within section boundaries)
        let textchunk_b_content_ids: Vec<Uuid> = textchunk_b_match.matched_content.iter().map(|mc| mc.id()).collect();
        assert_eq!(
            textchunk_b_content_ids,
            vec![section_start_id, section_content1_id, section_content2_id],
            "TextChunk B should process content within section boundaries"
        );

        // Verify TextChunk A content (should be introduction content before section)
        let textchunk_a_content_ids: Vec<Uuid> = textchunk_a_match.matched_content.iter().map(|mc| mc.id()).collect();
        assert_eq!(
            textchunk_a_content_ids,
            vec![intro1_id, intro2_id, intro3_id],
            "TextChunk A should process introduction content before section"
        );

        // Verify TextChunk C content (should be conclusion content after section)
        let textchunk_c_content_ids: Vec<Uuid> = textchunk_c_match.matched_content.iter().map(|mc| mc.id()).collect();
        assert_eq!(
            textchunk_c_content_ids,
            vec![conclusion1_id, conclusion2_id],
            "TextChunk C should process conclusion content after section"
        );

        println!("✓ Simple TextChunk-Section-TextChunk pattern test passed:");
        println!("  - TextChunk A correctly processed introduction content");
        println!("  - Section correctly identified boundaries and processed content");
        println!("  - TextChunk B correctly processed content within section boundaries");
        println!("  - TextChunk C correctly processed conclusion content after section");
        println!("  - No content overlap between elements");
    }
}
