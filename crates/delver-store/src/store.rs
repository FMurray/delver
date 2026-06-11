//! Async persistent index over Postgres (see docs/DECISIONS.md D-002, D-003, D-008).
//!
//! Postgres is the source of truth; the in-memory `PdfIndex` is derived data
//! rebuilt by [`crate::hydrate_index`]. All queries are runtime-checked
//! (`sqlx::query`) by design: builds must not require a live database.

use std::collections::BTreeMap;
use std::time::Duration;

use delver_core::layout::MatchContext;
use delver_core::parse::{get_page_content, ContentHandle, PageContents};
use delver_core::search_index::PdfIndex;
use lopdf::{Document, Object};
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;

use crate::error::StoreError;
use crate::types::{
    CorpusId, DocumentId, ElementId, ElementKind, ElementRow, ImagePayload, IngestOutcome,
    SearchScope, TextSearchHit,
};

/// Bump when migrations change the logical schema.
pub const SCHEMA_VERSION: i32 = 1;

/// Shared projection used everywhere element rows are returned: box corners
/// are decomposed into floats so no geometric-type decoding is needed
/// (`bbox[1]` = lower-left point, `bbox[0]` = upper-right point).
const ELEMENT_SELECT: &str = "SELECT e.id, e.document_id, e.page, e.kind, e.order_idx, e.text, \
       e.font_size, e.font_name, e.style_key, e.metadata, \
       (e.bbox[1])[0] AS bx0, (e.bbox[1])[1] AS by0, \
       (e.bbox[0])[0] AS bx1, (e.bbox[0])[1] AS by1, \
       i.width AS image_width, i.height AS image_height, i.data AS image_data \
  FROM elements e \
  LEFT JOIN images i ON i.element_id = e.id";

/// Service layer for the persistent document index.
#[derive(Debug, Clone)]
pub struct DelverStore {
    pool: PgPool,
}

impl DelverStore {
    /// Connect to Postgres, run embedded migrations, and record schema/build
    /// metadata in `index_meta`.
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .connect(url)
            .await?;

        sqlx::migrate!("./migrations").run(&pool).await?;

        sqlx::query(
            "INSERT INTO index_meta (id, schema_version, delver_version) \
             VALUES (1, $1, $2) \
             ON CONFLICT (id) DO UPDATE \
                SET schema_version = EXCLUDED.schema_version, \
                    delver_version = EXCLUDED.delver_version",
        )
        .bind(SCHEMA_VERSION)
        .bind(env!("CARGO_PKG_VERSION"))
        .execute(&pool)
        .await?;

        Ok(Self { pool })
    }

    /// Access the underlying pool (escape hatch for callers composing queries).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Get-or-create a corpus by unique name.
    pub async fn ensure_corpus(&self, name: &str) -> Result<CorpusId, StoreError> {
        let id: CorpusId = sqlx::query_scalar(
            "INSERT INTO corpora (name) VALUES ($1) \
             ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name \
             RETURNING id",
        )
        .bind(name)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    /// Parse `pdf_bytes` with delver-core and persist the result.
    ///
    /// Idempotent per D-008: if (corpus, sha256(bytes), parse_version) already
    /// exists, returns the existing document with `created: false` without
    /// re-parsing or writing any rows.
    pub async fn ingest_document(
        &self,
        corpus: CorpusId,
        uri: Option<&str>,
        pdf_bytes: &[u8],
        parse_version: i32,
    ) -> Result<IngestOutcome, StoreError> {
        let sha = sha256(pdf_bytes);
        if let Some(existing) = self.find_document(corpus, &sha, parse_version).await? {
            return Ok(IngestOutcome {
                document_id: existing,
                created: false,
            });
        }

        let doc = Document::load_mem(pdf_bytes).map_err(|e| StoreError::Pdf(e.to_string()))?;
        let pages = get_page_content(&doc).map_err(|e| StoreError::Pdf(e.to_string()))?;
        self.insert_parsed(corpus, uri, &sha, &pages, parse_version)
            .await
    }

    /// Persist an already-parsed document (same dedup contract as
    /// [`Self::ingest_document`]). Element ids from `pages` are stored
    /// verbatim, so a hydrated index is id-identical to the caller's parse.
    pub async fn ingest_parsed(
        &self,
        corpus: CorpusId,
        uri: Option<&str>,
        pdf_bytes: &[u8],
        pages: &BTreeMap<u32, PageContents>,
        parse_version: i32,
    ) -> Result<IngestOutcome, StoreError> {
        let sha = sha256(pdf_bytes);
        if let Some(existing) = self.find_document(corpus, &sha, parse_version).await? {
            return Ok(IngestOutcome {
                document_id: existing,
                created: false,
            });
        }
        self.insert_parsed(corpus, uri, &sha, pages, parse_version)
            .await
    }

    /// Number of element rows stored for a document (0 if the id is unknown).
    pub async fn element_count(&self, doc: DocumentId) -> Result<i64, StoreError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM elements WHERE document_id = $1")
            .bind(doc)
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// Load all element rows of a document in global document order.
    pub async fn load_document(&self, doc: DocumentId) -> Result<Vec<ElementRow>, StoreError> {
        let sql = format!("{ELEMENT_SELECT} WHERE e.document_id = $1 ORDER BY e.order_idx");
        let rows = sqlx::query(&sql).bind(doc).fetch_all(&self.pool).await?;
        rows.iter().map(element_from_row).collect()
    }

    /// Full-text search (`tsvector` + `ts_rank`) over a corpus or one document.
    pub async fn text_search(
        &self,
        scope: impl Into<SearchScope>,
        query: &str,
        limit: i64,
    ) -> Result<Vec<TextSearchHit>, StoreError> {
        const PROJECTION: &str = "SELECT e.id, e.document_id, e.page, e.order_idx, e.text, \
               ts_rank(e.text_fts, plainto_tsquery('english', $2)) AS rank \
          FROM elements e";
        const PREDICATE: &str = "e.text_fts @@ plainto_tsquery('english', $2) \
             ORDER BY rank DESC, e.document_id, e.order_idx LIMIT $3";

        let rows = match scope.into() {
            SearchScope::Corpus(corpus) => {
                let sql = format!(
                    "{PROJECTION} JOIN documents d ON d.id = e.document_id \
                     WHERE d.corpus_id = $1 AND {PREDICATE}"
                );
                sqlx::query(&sql)
                    .bind(corpus)
                    .bind(query)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
            SearchScope::Document(doc) => {
                let sql = format!("{PROJECTION} WHERE e.document_id = $1 AND {PREDICATE}");
                sqlx::query(&sql)
                    .bind(doc)
                    .bind(query)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
        };

        rows.iter()
            .map(|row| {
                Ok(TextSearchHit {
                    element_id: row.try_get("id")?,
                    document_id: row.try_get("document_id")?,
                    page: row.try_get("page")?,
                    order_idx: row.try_get("order_idx")?,
                    text: row.try_get::<Option<String>, _>("text")?.unwrap_or_default(),
                    rank: row.try_get("rank")?,
                })
            })
            .collect()
    }

    /// Elements on one page whose bbox overlaps the query rectangle
    /// (`bbox && box(...)`, GiST-indexed). Coordinates are top-left based,
    /// matching parsed element bboxes.
    pub async fn elements_in_bbox(
        &self,
        doc: DocumentId,
        page: i32,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
    ) -> Result<Vec<ElementRow>, StoreError> {
        let sql = format!(
            "{ELEMENT_SELECT} \
             WHERE e.document_id = $1 AND e.page = $2 \
               AND e.bbox && box(point($3, $4), point($5, $6)) \
             ORDER BY e.order_idx"
        );
        let rows = sqlx::query(&sql)
            .bind(doc)
            .bind(page)
            .bind(x0 as f64)
            .bind(y0 as f64)
            .bind(x1 as f64)
            .bind(y1 as f64)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(element_from_row).collect()
    }

    async fn find_document(
        &self,
        corpus: CorpusId,
        sha: &[u8],
        parse_version: i32,
    ) -> Result<Option<DocumentId>, StoreError> {
        let id: Option<DocumentId> = sqlx::query_scalar(
            "SELECT id FROM documents \
             WHERE corpus_id = $1 AND content_sha256 = $2 AND parse_version = $3",
        )
        .bind(corpus)
        .bind(sha)
        .bind(parse_version)
        .fetch_optional(&self.pool)
        .await?;
        Ok(id)
    }

    async fn insert_parsed(
        &self,
        corpus: CorpusId,
        uri: Option<&str>,
        sha: &[u8],
        pages: &BTreeMap<u32, PageContents>,
        parse_version: i32,
    ) -> Result<IngestOutcome, StoreError> {
        // Build the in-memory index once: it defines the global element order
        // (order_idx) and the per-row style keys we persist.
        let index = PdfIndex::new(pages, &MatchContext::default());
        let flat = FlatElements::from_index(&index);

        let mut tx = self.pool.begin().await?;

        let inserted: Option<DocumentId> = sqlx::query_scalar(
            "INSERT INTO documents (corpus_id, content_sha256, uri, page_count, parse_version) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (corpus_id, content_sha256, parse_version) DO NOTHING \
             RETURNING id",
        )
        .bind(corpus)
        .bind(sha)
        .bind(uri)
        .bind(pages.len() as i32)
        .bind(parse_version)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(document_id) = inserted else {
            // Lost a race with a concurrent identical ingest: defer to it.
            drop(tx);
            let existing = self
                .find_document(corpus, sha, parse_version)
                .await?
                .ok_or_else(|| {
                    StoreError::Corrupt(
                        "document insert conflicted but no existing row found".to_string(),
                    )
                })?;
            return Ok(IngestOutcome {
                document_id: existing,
                created: false,
            });
        };

        if !flat.ids.is_empty() {
            sqlx::query(
                "INSERT INTO elements \
                   (id, document_id, page, kind, order_idx, text, font_size, font_name, style_key, bbox) \
                 SELECT u.id, $1, u.page, u.kind, u.order_idx, u.text, u.font_size, u.font_name, \
                        u.style_key, box(point(u.x0, u.y0), point(u.x1, u.y1)) \
                   FROM UNNEST($2::uuid[], $3::int4[], $4::text[], $5::int4[], $6::text[], \
                               $7::float4[], $8::text[], $9::int8[], \
                               $10::float8[], $11::float8[], $12::float8[], $13::float8[]) \
                     AS u(id, page, kind, order_idx, text, font_size, font_name, style_key, x0, y0, x1, y1)",
            )
            .bind(document_id)
            .bind(&flat.ids)
            .bind(&flat.pages)
            .bind(&flat.kinds)
            .bind(&flat.order_idxs)
            .bind(&flat.texts)
            .bind(&flat.font_sizes)
            .bind(&flat.font_names)
            .bind(&flat.style_keys)
            .bind(&flat.x0s)
            .bind(&flat.y0s)
            .bind(&flat.x1s)
            .bind(&flat.y1s)
            .execute(&mut *tx)
            .await?;
        }

        if !flat.image_ids.is_empty() {
            sqlx::query(
                "INSERT INTO images (element_id, width, height, data) \
                 SELECT u.element_id, u.width, u.height, u.data \
                   FROM UNNEST($1::uuid[], $2::int4[], $3::int4[], $4::bytea[]) \
                     AS u(element_id, width, height, data)",
            )
            .bind(&flat.image_ids)
            .bind(&flat.image_widths)
            .bind(&flat.image_heights)
            .bind(&flat.image_datas)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        Ok(IngestOutcome {
            document_id,
            created: true,
        })
    }
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

/// Column-major staging buffers for the UNNEST bulk insert.
#[derive(Default)]
struct FlatElements {
    ids: Vec<uuid::Uuid>,
    pages: Vec<i32>,
    kinds: Vec<String>,
    order_idxs: Vec<i32>,
    texts: Vec<Option<String>>,
    font_sizes: Vec<Option<f32>>,
    font_names: Vec<Option<String>>,
    style_keys: Vec<Option<i64>>,
    x0s: Vec<f64>,
    y0s: Vec<f64>,
    x1s: Vec<f64>,
    y1s: Vec<f64>,

    image_ids: Vec<uuid::Uuid>,
    image_widths: Vec<Option<i32>>,
    image_heights: Vec<Option<i32>>,
    image_datas: Vec<Vec<u8>>,
}

impl FlatElements {
    fn from_index(index: &PdfIndex) -> Self {
        let mut flat = FlatElements::default();
        for order_idx in 0..index.doc_len() {
            let Some(handle) = index.get_handle(order_idx) else {
                continue;
            };
            match handle {
                ContentHandle::Text(_) => {
                    let th = index
                        .as_text_handle(handle)
                        .expect("text handle for text content");
                    let text = index.text(th);
                    flat.ids.push(text.id);
                    flat.pages.push(text.page_number as i32);
                    flat.kinds.push(ElementKind::Text.as_str().to_string());
                    flat.order_idxs.push(order_idx as i32);
                    flat.texts.push(Some(text.text.to_string()));
                    flat.font_sizes.push(Some(text.font_size));
                    flat.font_names.push(text.font_name.map(str::to_string));
                    // u64 -> i64 is a bit-preserving cast; the key is
                    // informational (process-local font interner, see D-010).
                    flat.style_keys
                        .push(index.style_key_bits(th).map(|bits| bits as i64));
                    let (x0, y0, x1, y1) = text.bbox;
                    flat.push_bbox(x0, y0, x1, y1);
                }
                ContentHandle::Image(_) => {
                    let ih = index
                        .as_image_handle(handle)
                        .expect("image handle for image content");
                    let image = index.image(ih);
                    flat.ids.push(image.id);
                    flat.pages.push(image.page_number as i32);
                    flat.kinds.push(ElementKind::Image.as_str().to_string());
                    flat.order_idxs.push(order_idx as i32);
                    flat.texts.push(None);
                    flat.font_sizes.push(None);
                    flat.font_names.push(None);
                    flat.style_keys.push(None);
                    flat.push_bbox(image.bbox.x0, image.bbox.y0, image.bbox.x1, image.bbox.y1);

                    let (width, height, data) = image_payload(image.image_object);
                    flat.image_ids.push(image.id);
                    flat.image_widths.push(width);
                    flat.image_heights.push(height);
                    flat.image_datas.push(data);
                }
            }
        }
        flat
    }

    fn push_bbox(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        self.x0s.push(x0 as f64);
        self.y0s.push(y0 as f64);
        self.x1s.push(x1 as f64);
        self.y1s.push(y1 as f64);
    }
}

fn image_payload(object: &Object) -> (Option<i32>, Option<i32>, Vec<u8>) {
    match object.as_stream() {
        Ok(stream) => {
            let dim = |key: &[u8]| {
                stream
                    .dict
                    .get(key)
                    .ok()
                    .and_then(|o| o.as_i64().ok())
                    .map(|v| v as i32)
            };
            (dim(b"Width"), dim(b"Height"), stream.content.clone())
        }
        Err(_) => (None, None, Vec::new()),
    }
}

fn element_from_row(row: &PgRow) -> Result<ElementRow, StoreError> {
    let kind = ElementKind::parse(&row.try_get::<String, _>("kind")?)?;

    let bx0: Option<f64> = row.try_get("bx0")?;
    let by0: Option<f64> = row.try_get("by0")?;
    let bx1: Option<f64> = row.try_get("bx1")?;
    let by1: Option<f64> = row.try_get("by1")?;
    // Postgres normalizes box corners (upper-right first); reorder to the
    // (min, min, max, max) convention parsed bboxes use.
    let bbox = match (bx0, by0, bx1, by1) {
        (Some(ax), Some(ay), Some(bx), Some(by)) => Some((
            (ax.min(bx)) as f32,
            (ay.min(by)) as f32,
            (ax.max(bx)) as f32,
            (ay.max(by)) as f32,
        )),
        _ => None,
    };

    let image = match kind {
        ElementKind::Image => Some(ImagePayload {
            width: row.try_get("image_width")?,
            height: row.try_get("image_height")?,
            data: row
                .try_get::<Option<Vec<u8>>, _>("image_data")?
                .unwrap_or_default(),
        }),
        ElementKind::Text => None,
    };

    Ok(ElementRow {
        id: row.try_get::<ElementId, _>("id")?,
        document_id: row.try_get("document_id")?,
        page: row.try_get("page")?,
        kind,
        order_idx: row.try_get("order_idx")?,
        text: row.try_get("text")?,
        font_size: row.try_get("font_size")?,
        font_name: row.try_get("font_name")?,
        style_key: row.try_get("style_key")?,
        bbox,
        metadata: row.try_get("metadata")?,
        image,
    })
}
