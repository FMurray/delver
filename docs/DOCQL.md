# DocQL Language Reference

DocQL is Delver's declarative query language: a template describes the *shape* of what you want out of a document, and the engine aligns the parsed document to it. This page is the language reference for the syntax that executes on `master` today. Examples marked **(ran)** were executed against the public 3M 2015 10-K fixture exactly as shown — see the [Quickstart](../README.md#quickstart-5-minutes) for how to index it.

Run a template with any of:

```bash
delver query --doc <uuid>   --template t.tmpl     # against a stored document
delver query --pdf file.pdf --template t.tmpl     # fresh parse, no database
delver query --corpus name  --template t.tmpl     # across a whole corpus
```

…or from Python (`DelverStore.run_template`), or in the [viewer](GETTING-STARTED.md#viewer)'s editor with Ctrl+Enter.

The grammar lives in [`crates/delver-core/src/docql.pest`](../crates/delver-core/src/docql.pest). Note: there is **no comment syntax** in DocQL yet — don't put `//` or `#` lines in templates.

## Template anatomy

A template is a sequence of, in any order:

1. **`TYPE` definitions** — record schemas for typed table extraction;
2. **`Match` definitions** — named, reusable matching rules;
3. **`SubCorpus` declarations** — named description constants for interpolation;
4. **elements** — the tree of `Section { … }`, `TextChunk(…)`, `Table(…)`, etc. that actually selects content.

```text
TYPE SegmentPerformance AS TABLE ( metric TEXT, y2015 DECIMAL, y2014 DECIMAL, y2013 DECIMAL );

Match<Section> perfHeading {
  Text("PERFORMANCE BY BUSINESS SEGMENT", threshold=0.8)
}

Section(match=perfHeading, as="segments") {
  TextChunk(chunkSize=500, chunkOverlap=150)
  Table(as="segment_performance", type="SegmentPerformance")
}
```

Elements are written `Name(attr=value, …) { children }`; both the attribute list and the body are optional. Attribute values are strings (`"…"`), numbers (`0.6`, `500`), booleans, or identifiers. Output is always a single JSON array of output objects (one JSON object keyed by document id for corpus queries).

Top-level siblings partition the document in order: content before the first matched `Section` belongs to the elements above it, and each `Section` claims the range from its start match to its end boundary.

---

## Element types

### `Section`

Matches a structural region of the document; children only see content inside it.

| Attribute | Meaning |
|---|---|
| `match=` | **Required.** What starts the section. A string → fuzzy text match (normalized Levenshtein, default threshold 0.6) against element text; an identifier → reference to a `Match` definition (see below). |
| `end_match=` | Where the section ends. Same forms as `match=`. If omitted, the end is inferred heuristically (next similarly-styled heading via the font-similarity fallback, else end of document) — specify it when you need precise boundaries. |
| `as=` | Label for the section; stamped into the `metadata.section` of every output produced inside it. |

A `Section`'s own direct content is text-chunked even when its only children are selectors like `Table` — nest a `TextChunk` to control the chunk size, or expect default chunks alongside your other outputs.

**(ran)** — section by fuzzy string with an explicit end, typed table inside (this is the [README teaser](../README.md#a-taste-of-docql-typed-tables); 18 text chunks + 6 typed tables on the 3M 10-K):

```text
Section(
  match="PERFORMANCE BY BUSINESS SEGMENT",
  end_match="PERFORMANCE BY GEOGRAPHIC AREA",
  as="segments"
) {
  Table(as="segment_performance", type="SegmentPerformance")
}
```

Sections nest arbitrarily: an inner `Section` only searches within its parent's range, and metadata accumulates downward.

### `TextChunk`

Splits the text in scope into chunks.

| Attribute | Meaning |
|---|---|
| `chunkSize=` | Budget per chunk — **tokens** when a tokenizer is configured (CLI default `Qwen/Qwen2-7B-Instruct`; pass `--tokenizer-model none` for character counting), characters otherwise. |
| `chunkOverlap=` | Trailing content carried into the next chunk (same unit as `chunkSize`). |
| `method=` | `"tokens"` (default; also the behavior when absent) or `"semantic"` (**experimental** — embedding-driven topic splitting, requires an embedding endpoint). Any other value is a compile error listing the supported set. |
| `breakpointPercentile=` | Semantic mode only: integer 0–100 (default 25); adjacent-segment similarities below this percentile become chunk boundaries. `0` disables valley splitting. Using it without `method="semantic"` is a compile error. |
| `template=` | Output interpolation template — see [SubCorpus & interpolation](#subcorpus--template-interpolation). |
| `as=` | Names the chunk stream (used in error messages and metadata). |

Output objects have `"type": "Text"` with `text`, `chunk_index`, `parent_name`/`parent_index` (links to the enclosing section's output), and `metadata` including `chunk_char_count`, `chunk_element_count`, `page_numbers`, `primary_page`, and any inherited section labels. Semantic-mode chunks additionally carry `method: "semantic"` and `segment_count`.

**(ran)** — running `method="semantic"` without an embedding endpoint fails loudly rather than silently degrading:

```text
TextChunk(method="semantic", chunkSize=800, breakpointPercentile=25)
```

```text
Error: TextChunk: method="semantic" requires an embedder but none is configured;
pass --embed-endpoint <name-or-url> or set DELVER_EMBED_ENDPOINT (D-006: no silent skip)
```

### `Paragraph`

Processed identically to `TextChunk` (same attributes, same `Text` outputs); use whichever reads better in your template.

### `Table`

Selects detected tables in scope — one output per table.

| Attribute | Meaning |
|---|---|
| `as=` | Output name. |
| `type=` | Coerce each table into a `TYPE … AS TABLE` record schema (see [Typed tables](#typed-tables-type--as-table)). Accepts `type="Name"` or bare `type=Name`. Referencing an undefined type, or putting `type=` on a non-`Table` element, is a compile error. |
| `model=`, `targetSchema=` | Parse but are **not executed** on master (LLM output enrichment is future work); template compile logs a warning and structural extraction proceeds. |

Tables are detected at parse time by three deterministic strategies — `ruled` (full line lattices, including the shaded-row style of SEC HTML-to-PDF filings), `row-ruled` (horizontal rules only), and `aligned` (borderless columns by edge alignment) — each output records its `strategy` and a `confidence` in (0, 1]. Detection internals and known limits (no merged-cell spans yet; very small tables below the 2×2 floor) are logged in [DECISIONS.md](DECISIONS.md) D-018.

Untyped output shape: `{"type": "Table", name, page, bbox, n_rows, n_cols, header, rows, cells, strategy, confidence, metadata, parent_name, parent_index}` where `header` is the detected header row's texts, `rows` are body-row texts, and `cells` carries the full per-cell objects (`row`, `col`, spans, bbox, text, `is_header`).

Note: `Table` (and `TypedTable`) outputs are appended **after** all positional outputs in the array, so text-chunk `parent_index` links stay stable; they carry their own `parent_name`/`parent_index` captured at match time.

**(ran)** — count every detected table in the document (125 on the 3M 10-K):

```text
Table(as="tables")
```

### `Annotation`

Selects PDF annotations in scope — links, comments, etc. One output per annotation: `{"type": "Annotation", id, page_number, bbox, text, metadata {subtype, uri?, dest?}, parent_*}`. Annotation `Contents` text is full-text searchable in the store.

**(ran)** — all 79 link annotations of the 3M 10-K:

```text
Annotation(as="links")
```

```json
{
  "type": "Annotation",
  "id": "7b3b0dd1-2fae-4063-a281-1258b75420f0",
  "page_number": 2,
  "bbox": [81.0, 116.25, 105.0, 125.25],
  "text": null,
  "metadata": {"name": "links", "subtype": "Link", "dest": "…#PARTI_522757"}
}
```

### `Figure`

Selects figure groupings — an embedded image plus its caption line (`Figure 3: …`, `Chart …`, `Exhibit …`). Grouping is conservative: **no caption ⇒ no figure** (the bare image is still an element, just not a figure). Output: `{"type": "Figure", id, page_number, bbox, caption, image_id, caption_id, …}` plus `contains` / `caption-of` reference edges in the store.

```text
Figure(as="charts")
```

(The 3M fixture contains no extractable raster images, so this selector yields nothing there — pick a chart-heavy PDF to see it.)

### `Image`

Selects raw embedded images matched in scope; outputs `{id, page_number, bbox, caption, bytes_base64, summary, …}`. The `model=`/`prompt=` enrichment attributes parse but are not executed on master.

---

## Match definitions

Named matching rules, declared once and referenced from `match=`/`end_match=`:

```text
Match<Section> name {
  RuleFunction(…)
}
```

`<Section>` is the target element type; the body holds one or more rule functions. **Multiple rules in one definition resolve to an implicit `FirstMatch`** (tried in order, first one that matches wins). A matcher that cannot execute — unknown function, invalid configuration, missing embedder — is a hard error, never a silent no-op (see [Fail-loud errors](#fail-loud-errors)).

### `Text` — fuzzy text

`Text("pattern", threshold=0.8)` — normalized Levenshtein similarity against element text; `threshold` defaults to **0.8** (a bare `match="string"` attribute uses a looser 0.6).

**(ran)** — survives the curly-vs-ASCII apostrophe difference in the real heading:

```text
Match<Section> mdna {
  Text("Management's Discussion and Analysis of Financial Condition and Results of Operations", threshold=0.7)
}

Section(match=mdna, end_match="Quantitative and Qualitative Disclosures About Market Risk", as="MD&A") {
  TextChunk(chunkSize=800, chunkOverlap=100)
}
```

→ 123 chunks spanning pages 16–45 of the 10-K.

### `Regex`

`Regex("^PERFORMANCE BY (BUSINESS SEGMENT|GEOGRAPHIC AREA)")` — Rust [`regex`](https://docs.rs/regex) syntax, matched against element text (`is_match`, score 1.0, document order). The pattern is compiled at template compile time; an invalid pattern is a compile error quoting it. **(ran** — 28 chunks starting at the segment-performance heading.**)**

### `Heuristic` — layout/style comparisons

`Heuristic(comparison, comparison, …)` — all comparisons are **AND**ed per element; operators `> >= < <= == !=`.

Supported properties: `fontSize`/`font_size`, `fontName`/`font_name`, `page`/`page_number`, `x0`/`x`/`x_position`, `y0`/`y`/`y_position`, `x1`, `y1`, `textLength`/`text_length`, `text`. An unknown property is a compile error listing this set. String-valued properties (`text`, `fontName`) allow only `==`/`!=`; font names compare case-insensitively on canonicalized names.

**(ran)** — large type on the cover pages:

```text
Match<Section> bigHeading {
  Heuristic(fontSize >= 20, page <= 3)
}

Section(match=bigHeading, as="front") {
  TextChunk(chunkSize=400, chunkOverlap=0)
}
```

→ matches the cover's "UNITED STATES" masthead.

### `EmbeddingSim` — semantic similarity *(experimental)*

`EmbeddingSim("query text", threshold=0.7, endpoint="…", model="…")` — embeds the query and the candidate elements (one batch), matches where cosine similarity ≥ `threshold` (default **0.7**), ranked by similarity. `Cosine(…)` and `Semantic(…)` parse as aliases.

It requires an embedding backend, selected by the **caller**, not the template: pass `--embed-endpoint <name-or-url>` or set `DELVER_EMBED_ENDPOINT` (the template's `endpoint=` is recorded and echoed in errors, but per-config endpoint selection is future work). The bundled backend (`crates/delver-embed`) speaks the Databricks model-serving HTTP API — endpoint name or full URL, `DATABRICKS_HOST`/`DATABRICKS_TOKEN` env — and a deterministic mock backs the test suite. No endpoint configured fails loud, **(ran)**:

```text
Error: match 'riskFactors': template uses EmbeddingSim("risk factors and uncertainties") but no
embedder is configured; pass --embed-endpoint <name-or-url> or set DELVER_EMBED_ENDPOINT (D-006: no silent skip)
```

(exit code 1, nothing on stdout).

### `FirstMatch` — ordered alternatives

`FirstMatch(rule, rule, …)` — alternatives tried in order; the first that produces matches wins. Arguments must be nested match functions.

**(ran)**:

```text
Match<Section> flexible {
  FirstMatch(
    Text("PERFORMANCE BY BUSINESS SEGMENT", threshold=0.9),
    Regex("^PERFORMANCE BY")
  )
}
```

### `Optional`

Parses but is **not implemented** — using it is a hard error on master.

---

## Typed tables: `TYPE … AS TABLE`

Declare a record schema, then attach it to a `Table` selector with `type=`:

```text
TYPE Ledger AS TABLE ( account TEXT, year INT, amount DECIMAL );

Table(as="ledger", type="Ledger")
```

Keywords are uppercase; the declaration ends with `;`. Field types are **`TEXT`**, **`INT`**, **`DECIMAL`** — anything else is a compile error listing that set, as are duplicate `TYPE` names and duplicate field names.

**Column mapping** (per table):

1. **Filler columns are skipped** — columns whose body cells are all empty or contain only `$ % ( )` (the interleaved `$` columns of SEC-style filings).
2. **Header matching** — when a header row was detected, fields claim columns in declared order by fuzzy header match (normalized: lowercase alphanumerics; similarity ≥ 0.8, ties go leftmost). `y2015` matches a `2015` header.
3. **Positional fallback** — unmatched fields take the remaining non-filler columns left to right; headerless tables map fully positionally.
4. A field with no column left is `null` in every record and adds one table-level `errors` entry; extra columns are ignored.

**Cell coercion**: `TEXT` is verbatim. For `INT`/`DECIMAL`: a trailing `%` is stripped (and recorded in output `metadata.percent_cells`), surrounding parentheses mean negative, then `$`, commas, and whitespace are removed, and the rest must parse as an integer/decimal. An **empty cell is `null` and counts as ok**. A cell that still doesn't parse is a *data* problem, not a template error: the field becomes `null`, and the record gains an entry in the table-level `errors` array — `{row, col, raw, reason}` in grid coordinates — while the run continues. `coerced_ok + coerced_err` always equals records × mapped fields.

Output shape:

```json
{
  "type": "TypedTable",
  "type_name": "Ledger",
  "name": "ledger",
  "records": [{"account": "Cash", "year": 2015, "amount": 1234.0}],
  "errors": [],
  "coerced_ok": 3,
  "coerced_err": 0,
  "provenance": {"element_id": "…", "page": 1, "bbox": […], "source_rows": [1]},
  "metadata": {…}, "parent_name": …, "parent_index": …
}
```

`provenance.source_rows[i]` is record *i*'s grid row in the detected table, and `element_id` is the stored table element — so every record traces back to cells you can inspect (e.g. in the viewer).

**(ran)** — see the [README teaser](../README.md#a-taste-of-docql-typed-tables) for the full real-document example, including a genuine coercion error: SEC filings write nil as an em dash (`—`), which coerces to `null` + `{"row": 4, "col": 4, "raw": "—", "reason": "cannot parse \"—\" as DECIMAL"}` while the other 35 cells extract cleanly.

---

## SubCorpus & template interpolation

`SubCorpus` declares a named description constant; `TextChunk(template=…)` interpolates it into chunk text (useful for prefixing chunks with corpus context before embedding/RAG ingestion):

```text
SubCorpus(description="3M 2015 annual report (10-K)", as="mmm_10k")

TextChunk(chunkSize=1500, chunkOverlap=0, template="[{mmm_10k}] {text}")
```

**(ran)** — every chunk comes out as `"[3M 2015 annual report (10-K)] Table of Contents …"`.

Rules:

- `{name}` substitutes the SubCorpus description at **compile** time; `{text}` substitutes the chunk's text at **output** time (all occurrences). A template without `{text}` is allowed (constant text).
- Declarations are top-level only, need both `description=` and `as=`, take no body, and may not duplicate names — violations are compile errors.
- An unknown `{var}` is a compile error listing the known variables; an unterminated `{` is a compile error; there is no escaping (a literal `{` always opens a placeholder).
- `template=` is only valid on `TextChunk` (compile error elsewhere). Chunk metadata (char counts, pages) keeps describing the *source* chunk.

---

## Partitions and `--where`

Documents can carry partition key/values (stored on the document, not in the template language):

```bash
delver index ~/datasets/3M_2015_10K.pdf --corpus demo --partition company=3M --partition year=2015
# → {"corpus":"demo","created":false,"document_id":"838c2f8a-…","element_count":26657,
#    "partitions":{"company":"3M","year":"2015"}}
```

- `--partition key=value` is repeatable. Hive-style `key=value` **directory** components of the input path are auto-inferred (`/filings/company=3M/year=2015/x.pdf` → both pairs; the filename never counts); explicit flags win on conflicts.
- Re-indexing identical bytes is a no-op for content but **replaces the whole partitions object** — the last `delver index` wins, so you can (re)tag existing documents.
- `search` and `query --corpus` accept repeatable `--where key=value`; documents must match **all** pairs. `--where` conflicts with `--doc`/`--pdf`.

**(ran)**:

```bash
delver search "net sales" --corpus demo --where year=2016 --limit 2
```

## Multi-document corpus queries

Run one template across every (matching) document of a corpus:

```bash
delver query --corpus demo --where company=3M --template annots.tmpl
```

**(ran)** — the output is a single JSON **object keyed by document id** (ascending), each value being exactly the array that `--doc` would produce for that document:

```json
{
  "838c2f8a-12b9-48ba-8b1a-bd511689b6e3": [ …79 outputs… ],
  "e00811da-9949-4547-a186-2980548a75cc": [ …84 outputs… ]
}
```

No matching documents → `{}` with exit 0 (an empty result is data, not an error).

---

## Output object reference

| `type` | Produced by | Key fields |
|---|---|---|
| `Text` | `TextChunk` / `Paragraph` / a `Section`'s direct content | `text`, `chunk_index`, `metadata.page_numbers`, `metadata.section` |
| `Table` | `Table(as=…)` | `header`, `rows`, `cells`, `n_rows`, `n_cols`, `strategy`, `confidence`, `page`, `bbox` |
| `TypedTable` | `Table(type=…)` | `type_name`, `records`, `errors`, `coerced_ok/err`, `provenance` |
| `Annotation` | `Annotation(as=…)` | `text`, `metadata.subtype/uri/dest`, `page_number`, `bbox` |
| `Figure` | `Figure(as=…)` | `caption`, `image_id`, `caption_id`, `page_number`, `bbox` |
| `Image` | `Image(…)` | `id`, `page_number`, `bbox`, `caption`, `bytes_base64`, `summary` |

All outputs carry `metadata` (inherited section labels + their own) and `parent_name`/`parent_index` linking to the enclosing section's output position.

## Fail-loud errors

DocQL's contract is **no silent no-ops** ([DECISIONS.md](DECISIONS.md) D-006): template *misuse* fails at compile time — unknown match functions (error lists `Text, Regex, EmbeddingSim (aliases: Cosine, Semantic), Heuristic, FirstMatch`), unknown heuristic properties, invalid regexes, undefined `TYPE`/match-definition references, misplaced attributes (`type=` off `Table`, `template=` off `TextChunk`), duplicate definitions, unknown interpolation variables. Failures that can only surface at match time (e.g. a missing embedder) abort the run: non-zero exit, message on stderr, nothing on stdout. *Data* problems (uncoercible cells) never abort — they are reported in-band in the output (`errors`, `coerced_err`).

One deliberate exception: `Table(model=…, targetSchema=…)` warns and proceeds, because those attributes are output enrichment, not selection semantics (D-018).
