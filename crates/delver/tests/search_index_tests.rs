use delver_core::geo::Rect;
use delver_core::layout::MatchContext;
use delver_core::parse::{ImageElement, PageContent, PageContents, TextElement};
use delver_core::search_index::{
    FontSizeStats, FontUsage, ImageHandle, PdfIndex, SpatialPageContent, TextHandle,
};
use lopdf::Object;
use ordered_float;
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

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
        font_size,
        bbox: (x, y, x + width, y + height),
        page_number: page,
        font_name: Some(font_name.to_string()),
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
        bbox: Rect {
            x0: x,
            y0: y,
            x1: x + width,
            y1: y + height,
        },
        image_object: Object::Null, // Placeholder for tests
    })
}

fn basic_page_map_and_context() -> (BTreeMap<u32, PageContents>, MatchContext) {
    let mut page_map: BTreeMap<u32, PageContents> = BTreeMap::new();
    let el1_id = Uuid::new_v4();
    let el2_id = Uuid::new_v4();
    let el3_id = Uuid::new_v4();
    let img1_id = Uuid::new_v4();

    // Create PageContents for page 1
    let mut page1_contents = PageContents::new();

    // Add text elements using the proper add_text method
    if let PageContent::Text(text_elem) = create_mock_text_element(
        el1_id,
        "Hello World",
        "Arial",
        12.0,
        1,
        50.0,
        700.0,
        100.0,
        12.0,
    ) {
        page1_contents.add_text(text_elem);
    }

    if let PageContent::Text(text_elem) = create_mock_text_element(
        el2_id,
        "Section Title",
        "Times New Roman",
        18.0,
        1,
        50.0,
        650.0,
        150.0,
        18.0,
    ) {
        page1_contents.add_text(text_elem);
    }

    if let PageContent::Image(image_elem) =
        create_mock_image_element(img1_id, 1, 50.0, 500.0, 200.0, 100.0)
    {
        page1_contents.add_image(image_elem);
    }

    // Create PageContents for page 2
    let mut page2_contents = PageContents::new();

    if let PageContent::Text(text_elem) = create_mock_text_element(
        el3_id,
        "Another page",
        "Arial",
        12.0,
        2,
        50.0,
        700.0,
        120.0,
        12.0,
    ) {
        page2_contents.add_text(text_elem);
    }

    page_map.insert(1, page1_contents);
    page_map.insert(2, page2_contents);

    let match_context = MatchContext {
        destinations: Default::default(),
    };
    (page_map, match_context)
}

#[cfg(test)]
mod pdf_index_tests {
    use super::*;

    #[test]
    fn test_pdf_index_new_basic_construction() {
        let (page_map, mock_match_context) = basic_page_map_and_context();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // Verify total document length
        assert_eq!(
            index.doc_len(),
            4,
            "Total content items should be 4 (3 text + 1 image)"
        );

        // Verify page structure
        assert_eq!(index.by_page.len(), 2, "Should be 2 pages in by_page map");

        // Check page 1 content count
        let page1_content_indices = index
            .by_page
            .get(&1)
            .expect("Page 1 should exist in by_page");
        assert_eq!(
            page1_content_indices.len(),
            3,
            "Page 1 should have 3 total content items (2 text, 1 image)"
        );

        // Check page 2 content count
        let page2_content_indices = index
            .by_page
            .get(&2)
            .expect("Page 2 should exist in by_page");
        assert_eq!(
            page2_content_indices.len(),
            1,
            "Page 2 should have 1 total content item (1 text)"
        );

        // Verify text store has correct number of elements
        assert_eq!(
            index.text_store.text.len(),
            3,
            "Should have 3 text elements"
        );
        assert_eq!(
            index.image_store.bbox.len(),
            1,
            "Should have 1 image element"
        );

        // Verify font_size_index (should have 3 entries for text elements)
        assert_eq!(index.font_size_index.len(), 3);
        let sizes: Vec<f32> = index.font_size_index.iter().map(|(s, _)| *s).collect();
        assert_eq!(sizes, vec![12.0, 12.0, 18.0], "Font sizes should be sorted");

        // Verify element_id_to_index contains all IDs
        assert_eq!(
            index.element_id_to_index.len(),
            4,
            "Should have 4 elements in ID mapping"
        );

        // Verify fonts map
        assert_eq!(
            index.fonts.len(),
            2,
            "Should be 2 unique font styles (Arial 12pt, Times New Roman 18pt)"
        );

        let arial_canonical = delver_core::fonts::canonicalize::canonicalize_font_name("Arial");
        let times_canonical =
            delver_core::fonts::canonicalize::canonicalize_font_name("Times New Roman");

        // Key for Arial 12pt
        let arial_12_key = (
            arial_canonical.clone(),
            ordered_float::NotNan::new(12.0).unwrap(),
        );
        assert!(index.fonts.contains_key(&arial_12_key));
        assert_eq!(
            index.fonts.get(&arial_12_key).unwrap().total_usage,
            2, // Both "Hello World" and "Another page" use Arial 12pt
            "Arial 12pt should be used twice"
        );

        // Key for Times New Roman 18pt
        let times_18_key = (
            times_canonical.clone(),
            ordered_float::NotNan::new(18.0).unwrap(),
        );
        assert!(index.fonts.contains_key(&times_18_key));
        assert_eq!(
            index.fonts.get(&times_18_key).unwrap().total_usage,
            1,
            "Times New Roman 18pt should be used once"
        );

        // Verify font_name_frequency_index
        assert_eq!(index.font_name_frequency_index.len(), 2);
        assert_eq!(
            index.font_name_frequency_index[0].0,
            2, // Arial name used twice
            "Arial name (2 uses) should be first by frequency"
        );
        assert_eq!(index.font_name_frequency_index[0].1, arial_canonical);
        assert_eq!(
            index.font_name_frequency_index[1].0,
            1, // Times New Roman name used once
            "Times New Roman name (1 use) should be second"
        );
        assert_eq!(index.font_name_frequency_index[1].1, times_canonical);

        assert!(
            index.reference_count_index.is_empty(),
            "Reference count should be empty initially"
        );
    }

    #[test]
    fn test_handle_based_access() {
        let (page_map, mock_match_context) = basic_page_map_and_context();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // Test text handle access
        for i in 0..index.text_store.text.len() {
            let handle = TextHandle(i as u32);
            let text_ref = index.text(handle);

            // Verify the reference contains valid data
            assert!(!text_ref.text.is_empty(), "Text should not be empty");
            assert!(text_ref.font_size > 0.0, "Font size should be positive");
            assert!(text_ref.page_number > 0, "Page number should be positive");
        }

        // Test image handle access
        for i in 0..index.image_store.bbox.len() {
            let handle = ImageHandle(i as u32);
            let image_ref = index.image(handle);

            // Verify the reference contains valid data
            assert!(image_ref.page_number > 0, "Page number should be positive");
            assert!(
                image_ref.bbox.x1 > image_ref.bbox.x0,
                "Image should have positive width"
            );
            assert!(
                image_ref.bbox.y1 > image_ref.bbox.y0,
                "Image should have positive height"
            );
        }
    }

    #[test]
    fn test_find_text_matches_with_handles() {
        let (page_map, mock_match_context) = basic_page_map_and_context();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // Test exact match
        let matches = index.find_text_matches("Hello World", 0.9, None, None);
        assert_eq!(
            matches.len(),
            1,
            "Should find one exact match for 'Hello World'"
        );

        let (handle, score) = matches[0];
        let text_ref = index.text(handle);
        assert_eq!(text_ref.text, "Hello World");
        assert!(score > 0.9, "Score should be high for exact match");

        // Test partial match
        let partial_matches = index.find_text_matches("Hello", 0.4, None, None);
        assert!(
            partial_matches.len() >= 1,
            "Should find at least one partial match for 'Hello'"
        );

        // Test no match
        let no_matches = index.find_text_matches("Nonexistent", 0.8, None, None);
        assert_eq!(
            no_matches.len(),
            0,
            "Should find no matches for nonexistent text"
        );
    }

    #[test]
    fn test_get_elements_between_markers() {
        let (page_map, mock_match_context) = basic_page_map_and_context();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // Find elements by searching through the order vector
        let mut hello_world_element = None;
        let mut section_title_element = None;
        let mut another_page_element = None;
        let mut image_element = None;

        for (doc_idx, &handle) in index.order.iter().enumerate() {
            match handle {
                delver_core::parse::ContentHandle::Text(text_idx) => {
                    let text_handle = TextHandle(text_idx as u32);
                    let text_ref = index.text(text_handle);

                    if text_ref.text == "Hello World" {
                        hello_world_element = Some(PageContent::Text(TextElement {
                            id: text_ref.id,
                            text: text_ref.text.to_string(),
                            font_size: text_ref.font_size,
                            font_name: text_ref.font_name.map(|s| s.to_string()),
                            bbox: text_ref.bbox,
                            page_number: text_ref.page_number,
                        }));
                    } else if text_ref.text == "Section Title" {
                        section_title_element = Some(PageContent::Text(TextElement {
                            id: text_ref.id,
                            text: text_ref.text.to_string(),
                            font_size: text_ref.font_size,
                            font_name: text_ref.font_name.map(|s| s.to_string()),
                            bbox: text_ref.bbox,
                            page_number: text_ref.page_number,
                        }));
                    } else if text_ref.text == "Another page" {
                        another_page_element = Some(PageContent::Text(TextElement {
                            id: text_ref.id,
                            text: text_ref.text.to_string(),
                            font_size: text_ref.font_size,
                            font_name: text_ref.font_name.map(|s| s.to_string()),
                            bbox: text_ref.bbox,
                            page_number: text_ref.page_number,
                        }));
                    }
                }
                delver_core::parse::ContentHandle::Image(image_idx) => {
                    let image_handle = ImageHandle(image_idx as u32);
                    let image_ref = index.image(image_handle);

                    image_element = Some(PageContent::Image(ImageElement {
                        id: image_ref.id,
                        page_number: image_ref.page_number,
                        bbox: image_ref.bbox,
                        image_object: image_ref.image_object.clone(),
                    }));
                }
            }
        }

        let hello_world = hello_world_element.expect("Should find 'Hello World' element");
        let section_title = section_title_element.expect("Should find 'Section Title' element");
        let another_page = another_page_element.expect("Should find 'Another page' element");
        let image = image_element.expect("Should find image element");

        // Test 1: Get elements between two text elements
        let elements_between =
            index.get_elements_between_markers(&hello_world, Some(&another_page));

        // Should include: "Hello World", "Section Title", and the image element
        // but exclude "Another page" since it's the end marker
        assert_eq!(
            elements_between.len(),
            3,
            "Should return 3 elements between Hello World and Another page"
        );

        // Test 2: Get elements from start to end of document
        let elements_to_end = index.get_elements_between_markers(&section_title, None);

        // Should include: "Section Title", image, and "Another page"
        assert_eq!(
            elements_to_end.len(),
            3,
            "Should return 3 elements from Section Title to end"
        );

        // Test 3: Get elements between same element (should return empty)
        let same_element = index.get_elements_between_markers(&hello_world, Some(&hello_world));
        assert_eq!(
            same_element.len(),
            0,
            "Should return empty when start and end are the same element"
        );

        // Test 4: Get elements with non-existent start element
        let non_existent_element = create_mock_text_element(
            Uuid::new_v4(),
            "Non-existent",
            "Arial",
            12.0,
            1,
            0.0,
            0.0,
            100.0,
            12.0,
        );
        let empty_result =
            index.get_elements_between_markers(&non_existent_element, Some(&hello_world));
        assert_eq!(
            empty_result.len(),
            0,
            "Should return empty when start element doesn't exist in index"
        );
    }

    #[test]
    fn test_handle_conversion() {
        let (page_map, mock_match_context) = basic_page_map_and_context();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // Test getting handles from document indices
        for doc_idx in 0..index.doc_len() {
            if let Some(handle) = index.get_handle(doc_idx) {
                match handle {
                    delver_core::parse::ContentHandle::Text(text_idx) => {
                        let text_handle = index
                            .as_text_handle(handle)
                            .expect("Should convert to text handle");
                        assert_eq!(text_handle.0, text_idx as u32);

                        // Test accessing the element
                        let text_ref = index.text(text_handle);
                        assert!(!text_ref.text.is_empty());
                    }
                    delver_core::parse::ContentHandle::Image(image_idx) => {
                        let image_handle = index
                            .as_image_handle(handle)
                            .expect("Should convert to image handle");
                        assert_eq!(image_handle.0, image_idx as u32);

                        // Test accessing the element
                        let image_ref = index.image(image_handle);
                        assert!(image_ref.page_number > 0);
                    }
                }
            }
        }
    }

    #[test]
    fn test_similarity_search_with_handles() {
        let (page_map, mock_match_context) = basic_page_map_and_context();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // Create a seed element similar to one in the index
        let seed = TextElement {
            id: Uuid::new_v4(),
            text: "Hello Test".to_string(),
            font_size: 12.0,
            font_name: Some("Arial".to_string()),
            bbox: (50.0, 700.0, 150.0, 712.0),
            page_number: 1,
        };

        // Test similarity search
        let similar = index.top_k_similar_text(&seed, 0, index.doc_len(), 2);

        // Should find similar elements (Arial 12pt elements)
        assert!(similar.len() > 0, "Should find similar elements");

        for (handle, similarity_score) in similar {
            let text_ref = index.text(handle);
            assert!(
                similarity_score >= 0.0 && similarity_score <= 1.0,
                "Similarity score should be between 0 and 1"
            );
            assert_eq!(
                text_ref.font_size, 12.0,
                "Similar elements should have same font size"
            );
        }
    }

    #[test]
    fn test_zero_copy_performance() {
        let (page_map, mock_match_context) = basic_page_map_and_context();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // Test that we can access many elements efficiently without cloning
        let start = std::time::Instant::now();

        for i in 0..index.text_store.text.len() {
            let handle = TextHandle(i as u32);
            let text_ref = index.text(handle);

            // Just access the data (zero-copy)
            let _text_len = text_ref.text.len();
            let _font_size = text_ref.font_size;
            let _page = text_ref.page_number;
        }

        let duration = start.elapsed();
        println!("Zero-copy access took: {:?}", duration);

        // This should be very fast since we're not cloning anything
        assert!(
            duration.as_millis() < 100,
            "Zero-copy access should be very fast"
        );
    }
}
