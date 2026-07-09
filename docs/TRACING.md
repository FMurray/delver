# Execution tracing (T1, D-027)

`delver` can narrate its own execution end to end — CLI arguments → store
(connect/ingest/load/search) → hydration → template compile → the two-pass
matcher (boundary candidates, scores, why a boundary won) → collation/chunking
→ what got persisted or printed. The traces narrate the *semantic* pipeline,
not function entry/exit: read one and you know which template compiled to
what, which section-match candidates were considered at what score, how each
end boundary was resolved, and what pass 2 assigned.

**Default is off, and off means byte-exact off**: without a trace flag,
`index`/`query`/`search` write the same stdout bytes as before this feature
and exactly 0 bytes of stderr; `process` keeps its historical debug
subscriber. With tracing on, stdout is *still* byte-identical — traces ride
on stderr, a file, or the collector, never stdout (D-013). Both properties
are enforced by `crates/delver/tests/trace_cli.rs`, including the 10-K
regression baselines (414 534 / 466 678 bytes).

## Enabling

| Switch | Effect |
| --- | --- |
| `--trace` | Export the run's span tree to an OpenTelemetry collector: OTLP/HTTP JSON to `$OTEL_EXPORTER_OTLP_ENDPOINT` (default `http://localhost:4318`). If the collector is unreachable, warns once on stderr and falls back to the stderr tree — the flag never silently does nothing. |
| `--trace-stderr` | Hierarchical tree on stderr (tracing-tree). No infrastructure needed. |
| `--trace-json <path>` | Structured JSON-lines (one span open/close or event per line, with span context) written to `<path>`. |
| `DELVER_TRACE=1` | Environment equivalent of `--trace`. |
| `RUST_LOG=...` | Filter override for all sinks. When unset, the default is `warn,delver=trace,delver_core=trace,delver_store=trace,delver_core::parse=info` — the full delver vocabulary, dependencies at `warn`, and the per-operator content-stream firehose gated off. |

The flags compose (e.g. `--trace --trace-json run.jsonl` exports and writes
the file). They exist on `query`, `search`, `index`, and `process`; for
`process`, trace layers COMPOSE with the historical `--debug-ops` capture
layer rather than replacing it (D-017).

Want the byte-level parser firehose (a span per content-stream operator, a
span per text run)? That is deliberate opt-in:
`RUST_LOG='warn,delver=trace,delver_core=trace,delver_store=trace' delver … --trace-stderr`
(or `delver_core::parse=trace` for everything). Expect millions of lines on a
real filing.

## Visual setup (Jaeger)

```bash
./scripts/dev-otel.sh          # downloads single-binary Jaeger v2 to ~/.delver/bin,
                               # starts it: OTLP :4318 + UI :16686 (no Docker)
delver query --doc 56e30967-eff1-4c0f-acdb-3fa13b30d4ef \
       --template crates/delver/tests/10k.tmpl --trace
# stderr prints:  trace: exported N spans to http://localhost:4318 — http://localhost:16686/trace/<id>
open http://localhost:16686    # service "delver" — every span/event below, on a timeline
```

The exporter is a hand-rolled OTLP/HTTP JSON layer
(`crates/delver/src/otel.rs`): spans buffer in-process and flush once on exit
with the run's subcommand as a resource attribute (`service.name=delver`).
Jaeger stores traces in memory — restarting it clears them. Stop it with
`kill $(cat ${TMPDIR:-/tmp}/delver-jaeger.pid)`.

## Span vocabulary

Span names are stable identifiers (tests grep for them). Events (`·`) attach
to the span they fire under. All text fields are char-truncated previews.

| Span / · event | Where | Key fields | What it tells you |
| --- | --- | --- | --- |
| `cli.query` / `cli.search` / `cli.index` / `cli.process` | delver (CLI) | the subcommand's arguments (doc/pdf/corpus, template, `--where`, engine, limit, ...) | Trace root; one per run — everything below nests under it. |
| `connect` | delver-store | `url` (password redacted) | DB connect; · `migrations applied` carries `schema_version`. |
| `ensure_corpus` | delver-store | `name` | Get-or-create corpus. |
| `ingest` | delver-store | `corpus`, `bytes`, `parse_version`, `parsed_by_caller` | One document ingest. · `dedup hit` = D-008 idempotency short-circuit (nothing parsed or written). |
| `parse` → `content_stream`, `figures`, `embedded_files`, `scan_classify` | delver-core | — | The parse phases. · walk summary (`pages`, `texts`, `images`, `aux_by_kind` incl. detected tables), · figure `ref_edges`, · scan verdict (`class`, `scanned_page_ratio`, `confidence`, `producer_hint`). |
| `persist` | delver-store | `page_count`, `parse_version` | The UNNEST bulk insert. · staged rows `by_kind` + `table_cells`/`blobs`/`ref_edges`/`scan_class`, · `transaction committed` with the new `document_id`. |
| `load_document` | delver-store | `doc` | Row fetch in `order_idx` order; · `rows`, `tables_with_cells`. |
| `hydrate` | delver-store | `rows` | Stored rows → fresh-parse page shape; · `pages`. Detection never re-runs (D-011/D-016/D-018). |
| `compile_template` | delver-core | `bytes` | DocQL compile. · WARNs for tolerated-but-unimplemented attrs (D-018 `model=`), · compiled summary: `element_names`, `match_definitions`, `types`, `sub_corpora`. |
| `build_index` | delver-core | — | · `elements`, `pages` of the in-memory `PdfIndex`. |
| `match_template` | delver-core | — | Wraps the whole two-pass alignment. |
| `pass1` | delver-core (matcher) | `section`, `depth` | One per `Section` in template order; child sections nest inside their parent's `pass1`. · `section matched — content partition claimed` (`start_idx`/`end_idx`/pages) or · `matched nothing`. |
| `start_boundary` | delver-core (matcher) | `matcher`, `pattern`, `threshold`, `start_index`, `max_index` | The start-marker search scope. · one `boundary_candidate` per candidate (`score`, `base_score`, `page`, `text`, `reasons` — the bonus narration), · `winner` with the ranked count. |
| `end_boundary` | delver-core (matcher) | `explicit_end_match`, `start_page` | End-marker resolution. · `boundary_candidate` events carry `source="explicit_end_match"` or `source="style_similarity"`; · `resolved` carries `path` ∈ `explicit_end_match` \| `style_similarity` \| `parent_end_marker` \| `next_similar_font` \| `document_end`. |
| · `match_miss` | delver-core (matcher) | `match_name`, `pattern`, `threshold`, `near_misses` | A match config found zero candidates; the top-3 rescue-scored near misses (D-024) — the same data the CLI `warning:` line prints. |
| · `text_match` | delver-core (search_index) | `pass`, `hits` | Which `Text()` scoring pass produced the candidates: pass 1 = pre-D-024 whole-string scorer, pass 2 = quote-folded substring rescue (only after pass 1 found nothing). |
| · `style_similarity` | delver-core (search_index) | `seed_text`, `seed_font_size`, range, `candidates` | The packed style-key bucket probe behind the no-`end_match` fallback and `top_k_similar_text`. |
| `pass2` | delver-core (matcher) | `depth` | Non-structural assignment. · per element: routed slice (`range_start`/`range_end`) and `assigned` count (TextChunk/Annotation/Figure/Table). |
| `collate` | delver-core (docql) | — | Output assembly. · `chunk outputs appended`, · `table output … deferred to array tail (D-018)` with `page`/`n_rows`/`n_cols`/`strategy`/`confidence`. |
| `chunk` | delver-core (docql) | `element`, `method`, `chunk_size`, `chunk_overlap`, `source_elements`, `tokenizer` | One per chunked element (including a Section's own content). · `chunks` formed (+ `segments` for `method="semantic"`, whose batch embed logs · `semantic chunking: embedding …`). |
| · `typed_table` | delver-core (udt/docql) | `type_name`, `mapping`, `coerced_ok`, `coerced_err` | `TYPE … AS TABLE` extraction (D-021): which grid column each field claimed (header-match vs positional) and the coercion tallies. |
| `embed_match` | delver-core (matcher) | `owner`, `query`, `threshold`, `candidates` | `EmbeddingSim(...)` execution; · batch size. Endpoint identity is CLI/env config; tokens are never logged. |
| `text_search` | delver-store | `scope`, `corpus`/`doc`, `query`, `limit`, `filter` | FTS execution (`plainto_tsquery` + `ts_rank`; jsonb containment for `--where`); · `hits`. |
| `documents_matching` | delver-store | `corpus`, `filter` | Multi-doc `query --corpus` candidate set; · `documents`. |
| `corpus_doc` | delver (CLI) | `doc` | Per-document wrapper inside a `query --corpus` run. |
| `elements_in_bbox` / `set_partitions` / `element_count` | delver-store | `doc`, bbox / `partitions` | Spatial lookups, partition tagging (D-023), receipt counts. |

## Annotated trace (a): `index` — parse phases → scan classification → store ingest

```bash
delver index ~/datasets/3M_2015_10K.pdf --corpus trace-demo-scratch --trace
```

```text
┐delver::cli.index pdf=…/3M_2015_10K.pdf, corpus=trace-demo-scratch, parse_version=1, engine=Native, partition_flags=[]
└─┐delver_store::store::connect url=<the local dev DB url, password shown as ***>   ← credentials never reach traces
  ├─ DEBUG migrations applied, schema_version=3
└─┐delver_store::store::ensure_corpus name="trace-demo-scratch"
└─┐delver_store::store::ingest corpus=CorpusId(0be31924-…), bytes=1160173, parsed_by_caller=false
  └─┐delver_core::parse::parse                                  ← ingest was no dedup hit, so the full parse runs INSIDE the store span
    └─┐delver_core::parse::content_stream
      ├─ INFO content-stream walk done: …, pages=158, texts=11476, images=0,
      │       aux_by_kind={"Annotation": 79, "Path": 14977, "Table": 125}    ← same counts D-016/D-018 recorded as real-doc evidence
    └─┐delver_core::parse::figures
      ├─ INFO figure grouping done (image + adjacent caption line, D-016), ref_edges=0   ← no images ⇒ no figures in this filing
    └─┐delver_core::parse::embedded_files
    └─┐delver_core::parse::scan_classify
      ├─ INFO scan classification: per-doc verdict …, class=native, scanned_page_ratio=0.0, confidence=1.0, pages=158
  └─┐delver_store::store::persist page_count=158
    ├─ INFO persist: element rows staged for UNNEST bulk insert (order_idx = global document order),
    │       elements=26657, by_kind={"annotation": 79, "path": 14977, "table": 125, "text": 11476},
    │       table_cells=11615, ref_edges=0, scan_class="native"
    ├─ WARN sqlx::query slow statement: … "INSERT INTO elements …"            ← deps stay at warn; real slowness still surfaces
    ├─ INFO persist: transaction committed — document created, document_id=57eb039f-…
└─┐delver_store::store::element_count doc=DocumentId(57eb039f-…)              ← the COUNT(*) behind the JSON receipt
┘
```

Re-running the same command narrates the D-008 path instead: the `ingest`
span contains a single `dedup hit — identical bytes at this parse_version
already stored; nothing re-parsed or written` event and no `parse`/`persist`
children.

## Annotated trace (b): `query --doc` — hydrate → compile → pass-1 boundaries → pass-2 → outputs

```bash
delver query --doc 56e30967-eff1-4c0f-acdb-3fa13b30d4ef \
      --template crates/delver/tests/10k.tmpl --trace
```

Trimmed (repeated candidates elided with `⋮`); annotations at the right.

```text
┐delver::cli.query template=crates/delver/tests/10k.tmpl, doc=56e30967-…, tokenizer_model=Qwen/Qwen2-7B-Instruct
└─┐connect url=<the local dev DB url, password shown as ***>
└─┐load_document doc=56e30967-…
  ├─ INFO … rows=26657, tables_with_cells=125            ← the --parse-version 3 document (D-018 evidence)
└─┐hydrate rows=26657
  ├─ INFO … no detection re-runs (D-011/D-016/D-018), pages=158
└─┐compile_template bytes=737
  ├─ WARN Table 'Table': attribute(s) model, targetSchema are not implemented yet; …   ← the D-018 documented exception
  ├─ INFO template compiled: …, element_names=["TextChunk 'TextChunk'", "Section 'Section'"], match_definitions=0
└─┐build_index
  ├─ INFO in-memory PdfIndex built …, elements=26657, pages=158
└─┐match_template
  └─┐pass1 section=Section, depth=0                       ← the MD&A section
    └─┐start_boundary matcher=Text, pattern=Management’s Discussion and Analysis of…, threshold=0.6, start_index=0
      ├─ text_match: whole-string Levenshtein pass matched (pre-D-024 scoring wins outright), pass=1, hits=6
      ├─ boundary_candidate score=0.876, page=2,  text=ITEM 7  Management’s Discussion and…  reasons=[]          ← the TOC row
      ├─ boundary_candidate score=0.839, page=9,  text=This Annual Report on Form 10-K, including…  reasons=["Top of page"]  ← cross-reference
      ⋮  (3 more page-9/16 candidates)
      ├─ boundary_candidate score=1.047, page=16, text= Discussion and Analysis of…  reasons=["Top of page"]     ← the real heading (split row) + bonus
    ├─ INFO start_boundary: winner …, page=16, score=1.047                    ← why p16 beat the p2 TOC entry: base 0.847 + top-of-page 0.2
    └─┐end_boundary explicit_end_match=true, start_page=16
      ├─ boundary_candidate source="explicit_end_match", score=1.476, page=45, text= and Qualitative Disclosures About Market Risk.
      ├─ style_similarity: probed packed style-key buckets … candidates=5     ← similarity candidates still compete (tie-breakers)
      ⋮  (5 style candidates, all score 0.1)
    ├─ INFO end_boundary: resolved, path="explicit_end_match", page=45        ← the template's end_match won
    └─┐pass1 section=Section, depth=1                     ← nested: PERFORMANCE BY BUSINESS SEGMENT
      └─┐start_boundary pattern=PERFORMANCE BY BUSINESS SEGMENT, threshold=0.6, start_index=1704   ← scoped INSIDE the parent partition
        ├─ boundary_candidate score=1.0, page=24, text=PERFORMANCE BY BUSINESS SEGMENT             ← exact heading
      └─┐end_boundary explicit_end_match=false, start_page=24
        ├─ end_boundary: no end_match declared — style-similarity candidates only                  ← THE FALLBACK: this Section has no end_match
        ├─ style_similarity: probed packed style-key buckets (font/size/z/pos/caps) …
        ├─ boundary_candidate source="style_similarity", similarity=1.0, page=32, text=PERFORMANCE BY GEOGRAPHIC AREA
        ⋮  (CRITICAL ACCOUNTING ESTIMATES p34, NEW ACCOUNTING PRONOUNCEMENTS p37, … — every same-styled heading)
      ├─ INFO end_boundary: resolved, path="style_similarity", text=PERFORMANCE BY GEOGRAPHIC AREA, page=32
      │        ← ties broke by document position: the NEXT same-styled heading ends the section
      └─┐pass2 depth=2
        ├─ INFO pass2: content assigned, element=TextChunk, range=2949..3977, assigned=494
        ├─ INFO pass2: content assigned, element=Table,     range=2949..3977, assigned=6    ← the six segment tables (D-018)
      ├─ INFO pass1: section matched — content partition claimed, start_idx=2949, end_idx=3977, start_page=24, end_page=32
    ⋮  (sibling pass1 for PERFORMANCE BY GEOGRAPHIC AREA: starts at index 3977 — never backtracks — ends at parent boundary p45,
        path="style_similarity"; its Image child is not a pass-2 type and matches nothing)
  ├─ INFO pass1: section matched — content partition claimed, section=Section(MD&A), start_idx=1704, end_idx=6173, pages 16→45
  └─┐pass2 depth=0
    ├─ INFO pass2: content assigned, element=TextChunk, range=0..1704, assigned=897   ← the top-level TextChunk gets the PRE-section
                                                                                        slice; 897 of the 1704 slots are text (aux skipped)
└─┐collate
  └─┐chunk element=TextChunk, method=Tokens, chunk_size=1000, chunk_overlap=250, source_elements=897, tokenizer=true
    ├─ INFO chunk: contiguous element slices formed, chunks=17
  ⋮  (per-element chunk spans follow: MD&A's own content 1984 → 83 chunks and its TextChunk 590 → 27,
      the nested sections' 494 → 18 and 242 → 6 — each at chunkSize=500)
  ├─ collate: table output (structural, untyped) — deferred to array tail (D-018), page=25, n_rows=8,  n_cols=3, strategy=Ruled, confidence=0.913
  ⋮  (5 more tables: pages 26/27/28/30/31 — exactly the D-018 regression additions)
├─ INFO template outputs assembled, outputs=181            ← matches the 466 678-byte / 181-object baseline
┘
```

## Annotated trace (c): partition-scoped `search --where`

```bash
delver search "net sales" --corpus demo --where year=2015 --limit 3 --trace
```

```text
┐delver::cli.search query=net sales, corpus=demo, r#where=["year=2015"], limit=3
└─┐connect url=<the local dev DB url, password shown as ***>
└─┐ensure_corpus name="demo"
└─┐text_search scope="corpus", corpus=CorpusId(a8b3b730-…), filter=Some(Object {"partitions": Object {"year": String("2015")}})
  │        ← the --where pair became a jsonb containment filter over documents.metadata (D-023)
  ├─ INFO text_search: plainto_tsquery over elements.text_fts, jsonb-containment partition filter, ts_rank ordered, hits=3
┘
```

All three hits come from the single `year=2015` document of the `demo`
corpus (`company=3M` has one document per year) — the partition filter is
visible both as the span's `filter` field and in the hit count.

## Reading traces programmatically

`--trace-json run.jsonl` writes one JSON document per line; span opens/closes
carry the span's fields under `span` and the enclosing stack under `spans`,
events carry `fields`. The OTLP export (`--trace`) is the same tree with
parent/child edges explicit (`spanId`/`parentSpanId`), one trace per CLI run,
events attached to their spans — see `crates/delver/src/otel.rs` for the
exact encoding (unit-tested against the OTLP/HTTP JSON spec shape, and
end-to-end against an in-test collector in
`crates/delver/tests/trace_cli.rs`).
