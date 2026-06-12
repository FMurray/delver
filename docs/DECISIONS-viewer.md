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

**DV-013 · 2026-06-12 · "LSP Disconnected" root cause: deep-link autorun streamed multi-MB
output into SSR; >64 KiB text nodes split by the browser parser kill hydration. Autorun is now
post-hydration.**
The user-visible symptom (red dot, "LSP Disconnected", dead editor) was NOT a websocket bug —
the ws server fn route (`/api/lsp_websocket<hash>`, registered by `leptos_routes` via
`server_fn_paths()`, upgrade compiled in through server_fn's `axum` feature) connects fine
(101) from any fresh session. The real bug was confined to `?template=…&run=1` deep links
(DV-006): seeding `run_request` at component setup made the results Suspense execute the
template DURING SSR and stream its whole pretty-JSON output (2.17 MB for the starter template
over the 158-page demo doc) as ONE text node inside the results `<pre>`. Browsers' HTML
parsers split text nodes at 65 536 chars (measured: the live `<pre>` held 34 sibling Text
nodes), so tachys hydration — which walks exactly one Text node per dynamic string and then
expects the trailing `<!>` marker — found `[Text, Text, …]`, reported "expected a marker node,
but found this instead: [node Text]" (query_panel.rs:700) and panicked unrecoverably
(tachys hydration.rs:186). The wasm app died mid-hydration, freezing the panel at its
server-rendered state — `connected` is `false` during SSR, which renders precisely
"LSP Disconnected". Why earlier probes were inconclusive: plain `/viewer/{id}` sessions never
render the panel at all (it mounts inside `<Show when=show_query>`, default closed — the
lead's dump-dom containing neither indicator string is by design), and the toggle flow
connects fine, so only deep-link entries (bookmark/refresh/new tab) crashed.
Fix (`query_panel.rs`): `run_request` now starts `None` on both server and client; the
deep-link autorun moved into an `Effect` (client-only, post-hydration). The output subtree is
therefore only ever rendered client-side and never hydrated — immune to parser text-splitting
at ANY output size — and SSR no longer blocks seconds on template execution (the deep-link
page shrank ~5.2 MB → ~360 KB). Behavior preserved: `?template` still pre-fills, `run=1`
still auto-runs (one server-fn round trip after hydrate); curl-visible execution lives in
REST `POST /api/v/docs/{id}/template`. Rule of thumb recorded: never SSR unbounded text into
a hydrated subtree — anything that can exceed ~64 KiB must render post-hydration (or be
chunked below the parser's split boundary).

**DV-014 · 2026-06-12 · Discover mode moves to a persistent right sidebar.**
Owner request: the element inspector is now the third column of the doc view — left sidebar
(palette + upload, unchanged) | page canvas | right `<aside>` housing the discover-mode
inspector (kind badge, id/bbox/font, insert-into-query actions, table structure with the
D-018 cell grid, text, metadata). `InspectorPanel` (pdf_viewer.rs) renders persistently where
the old inline `ElementPanel` appeared, with an empty-state hint ("Click any element on the
page to inspect it") instead of mounting on selection; `ElementPanel` is now just its
populated contents (header pinned, body scrolls). Collapsible exactly like the left aside:
a new `InspectorContext` (app.rs, default open) + a third nav `Toggle` ("Toggle element
inspector"), aside classes mirror `SidePanel` (`w-96` instead of `w-80` for the cell grid,
`border-l` for `border-r`). Clicking an overlay also re-opens a collapsed inspector
(`page_view` takes the context's write half) so click-select never looks dead. DV-009
discipline: `selected` starts `None` on server and client and the inspector/Show initial
states are constants, so the hydration subtree is identical; no resources moved. All existing
behavior kept: overlay toggles, click-select, insert-bus publishing, cell grid (verified in a
fresh headless session: empty state → p26 segment table click → "10 rows × 7 cols • ruled •
confidence 0.88" + grid + both insert chips; chip click auto-opens the query panel and lands
`Table(as="table_p26")` at the cursor).

**DV-015 · 2026-06-12 · "Documents in Store" is a partition file tree.**
The flat newest-first list is replaced by `components/doc_tree.rs`: corpus (collapsible) →
one collapsible level per hive-style partition `key=value` (D-023,
`documents.metadata.partitions`) → compact document leaf (name, pages • parse version •
date, source-bytes dot, `View` as a plain `<a href>` per DV-009). Documents without
partitions sit directly under their corpus. Built from the ONE existing listing call —
`DocumentSummary` gains a `partitions: BTreeMap<String, String>` (additive; REST `/api/v/docs`
carries it automatically); no per-node requests. Ordering decision: the stored jsonb object
is UNORDERED, so the original hive-path key order (`company=3M/year=2015`) is lost at ingest —
levels use deterministic ALPHABETICAL key order (the BTreeMap makes this structural), and
corpora + sibling segment values sort alphabetically too. Future fix noted: persist partition
key order at ingest (e.g. a `partition_keys` array next to the object) and order levels by it.
Default state: all corpora collapsed except the one containing the open document, whose full
partition path auto-expands and whose leaf is highlighted — with the test-residue corpora in
the shared dev DB (DV-010) the tree collapses ~250 documents into ~230 one-line rows.
Initial expansion derives only from (listing, route pathname), identical on server and
client, so the structural `<Show>`s hydrate cleanly (DV-009). Tree building is pure and
unit-tested (alphabetical paths, unpartitioned docs at corpus level, descendant counts,
listing order inside nodes).

**DV-016 · 2026-06-12 · Slice V4: the palette is a no-code structural query builder; the editor becomes "view source".**
The owner's design verbatim: the palette must "match the DOM structure of the actual query" — and
"the attributes [must] be selectable in the palette — no one knows what DocQL actually is". The
palette now renders the parsed buffer as a node tree (add-slot / node / add-slot …, child slots
inside structural nodes, ONE root slot when the buffer is empty) and every node expands into an
attribute form whose values are pickable: headings from the document, rule types from the grammar,
TYPE fields from detected table columns. LSP autocomplete polish was explicitly deprioritized
below this; nothing there changed.
- **Parse "service" decision: neither a server fn nor an LSP custom request — a pure shared
  function.** The slice offered two transports; both were beaten by the observation that
  delver-core is a NON-OPTIONAL viewer dependency and therefore already compiles into the wasm
  bundle. `query_tree::parse_query_tree(text)` walks the pest pairs (spans!) into
  `QueryTree { nodes, compile }` — nodes carry kind (Element / MatchDef / TypeDef), byte spans
  (whole node, header, body interior, declaration name), attributes with value + value-span
  (raw source preserved), children, match rules (function, pattern, threshold, endpoint,
  Heuristic comparisons, FirstMatch nesting) and TYPE fields — then runs the real
  `parse_template` for the D-006 compile surface. It executes synchronously IN the client
  (sub-ms at palette scale), so there is no round trip, no JSON-RPC id correlation, no debounce
  and no staleness window; the same code unit-tests under `cargo test -p viewer --lib`. pest
  span quirk recorded: spans absorb whitespace consumed by a trailing failed optional rule
  (an element's missing body), so node spans are trimmed back to the last non-whitespace byte.
- **Sync semantics: span surgery against the last-good snapshot, gated byte-for-byte.**
  `components::builder::QueryBuilder` (app context) holds ONE buffer signal (editor and forms
  both write it; `?template` deep links seed it in `QueryParamSync` so the tree sees them with
  the panel closed), the last good `Snapshot { text, tree }`, the syntax error, and the selected
  node path. Form edits compute byte splices from node spans and apply them ONLY when
  `snapshot.text == buffer` (the `fresh()` guard) — so hand formatting outside the edited span
  survives verbatim, and a syntactically broken buffer shows the last-good tree with an amber
  "fix syntax in the editor" banner and forms disabled (opacity + pointer-events) until the
  reparse succeeds. Manual edits round-trip: every buffer change reparses (client effect in
  `BuilderSync`) → tree rebuild. Editor mirror: programmatic buffer changes setValue with
  cursor+scroll preserved; editor keystrokes are equal-value no-ops, so the loop converges.
  Tree click selects the node's span in CodeMirror (byte → UTF-16 conversion for
  `posFromIndex`); cursor→tree was the documented nice-to-have and is deferred.
- **Compile diagnostics per node, by name heuristic.** `parse_template` stops at its first
  error and carries no positions, so the diagnostic is attributed to the first DFS node whose
  declaration name, `as=` value, or element identifier appears single-quoted in the message
  (red dot on the row, message on expand); unattributed messages render under the tree. Good
  enough in practice because the engine's fail-loud messages consistently name their owner.
- **Slot legality table** (`query_tree::slot_menu`): top level → Section, TextChunk, Table,
  Paragraph, Annotation, Figure, Image + Match, TYPE, SubCorpus; inside Section → the seven
  element kinds; everything else is a leaf. Inserted nodes are minimal-but-valid
  (`Section(as="section1") {}`, `Table(as="table1")`, `Match<Section> Match1 { Text("",
  threshold=0.6) }`, …), names uniquified DV-012-style; the new node is auto-selected with its
  form open.
- **Forms.** Section: heading picker (DV-012 palette fetch feeds it) + free text + rule-type
  selector — Text (threshold stepper 0.6) / Regex (pattern) / Heuristic (property dropdown over
  the D-014 supported set + comparator + value rows, string properties forced to ==/!=) /
  EmbeddingSim (text + threshold + endpoint); optional end_match with the same editor; `as`
  input. EVERY Section rule lives in a named `Match<Section>` block (created on first edit,
  auto-named from the pattern slug `Overview`/`overview`, inserted right before the section's
  top-level ancestor; the selection path shifts with it). TextChunk: chunkSize/chunkOverlap
  steppers (500/150), method dropdown (tokens drops the attr — engine default — and removes
  breakpointPercentile; semantic reveals the percentile stepper). Table: `as` + `type` dropdown
  over declared TYPEs + "New TYPE from a detected table…" → pick one of the doc's tables →
  field rows prefilled via `snippets::type_fields_from_columns` (extracted from the DV-012
  typed-table generator so form and snippet can never drift) → editable names/types → emits the
  TYPE at the TOP of the buffer and sets `type=`. TYPE nodes: row editor (add/remove/rename
  fields, type dropdown) and rename-with-reference-rewrite (`type=` values); Match nodes: the
  rule editor + rename-with-reference-rewrite (`match=`/`end_match=` identifiers).
  Annotation/Figure/Image/Paragraph: `as`. SubCorpus: description + as. Unknown elements render
  with no form ("edit in the editor"). All text inputs commit on change (blur/Enter), not per
  keystroke — the tree re-renders on every buffer change and a focused input would not survive.
- **Fidelity caveats (documented behavior, not bugs):** editing converts inline
  `match="string"` to a named block; only the FIRST clause of a multi-clause definition is
  form-edited (the rest are preserved verbatim and noted); FirstMatch/exotic clauses and
  array-valued attributes render read-only with their source; a heading pick overwrites `as=`
  with the slug (stable when re-picking the same heading); rewritten spans are re-rendered in
  canonical form (number formatting, attribute spacing) while existing attributes keep source
  order and new ones append; TYPE field-row edits rewrite the whole declaration (nothing else
  in a TYPE to preserve — the grammar has no comments).
- **Insert chips re-routed.** The DV-012 insert bus is consumed by the builder now (`BuilderSync`),
  not at the editor cursor: single-element specs (table/annotation/figure/plain-chunks) land in
  the selected node's child slot when slot-legal, everything else (and everything while the
  buffer is broken) appends at top level. The pristine-starter special case died with the
  starter: the DEFAULT BUFFER IS NOW EMPTY — the root slot is the empty state the owner asked
  for, and an empty template is valid DocQL (the DV-012 starter existed to avoid a syntax-error
  greeting, which no longer applies). The buffer also survives panel close/reopen now (it
  out-lives the panel-local signal it used to be).
- **Deliberately deferred:** Image element children (ImageSummary/Bytes/Caption/Embedding) —
  Image stays a leaf in the builder; FirstMatch composition UI; editing clauses past the first;
  cursor-position→tree-selection sync; a REST mirror for the parse (it is client-side; REST
  template execution from DV-006 still covers curl); doc-aware LSP completions (DV-012 stance
  unchanged).
- **Verified** (fresh headless sessions, evidence in /tmp/v4-evidence): empty buffer → root
  slot with the 10-kind menu; Section form with the real 10-K heading picker; OVERVIEW pick →
  `Match<Section> Overview { Text("OVERVIEW", threshold=0.6) }` + `Section(as="overview",
  match=Overview)`; child slot → TextChunk; editor buffer textually identical to the tree; run
  → chunk outputs (post-hydration path, LSP Connected, zero hydration/panic console errors);
  New TYPE… on the p26 10×7 table prefilled `col1 TEXT, c2015/c2014/c2013 DECIMAL` and emitted
  the declaration at top; deep-link `?template&run=1` autorun + tree intact; inspector chip
  landed inside the selected Section. CLI baselines 414534/466678 bytes, 0-byte stderr.

**DV-017 · 2026-06-12 · Slice V5: the builder gets a Run button; results stop hiding behind the
closed panel.**
Owner's report verbatim: "I am not seeing the query results anywhere but the authoring
experience is much better." Root cause confirmed: the V4 builder had no run affordance, and
results render only inside the bottom `QueryPanel`, which mounts behind `QueryContext`'s
`show_query` (default false) — a user authoring purely in the palette had no path to results.
- **Run button** at the top of the "Query builder" section (`palette::run_bar`): enabled
  exactly when the buffer is runnable — non-empty, parses, compiles — via
  `builder::run_gate(buffer, syntax_error, snapshot)` (pure, unit-tested; the reactive
  `QueryBuilder::run_gate` wraps it). When disabled, the reason renders as a hint below the
  button (and as its tooltip): empty buffer / positioned syntax error / `parse_template`
  compile message / "Reading query structure..." for the pre-first-parse instant (that
  fallback is what SSR renders, so the disabled button hydrates cleanly per DV-009).
- **Run plumbing — `RunBus` context (app.rs), two halves.** `request: RwSignal<Option<u64>>`
  is a consumed one-shot exactly like the DV-012 insert bus: the button sets `show_query`
  true and publishes a nonce; the panel's client-side effect calls the SAME `execute()` as
  Ctrl/Cmd-Enter (doc id from the route + shared buffer) and clears the request — so a
  request published while the panel is closed is consumed by the mount-time effect run, and
  panel re-mounts (toggle close/open) never replay a stale one. Effects never run during SSR,
  so DV-013's never-execute-server-side rule holds. `status: RwSignal<RunStatus>` flows back:
  Idle | Running | Done(n) | Failed(msg) — sharing the multi-MB output itself would be
  pointlessly heavy; the count is all the builder needs.
- **Status badge** next to Run: "running…" → "{n} outputs" (green, tooltip points at the
  results panel) or "run failed" (red, tooltip carries the template/transport error). The
  panel reduces its run resource into the enum; results now ride with their request nonce
  (`Resource` value `(nonce, result)`) so a stale value — the previous run's, still readable
  while the next is in flight — can never overwrite "running…"; `count_outputs` measures the
  top-level array length via `Vec<serde::de::IgnoredAny>` without materializing values.
  Closing the panel mid-run drops the resource (aborting the run), so `on_cleanup` resets a
  dangling Running to Idle; a finished Done/Failed badge survives the panel closing (it
  describes the last run).
- **Unchanged behaviors verified:** Ctrl/Cmd-Enter in the editor, the panel's Execute Query
  button (badge cycles running… → count there too), insert chips opening the panel, and
  `?template&run=1` deep-link autorun (now also feeds the badge).
- **Verified** (fresh headless sessions, evidence in /tmp/v5-evidence): empty buffer → Run
  disabled with "query is empty" hint, panel closed; Section(OVERVIEW)+TextChunk built purely
  in the palette → Run enabled → click → panel auto-opens, badge "running…" → "214 outputs",
  results `<pre>` parses to exactly 214 entries; `Table(as=` → disabled + "Fix the syntax
  error first (line 1:10: expected value)"; `type="Nope"` → disabled + the undefined-TYPE
  compile message; deep-link autorun: panel open, 3036 outputs, badge agrees, LSP Connected;
  zero hydration/panic console errors in all sessions. cargo check ssr + hydrate/wasm clean;
  viewer lib tests 57 (default) / 71 (ssr), workspace suite green; CLI baselines
  414534/466678 bytes, 0-byte stderr.

**DV-018 · 2026-06-12 · Slice V6: results work like Ctrl+F — highlights + match pagination +
section page-filters, driven by the D-025 provenance sidecar.**
Owner's design verbatim: "the results display should work like a ctrl + f search, where the
matching elements are highlighted and the prev,next buttons can paginate through matches. For
sections it should filter the whole document to the matching pages."
- **Data**: the run server fn now uses `store::execute_template_full` →
  `process_parsed_with_provenance` (D-025); `TemplateRun` gains optional `provenance` +
  `diagnostics` (serde-default, so old payloads still parse). The REST
  `POST /api/v/docs/{id}/template` keeps its exact pre-V6 outputs-only payload
  (`execute_template` delegates and drops the extras). The JSON results panel and the DV-017
  count badge are unchanged — results mode is a NAVIGABLE view over the same run, not a
  replacement.
- **Model** (`results.rs`, pure + unit-tested): `build_results` turns the sidecar into
  `RunResults { doc_id, run_id, matches, sections, misses }` — matches sorted by
  (first page, document order, output index), which re-interleaves the D-018 tail-deferred
  table outputs into reading order; sections deduplicated by value and ordered by span. An
  app-level `ResultsBus` (results `Arc`, current match position, section filter) is published
  by the query panel once per run nonce (builder Run, Ctrl/Cmd-Enter, Execute button, and
  `?template&run=1` deep links all land here); failed runs CLEAR it — stale highlights must
  not lie about the document.
- **Results bar** (between header and canvas): "x of N matches", section chips
  ("all" + `name · pA–B` per distinct attribution), near-miss warnings, ✕ exit. Near misses
  (D-024) render one amber row per miss — `match 'X' matched nothing at threshold 0.6 —
  closest: '…' (0.55, p45); …` — with every candidate page reference a plain `<a>` to that
  page: the silent-`[]` failure is now guided iteration (run → see closest → fix pattern).
- **Highlights**: a second per-element overlay layer, ALWAYS in the tree with visibility via
  the style attribute (the DV-009 overlay discipline) — yellow fill for every visible match
  touching the page, orange emphasis for the current match. Per-page id sets come from one
  memo over the sidecar (multi-page matches contribute all their ids; the page's own element
  join filters naturally). Results are client-side post-run state (DV-013), so SSR and
  hydration both render every highlight `display:none` — structurally identical. Highlights
  are click-to-inspect like discover overlays (DV-014) and independent of the kind toggles.
- **Nav semantics (the UX calls, made here):** in results mode the EXISTING header Prev/Next
  become "← Prev match"/"Next match →", stepping through visible matches with wrap-around
  (Ctrl+F convention); compact "‹ pg"/"pg ›" steppers appear beside them so plain page nav
  stays one click away (exit ✕ restores the pair fully). Keys n/p step too, by clicking the
  real anchors (never while typing — input/textarea/CodeMirror targets are ignored). A run
  never yanks navigation away: the cursor seeds to the first match ON the open page (else the
  first match overall) and the user drives from there. Section chips filter BY PAGE SPAN —
  the owner's "filter the whole document to the matching pages" — so any match on an
  in-span page stays navigable regardless of attribution; the header indicator becomes
  "page 17 of 16–21" while filtered; chips re-seed the cursor to the open page's first
  visible match.
- **Anchor mechanics (DV-009 extension):** Prev/Next stay plain `<a href>` links, but the
  href tracks the CURRENT match's page while on:click advances the cursor synchronously —
  tachys flushes the reactive attribute before the router's document-level listener reads the
  anchor, so navigation lands on the NEW current match's page with no dependence on
  microtask ordering between the two listeners (a deferred-update variant measured one tick
  stale at page boundaries in headless probing; this shape is timing-independent by
  construction: worst case is a same-URL no-op, never a skip).
- **Verified** (fresh headless sessions, evidence in /tmp/v6-evidence; zero console errors /
  hydration panics): Section(OVERVIEW→RESULTS, as="overview")+TextChunk deep-link autorun on
  the 3M 10-K → bar "1 of 112 matches", chip "overview · p16–21", 52 visible highlights with
  the OVERVIEW heading orange; Next twice from the landing page's last match → URL p16→p17
  with highlights on the new page ("8 of" → "9 of" → "10 of"); chip click → selected state +
  "page 17 of 16–21" indicator; 'n' key steps ("9 of" → "10 of"); a never-matching fragment
  query → "0 matches" + the near-miss warning whose "p45" link jumps to that page. cargo
  check ssr + hydrate/wasm clean; viewer lib tests 66 (default) / 80 (ssr); workspace 269
  passed / 0 failed / 1 ignored (pre-existing gated live test); CLI baselines byte-exact
  414534 / 466678 with 0-byte stderr.
