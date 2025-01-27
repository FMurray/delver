pub mod chunker;
pub mod debug_viewer;
pub mod dom;
pub mod layout;
pub mod logging;
pub mod parse;

use crate::dom::{parse_template, process_template_element};
use crate::parse::{get_pdf_text, group_text_into_lines_and_blocks};
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
) -> Result<String, Box<dyn std::error::Error>> {
    // let dom = parse_template(template_str)?;
    let doc = Document::load_mem(pdf_bytes)?;
    let text_elements = get_pdf_text(&doc)?;

    let line_join_threshold = 5.0; // Example threshold in PDF units
    let block_join_threshold = 12.0; // Example threshold in PDF units
    let blocks =
        group_text_into_lines_and_blocks(&text_elements, line_join_threshold, block_join_threshold);

    // Now `blocks` is a Vec<TextBlock> with grouped lines.
    for block in blocks.iter().take(5) {
        tracing::info!(target: PDF_TEXT_BLOCK, "Block bbox: {:?}", block.bbox);
        for line in &block.lines {
            tracing::info!(target: PDF_TEXT_BLOCK, "Line bbox: {:?} text: {}", line.bbox, line.text);
        }
    }

    #[cfg(feature = "debug-viewer")]
    debug_viewer::launch_viewer(&doc, &blocks)?;

    // let mut all_chunks = Vec::new();

    // // Process the template DOM recursively and collect chunks
    // for element in dom.elements {
    //     let mut metadata = HashMap::new();
    //     all_chunks.extend(process_template_element(
    //         &element,
    //         &text_elements,
    //         &doc,
    //         &mut metadata,
    //     ));
    // }

    // // Convert chunks to JSON
    // let json = serde_json::to_string_pretty(&all_chunks)?;
    // Ok(json)
    Ok("done".to_string())
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
