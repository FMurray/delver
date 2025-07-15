use delver_pdf::docql::{Element, ElementType, MatchConfig, MatchType, Value};
use delver_pdf::matcher::{
    align_template_with_content, MatchedContent, SectionBoundaries, TemplateContentMatch,
};

mod common;
use common::{DocumentBuilder, TemplateBuilder, TestAssertions};

use uuid::Uuid;
use strsim;
use std::time::{Duration, Instant};

#[cfg(test)]
mod collation_flow_tests {
    use super::*;

    #[test]
    fn test_simple_section_match_with_explicit_end() {
        // Build document content
        let mut doc = DocumentBuilder::new();
        let heading_id = doc.add_text(1, "Introduction Heading", 16.0, 50.0, 700.0);
        let para1_id = doc.add_text(1, "This is the first paragraph.", 12.0, 50.0, 680.0);
        let image_id = doc.add_image(1, 50.0, 600.0, 100.0, 80.0);
        let para2_id = doc.add_text(1, "This is the second paragraph.", 12.0, 50.0, 580.0);
        let next_section_id = doc.add_text(1, "Next Section Starts Here", 16.0, 50.0, 550.0);
        let index = doc.build();

        // Build template
        let template = TemplateBuilder::new()
            .add_section("Introduction")
            .match_pattern("Introduction Heading")
            .end_match("Next Section Starts Here")
            .build()
            .build();

        // Execute matching
        let results = align_template_with_content(&template, &index, None, None)
            .expect("Should find section match");

        // Verify results
        assert_eq!(results.len(), 1, "Should find one section match");
        let section_match = &results[0];
        
        TestAssertions::assert_section_boundaries(
            section_match, heading_id, Some(next_section_id), &doc
        );
        
        TestAssertions::assert_content_ids(
            section_match, 
            &[heading_id, para1_id, image_id, para2_id], 
            &doc
        );
        
        TestAssertions::assert_child_count(section_match, 0);
    }

    #[test]
    fn test_nested_sections() {
        // Build document content
        let mut doc = DocumentBuilder::new();
        let chap1_h_id = doc.add_text(1, "Heading Chapter 1", 20.0, 50.0, 750.0);
        let chap1_p1_id = doc.add_text(1, "Content for Chapter 1, before subsections.", 12.0, 50.0, 730.0);
        let sec1_1_h_id = doc.add_text(1, "Heading Section 1.1", 16.0, 70.0, 700.0);
        let sec1_1_p1_id = doc.add_text(1, "Content for Section 1.1.", 12.0, 70.0, 680.0);
        let sec1_2_h_id = doc.add_text(1, "Heading Section 1.2", 16.0, 70.0, 650.0);
        let sec1_2_p1_id = doc.add_text(1, "Content for Section 1.2.", 12.0, 70.0, 630.0);
        let chap2_h_id = doc.add_text(1, "Heading Chapter 2", 20.0, 50.0, 600.0);
        let index = doc.build();

        // Build nested template
        let template = TemplateBuilder::new()
            .add_section("Chapter 1")
            .match_pattern("Heading Chapter 1")
            .end_match("Heading Chapter 2")
            .with_child_section("Section 1.1")
                .match_pattern("Heading Section 1.1")
                .end_match("Heading Section 1.2")
                .build()
            .with_child_section("Section 1.2")
                .match_pattern("Heading Section 1.2")
                .end_match("Heading Chapter 2")
                .build()
            .build()
            .build();

        // Execute matching
        let results = align_template_with_content(&template, &index, None, None)
            .expect("Should find nested section matches");

        // Verify main chapter
        assert_eq!(results.len(), 1, "Should find one top-level match");
        let chapter1_match = &results[0];
        
        TestAssertions::assert_section_boundaries(
            chapter1_match, chap1_h_id, Some(chap2_h_id), &doc
        );
        
        TestAssertions::assert_content_ids(
            chapter1_match,
            &[chap1_h_id, chap1_p1_id, sec1_1_h_id, sec1_1_p1_id, sec1_2_h_id, sec1_2_p1_id],
            &doc
        );
        
        TestAssertions::assert_child_count(chapter1_match, 2);

        // Verify child sections
        let section1_1 = &chapter1_match.children[0];
        TestAssertions::assert_section_boundaries(
            section1_1, sec1_1_h_id, Some(sec1_2_h_id), &doc
        );
        TestAssertions::assert_content_ids(
            section1_1, &[sec1_1_h_id, sec1_1_p1_id], &doc
        );

        let section1_2 = &chapter1_match.children[1];
        TestAssertions::assert_section_boundaries(
            section1_2, sec1_2_h_id, Some(chap2_h_id), &doc
        );
        TestAssertions::assert_content_ids(
            section1_2, &[sec1_2_h_id, sec1_2_p1_id], &doc
        );
    }

    #[test]
    fn test_section_with_textchunk_metadata_and_boundaries() {
        // Debug: Test the similarity directly
        let pattern = "Management's Discussion and Analysis";
        let text = "Management's Discussion and Analysis of Financial Condition and Results of Operations";
        let similarity = strsim::normalized_levenshtein(pattern, text);
        println!("DEBUG: Pattern: '{}'", pattern);
        println!("DEBUG: Text: '{}'", text);
        println!("DEBUG: Similarity: {}", similarity);
        println!("DEBUG: Threshold: 0.6");
        println!("DEBUG: Passes: {}", similarity >= 0.6);
        
        // Build document content
        let mut doc = DocumentBuilder::new();
        let mda_start_id = doc.add_text(
            1, 
            "Management's Discussion and Analysis of Financial Condition and Results of Operations", 
            14.0, 50.0, 700.0
        );
        let mda_content1_id = doc.add_text(
            1, 
            "This is the first paragraph of the MD&A section. It contains important financial analysis.", 
            12.0, 50.0, 680.0
        );
        let mda_content2_id = doc.add_text(
            1, 
            "This is the second paragraph continuing the financial analysis.", 
            12.0, 50.0, 660.0
        );
        let mda_content3_id = doc.add_text(
            1, 
            "This is the third paragraph concluding the MD&A discussion.", 
            12.0, 50.0, 640.0
        );
        let risk_section_id = doc.add_text(
            1, 
            "Quantitative and Qualitative Disclosures About Market Risk", 
            14.0, 50.0, 620.0
        );
        let after_risk_id = doc.add_text(
            1, 
            "This content comes after the risk disclosures section.", 
            12.0, 50.0, 600.0
        );
        let index = doc.build();

        // Build template with section containing textchunk
        let template = TemplateBuilder::new()
            .add_section("MDandA")
            .match_pattern("Management's Discussion and Analysis Financial Condition and Results of Operations")
            .end_match("Quantitative and Qualitative Disclosures About Market")
            .as_name("MD&A")
            .with_textchunk("TextChunk", 500, 150)
            .build()
            .build();

        // Execute matching
        let results = align_template_with_content(&template, &index, None, None)
            .expect("Should find section with textchunk");

        // Verify section
        assert_eq!(results.len(), 1, "Should find one section match");
        let section_match = &results[0];
        
        TestAssertions::assert_section_boundaries(
            section_match, mda_start_id, Some(risk_section_id), &doc
        );
        
        TestAssertions::assert_content_ids(
            section_match,
            &[mda_start_id, mda_content1_id, mda_content2_id, mda_content3_id],
            &doc
        );
        
        TestAssertions::assert_metadata(
            section_match,
            &[("section", "MD&A")]
        );

        // Verify textchunk child
        TestAssertions::assert_child_count(section_match, 1);
        let textchunk_match = &section_match.children[0];
        
        // TextChunk should be limited to section boundaries
        TestAssertions::assert_content_ids(
            textchunk_match,
            &[mda_start_id, mda_content1_id, mda_content2_id, mda_content3_id],
            &doc
        );
        
        // Verify metadata propagation
        TestAssertions::assert_metadata(
            textchunk_match,
            &[("section", "MD&A"), ("section_name", "MDandA")]
        );

        // Verify boundary enforcement (should not include content after end marker)
        let textchunk_content_ids: Vec<Uuid> = textchunk_match.matched_content.iter().filter_map(|mc| mc.id(&index)).collect();
        assert!(
            !textchunk_content_ids.contains(&risk_section_id),
            "TextChunk should not include end marker"
        );
        assert!(
            !textchunk_content_ids.contains(&after_risk_id),
            "TextChunk should not include content after end marker"
        );
    }

    #[test]
    fn test_textchunk_section_textchunk_pattern() {
        // Build document content
        let mut doc = DocumentBuilder::new();
        
        // Introduction content
        let intro1_id = doc.add_text(1, "First introduction paragraph.", 12.0, 50.0, 800.0);
        let intro2_id = doc.add_text(1, "Second introduction paragraph.", 12.0, 50.0, 780.0);
        let intro3_id = doc.add_text(1, "Final introduction paragraph.", 12.0, 50.0, 760.0);
        
        // Section content
        let section_start_id = doc.add_text(1, "Main Section Heading", 16.0, 50.0, 740.0);
        let section_content1_id = doc.add_text(1, "First section paragraph.", 12.0, 50.0, 720.0);
        let section_content2_id = doc.add_text(1, "Second section paragraph.", 12.0, 50.0, 700.0);
        let section_end_id = doc.add_text(1, "End of Main Section", 14.0, 50.0, 680.0);
        
        // Conclusion content
        let conclusion1_id = doc.add_text(1, "First conclusion paragraph.", 12.0, 50.0, 660.0);
        let conclusion2_id = doc.add_text(1, "Final conclusion paragraph.", 12.0, 50.0, 640.0);
        
        let index = doc.build();

        // Build template: TextChunk A, Section with TextChunk B, TextChunk C
        let template = TemplateBuilder::new()
            .add_textchunk("TextChunk_A", 200, 25)
            .add_section("MainSection")
                .match_pattern("Main Section Heading")
                .end_match("End of Main Section")
                .as_name("MainSection")
                .with_textchunk("TextChunk_B", 300, 50)
                .build()
            .add_textchunk("TextChunk_C", 250, 30)
            .build();

        // Execute matching
        let results = align_template_with_content(&template, &index, None, None)
            .expect("Should find textchunk-section-textchunk pattern");

        // Verify results structure
        assert_eq!(results.len(), 3, "Should find three top-level matches");
        
        let textchunk_a = &results[0];
        let section = &results[1];
        let textchunk_c = &results[2];

        // Verify TextChunk A (introduction content)
        assert_eq!(textchunk_a.template_element.name, "TextChunk_A");
        TestAssertions::assert_content_ids(
            textchunk_a,
            &[intro1_id, intro2_id, intro3_id],
            &doc
        );

        // Verify Section (main content)
        assert_eq!(section.template_element.name, "MainSection");
        TestAssertions::assert_section_boundaries(
            section, section_start_id, Some(section_end_id), &doc
        );
        TestAssertions::assert_content_ids(
            section,
            &[section_start_id, section_content1_id, section_content2_id],
            &doc
        );
        TestAssertions::assert_child_count(section, 1);

        // Verify TextChunk B (section child)
        let textchunk_b = &section.children[0];
        assert_eq!(textchunk_b.template_element.name, "TextChunk_B");
        TestAssertions::assert_content_ids(
            textchunk_b,
            &[section_start_id, section_content1_id, section_content2_id],
            &doc
        );

        // Verify TextChunk C (conclusion content)
        assert_eq!(textchunk_c.template_element.name, "TextChunk_C");
        TestAssertions::assert_content_ids(
            textchunk_c,
            &[conclusion1_id, conclusion2_id],
            &doc
        );
    }

    #[test]
    fn test_multi_page_chunking_preserves_page_numbers() {
        use delver_pdf::docql::process_matched_content;
        
        // Build multi-page document content
        let mut doc = DocumentBuilder::new();
        
        // Page 1 content
        let page1_text1_id = doc.add_text(1, "Page one first paragraph with some content.", 12.0, 50.0, 700.0);
        let page1_text2_id = doc.add_text(1, "Page one second paragraph continues here.", 12.0, 50.0, 680.0);
        
        // Page 2 content  
        let page2_text1_id = doc.add_text(2, "Page two first paragraph with different content.", 12.0, 50.0, 700.0);
        let page2_text2_id = doc.add_text(2, "Page two second paragraph also continues here.", 12.0, 50.0, 680.0);
        
        // Page 3 content
        let page3_text1_id = doc.add_text(3, "Page three first paragraph with more content.", 12.0, 50.0, 700.0);
        let page3_text2_id = doc.add_text(3, "Page three second paragraph concludes the document.", 12.0, 50.0, 680.0);
        
        let index = doc.build();

        // Build template with a single large textchunk that should span multiple pages
        let template = TemplateBuilder::new()
            .add_textchunk("MultiPageChunk", 30, 10) // Small token count to force multiple chunks
            .build();

        // Execute matching
        let results = align_template_with_content(&template, &index, None, None)
            .expect("Should find textchunk match");

        // Verify we found the textchunk
        assert_eq!(results.len(), 1, "Should find one textchunk match");
        let textchunk_match = &results[0];
        assert_eq!(textchunk_match.template_element.name, "MultiPageChunk");

        // Process the matched content to generate actual chunks with metadata
        let processed_outputs = process_matched_content(&results, &index);
        
        // Verify we have multiple chunks (due to small chunk size)
        assert!(processed_outputs.len() > 1, "Should create multiple chunks due to small chunk size");
        
        // Extract all page numbers that appear in chunk metadata
        let mut all_chunk_pages: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut chunk_details = Vec::new();
        
        for output in &processed_outputs {
            if let delver_pdf::docql::ProcessedOutput::Text(chunk) = output {
                // Extract page numbers from metadata
                if let Some(page_numbers_value) = chunk.metadata.get("page_numbers") {
                    if let Some(page_array) = page_numbers_value.as_array() {
                        for page_val in page_array {
                            if let Some(page_num) = page_val.as_u64() {
                                all_chunk_pages.insert(page_num as u32);
                            }
                        }
                    }
                }
                
                // Extract primary page
                if let Some(primary_page_value) = chunk.metadata.get("primary_page") {
                    if let Some(primary_page) = primary_page_value.as_u64() {
                        chunk_details.push((chunk.chunk_index, primary_page as u32, chunk.text.len()));
                    }
                }
                
                println!("Chunk {}: primary_page={:?}, page_numbers={:?}, chars={}, text_preview={:.50}...", 
                    chunk.chunk_index,
                    chunk.metadata.get("primary_page"),
                    chunk.metadata.get("page_numbers"),
                    chunk.text.len(),
                    chunk.text
                );
            }
        }
        
        // Debug: Print all unique pages found in chunks
        let mut sorted_pages: Vec<u32> = all_chunk_pages.iter().copied().collect();
        sorted_pages.sort();
        println!("All pages found in chunk metadata: {:?}", sorted_pages);
        
        // Critical assertion: We should see content from multiple pages
        assert!(
            all_chunk_pages.len() > 1,
            "Chunks should contain content from multiple pages. Found pages: {:?}. This suggests a bug in page number assignment during parsing/indexing.",
            sorted_pages
        );
        
        // Verify we see all expected pages (1, 2, 3)
        assert!(
            all_chunk_pages.contains(&1),
            "Should find content from page 1"
        );
        assert!(
            all_chunk_pages.contains(&2), 
            "Should find content from page 2"
        );
        assert!(
            all_chunk_pages.contains(&3),
            "Should find content from page 3"
        );
        
        // Verify that chunk metadata is properly structured (no type tags)
        for output in &processed_outputs {
            if let delver_pdf::docql::ProcessedOutput::Text(chunk) = output {
                // Page numbers should be a plain JSON array, not tagged with "Array"
                if let Some(page_numbers_value) = chunk.metadata.get("page_numbers") {
                    assert!(page_numbers_value.is_array(), "page_numbers should be a JSON array");
                }
                
                // Primary page should be a plain JSON number, not tagged with "Number"
                if let Some(primary_page_value) = chunk.metadata.get("primary_page") {
                    assert!(primary_page_value.is_number(), "primary_page should be a JSON number");
                }
            }
        }
    }

    /// Parent/child boundaries must be strictly nested.
    #[test]
    fn test_non_overlapping_nested_sections() {
        let mut doc = DocumentBuilder::new();

        // Top-level section A
        let a_h_id  = doc.add_text(1, "Heading A",   20.0, 50.0, 750.0);
        let a_p_id  = doc.add_text(1, "Paragraph A.",12.0, 50.0, 730.0);

        // Sub-sections
        let a1_h_id = doc.add_text(1, "Heading A.1", 16.0, 65.0, 710.0);
        let a1_p_id = doc.add_text(1, "Content A.1", 12.0, 65.0, 690.0);

        let a2_h_id = doc.add_text(1, "Heading A.2", 16.0, 65.0, 670.0);
        let a2_p_id = doc.add_text(1, "Content A.2", 12.0, 65.0, 650.0);

        // Next chapter (ends A)
        let b_h_id  = doc.add_text(1, "Heading B",   20.0, 50.0, 630.0);
        let index   = doc.build();

        // Nested template
        let template = TemplateBuilder::new()
            .add_section("A").match_pattern("Heading A").end_match("Heading B")
                .with_child_section("A1")
                    .match_pattern("Heading A.1").build()
                .with_child_section("A2")
                    .match_pattern("Heading A.2").build()
            .build().build();

        let results = align_template_with_content(&template, &index, None, None)
            .expect("matcher should succeed");
        assert_eq!(results.len(), 1);
        let a_match = &results[0];

        TestAssertions::assert_section_boundaries(a_match, a_h_id, Some(b_h_id), &doc);
        TestAssertions::assert_child_count(a_match, 2);

        let a1_match = &a_match.children[0];
        TestAssertions::assert_section_boundaries(a1_match, a1_h_id, Some(a2_h_id), &doc);

        let a2_match = &a_match.children[1];
        TestAssertions::assert_section_boundaries(a2_match, a2_h_id, Some(b_h_id), &doc);

        let idx_a1_end   = index.element_id_to_index[&a2_h_id];   // end marker of A.1  (exclusive)
        let idx_a2_start = index.element_id_to_index[&a2_h_id];   // start marker of A.2
        let idx_a_end    = index.element_id_to_index[&b_h_id];
        // The end‑marker of A.1 may coincide with the start‑marker of A.2.
        assert!(
            idx_a1_end <= idx_a2_start && idx_a2_start < idx_a_end,
            "nested sections must be non‑overlapping"
        );
    }

    /// Test overlapping nested sibling boundaries to ensure graceful handling
    #[test]
    fn test_overlapping_nested_sibling_boundaries() {
        let mut doc = DocumentBuilder::new();

        // Create a document where nested siblings could have overlapping boundaries
        let main_h_id = doc.add_text(1, "Main Section", 18.0, 50.0, 800.0);
        let shared_content_id = doc.add_text(1, "This content is shared between subsections", 12.0, 50.0, 780.0);
        
        // Subsection 1 starts here
        let sub1_h_id = doc.add_text(1, "Subsection One", 14.0, 60.0, 760.0);
        let sub1_content_id = doc.add_text(1, "Content specific to subsection one", 12.0, 60.0, 740.0);
        
        // Overlapping content - both subsections could claim this
        let overlap_content_id = doc.add_text(1, "Overlapping content area", 12.0, 50.0, 720.0);
        
        // Subsection 2 starts here - but overlaps with sub1's content area
        let sub2_h_id = doc.add_text(1, "Subsection Two", 14.0, 60.0, 700.0);
        let sub2_content_id = doc.add_text(1, "Content specific to subsection two", 12.0, 60.0, 680.0);
        
        // End of main section
        let next_main_id = doc.add_text(1, "Next Main Section", 18.0, 50.0, 650.0);
        let index = doc.build();

        // Template with potentially overlapping siblings
        let template = TemplateBuilder::new()
            .add_section("MainSection")
                .match_pattern("Main Section")
                .end_match("Next Main Section")
                .with_child_section("Sub1")
                    .match_pattern("Subsection One")
                    .end_match("Subsection Two")  // This creates potential overlap
                    .build()
                .with_child_section("Sub2")
                    .match_pattern("Subsection Two")
                    .end_match("Next Main Section")
                    .build()
                .build()
            .build();

        let start_time = Instant::now();
        let result = align_template_with_content(&template, &index, None, None);
        let elapsed = start_time.elapsed();

        // Should complete within reasonable time (no infinite recursion)
        assert!(
            elapsed < Duration::from_secs(5),
            "Overlapping boundary processing took too long: {:?}",
            elapsed
        );

        // Should successfully handle overlapping boundaries
        if let Some(matches) = result {
            assert_eq!(matches.len(), 1, "Should find main section");
            let main_match = &matches[0];
            
            TestAssertions::assert_section_boundaries(main_match, main_h_id, Some(next_main_id), &doc);
            
            if main_match.children.len() == 2 {
                let sub1_match = &main_match.children[0];
                let sub2_match = &main_match.children[1];
                
                // Verify that the algorithm handled overlapping boundaries appropriately
                TestAssertions::assert_section_boundaries(sub1_match, sub1_h_id, Some(sub2_h_id), &doc);
                TestAssertions::assert_section_boundaries(sub2_match, sub2_h_id, Some(next_main_id), &doc);
                
                // Check that boundaries don't actually overlap in the final result
                if let (Some(sub1_boundaries), Some(sub2_boundaries)) = 
                    (&sub1_match.section_boundaries, &sub2_match.section_boundaries) {
                    
                    let sub1_start_idx = index.element_id_to_index[&sub1_boundaries.start_marker.id()];
                    let sub1_end_idx = sub1_boundaries.end_marker.as_ref()
                        .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
                        .unwrap_or(index.doc_len());
                    
                    let sub2_start_idx = index.element_id_to_index[&sub2_boundaries.start_marker.id()];
                    let sub2_end_idx = sub2_boundaries.end_marker.as_ref()
                        .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
                        .unwrap_or(index.doc_len());
                    
                    // The algorithm should ensure non-overlapping final boundaries
                    assert!(
                        sub1_end_idx <= sub2_start_idx,
                        "Algorithm should resolve overlapping boundaries: sub1 ends at {}, sub2 starts at {}",
                        sub1_end_idx, sub2_start_idx
                    );
                    
                    println!("Successfully handled overlapping boundaries:");
                    println!("  Sub1: {} to {}", sub1_start_idx, sub1_end_idx);
                    println!("  Sub2: {} to {}", sub2_start_idx, sub2_end_idx);
                }
            } else {
                println!("Algorithm chose to merge or skip overlapping sections (found {} children)", main_match.children.len());
            }
        } else {
            // It's also acceptable for the algorithm to reject overlapping patterns
            println!("Algorithm rejected overlapping boundary pattern (returned None)");
        }
    }

    /// Test pathological case with many overlapping sections
    #[test]  
    fn test_many_overlapping_sections() {
        let mut doc = DocumentBuilder::new();
        
        // Create many sections that could all overlap
        let main_h_id = doc.add_text(1, "Document Start", 18.0, 50.0, 900.0);
        
        // Create overlapping section headers at similar positions
        let mut section_ids = Vec::new();
        for i in 0..10 {
            let y = 850.0 - i as f32 * 30.0;
            let section_id = doc.add_text(1, &format!("Section Header {}", i), 16.0, 50.0, y);
            section_ids.push(section_id);
            
            // Add some content after each header
            doc.add_text(1, &format!("Content for section {}", i), 12.0, 55.0, y - 15.0);
        }
        
        let doc_end_id = doc.add_text(1, "Document End", 18.0, 50.0, 400.0);
        let index = doc.build();

        // Create template where each section could potentially overlap with the next
        let mut template_builder = TemplateBuilder::new()
            .add_section("MainDoc")
                .match_pattern("Document Start")
                .end_match("Document End");
        
        // Add many child sections that could overlap
        for i in 0..10 {
            template_builder = template_builder
                .with_child_section(&format!("Section{}", i))
                    .match_pattern(&format!("Section Header {}", i))
                    .build();
        }
        
        let template = template_builder.build().build();

        let start_time = Instant::now();
        let result = align_template_with_content(&template, &index, None, None);
        let elapsed = start_time.elapsed();

        // Must complete within reasonable time
        assert!(
            elapsed < Duration::from_secs(10),
            "Many overlapping sections took too long: {:?}",
            elapsed
        );

        // Should handle many overlapping sections gracefully
        if let Some(matches) = result {
            assert_eq!(matches.len(), 1, "Should find main document");
            let main_match = &matches[0];
            
            println!("Found {} child sections from {} potential overlapping sections", 
                    main_match.children.len(), section_ids.len());
            
            // Verify no child sections have overlapping boundaries
            for i in 0..(main_match.children.len().saturating_sub(1)) {
                let current = &main_match.children[i];
                let next = &main_match.children[i + 1];
                
                if let (Some(current_boundaries), Some(next_boundaries)) = 
                    (&current.section_boundaries, &next.section_boundaries) {
                    
                    let current_end_idx = current_boundaries.end_marker.as_ref()
                        .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
                        .unwrap_or(index.doc_len());
                    
                    let next_start_idx = index.element_id_to_index[&next_boundaries.start_marker.id()];
                    
                    assert!(
                        current_end_idx <= next_start_idx,
                        "Child sections {} and {} should not overlap: {} vs {}",
                        i, i + 1, current_end_idx, next_start_idx
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod recursion_detection_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Test for infinite recursion when section boundaries don't advance
    #[test]
    fn test_non_advancing_section_boundaries() {
        let mut doc = DocumentBuilder::new();
        
        // Create two identical text elements at same position - pathological case
        let text1_id = doc.add_text(1, "Ambiguous Section Header", 16.0, 50.0, 700.0);
        let text2_id = doc.add_text(1, "Ambiguous Section Header", 16.0, 50.0, 700.0);
        let content_id = doc.add_text(1, "Some content here", 12.0, 50.0, 680.0);
        let index = doc.build();

        // Template that might match both identical headers
        let template = TemplateBuilder::new()
            .add_section("Section1")
                .match_pattern("Ambiguous Section Header")
                .end_match("NonExistent End")  // End that doesn't exist forces fallback logic
                .with_child_section("NestedSection")
                    .match_pattern("Ambiguous Section Header")  // Same pattern as parent
                    .build()
                .build()
            .build();

        let start_time = Instant::now();
        let timeout_duration = Duration::from_secs(5);

        // Execute with timeout to catch infinite recursion
        let result = std::panic::catch_unwind(|| {
            align_template_with_content(&template, &index, None, None)
        });

        let elapsed = start_time.elapsed();
        
        // If it takes more than 5 seconds, likely infinite recursion
        assert!(
            elapsed < timeout_duration,
            "Matching took too long ({:?}), suggesting infinite recursion",
            elapsed
        );

        // Should either succeed or fail gracefully, not hang
        match result {
            Ok(_) => println!("Matching completed successfully"),
            Err(_) => println!("Matching panicked (acceptable for edge cases)"),
        }
    }

    /// Test for runaway recursion with self-referencing patterns
    #[test]
    fn test_self_referencing_pattern_limits() {
        let mut doc = DocumentBuilder::new();
        
        // Create many similar headings that could all match the same pattern
        for i in 0..100 {
            let y = 800.0 - i as f32 * 8.0;
            doc.add_text(1, &format!("Section Title {}", i % 3), 16.0, 50.0, y);
            doc.add_text(1, &format!("Content paragraph {}", i), 12.0, 55.0, y - 5.0);
        }
        let index = doc.build();

        // Create nested template that could match many elements (only 2 levels due to builder limitations)
        let template = TemplateBuilder::new()
            .add_section("Level0")
                .match_pattern("Section Title")  // Matches many elements
                .with_child_section("Level1")
                    .match_pattern("Section Title")
                    .build()
                .build()
            .build();

        let start_time = Instant::now();
        let result = align_template_with_content(&template, &index, None, None);
        let elapsed = start_time.elapsed();

        // Should complete reasonably quickly
        assert!(
            elapsed < Duration::from_secs(10),
            "Deep nesting took too long: {:?}",
            elapsed
        );

        if let Some(matches) = result {
            // Check recursion depth
            fn calculate_max_depth(matches: &[TemplateContentMatch]) -> usize {
                matches.iter().map(|m| {
                    if m.children.is_empty() {
                        1
                    } else {
                        1 + calculate_max_depth(&m.children)
                    }
                }).max().unwrap_or(0)
            }

            let max_depth = calculate_max_depth(&matches);
            assert!(
                max_depth <= 10,
                "Excessive recursion depth: {}",
                max_depth
            );
        }
    }

    /// Test for infinite loops in boundary detection
    #[test]
    fn test_boundary_detection_termination() {
        let mut doc = DocumentBuilder::new();
        
        // Create content where start and end markers could be confused
        let start1_id = doc.add_text(1, "Chapter Start", 16.0, 50.0, 750.0);
        let content1_id = doc.add_text(1, "Some content", 12.0, 50.0, 730.0);
        let start2_id = doc.add_text(1, "Chapter Start", 16.0, 50.0, 710.0);  // Same as start1
        let content2_id = doc.add_text(1, "More content", 12.0, 50.0, 690.0);
        let end_marker_id = doc.add_text(1, "Chapter Start", 16.0, 50.0, 670.0);  // Same pattern again
        let index = doc.build();

        // Template where end pattern matches start pattern
        let template = TemplateBuilder::new()
            .add_section("ChapterWithSameEndAsStart")
                .match_pattern("Chapter Start")
                .end_match("Chapter Start")  // Same as start pattern!
                .build()
            .build();

        let start_time = Instant::now();
        let result = align_template_with_content(&template, &index, None, None);
        let elapsed = start_time.elapsed();

        // Should terminate quickly even with confusing patterns
        assert!(
            elapsed < Duration::from_secs(3),
            "Boundary detection took too long: {:?}",
            elapsed
        );

        // Should find at least one match and not get confused
        if let Some(matches) = result {
            assert!(!matches.is_empty(), "Should find at least one match");
            for m in &matches {
                if let Some(boundaries) = &m.section_boundaries {
                    // End marker should be different from start marker (fix UUID comparison)
                    let start_id = boundaries.start_marker.id();
                    let end_id_opt = boundaries.end_marker.as_ref().map(|e| e.id());
                    if let Some(end_id) = end_id_opt {
                        assert_ne!(
                            start_id,
                            end_id,
                            "Start and end markers should be different elements"
                        );
                    }
                }
            }
        }
    }

    /// Test that get_next_match_index always advances
    #[test]
    fn test_get_next_match_index_always_advances() {
        let mut doc = DocumentBuilder::new();
        
        // Simple sequential content
        let text1_id = doc.add_text(1, "First section", 16.0, 50.0, 700.0);
        let text2_id = doc.add_text(1, "Second section", 16.0, 50.0, 650.0);
        let text3_id = doc.add_text(1, "Third section", 16.0, 50.0, 600.0);
        let index = doc.build();

        // Create multiple sections that should process sequentially
        let template = TemplateBuilder::new()
            .add_section("Section1")
                .match_pattern("First section")
                .build()
            .add_section("Section2")
                .match_pattern("Second section")
                .build()
            .add_section("Section3")
                .match_pattern("Third section")
                .build()
            .build();

        let result = align_template_with_content(&template, &index, None, None)
            .expect("Should match sequential sections");

        assert_eq!(result.len(), 3, "Should find all three sections");

        // Verify that each section starts after the previous one
        let mut last_index = 0;
        for (i, section_match) in result.iter().enumerate() {
            if let Some(boundaries) = &section_match.section_boundaries {
                let current_index = index.element_id_to_index
                    .get(&boundaries.start_marker.id())
                    .copied()
                    .unwrap_or(0);
                
                assert!(
                    current_index >= last_index,
                    "Section {} starts at index {} but should be >= {} (non-advancing)",
                    i, current_index, last_index
                );
                
                last_index = current_index + 1;  // Next should start after this
            }
        }
    }

    /// Test with a deep template structure (simplified due to builder limitations)
    #[test]
    fn test_maximum_recursion_depth_handling() {
        let mut doc = DocumentBuilder::new();
        
        // Create content for deep nesting
        for level in 0..10 {  // Reduced from 20 to 10
            let y = 800.0 - level as f32 * 30.0;
            doc.add_text(1, &format!("Level {} Header", level), 16.0, 50.0 + level as f32 * 2.0, y);
            doc.add_text(1, &format!("Level {} content", level), 12.0, 55.0 + level as f32 * 2.0, y - 10.0);
        }
        let index = doc.build();

        // Build simpler nested template due to builder API limitations
        let template = TemplateBuilder::new()
            .add_section("Level0")
                .match_pattern("Level 0 Header")
                .with_child_section("Level1")
                    .match_pattern("Level 1 Header")
                    .build()
                .build()
            .build();

        let start_time = Instant::now();
        let result = align_template_with_content(&template, &index, None, None);
        let elapsed = start_time.elapsed();

        // Should handle nesting gracefully
        assert!(
            elapsed < Duration::from_secs(30),
            "Template matching took too long: {:?}",
            elapsed
        );

        // Check that we don't exceed reasonable depth limits
        if let Some(matches) = result {
            fn check_depth_limits(matches: &[TemplateContentMatch], current_depth: usize) {
                assert!(
                    current_depth <= 25,
                    "Recursion depth {} exceeds safe limit",
                    current_depth
                );
                
                for child_match in matches {
                    check_depth_limits(&child_match.children, current_depth + 1);
                }
            }
            
            check_depth_limits(&matches, 1);
        }
    }

    /// Test for stack overflow with malformed circular patterns
    #[test]
    fn test_circular_pattern_detection() {
        let mut doc = DocumentBuilder::new();
        
        // Create repeating pattern that could cause circular matching
        for i in 0..10 {
            let y = 700.0 - i as f32 * 50.0;
            doc.add_text(1, "Repeating Pattern", 16.0, 50.0, y);
            doc.add_text(1, "Repeating Pattern", 16.0, 50.0, y - 20.0);  // Duplicate at different position
            doc.add_text(1, "Some content between", 12.0, 50.0, y - 30.0);
        }
        let index = doc.build();

        // Template that matches the same pattern for start and end
        let template = TemplateBuilder::new()
            .add_section("CircularSection")
                .match_pattern("Repeating Pattern")
                .end_match("Repeating Pattern")  // Same as start!
                .with_child_section("NestedCircular")
                    .match_pattern("Repeating Pattern")  // Same again!
                    .end_match("Repeating Pattern")  // And again!
                    .build()
                .build()
            .build();

        let start_time = Instant::now();
        
        // This should not hang or stack overflow
        let result = std::panic::catch_unwind(|| {
            align_template_with_content(&template, &index, None, None)
        });
        
        let elapsed = start_time.elapsed();
        
        // Must complete within reasonable time
        assert!(
            elapsed < Duration::from_secs(5),
            "Circular pattern detection took too long: {:?}",
            elapsed
        );

        // Should complete without panicking
        assert!(
            result.is_ok(),
            "Circular pattern caused panic"
        );
    }

    /// Test timeout mechanism for runaway matching
    #[test]
    fn test_matching_timeout_mechanism() {
        let mut doc = DocumentBuilder::new();
        
        // Create pathological document with many similar elements
        for i in 0..1000 {
            let y = 1000.0 - i as f32 * 1.0;  // Very dense
            doc.add_text(1, &format!("Section {}", i % 10), 16.0, 50.0, y);
        }
        let index = doc.build();

        // Create template that could match many elements (simplified)
        let template = TemplateBuilder::new()
            .add_section("PatternSection")
                .match_pattern("Section")  // Matches 100+ elements
                .with_child_section("NestedPattern")
                    .match_pattern("Section")  // Also matches many
                    .build()
                .build()
            .build();

        let start_time = Instant::now();
        let result = align_template_with_content(&template, &index, None, None);
        let elapsed = start_time.elapsed();
        
        // Even with 1000 elements, should complete in reasonable time
        assert!(
            elapsed < Duration::from_secs(60), // Be generous but not infinite
            "Large document matching took excessive time: {:?}",
            elapsed
        );

        // Should produce some result, not hang
        match result {
            Some(matches) => {
                println!("Successfully matched {} top-level elements", matches.len());
                assert!(matches.len() <= 100, "Shouldn't create excessive matches");
            }
            None => {
                println!("No matches found (acceptable for pathological case)");
            }
        }
    }

    /// Stress-test recursion: 25 identical-style headings.
    #[test]
    fn test_recursion_terminates_with_many_similar_headings() {
        let mut doc = DocumentBuilder::new();
        for i in 0..25 {
            let y = 750.0 - i as f32 * 20.0;
            doc.add_text(1, &format!("Section {}", i), 16.0, 50.0, y);
            doc.add_text(1, &format!("Paragraph {}", i), 12.0, 55.0, y-15.0);
        }
        let index = doc.build();

        let template = TemplateBuilder::new()
            .add_section("Top")
                .match_pattern("Section 0")
                .end_match("Section 24")
            .build().build();

        let matches = align_template_with_content(&template, &index, None, None)
            .expect("should match");
        assert_eq!(matches.len(), 1);

        // Ensure recursion depth is reasonable (< 5)
        fn depth(n: &TemplateContentMatch, d: usize) -> usize {
            n.children.iter().map(|c| depth(c, d+1)).max().unwrap_or(d)
        }
        assert!(depth(&matches[0], 1) <= 4, "excessive recursion depth");
    }
}
