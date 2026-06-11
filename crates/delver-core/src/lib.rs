pub mod chunker;
pub mod docql;
pub mod fonts;
pub mod geo;
pub mod layout;
pub mod logging;
pub mod matcher;
pub mod parse;
pub mod search_index;
// pub mod viewer;

use crate::docql::{parse_template, process_matched_content, ProcessedOutput, Root};
use crate::layout::{group_text_into_lines_and_blocks, MatchContext, TextBlock};
use crate::matcher::align_template_with_content;
use crate::parse::{get_page_content, get_refs, PageContents, TextElement};
use anyhow::Result;
use lopdf::Document;
use search_index::PdfIndex;
use std::collections::BTreeMap;
use tokenizers::Tokenizer;

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
    tokenizer: Option<&Tokenizer>,
) -> Result<(String, Vec<TextBlock>, Document)> {
    let dom = parse_template(template_str)?;

    let doc = Document::load_mem(pdf_bytes)?;
    let pages_map = get_page_content(&doc)?;

    let mut text_pages_map: BTreeMap<u32, Vec<TextElement>> = BTreeMap::new();
    for (page_num, page_contents) in &pages_map {
        let text_elements = page_contents.text_elements();
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

    let json = run_template(&dom, &pages_map, &match_context, tokenizer)?;
    Ok((json, blocks, doc))
}

/// Execute a template against already-parsed page content (D-012).
///
/// This is the back half of [`process_pdf`], exposed so callers that already
/// hold a `BTreeMap<page, PageContents>` — e.g. delver-store hydrating a
/// persisted document — run the exact same index/match/chunk pipeline as a
/// fresh parse. Callers without a PDF in hand (no named destinations) pass
/// `&MatchContext::default()`.
///
/// # Returns
/// * JSON string containing the processed outputs (same payload as the JSON
///   returned by [`process_pdf`]).
pub fn process_parsed(
    pages_map: &BTreeMap<u32, PageContents>,
    match_context: &MatchContext,
    template_str: &str,
    tokenizer: Option<&Tokenizer>,
) -> Result<String> {
    let dom = parse_template(template_str)?;
    run_template(&dom, pages_map, match_context, tokenizer)
}

/// Shared template-execution core for [`process_pdf`] and [`process_parsed`]:
/// build the index, align the template, process matches, serialize.
fn run_template(
    dom: &Root,
    pages_map: &BTreeMap<u32, PageContents>,
    match_context: &MatchContext,
    tokenizer: Option<&Tokenizer>,
) -> Result<String> {
    let mut all_outputs: Vec<ProcessedOutput> = Vec::new();

    let index = PdfIndex::new(pages_map, match_context);

    if let Some(matched_content) = align_template_with_content(&dom.elements, &index, None, None) {
        let outputs = process_matched_content(&matched_content, &index, tokenizer);
        all_outputs.extend(outputs);
    }

    let json = serde_json::to_string_pretty(&all_outputs)?;
    Ok(json)
}
