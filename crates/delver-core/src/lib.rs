pub mod chunker;
pub mod diagnostics;
pub mod docql;
pub mod embed;
pub mod fonts;
pub mod geo;
pub mod layout;
pub mod logging;
pub mod matcher;
pub mod parse;
pub mod provenance;
pub mod scan;
pub mod search_index;
pub mod table;
pub mod udt;
// pub mod viewer;

use crate::diagnostics::RunDiagnostics;
use crate::docql::{parse_template, process_matched_content_with_provenance, ProcessedOutput, Root};
use crate::layout::{group_text_into_lines_and_blocks, MatchContext, TextBlock};
use crate::matcher::align_template_with_content_diag;
use crate::parse::{get_refs, parse_document, PageContents, TextElement};
use crate::provenance::RunProvenance;
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
/// * `embedder` - Embedding backend for `EmbeddingSim(...)` matches; pass
///   `None` when the template uses no embedding matches (using one anyway is
///   a hard error at match time, D-006)
///
/// # Returns
/// * `Result<String, Box<dyn std::error::Error>>` - JSON string containing the chunks
pub fn process_pdf(
    pdf_bytes: &[u8],
    template_str: &str,
    tokenizer: Option<&Tokenizer>,
    embedder: Option<std::sync::Arc<dyn embed::Embedder>>,
) -> Result<(String, Vec<TextBlock>, Document)> {
    let (json, blocks, doc, _diagnostics) =
        process_pdf_with_diagnostics(pdf_bytes, template_str, tokenizer, embedder)?;
    Ok((json, blocks, doc))
}

/// [`process_pdf`] plus the run's [`RunDiagnostics`] (D-024): match configs
/// that yielded zero candidates, each with its top-3 near misses. The JSON
/// payload is identical to [`process_pdf`]'s.
pub fn process_pdf_with_diagnostics(
    pdf_bytes: &[u8],
    template_str: &str,
    tokenizer: Option<&Tokenizer>,
    embedder: Option<std::sync::Arc<dyn embed::Embedder>>,
) -> Result<(String, Vec<TextBlock>, Document, RunDiagnostics)> {
    let dom = parse_template(template_str)?;

    let doc = Document::load_mem(pdf_bytes)?;
    // Full parse (D-016): content-stream walk plus annotations, paths,
    // figure grouping, and embedded files — identical to the ingest path.
    let pages_map = parse_document(&doc)?.pages;

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

    let mut match_context = get_refs(&doc)?;
    match_context.embedder = embed::SharedEmbedder::from(embedder);

    let mut diagnostics = RunDiagnostics::default();
    let (json, _provenance) =
        run_template(&dom, &pages_map, &match_context, tokenizer, &mut diagnostics)?;
    Ok((json, blocks, doc, diagnostics))
}

/// Parse PDF bytes end to end (load + [`parse::parse_document`]) without
/// running a template — for callers that need the `ParsedDocument` itself,
/// e.g. engine routing on the scan classification (slice P1) followed by
/// `ingest_parsed`.
pub fn parse_pdf_bytes(pdf_bytes: &[u8]) -> Result<parse::ParsedDocument> {
    let doc = Document::load_mem(pdf_bytes)?;
    Ok(parse_document(&doc)?)
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
    let (json, _diagnostics) =
        process_parsed_with_diagnostics(pages_map, match_context, template_str, tokenizer)?;
    Ok(json)
}

/// [`process_parsed`] plus the run's [`RunDiagnostics`] (D-024): match
/// configs that yielded zero candidates, each with its top-3 near misses.
/// The JSON payload is identical to [`process_parsed`]'s; an all-matching
/// run returns an empty diagnostics value.
pub fn process_parsed_with_diagnostics(
    pages_map: &BTreeMap<u32, PageContents>,
    match_context: &MatchContext,
    template_str: &str,
    tokenizer: Option<&Tokenizer>,
) -> Result<(String, RunDiagnostics)> {
    let (json, diagnostics, _provenance) =
        process_parsed_with_provenance(pages_map, match_context, template_str, tokenizer)?;
    Ok((json, diagnostics))
}

/// [`process_parsed_with_diagnostics`] plus the run's provenance sidecar
/// (D-025): one [`provenance::OutputProvenance`] per output, index-aligned
/// with the outputs array, carrying source element ids/pages and section
/// page spans. The JSON payload stays byte-identical to
/// [`process_parsed`]'s — the sidecar is a separate value, never serialized
/// into the outputs.
pub fn process_parsed_with_provenance(
    pages_map: &BTreeMap<u32, PageContents>,
    match_context: &MatchContext,
    template_str: &str,
    tokenizer: Option<&Tokenizer>,
) -> Result<(String, RunDiagnostics, RunProvenance)> {
    let dom = parse_template(template_str)?;
    let mut diagnostics = RunDiagnostics::default();
    let (json, provenance) =
        run_template(&dom, pages_map, match_context, tokenizer, &mut diagnostics)?;
    Ok((json, diagnostics, provenance))
}

/// Shared template-execution core for [`process_pdf`] and [`process_parsed`]:
/// build the index, align the template, process matches, serialize. The
/// provenance sidecar rides alongside the serialized outputs (D-025).
fn run_template(
    dom: &Root,
    pages_map: &BTreeMap<u32, PageContents>,
    match_context: &MatchContext,
    tokenizer: Option<&Tokenizer>,
    diagnostics: &mut RunDiagnostics,
) -> Result<(String, RunProvenance)> {
    let mut all_outputs: Vec<ProcessedOutput> = Vec::new();
    let mut provenance = RunProvenance::default();

    let index = PdfIndex::new(pages_map, match_context);

    if let Some(matched_content) =
        align_template_with_content_diag(&dom.elements, &index, None, None, diagnostics)?
    {
        let (outputs, sidecar) =
            process_matched_content_with_provenance(&matched_content, &index, tokenizer)?;
        all_outputs.extend(outputs);
        provenance.outputs.extend(sidecar);
    }

    let json = serde_json::to_string_pretty(&all_outputs)?;
    Ok((json, provenance))
}
