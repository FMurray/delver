-- Stage B slice 3 (docs/DECISIONS.md D-018): TABLE structure.
--   * table_cells: one row per grid cell of a kind=table element; the
--     table-level fields (n_rows, n_cols, strategy, confidence) live in the
--     table element's metadata jsonb.
--   * "row" is quoted: ROW is a reserved word in PostgreSQL.
-- Element kinds gain: table.

CREATE TABLE table_cells (
    table_element_id uuid NOT NULL REFERENCES elements (id) ON DELETE CASCADE,
    "row"            int NOT NULL,
    col              int NOT NULL,
    row_span         int NOT NULL DEFAULT 1,
    col_span         int NOT NULL DEFAULT 1,
    text             text,
    bbox             box,
    is_header        bool NOT NULL DEFAULT false,
    PRIMARY KEY (table_element_id, "row", col)
);
