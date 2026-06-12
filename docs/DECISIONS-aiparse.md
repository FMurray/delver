# ai_parse_document Backend + Scanned-PDF Detection — Decision Log (slice P1)

Append-only, same contract as `docs/DECISIONS.md` (D-001). Lives in its own
file by explicit instruction (merge-conflict avoidance with the other active
worktrees); numbering DA-001…. Branch `feat/aiparse-backend`, cut from
`6db0e7a`. Companion: `docs/scanned-detection.md` (research + decision rules).

---

**DA-001 · 2026-06-12 · Slice scope: document-level engine granularity.**
Owner decision: classification per page, decision per document — "a document
is parsed by exactly one engine per parse_version"; page-level engine mixing
is out of scope. Per-page records persist anyway (metadata `scan.pages`) so a
future page-level router needs no re-parse. Two parts: (1) scanned-PDF
detection at parse time + DocQL expressibility, (2) a Databricks
`ai_parse_document` parsing backend behind `delver index --engine`.

**DA-002 · 2026-06-12 · Scan signals captured inside the existing walk; the
legacy image bbox is NOT trusted.**
New `delver_core::scan` module + transient `PageContents.scan` signals
(`cell_fragments` pattern: empty on hydrated pages, never persisted per page).
Capture points in `parse.rs`:
- **Image coverage**: at `Do` time the device envelope is the CTM image of
  the unit square (PDF 32000-1 §8.9.5.2), accumulated into a 64×64 occupancy
  grid (union coverage, overlap-safe, deterministic, exact to ~1.6%/axis).
  Deliberately does NOT reuse `ImageElement.bbox`, which is a known-broken
  placeholder (transforms point (100,100) instead of (1,1) — ~100× off under
  the standard scaled-unit-square CTM). Fixing that bbox changes persisted
  geometry and figure grouping, so it is left for its own slice; scan
  coverage must not inherit the bug. Filter names (`/Filter` name or array)
  recorded per page.
- **Text by rendering mode**: at each show op (`Tj/TJ/'/"`), the glyph-count
  delta is attributed to the current `Tr` mode — mode 3 = invisible (the
  Acrobat/ocrmypdf/ABBYY OCR-layer convention). O(1) per op; no per-glyph
  signature churn.
- Blind spots (documented, accepted): inline images (`BI/ID/EI`) and content
  inside Form XObjects are not walked — pre-existing parser scope; the
  DA-003 producer escalation is the backstop.

**DA-003 · 2026-06-12 · Classification rules, aggregate, and persistence.**
Page classes `native | scanned_no_text | scanned_ocr | mixed`; document
aggregate `{class, scanned_page_ratio, confidence, producer?, creator?,
producer_hint?, pages[]}`. Thresholds (constants in `scan.rs`, rationale +
references in docs/scanned-detection.md): coverage 0.75 (docling
`bitmap_area_threshold` default), OCR rule = coverage ≥ 0.5 ∧ invisible
ratio ≥ 0.5, sparse-text floor 50 visible chars (Bates stamps), doc bands
0.8/0.2. Producer/Creator fingerprints are informational PLUS exactly one
rule: zero-text + zero-recognized-imagery docs escalate native →
scanned_no_text at confidence 0.5 when the producer matches a scanner/OCR
fingerprint (the inline-image blind spot). `parse_document` attaches the
block to `ParsedDocument.metadata.scan` → persists through the existing
`documents.metadata` jsonb (NO migration), returns via `load_document`, and
the `delver index` receipt echoes it (receipt reads stored metadata via new
`DelverStore::document_metadata` — one scalar query, no element load).
Fresh-parse query paths compute the block and ignore it; the two 10-K
regression baselines re-verified byte-identical (414 534 / 466 678, stderr 0).

**DA-004 · 2026-06-12 · DocQL expression v1 = `--where scan.class`; store
filter generalized to whole-metadata containment.**
`text_search_filtered`/`documents_matching` now take the FULL containment
object (`metadata @> $n::jsonb`) instead of internally wrapping a partitions
object; the service layer builds `{"partitions": {...}, "scan": {...}}` from
`--where` pairs (`metadata_filter_json`). Reserved prefix `scan.`: v1
supports exactly `scan.class=<native|scanned_no_text|scanned_ocr|mixed>`;
invalid classes and other `scan.*` keys are compile-the-request errors
listing the supported set (D-006) — containment on non-string scan fields
(ratios) would silently never match, hence the strict gate. Partition
`--where` behavior is byte-identical (same SQL result; partition_cli green).
Future grammar hook (Document/Corpus-level predicate) documented in
scanned-detection.md §3, deliberately NOT implemented this slice.

**DA-005 · 2026-06-12 · ai_parse_document invocation + output schema (research).**
Public docs (cited):
- Function + output schema 2.0: <https://docs.databricks.com/aws/en/sql/language-manual/functions/ai_parse_document>
  and <https://learn.microsoft.com/en-us/azure/databricks/sql/language-manual/functions/ai_parse_document>.
  VARIANT shape: `document.pages[] {id (0-based), image_uri}`,
  `document.elements[] {id, type, content, confidence, bbox[{coord[4],
  page_id}], description}`, `error_status[] {error_message, page_id}`,
  `metadata {id, version, file_metadata{…}}`. Element types: text, table,
  figure, title, caption, section_header, page_header, page_footer,
  page_number, footnote. Tables are HTML in `content`; figure `content` may
  be NULL with AI `description` (v2.0); bboxes are pixel coordinates of the
  rendered page, top-left origin. Limits: 500 pages, 100 MB; DBR 17.3+/
  serverless env 3+. Versioning contract: minor = additive, major = breaking
  → the mapper accepts `2.x` and refuses everything else.
- SQL Statement Execution API: <https://docs.databricks.com/aws/en/dev-tools/sql-execution-tutorial>
  (`POST /api/2.0/sql/statements`, `GET /api/2.0/sql/statements/{id}`,
  states PENDING/RUNNING/SUCCEEDED/FAILED/CANCELED/CLOSED, JSON_ARRAY +
  INLINE ≤ 25 MiB, `manifest.truncated`).
- Files API (UC volumes): <https://docs.databricks.com/api/workspace/files/upload>,
  <https://docs.databricks.com/aws/en/volumes/volume-files>
  (`PUT|DELETE /api/2.0/fs/files{path}`, `overwrite=true`, octet-stream).

**DA-006 · 2026-06-12 · Client shape (`delver-parse-dbx`): ureq, pure
helpers, fail-loud everywhere.**
New workspace crate mirroring delver-embed: `ureq 2.12.1` (Cargo.lock-pinned;
builds `--offline`), pure unit-tested helpers for every body/parse
(`statement_body`, `statement_state`, `extract_parsed_json`,
`files_api_url`, `temp_volume_path`), no network in any test. Flow: upload →
execute `SELECT to_json(ai_parse_document(content, map('version','2.0')))
FROM READ_FILES('<path>', format => 'binaryFile')` → poll (2 s interval,
600 s budget; unknown states are errors, never an infinite loop) → fetch
`result.data_array[0][0]` (truncated ⇒ error) → DELETE temp file (best
effort: a leaked temp file warns on stderr, never fails the parse — the
parse result is already in hand and the file is uniquely named). The volume
path is embedded as a SQL literal because `READ_FILES` takes a constant
path, not a parameter marker; the path is server-generated
(UUID + charset-sanitized file name) so it is injection-free by
construction, and quote/backslash are rejected as defense in depth.
`to_json` avoids depending on VARIANT's wire encoding under JSON_ARRAY.

**DA-007 · 2026-06-12 · Env config surface (strict, fail-loud, one error
naming every missing var).**
`DbxConfig::from_env`: `DATABRICKS_HOST` + `DATABRICKS_TOKEN`, or
`DELVER_DBX_PROFILE` naming a `~/.databrickscfg` profile (minimal INI reader;
explicit env vars win per key); plus `DELVER_DBX_WAREHOUSE_ID` and
`DELVER_DBX_VOLUME` (must start `/Volumes/`). The user's `.databrickscfg`
holds 22 profiles of which only 2 carry `token =` (the rest are
OAuth `auth_type` profiles) → token-less profiles are rejected BY NAME with
the PAT-vs-OAuth explanation rather than a vague missing-key error. Config
is resolved via an injectable lookup (`from_lookup`) so tests never mutate
process env. Nothing contacts a workspace unless the caller selected the
ai-parse engine with complete config; the live path ships env-gated and OFF
(`DELVER_DBX_LIVE=1` + full config; the test skips with a message listing
the required vars).

**DA-008 · 2026-06-12 · Mapper: ai_parse output → ParsedDocument → existing
ingest path; one engine per parse_version falls out of D-008.**
`map_ai_parse_response` builds a `ParsedDocument` and the stock
`ingest_parsed` persists it — no new store writes, same element rows /
`table_cells` / bulk-insert path, and the D-008 dedup key (corpus, sha256,
parse_version) makes re-ingest under a different engine a no-op returning
the existing document: exactly one engine per parse_version by construction
(the receipt's `engine` field reads the STORED doc's metadata so a no-op is
visible). Mapping decisions:
- 8 textual types → kind=text rows (page = bbox[0].page_id + 1, bbox =
  coord). `TextElement` has no metadata slot, so the fine-grained type tag
  (title vs section_header vs footnote) is dropped in v1 — recorded here as
  a known loss; revisit when DocQL grows type-aware text selectors. Font
  fields stay empty (ai_parse exposes none) → font-similarity matching
  degrades on ai-parse docs; Text/Regex/FTS matching works.
- `table` → kind=table + `table_cells` via a minimal deterministic HTML
  scanner (`<table>/<thead>/<tr>/<td|th>`, rowspan/colspan placed with the
  standard occupancy algorithm; `<th>`/thead ⇒ is_header; entities decoded;
  spans live on anchor cells, matching the table_cells PK). New
  `TableStrategy::AiParse` ("ai-parse") variant so strategy round-trips
  hydration. Per-cell geometry does not exist in ai_parse output → cell
  bboxes are zero (honest, not the table bbox).
- `figure` → kind=figure; the AI `description` becomes element text (FTS-able)
  and metadata `{source, description, confidence}`.
- Provenance in `documents.metadata`: `{"parser": "ai_parse_document",
  "parser_version", "parser_run_id", "bbox_space": "pixels"}`. Native docs
  carry NO `parser` key (absence = native) — zero regression surface on the
  native path. Bboxes are stored in ai_parse's pixel space as-is
  (top-left origin matches; the unit does not — converting without the
  render DPI would be fabrication), flagged by `bbox_space`.
- Fail-loud: non-empty `error_status` (partial parses are corruption, not a
  warning), non-2.x schema versions, unknown element types, missing
  bbox/page_id, unparseable table HTML — all hard errors naming the element.

**DA-009 · 2026-06-12 · CLI/facade engine surface and `auto` routing.**
`delver index --engine native|ai-parse|auto` (clap ValueEnum; default native
— the native path still calls `ingest_document` verbatim). `ai-parse`:
`DbxConfig::from_env()` first, one error listing every missing var.
`auto`: native parse runs first (needed for classification; reused for
native ingest, so nothing parses twice), then `scan.class`:
scanned_* + configured → ai-parse (the routing scan block is merged into the
stored metadata next to the parser provenance); scanned_* + unconfigured →
error carrying BOTH the classification (class + ratio + confidence) and the
missing config; otherwise native. Receipt gains `"engine"` and `"scan"`
(both read from stored metadata, so idempotent re-ingests report the
original engine truthfully). Python facade: `ingest(..., engine=None)` with
the same names. Receipt keys are additive — store_cli/partition_cli
assertions are key-based and stay green.

**DA-010 · 2026-06-12 · Verify gate (run for real).**
`DATABASE_URL=postgres://delver:delver@localhost:5433/delver_aiparse cargo
test --offline --workspace`: 35 suites, 189 passed, 0 failed, 0 skipped (DB
tests executed against the dedicated empty `delver_aiparse`; migrations
applied by `DelverStore::connect`). `cargo check --workspace --offline`
clean (pre-existing viewer warnings only). Shared-DB read-only baselines:
`query --doc 1129cea2…` = 414 534 bytes, `query --doc 56e30967…` = 466 678
bytes, stderr 0 on both (native path untouched). Demo: hand-generated
full-page-CCITT PDF at /tmp indexed into `delver_aiparse` → receipt
`scan.class=scanned_no_text`, coverage 1.0, filters ["CCITTFaxDecode"],
engine "native", confidence 1.0.
