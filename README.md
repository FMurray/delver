# Delver

**Delver** is a declarative document-extraction engine written in Rust, and **DocQL** is its query language.

Point it at a PDF and a template, and it will:

- **parse** the document into typed elements — text, tables, annotations, figures, vector paths, images, embedded files — with bounding boxes, fonts, and reading order;
- **match document structure declaratively** — find sections by fuzzy text, regex, layout heuristics, or embedding similarity, and scope everything nested inside them;
- **extract typed records from tables** — declare a `TYPE … AS TABLE` schema and get coerced, provenance-tracked rows out of detected tables (currency symbols, parenthesized negatives, and percent signs handled);
- **persist everything in a Postgres-backed semantic index** — content-hash-deduplicated documents, full-text search, bbox queries, pgvector embeddings;
- **query ad hoc across documents** — run one template over a whole corpus, filtered by partitions (`--where year=2016`).

DocQL is inspired by SQL and DOM parsing: instead of writing imperative parsing code, you declare the shape of what you want and Delver aligns the document to it.

```text
Section(match="PERFORMANCE BY BUSINESS SEGMENT", as="segments") {
  TextChunk(chunkSize=500, chunkOverlap=150)
  Table(as="segment_performance", type="SegmentPerformance")
}
```

**Status**: `0.2.0-rc.1` — a working release candidate under active development. The CLI, Python bindings, Postgres store, table extraction, and viewer described below all work today; features marked *experimental* are called out honestly. **License: TBD** (not yet chosen — do not redistribute until one is added).

---

## Table of contents

- [Quickstart (5 minutes)](#quickstart-5-minutes)
- [A taste of DocQL: typed tables](#a-taste-of-docql-typed-tables)
- [Python](#python)
- [Viewer](#viewer)
- [Repository map](#repository-map)
- [Status and roadmap](#status-and-roadmap)

---

## Quickstart (5 minutes)

### Prerequisites

- **Rust** (we build with 1.88) — `rustup` recommended.
- **Postgres 17 with pgvector**, listening on `localhost:5433`. Two supported paths:
  - **Docker**: `scripts/dev-db.sh` (wraps `docker compose -f docker-compose.dev.yml up -d --wait db`, image `pgvector/pgvector:pg17`).
  - **Homebrew** (if Docker isn't an option): see [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md#path-b-homebrew-postgres) — five commands, fully equivalent.

Everything defaults to `postgres://delver:delver@localhost:5433/delver`; override with `--db` or `DATABASE_URL`. Schema migrations run automatically on first connect.

### Build and fetch a test document

```bash
cargo build -p delver
mkdir -p ~/datasets && curl -sL -o ~/datasets/3M_2015_10K.pdf \
  https://raw.githubusercontent.com/patronus-ai/financebench/main/pdfs/3M_2015_10K.pdf
```

The fixture is a 158-page SEC 10-K filing from the public [FinanceBench](https://github.com/patronus-ai/financebench) repo (1.2 MB). Keep datasets **outside** the repo — see `scripts/fetch-testdata.sh` for the full corpus fetcher and its compliance notes.

### 1. Index — parse and persist

```bash
./target/debug/delver index ~/datasets/3M_2015_10K.pdf --corpus demo
```

```json
{"corpus":"demo","created":true,"document_id":"838c2f8a-12b9-48ba-8b1a-bd511689b6e3","element_count":26657,"partitions":{}}
```

Ingest is idempotent: re-indexing identical bytes returns the same document with `"created":false`.

### 2. Query — run a DocQL template

Use the `document_id` from *your* receipt (the ids below are from the run that produced these snippets):

```bash
./target/debug/delver query --doc 838c2f8a-12b9-48ba-8b1a-bd511689b6e3 \
  --template crates/delver/tests/10k.tmpl --pretty | head -8
```

```json
[
  {
    "type": "Text",
    "text": "Table of Contents   low   UNITED STATES SECURITIES AND EXCHANGE COMMISSION Washington, D.C. 20549   FORM 10-K …",
    "metadata": {
      "chunk_char_count": 3712,
      "chunk_element_count": 87,
      "page_numbers": [
```

That template chunks the whole filing, carves out the *Management's Discussion and Analysis* section, and collects the tables inside it — 181 outputs (175 text chunks + 6 tables) for this document. `--pdf <file>` runs the same template with a fresh parse, no database needed.

### 3. Search — full-text over the corpus

```bash
./target/debug/delver search "research and development" --corpus demo --limit 3
```

```json
[{"document_id":"838c2f8a-12b9-48ba-8b1a-bd511689b6e3","element_id":"fe227241-c7c2-4359-b67e-e8509324b18c","page":6,"rank":0.232880637049675,"snippet":"Research and development, covering basic scientific research and the application of scientific advances in the development of new and"}, ...]
```

All three subcommands print exactly one JSON document on stdout (diagnostics go to stderr), so they compose with `jq`, pipes, and `head`.

## A taste of DocQL: typed tables

Declare a record type, match a section, and coerce the tables inside it — `segments.tmpl`:

```text
TYPE SegmentPerformance AS TABLE (
  metric TEXT,
  y2015 DECIMAL,
  y2014 DECIMAL,
  y2013 DECIMAL,
);

Section(
  match="PERFORMANCE BY BUSINESS SEGMENT",
  end_match="PERFORMANCE BY GEOGRAPHIC AREA",
  as="segments"
) {
  Table(as="segment_performance", type="SegmentPerformance")
}
```

```bash
./target/debug/delver query --doc 838c2f8a-12b9-48ba-8b1a-bd511689b6e3 \
  --template segments.tmpl --pretty
```

Real output excerpt (the Industrial segment table on page 26 — headers `2015/2014/2013` fuzzy-matched onto `y2015/y2014/y2013`, `$`-filler columns skipped, parens coerced to negatives, percent signs stripped and recorded):

```json
{
  "type": "TypedTable",
  "type_name": "SegmentPerformance",
  "name": "segment_performance",
  "records": [
    {"metric": "Sales (millions)",       "y2015": 10328.0, "y2014": 10990.0, "y2013": 10657.0},
    {"metric": "Organic local currency", "y2015": 0.7,     "y2014": 4.9,     "y2013": 4.6},
    {"metric": "Translation",            "y2015": -7.3,    "y2014": -1.8,    "y2013": -1.7}
  ],
  "coerced_ok": 35,
  "coerced_err": 1,
  "errors": [{"row": 4, "col": 4, "raw": "—", "reason": "cannot parse \"—\" as DECIMAL"}],
  "provenance": {"element_id": "79108707-b2e0-4c4f-818b-9b2d6714e75a", "page": 26, "source_rows": [1, 2, 3, 4, 5, 6, 7, 8, 9]}
}
```

Bad cells never abort a run: they become `null` plus an `errors` entry, and every record keeps provenance back to the table element and grid rows it came from.

**[Full language reference → docs/DOCQL.md](docs/DOCQL.md)** — all element types, match rules, coercion semantics, partitions, and multi-document queries, with runnable examples.

## Python

Delver ships Python bindings (`delver_pdf`, PyO3). Build them into a virtualenv with [uv](https://docs.astral.sh/uv/) and [maturin](https://www.maturin.rs/):

```bash
uv tool install maturin            # if you don't have maturin yet
uv venv .venv && source .venv/bin/activate
maturin develop --uv -m crates/delver/Cargo.toml --features extension-module
```

(Plain alternative: `python3 -m venv .venv && source .venv/bin/activate && pip install maturin`, then the same `maturin develop` without `--uv`.)

```python
import json, os
import delver_pdf

# One-shot: parse a PDF and run a DocQL template (no database needed)
pdf = os.path.expanduser("~/datasets/3M_2015_10K.pdf")
outputs = json.loads(delver_pdf.process_pdf_file(pdf, "crates/delver/tests/10k.tmpl"))
print(len(outputs), "outputs")                      # -> 181 outputs

# Persistent store: ingest + search (uses $DATABASE_URL, else the local dev DB)
store = delver_pdf.DelverStore()
receipt = json.loads(store.ingest(pdf, "demo"))     # idempotent, same receipt as the CLI
hits = json.loads(store.search("research and development", "demo", limit=3))
print(hits[0]["page"], "-", hits[0]["snippet"][:60])

# Run a DocQL template against the stored document
tables = json.loads(store.run_template(receipt["document_id"], 'Table(as="tables")'))
print(len(tables), "tables")                        # -> 125 tables in this 10-K
```

CLI and Python route through one shared service layer, so the JSON shapes are identical.

## Viewer

A web viewer for inspecting parsed documents and iterating on templates:

```bash
./scripts/dev-viewer.sh        # builds and serves on http://127.0.0.1:3017
```

What you can do there:

- **Upload / index** PDFs into the store (drag-and-drop; ingest is the same idempotent path as the CLI).
- **Render pages** (pdfium rasters, WebP) with **element overlays**: text, annotations, figures, paths, images — and **tables with their full cell grid** (header cells tinted; click a table for the per-cell text grid, strategy, and confidence).
- **Edit and run DocQL** in a CodeMirror editor with language support (live parse diagnostics, completions) — press **Ctrl+Enter** to execute the template against the open document; failures render as readable error banners, results as pretty JSON.
- Drive it headlessly via the plain REST API (`/api/v/docs`, `…/pages/{n}/image.webp`, `…/pages/{n}/elements`, `/api/v/upload`).

Requires `cargo-leptos` (we use 0.2.42) and a pdfium dylib — the script resolves pdfium automatically; see [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md#viewer) for details.

## Repository map

| Crate | What it is |
|---|---|
| [`crates/delver-core`](crates/delver-core) | The engine: PDF parsing into typed elements, table detection, DocQL template compiler, matching, chunking. Pure and synchronous — no database dependency. |
| [`crates/delver-store`](crates/delver-store) | Postgres persistence (SQLx + pgvector): idempotent ingest, hydration back into the in-memory index, full-text & bbox queries. Async with a blocking facade. |
| [`crates/delver-embed`](crates/delver-embed) | `Embedder` backends for embedding-based matching: an HTTP serving-endpoint client and a deterministic mock for tests. |
| [`crates/delver`](crates/delver) | The `delver` CLI (`process` / `index` / `query` / `search`) and the `delver_pdf` Python module — both over one shared service layer. |
| [`crates/viewer`](crates/viewer) | Leptos (SSR + wasm) web viewer: page rasters, element/table overlays, DocQL editor, REST API. |

Depth lives in `docs/`:

- [docs/GETTING-STARTED.md](docs/GETTING-STARTED.md) — full environment setup, test data, store internals, CLI reference, troubleshooting.
- [docs/DOCQL.md](docs/DOCQL.md) — the DocQL language reference.
- [docs/DECISIONS.md](docs/DECISIONS.md) and [docs/DECISIONS-viewer.md](docs/DECISIONS-viewer.md) — the append-only design logs (the "why" behind everything above; docs here cite their `D-…`/`DV-…` entries).
- [docs/PARSER.md](docs/PARSER.md), [docs/COLLATION.md](docs/COLLATION.md), [docs/TEMPLATE_SYNTAX.md](docs/TEMPLATE_SYNTAX.md) — architecture notes on the parse pipeline and template/content alignment.

## Status and roadmap

Working today (verified on real SEC filings): parsing with text/table/annotation/path/figure/blob extraction, the persistent index, fuzzy/regex/heuristic section matching, `TYPE … AS TABLE` typed extraction, partitions and multi-document queries, Python bindings, and the viewer.

**Experimental / not yet there:**

- **`EmbeddingSim` matching and `method="semantic"` chunking** require an embedding endpoint (`--embed-endpoint` / `DELVER_EMBED_ENDPOINT`); without one they fail loudly rather than silently skipping. The bundled backend targets an HTTP serving-endpoint API; a local backend is future work.
- **Table cell spans** (merged cells) are not detected yet; `model=`/`targetSchema=` enrichment attributes on `Table`/`Image` parse but are not executed (a warning is logged).
- **OCR / scanned-PDF support** is not in this release — Delver currently targets born-digital PDFs.
- Some PDFs with unusual encodings can fail ingest (see [troubleshooting](docs/GETTING-STARTED.md#troubleshooting)).

Contributions and issue reports are welcome.

## License

**TBD.** This repository does not yet have a license file; until one is added, the code is source-available for evaluation but not licensed for redistribution or production use.
