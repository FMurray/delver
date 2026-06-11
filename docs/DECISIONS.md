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
