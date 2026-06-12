//! Viewer service layer over the persistent Postgres index (delver-store).
//!
//! The SQLite page-image store is retired (DV-001): Postgres is the source of
//! truth for documents/elements, original PDF bytes live in a local byte-cache
//! directory (`DELVER_DOC_CACHE`, DV-002), and page rasters are produced on
//! demand with pdfium and held in a small in-process LRU (DV-003) — no page
//! images are ever written to Postgres.
//!
//! Shapes shared with the WASM client (DTOs) live at the top of this module;
//! everything that touches the database, the filesystem, or pdfium is gated
//! behind the `ssr` feature.

use serde::{Deserialize, Serialize};

/// One row of the document list: `documents` joined with `corpora`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentSummary {
    pub id: String,
    pub corpus: String,
    /// Display name: PDF Info title > URI basename > short id (DV-002).
    pub name: String,
    pub uri: Option<String>,
    pub page_count: i32,
    pub parse_version: i32,
    pub parsed_at: chrono::DateTime<chrono::Utc>,
    /// Whether the original bytes are reachable (uri set and file readable),
    /// i.e. whether page rasters can be produced.
    pub has_source: bool,
}

/// Receipt for an upload/ingest (mirrors the CLI `index` JSON, D-012).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadReceipt {
    pub document_id: String,
    pub corpus: String,
    pub filename: String,
    /// `false` when the identical (corpus, sha256, parse_version) document
    /// already existed (D-008 dedup).
    pub created: bool,
    pub element_count: i64,
    pub page_count: i32,
}

/// Layout metadata for one page raster (or why it cannot be rendered).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageMeta {
    pub available: bool,
    /// Raster pixel size (only when `available`).
    pub width_px: u32,
    pub height_px: u32,
    /// Page size in PDF points (only when `available`).
    pub width_pts: f32,
    pub height_pts: f32,
    /// Human-readable reason when `available == false`.
    pub reason: Option<String>,
}

/// One element overlay: everything the viewer needs to draw a bbox and show
/// the "discover mode" side panel. Payload bytes (image/blob) are never
/// included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementOverlay {
    pub id: String,
    /// `text` | `image` | `annotation` | `path` | `figure` | `table` | `blob`.
    pub kind: String,
    /// 1-based store page number.
    pub page: i32,
    pub order_idx: i32,
    /// (x0, y0, x1, y1) in top-left PDF points.
    pub bbox: Option<(f32, f32, f32, f32)>,
    pub text: Option<String>,
    pub font_size: Option<f32>,
    pub font_name: Option<String>,
    pub metadata: serde_json::Value,
    /// Cell grid for `kind == "table"` rows (D-018), ordered by (row, col);
    /// `None` for every other kind. delver-store attaches cells on both
    /// `load_document` and `elements_in_bbox`, so this is a pure DTO mapping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells: Option<Vec<CellOverlay>>,
}

/// One stored table cell of a `kind == "table"` overlay (mirrors
/// delver-store's `TableCellRow`, D-018).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellOverlay {
    pub row: i32,
    pub col: i32,
    pub row_span: i32,
    pub col_span: i32,
    /// (x0, y0, x1, y1) in top-left PDF points.
    pub bbox: Option<(f32, f32, f32, f32)>,
    pub text: Option<String>,
    pub is_header: bool,
}

/// Result of executing a DocQL template against a stored document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateRun {
    pub ok: bool,
    /// Outputs JSON (pretty) when `ok`.
    pub output: Option<String>,
    /// Readable error message (anyhow chain, no backtrace) when `!ok`.
    pub error: Option<String>,
}

/// Doc-aware query palette (DV-012): heading candidates + detected tables
/// for the open document, with enough column structure to generate typed
/// table snippets client-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaletteData {
    /// Section-heading candidates (heuristic in `snippets::select_headings`),
    /// capped at [`crate::snippets`]' palette cap; pages are 1-based.
    pub headings: Vec<crate::snippets::HeadingInput>,
    /// Detected tables in (page, order) order.
    pub tables: Vec<TableEntry>,
}

/// One detected table for the palette list (the D-018 metadata keys) plus
/// the non-filler column specs used by the typed-table snippet generator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableEntry {
    /// 1-based store page.
    pub page: i32,
    pub n_rows: i64,
    pub n_cols: i64,
    pub strategy: String,
    pub confidence: f64,
    pub columns: Vec<crate::snippets::ColumnSpec>,
}

// ───────────────────────── server-side implementation ─────────────────────────

#[cfg(feature = "ssr")]
pub use server::*;

#[cfg(feature = "ssr")]
mod server {
    use super::{CellOverlay, DocumentSummary, ElementOverlay, PageMeta, UploadReceipt};
    use anyhow::{anyhow, Context, Result};
    use delver_store::{DelverStore, DocumentId, ElementRow};
    use sha2::{Digest, Sha256};
    use sqlx::Row;
    use std::collections::HashMap;
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use tokio::sync::OnceCell;
    use uuid::Uuid;

    /// The SHARED local dev database (DV-010): the viewer branch is merged,
    /// its embedded migrator matches the shared schema (v3), so the DV-007
    /// split-database rule no longer applies — `delver_viewer` is legacy.
    pub const DEFAULT_DB_URL: &str = "postgres://delver:delver@localhost:5433/delver";
    /// Default corpus for documents ingested through the viewer.
    pub const DEFAULT_CORPUS: &str = "viewer-dev";
    /// Raster DPI (PDF default is 72; 150 matches the previous SQLite store).
    const RASTER_SCALE: f32 = 150.0 / 72.0;
    /// Max page rasters held in memory (DV-003).
    const RASTER_CACHE_CAP: usize = 32;
    /// Viewer ingests are parse_version 1 (dedup key component, D-008).
    const PARSE_VERSION: i32 = 1;

    static STORE: OnceCell<DelverStore> = OnceCell::const_new();

    /// Resolve the database URL: `DATABASE_URL` env > local dev default.
    pub fn db_url() -> String {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DB_URL.to_string())
    }

    /// Shared store handle (connects once; `DelverStore::connect` runs the
    /// embedded migrations).
    pub async fn store() -> Result<&'static DelverStore> {
        STORE
            .get_or_try_init(|| async {
                let url = db_url();
                DelverStore::connect(&url)
                    .await
                    .with_context(|| format!("connecting to Postgres at {url}"))
            })
            .await
    }

    /// Byte-cache directory: `DELVER_DOC_CACHE` env > `~/.delver/doc-cache`.
    pub fn doc_cache_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("DELVER_DOC_CACHE") {
            return PathBuf::from(dir);
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        Path::new(&home).join(".delver").join("doc-cache")
    }

    /// Cache path for a document's original bytes: `<dir>/<sha256-hex>.pdf`.
    pub fn doc_cache_path(dir: &Path, sha256_hex: &str) -> PathBuf {
        dir.join(format!("{sha256_hex}.pdf"))
    }

    /// Display name precedence: PDF Info title > URI basename > short id.
    pub fn display_name(title: Option<&str>, uri: Option<&str>, id: &str) -> String {
        if let Some(title) = title {
            let title = title.trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }
        if let Some(uri) = uri {
            if let Some(base) = uri.rsplit('/').next() {
                let base = base.trim();
                if !base.is_empty() {
                    return base.to_string();
                }
            }
        }
        format!("document {}", &id[..id.len().min(8)])
    }

    fn parse_doc_id(doc_id: &str) -> Result<DocumentId> {
        Uuid::parse_str(doc_id)
            .map(DocumentId)
            .map_err(|e| anyhow!("invalid document id {doc_id:?}: {e}"))
    }

    // ── document listing ─────────────────────────────────────────────────

    const SUMMARY_SELECT: &str = "SELECT d.id::text AS id, c.name AS corpus, d.uri, \
            d.page_count, d.parse_version, d.parsed_at, \
            d.metadata->>'title' AS title \
       FROM documents d JOIN corpora c ON c.id = d.corpus_id";

    fn summary_from_row(row: &sqlx::postgres::PgRow) -> Result<DocumentSummary> {
        let id: String = row.try_get("id")?;
        let uri: Option<String> = row.try_get("uri")?;
        let title: Option<String> = row.try_get("title")?;
        let has_source = uri
            .as_deref()
            .map(|u| Path::new(u).is_file())
            .unwrap_or(false);
        Ok(DocumentSummary {
            name: display_name(title.as_deref(), uri.as_deref(), &id),
            corpus: row.try_get("corpus")?,
            page_count: row.try_get("page_count")?,
            parse_version: row.try_get("parse_version")?,
            parsed_at: row.try_get("parsed_at")?,
            has_source,
            uri,
            id,
        })
    }

    /// All documents (joined with corpus name), newest first.
    pub async fn list_documents() -> Result<Vec<DocumentSummary>> {
        let store = store().await?;
        let sql = format!("{SUMMARY_SELECT} ORDER BY d.parsed_at DESC");
        let rows = sqlx::query(&sql).fetch_all(store.pool()).await?;
        rows.iter().map(summary_from_row).collect()
    }

    /// One document summary by id.
    pub async fn document_summary(doc_id: &str) -> Result<Option<DocumentSummary>> {
        let store = store().await?;
        let id = parse_doc_id(doc_id)?;
        let sql = format!("{SUMMARY_SELECT} WHERE d.id = $1");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(store.pool())
            .await?;
        row.as_ref().map(summary_from_row).transpose()
    }

    // ── upload / ingest ──────────────────────────────────────────────────

    /// Write `bytes` to the byte-cache and ingest into `corpus` with the
    /// cache path as the document URI (DV-002). Idempotent end to end:
    /// the cache write is keyed by content hash and the store dedups on
    /// (corpus, sha256, parse_version) (D-008).
    pub async fn ingest_upload(
        filename: &str,
        corpus: Option<&str>,
        bytes: Vec<u8>,
    ) -> Result<UploadReceipt> {
        if bytes.is_empty() {
            return Err(anyhow!("no file content received"));
        }
        let corpus = match corpus.map(str::trim) {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => DEFAULT_CORPUS.to_string(),
        };
        let sha_hex = format!("{:x}", Sha256::digest(&bytes));
        let cache_dir = doc_cache_dir();
        let cache_path = doc_cache_path(&cache_dir, &sha_hex);

        // Persist original bytes locally so pages can be rasterized later.
        let write_path = cache_path.clone();
        let write_bytes = bytes.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            std::fs::create_dir_all(write_path.parent().expect("cache path has parent"))?;
            if !write_path.exists() {
                std::fs::write(&write_path, &write_bytes)
                    .with_context(|| format!("writing byte-cache {}", write_path.display()))?;
            }
            Ok(())
        })
        .await
        .context("byte-cache task panicked")??;

        let store = store().await?;
        let corpus_id = store.ensure_corpus(&corpus).await?;
        let uri = cache_path.to_string_lossy().to_string();
        let outcome = store
            .ingest_document(corpus_id, Some(&uri), &bytes, PARSE_VERSION)
            .await
            .context("ingesting document")?;
        let element_count = store.element_count(outcome.document_id).await?;

        let page_count: i32 =
            sqlx::query_scalar("SELECT page_count FROM documents WHERE id = $1")
                .bind(outcome.document_id)
                .fetch_one(store.pool())
                .await?;

        Ok(UploadReceipt {
            document_id: outcome.document_id.to_string(),
            corpus,
            filename: filename.to_string(),
            created: outcome.created,
            element_count,
            page_count,
        })
    }

    // ── page elements (overlays) ─────────────────────────────────────────

    /// Elements on one page (0-based viewer index → 1-based store page),
    /// payload bytes stripped. Uses the store's GiST page query with an
    /// effectively-infinite rectangle — per-page element listing without
    /// adding store API (boundary rule), see DV-004.
    pub async fn page_elements(doc_id: &str, page_index: usize) -> Result<Vec<ElementOverlay>> {
        let store = store().await?;
        let id = parse_doc_id(doc_id)?;
        let page = page_index as i32 + 1;
        let rows = store
            .elements_in_bbox(id, page, -1e9, -1e9, 1e9, 1e9)
            .await?;
        Ok(rows.iter().map(overlay_from_row).collect())
    }

    fn overlay_from_row(row: &ElementRow) -> ElementOverlay {
        ElementOverlay {
            id: row.id.to_string(),
            kind: row.kind.as_str().to_string(),
            page: row.page,
            order_idx: row.order_idx,
            bbox: row.bbox,
            text: row.text.clone(),
            font_size: row.font_size,
            font_name: row.font_name.clone(),
            metadata: row.metadata.clone(),
            cells: row.table_cells.as_ref().map(|cells| {
                cells
                    .iter()
                    .map(|c| CellOverlay {
                        row: c.row,
                        col: c.col,
                        row_span: c.row_span,
                        col_span: c.col_span,
                        bbox: c.bbox,
                        text: c.text.clone(),
                        is_header: c.is_header,
                    })
                    .collect()
            }),
        }
    }

    // ── doc-aware query palette (DV-012) ─────────────────────────────────

    /// Max heading candidates returned by the palette.
    const PALETTE_HEADING_CAP: usize = 20;

    /// Heading candidates + detected tables for one document. Pure viewer-
    /// layer SQL over the store pool (the DV-001/DV-004 boundary precedent);
    /// selection/inference logic lives in `crate::snippets` so it is unit-
    /// testable without a database.
    pub async fn doc_palette(doc_id: &str) -> Result<super::PaletteData> {
        use crate::snippets::{column_specs, select_headings, CellLite, HeadingInput};

        let store = store().await?;
        let id = parse_doc_id(doc_id)?;

        // Dominant (modal) text style: the document's body size + boldness.
        let modal: Option<(f64, Option<String>)> = sqlx::query_as(
            "SELECT round(font_size::numeric, 1)::float8 AS fs, font_name \
               FROM elements \
              WHERE document_id = $1 AND kind = 'text' \
                AND font_size IS NOT NULL AND text IS NOT NULL \
                AND length(trim(text)) > 0 \
              GROUP BY fs, font_name ORDER BY count(*) DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(store.pool())
        .await?;
        let (body_size, body_font) = modal
            .map(|(fs, font)| (fs as f32, font))
            .unwrap_or((12.0, None));
        let body_is_bold = body_font
            .as_deref()
            .is_some_and(|f| f.to_ascii_lowercase().contains("bold"));

        // Short-line candidate pool (length(text) is a char count in PG,
        // matching the heuristic's char-based bounds).
        let pool: Vec<(i32, i32, String, f32, Option<String>)> = sqlx::query_as(
            "SELECT page, order_idx, trim(text), font_size, font_name \
               FROM elements \
              WHERE document_id = $1 AND kind = 'text' \
                AND font_size IS NOT NULL AND text IS NOT NULL \
                AND length(trim(text)) BETWEEN 3 AND 80 \
              ORDER BY page, order_idx",
        )
        .bind(id)
        .fetch_all(store.pool())
        .await?;
        let pool: Vec<HeadingInput> = pool
            .into_iter()
            .map(|(page, order_idx, text, font_size, font_name)| HeadingInput {
                page,
                order_idx,
                text,
                font_size,
                font_name,
            })
            .collect();
        let headings = select_headings(&pool, body_size, body_is_bold, PALETTE_HEADING_CAP);

        // Detected tables (kind=table elements; D-018 metadata keys) plus
        // their cells, reduced server-side to non-filler column specs.
        let tables: Vec<(Uuid, i32, serde_json::Value)> = sqlx::query_as(
            "SELECT id, page, metadata FROM elements \
              WHERE document_id = $1 AND kind = 'table' ORDER BY page, order_idx",
        )
        .bind(id)
        .fetch_all(store.pool())
        .await?;
        let table_ids: Vec<Uuid> = tables.iter().map(|(id, _, _)| *id).collect();
        let mut cells_by_table: HashMap<Uuid, Vec<CellLite>> = HashMap::new();
        if !table_ids.is_empty() {
            let cells: Vec<(Uuid, i32, i32, Option<String>, bool)> = sqlx::query_as(
                "SELECT table_element_id, \"row\", col, text, is_header \
                   FROM table_cells WHERE table_element_id = ANY($1) \
                  ORDER BY \"row\", col",
            )
            .bind(&table_ids)
            .fetch_all(store.pool())
            .await?;
            for (table_id, row, col, text, is_header) in cells {
                cells_by_table.entry(table_id).or_default().push(CellLite {
                    row,
                    col,
                    text,
                    is_header,
                });
            }
        }
        let tables = tables
            .into_iter()
            .map(|(table_id, page, metadata)| {
                let int = |key: &str| metadata.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
                let cells = cells_by_table.remove(&table_id).unwrap_or_default();
                let n_cols = (int("n_cols").max(
                    cells.iter().map(|c| c.col as i64 + 1).max().unwrap_or(0),
                )) as usize;
                super::TableEntry {
                    page,
                    n_rows: int("n_rows"),
                    n_cols: n_cols as i64,
                    strategy: metadata
                        .get("strategy")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string(),
                    confidence: metadata
                        .get("confidence")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                    columns: column_specs(n_cols, &cells),
                }
            })
            .collect();

        Ok(super::PaletteData { headings, tables })
    }

    // ── page rasters ─────────────────────────────────────────────────────

    /// A cached raster (or a cached "cannot render" verdict).
    #[derive(Clone)]
    pub enum PageRaster {
        Rendered {
            webp: Vec<u8>,
            width_px: u32,
            height_px: u32,
            width_pts: f32,
            height_pts: f32,
        },
        Unavailable {
            reason: String,
        },
    }

    impl PageRaster {
        pub fn meta(&self) -> PageMeta {
            match self {
                PageRaster::Rendered {
                    width_px,
                    height_px,
                    width_pts,
                    height_pts,
                    ..
                } => PageMeta {
                    available: true,
                    width_px: *width_px,
                    height_px: *height_px,
                    width_pts: *width_pts,
                    height_pts: *height_pts,
                    reason: None,
                },
                PageRaster::Unavailable { reason } => PageMeta {
                    available: false,
                    width_px: 0,
                    height_px: 0,
                    width_pts: 0.0,
                    height_pts: 0.0,
                    reason: Some(reason.clone()),
                },
            }
        }
    }

    /// Minimal LRU: HashMap + recency queue, capped at `cap` (DV-003).
    pub struct LruCache<K, V> {
        cap: usize,
        map: HashMap<K, V>,
        recency: VecDeque<K>,
    }

    impl<K: std::hash::Hash + Eq + Clone, V: Clone> LruCache<K, V> {
        pub fn new(cap: usize) -> Self {
            Self {
                cap: cap.max(1),
                map: HashMap::new(),
                recency: VecDeque::new(),
            }
        }

        pub fn get(&mut self, key: &K) -> Option<V> {
            if let Some(value) = self.map.get(key) {
                let value = value.clone();
                self.touch(key);
                Some(value)
            } else {
                None
            }
        }

        pub fn put(&mut self, key: K, value: V) {
            if self.map.insert(key.clone(), value).is_none() && self.map.len() > self.cap {
                if let Some(oldest) = self.recency.pop_front() {
                    self.map.remove(&oldest);
                }
            }
            self.touch(&key);
        }

        pub fn len(&self) -> usize {
            self.map.len()
        }

        fn touch(&mut self, key: &K) {
            self.recency.retain(|k| k != key);
            self.recency.push_back(key.clone());
        }
    }

    static RASTER_CACHE: Mutex<Option<LruCache<(Uuid, usize), PageRaster>>> = Mutex::new(None);

    fn cache_get(key: &(Uuid, usize)) -> Option<PageRaster> {
        let mut guard = RASTER_CACHE.lock().expect("raster cache poisoned");
        guard
            .get_or_insert_with(|| LruCache::new(RASTER_CACHE_CAP))
            .get(key)
    }

    fn cache_put(key: (Uuid, usize), value: PageRaster) {
        let mut guard = RASTER_CACHE.lock().expect("raster cache poisoned");
        guard
            .get_or_insert_with(|| LruCache::new(RASTER_CACHE_CAP))
            .put(key, value);
    }

    /// Raster for one page, from cache or rendered on demand from the
    /// byte-cache file referenced by the document's `uri`. Documents without
    /// a usable `uri` yield `PageRaster::Unavailable` (never an error) so the
    /// UI can show a placeholder (DV-002).
    pub async fn page_raster(doc_id: &str, page_index: usize) -> Result<PageRaster> {
        let id = parse_doc_id(doc_id)?;
        let key = (id.into_uuid(), page_index);
        if let Some(hit) = cache_get(&key) {
            return Ok(hit);
        }

        let store = store().await?;
        let row: Option<(Option<String>, i32)> =
            sqlx::query_as("SELECT uri, page_count FROM documents WHERE id = $1")
                .bind(id)
                .fetch_optional(store.pool())
                .await?;
        let Some((uri, page_count)) = row else {
            return Err(anyhow!("unknown document {doc_id}"));
        };
        if page_index as i32 >= page_count {
            return Err(anyhow!(
                "page {page_index} out of range (document has {page_count} pages)"
            ));
        }

        let raster = match uri.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            None => PageRaster::Unavailable {
                reason: "original bytes not available — re-ingest with the viewer \
                         or pass --uri at index time"
                    .to_string(),
            },
            Some(uri) if !Path::new(uri).is_file() => PageRaster::Unavailable {
                reason: format!(
                    "original bytes not available — uri {uri:?} is not a readable file; \
                     re-ingest with the viewer or pass --uri at index time"
                ),
            },
            Some(uri) => {
                let path = uri.to_string();
                tokio::task::spawn_blocking(move || render_pdf_page(&path, page_index))
                    .await
                    .context("raster task panicked")??
            }
        };

        cache_put(key, raster.clone());
        Ok(raster)
    }

    /// Rasterize one page with pdfium and encode it as WebP. Blocking; call
    /// from `spawn_blocking`.
    fn render_pdf_page(path: &str, page_index: usize) -> Result<PageRaster> {
        use image::{ImageBuffer, ImageFormat, RgbaImage};
        use pdfium_render::prelude::*;
        use std::io::Cursor;

        let pdfium = bind_pdfium()?;
        let bytes =
            std::fs::read(path).with_context(|| format!("reading byte-cache file {path}"))?;
        let document = pdfium
            .load_pdf_from_byte_slice(&bytes, None)
            .map_err(|e| anyhow!("failed to load PDF {path}: {e}"))?;
        let page = document
            .pages()
            .get(page_index as u16)
            .map_err(|e| anyhow!("failed to get page {page_index}: {e}"))?;

        let width_pts = page.width().value;
        let height_pts = page.height().value;
        let width_px = (width_pts * RASTER_SCALE) as i32;
        let height_px = (height_pts * RASTER_SCALE) as i32;

        let render_config = PdfRenderConfig::new()
            .set_target_width(width_px)
            .set_target_height(height_px)
            .use_lcd_text_rendering(true)
            .render_annotations(true)
            .render_form_data(false);
        let bitmap = page
            .render_with_config(&render_config)
            .map_err(|e| anyhow!("failed to render page {page_index}: {e}"))?;
        let rgba = bitmap.as_rgba_bytes();
        let (width_px, height_px) = (bitmap.width() as u32, bitmap.height() as u32);

        let img: RgbaImage = ImageBuffer::from_raw(width_px, height_px, rgba)
            .ok_or_else(|| anyhow!("rendered page has inconsistent buffer size"))?;
        let mut webp = Vec::new();
        img.write_to(&mut Cursor::new(&mut webp), ImageFormat::WebP)
            .context("encoding WebP")?;

        Ok(PageRaster::Rendered {
            webp,
            width_px,
            height_px,
            width_pts,
            height_pts,
        })
    }

    /// Bind pdfium the same way the start scripts arrange it: runtime
    /// `PDFIUM_LIBRARY_PATH` > compile-time path from build.rs > `./` >
    /// system library (DV-005).
    fn bind_pdfium() -> Result<pdfium_render::prelude::Pdfium> {
        use pdfium_render::prelude::*;
        let configured = std::env::var("PDFIUM_LIBRARY_PATH")
            .ok()
            .or_else(|| option_env!("PDFIUM_LIBRARY_PATH").map(str::to_string));
        let bindings = match configured {
            Some(dir) => {
                Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&dir))
                    .or_else(|_| {
                        Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./"))
                    })
                    .or_else(|_| Pdfium::bind_to_system_library())
            }
            None => Pdfium::bind_to_system_library(),
        }
        .map_err(|e| anyhow!("failed to bind pdfium library: {e}"))?;
        Ok(Pdfium::new(bindings))
    }

    // ── template execution ───────────────────────────────────────────────

    /// Execute DocQL template source against a stored document via the same
    /// path the CLI `query --doc` uses: load rows → `hydrate_pages` →
    /// `process_parsed` (D-012). Embedder comes from `DELVER_EMBED_ENDPOINT`
    /// (DV-006); no tokenizer (character-based chunking) this slice.
    pub async fn execute_template(doc_id: &str, template: &str) -> Result<String> {
        let store = store().await?;
        let id = parse_doc_id(doc_id)?;
        let loaded = store.load_document(id).await?;
        if loaded.elements.is_empty() {
            return Err(anyhow!(
                "document {doc_id} has no stored elements (unknown id or empty document)"
            ));
        }
        let template = template.to_string();
        tokio::task::spawn_blocking(move || -> Result<String> {
            let pages = delver_store::hydrate_pages(&loaded.elements);
            let mut match_context = delver_core::layout::MatchContext::default();
            match_context.embedder = embedder_from_env()?.into();
            delver_core::process_parsed(&pages, &match_context, &template, None)
        })
        .await
        .context("template task panicked")?
    }

    /// `DELVER_EMBED_ENDPOINT` passthrough → Databricks serving embedder
    /// (same env contract as the CLI's `build_embedder`, D-015). `None`
    /// when unset, so EmbeddingSim templates fail loud (D-006).
    fn embedder_from_env(
    ) -> Result<Option<std::sync::Arc<dyn delver_core::embed::Embedder>>> {
        match std::env::var("DELVER_EMBED_ENDPOINT") {
            Err(_) => Ok(None),
            Ok(endpoint) if endpoint.trim().is_empty() => Ok(None),
            Ok(endpoint) => {
                let embedder = delver_embed::DatabricksEmbedder::new(&endpoint)
                    .map_err(|e| anyhow!("configuring embedding endpoint {endpoint:?}: {e}"))?;
                Ok(Some(std::sync::Arc::new(embedder)))
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn display_name_prefers_title_then_uri_basename_then_id() {
            assert_eq!(
                display_name(Some("3M 10-K"), Some("/x/y.pdf"), "abcdef12-3456"),
                "3M 10-K"
            );
            assert_eq!(
                display_name(Some("   "), Some("/x/deadbeef.pdf"), "abcdef12-3456"),
                "deadbeef.pdf"
            );
            assert_eq!(
                display_name(None, None, "abcdef12-3456"),
                "document abcdef12"
            );
            assert_eq!(
                display_name(None, Some("mem://synthetic.pdf"), "abcdef12-3456"),
                "synthetic.pdf"
            );
        }

        #[test]
        fn overlay_from_row_maps_table_cells() {
            let id = Uuid::new_v4();
            let row = ElementRow {
                id: delver_store::ElementId(id),
                document_id: DocumentId(Uuid::new_v4()),
                page: 26,
                kind: delver_store::ElementKind::Table,
                order_idx: 7,
                text: None,
                font_size: None,
                font_name: None,
                style_key: None,
                bbox: Some((10.0, 20.0, 110.0, 80.0)),
                metadata: serde_json::json!({
                    "n_rows": 1, "n_cols": 2, "strategy": "ruled", "confidence": 0.9
                }),
                image: None,
                blob: None,
                table_cells: Some(vec![
                    delver_store::TableCellRow {
                        row: 0,
                        col: 0,
                        row_span: 1,
                        col_span: 1,
                        text: Some("Sales".to_string()),
                        bbox: Some((10.0, 20.0, 60.0, 40.0)),
                        is_header: true,
                    },
                    delver_store::TableCellRow {
                        row: 0,
                        col: 1,
                        row_span: 1,
                        col_span: 1,
                        text: None,
                        bbox: None,
                        is_header: false,
                    },
                ]),
            };
            let overlay = overlay_from_row(&row);
            assert_eq!(overlay.kind, "table");
            let cells = overlay.cells.expect("table overlay carries cells");
            assert_eq!(cells.len(), 2);
            assert_eq!(cells[0].text.as_deref(), Some("Sales"));
            assert!(cells[0].is_header);
            assert_eq!(cells[0].bbox, Some((10.0, 20.0, 60.0, 40.0)));
            assert_eq!((cells[1].row, cells[1].col), (0, 1));
            assert!(!cells[1].is_header);
        }

        #[test]
        fn doc_cache_path_is_sha_keyed() {
            let p = doc_cache_path(Path::new("/tmp/cache"), "00ff");
            assert_eq!(p, PathBuf::from("/tmp/cache/00ff.pdf"));
        }

        #[test]
        fn lru_caps_and_evicts_least_recently_used() {
            let mut lru: LruCache<u32, u32> = LruCache::new(2);
            lru.put(1, 10);
            lru.put(2, 20);
            assert_eq!(lru.get(&1), Some(10)); // refresh 1 → 2 is now oldest
            lru.put(3, 30); // evicts 2
            assert_eq!(lru.len(), 2);
            assert_eq!(lru.get(&2), None);
            assert_eq!(lru.get(&1), Some(10));
            assert_eq!(lru.get(&3), Some(30));
        }

        #[test]
        fn lru_overwrite_does_not_evict() {
            let mut lru: LruCache<u32, u32> = LruCache::new(2);
            lru.put(1, 10);
            lru.put(2, 20);
            lru.put(2, 21); // overwrite, not insert
            assert_eq!(lru.len(), 2);
            assert_eq!(lru.get(&1), Some(10));
            assert_eq!(lru.get(&2), Some(21));
        }
    }
}
