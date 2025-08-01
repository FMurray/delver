use delver_core::docql::{process_matched_content, Element, ElementType, ProcessedOutput, Value};
use delver_core::matcher::{align_template_with_content, MatchedContent, TemplateContentMatch};
use delver_core::parse::{PageContent, TextElement};
use delver_core::search_index::PdfIndex;
use std::collections::HashMap;
mod common;
use common::{create_mock_text_element, DocumentBuilder, TemplateBuilder};

#[test]
fn test_nested_sections_parent_references() {
    // Create a document with text elements using the builder pattern
    let mut doc_builder = DocumentBuilder::new();

    // Add some test text elements
    let _text_id1 = doc_builder.add_text(1, "This is the main section content", 12.0, 100.0, 700.0);
    let _text_id2 = doc_builder.add_text(1, "This is a subsection content", 10.0, 120.0, 650.0);
    let _text_id3 =
        doc_builder.add_text(1, "This is nested subsection content", 10.0, 140.0, 600.0);

    // Build the index
    let index = doc_builder.build();

    // Create template elements - fix structure: Sections can have children, TextChunks cannot
    let main_section = Element::new("MainSection".to_string(), ElementType::Section);

    let subsection = Element::new("SubSection".to_string(), ElementType::Section);

    let mut nested_textchunk = Element::new("NestedTextChunk".to_string(), ElementType::TextChunk);
    nested_textchunk
        .attributes
        .insert("chunkSize".to_string(), Value::Number(100));

    // Create template content matches
    let main_section_match = TemplateContentMatch {
        template_element: &main_section,
        matched_content: vec![MatchedContent::Index(0)], // text_elem1
        children: vec![TemplateContentMatch {
            template_element: &subsection,
            matched_content: vec![MatchedContent::Index(1)], // text_elem2
            children: vec![TemplateContentMatch {
                template_element: &nested_textchunk,
                matched_content: vec![MatchedContent::Index(2)], // text_elem3
                children: vec![],                                // TextChunks don't have children
                metadata: HashMap::new(),
                section_boundaries: None,
            }],
            metadata: HashMap::new(),
            section_boundaries: None,
        }],
        metadata: HashMap::new(),
        section_boundaries: None,
    };

    // Process the matched content
    let matches = vec![main_section_match];
    let outputs = process_matched_content(&matches, &index, None);

    // Verify the outputs
    assert!(!outputs.is_empty(), "Should produce some outputs");

    // Debug: Print what outputs we actually got
    println!("Total outputs: {}", outputs.len());
    for (i, output) in outputs.iter().enumerate() {
        match output {
            ProcessedOutput::Text(chunk) => {
                println!("Output {}: Text - '{}'", i, chunk.text);
                println!(
                    "  Parent name: {:?}, Parent index: {:?}",
                    chunk.parent_name, chunk.parent_index
                );
            }
            ProcessedOutput::Image(img) => {
                println!("Output {}: Image - {}", i, img.id);
            }
        }
    }

    // Check that we have the expected number of outputs
    // We expect 3 text outputs: main section, subsection, and nested subsection
    let text_outputs: Vec<_> = outputs
        .iter()
        .filter_map(|output| match output {
            ProcessedOutput::Text(chunk) => Some(chunk),
            _ => None,
        })
        .collect();

    println!("Text outputs count: {}", text_outputs.len());
    assert_eq!(text_outputs.len(), 3, "Should have 3 text outputs");

    // First output should be the main section with no parent
    let main_output = &text_outputs[0];
    assert!(main_output.text.contains("main section content"));
    assert_eq!(
        main_output.parent_name, None,
        "Main section should have no parent name"
    );
    assert_eq!(
        main_output.parent_index, None,
        "Main section should have no parent index"
    );

    // Second output should be the subsection with main section as parent
    let sub_output = &text_outputs[1];
    assert!(sub_output.text.contains("subsection content"));
    assert_eq!(
        sub_output.parent_name,
        Some("MainSection".to_string()),
        "Subsection should have MainSection as parent"
    );
    assert_eq!(
        sub_output.parent_index,
        Some(0),
        "Subsection should have parent index 0"
    );

    // Third output should be the nested textchunk with subsection as parent
    let nested_output = &text_outputs[2];
    assert!(nested_output.text.contains("nested subsection content"));
    assert_eq!(
        nested_output.parent_name,
        Some("SubSection".to_string()),
        "Nested textchunk should have SubSection as parent"
    );
    // The parent index should be 1 since the SubSection will be the second output (index 1)
    assert_eq!(
        nested_output.parent_index,
        Some(1),
        "Nested textchunk should correctly reference SubSection at index 1"
    );
}

#[test]
fn test_top_level_elements_no_parent() {
    // Create a document with a single text element
    let mut doc_builder = DocumentBuilder::new();
    let _text_id = doc_builder.add_text(1, "Top level content", 12.0, 100.0, 700.0);
    let index = doc_builder.build();

    // Create template element
    let mut text_chunk = Element::new("TopLevelChunk".to_string(), ElementType::TextChunk);
    text_chunk
        .attributes
        .insert("chunkSize".to_string(), Value::Number(100));

    // Create template content match
    let text_match = TemplateContentMatch {
        template_element: &text_chunk,
        matched_content: vec![MatchedContent::Index(0)],
        children: vec![],
        metadata: HashMap::new(),
        section_boundaries: None,
    };

    // Process the matched content
    let matches = vec![text_match];
    let outputs = process_matched_content(&matches, &index, None);

    // Verify the output
    assert_eq!(outputs.len(), 1, "Should have exactly one output");

    match &outputs[0] {
        ProcessedOutput::Text(chunk) => {
            assert!(chunk.text.contains("Top level content"));
            assert_eq!(
                chunk.parent_name, None,
                "Top level element should have no parent name"
            );
            assert_eq!(
                chunk.parent_index, None,
                "Top level element should have no parent index"
            );
        }
        _ => panic!("Expected text output"),
    }
}

#[test]
fn test_multiple_sibling_sections() {
    // Create a document with multiple text elements
    let mut doc_builder = DocumentBuilder::new();
    let _text_id1 = doc_builder.add_text(1, "First sibling content", 12.0, 100.0, 700.0);
    let _text_id2 = doc_builder.add_text(1, "Second sibling content", 12.0, 100.0, 650.0);
    let index = doc_builder.build();

    // Create template elements
    let mut chunk1 = Element::new("FirstChunk".to_string(), ElementType::TextChunk);
    chunk1
        .attributes
        .insert("chunkSize".to_string(), Value::Number(100));

    let mut chunk2 = Element::new("SecondChunk".to_string(), ElementType::TextChunk);
    chunk2
        .attributes
        .insert("chunkSize".to_string(), Value::Number(100));

    // Create template content matches (siblings)
    let matches = vec![
        TemplateContentMatch {
            template_element: &chunk1,
            matched_content: vec![MatchedContent::Index(0)],
            children: vec![],
            metadata: HashMap::new(),
            section_boundaries: None,
        },
        TemplateContentMatch {
            template_element: &chunk2,
            matched_content: vec![MatchedContent::Index(1)],
            children: vec![],
            metadata: HashMap::new(),
            section_boundaries: None,
        },
    ];

    // Process the matched content
    let outputs = process_matched_content(&matches, &index, None);

    // Verify both outputs have no parent (since they're siblings at top level)
    assert_eq!(outputs.len(), 2, "Should have exactly two outputs");

    for output in &outputs {
        match output {
            ProcessedOutput::Text(chunk) => {
                assert_eq!(
                    chunk.parent_name, None,
                    "Sibling elements should have no parent name"
                );
                assert_eq!(
                    chunk.parent_index, None,
                    "Sibling elements should have no parent index"
                );
            }
            _ => panic!("Expected text output"),
        }
    }
}

#[test]
fn test_section_with_text_children() {
    // Create a document with text elements for a section
    let mut doc_builder = DocumentBuilder::new();
    let _text_id1 = doc_builder.add_text(1, "Section header text", 14.0, 100.0, 700.0);
    let _text_id2 = doc_builder.add_text(1, "Section body content", 12.0, 100.0, 650.0);
    let index = doc_builder.build();

    // Create a section with text content (not children)
    let section = Element::new("DocumentSection".to_string(), ElementType::Section);

    // Create template content match for section without children
    let section_match = TemplateContentMatch {
        template_element: &section,
        matched_content: vec![MatchedContent::Index(0), MatchedContent::Index(1)],
        children: vec![], // No children, so section will process its own content
        metadata: HashMap::new(),
        section_boundaries: None,
    };

    // Process the matched content
    let matches = vec![section_match];
    let outputs = process_matched_content(&matches, &index, None);

    // Section with no children should process its own content and have no parent
    assert_eq!(outputs.len(), 1, "Should have exactly one output");

    match &outputs[0] {
        ProcessedOutput::Text(chunk) => {
            assert!(
                chunk.text.contains("Section header text")
                    || chunk.text.contains("Section body content")
            );
            assert_eq!(
                chunk.parent_name, None,
                "Section without children should have no parent name"
            );
            assert_eq!(
                chunk.parent_index, None,
                "Section without children should have no parent index"
            );
        }
        _ => panic!("Expected text output"),
    }
}

#[test]
fn test_parent_index_references_actual_output_position() {
    // This test verifies that parent_index actually references the correct position
    // in the final output array, not some reset chunk index
    let mut doc_builder = DocumentBuilder::new();

    // Create multiple sections with multiple chunks each to test index behavior
    let _main_text = doc_builder.add_text(1, "Main section text", 12.0, 100.0, 700.0);
    let _sub1_text = doc_builder.add_text(1, "First subsection text", 12.0, 100.0, 650.0);
    let _sub2_text = doc_builder.add_text(1, "Second subsection text", 12.0, 100.0, 600.0);
    let _nested_text = doc_builder.add_text(1, "Nested text chunk", 12.0, 100.0, 550.0);

    let index = doc_builder.build();

    // Create template structure: MainSection -> [SubSection1, SubSection2 -> NestedChunk]
    let main_section = Element::new("MainSection".to_string(), ElementType::Section);
    let sub_section1 = Element::new("SubSection1".to_string(), ElementType::Section);
    let sub_section2 = Element::new("SubSection2".to_string(), ElementType::Section);

    let mut nested_chunk = Element::new("NestedChunk".to_string(), ElementType::TextChunk);
    nested_chunk
        .attributes
        .insert("chunkSize".to_string(), Value::Number(100));

    let main_section_match = TemplateContentMatch {
        template_element: &main_section,
        matched_content: vec![MatchedContent::Index(0)], // main_text
        children: vec![
            TemplateContentMatch {
                template_element: &sub_section1,
                matched_content: vec![MatchedContent::Index(1)], // sub1_text
                children: vec![],
                metadata: HashMap::new(),
                section_boundaries: None,
            },
            TemplateContentMatch {
                template_element: &sub_section2,
                matched_content: vec![MatchedContent::Index(2)], // sub2_text
                children: vec![TemplateContentMatch {
                    template_element: &nested_chunk,
                    matched_content: vec![MatchedContent::Index(3)], // nested_text
                    children: vec![],
                    metadata: HashMap::new(),
                    section_boundaries: None,
                }],
                metadata: HashMap::new(),
                section_boundaries: None,
            },
        ],
        metadata: HashMap::new(),
        section_boundaries: None,
    };

    let matches = vec![main_section_match];
    let outputs = process_matched_content(&matches, &index, None);

    // Debug output to see the actual structure
    println!("=== Parent Index Reference Test ===");
    for (i, output) in outputs.iter().enumerate() {
        match output {
            ProcessedOutput::Text(chunk) => {
                println!(
                    "Output[{}]: '{}' (chunk_index: {}) -> parent: {:?} @ {:?}",
                    i, chunk.text, chunk.chunk_index, chunk.parent_name, chunk.parent_index
                );
            }
            ProcessedOutput::Image(img) => {
                println!("Output[{}]: Image {}", i, img.id);
            }
        }
    }

    // Expected structure:
    // 0: MainSection text (no parent)
    // 1: SubSection1 text (parent: MainSection @ 0)
    // 2: SubSection2 text (parent: MainSection @ 0)
    // 3: NestedChunk text (parent: SubSection2 @ 2)

    assert_eq!(outputs.len(), 4, "Should have 4 outputs");

    let text_outputs: Vec<_> = outputs
        .iter()
        .filter_map(|output| match output {
            ProcessedOutput::Text(chunk) => Some(chunk),
            _ => None,
        })
        .collect();

    // Verify the main section (output 0)
    let main_output = &text_outputs[0];
    assert!(main_output.text.contains("Main section text"));
    assert_eq!(main_output.parent_name, None);
    assert_eq!(main_output.parent_index, None);

    // Verify first subsection (output 1)
    let sub1_output = &text_outputs[1];
    assert!(sub1_output.text.contains("First subsection text"));
    assert_eq!(sub1_output.parent_name, Some("MainSection".to_string()));
    assert_eq!(
        sub1_output.parent_index,
        Some(0),
        "SubSection1 should reference MainSection at output index 0"
    );

    // Verify second subsection (output 2)
    let sub2_output = &text_outputs[2];
    assert!(sub2_output.text.contains("Second subsection text"));
    assert_eq!(sub2_output.parent_name, Some("MainSection".to_string()));
    assert_eq!(
        sub2_output.parent_index,
        Some(0),
        "SubSection2 should reference MainSection at output index 0"
    );

    // Verify nested chunk (output 3) - this is the critical test
    let nested_output = &text_outputs[3];
    assert!(nested_output.text.contains("Nested text chunk"));
    assert_eq!(nested_output.parent_name, Some("SubSection2".to_string()));
    // Fixed: NestedChunk should correctly reference SubSection2 at output index 2
    assert_eq!(
        nested_output.parent_index,
        Some(2),
        "NestedChunk should reference SubSection2 at output index 2"
    );

    // Additional verification: chunk indices should be unique within the overall output
    let chunk_indices: Vec<usize> = text_outputs.iter().map(|chunk| chunk.chunk_index).collect();
    println!("Chunk indices: {:?}", chunk_indices);

    // Check if chunk indices are globally unique or reset per section
    let unique_chunk_indices: std::collections::HashSet<usize> =
        chunk_indices.iter().cloned().collect();
    if chunk_indices.len() != unique_chunk_indices.len() {
        println!("WARNING: Chunk indices are not unique! This suggests indices reset per section.");
        println!("This makes parent_index references unreliable.");
    }
}

#[test]
fn test_hierarchical_parent_reference_inconsistency() {
    // This test demonstrates the parent reference inconsistency issue
    // where elements at different levels don't consistently reference their immediate parent
    let mut doc_builder = DocumentBuilder::new();

    // Create content for a deep hierarchy: Root -> Level1 -> Level2 -> TextChunk
    let _root_text = doc_builder.add_text(1, "Root section content", 12.0, 100.0, 800.0);
    let _level1_text = doc_builder.add_text(1, "Level 1 section content", 12.0, 100.0, 750.0);
    let _level2_text = doc_builder.add_text(1, "Level 2 section content", 12.0, 100.0, 700.0);
    let _textchunk_text = doc_builder.add_text(1, "Text chunk content", 12.0, 100.0, 650.0);

    let index = doc_builder.build();

    // Create a 4-level hierarchy
    let root_section = Element::new("RootSection".to_string(), ElementType::Section);
    let level1_section = Element::new("Level1Section".to_string(), ElementType::Section);
    let level2_section = Element::new("Level2Section".to_string(), ElementType::Section);

    let mut text_chunk = Element::new("DeepTextChunk".to_string(), ElementType::TextChunk);
    text_chunk
        .attributes
        .insert("chunkSize".to_string(), Value::Number(100));

    let root_match = TemplateContentMatch {
        template_element: &root_section,
        matched_content: vec![MatchedContent::Index(0)], // root_text
        children: vec![TemplateContentMatch {
            template_element: &level1_section,
            matched_content: vec![MatchedContent::Index(1)], // level1_text
            children: vec![TemplateContentMatch {
                template_element: &level2_section,
                matched_content: vec![MatchedContent::Index(2)], // level2_text
                children: vec![TemplateContentMatch {
                    template_element: &text_chunk,
                    matched_content: vec![MatchedContent::Index(3)], // textchunk_text
                    children: vec![],
                    metadata: HashMap::new(),
                    section_boundaries: None,
                }],
                metadata: HashMap::new(),
                section_boundaries: None,
            }],
            metadata: HashMap::new(),
            section_boundaries: None,
        }],
        metadata: HashMap::new(),
        section_boundaries: None,
    };

    let matches = vec![root_match];
    let outputs = process_matched_content(&matches, &index, None);

    println!("=== Hierarchical Parent Reference Test ===");
    for (i, output) in outputs.iter().enumerate() {
        match output {
            ProcessedOutput::Text(chunk) => {
                println!(
                    "Output[{}]: '{}' -> parent: {:?} @ {:?}",
                    i, chunk.text, chunk.parent_name, chunk.parent_index
                );
            }
            ProcessedOutput::Image(img) => {
                println!("Output[{}]: Image {}", i, img.id);
            }
        }
    }

    // Expected outputs:
    // 0: RootSection content (no parent)
    // 1: Level1Section content (parent: RootSection @ 0)
    // 2: Level2Section content (parent: Level1Section @ 1)
    // 3: DeepTextChunk content (parent: Level2Section @ 2)

    let text_outputs: Vec<_> = outputs
        .iter()
        .filter_map(|output| match output {
            ProcessedOutput::Text(chunk) => Some(chunk),
            _ => None,
        })
        .collect();

    assert_eq!(text_outputs.len(), 4, "Should have 4 outputs");

    // Level 0: Root section (no parent)
    let root_output = &text_outputs[0];
    assert!(root_output.text.contains("Root section content"));
    assert_eq!(root_output.parent_name, None);
    assert_eq!(root_output.parent_index, None);

    // Level 1: Should reference Root at index 0
    let level1_output = &text_outputs[1];
    assert!(level1_output.text.contains("Level 1 section content"));
    assert_eq!(level1_output.parent_name, Some("RootSection".to_string()));
    assert_eq!(level1_output.parent_index, Some(0));

    // Level 2: Should reference Level1 at index 1 (immediate parent)
    let level2_output = &text_outputs[2];
    assert!(level2_output.text.contains("Level 2 section content"));
    assert_eq!(level2_output.parent_name, Some("Level1Section".to_string()));
    // THIS IS THE CRITICAL TEST: Does Level2 reference Level1 (index 1) or Root (index 0)?
    assert_eq!(
        level2_output.parent_index,
        Some(1),
        "Level2 should reference Level1 at index 1, not Root"
    );

    // Level 3: TextChunk should reference Level2 at index 2 (immediate parent)
    let textchunk_output = &text_outputs[3];
    assert!(textchunk_output.text.contains("Text chunk content"));
    assert_eq!(
        textchunk_output.parent_name,
        Some("Level2Section".to_string())
    );
    // THIS IS ANOTHER CRITICAL TEST: Does TextChunk reference Level2 (index 2) correctly?
    assert_eq!(
        textchunk_output.parent_index,
        Some(2),
        "TextChunk should reference Level2 at index 2"
    );

    // Additional test: Verify we can traverse the tree upward
    // textchunk (3) -> level2 (2) -> level1 (1) -> root (0) -> None
    println!("\n=== Tree Traversal Test ===");
    println!(
        "TextChunk[3] parent -> Level2[{}]",
        textchunk_output.parent_index.unwrap()
    );
    println!(
        "Level2[2] parent -> Level1[{}]",
        level2_output.parent_index.unwrap()
    );
    println!(
        "Level1[1] parent -> Root[{}]",
        level1_output.parent_index.unwrap()
    );
    println!("Root[0] parent -> {:?}", root_output.parent_index);
}

#[test]
fn test_real_nested_sections_parent_references() {
    use delver_core::docql::process_matched_content;
    use delver_core::matcher::align_template_with_content;

    // Build document content with realistic nested structure
    let mut doc = DocumentBuilder::new();

    // Main section content
    let main_section_header_id =
        doc.add_text(1, "Management Discussion and Analysis", 16.0, 50.0, 750.0);
    let main_section_content_id =
        doc.add_text(1, "This is the main MD&A content.", 12.0, 50.0, 730.0);

    // First subsection
    let sub1_header_id = doc.add_text(1, "Financial Performance Overview", 14.0, 60.0, 700.0);
    let sub1_content_id =
        doc.add_text(1, "Details about financial performance.", 12.0, 60.0, 680.0);

    // Second subsection with nested content
    let sub2_header_id = doc.add_text(1, "Risk Factors Analysis", 14.0, 60.0, 650.0);
    let sub2_content_id = doc.add_text(1, "Analysis of various risk factors.", 12.0, 60.0, 630.0);
    let nested_content_id = doc.add_text(1, "Detailed risk assessment data.", 12.0, 70.0, 610.0);

    // End marker
    let next_section_id = doc.add_text(1, "Quantitative Disclosures", 16.0, 50.0, 580.0);

    let index = doc.build();

    // Build template with proper nested structure
    let template = TemplateBuilder::new()
        .add_section("MDandA")
        .match_pattern("Management Discussion and Analysis")
        .end_match("Quantitative Disclosures")
        .as_name("MD&A Section")
        .with_child_section("FinancialPerformance")
        .match_pattern("Financial Performance Overview")
        .end_match("Risk Factors Analysis")
        .build()
        .with_child_section("RiskFactors")
        .match_pattern("Risk Factors Analysis")
        .end_match("Quantitative Disclosures")
        .build()
        .build()
        .build();

    // Execute proper template matching
    let template_matches = align_template_with_content(&template, &index, None, None)
        .expect("Should find nested section matches");

    // Process the matched content to get the actual output with parent references
    let processed_outputs = process_matched_content(&template_matches, &index, None);

    // Debug: Print what we actually got
    println!("=== Real Template Matching Test ===");
    for (i, output) in processed_outputs.iter().enumerate() {
        match output {
            delver_core::docql::ProcessedOutput::Text(chunk) => {
                println!(
                    "Output[{}]: '{}' -> parent: {:?} @ {:?}",
                    i,
                    chunk.text.chars().take(50).collect::<String>(),
                    chunk.parent_name,
                    chunk.parent_index
                );
            }
            delver_core::docql::ProcessedOutput::Image(img) => {
                println!("Output[{}]: Image {}", i, img.id);
            }
        }
    }

    // Extract text outputs for analysis
    let text_outputs: Vec<_> = processed_outputs
        .iter()
        .filter_map(|output| match output {
            delver_core::docql::ProcessedOutput::Text(chunk) => Some(chunk),
            _ => None,
        })
        .collect();

    // We should have outputs for: main section, financial performance subsection, risk factors subsection
    assert!(
        text_outputs.len() >= 3,
        "Should have at least 3 text outputs, got {}",
        text_outputs.len()
    );

    // Find the main section output (should have no parent)
    let main_section_output = text_outputs
        .iter()
        .find(|chunk| chunk.text.contains("main MD&A content"))
        .expect("Should find main section content");

    assert_eq!(
        main_section_output.parent_name, None,
        "Main section should have no parent"
    );
    assert_eq!(
        main_section_output.parent_index, None,
        "Main section should have no parent index"
    );

    // Find the financial performance output (should reference main section)
    let financial_output = text_outputs
        .iter()
        .find(|chunk| chunk.text.starts_with("Financial Performance Overview"))
        .expect("Should find financial performance content");

    println!(
        "Financial performance parent: {:?} @ {:?}",
        financial_output.parent_name, financial_output.parent_index
    );

    // Find the risk factors output (should reference main section, NOT financial performance)
    let risk_output = text_outputs
        .iter()
        .find(|chunk| chunk.text.starts_with("Risk Factors Analysis"))
        .expect("Should find risk factors content");

    println!(
        "Risk factors parent: {:?} @ {:?}",
        risk_output.parent_name, risk_output.parent_index
    );

    // This is the critical test: both subsections should reference the main section
    // If there's inconsistency, one might reference the main section while another references a different parent

    // Check if we can find the main section's output index
    let main_section_index = text_outputs
        .iter()
        .position(|chunk| chunk.parent_name.is_none() && chunk.text.contains("main MD&A content"))
        .expect("Should find main section output index");

    // Both subsections should reference the main section consistently
    if let Some(financial_parent_idx) = financial_output.parent_index {
        assert_eq!(
            financial_parent_idx, main_section_index,
            "Financial performance should reference main section at index {}, but got {}",
            main_section_index, financial_parent_idx
        );
    }

    if let Some(risk_parent_idx) = risk_output.parent_index {
        assert_eq!(
            risk_parent_idx, main_section_index,
            "Risk factors should reference main section at index {}, but got {}",
            main_section_index, risk_parent_idx
        );
    }

    // Verify parent names are consistent
    assert_eq!(
        financial_output.parent_name,
        Some("MDandA".to_string()),
        "Financial performance should have MDandA as parent name"
    );
    assert_eq!(
        risk_output.parent_name,
        Some("MDandA".to_string()),
        "Risk factors should have MDandA as parent name"
    );
}

#[test]
fn test_10k_template_structure_parent_references() {
    use delver_core::docql::process_matched_content;
    use delver_core::matcher::align_template_with_content;

    // Build document content that matches the 10k_1.tmpl structure
    let mut doc = DocumentBuilder::new();

    // Root level content (should be picked up by root TextChunk)
    let _root_content1 = doc.add_text(
        1,
        "This is some root level content before MD&A.",
        12.0,
        50.0,
        900.0,
    );
    let _root_content2 = doc.add_text(1, "More root content here.", 12.0, 50.0, 880.0);

    // MD&A section header and content
    let _mda_header = doc.add_text(
        1,
        "Management's Discussion and Analysis of Financial Condition and Results of Operations",
        16.0,
        50.0,
        850.0,
    );
    let _mda_content1 = doc.add_text(1, "This is MD&A section content.", 12.0, 50.0, 830.0);
    let _mda_content2 = doc.add_text(1, "More MD&A content here.", 12.0, 50.0, 810.0);

    // Performance by Business Segment subsection
    let _perf_header = doc.add_text(1, "PERFORMANCE BY BUSINESS SEGMENT", 14.0, 50.0, 780.0);
    let _perf_content1 = doc.add_text(1, "Performance by segment analysis.", 12.0, 50.0, 760.0);
    let _perf_content2 = doc.add_text(1, "Detailed segment performance data.", 12.0, 50.0, 740.0);

    // End marker for MD&A
    let _end_marker = doc.add_text(
        1,
        "Quantitative and Qualitative Disclosures About Market Risk",
        16.0,
        50.0,
        700.0,
    );

    let index = doc.build();

    // Build template that matches 10k_1.tmpl structure
    let template = TemplateBuilder::new()
        .add_textchunk("RootTextChunk", 500, 150)
        .add_section("MDandA")
        .match_pattern(
            "Management's Discussion and Analysis of Financial Condition and Results of Operations",
        )
        .end_match("Quantitative and Qualitative Disclosures About Market Risk")
        .as_name("MD&A")
        .with_textchunk("MDATextChunk", 500, 150)
        .with_child_section("PerformanceByBusinessSegment")
        .match_pattern("PERFORMANCE BY BUSINESS SEGMENT")
        .end_match("Quantitative and Qualitative Disclosures About Market Risk")
        .build()
        .build()
        .build();

    // Execute proper template matching
    let template_matches = align_template_with_content(&template, &index, None, None)
        .expect("Should find template matches");

    // Process the matched content to get the actual output with parent references
    let processed_outputs = process_matched_content(&template_matches, &index, None);

    // Debug: Print what we actually got (limit to first 10 to avoid spam)
    println!("=== 10K Template Structure Test ===");
    for (i, output) in processed_outputs.iter().take(10).enumerate() {
        match output {
            delver_core::docql::ProcessedOutput::Text(chunk) => {
                println!(
                    "Output[{}]: '{}' -> parent: {:?} @ {:?}",
                    i,
                    chunk.text.chars().take(50).collect::<String>(),
                    chunk.parent_name,
                    chunk.parent_index
                );
            }
            delver_core::docql::ProcessedOutput::Image(img) => {
                println!("Output[{}]: Image {}", i, img.id);
            }
        }
    }

    // Extract text outputs for analysis
    let text_outputs: Vec<_> = processed_outputs
        .iter()
        .filter_map(|output| match output {
            delver_core::docql::ProcessedOutput::Text(chunk) => Some(chunk),
            _ => None,
        })
        .collect();

    // We should have at least 3 outputs: root content, MD&A content, Performance content
    assert!(
        text_outputs.len() >= 3,
        "Should have at least 3 text outputs, got {}",
        text_outputs.len()
    );

    // Find the root-level content (should have no parent)
    let root_output = text_outputs
        .iter()
        .find(|chunk| chunk.text.contains("root level content"))
        .expect("Should find root level content");

    assert_eq!(
        root_output.parent_name, None,
        "Root level content should have no parent"
    );
    assert_eq!(
        root_output.parent_index, None,
        "Root level content should have no parent index"
    );

    // Find the MD&A section content (should have no parent - it's a root-level section)
    let mda_output = text_outputs
        .iter()
        .find(|chunk| chunk.text.contains("MD&A section content"))
        .expect("Should find MD&A section content");

    assert_eq!(
        mda_output.parent_name, None,
        "MD&A section content should have no parent (root-level section)"
    );
    assert_eq!(
        mda_output.parent_index, None,
        "MD&A section content should have no parent index"
    );

    // Find the Performance by Business Segment content (should have MD&A as parent)
    let perf_output = text_outputs
        .iter()
        .find(|chunk| chunk.text.starts_with("PERFORMANCE BY BUSINESS SEGMENT"))
        .expect("Should find performance segment content");

    // This is the critical test: Performance content should reference MD&A section
    assert_eq!(
        perf_output.parent_name,
        Some("MDandA".to_string()),
        "Performance segment content should have MDandA as parent name"
    );

    // Find the MD&A section's output index to verify parent_index
    let mda_section_index = text_outputs
        .iter()
        .position(|chunk| {
            chunk.parent_name.is_none() && chunk.text.contains("MD&A section content")
        })
        .expect("Should find MD&A section output index");

    assert_eq!(
        perf_output.parent_index,
        Some(mda_section_index),
        "Performance segment content should reference MD&A section at index {}, but got {:?}",
        mda_section_index,
        perf_output.parent_index
    );

    println!("✅ Parent references are correct:");
    println!("  - Root content: no parent");
    println!("  - MD&A content: no parent (root-level section)");
    println!(
        "  - Performance content: parent = MD&A @ index {}",
        mda_section_index
    );
}
