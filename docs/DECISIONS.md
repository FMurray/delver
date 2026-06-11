# DocQL Full-Spec Implementation — Decision Log

Append-only. Each entry: what was decided, why, and what it constrains. This file replaces an
upfront spec by explicit choice (D-001). Companion context: `docs/landscape-report-2026-06-11.md`
(in main worktree), DocQL design doc (Google Docs, "DocQL / Delver Design Doc").

---

**D-001 · 2026-06-11 · Workflow: iterative loop, no upfront spec.**
Owner waived the formal spec ("document choices as you go"). Operating loop:
gather context → take action → verify → document → repeat. Every slice ends with
`cargo test` green and an entry here, committed together with the code it describes.
Work happens in worktree `~/delver-docql`, branch `feat/docql-full-spec`, cut from
`feat/viewer-mlflow-integration` @ `b643f63` (preserves uncommitted viewer WIP in the main tree).

**D-002 · 2026-06-11 · Index store = Postgres, Lakebase-compatible.**
Owner decision: "postgres so we can use lakebase which will be the best for databricks."
Consequences: SQLx (postgres feature); embeddings in **pgvector** (available on Lakebase);
spatial via **native `box` geometric type + GiST index** (no PostGIS assumption — revisit if
the target Lakebase instance has PostGIS); text search via **generated `tsvector` column + GIN**;
schema versioned with sqlx migrations plus an `index_meta` table (schema_version, delver_version,
tokenizer id). Local dev: `pgvector/pgvector:pg17` via `docker-compose.dev.yml`, port **5433**,
`DATABASE_URL=postgres://delver:delver@localhost:5433/delver`.

**D-003 · 2026-06-11 · Crate boundary: new `delver-store`; core stays pure.**
`delver-core` remains sync with no DB dependency (hot parse/match path). `delver-store` is
async (tokio + SQLx) with a small blocking facade for CLI/Python. The in-memory `PdfIndex`
structures (RTree, style buckets, SoA stores) are **derived data**: Postgres is the source of
truth; hydration rebuilds them. Contract: persist → hydrate → match results must be identical
to in-memory results (round-trip test enforces this).

**D-004 · 2026-06-11 · Staging: A persistence → B types/matchers → C UDT+corpus.**
Stage A: persistent index + query API. Stage B: TABLE structure, FIGURE, ANNOTATION, BLOB,
POINT/PATH, DOCUMENT, Ref edges; make Regex/Heuristic/EmbeddingSim/semantic-chunking execute.
Stage C: `TYPE … AS TABLE` UDTs, SubCorpus + template interpolation, partition-scoped queries.

**D-005 · 2026-06-11 · Embeddings: `Embedder` trait, HTTP endpoint first.**
First implementation targets Databricks serving-endpoint API shape (spec example:
`EmbeddingSim("…", threshold=0.6, endpoint="databricks-bge")`), plus a deterministic mock for
tests. Vectors cached in the `embeddings` table keyed by (element_id, model). No model weights
or downloads in this public repo. Local backend may be added later behind the same trait.

**D-006 · 2026-06-11 · Fail-loud matchers.**
Recon found `Regex()`, `Heuristic()`, and `Cosine()/EmbeddingSim` parse but silently never
execute. Under this work, a matcher that cannot execute (unimplemented, missing endpoint,
invalid config) is a hard error surfaced to the caller — never a silent no-op.

**D-007 · 2026-06-11 · Test-data compliance (repo is PUBLIC).**
All real corpora live in `~/datasets/` (outside the repo; `DELVER_TESTDATA` overrides).
The repo carries only `scripts/fetch-testdata.sh`. Never commit, redistribute, or deploy
datasets. Approved for local testing: FinanceBench (SEC filings; QA labels CC-BY-NC → local
dev only), OfficeQA (HF-gated; public-source Treasury Bulletins; includes hard-133 =
`officeqa_pro.csv`), OmniDocBench (optional; layout ground truth). **PROHIBITED (customer-derived):
PMBench, CustomerBench, Ares hackathon data.**

**D-008 · 2026-06-11 · Idempotent ingest.**
`documents` are keyed by sha256 content hash + `parse_version`. Re-ingesting identical bytes
with the same parser version is a no-op; bumping parse_version re-parses without losing prior
runs. This implements the spec FR "re-run extraction with different configuration without full
reprocessing" and gives extraction runs provenance.

**D-009 · 2026-06-11 · CI test fixtures are synthetic.**
Integration tests generate small PDFs in-test via lopdf (no binary fixtures committed).
DB-backed tests require `DATABASE_URL` and skip with an explicit message when absent.
Real-corpus evaluation (3M 10-K, OfficeQA subset) runs locally only, from `~/datasets/`.

**D-010 · 2026-06-11 · delver-core surface opened for the store: one read-only accessor.**
`PdfIndex::style_key_bits(TextHandle) -> Option<u64>` exposes the packed per-row style
signature so ingest can persist it (`elements.style_key`). Zero behavior change; the
`StyleKey` type and buckets stay private. Persisted style keys are **informational only**:
font ids inside the key come from a process-local interner, so they are not comparable
across runs. Hydration never reads them back — it recomputes all style state.

**D-011 · 2026-06-11 · delver-store slice-1 shape: hydration reuses the fresh-parse code path.**
Equivalence (D-003) is guaranteed structurally, not re-implemented: hydration rebuilds the
exact `BTreeMap<page, PageContents>` shape `get_page_content` produces (rows re-added in
global `order_idx` order) and calls the same `PdfIndex::new` the fresh path uses.
Supporting choices:
- `ingest_parsed(corpus, uri, bytes, pages, parse_version)` exists alongside
  `ingest_document`: callers that already parsed keep their element ids verbatim
  (parse-time UUIDs are per-run, so the round-trip/match-equivalence tests require it),
  and double parsing is avoided. Both share the D-008 dedup path.
- `bbox` is written as `box(point(x0,y0), point(x1,y1))` and read back by corner
  subscripting (`(bbox[1])[0]` etc.), so no geometric-type decoding is needed; Postgres
  normalizes corners, rows are re-normalized to the parser's (min,min,max,max) convention.
  Parser bboxes are always corner-ordered (min/max construction), so this is lossless.
- Image rows persist stream bytes + Width/Height; hydration rebuilds a minimal lopdf
  XObject carrier (not byte-identical to the original object — index behavior only reads
  id/page/bbox, never the object).
- Runtime `sqlx::query` only (no compile-time `query!` macros): builds never need a live
  DB or offline cache. Ids are `#[sqlx(transparent)]` newtypes (CorpusId/DocumentId/ElementId).
- Blocking facade `blocking::DelverStoreBlocking` wraps a private current-thread runtime.
- Dev note: when Docker Desktop is gated by org sign-in, a local Homebrew Postgres 17 +
  pgvector serving `postgres://delver:delver@localhost:5433/delver` satisfies the same
  contract; the slice-1 tests were verified against it (4/4 green, none skipped).

**D-012 · 2026-06-11 · Slice-2 CLI surface, Python facade, and `process_parsed` extraction.**
The `delver` binary is now subcommand-based (clap): `process` (the pre-subcommand CLI,
verbatim flags and behavior), `index`, `query`, `search`. Database URL precedence everywhere:
`--db` flag > `DATABASE_URL` env > the D-002 local dev default.
- `index <pdf> --corpus <name> [--uri] [--parse-version=1] [--db]` → ingest via
  `DelverStoreBlocking::ingest_document` (D-008 idempotency holds through the CLI), prints
  `{"document_id","created","element_count","corpus"}`. `DelverStore::element_count` was
  added (COUNT query) so the receipt does not load full rows/image bytes.
- `query --template <path> (--doc <uuid> | --pdf <path>) [--db] [--pretty] [--tokenizer-model]`
  → `--pdf` runs the fresh `process_pdf` pipeline; `--doc` loads rows, `hydrate_pages`, and
  executes the same pipeline. To enable this, delver-core gained
  `process_parsed(pages, match_context, template_str, tokenizer) -> Result<String>`: a pure
  refactor-extract of the back half of `process_pdf` (index → align → process → serialize via
  a shared private `run_template`; `process_pdf` routes through it with identical operation
  order, so the fresh path is behavior-identical). The hydrated path passes
  `MatchContext::default()` — named destinations are not persisted yet (same Stage A
  limitation as D-011 hydration). Verified on the 3M 2015 10-K: fresh vs hydrated outputs are
  equal on all stable fields (175/175 chunks, texts, metadata, pages 1–45) — element UUIDs
  are per-parse and excluded per D-011. `--tokenizer-model none` disables token chunking
  explicitly; an unfetchable model warns on stderr and falls back to character chunking
  (`process` keeps its original silent fallback). Output is compact JSON unless `--pretty`
  (`process` keeps its always-pretty output).
- `search <query> --corpus <name> [--doc <uuid>] [--limit=10] [--db]` → `text_search`
  (corpus scope, or document scope when `--doc` given), prints a JSON array of
  `{"element_id","document_id","page","rank","snippet"}` (snippet = element text truncated
  to 200 chars).
- Python facade: `delver_pdf.DelverStore` (pyclass) wraps `DelverStoreBlocking` with
  `new(db_url=None)`, `ingest(path, corpus, uri=None, parse_version=None)`,
  `search(query, corpus, limit=None)`, `run_template(doc_id, template, tokenizer_model=None)`
  where `template` is DocQL source text. CLI and Python route through one shared service
  layer in `crates/delver/src/lib.rs` (`ingest_file`/`search_store`/`run_template_on_doc`),
  so the JSON shapes cannot drift. `process_pdf_file` is unchanged.
- Tests: `crates/delver/tests/store_cli.rs` drives the real binary
  (`env!("CARGO_BIN_EXE_delver")`, no assert_cmd dependency) through
  index → re-index (idempotent) → `query --doc` vs `query --pdf` (stable-field equality) →
  corpus/document search, using the D-009 synthetic-PDF + skip-without-DB pattern
  (PDF builder duplicated from delver-store/tests/roundtrip.rs by design — no shared
  test-util crate for ~60 lines).

**D-013 · 2026-06-11 · Library stdout is reserved for data: debug prints moved to stderr.**
`index`/`query`/`search` print exactly one JSON document on stdout so they compose with
pipes and tests parse stdout wholesale. delver-core's match pipeline contained ~50 stray
debug `println!`s (matcher, search_index, layout, docql) that broke that contract; they are
now `eprintln!` (computation untouched, diagnostics preserved on stderr). `logging.rs` is
unchanged (its prints only fire under the `process` subcommand's debug subscriber).
Related hygiene: `flamegraph` was removed from delver's dev-dependencies — it is the
`cargo flamegraph` subcommand, not a library, and it forced every `cargo test -p delver`
to build the cargo-toolchain dependency tree (which also blocked offline test runs).

**D-014 · 2026-06-11 · Stage B slice 1: match rules execute for real, fail-loud (D-005/D-006).**
`EmbeddingSim`/`Regex`/`Heuristic` were parse-only; they now execute, and every config that
cannot execute is a hard error — at template compile where possible, at match time otherwise.
- **Embedder trait in core, zero new core deps**: `delver_core::embed::Embedder`
  (`embed(&[&str]) -> Result<Vec<Vec<f32>>, EmbedError>`, `Send + Sync`). Threading follows the
  MatchContext flow: `MatchContext.embedder` (a `SharedEmbedder` newtype over
  `Option<Arc<dyn Embedder>>`, kept so `Debug`/`Default` derives and `catch_unwind` keep working)
  is copied into `PdfIndex` by `PdfIndex::new` — the previously ignored `_match_context`
  parameter now earns its keep — and the matcher reads `index.embedder()`. `process_pdf` gained
  an `embedder: Option<Arc<dyn Embedder>>` parameter; `process_parsed` callers set it on the
  context they already pass.
- **Grammar/config**: canonical `EmbeddingSim("q", threshold=0.6, endpoint="…", model="…")`;
  `Cosine`/`Semantic` parse as aliases of the same `MatchType::EmbeddingSim`. `MatchConfig` gained
  typed `endpoint`/`model` fields, a `name` field (source match-definition name, so errors can
  name the match block), and `compiled_regex` (compiled+cached at template compile; invalid
  pattern → compile error quoting the pattern). `ComparisonValue::Number` is now `f64` as written
  (the old i64 fixed-point ×1000 encoding was ambiguous at evaluation time).
- **Execution semantics** (all scoped to the same `[start, max)` document range the Text matcher
  uses): Regex = `is_match` over element text, score 1.0, document order. Heuristic = comparisons
  ANDed per element, score 1.0; properties: fontSize/font_size, fontName/font_name (equality on
  canonicalized names, case-insensitive), page/page_number, x0/x/x_position, y0/y/y_position, x1,
  y1, textLength/text_length, text (exact equality); unknown property → compile error listing the
  set; strings only allow ==/!=. EmbeddingSim = embed query once + scoped candidates as one batch,
  cosine ≥ threshold, ranked by similarity; no embedder configured → match-time error naming the
  match block (template `endpoint=` is recorded and echoed in that error, but backend selection is
  the caller's `--embed-endpoint`/env for now — per-config endpoints are future work).
- **Combinators**: `FirstMatch(...)` is real (alternatives tried in order, first non-empty result
  wins) — and a match definition with several executable clauses now resolves to an implicit
  FirstMatch instead of silently dropping all but the first. `Optional(...)` errors
  "not yet implemented" (semantics need non-optional failure to mean something first).
- **Fail-loud sweep**: unknown match functions, malformed args, bare-value clauses, references to
  unknown or empty match definitions, non-config `match=` attribute values, and `Custom` array
  types are all template-compile errors now; `align_template_with_content` returns
  `Result<Option<…>>` so match-time failures propagate to callers (CLI: non-zero exit, stderr
  message, zero stdout — verified). Tests asserting the old inert behavior (FirstMatch/Optional
  parked as FunctionCalls, CustomFunction tolerated, empty definition referenced silently,
  fixed-point comparison numbers) were rewritten to assert the new contracts.

**D-015 · 2026-06-11 · delver-embed crate: Databricks + mock backends; offline-lock dependency rule held.**
New workspace member `delver-embed` (NOT a delver-core dependency) implements the core trait:
- `DatabricksEmbedder`: endpoint name → `https://$DATABRICKS_HOST/serving-endpoints/{name}/invocations`
  (scheme/trailing-slash tolerant) or full URL as-is; Bearer `$DATABRICKS_TOKEN` required. Request
  body `{"input": [...]}` (+ optional `"model"`); accepts `{"data":[{"embedding":…,"index":…}]}`
  (reordered by index) and `{"predictions":[[…]]}` response shapes, rejects others naming the keys
  found, and enforces vector-count == input-count. Pure helpers (`request_body`, `parse_response`,
  `resolve_endpoint_url`) are unit-tested against canned JSON — no network in any test.
- `MockEmbedder`: seeded `HashMap<String, Vec<f32>>`; all seeds padded with a trailing 0 component
  and unknown texts embed to the extra-axis unit vector — orthogonal to every seed by construction.
  Used by delver-core's matcher tests via a dev-dependency cycle (allowed by cargo).
- **Dependency approach**: HTTP client is `ureq 2.12.1` + `features=["json"]` — already pinned in
  Cargo.lock (hf-hub transitively) with its closure in the local cargo cache, so the firewalled
  crates.io index was never needed; the whole slice builds/tests with `--offline`. No new external
  crates were added anywhere.
- **CLI/Python wiring**: `delver query|process --embed-endpoint <name-or-url>` (fallback
  `$DELVER_EMBED_ENDPOINT`; precedence mirrors `--db`/`DATABASE_URL`), service-layer
  `build_embedder` shared by both subcommands and the PyO3 facade;
  `DelverStore.run_template(doc_id, template, tokenizer_model=None, embed_endpoint=None)`.
  Verified: the 3M 10-K `query --doc` output is byte-identical to the pre-change baseline
  (no behavior change without embedding configs).

**D-016 · 2026-06-11 · Stage B slice 2: non-TABLE spec types end to end (ANNOTATION, PATH, FIGURE+refs, BLOB, DOCUMENT metadata).**
TABLE structure is deliberately deferred to the next slice. New parse entry point
`delver_core::parse::parse_document(doc) -> ParsedDocument { pages, refs, metadata }`; both
`process_pdf` (fresh query) and `DelverStore::ingest_document` route through it, so fresh and
ingested element sets stay identical. `get_page_content` keeps its signature (now also
emitting paths/annotations/page blobs); figure grouping, document-level blobs, and Info
metadata exist only via `parse_document`. Hydration never re-runs any extraction — rows
round-trip verbatim, so pre-slice documents (the 414534-byte 10-K regression) are untouched.
- **Core model — one kind-tagged aux store, not four**: `AuxElement { id, kind: AuxKind
  {Annotation|Path|Figure|Blob}, page_number, bbox, text, metadata: serde_json::Value,
  blob: Option<BlobPayload> }`, `ContentHandle::Aux`, `PageContent::Aux`, row-oriented
  `AuxStore` (these kinds are sparse and never on a hot loop; four SoA stores would be
  near-identical plumbing ×4). `TextElement`/`ImageElement` untouched. Aux elements are
  **transparent to all existing matching**: TextChunk collection, boundary candidates, the
  section-end font-similarity fallback, and `top_k_similar_text` all skip them — verified by
  the 10-K re-index: `query --doc` over the v2 document (14 977 interleaved paths, 79
  annotations) is byte-identical to the v1 output.
- **ANNOTATION**: per-page `Annots` entries → kind=annotation elements; text = `Contents`
  (decoded UTF-16BE/lossy-UTF-8; lands in `elements.text`, so it is FTS-able), bbox = `Rect`
  flipped to top-left coordinates, metadata `{subtype, uri?, dest?}` (`A`/URI action, `A`/D
  or `Dest` names). Appearance streams are not rendered. `FileAttachment` annotations become
  kind=blob elements instead (the attached file is the payload of interest).
- **PATH**: captured during the existing content-stream walk (page streams only; paths
  inside Form XObjects are out of scope). Construction ops m/l/c/v/y/re/h accumulate points
  (curve control points included — conservative envelope), each point CTM-transformed and
  flipped at capture; painting ops S/s/f/F/f*/B/B*/b/b* emit one element per painted path
  (`n` discards, clip-only paths are not elements). bbox = point envelope; metadata
  `{op_count, stroke, fill, point_count, points: first 32 [x,y] pairs}` — the points are the
  hook for future table-rule detection. **Cap**: 512 paths/page; past it, painted paths are
  counted, not captured, and the overflow note `{path_overflow: {cap, skipped}}` is merged
  into the metadata of the page's last captured path (there is no page-level metadata slot
  anywhere in the model or schema — sentinel-free and queryable). Real 10-K: the cap engaged
  on 6 of 158 pages.
- **FIGURE + Ref edges**: conservative grouping — image + caption text line matching
  `(?i)^\s*(figure|fig\.|table|chart|exhibit)\b`, same page, horizontal overlap required,
  vertical gap ≤ 50pt; nearest such line below the image, else nearest above. Emits
  kind=figure (bbox = union) plus document-level `RefEdge { from, to, kind, metadata }`
  edges figure→image ("contains") and figure→caption ("caption-of") carried alongside pages
  (never inside elements). No caption ⇒ no figure (additive, never destructive). Caption
  text/ids are also denormalized into figure metadata `{caption, image_id, caption_id,
  caption_position}` so template output needs no edge lookup at match time.
- **BLOB**: embedded files from the catalog `EmbeddedFiles` name tree (Names+Kids walk,
  depth-capped) and `FileAttachment` annotations. Bytes/mime/filename live in a
  `BlobPayload` beside the element (not in metadata JSON); metadata carries
  `{source, filename, mime, size}`. Document-level blobs sit on **synthetic page 0** (they
  belong to no page; page 0 orders ahead of all real pages); `ParsedDocument::page_count()`
  excludes it, so `documents.page_count` stays the real page count.
- **DOCUMENT metadata**: PDF Info dict subset `{title, author, subject, creation_date}` (all
  strings) captured at ingest into a new `documents.metadata jsonb`; exposed via
  `load_document`.
- **Persistence (migration 0002_types.sql, SCHEMA_VERSION=2)**: `element_refs(from_element,
  to_element, kind, metadata, PK(from,to,kind))` + reverse index; `blobs(element_id PK, data,
  mime, filename)`; `documents.metadata jsonb not null default '{}'`. Elements now persist
  their `metadata` jsonb at ingest (was always-default). `ingest_parsed` takes
  `&ParsedDocument` (shared insert path with `ingest_document` per D-011);
  `load_document` returns `LoadedDocument { document_id, metadata, elements, refs }` and
  element rows of kind=blob carry their payload (blobs LEFT JOIN). Round-trip (D-003) extended:
  counts by kind, figure edges, blob bytes, and document metadata all survive
  persist → load → hydrate (delver-store/tests/roundtrip.rs::roundtrip_new_kinds_refs_and_metadata).
- **Template queryability (minimal)**: `Annotation(as="…")` and `Figure(as="…")` are element
  selectors. No .pest change was needed — element names are generic identifiers in the
  grammar; the two names map to `ElementType::Annotation/Figure` in `process_element`
  (deviation from the "add to the grammar" instruction, with the same effect). They join
  Pass-2 content assignment exactly like TextChunk (full document when no sections; the
  section partition when nested; section-boundary scoping verified by test). Every matched
  element of the kind produces one output: `AnnotationOutput { id, page_number, bbox, text,
  metadata, parent_* }` / `FigureOutput { …, caption, image_id, … }` (`type` tags
  "Annotation"/"Figure"); output metadata = inherited template metadata overlaid with the
  element's own, plus `name` = the `as=` value (sections keep using `section`).
- Tests: crates/delver/tests/spec_types.rs (9 tests: parse-level per kind, figure-negative,
  Info metadata, selectors top-level / inside section / boundary-scoped) and the extended
  store round-trip. Real-document evidence (3M 10-K re-indexed at `--parse-version 2`,
  document 1d983d90-1042-4b6e-a70a-cfb690265afc): text 11 476 (identical to v1 — text
  pipeline untouched), annotation 79 (all Link, dest-style), path 14 977, no images detected
  in this PDF (same as v1) hence no figures, no embedded files. Document metadata captured
  `{title: "10-K - 02/11/2016 - 3M Company", creation_date: …}`.

**D-017 · 2026-06-11 · Matcher/collation debug output gated behind tracing (default stderr: quiet).**
Slice-1 verification found ~200KB of unconditional stderr per real-document query
(`align_template_with_content_with_depth: …`, boundary-candidate traces, chunk dumps) from
the D-013 `eprintln!` conversion. All 50 of those call sites in matcher.rs, search_index.rs,
layout.rs, and docql.rs are now `tracing::debug!` — quiet unless a subscriber is installed
(the `process` subcommand's `init_debug_logging`/`--debug-ops` pathway; `index`/`query`/
`search` install none). stdout untouched. Measured on the 10-K regression query: stderr
204 849 → 0 bytes, stdout byte-identical (414 534). Intentional user-facing warnings (e.g.
the tokenizer fallback in the service layer) stay on stderr.

**D-018 · 2026-06-11 · Stage B slice 3: TABLE structure end to end (detect → structure → output → persist → hydrate).**
`make TABLE real`: tables are detected at parse time, become kind=table elements, are selectable in
templates, persist to Postgres, and hydrate back bit-faithfully. Detection is deterministic and
parse-time-only; hydration never re-runs it (D-011/D-016 round-trip holds verbatim).
- **Cell fragments (new parse side-channel)**: SEC HTML-to-PDF filings draw a whole visual row as
  one text run with multi-space gaps between cells ("28.9%␣␣28.1%"), so post-grouping TextElements
  are row-grained and even raw runs are line-grained. `finalize_text_run` now also emits
  `CellFragment`s — the run split at ≥2 consecutive whitespace glyphs or an inter-glyph x-jump
  ≥ clamp(0.5×glyph-height, 2.5, 8)pt, bboxes from the per-glyph boxes (whitespace never inflates
  an extent). TextElement production is untouched (`process_glyph` pushes exactly one buffer char
  per glyph, so glyphs↔chars stay 1:1); fragments live in `PageContents.cell_fragments`, are
  consumed by detection at the end of the page walk, then dropped — never persisted, always empty
  on hydrated pages.
- **Core model**: `table::TableStructure { bbox, page, n_rows, n_cols, cells, strategy,
  confidence }`, `TableCell { row, col, row_span, col_span, bbox, text, is_header }` (spans always
  1 this slice — span *detection* was deliberately cut: missing-edge merging misfires on
  zebra-striped tables whose unshaded rows have no rules at all; the fields exist so consumers and
  the schema need no change when it lands). Tables are `AuxKind::Table` aux elements with the
  structure carried beside the element (`AuxElement.table`, the D-016 BlobPayload pattern — never
  inside metadata JSON); element metadata carries exactly `{n_rows, n_cols, strategy, confidence}`.
  Element placement: handles are inserted into the page order at reading position (before the
  first text element starting below the table top), not appended at page end like annotations —
  this keeps tables on the correct side of section headings that split a page (verified: the
  page-24 effective-tax table physically above "PERFORMANCE BY BUSINESS SEGMENT" stays out of that
  section). Aux transparency (D-016) keeps all text matching byte-identical.
- **Detection strategies** (priority order; each consumes its evidence; strategy + confidence
  recorded per table; candidates <2×2 after dropping fully-empty rows/columns rejected):
  1. **ruled** — painted paths become axis-aligned rules: thin paths (≤2.5pt thick, ≥6pt long),
     the 4 edges of stroked rects and of filled cell-background boxes (≤60pt tall, ≤95% page
     width — the SEC row-shading pattern), plus per-segment extraction from captured PATH points
     (D-016's 32-point cap is the designed hook). Rules cluster by position (2pt) and merge
     collinearly across ≤20pt gaps (bridges zebra striping); H–V intersection (2pt across,
     10pt slack along verticals) builds connected components via union-find; components with ≥2 H
     and ≥2 V rules become lattices; fragments snap to cells by bbox-center containment. At most
     one text line just above the lattice (≤12pt, ≥2 columns hit) is absorbed as a header row —
     SEC tables set the header above the first shaded/ruled band. Confidence = 0.7 + 0.3×occupancy.
  2. **row-ruled** — leftover horizontal rules ≥50pt long, stacked ≤40pt apart with ≥0.8 mutual
     x-overlap, ≥3 per band, evenly stacked (max gap ≤ 2.5×min gap, floor 4pt); text rows are the
     lines inside [top−median-gap, bottom]; columns from strategy-3 inference. Rows above the first
     rule are rule-separated headers. Confidence = 0.55 + 0.35×alignment-support (+0.05 for ≥4 rules), cap 0.95.
  3. **aligned** — ≥3 consecutive lines of ≥2 cells each (≤28pt spacing); columns are merged
     x-extents of cell intervals; a column is valid when supported by ≥max(2, 60% of lines) and
     left- or right-edge aligned within 3pt for ≥80% of its cells (right-edge stands in for
     decimal-point alignment, which it equals for uniformly formatted numeric columns); needs ≥2
     valid columns and a ≥3-line streak hitting ≥2 of them. **Prose guard**: any single column
     spanning >75% of the band rejects the candidate (kills the canonical false positive, bulleted
     lists — caught live on 10-K pages 27/28/31). Confidence = 0.4 + 0.4×support + size bonus, cap 0.9.
- **Header heuristic** (documented order): absorbed above-lattice line ⇒ header; row-ruled rows
  above the first rule ⇒ header; else first row is header when its dominant (font name, size)
  differs from the body's dominant style. Uniform-style borderless tables get no header row.
- **Template integration**: `Table(as="…")` routes through Pass 2 exactly like Annotation/Figure
  (D-016), scoped to section partitions. Output: `TableOutput { type:"Table", name, page, bbox,
  n_rows, n_cols, header, rows, cells, strategy, confidence }` plus metadata/parent_* like sibling
  outputs (`header` = the detected header row's texts, `rows` = body rows; full per-cell objects in
  `cells`). TableOutputs are appended after all positional outputs: chunk `parent_index` links are
  array positions, so inline insertion would shift every later section's recorded indices — the
  deferral is what makes the old-objects-unchanged guarantee below possible (table outputs carry
  their own parent_name/parent_index captured at match time; earlier positions never move).
  **`Table(model=, targetSchema=)` is a documented, justified exception to D-006**: the existing
  10k.tmpl carries them, they are NOT implemented this slice, and template compile emits a single
  WARN line (tracing — quiet on default runs per D-017) and proceeds with structural extraction.
  Rationale: model= is output *enrichment*, not match/selection semantics — selection correctness
  is unaffected; Stage C wires LLM enrichment.
- **Persistence (migration 0003_tables.sql, SCHEMA_VERSION=3)**: `table_cells(table_element_id
  uuid FK→elements ON DELETE CASCADE, "row", col, row_span/col_span default 1, text, bbox box,
  is_header bool, PK(table_element_id, "row", col))` — `"row"` is quoted (reserved word).
  Table-level fields live in the element's metadata jsonb. Ingest bulk-UNNESTs cells; load
  (`load_document`, `elements_in_bbox`) attaches cells to kind=table rows via one extra query
  (skipped when no tables); hydration rebuilds `TableStructure` from metadata + cells (cell-derived
  fallbacks if metadata is absent). Round-trip contract extended and enforced by test: fresh vs
  hydrated template runs produce byte-identical TableOutputs (TableOutput carries no per-run ids).
- **Regression baseline (intentional change)**: making Table real grows the 10k.tmpl 10-K query
  output 414 534 → **466 678 bytes**, 175 → **181 objects**. Verified: all 175 previous objects
  unchanged AND still at their exact array positions; the 6 additions are TableOutputs only (the
  five "Performance by Business Segment" per-segment tables, pages 26/27/28/30/31, plus the
  page-25 segment summary). `query --doc` on the pre-slice v2 document stays byte-identical at
  414 534 (hydration re-runs nothing). Real-doc evidence at `--parse-version 3`
  (56e30967-eff1-4c0f-acdb-3fa13b30d4ef): kinds text 11 476 / path 14 977 / annotation 79 /
  **table 125** (114 ruled, 8 aligned, 3 row-ruled), 11 615 table_cells.
- **Known limits** (acceptable this slice): cells inside one glyph run separated by a single
  space + no x-jump stay joined; tables without any of (rules, cell boxes, ≥3 aligned multi-cell
  lines) — e.g. header + one data row, borderless — are below the 2×2/3-line floors; pages past
  the 512-path cap (D-016) lose late-page rule evidence; spans are not detected.
- **Test-suite hygiene**: pre-existing race fixed — parse.rs/setup.rs tests create→read→delete the
  shared tests/example.pdf in parallel; table detection shifted timings enough to surface it as
  rare test_get_pdf_text/test_get_refs failures. They now hold a `setup::fixture_guard()` mutex
  for the test body. (Surgical: only the six fixture-file tests serialize.)

**D-019 · 2026-06-11 · CLI: SIGPIPE restored to default disposition.**
Rust ignores SIGPIPE, so `delver query … | head` panicked with "failed printing to stdout: Broken
pipe" once the reader closed the pipe — every JSON-emitting subcommand was affected. `main()` now
restores `SIG_DFL` for SIGPIPE first thing (unix only): conventional silent termination, chosen
over EPIPE-tolerant write wrapping because every stdout/stderr writer (including panics-on-print
inside libraries) is covered with one line and no error-path audit. `libc` added as a direct
dependency (already Cargo.lock-pinned transitively — the firewalled index was never consulted).
Verified: `query --doc … --pretty | head -5` exits silently, 0 bytes on stderr, no panic.

**D-020 · 2026-06-11 · Stage B slice 4: `TextChunk(method="semantic")` executes (spec STRATEGY ← Token | Semantic).**
`method="tokens"` is the pre-slice behavior and stays the default when the attribute is absent
(token budget with a tokenizer, character budget without); `method="semantic"` is embedding-driven
valley splitting (`chunker::chunk_semantic`). Unknown method values are a template-compile error
listing the supported set (D-006); the same validation re-runs at processing time because Elements
can be built programmatically. `method` is honored (and validated) wherever text is chunked:
TextChunk/Paragraph elements and a Section's own direct content all route through the one chunking
function.
- **Segmentation (sentence-ish units, element-granular)**: elements accumulate into the current
  segment; the segment closes after any element whose text — trailing ASCII whitespace trimmed,
  trailing closing delimiters (`"`, `'`, `”`, `’`, `)`, `]`) stripped — ends with `.`, `!`, or `?`;
  the final segment closes at end of input. Sentence boundaries strictly *inside* one element's
  text are not split candidates: chunks stay contiguous element slices, so output assembly
  (page stats, chunk text joining, parent links) is shared verbatim with the other strategies.
  Abbreviation handling ("U.S.") is deliberately out of scope — deterministic approximation.
- **Embedding + valley rule (percentile-based)**: segment text = element texts joined with a
  single space (the chunk-text joiner); all segments embed in ONE batch call; vector-count
  mismatch and dimension mismatch are hard errors. For adjacent-segment cosine similarities, the
  breakpoint threshold is the P-th percentile (ascending sort, integer index `P*(len-1)/100`),
  P = `breakpointPercentile` attr, **default 25**; a boundary breaks iff its similarity is
  *strictly* below the threshold — all-equal similarities (homogeneous text) produce no breaks,
  and P=0 disables valley splitting. Percentile over absolute threshold is deliberate: attribute
  floats still parse via the legacy ×1000 fixed-point `Value::Number(i64)` encoding (only
  ComparisonValue was fixed in D-014), so an integer 0..=100 attr is the only unambiguous knob.
  Out-of-range/non-integer percentile, or `breakpointPercentile` without `method="semantic"`,
  are template-compile errors (no silent no-op).
- **Token-budget interplay**: `chunkSize` caps each chunk — token count when a tokenizer is
  configured (the Tokens strategy's batch-encode path, helper now shared), else character count
  (sum of element text lengths, joiner spaces uncounted — the Characters strategy's accounting).
  Enforced at segment granularity: a segment that would overflow a non-empty chunk closes it
  first; a single segment larger than the whole budget forms its own over-budget chunk (the
  Tokens strategy's single-element overflow rule, one level up). `chunkOverlap` carries trailing
  whole segments into the next chunk (step back while carried cost < overlap; the crossing
  segment is included — the Tokens element rule at segment level), never past the closed chunk's
  start; each valley closes at most one chunk (a watermark stops the overlap tail from
  immediately re-splitting at the consumed boundary). Deterministic given a deterministic
  embedder.
- **Fail-loud shape (D-006)**: `method="semantic"` with no embedder configured errors at
  processing time naming the element (`TextChunk 'name'` with the `as=` name when present) and
  citing the remedies — `pass --embed-endpoint <name-or-url> or set DELVER_EMBED_ENDPOINT` —
  mirroring the D-014 EmbeddingSim message. To propagate, `process_matched_content` (and its
  recursion) now returns `Result`; the 10 direct test callers unwrap explicitly.
- **Output shape**: identical ChunkOutput objects plus, on the semantic path only, metadata
  `method: "semantic"` and `segment_count` (segments in the chunk, overlap-carried included).
  The default/tokens path is byte-identical to pre-slice output — both 10-K regression
  constants verified post-change (414 534 and 466 678 bytes, stderr 0, SIGPIPE clean).
- Tests: crates/delver-core/tests/semantic_chunking.rs (7: valley split at a designed topic
  break via MockEmbedder canned vectors, chunkSize cap, overlap carry incl. the no-re-split
  watermark, no-embedder error naming the element, unknown-method compile error listing values,
  percentile-without-semantic compile error, default ≡ explicit-tokens with no metadata
  additions). PDF builder copied from match_exec.rs per the D-014 precedent.
