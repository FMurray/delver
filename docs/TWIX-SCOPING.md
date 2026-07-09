# TWIX for Delver — scoping note (2026-07-09)

Source: *Benchmarking Extraction of Structured Data from Templatized
Documents*, Hasan (Parameswaran/Cheung), UCB EECS-2025-77, which summarizes
the TWIX system (Lin et al., arXiv:2501.06659). Read in full for this note —
including, memorably, by delver itself (see "What dogfooding taught us").

## What TWIX actually does

Premise: enterprise corpora are full of **templatized documents** — invoices,
forms, reports generated programmatically from a hidden template. Instead of
extracting each page independently (per-page LLM/API calls), infer the
template ONCE from the collection, then extract everywhere by alignment.

Four-stage pipeline:

1. **Phrase extraction** — any OCR/parser that yields phrases + bounding
   boxes; phrases grouped into rows by horizontal alignment.
2. **Field prediction** — the core insight: build a *location vector* per
   phrase (its positional indices across documents/records). Phrases that
   recur at *constant relative offsets* across records are template **fields**
   (keys/headers); irregular phrases are values. Clustering rules: perfect
   match (equal-length vectors, constant offset Δ), partial perfect match
   (subsequence), singletons → values. A minimalist LLM prompt ("which of
   these phrases are keys?") disambiguates clusters that position alone
   can't; a dominance graph picks the winning clusters.
3. **Template assembly** — assign each row a label in {header, key, value,
   blank} via an ILP maximizing assignment probability under visual-alignment
   constraints (NP-hard; pruned to feasibility). Produces a **template tree**
   of key-value blocks and tables. Assembly is corrective: misclassified
   fields get relabeled when structure contradicts them.
4. **Extraction** — zero LLM calls: split documents into records (smallest
   row-span visiting all template nodes in pre-order), split records into
   blocks, pair keys/values sequentially (two adjacent fields ⇒ NULL value),
   align table rows vertically under headers. Output: nested JSON (or flat
   CSV) with type-tagged blocks.

Headline results: ~90% precision/recall; **+25% F1** over Textract, Azure DI,
GPT-4V, Evaporate; after template inference, extraction is **520× faster and
~3,700× cheaper** than vision-LLM baselines ($0.014 vs ~$54 per 2,000 pages).
Stated limits: needs template repetition; struggles on corrupted/missing
text and one-off templates.

## Why this fits delver unusually well

TWIX's paradigm is delver's paradigm: **structure from statistical
regularity, computed deterministically over an index, with LLMs as a last
resort.** We already have most of the substrate:

| TWIX needs | Delver has |
|---|---|
| Phrases + bboxes | `elements` (text runs with bbox, font, page, order) — see prerequisite TW0 below |
| Row grouping | layout line/block machinery |
| "Documents sharing a template" grouping | **corpora + hive partitions** (`--partition vendor=X`) — SubCorpus from the original design doc, already the unit the eval uses |
| Cross-document analysis surface | the Postgres store — location vectors are a windowed SQL query over `elements` across a partition |
| Key/value LLM disambiguation | the `Embedder` HTTP pattern generalizes to a `Completer` trait (env-gated, one cheap prompt class) |
| Typed output machinery | `TYPE … AS TABLE`, `TypedTableOutput`, `table_cells`, provenance sidecar |
| Record/block priors | **B6's planned outline detection** (see corollary) |

## The corollary to B6 (owner's instinct, confirmed)

They are the same move at two scopes:

- **B6**: cluster *within* one document by **style signature** → headings
  emerge → outline.
- **TWIX**: cluster *across* documents by **positional recurrence** →
  fields emerge → template.

Both replace "match what the user typed" / "call a model per page" with
"read the structure out of the index's statistics." They should share a
`structure` analysis module (row grouping, cluster analytics, confidence
gating), and they feed each other: B6's section boundaries constrain TWIX's
record/block separation; TWIX's field clusters corroborate B6's heading
tiers. Recommendation: build TW1 and B6 against the same substrate rather
than sequentially independent.

## What dogfooding taught us (prerequisite discovered)

Indexing the TWIX paper itself with delver surfaced two things:

1. **NUL-byte ingest failure** — fixed (store-boundary sanitization,
   `fix/nul-ingest` @ 7656e49).
2. **Word-gap loss on LaTeX-generated PDFs**: the parser concatenates text
   runs without inferring word boundaries from glyph x-advances
   ("locationvector", "4.1TWIXSystemArchitecture"). Fuzzy matching survives
   this; FTS and — critically — **TWIX's phrase-recurrence clustering would
   not** (location vectors need consistent phrase tokenization).
   → **TW0 below is a hard prerequisite**, and the TWIX pdf is its
   regression fixture.

## Proposed phases

- **TW0 — word-gap inference in run assembly** (delver-core, prerequisite).
  Insert word boundaries when glyph/run x-advance exceeds a threshold
  relative to font size. ⚠️ Deliberately baseline-migrating (output text
  changes everywhere): follow the B3 procedure — capture before/after,
  assert only-improvements on reference docs, mint new baseline constants.
  Effort M.
- **TW1 — recurrence clustering** (delver-store analysis + small core).
  Location-vector construction over a corpus partition (SQL), perfect /
  partial-match clustering, singleton separation; `Completer` trait + the
  key/value verification prompt (env-gated, mockable). Evidence gate:
  clusters for one Treasury Bulletin year-partition and one invoice-like
  set, printed with confidence. Effort M.
- **TW2 — template assembly.** Row labeling {header,key,value,blank}
  honoring visual alignment: start greedy/DP with the paper's pruning
  ideas; exact ILP later if quality demands (solver crates are installable
  now via the crates proxy). Template tree persisted (new table or corpus
  metadata). The hard phase. Effort L.
- **TW3 — alignment extraction.** Record/block separation, kv pairing,
  table alignment → typed outputs with provenance, reusing TYPE coercion.
  Evidence gate: extract a full year-partition of Treasury tables and diff
  against the ai_parse_document-derived `table_cells` ground truth already in the store — a free
  accuracy harness. Effort M.
- **TW4 — surface.** `delver infer-template --corpus X [--where …]`,
  `delver extract --template <id>`, DocQL `Corpus(template=…)` hook, viewer
  template inspector. Effort S.

## Where it pays off first

- **OfficeQA answer-layer bottleneck** (our retrieval evaluation finding): Treasury
  Bulletins are an extreme templatized corpus — the same tables recur for
  decades. Per-era inferred templates → aligned extraction (scanned era via
  the ai_parse_document outputs, which carry coordinates) attacks exactly the
  failure mode that capped every arm at 5–15% accuracy.
- **The native-vs-ai_parse_document table head-to-head** gains a third contender:
  TWIX-aligned extraction over the same 108 dual-parsed docs.
- **The pitch**: "upload 50 invoices, delver infers the template, every
  future invoice extracts for free" — the exact enterprise story TWIX
  benchmarks at $0.014/2,000 pages.

## Risks / open questions

- **Template drift across decades** (Treasury formats evolve): cluster eras
  first (a meta-clustering step; B6-style signals can seed it).
- **OCR noise vs verbatim recurrence** on the scanned era: the paper's
  partial matches help; we additionally have quote-folding + Levenshtein —
  fuzzy phrase-equality for vector construction is likely needed.
- **ILP scale**: paper proves NP-hardness; pruning + greedy first, measure
  before reaching for a solver.
- **Repetition assumption**: fine for our corpora; one-off documents stay
  on the existing match-rule path (TWIX is additive, not a replacement).
