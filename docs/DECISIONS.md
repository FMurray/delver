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
