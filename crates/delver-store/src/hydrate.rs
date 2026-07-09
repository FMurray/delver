//! Rebuild delver-core's in-memory structures from stored element rows.
//!
//! Contract (D-003): matching against a hydrated index must behave identically
//! to matching against a freshly-parsed one. We guarantee this structurally by
//! reconstructing the exact `BTreeMap<page, PageContents>` shape that
//! `delver_core::parse::get_page_content` produces (elements re-added in
//! global `order_idx` order, which preserves per-page document order) and then
//! running the *same* `PdfIndex::new` constructor the fresh-parse path uses.

use std::collections::BTreeMap;

use delver_core::geo::Rect;
use delver_core::layout::MatchContext;
use delver_core::parse::{AuxElement, BlobPayload, ImageElement, PageContents, TextElement};
use delver_core::search_index::PdfIndex;
use delver_core::table::{TableCell, TableStrategy, TableStructure};
use lopdf::{dictionary, Object, Stream};

use crate::types::{ElementKind, ElementRow};

/// Rebuild the per-page content map from stored rows.
#[tracing::instrument(name = "hydrate", skip_all, fields(rows = rows.len()))]
pub fn hydrate_pages(rows: &[ElementRow]) -> BTreeMap<u32, PageContents> {
    // Defensive: rows are stored/loaded ordered by order_idx, but hydration
    // correctness depends on it, so sort rather than assume.
    let mut ordered: Vec<&ElementRow> = rows.iter().collect();
    ordered.sort_by_key(|r| r.order_idx);

    let mut pages: BTreeMap<u32, PageContents> = BTreeMap::new();
    for row in ordered {
        let page_number = row.page as u32;
        let page = pages.entry(page_number).or_insert_with(PageContents::new);
        let bbox = row.bbox.unwrap_or((0.0, 0.0, 0.0, 0.0));

        match row.kind {
            ElementKind::Text => page.add_text(TextElement {
                id: row.id.into_uuid(),
                text: row.text.clone().unwrap_or_default(),
                font_size: row.font_size.unwrap_or(0.0),
                font_name: row.font_name.clone(),
                bbox,
                page_number,
            }),
            ElementKind::Image => page.add_image(ImageElement {
                id: row.id.into_uuid(),
                page_number,
                bbox: Rect {
                    x0: bbox.0,
                    y0: bbox.1,
                    x1: bbox.2,
                    y1: bbox.3,
                },
                image_object: rebuild_image_object(row),
            }),
            // Aux kinds (annotation/path/figure/blob D-016, table D-018)
            // round-trip verbatim: bbox/text/metadata from the element row,
            // blob bytes from the blobs table payload, table cells from the
            // table_cells payload. Detection is never re-run.
            ElementKind::Annotation
            | ElementKind::Path
            | ElementKind::Figure
            | ElementKind::Blob
            | ElementKind::Table => page.add_aux(AuxElement {
                id: row.id.into_uuid(),
                kind: row.kind.as_aux().expect("aux kinds matched above"),
                page_number,
                bbox: Rect {
                    x0: bbox.0,
                    y0: bbox.1,
                    x1: bbox.2,
                    y1: bbox.3,
                },
                text: row.text.clone(),
                metadata: row.metadata.clone(),
                blob: row.blob.as_ref().map(|b| BlobPayload {
                    data: b.data.clone(),
                    mime: b.mime.clone(),
                    filename: b.filename.clone(),
                }),
                table: (row.kind == ElementKind::Table).then(|| rebuild_table(row, bbox)),
            }),
        }
    }
    tracing::info!(
        pages = pages.len(),
        "hydrate: stored rows rebuilt into the fresh-parse page shape — no detection re-runs (D-011/D-016/D-018)"
    );
    pages
}

/// Rebuild a [`TableStructure`] from a stored table row: table-level fields
/// from the element's metadata jsonb, cells from the `table_cells` payload
/// (D-018). Missing metadata falls back to values derived from the cells so
/// older/partial rows still hydrate.
fn rebuild_table(row: &ElementRow, bbox: (f32, f32, f32, f32)) -> TableStructure {
    let cells: Vec<TableCell> = row
        .table_cells
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|c| TableCell {
            row: c.row.max(0) as u32,
            col: c.col.max(0) as u32,
            row_span: c.row_span.max(1) as u32,
            col_span: c.col_span.max(1) as u32,
            bbox: c.bbox.unwrap_or((0.0, 0.0, 0.0, 0.0)),
            text: c.text.clone().unwrap_or_default(),
            is_header: c.is_header,
        })
        .collect();

    let meta_u32 = |key: &str| -> Option<u32> {
        row.metadata.get(key).and_then(|v| v.as_u64()).map(|v| v as u32)
    };
    let n_rows = meta_u32("n_rows")
        .unwrap_or_else(|| cells.iter().map(|c| c.row + c.row_span).max().unwrap_or(0));
    let n_cols = meta_u32("n_cols")
        .unwrap_or_else(|| cells.iter().map(|c| c.col + c.col_span).max().unwrap_or(0));
    let strategy = row
        .metadata
        .get("strategy")
        .and_then(|v| v.as_str())
        .and_then(TableStrategy::parse)
        .unwrap_or(TableStrategy::Aligned);
    let confidence = row
        .metadata
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    TableStructure {
        bbox: Rect {
            x0: bbox.0,
            y0: bbox.1,
            x1: bbox.2,
            y1: bbox.3,
        },
        page: row.page as u32,
        n_rows,
        n_cols,
        cells,
        strategy,
        confidence,
    }
}

/// Rebuild a ready-to-match `PdfIndex` from stored rows.
pub fn hydrate_index(rows: &[ElementRow]) -> PdfIndex {
    let pages = hydrate_pages(rows);
    PdfIndex::new(&pages, &MatchContext::default())
}

/// Reconstitute a minimal lopdf image XObject from the stored payload.
/// Index behavior never inspects the object (only id/bbox/page), so this is a
/// faithful-enough carrier for the stored bytes and dimensions.
fn rebuild_image_object(row: &ElementRow) -> Object {
    let (width, height, data) = match &row.image {
        Some(image) => (
            image.width.unwrap_or_default() as i64,
            image.height.unwrap_or_default() as i64,
            image.data.clone(),
        ),
        None => (0, 0, Vec::new()),
    };
    Object::Stream(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => width,
            "Height" => height,
        },
        data,
    ))
}
