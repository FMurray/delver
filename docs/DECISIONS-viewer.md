# Viewer-on-Postgres (Slice V1) — Decision Log

Append-only, same conventions as `docs/DECISIONS.md` (D-001) but scoped to the
viewer worktree (`feat/viewer-postgres`). Cross-references like D-008 point at
the main log.

---

**DV-001 · 2026-06-11 · Store swap: viewer reads/writes Postgres via delver-store; SQLite retired.**
`crates/viewer/src/store.rs` is rewritten as a thin service layer over
`delver_store::DelverStore` (shared `tokio::sync::OnceCell` handle;
`DelverStore::connect` runs the embedded migrations on first use). Client-safe
DTOs (`DocumentSummary`, `UploadReceipt`, `PageMeta`, `ElementOverlay`,
`TemplateRun`) live at the top of the module; everything touching the DB,
filesystem, or pdfium is `#[cfg(feature = "ssr")]`. The sqlx dependency is now
Postgres-only (feature set mirrors delver-store's so the build stays inside
Cargo.lock pins); the old SQLite page-image tables, pool setup, and
`DocumentPage`/`PdfDocument` CRUD are deleted. Document listing joins
`documents` with `corpora` in the viewer layer — no delver-store code changes
(hard boundary for this slice).

**DV-002 · 2026-06-11 · Byte-cache + uri convention; display names.**
Original PDF bytes never go in Postgres. On upload the viewer writes them to
`DELVER_DOC_CACHE` (default `~/.delver/doc-cache`) as `<sha256-hex>.pdf` and
passes that absolute path as the document `uri` to `ingest_document`, so the
ingest stays idempotent end to end (content-hash keyed file + D-008 dedup on
(corpus, sha256, parse_version)). Rasters are produced from the uri file; a
document with a NULL/unreadable uri renders a clear "original bytes not
available" placeholder (overlays still draw at page scale) instead of erroring.
Display name precedence: PDF Info title > uri basename > short id.

**DV-003 · 2026-06-11 · Raster cache: in-process LRU, no page images in Postgres.**
Pages are rasterized on demand with pdfium at 150 DPI and encoded as WebP, then
held in a small in-process LRU (`(doc_uuid, page_index)` → raster, cap 32,
"cannot render" verdicts cached too). No disk cache this slice — the byte-cache
plus a warm LRU is fast enough for a dev viewer, and a disk cache would need an
invalidation story; revisit if multi-document browsing thrashes. HTTP responses
add `Cache-Control: private, max-age=300` so the browser absorbs repeat views.

**DV-004 · 2026-06-11 · REST surface + overlays from store reads only.**
A plain-JSON REST layer (`/api/v/docs`, `/api/v/docs/{id}`,
`…/pages/{page}/elements`, `…/pages/{page}/image.webp`, `…/pages/{page}/meta`,
`…/template`, `/api/v/upload`) sits next to the Leptos server fns over the same
service layer, so the store can be driven with curl and page images load as
ordinary `<img>` URLs. Per-page element listing reuses the store's GiST bbox
query with an effectively-infinite rectangle (`elements_in_bbox(doc, page,
±1e9)`) — page filtering in the viewer layer, zero delver-store changes.
Overlays are color-coded per kind with toggles (text/annotation/figure/path/
image; table greyed out "coming in B3"); clicking a bbox opens a side panel
with the element's id, bbox, font, text, and metadata JSON (payload bytes are
never shipped to the client).

**DV-005 · 2026-06-11 · pdfium / npm runtime notes.**
Server-side rendering binds pdfium at runtime: `PDFIUM_LIBRARY_PATH` env >
build-time path > `./` > system library. `scripts/dev-viewer.sh` pre-seeds
`target/debug/libpdfium.dylib` from the `deps/pdfium-mac-arm64` checkout so
`build.rs` early-returns and nothing is downloaded. The crates.io *index* is
firewalled but `static.crates.io` is reachable — builds work offline-ish as
long as they stay inside Cargo.lock pins (`cargo check --offline` is the
verify gate). No npm tooling was needed this slice; if it becomes necessary,
symlink `~/delver/crates/viewer/node` from the main worktree (gitignored).

**DV-006 · 2026-06-11 · Template execution panel: hydrate + process_parsed, fail-loud.**
The CodeMirror DocQL editor gets an execution panel that runs the template
against the currently open document via the same path as the CLI `query
--doc`: `load_document` → `hydrate_pages` → `delver_core::process_parsed`
(D-012). Embedder is the `DELVER_EMBED_ENDPOINT` env passthrough (D-015); when
unset, `EmbeddingSim` templates fail loud (D-006) and the error (full anyhow
chain) is rendered as a readable red banner — template failures travel as a
structured `TemplateRun { ok, output, error }`, not transport errors. Results
render as pretty JSON. Deep links: `?template=…&run=1` pre-fills and auto-runs.
No tokenizer this slice (character-based chunking).

**DV-007 · 2026-06-11 · Lesson: parallel worktrees need separate databases.**
This slice initially pointed at the shared `delver` database; a parallel
worktree applied its own migration 0003 there, and this checkout's embedded
migrator (0001+0002) then correctly REFUSED the database — sqlx records
applied migrations in `_sqlx_migrations`, which is shared mutable state across
every branch using the DB. The viewer now uses a dedicated database
(`postgres://delver:delver@localhost:5433/delver_viewer`, default in
`crates/viewer/.env`, `scripts/dev-viewer.sh`, and `store::DEFAULT_DB_URL`);
the shared `delver` DB is off-limits until the branches converge at merge
time. Rule going forward: one database per worktree/branch that owns
migrations; converge schemas at merge, not through a shared dev DB.

**DV-008 · 2026-06-11 · MLflow: nothing to neutralize at this base.**
The slice plan deprioritized MLflow ("neutralize minimally if it blocks
compilation"). At base 2ebb9d6 the viewer contains no MLflow code — the MLflow
integration lives on `feat/viewer-mlflow-integration`, which is not part of
this branch — so the workspace compiles with zero MLflow work and nothing new
was built for it.

**DV-009 · 2026-06-11 · SSR streaming fixes: no programmatic navigate in the rendered tree; resources read at the boundary; overlay visibility via style, not structure.**
Two inherited bugs surfaced once the pages were driven over HTTP for real.
(1) The Prev/Next pager called `use_navigate`'s closure from view!-macro click
handlers; that closure provably executed during SSR suspense resolve
(backtrace: `RouterContext::navigate` → `BrowserUrl::parse` →
`js_sys::global()` → "cannot access imported statics on non-wasm targets"),
panicking the render task mid-stream and truncating every `/viewer/…`
response (it also poisoned a Lazy, breaking all later renders in the
process). The pager is now plain `<a href>` links — leptos_router intercepts
same-origin clicks, so client-side navigation is preserved with zero JS in
the SSR path.
(2) Page meta + element resources were read only inside a *nested* reactive
closure under the `<Suspense>`; nested reads are not part of the boundary's
await set, so out-of-order streaming serialized the subtree before they
resolved — the raster appeared only when the LRU happened to be warm and the
overlays were always empty; the per-overlay structural `<Show>` additionally
diverged server/client DOM (hydration mismatch: client toggles parsed from
the URL, server subtree rendered without). Fix: read every resource at the
top of the Suspense-tracked closure (the boundary now awaits document, meta,
and elements), render `page_view` from plain data, and keep every overlay div
permanently in the tree with the kind toggle driving a reactive
`display:none` style. Rule of thumb recorded: under streaming SSR, read all
resources you render from directly in the suspense closure, and prefer
attribute-level reactivity over structural conditionals for server-rendered
collections.

**DV-010 · 2026-06-11 · DB reunification: viewer back on the shared `delver` database; `delver_viewer` is legacy/disposable.**
The merge (9062694) converged the branches: this checkout's embedded migrator
(delver-store 0001+0002+0003) now matches the shared database's applied
migrations exactly, so the DV-007 one-database-per-worktree rule no longer
applies *to this checkout* — the defaults in `store::DEFAULT_DB_URL`,
`crates/viewer/.env`, and `scripts/dev-viewer.sh` are flipped to
`postgres://delver:delver@localhost:5433/delver`. The stopgap `delver_viewer`
database is legacy/disposable: nothing reads it anymore; its only real content
(a viewer-dev ingest of the 3M 10-K) was re-ingested into the shared DB (the
byte-cache file is content-hash keyed, so it was already shared). DV-007's
rule still stands for any future worktree that diverges on migrations. Note:
the shared DB accumulates synthetic test corpora (`idempotent-…`,
`roundtrip-…`, two-page fixtures) — the store integration tests create
uniquely named corpora and do not delete them, so the viewer's newest-first
document list shows real documents interleaved with that residue.

**DV-011 · 2026-06-11 · Table overlays: cells ride the element DTO; grid drawn inside the table overlay; demo document.**
The table toggle is live (the greyed-out "coming in B3" placeholder is gone).
`ElementOverlay` gains `cells: Option<Vec<CellOverlay>>` (row, col, spans,
bbox, text, is_header — the D-018 cell shape), mapped 1:1 from
`ElementRow.table_cells`, which delver-store already attaches on both
`load_document` and `elements_in_bbox` — per-page REST and server-fn element
payloads carry cells with zero new store queries (`TableCellRow` is now
re-exported from delver-store's lib.rs, an additive fix: the public
`ElementRow.table_cells` field's type was un-nameable downstream).
Rendering: table bboxes draw in their own color (red family, distinct from
the five Stage-B kinds); the cell grid renders as absolutely positioned
children *inside* the table overlay div (each cell's bbox offset by the table
origin), 1px inner borders, fill tint where `is_header`, and
`pointer-events:none` so clicks land on the table element — nesting means the
kind toggle's attribute-level `display:none` (DV-009) hides the grid for
free, and SSR/hydration stay structurally identical. Clicking a table opens
the side panel with "n rows × m cols • strategy • confidence" (exactly the
D-018 metadata keys) plus a dense per-(row, col) text grid sized from
metadata with cell-extent fallback (header cells tinted).
Demo document: `~/datasets/3M_2015_10K.pdf` ingested through the running
server's upload path into corpus `viewer-dev` on the shared DB → document
`c5bd3aa0-d6e3-49ca-aae0-82eb3a05f3c3` (created, 158 pages, 26 657 elements,
125 tables / 11 615 cells, byte-cache uri set so rasters render). Store page
26 (viewer page index 25) carries the 10×7 ruled segment-performance table
(confidence 0.88) used for the overlay evidence.

**DV-012 · 2026-06-12 · Slice V2: discover → query. Insert-into-query snippets, doc-aware palette, LSP refresh + completions.**
The core loop (click an element → see what it is → add it to a query → run) is wired end to end.
- **Snippet generation** (`crates/viewer/src/snippets.rs`, pure + unit-tested): the element side
  panel gains kind-appropriate "Insert into query" actions, the palette reuses the same
  generators. Text → `Match<Section> SectionN { Text("…", threshold=0.6) }` ("match only") or
  that plus `Section(match=SectionN, as="sectionN") { TextChunk(chunkSize=500, chunkOverlap=150) }`
  ("section scaffold") — N is the smallest integer where both names are free in the buffer. Table →
  `Table(as="table_p<page>")` and a typed scaffold `TYPE TableP<page> AS TABLE ( … );` +
  `Table(as=…, type=…)`; Annotation/Figure → `Annotation(as="annotation_p<page>")` /
  `Figure(as="figure_p<page>")`. Pages are 1-based store pages. Generation rules: pattern text is
  whitespace-collapsed and elided to ≤80 chars (no ellipsis — a truncated prefix still
  fuzzy-matches; an added ellipsis would not), `\` and `"` escaped (the only chars the grammar's
  string rule cannot take raw). Field names are lowercased slugs of header-cell texts
  (non-alphanumerics collapse to `_`, 30-char cap, digit-leading slugs get a `c` prefix — `c2015`
  ↔ "2015" sits exactly on D-021's 0.8 fuzzy-match boundary), fallback `col<index>` (1-based
  original column), deduplicated within the field list. Field type DECIMAL when a strict majority
  of the column's non-empty body cells coerce under the D-021 conventions (trailing `%`, parens
  negative, `$`/`,`/whitespace stripped), else TEXT — majority, not all, so the p26 em-dash nil
  doesn't flip its column to TEXT. `$`-filler columns (every body cell empty or only `$%()`/
  whitespace) are skipped entirely so every declared field can claim a real column at run time.
  All generated identifiers are made unique against the editor buffer (whole-word scan; `_2`,
  `_3`… suffixes; TYPE name + `as=` name share one suffix). Note: `Table(as=)` selects every
  table in scope (Pass 2 has no per-element match filtering, D-018) — the generated snippets
  follow the slice spec's shapes verbatim; scoping is the user's next iteration step (wrap in a
  Section), not invented core semantics.
- **Insert plumbing**: an `InsertBus` context carries `SnippetSpec`s (not pre-rendered text)
  from the side panel / palette to the query panel, which renders against the LIVE buffer at
  insertion time (uniqueness cannot be decided earlier) and inserts at the CodeMirror cursor
  (own line; replaces the buffer when it still holds the pristine starter). Any insert action
  opens the query panel; with the panel previously closed, the bus is consumed by the
  mount-time effect run, and the consumed value is cleared so panel re-mounts cannot re-insert.
- **Doc-aware palette** (`components/palette.rs`, collapsible section in the left side panel;
  REST mirror `GET /api/v/docs/{id}/palette`): (a) heading candidates — documented heuristic in
  `snippets::select_headings`: pool = text elements 3..=80 chars with a letter; modal (font_size,
  font_name) over non-empty text = body style; keep lines that are size-prominent (≥ body+1.5pt)
  OR bold-emphasis (bold while body isn't, ≥ body−0.5pt, ALL-CAPS — the SEC convention where
  headings share the body size: the 3M 10-K's section heads are Times-Bold at body 13pt); order
  by size desc then position, dedupe case-insensitively, cap 20. On the demo doc this surfaces
  the four 21pt title-page lines plus OVERVIEW / RESULTS OF OPERATIONS / PERFORMANCE BY BUSINESS
  SEGMENT (p24) / PERFORMANCE BY GEOGRAPHIC AREA…; (b) detected tables (page, n×m, strategy,
  confidence — the D-018 metadata keys) with server-reduced non-filler `ColumnSpec`s so the
  typed scaffold generates client-side without shipping 11.6k cells; (c) four starter templates
  (plain chunks / sections+chunks / tables-in-section / typed table), pre-filled with the first
  real heading and the best table (named headers preferred, then confidence). Palette queries
  are viewer-layer SQL over the store pool (the DV-001/DV-004 boundary precedent; zero
  delver-store changes).
- **LSP findings + refresh** (`language_server/docql_server.rs`): syntax validation already used
  the real pest grammar (so TYPE…AS etc. *parsed*), but compile-level checks never ran and the
  completion inventory was stale in THREE drifting copies (no Annotation/Figure/SubCorpus/TYPE,
  a `Cosine` item instead of canonical `EmbeddingSim`, `Table(match=…)` which the engine
  ignores); the websocket `didChange` path also never stored the document, so position-aware
  completion was impossible. Now: diagnostics run pest (positioned errors) then full
  `parse_template` (D-006 fail-loud compile surface: unknown TYPE/type= placement, unknown
  method=, undefined match refs, bad regexes, template= interpolation); one inventory table
  (elements Section/TextChunk/Paragraph/Table/Annotation/Figure/Image/SubCorpus + Match/TYPE
  keywords, per-element attribute keys incl. method/breakpointPercentile/template/type, match
  functions Text/Regex/Heuristic/EmbeddingSim/FirstMatch, TYPE field types TEXT/INT/DECIMAL)
  serves a context-aware `textDocument/completion` (string/paren/brace scanner: element attrs
  inside `Name(…`, functions inside match bodies and `FirstMatch(`, field types inside
  `TYPE … AS TABLE (`); the dead tower-lsp typed methods were deleted rather than refreshed.
  Doc-aware completions (real heading strings inside `Text("…")`) were deliberately NOT added —
  the palette covers discovery.
- **Editor wiring** (`components/query_panel.rs`): the `codemirror` crate wraps only
  value/change, so the raw CM5 instance is recovered (wrapper div's `CodeMirror` property) for
  cursor insertion, `extraKeys`, and the show-hint addon (now loaded in the shell head).
  Ctrl-Space sends `textDocument/completion` through the existing websocket (id-correlated
  thread-local pending-hint slot; `$N` snippet markers stripped for CM5) and renders via
  show-hint. Ctrl/Cmd-Enter now executes FROM INSIDE the editor — the original textarea's
  keydown never fired once CodeMirror owned input, so the documented one-keystroke run flow was
  actually dead under hydration. The default editor content is a runnable starter
  (`TextChunk(chunkSize=500, chunkOverlap=150)`) — the old `// comment` placeholder was itself a
  DocQL syntax error (the grammar has no comments) and greeted users with a red diagnostic.
  The LSP server's document state is seeded right after initialize so completions are
  position-aware before the first edit.
