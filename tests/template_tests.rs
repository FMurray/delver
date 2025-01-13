use delver::{
    dom::{parse_template, process_template_element, Value},
    parse::get_pdf_text,
};
use lopdf::Document;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};

mod common;

#[test]
fn test_10k_template_parsing() -> std::io::Result<()> {
    common::setup();

    // First test template parsing
    let template_str = include_str!("../10k.tmpl");
    let root = parse_template(template_str)?;

    assert!(!root.elements.is_empty());
    assert_eq!(root.elements.len(), 2); // TextChunk and Section

    // Check first element is TextChunk
    let first_element = &root.elements[0];
    assert_eq!(first_element.name, "TextChunk");
    if let Some(Value::Number(n)) = first_element.attributes.get("chunkSize") {
        assert_eq!(*n, 1000);
    }

    // Check second element is Section
    let section = &root.elements[1];
    assert_eq!(section.name, "Section");
    if let Some(Value::String(s)) = section.attributes.get("match") {
        assert_eq!(
            s,
            "Management’s Discussion and Analysis of Financial Condition and Results of Operations"
        );
    }

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_10k_template_processing() -> std::io::Result<()> {
    common::setup();

    let pdf_path = common::get_test_pdf_path();
    println!("Testing with PDF at path: {:?}", pdf_path);

    let doc = Document::load(&pdf_path).map_err(|e| {
        println!("Failed to load PDF: {}", e);
        Error::new(ErrorKind::Other, e.to_string())
    })?;

    // Parse PDF into text elements
    let text_elements = get_pdf_text(&doc).map_err(|e| {
        println!("Failed to extract text: {}", e);
        Error::new(ErrorKind::Other, e.to_string())
    })?;
    println!("Extracted {} text elements", text_elements.len());

    // Parse template
    let template_str = include_str!("../10k.tmpl");
    let root = parse_template(template_str)?;
    println!(
        "Template parsed successfully with {} root elements",
        root.elements.len()
    );

    // Process template with empty metadata
    let metadata = HashMap::new();
    let chunks = process_template_element(&root.elements[1], &text_elements, &doc, &metadata);
    println!("Processed {} chunks", chunks.len());

    // Print first few chunks for debugging
    for (i, chunk) in chunks.iter().take(3).enumerate() {
        println!(
            "Chunk {}: {} chars, metadata: {:?}",
            i,
            chunk.text.len(),
            chunk.metadata
        );
        println!("Preview: {:.100}...", chunk.text.replace('\n', " "));
    }

    // Verify we got chunks back
    assert!(!chunks.is_empty());

    // Verify the chunks contain expected content
    let contains_md_and_a = chunks.iter().any(|chunk| {
        chunk.text.contains("Management’s Discussion and Analysis")
            && chunk.text.contains("Results of Operations")
    });
    assert!(
        contains_md_and_a,
        "Chunks should contain MD&A section content"
    );

    // Verify chunk metadata
    let md_and_a_chunks = chunks
        .iter()
        .filter(|chunk| chunk.metadata.contains_key("MD&A"))
        .collect::<Vec<_>>();

    assert!(
        !md_and_a_chunks.is_empty(),
        "Should have chunks with MD&A metadata"
    );

    // Verify chunk sizes are reasonable
    for chunk in &chunks {
        assert!(
            chunk.text.len() <= 1000,
            "Chunk size should not exceed 1000 characters"
        );
    }

    common::cleanup_all();
    Ok(())
}

#[test]
fn test_nested_sections() -> std::io::Result<()> {
    common::setup();

    let pdf_path = common::get_test_pdf_path();
    let doc = Document::load(&pdf_path).map_err(|e| Error::new(ErrorKind::Other, e.to_string()))?;
    let text_elements =
        get_pdf_text(&doc).map_err(|e| Error::new(ErrorKind::Other, e.to_string()))?;

    let template_str = include_str!("../10k.tmpl");
    let root = parse_template(template_str)?;

    // Get the main MD&A section
    let md_and_a_section = &root.elements[1];
    assert_eq!(md_and_a_section.children.len(), 3); // TextChunk and 2 nested Sections

    // Process with empty metadata
    let metadata = HashMap::new();
    let chunks = process_template_element(md_and_a_section, &text_elements, &doc, &metadata);

    // Verify business segment section content
    let business_segment_chunks = chunks
        .iter()
        .filter(|chunk| chunk.text.contains("PERFORMANCE BY BUSINESS SEGMENT"))
        .collect::<Vec<_>>();

    assert!(
        !business_segment_chunks.is_empty(),
        "Should find business segment section"
    );

    // Verify geographic area section content
    let geographic_chunks = chunks
        .iter()
        .filter(|chunk| chunk.text.contains("PERFORMANCE BY GEOGRAPHIC AREA"))
        .collect::<Vec<_>>();

    assert!(
        !geographic_chunks.is_empty(),
        "Should find geographic area section"
    );

    common::cleanup_all();
    Ok(())
}
