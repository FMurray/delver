pub mod chunker;
pub mod debug_viewer;
pub mod dom;
pub mod fonts;
pub mod geo;
pub mod layout;
pub mod logging;
pub mod matcher;
pub mod parse;

use crate::dom::{parse_template, process_matched_content, ChunkOutput};
use crate::layout::{group_text_into_lines_and_blocks, MatchContext, TextBlock, TextLine};
use crate::matcher::align_template_with_content;
use crate::parse::{get_pdf_text, get_refs};
use logging::{PDF_TEXT_BLOCK, PDF_TEXT_OBJECT};
use lopdf::Document;
use std::collections::HashMap;
use tracing::event;

#[cfg(feature = "extension-module")]
use pyo3::prelude::*;

/// Process a PDF document using a template and return chunks as JSON
///
/// # Arguments
/// * `pdf_bytes` - The PDF file contents as bytes
/// * `template_str` - The template string to use for processing
///
/// # Returns
/// * `Result<String, Box<dyn std::error::Error>>` - JSON string containing the chunks
pub fn process_pdf(
    pdf_bytes: &[u8],
    template_str: &str,
) -> Result<(String, Vec<TextBlock>, Document), Box<dyn std::error::Error>> {
    // 1. Parse the template
    let dom = parse_template(template_str)?;

    // 2. Load and parse the PDF
    let doc = Document::load_mem(pdf_bytes)?;
    let pages_map = get_pdf_text(&doc)?;

    // 3. Get the document context for matching
    let match_context = get_refs(&doc)?;

    // 4. Group text elements into lines and blocks
    let line_join_threshold = 5.0;
    let block_join_threshold = 12.0;
    let blocks =
        group_text_into_lines_and_blocks(&pages_map, line_join_threshold, block_join_threshold);

    // 5. Extract a flat list of all lines for matching
    let text_lines: Vec<TextLine> = blocks
        .iter()
        .flat_map(|block| block.lines.clone())
        .collect();

    // 6. Get a flat list of all text elements for content extraction
    let text_elements: Vec<_> = pages_map
        .values()
        .flat_map(|elements| elements.clone())
        .collect();

    // 7. Process the template DOM against the content
    let mut all_chunks: Vec<ChunkOutput> = Vec::new();

    for template_element in &dom.elements {
        // Empty initial metadata
        let metadata = HashMap::new();

        // Match template to content
        if let Some(matched_content) = align_template_with_content(
            template_element,
            &text_lines,
            &text_elements,
            &match_context,
            &metadata,
        ) {
            // Process the matched content into chunks
            let chunks = process_matched_content(&matched_content);
            all_chunks.extend(chunks);
        }
    }

    // 8. Convert chunks to JSON
    let json = serde_json::to_string_pretty(&all_chunks)?;
    Ok((json, blocks, doc))
}

/// Process a PDF file using a template and return extracted data as JSON
#[cfg(feature = "extension-module")]
#[pyfunction]
fn process_pdf_file(pdf_path: String, template_path: String) -> PyResult<String> {
    // Read the files
    let pdf_bytes = std::fs::read(pdf_path)?;
    let template_str = std::fs::read_to_string(template_path)?;

    // Process using existing function
    let json = process_pdf(&pdf_bytes, &template_str)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;

    Ok(json)
}

/// A Python module implemented in Rust
#[cfg(feature = "extension-module")]
#[pymodule(name = "delver_pdf")]
fn delver_pdf(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(process_pdf_file, m)?)?;
    Ok(())
}
