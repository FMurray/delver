//! Async persistent index over Postgres (see docs/DECISIONS.md D-002, D-003, D-008).
//!
//! Postgres is the source of truth; the in-memory `PdfIndex` is derived data
//! rebuilt by [`crate::hydrate_index`]. All queries are runtime-checked
//! (`sqlx::query`) by design: builds must not require a live database.

use std::time::Duration;

use delver_core::layout::MatchContext;
use delver_core::parse::{parse_document, ContentHandle, ParsedDocument};
use delver_core::search_index::PdfIndex;
use lopdf::{Document, Object};
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;

use crate::error::StoreError;
use crate::types::{
    BlobRow, CorpusId, DocumentId, ElementId, ElementKind, ElementRow, ImagePayload, IngestOutcome,
    LoadedDocument, RefEdgeRow, SearchScope, TableCellRow, TextSearchHit,
};

/// Bump when migrations change the logical schema.
pub const SCHEMA_VERSION: i32 = 3;

/// Shared projection used everywhere element rows are returned: box corners
/// are decomposed into floats so no geometric-type decoding is needed
/// (`bbox[1]` = lower-left point, `bbox[0]` = upper-right point).
const ELEMENT_SELECT: &str = "SELECT e.id, e.document_id, e.page, e.kind, e.order_idx, e.text, \
       e.font_size, e.font_name, e.style_key, e.metadata, \
       (e.bbox[1])[0] AS bx0, (e.bbox[1])[1] AS by0, \
       (e.bbox[0])[0] AS bx1, (e.bbox[0])[1] AS by1, \
       i.width AS image_width, i.height AS image_height, i.data AS image_data, \
       b.data AS blob_data, b.mime AS blob_mime, b.filename AS blob_filename \
  FROM elements e \
  LEFT JOIN images i ON i.element_id = e.id \
  LEFT JOIN blobs b ON b.element_id = e.id";

/// Shared full-text search projection/predicate (`text_search`,
/// `text_search_filtered`): `$2` is the query, `$3` the limit.
const SEARCH_PROJECTION: &str = "SELECT e.id, e.document_id, e.page, e.order_idx, e.text, \
       ts_rank(e.text_fts, plainto_tsquery('english', $2)) AS rank \
  FROM elements e";
const SEARCH_PREDICATE: &str = "e.text_fts @@ plainto_tsquery('english', $2) \
     ORDER BY rank DESC, e.document_id, e.order_idx LIMIT $3";

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
        // Full parse (D-016): identical pipeline to the fresh-query path —
        // content stream plus annotations, paths, figures, embedded files,
        // and the Info-dict document metadata.
        let parsed = parse_document(&doc).map_err(|e| StoreError::Pdf(e.to_string()))?;
        self.insert_parsed(corpus, uri, &sha, &parsed, parse_version)
            .await
    }

    /// Persist an already-parsed document (same dedup contract as
    /// [`Self::ingest_document`]). Element ids from `parsed` are stored
    /// verbatim, so a hydrated index is id-identical to the caller's parse.
    pub async fn ingest_parsed(
        &self,
        corpus: CorpusId,
        uri: Option<&str>,
        pdf_bytes: &[u8],
        parsed: &ParsedDocument,
        parse_version: i32,
    ) -> Result<IngestOutcome, StoreError> {
        let sha = sha256(pdf_bytes);
        if let Some(existing) = self.find_document(corpus, &sha, parse_version).await? {
            return Ok(IngestOutcome {
                document_id: existing,
                created: false,
            });
        }
        self.insert_parsed(corpus, uri, &sha, parsed, parse_version)
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

    /// Load a document: Info-dict metadata, all element rows in global
    /// document order, and the document's ref edges (D-016). Unknown ids
    /// yield an empty `LoadedDocument` (callers treat no elements as
    /// "unknown or empty", matching the pre-slice contract).
    pub async fn load_document(&self, doc: DocumentId) -> Result<LoadedDocument, StoreError> {
        let metadata: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT metadata FROM documents WHERE id = $1")
                .bind(doc)
                .fetch_optional(&self.pool)
                .await?;

        let sql = format!("{ELEMENT_SELECT} WHERE e.document_id = $1 ORDER BY e.order_idx");
        let rows = sqlx::query(&sql).bind(doc).fetch_all(&self.pool).await?;
        let mut elements: Vec<ElementRow> = rows
            .iter()
            .map(element_from_row)
            .collect::<Result<_, _>>()?;
        self.attach_table_cells(&mut elements).await?;

        let refs = sqlx::query(
            "SELECT r.from_element, r.to_element, r.kind, r.metadata \
               FROM element_refs r \
               JOIN elements e ON e.id = r.from_element \
              WHERE e.document_id = $1 \
              ORDER BY r.from_element, r.to_element, r.kind",
        )
        .bind(doc)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|row| {
            Ok(RefEdgeRow {
                from_element: row.try_get("from_element")?,
                to_element: row.try_get("to_element")?,
                kind: row.try_get("kind")?,
                metadata: row.try_get("metadata")?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;

        Ok(LoadedDocument {
            document_id: doc,
            metadata: metadata.unwrap_or_else(|| serde_json::json!({})),
            elements,
            refs,
        })
    }

    /// Full-text search (`tsvector` + `ts_rank`) over a corpus or one document.
    pub async fn text_search(
        &self,
        scope: impl Into<SearchScope>,
        query: &str,
        limit: i64,
    ) -> Result<Vec<TextSearchHit>, StoreError> {
        let rows = match scope.into() {
            SearchScope::Corpus(corpus) => {
                return self.text_search_filtered(corpus, query, limit, None).await
            }
            SearchScope::Document(doc) => {
                let sql =
                    format!("{SEARCH_PROJECTION} WHERE e.document_id = $1 AND {SEARCH_PREDICATE}");
                sqlx::query(&sql)
                    .bind(doc)
                    .bind(query)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        rows.iter().map(search_hit_from_row).collect()
    }

    /// Corpus-scoped full-text search additionally filtered by document
    /// partition values (Stage C, D-023): when `partitions` is given, only
    /// documents whose `metadata.partitions` contains it (jsonb containment)
    /// are searched. `None` is exactly [`Self::text_search`] corpus scope.
    pub async fn text_search_filtered(
        &self,
        corpus: CorpusId,
        query: &str,
        limit: i64,
        partitions: Option<&serde_json::Value>,
    ) -> Result<Vec<TextSearchHit>, StoreError> {
        let sql = format!(
            "{SEARCH_PROJECTION} JOIN documents d ON d.id = e.document_id \
             WHERE d.corpus_id = $1 \
               AND ($4::jsonb IS NULL OR d.metadata @> jsonb_build_object('partitions', $4::jsonb)) \
               AND {SEARCH_PREDICATE}"
        );
        let rows = sqlx::query(&sql)
            .bind(corpus)
            .bind(query)
            .bind(limit)
            .bind(partitions)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(search_hit_from_row).collect()
    }

    /// Merge partition key/values into `documents.metadata` under
    /// `"partitions"` (Stage C, D-023). The whole `partitions` object is
    /// replaced (last `delver index` wins); other metadata keys are
    /// untouched. Unknown document ids are an error (D-006).
    pub async fn set_document_partitions(
        &self,
        doc: DocumentId,
        partitions: &serde_json::Value,
    ) -> Result<(), StoreError> {
        let result = sqlx::query(
            "UPDATE documents \
                SET metadata = jsonb_set(metadata, '{partitions}', $2, true) \
              WHERE id = $1",
        )
        .bind(doc)
        .bind(partitions)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::Corrupt(format!(
                "cannot set partitions: unknown document {doc}"
            )));
        }
        Ok(())
    }

    /// Document ids in `corpus` whose `metadata.partitions` contains
    /// `partitions` (jsonb containment); `None` lists the whole corpus.
    /// Ordered by id so multi-document query output is deterministic
    /// (Stage C, D-023).
    pub async fn documents_matching(
        &self,
        corpus: CorpusId,
        partitions: Option<&serde_json::Value>,
    ) -> Result<Vec<DocumentId>, StoreError> {
        let ids: Vec<DocumentId> = sqlx::query_scalar(
            "SELECT id FROM documents \
              WHERE corpus_id = $1 \
                AND ($2::jsonb IS NULL OR metadata @> jsonb_build_object('partitions', $2::jsonb)) \
              ORDER BY id",
        )
        .bind(corpus)
        .bind(partitions)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids)
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
        let mut elements: Vec<ElementRow> = rows
            .iter()
            .map(element_from_row)
            .collect::<Result<_, _>>()?;
        self.attach_table_cells(&mut elements).await?;
        Ok(elements)
    }

    /// Populate `table_cells` for any kind=table rows in `elements` (D-018).
    /// One query for all tables; no-op (and no query) when there are none.
    async fn attach_table_cells(&self, elements: &mut [ElementRow]) -> Result<(), StoreError> {
        let table_ids: Vec<uuid::Uuid> = elements
            .iter()
            .filter(|e| e.kind == ElementKind::Table)
            .map(|e| e.id.into_uuid())
            .collect();
        if table_ids.is_empty() {
            return Ok(());
        }

        let rows = sqlx::query(
            "SELECT c.table_element_id, c.\"row\", c.col, c.row_span, c.col_span, c.text, \
                    c.is_header, \
                    (c.bbox[1])[0] AS bx0, (c.bbox[1])[1] AS by0, \
                    (c.bbox[0])[0] AS bx1, (c.bbox[0])[1] AS by1 \
               FROM table_cells c \
              WHERE c.table_element_id = ANY($1) \
              ORDER BY c.table_element_id, c.\"row\", c.col",
        )
        .bind(&table_ids)
        .fetch_all(&self.pool)
        .await?;

        let mut by_table: std::collections::HashMap<uuid::Uuid, Vec<TableCellRow>> =
            std::collections::HashMap::new();
        for row in &rows {
            let table_id: uuid::Uuid = row.try_get("table_element_id")?;
            by_table
                .entry(table_id)
                .or_default()
                .push(TableCellRow {
                    row: row.try_get("row")?,
                    col: row.try_get("col")?,
                    row_span: row.try_get("row_span")?,
                    col_span: row.try_get("col_span")?,
                    text: row.try_get("text")?,
                    bbox: decode_bbox(row)?,
                    is_header: row.try_get("is_header")?,
                });
        }
        for element in elements
            .iter_mut()
            .filter(|e| e.kind == ElementKind::Table)
        {
            element.table_cells =
                Some(by_table.remove(&element.id.into_uuid()).unwrap_or_default());
        }
        Ok(())
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
        parsed: &ParsedDocument,
        parse_version: i32,
    ) -> Result<IngestOutcome, StoreError> {
        // Build the in-memory index once: it defines the global element order
        // (order_idx) and the per-row style keys we persist.
        let index = PdfIndex::new(&parsed.pages, &MatchContext::default());
        let flat = FlatElements::from_index(&index);

        let mut tx = self.pool.begin().await?;

        let inserted: Option<DocumentId> = sqlx::query_scalar(
            "INSERT INTO documents \
               (corpus_id, content_sha256, uri, page_count, parse_version, metadata) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (corpus_id, content_sha256, parse_version) DO NOTHING \
             RETURNING id",
        )
        .bind(corpus)
        .bind(sha)
        .bind(uri)
        .bind(parsed.page_count() as i32)
        .bind(parse_version)
        .bind(&parsed.metadata)
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
                   (id, document_id, page, kind, order_idx, text, font_size, font_name, style_key, bbox, metadata) \
                 SELECT u.id, $1, u.page, u.kind, u.order_idx, u.text, u.font_size, u.font_name, \
                        u.style_key, box(point(u.x0, u.y0), point(u.x1, u.y1)), u.metadata \
                   FROM UNNEST($2::uuid[], $3::int4[], $4::text[], $5::int4[], $6::text[], \
                               $7::float4[], $8::text[], $9::int8[], \
                               $10::float8[], $11::float8[], $12::float8[], $13::float8[], \
                               $14::jsonb[]) \
                     AS u(id, page, kind, order_idx, text, font_size, font_name, style_key, x0, y0, x1, y1, metadata)",
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
            .bind(&flat.metadatas)
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

        if !flat.blob_ids.is_empty() {
            sqlx::query(
                "INSERT INTO blobs (element_id, data, mime, filename) \
                 SELECT u.element_id, u.data, u.mime, u.filename \
                   FROM UNNEST($1::uuid[], $2::bytea[], $3::text[], $4::text[]) \
                     AS u(element_id, data, mime, filename)",
            )
            .bind(&flat.blob_ids)
            .bind(&flat.blob_datas)
            .bind(&flat.blob_mimes)
            .bind(&flat.blob_filenames)
            .execute(&mut *tx)
            .await?;
        }

        if !flat.cell_table_ids.is_empty() {
            sqlx::query(
                "INSERT INTO table_cells \
                   (table_element_id, \"row\", col, row_span, col_span, text, bbox, is_header) \
                 SELECT u.table_element_id, u.row, u.col, u.row_span, u.col_span, u.text, \
                        box(point(u.x0, u.y0), point(u.x1, u.y1)), u.is_header \
                   FROM UNNEST($1::uuid[], $2::int4[], $3::int4[], $4::int4[], $5::int4[], \
                               $6::text[], $7::float8[], $8::float8[], $9::float8[], \
                               $10::float8[], $11::bool[]) \
                     AS u(table_element_id, row, col, row_span, col_span, text, x0, y0, x1, y1, is_header)",
            )
            .bind(&flat.cell_table_ids)
            .bind(&flat.cell_rows)
            .bind(&flat.cell_cols)
            .bind(&flat.cell_row_spans)
            .bind(&flat.cell_col_spans)
            .bind(&flat.cell_texts)
            .bind(&flat.cell_x0s)
            .bind(&flat.cell_y0s)
            .bind(&flat.cell_x1s)
            .bind(&flat.cell_y1s)
            .bind(&flat.cell_is_headers)
            .execute(&mut *tx)
            .await?;
        }

        if !parsed.refs.is_empty() {
            let from: Vec<uuid::Uuid> = parsed.refs.iter().map(|r| r.from).collect();
            let to: Vec<uuid::Uuid> = parsed.refs.iter().map(|r| r.to).collect();
            let kinds: Vec<String> = parsed.refs.iter().map(|r| r.kind.clone()).collect();
            let metas: Vec<serde_json::Value> =
                parsed.refs.iter().map(|r| r.metadata.clone()).collect();
            sqlx::query(
                "INSERT INTO element_refs (from_element, to_element, kind, metadata) \
                 SELECT u.from_element, u.to_element, u.kind, u.metadata \
                   FROM UNNEST($1::uuid[], $2::uuid[], $3::text[], $4::jsonb[]) \
                     AS u(from_element, to_element, kind, metadata)",
            )
            .bind(&from)
            .bind(&to)
            .bind(&kinds)
            .bind(&metas)
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
    metadatas: Vec<serde_json::Value>,

    image_ids: Vec<uuid::Uuid>,
    image_widths: Vec<Option<i32>>,
    image_heights: Vec<Option<i32>>,
    image_datas: Vec<Vec<u8>>,

    blob_ids: Vec<uuid::Uuid>,
    blob_datas: Vec<Vec<u8>>,
    blob_mimes: Vec<Option<String>>,
    blob_filenames: Vec<Option<String>>,

    // table_cells staging (one entry per cell of every kind=table element).
    cell_table_ids: Vec<uuid::Uuid>,
    cell_rows: Vec<i32>,
    cell_cols: Vec<i32>,
    cell_row_spans: Vec<i32>,
    cell_col_spans: Vec<i32>,
    cell_texts: Vec<Option<String>>,
    cell_x0s: Vec<f64>,
    cell_y0s: Vec<f64>,
    cell_x1s: Vec<f64>,
    cell_y1s: Vec<f64>,
    cell_is_headers: Vec<bool>,
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
                    flat.metadatas.push(serde_json::json!({}));
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
                    flat.metadatas.push(serde_json::json!({}));

                    let (width, height, data) = image_payload(image.image_object);
                    flat.image_ids.push(image.id);
                    flat.image_widths.push(width);
                    flat.image_heights.push(height);
                    flat.image_datas.push(data);
                }
                ContentHandle::Aux(_) => {
                    let aux = index
                        .aux_at(order_idx)
                        .expect("aux element for aux content");
                    flat.ids.push(aux.id);
                    flat.pages.push(aux.page_number as i32);
                    flat.kinds
                        .push(ElementKind::from_aux(aux.kind).as_str().to_string());
                    flat.order_idxs.push(order_idx as i32);
                    // Annotation Contents lands in elements.text → FTS-able.
                    flat.texts.push(aux.text.clone());
                    flat.font_sizes.push(None);
                    flat.font_names.push(None);
                    flat.style_keys.push(None);
                    flat.push_bbox(aux.bbox.x0, aux.bbox.y0, aux.bbox.x1, aux.bbox.y1);
                    flat.metadatas.push(aux.metadata.clone());

                    if let Some(blob) = &aux.blob {
                        flat.blob_ids.push(aux.id);
                        flat.blob_datas.push(blob.data.clone());
                        flat.blob_mimes.push(blob.mime.clone());
                        flat.blob_filenames.push(blob.filename.clone());
                    }
                    if let Some(table) = &aux.table {
                        for cell in &table.cells {
                            flat.cell_table_ids.push(aux.id);
                            flat.cell_rows.push(cell.row as i32);
                            flat.cell_cols.push(cell.col as i32);
                            flat.cell_row_spans.push(cell.row_span as i32);
                            flat.cell_col_spans.push(cell.col_span as i32);
                            flat.cell_texts
                                .push((!cell.text.is_empty()).then(|| cell.text.clone()));
                            flat.cell_x0s.push(cell.bbox.0 as f64);
                            flat.cell_y0s.push(cell.bbox.1 as f64);
                            flat.cell_x1s.push(cell.bbox.2 as f64);
                            flat.cell_y1s.push(cell.bbox.3 as f64);
                            flat.cell_is_headers.push(cell.is_header);
                        }
                    }
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

/// Decode the corner-subscripted `bbox` projection columns (bx0..by1).
/// Postgres normalizes box corners (upper-right first); reorder to the
/// (min, min, max, max) convention parsed bboxes use.
fn decode_bbox(row: &PgRow) -> Result<Option<(f32, f32, f32, f32)>, StoreError> {
    let bx0: Option<f64> = row.try_get("bx0")?;
    let by0: Option<f64> = row.try_get("by0")?;
    let bx1: Option<f64> = row.try_get("bx1")?;
    let by1: Option<f64> = row.try_get("by1")?;
    Ok(match (bx0, by0, bx1, by1) {
        (Some(ax), Some(ay), Some(bx), Some(by)) => Some((
            (ax.min(bx)) as f32,
            (ay.min(by)) as f32,
            (ax.max(bx)) as f32,
            (ay.max(by)) as f32,
        )),
        _ => None,
    })
}

fn search_hit_from_row(row: &PgRow) -> Result<TextSearchHit, StoreError> {
    Ok(TextSearchHit {
        element_id: row.try_get("id")?,
        document_id: row.try_get("document_id")?,
        page: row.try_get("page")?,
        order_idx: row.try_get("order_idx")?,
        text: row
            .try_get::<Option<String>, _>("text")?
            .unwrap_or_default(),
        rank: row.try_get("rank")?,
    })
}

fn element_from_row(row: &PgRow) -> Result<ElementRow, StoreError> {
    let kind = ElementKind::parse(&row.try_get::<String, _>("kind")?)?;
    let bbox = decode_bbox(row)?;

    let image = match kind {
        ElementKind::Image => Some(ImagePayload {
            width: row.try_get("image_width")?,
            height: row.try_get("image_height")?,
            data: row
                .try_get::<Option<Vec<u8>>, _>("image_data")?
                .unwrap_or_default(),
        }),
        _ => None,
    };

    let blob = match kind {
        ElementKind::Blob => Some(BlobRow {
            data: row
                .try_get::<Option<Vec<u8>>, _>("blob_data")?
                .unwrap_or_default(),
            mime: row.try_get("blob_mime")?,
            filename: row.try_get("blob_filename")?,
        }),
        _ => None,
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
        blob,
        // Filled by `attach_table_cells` for kind=table rows.
        table_cells: None,
    })
}
