-- Stage B slice 2 (docs/DECISIONS.md D-016..): non-TABLE spec types.
--   * element_refs: typed edges between elements (figure→image "contains",
--     figure→caption "caption-of"); document-level, carried alongside pages.
--   * blobs: embedded-file payloads for kind=blob elements.
--   * documents.metadata: PDF Info dict subset captured at ingest.
-- Element kinds gain: annotation, path, figure, blob (kind stays free text).

CREATE TABLE element_refs (
    from_element uuid NOT NULL REFERENCES elements (id) ON DELETE CASCADE,
    to_element   uuid NOT NULL REFERENCES elements (id) ON DELETE CASCADE,
    kind         text NOT NULL,
    metadata     jsonb NOT NULL DEFAULT '{}',
    PRIMARY KEY (from_element, to_element, kind)
);

-- Edges are loaded per document via from_element's document; index the
-- reverse direction for "what points at this element" lookups.
CREATE INDEX element_refs_to_element ON element_refs (to_element);

CREATE TABLE blobs (
    element_id uuid PRIMARY KEY REFERENCES elements (id) ON DELETE CASCADE,
    data       bytea NOT NULL,
    mime       text,
    filename   text
);

ALTER TABLE documents ADD COLUMN metadata jsonb NOT NULL DEFAULT '{}';
