-- Stage A slice 1: persistent index schema.
-- Lakebase-compatible by construction (docs/DECISIONS.md D-002):
--   * embeddings via pgvector,
--   * spatial via the native `box` geometric type + GiST (no PostGIS),
--   * full-text via a generated tsvector column + GIN.
-- Idempotent ingest (D-008): documents keyed by (corpus, sha256, parse_version).

CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE corpora (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name           text UNIQUE NOT NULL,
    partition_meta jsonb NOT NULL DEFAULT '{}',
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE documents (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    corpus_id      uuid NOT NULL REFERENCES corpora ON DELETE CASCADE,
    content_sha256 bytea NOT NULL,
    uri            text,
    page_count     int NOT NULL,
    parse_version  int NOT NULL,
    parsed_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (corpus_id, content_sha256, parse_version)
);

-- One row per parsed element; order_idx is the global document-order index
-- the in-memory PdfIndex uses, so hydration can rebuild identical state.
CREATE TABLE elements (
    id          uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES documents ON DELETE CASCADE,
    page        int NOT NULL,
    kind        text NOT NULL,
    order_idx   int NOT NULL,
    text        text,
    font_size   real,
    font_name   text,
    style_key   bigint,
    bbox        box,
    metadata    jsonb NOT NULL DEFAULT '{}',
    text_fts    tsvector GENERATED ALWAYS AS (to_tsvector('english', coalesce(text, ''))) STORED,
    UNIQUE (document_id, order_idx)
);

CREATE INDEX elements_bbox_gist ON elements USING gist (bbox);
CREATE INDEX elements_text_fts_gin ON elements USING gin (text_fts);
CREATE INDEX elements_document_page ON elements (document_id, page);

CREATE TABLE images (
    element_id uuid PRIMARY KEY REFERENCES elements ON DELETE CASCADE,
    width      int,
    height     int,
    data       bytea,
    caption    text,
    summary    text
);

-- Vector cache keyed by (element, model); dimension intentionally untyped for
-- now so multiple models can share the table (D-005).
CREATE TABLE embeddings (
    element_id uuid NOT NULL REFERENCES elements ON DELETE CASCADE,
    model      text NOT NULL,
    dim        int NOT NULL,
    embedding  vector NOT NULL,
    PRIMARY KEY (element_id, model)
);

CREATE TABLE index_meta (
    id             int PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    schema_version int NOT NULL,
    delver_version text NOT NULL,
    tokenizer      text
);
