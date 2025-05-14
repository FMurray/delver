use delver_pdf::geo::Rect;
use delver_pdf::layout::MatchContext; // Assuming MatchContext is in layout
use delver_pdf::parse::{ImageElement, PageContent, TextElement};
use delver_pdf::search_index::{FontSizeStats, FontUsage, PdfIndex, SpatialPageContent};
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
        font_name: Some(font_name.to_string()), // Keep it simple for tests
        operators: Vec::new(),
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

fn basic_page_map_and_context() -> (BTreeMap<u32, Vec<PageContent>>, MatchContext) {
    let mut page_map: BTreeMap<u32, Vec<PageContent>> = BTreeMap::new();
    let el1_id = Uuid::new_v4();
    let el2_id = Uuid::new_v4();
    let el3_id = Uuid::new_v4();
    let img1_id = Uuid::new_v4();

    page_map.insert(
        1,
        vec![
            create_mock_text_element(
                el1_id,
                "Hello World",
                "Arial",
                12.0,
                1,
                50.0,
                700.0,
                100.0,
                12.0,
            ),
            create_mock_text_element(
                el2_id,
                "Section Title",
                "Times New Roman",
                18.0,
                1,
                50.0,
                650.0,
                150.0,
                18.0,
            ),
            create_mock_image_element(img1_id, 1, 50.0, 500.0, 200.0, 100.0),
        ],
    );
    page_map.insert(
        2,
        vec![create_mock_text_element(
            el3_id,
            "Another page",
            "Arial",
            12.0,
            2,
            50.0,
            700.0,
            120.0,
            12.0,
        )],
    );

    let match_context = MatchContext {
        destinations: Default::default(), // Empty for now
                                          // lines_by_page: Default::default(), // Assuming this field exists and is needed for PdfIndex::new or its helpers
    };
    (page_map, match_context)
}

#[cfg(test)]
mod pdf_index_tests {
    use super::*; // Import helpers from parent module

    #[test]
    fn test_pdf_index_new_basic_construction() {
        let (page_map, mock_match_context) = basic_page_map_and_context();
        let index = PdfIndex::new(&page_map, &mock_match_context);

        // Verify counts
        let total_text_elements = index
            .all_ordered_content
            .iter()
            .filter(|pc| matches!(pc, PageContent::Text(_)))
            .count();
        let total_image_elements = index
            .all_ordered_content
            .iter()
            .filter(|pc| matches!(pc, PageContent::Image(_)))
            .count();

        assert_eq!(total_text_elements, 3, "Total text elements should be 3");
        assert_eq!(total_image_elements, 1, "Total image elements should be 1");
        assert_eq!(
            index.all_ordered_content.len(),
            4,
            "Total content items should be 4 (3 text + 1 image)"
        );

        assert_eq!(index.by_page.len(), 2, "Should be 2 pages in by_page map");

        // Verify image count on page 1 using by_page and all_ordered_content
        let page1_content_indices = index
            .by_page
            .get(&1)
            .expect("Page 1 should exist in by_page");
        let page1_images_count = page1_content_indices
            .iter()
            .filter(|&&idx| {
                matches!(
                    index.all_ordered_content.get(idx),
                    Some(PageContent::Image(_))
                )
            })
            .count();
        assert_eq!(page1_images_count, 1, "Page 1 should have 1 image element");

        // Verify image count on page 2 (should be 0)
        if let Some(page2_content_indices) = index.by_page.get(&2) {
            let page2_images_count = page2_content_indices
                .iter()
                .filter(|&&idx| {
                    matches!(
                        index.all_ordered_content.get(idx),
                        Some(PageContent::Image(_))
                    )
                })
                .count();
            assert_eq!(
                page2_images_count, 0,
                "Page 2 should have no image elements"
            );
        } else {
            panic!("Page 2 not found in by_page, but it has content.");
        }

        // Verify content of by_page (total items per page)
        assert_eq!(
            index.by_page.get(&1).unwrap().len(),
            3,
            "Page 1 should have 3 total content items (2 text, 1 image)"
        );
        assert_eq!(
            index.by_page.get(&2).unwrap().len(),
            1,
            "Page 2 should have 1 total content item (1 text)"
        );

        // Verify font_size_index (still refers to indices in all_ordered_content, but only for Text elements)
        assert_eq!(index.font_size_index.len(), 3);
        let sizes: Vec<f32> = index.font_size_index.iter().map(|(s, _)| *s).collect();
        assert_eq!(sizes, vec![12.0, 12.0, 18.0], "Font sizes should be sorted");

        // Verify element_id_to_index contains all IDs (Text and Image)
        for (_page_num, page_content_vec) in &page_map {
            // Iterate over page_map to get original elements and IDs
            for pc in page_content_vec {
                assert!(
                    index.element_id_to_index.contains_key(&pc.id()),
                    "ID {} not found in element_id_to_index",
                    pc.id()
                );
                // Check if the index stored is correct
                let stored_idx = index.element_id_to_index.get(&pc.id()).unwrap();
                assert_eq!(
                    index.all_ordered_content[*stored_idx].id(),
                    pc.id(),
                    "Mismatch between ID and content at stored index"
                );
            }
        }

        // Verify fonts map
        assert_eq!(
            index.fonts.len(),
            2, // Corrected: Arial 12pt, Times New Roman 18pt are the 2 distinct styles
            "Should be 2 unique font styles"
        );
        let arial_canonical = delver_pdf::fonts::canonicalize::canonicalize_font_name("Arial");
        let times_canonical =
            delver_pdf::fonts::canonicalize::canonicalize_font_name("Times New Roman");

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
        let arial_element_idx = index.fonts.get(&arial_12_key).unwrap().elements[0];
        match &index.all_ordered_content[arial_element_idx] {
            PageContent::Text(te) => assert!((te.font_size - 12.0).abs() < 0.1),
            _ => panic!("Expected text element for Arial 12pt font usage"),
        }

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
        let times_element_idx = index.fonts.get(&times_18_key).unwrap().elements[0];
        match &index.all_ordered_content[times_element_idx] {
            PageContent::Text(te) => assert!((te.font_size - 18.0).abs() < 0.1),
            _ => panic!("Expected text element for Times New Roman 18pt font usage"),
        }

        // Verify font_name_frequency_index (this was renamed)
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
}
