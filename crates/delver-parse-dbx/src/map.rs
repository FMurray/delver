//! ai_parse_document (output schema 2.0) → `delver_core::parse::ParsedDocument`
//! mapper (DA-008). The mapped document rides the existing
//! `DelverStore::ingest_parsed` path, so ai-parse output lands in exactly the
//! same element rows / `table_cells` / metadata shapes as native parses — but
//! a given document is parsed by exactly one engine per parse_version (the
//! D-008 dedup key makes re-ingest under another engine a no-op).
//!
//! Mapping (full type list per the published schema):
//! * `text`, `title`, `caption`, `section_header`, `page_header`,
//!   `page_footer`, `page_number`, `footnote` → kind=text rows (page + bbox;
//!   no font information exists in ai_parse output, so font_size/font_name
//!   stay empty and the fine-grained type tag is dropped — `TextElement`
//!   carries no metadata slot; recorded as a DA decision).
//! * `table` → kind=table + `table_cells` (HTML content parsed into a grid
//!   with rowspan/colspan; per-cell geometry does not exist in ai_parse
//!   output, so cell bboxes are zero).
//! * `figure` → kind=figure; the AI description (when generated) becomes the
//!   element text so it is full-text searchable.
//!
//! Coordinates: ai_parse bboxes are pixel coordinates of the rendered page
//! image, top-left origin — the same orientation as delver's page
//! coordinates but a different unit. They are stored as-is; the parser
//! provenance block records `"bbox_space": "pixels"`.
//!
//! Fail-loud (D-006): non-empty `error_status`, unsupported schema versions,
//! unknown element types, missing pages/bboxes, and unparseable table HTML
//! are hard errors naming the offending element.

use std::collections::BTreeMap;

use serde_json::Value;
use uuid::Uuid;

use delver_core::geo::Rect;
use delver_core::parse::{AuxElement, AuxKind, PageContents, ParsedDocument, TextElement};
use delver_core::table::{TableCell, TableStrategy, TableStructure};

use crate::html_table::parse_html_table;
use crate::ParseDbxError;

/// Map one ai_parse_document response (the VARIANT JSON, schema 2.x) onto a
/// [`ParsedDocument`]. `ParsedDocument.metadata` carries the parser
/// provenance; callers merge any additional metadata (e.g. the Part-1 scan
/// block in `auto` mode) before ingest.
pub fn map_ai_parse_response(parsed: &Value) -> Result<ParsedDocument, ParseDbxError> {
    // Partial parses are data corruption, not a warning (D-006).
    if let Some(errors) = parsed.get("error_status").and_then(Value::as_array) {
        if !errors.is_empty() {
            let details: Vec<String> = errors
                .iter()
                .map(|e| {
                    format!(
                        "page {}: {}",
                        e.get("page_id").cloned().unwrap_or(Value::Null),
                        e.get("error_message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                    )
                })
                .collect();
            return Err(ParseDbxError(format!(
                "ai_parse_document reported {} page error(s): {}",
                details.len(),
                details.join("; ")
            )));
        }
    }

    let version = parsed
        .pointer("/metadata/version")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !version.starts_with("2.") {
        return Err(ParseDbxError(format!(
            "unsupported ai_parse_document output schema version {version:?} \
             (this build understands 2.x; minor versions are \
             backward-compatible per the published contract)"
        )));
    }

    let mut pages: BTreeMap<u32, PageContents> = BTreeMap::new();
    // Every page the parser saw exists, elements or not, so page_count is
    // faithful (pages[].id is 0-based; delver pages are 1-based).
    if let Some(page_list) = parsed.pointer("/document/pages").and_then(Value::as_array) {
        for page in page_list {
            if let Some(id) = page.get("id").and_then(Value::as_u64) {
                pages.entry(id as u32 + 1).or_insert_with(PageContents::new);
            }
        }
    }

    let elements = parsed
        .pointer("/document/elements")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ParseDbxError(
                "ai_parse_document output has no document.elements array".to_string(),
            )
        })?;

    for element in elements {
        let id = element.get("id").cloned().unwrap_or(Value::Null);
        let kind = element
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| ParseDbxError(format!("element {id} has no type")))?;
        let (page_number, bbox) = element_placement(element, &id)?;
        let content = element.get("content").and_then(Value::as_str);
        let confidence = element.get("confidence").and_then(Value::as_f64);
        let page = pages.entry(page_number).or_insert_with(PageContents::new);

        match kind {
            // The textual types all become matchable, FTS-able text rows.
            "text" | "title" | "caption" | "section_header" | "page_header" | "page_footer"
            | "page_number" | "footnote" => {
                page.add_text(TextElement {
                    id: Uuid::new_v4(),
                    text: content.unwrap_or_default().to_string(),
                    font_size: 0.0,
                    font_name: None,
                    bbox,
                    page_number,
                });
            }
            "figure" => {
                let description = element
                    .get("description")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                let mut metadata = serde_json::Map::new();
                metadata.insert("source".into(), "ai_parse_document".into());
                if let Some(description) = description {
                    metadata.insert("description".into(), description.into());
                }
                if let Some(confidence) = confidence {
                    metadata.insert("confidence".into(), confidence.into());
                }
                page.add_aux(AuxElement {
                    id: Uuid::new_v4(),
                    kind: AuxKind::Figure,
                    page_number,
                    bbox: rect(bbox),
                    // The AI description doubles as searchable text.
                    text: description.map(str::to_string),
                    metadata: Value::Object(metadata),
                    blob: None,
                    table: None,
                });
            }
            "table" => {
                let html = content.ok_or_else(|| {
                    ParseDbxError(format!("table element {id} has no HTML content"))
                })?;
                let grid = parse_html_table(html)
                    .map_err(|e| ParseDbxError(format!("table element {id}: {e}")))?;
                let cells: Vec<TableCell> = grid
                    .cells
                    .iter()
                    .map(|c| TableCell {
                        row: c.row,
                        col: c.col,
                        row_span: c.row_span,
                        col_span: c.col_span,
                        // ai_parse provides no per-cell geometry.
                        bbox: (0.0, 0.0, 0.0, 0.0),
                        text: c.text.clone(),
                        is_header: c.is_header,
                    })
                    .collect();
                let table = TableStructure {
                    bbox: rect(bbox),
                    page: page_number,
                    n_rows: grid.n_rows,
                    n_cols: grid.n_cols,
                    cells,
                    strategy: TableStrategy::AiParse,
                    confidence: confidence.unwrap_or(0.0),
                };
                let mut metadata = table.element_metadata();
                if let Some(map) = metadata.as_object_mut() {
                    map.insert("source".into(), "ai_parse_document".into());
                }
                page.add_aux(AuxElement {
                    id: Uuid::new_v4(),
                    kind: AuxKind::Table,
                    page_number,
                    bbox: rect(bbox),
                    text: None,
                    metadata,
                    blob: None,
                    table: Some(table),
                });
            }
            other => {
                return Err(ParseDbxError(format!(
                    "element {id} has unknown ai_parse_document type {other:?} \
                     (known: text, table, figure, title, caption, \
                     section_header, page_header, page_footer, page_number, \
                     footnote) — schema may have moved; refusing to guess"
                )));
            }
        }
    }

    Ok(ParsedDocument {
        pages,
        refs: Vec::new(),
        metadata: provenance_metadata(parsed),
    })
}

/// Parser provenance for `documents.metadata` (DA-008): which engine, which
/// output schema version, the run id the response exposes, and the bbox
/// coordinate space.
fn provenance_metadata(parsed: &Value) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("parser".into(), "ai_parse_document".into());
    if let Some(version) = parsed.pointer("/metadata/version").and_then(Value::as_str) {
        map.insert("parser_version".into(), version.into());
    }
    if let Some(run_id) = parsed.pointer("/metadata/id").and_then(Value::as_str) {
        map.insert("parser_run_id".into(), run_id.into());
    }
    map.insert("bbox_space".into(), "pixels".into());
    Value::Object(map)
}

/// Page (1-based) and bbox tuple from an element's first bbox entry.
/// Elements spanning pages keep their first placement (document-level
/// retrieval cares about a stable anchor, not the continuation).
fn element_placement(
    element: &Value,
    id: &Value,
) -> Result<(u32, (f32, f32, f32, f32)), ParseDbxError> {
    let first = element
        .pointer("/bbox/0")
        .ok_or_else(|| ParseDbxError(format!("element {id} has no bbox entries")))?;
    let page_id = first
        .get("page_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| ParseDbxError(format!("element {id} bbox has no page_id")))?;
    let coord = first
        .get("coord")
        .and_then(Value::as_array)
        .ok_or_else(|| ParseDbxError(format!("element {id} bbox has no coord array")))?;
    if coord.len() != 4 {
        return Err(ParseDbxError(format!(
            "element {id} bbox coord has {} values (expected 4: x0,y0,x1,y1)",
            coord.len()
        )));
    }
    let n = |i: usize| coord[i].as_f64().unwrap_or(0.0) as f32;
    Ok((page_id as u32 + 1, (n(0), n(1), n(2), n(3))))
}

fn rect(bbox: (f32, f32, f32, f32)) -> Rect {
    Rect {
        x0: bbox.0,
        y0: bbox.1,
        x1: bbox.2,
        y1: bbox.3,
    }
}
