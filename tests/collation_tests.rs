use delver_pdf::dom::{Element, ElementType, MatchConfig, MatchType, Value};
use delver_pdf::matcher::{
    align_template_with_content, MatchedContent, SectionBoundaries, TemplateContentMatch,
};

mod common;
use common::{DocumentBuilder, TemplateBuilder, TestAssertions};

use uuid::Uuid;
use strsim;

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
        let textchunk_content_ids: Vec<Uuid> = textchunk_match.matched_content.iter().map(|mc| mc.id()).collect();
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
}
