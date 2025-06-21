pub mod chunker;
pub mod debug_viewer;
pub mod dom;
pub mod features;
pub mod fonts;
pub mod geo;
pub mod layout;
pub mod logging;
pub mod matcher;
pub mod parse;
pub mod search_index;

use crate::dom::{parse_template, process_matched_content, ChunkOutput, ProcessedOutput};
use crate::layout::{group_text_into_lines_and_blocks, TextBlock};
use crate::matcher::align_template_with_content;
use crate::parse::{get_page_content, get_refs, PageContent, TextElement};
use anyhow::Result;
use lopdf::Document;
use search_index::PdfIndex;
use std::collections::BTreeMap;

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
) -> Result<(String, Vec<TextBlock>, Document)> {
    let dom = parse_template(template_str)?;

    let doc = Document::load_mem(pdf_bytes)?;
    let pages_map = get_page_content(&doc)?;

    let mut text_pages_map: BTreeMap<u32, Vec<TextElement>> = BTreeMap::new();
    for (page_num, contents) in &pages_map {
        // Extract text elements and collect them
        let text_elements: Vec<TextElement> = contents
            .iter()
            .filter_map(|content| match content {
                PageContent::Text(text_elem) => Some(text_elem.clone()),
                _ => None,
            })
            .collect();

        if !text_elements.is_empty() {
            text_pages_map.insert(*page_num, text_elements);
        }
    }

    let line_join_threshold = 5.0;
    let block_join_threshold = 12.0;
    let blocks = group_text_into_lines_and_blocks(
        &text_pages_map,
        line_join_threshold,
        block_join_threshold,
    );

    let match_context = get_refs(&doc)?;

    let mut all_outputs: Vec<ProcessedOutput> = Vec::new();

    let index = PdfIndex::new(&pages_map, &match_context);

    if let Some(matched_content) = align_template_with_content(&dom.elements, &index, None, None) {
        let outputs = process_matched_content(&matched_content);
        all_outputs.extend(outputs);
    }

    let json = serde_json::to_string_pretty(&all_outputs)?;
    Ok((json, blocks, doc))
}

/// Process a PDF file using a template and return extracted data as JSON
#[cfg(feature = "extension-module")]
#[pyfunction]
fn process_pdf_file(pdf_path: String, template_path: String) -> PyResult<String> {
    let pdf_bytes = std::fs::read(pdf_path)?;
    let template_str = std::fs::read_to_string(template_path)?;

    let (json, _blocks, _doc) = process_pdf(&pdf_bytes, &template_str)
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
