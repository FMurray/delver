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
