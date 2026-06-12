# Getting Started (developer guide)

The [README Quickstart](../README.md#quickstart-5-minutes) gets you to a first query in five minutes; this page is the longer version — full environment setup, test data, how the persistent store works, the CLI reference, and troubleshooting. Design rationale is *not* duplicated here: the append-only decision logs [DECISIONS.md](DECISIONS.md) (engine/store/CLI, entries `D-001…`) and [DECISIONS-viewer.md](DECISIONS-viewer.md) (`DV-001…`) are the source of truth for the "why", and this page cites them.

- [Prerequisites](#prerequisites)
- [Build](#build)
- [Database setup](#database-setup)
- [Test data](#test-data)
- [How the persistent store works](#how-the-persistent-store-works)
- [Running the tests](#running-the-tests)
- [CLI reference](#cli-reference)
- [Python bindings](#python-bindings)
- [Viewer](#viewer)
- [Troubleshooting](#troubleshooting)

## Prerequisites

| What | Version we develop against | Needed for |
|---|---|---|
| Rust toolchain | 1.88 | everything |
| Postgres + pgvector | 17 / pgvector 0.8 | `index`, `query --doc/--corpus`, `search`, store tests, viewer |
| Docker **or** Homebrew | — | running that Postgres (two equivalent paths below) |
| [uv](https://docs.astral.sh/uv/) + [maturin](https://www.maturin.rs/) | maturin ≥ 1.8 | Python bindings only |
| `cargo-leptos` + wasm target | 0.2.42 / `wasm32-unknown-unknown` | viewer only |

`delver process` and `delver query --pdf` work with **no database at all** — you can evaluate the engine and DocQL before setting up Postgres.

## Build

```bash
git clone https://github.com/FMurray/delver.git
cd delver
cargo build -p delver          # the CLI; add --release for speed
./target/debug/delver --help
```

The first query run downloads the default tokenizer (`Qwen/Qwen2-7B-Instruct`) from Hugging Face and caches it; pass `--tokenizer-model none` to skip that and use character-based chunking (see [troubleshooting](#troubleshooting)).

## Database setup

Everything (CLI, Python, viewer, tests) resolves the database URL the same way (D-012):

```
--db flag  >  $DATABASE_URL  >  postgres://delver:delver@localhost:5433/delver
```

Schema migrations are embedded and run automatically on first connect — there is no separate migrate step. The store is deliberately *Lakebase-compatible* Postgres (D-002): pgvector for embeddings, the native `box` type + GiST for bboxes (no PostGIS), `tsvector` + GIN for text search. Port **5433** is used so a stock local Postgres on 5432 doesn't collide.

### Path A: Docker

```bash
./scripts/dev-db.sh
```

That's `docker compose -f docker-compose.dev.yml up -d --wait db` — image `pgvector/pgvector:pg17`, user/password/db `delver`/`delver`/`delver`, data in the `delver-pgdata` named volume.

### Path B: Homebrew Postgres

Fully equivalent when Docker isn't an option (e.g. Docker Desktop gated behind an org sign-in — the exact situation that motivated this path, D-011):

```bash
brew install postgresql@17 pgvector
initdb -D ~/delver-pg -U delver --pwfile=<(echo delver)
pg_ctl -D ~/delver-pg -o "-p 5433" start
createdb -p 5433 -U delver delver
export DATABASE_URL=postgres://delver:delver@localhost:5433/delver
```

If `initdb`/`pg_ctl` aren't found, Homebrew keeps postgresql@17 keg-only — add it to your PATH first: `export PATH="$(brew --prefix postgresql@17)/bin:$PATH"`. The `pgvector` formula installs the extension into postgresql@17, and the embedded migrations `CREATE EXTENSION vector` on first connect, so no manual extension setup is needed.

Verify either path:

```bash
psql "postgres://delver:delver@localhost:5433/delver" -c "select 1"
```

## Test data

The repo carries **no datasets** — only the fetcher:

```bash
./scripts/fetch-testdata.sh
```

The script's compliance banner is policy, not decoration (D-007): **this is a public repository, and fetched corpora are for local testing only** — they land outside the repo (`$DELVER_TESTDATA`, default `~/datasets/`) and must never be committed, redistributed, deployed, or uploaded. What it fetches:

1. **3M 2015 10-K** — the single-document demo fixture (1.2 MB) used throughout the docs. Just this one, without the script:
   ```bash
   mkdir -p ~/datasets && curl -sL -o ~/datasets/3M_2015_10K.pdf \
     https://raw.githubusercontent.com/patronus-ai/financebench/main/pdfs/3M_2015_10K.pdf
   ```
2. **[FinanceBench](https://github.com/patronus-ai/financebench)** — ~360 SEC filing PDFs + QA pairs. The underlying filings are public records; the QA *labels* are CC-BY-NC, hence local dev only.
3. **OfficeQA** (opt-in: `FETCH_OFFICEQA=1`) — Treasury Bulletin PDFs + QA. **Gated on Hugging Face**: one-time `hf auth login` and accepting the terms at [huggingface.co/datasets/databricks/officeqa](https://huggingface.co/datasets/databricks/officeqa) first; needs the `hf` CLI (`pip install -U 'huggingface_hub[cli]'`).
4. **OmniDocBench** (opt-in: `FETCH_OMNIDOCBENCH=1`) — layout/table ground truth.

## How the persistent store works

`crates/delver-store` owns the schema (three sqlx migrations, `SCHEMA_VERSION=3`). The mental model (D-003): **Postgres is the source of truth; the in-memory index is derived data.** Ingest persists parsed elements; *hydration* rebuilds the exact in-memory structures a fresh parse would have produced, and the round-trip is contract-tested — a template run against a hydrated document is equal (byte-identical for tables) to one against a fresh parse.

The tables:

| Table | Contents |
|---|---|
| `corpora` | Named document collections (`name` unique; created on demand by `index --corpus`). |
| `documents` | One row per ingested document: `content_sha256`, `uri`, `page_count`, `parse_version`, `metadata` jsonb (PDF Info subset + `partitions`). |
| `elements` | One row per parsed element in global reading order (`order_idx`): `kind` (`text`, `image`, `annotation`, `path`, `figure`, `blob`, `table`), `page`, `bbox` (`box` + GiST index), `text` (+ generated `tsvector`, GIN index), font fields, `metadata` jsonb. |
| `table_cells` | Per-cell grid of `kind=table` elements: `(table_element_id, row, col)` PK, spans, text, bbox, `is_header` (D-018). |
| `images` / `blobs` | Payload bytes for image elements and embedded files. |
| `element_refs` | Typed edges between elements (figure→image `contains`, figure→caption `caption-of`). |
| `embeddings` | pgvector cache keyed by `(element_id, model)`. |
| `index_meta` | `schema_version`, `delver_version`, tokenizer id. |

**Idempotent ingest (D-008):** documents are keyed by `(corpus, sha256 of the bytes, parse_version)`. Re-ingesting identical bytes with the same `--parse-version` is a no-op returning the existing id (`"created": false`); bumping `--parse-version` re-parses the same bytes as a *new* document without disturbing the old one (provenance across parser upgrades). Partition tags are the one exception: they are (re)applied on every `index`, replacing the previous `partitions` object (D-023).

The original PDF bytes are **not** stored in Postgres. The viewer keeps them in a content-addressed byte-cache (`$DELVER_DOC_CACHE`, default `~/.delver/doc-cache/<sha256>.pdf`) and records that path as the document `uri` (DV-002); CLI-ingested documents store whatever `--uri` you pass (or none — the viewer then shows a placeholder instead of page rasters).

## Running the tests

```bash
DATABASE_URL=postgres://delver:delver@localhost:5433/delver cargo test --workspace
```

What to expect:

- **No fixtures needed** — integration tests generate small PDFs in-memory via lopdf (D-009); nothing is downloaded.
- **DB-backed tests skip, loudly, when Postgres is unreachable.** They use `$DATABASE_URL` (falling back to the dev default URL) and, if they cannot connect, print `SKIP <test>: Postgres unreachable at <url> (…); set DATABASE_URL or run scripts/dev-db.sh` and pass vacuously — so a no-database `cargo test` run is still green, just thinner.
- **Test residue is normal**: store/CLI tests create uniquely-named corpora (`cli-slice2-…`, `roundtrip-…`, …) and don't delete them (DV-010). Your dev database accumulates them; they're harmless. Drop and recreate the `delver` database if you want a clean slate.

## CLI reference

Generated from `--help` at version `0.2.0-rc.1` (`delver --version`). All of `index`/`query`/`search` print exactly **one JSON document on stdout**; diagnostics go to stderr (D-013/D-017), and `… | head` is safe (SIGPIPE terminates silently, D-019).

```text
Parse, index, query, and search PDF documents with DocQL templates.

Usage: delver <COMMAND>

Commands:
  process  Extract template outputs from a PDF (fresh parse, no database)
  index    Parse a PDF and persist its element index to Postgres
  query    Execute a DocQL template against a stored document or a PDF file
  search   Full-text search over a stored corpus or document
```

### `delver index`

```text
Usage: delver index [OPTIONS] --corpus <CORPUS> <PDF_PATH>

Arguments:
  <PDF_PATH>  Path to the PDF file to ingest

Options:
      --corpus <CORPUS>                Corpus name (created if it does not exist)
      --uri <URI>                      Optional source URI recorded with the document
      --parse-version <PARSE_VERSION>  Parser version for idempotent re-ingest (D-008) [default: 1]
      --partition <KEY=VALUE>          Partition key=value stored with the document (repeatable). Merged
                                       over key=value segments auto-inferred from the input path's
                                       directories (e.g. /loans/state=CA/x.pdf); explicit flags win
      --db <DB>                        Postgres URL (default: $DATABASE_URL, then the local dev database)
```

Prints an ingest receipt: `{"corpus", "created", "document_id", "element_count", "partitions"}`.

### `delver query`

```text
Usage: delver query [OPTIONS] --template <TEMPLATE> <--doc <DOC>|--pdf <PDF>|--corpus <CORPUS>>

Options:
  -t, --template <TEMPLATE>            Path to the template file
      --doc <DOC>                      Stored document id to query (hydrates the index from Postgres)
      --pdf <PDF>                      PDF file to query with a fresh parse (no database)
      --corpus <CORPUS>                Run the template across every stored document of this corpus
                                       (filtered by --where); output is keyed by document id
      --where <KEY=VALUE>              Partition filter key=value (repeatable; documents must match all).
                                       Only meaningful with --corpus
      --db <DB>                        Postgres URL (default: $DATABASE_URL, then the local dev database)
  -p, --pretty                         Pretty-print the JSON output
      --tokenizer-model <TOKENIZER_MODEL>  Tokenizer model name ("none" for character-based chunking)
                                       [default: Qwen/Qwen2-7B-Instruct]
      --embed-endpoint <EMBED_ENDPOINT>    Databricks embedding endpoint (name or full URL) for
                                       EmbeddingSim matches; falls back to $DELVER_EMBED_ENDPOINT
```

Output: a JSON array of [DocQL outputs](DOCQL.md#output-object-reference) (`--doc`/`--pdf`), or an object keyed by document id (`--corpus`). Match-time failures exit non-zero with the error on stderr and nothing on stdout.

### `delver search`

```text
Usage: delver search [OPTIONS] --corpus <CORPUS> <QUERY>

Arguments:
  <QUERY>  Full-text query (Postgres plainto_tsquery semantics)

Options:
      --corpus <CORPUS>    Corpus to search
      --doc <DOC>          Restrict the search to one stored document
      --where <KEY=VALUE>  Partition filter key=value (repeatable; documents must match all)
      --limit <LIMIT>      Maximum number of hits [default: 10]
      --db <DB>            Postgres URL (default: $DATABASE_URL, then the local dev database)
```

Prints a JSON array of `{"element_id", "document_id", "page", "rank", "snippet"}` (snippet = element text truncated to 200 chars).

### `delver process`

The original pre-store one-shot command, kept verbatim (D-012): fresh parse + template, no database, always-pretty output, plus debug tooling (`--debug-ops`, `--log-dir`, `--password` for encrypted PDFs). Note that `process` writes legacy `LOGGING:` lines to stdout — for clean, pipeable stdout use `delver query --pdf` instead, or `process -o <file>` to route the JSON to a file.

## Python bindings

```bash
uv tool install maturin            # once
uv venv .venv && source .venv/bin/activate
maturin develop --uv -m crates/delver/Cargo.toml --features extension-module
python -c "import delver_pdf; print(delver_pdf.__name__)"
```

(Fallback without uv: `python3 -m venv .venv && source .venv/bin/activate && pip install maturin`, then the same `maturin develop` minus `--uv`.) Requires Python ≥ 3.11 (`crates/delver/pyproject.toml`).

The module surface (one shared service layer with the CLI, so JSON shapes match exactly — D-012):

- `delver_pdf.process_pdf_file(pdf_path, template_path) -> str` — one-shot, no DB.
- `delver_pdf.DelverStore(db_url=None)` — `ingest(path, corpus, uri=None, parse_version=None)`, `search(query, corpus, limit=None)`, `run_template(doc_id, template_source, tokenizer_model=None, embed_endpoint=None)`; all return JSON strings.

See the [README Python section](../README.md#python) for a verified end-to-end snippet.

## Viewer

```bash
cargo install cargo-leptos --version 0.2.42   # once
rustup target add wasm32-unknown-unknown      # once
./scripts/dev-viewer.sh                       # serves http://127.0.0.1:3017
```

The script exports the shared dev `DATABASE_URL`, creates the byte-cache dir, resolves the pdfium dylib, and runs `cargo leptos serve` (server + wasm build; the first build takes a while). Server-side page rendering needs **pdfium**: the viewer's `build.rs` downloads a prebuilt library (from the public `bblanchon/pdfium-binaries` releases) into `target/debug/` if one isn't already there, and `PDFIUM_LIBRARY_PATH` overrides resolution at runtime (DV-005).

Capabilities and REST surface are summarized in the [README](../README.md#viewer); design history (byte-cache, raster LRU, table-cell overlays, SSR pitfalls) is in [DECISIONS-viewer.md](DECISIONS-viewer.md). The viewer shares the dev database with the CLI — documents you `delver index` appear in its list (newest first), interleaved with any test-residue corpora.

## Troubleshooting

**Port 5433 already in use / Docker blocked.** Something else is serving 5433 (often: you already followed Path B, and then ran `scripts/dev-db.sh` too — they're alternatives, not steps). Use whichever instance is up, or stop one. If Docker Desktop is gated behind an organization sign-in, Path B is the supported escape hatch (D-011).

**`could not open extension control file … vector.control`** on first connect: the embedded migrations run `CREATE EXTENSION vector`, so the *server* must have pgvector installed. The `pgvector/pgvector:pg17` image has it; for Homebrew, `brew install pgvector` (it installs into postgresql@17). A plain stock Postgres won't work.

**`VersionMismatch` / migration errors on an existing database**: the schema is migration-versioned; a database touched by a *newer* checkout refuses an older one. One database per branch that owns migrations (DV-007) — or drop and recreate `delver` (cheap: documents re-ingest idempotently).

**`delver query … | head` panics with "Broken pipe"**: fixed on master (D-019) — the CLI restores default SIGPIPE handling and terminates silently. If you see this, you're on an older build; rebuild.

**Tokenizer warnings / offline runs.** The default tokenizer is fetched from Hugging Face on first use and cached. If it can't be fetched, `query` warns on stderr and falls back to character-based chunking (chunk *sizes* change, nothing else). For deterministic offline behavior pass `--tokenizer-model none`.

**`index` fails with `unsupported Unicode escape sequence`.** Some PDFs yield text containing NUL (0x00, i.e. \\u0000), which Postgres cannot store in `text`/`jsonb`; ingest of that document fails (e.g. `crates/delver/tests/AAPL_2024_10K.pdf` reproduces this). Known limitation on master — the no-database paths (`query --pdf`, `process`) still work on such files.

**OfficeQA fetch fails.** The dataset is Hugging Face-gated: accept the terms at [huggingface.co/datasets/databricks/officeqa](https://huggingface.co/datasets/databricks/officeqa), run `hf auth login`, then `FETCH_OFFICEQA=1 ./scripts/fetch-testdata.sh`.

**`EmbeddingSim`/semantic chunking errors about a missing embedder** — that's [by design](DOCQL.md#embeddingsim--semantic-similarity-experimental): configure `--embed-endpoint` / `DELVER_EMBED_ENDPOINT` (the bundled backend also needs `DATABRICKS_HOST` + `DATABRICKS_TOKEN`), or remove the embedding constructs from the template.
