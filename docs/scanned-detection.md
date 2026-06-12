# Scanned-PDF Detection

Slice P1 (branch `feat/aiparse-backend`); decisions in `docs/DECISIONS-aiparse.md`
(DA-002…DA-004). Implementation: `crates/delver-core/src/scan.rs` plus signal
capture in `crates/delver-core/src/parse.rs`.

**Scope (owner decision):** classification is computed **per page** but acted on
at **document level** — a document is parsed by exactly one engine per
`parse_version`; page-level engine mixing is out of scope. The per-page records
persist anyway (under `documents.metadata.scan.pages`) so a future page-level
router needs no re-parse.

---

## 1. Research summary: how practitioners tell scans from born-digital PDFs

A scanned PDF is structurally a *photo album*: one raster image per page,
usually with no text operators at all — unless an OCR pass added a hidden text
layer on top. A born-digital PDF draws its content as text and vector
operators. Every practical detector composes some subset of the signals below.

### Signal catalog

| # | Signal | Strength | Implemented |
|---|--------|----------|-------------|
| 1 | Full-page image XObject coverage | strong, with known false positives | yes |
| 2 | Text-operator/character scarcity | strong in combination | yes |
| 3 | Invisible OCR text layer (`3 Tr`) | very strong (distinguishes `scanned_ocr`) | yes |
| 4 | Producer/Creator fingerprints | medium; great tiebreak | yes (informational + one escalation rule) |
| 5 | Scan-typical image encodings | medium; informational | recorded, not a decision input |
| 6 | Absence of embedded fonts | weak | evaluated, **not** implemented |
| 7 | PDF/A derivation markers | weak/orthogonal | evaluated, **not** implemented |
| 8 | Single-image-page consistency across the doc | strong at doc level | yes (via per-page classes + ratio) |

#### 1. Full-page image coverage (the owner's hypothesis)

The standard first-order test: a page whose drawn images cover (nearly) the
whole page area is image-dominated. Docling's pipeline gates OCR on exactly
this measure with `bitmap_area_threshold` **default 0.75**, and its
maintainers describe the rule as "no text cells but large bitmap coverage ⇒
almost certainly scanned" ([docling discussion #2755]). We adopt 0.75 as the
coverage threshold (constant `COVERAGE_FULL_PAGE`).

**What contradicts the bare hypothesis** (and why coverage alone is not the
rule):

* **Born-digital marketing/brochure PDFs** draw a full-bleed background photo
  *plus* a real, visible text layer. Coverage ≈ 1.0, yet the text is perfectly
  extractable and OCR would be wrong. → the `mixed` page class exists for
  exactly this shape (coverage ≥ 0.75 *and* ≥ 50 visible chars).
* **Vector-heavy pages (CAD exports, charts-only pages)** have ~0 image
  coverage and ~0 text — they are *not* scans (the content is vector
  operators). Coverage-only rules that treat "no text" as "scanned" misfire
  here; we classify them `native`.
* **Scans the content-stream walk cannot see**: pages drawn via *inline
  images* (`BI…ID…EI`) or images wrapped in *Form XObjects* have real raster
  content but zero *visible-to-us* XObject draws. Pure coverage misses them →
  the producer-fingerprint escalation rule (signal 4) is the backstop.
* **Hybrid re-scans** (a native doc printed, scanned, and merged with native
  pages) defeat any single-page rule — hence the per-page classification and
  the `mixed` *document* verdict.

#### 2. Text scarcity

Image-only scans show zero text-showing operators (`Tj/TJ/'/"`). But real
scans frequently carry a *little* text: Bates stamps, scanner-added page
numbers, fax headers. We therefore use a sparse-text floor
(`SPARSE_TEXT_CHARS = 50` visible characters) rather than "zero text", so a
stamp does not flip a scan to `mixed`. The "calculate the ratio of images to
text" framing is the common practitioner recipe ([Open Preservation
Foundation], [imagetotext.online]).

#### 3. Invisible OCR text layers — "scanned WITH OCR" is its own class

OCR pipelines (Acrobat *Searchable Image*, ABBYY, Tesseract/ocrmypdf) write
the recognized text **invisibly** over the page image using text rendering
mode 3 (`3 Tr`, "neither fill nor stroke" — PDF 32000-1 §9.3.6). Acrobat's
Searchable Image output is exactly Tr 3 text ([acrobatusers Q&A]); Tesseract
goes further and uses a *GlyphLessFont* so the layer cannot render even if the
mode is flipped ([PyMuPDF discussion #3537]). OCRmyPDF's `--redo-ocr` is built
on the same distinction: it "categorizes text as either visible or invisible;
invisible text (OCR) is stripped out" ([OCRmyPDF advanced docs]).

This is the highest-value signal in the set because it splits the scanned
world into the two classes that matter operationally:

* `scanned_no_text` — pixels only; **no extraction is possible natively**.
* `scanned_ocr` — pixels + machine text of *unknown quality* (the OCR engine
  and its error profile are unknown; a re-parse through a modern engine like
  `ai_parse_document` may still be warranted).

We count shown characters by rendering mode during the existing walk:
`invisible_text_ratio = invisible / (visible + invisible)`. A page is
OCR-layered when imagery is substantial (coverage ≥ 0.5) and invisible text
dominates (ratio ≥ 0.5; real OCR layers measure ≈ 1.0). Invisible text
*without* imagery is **not** scan evidence (accessibility/watermark tricks) —
classified `native`.

#### 4. Producer/Creator fingerprints

Scanner hardware and OCR software identify themselves in the Info dict:
"ABBYY FineReader", "Paper Capture", "tesseract"/"ocrmypdf", "Epson Scan",
"Xerox WorkCentre", Canon "iR-ADV", Toshiba "e-STUDIO", … The Open
Preservation Foundation recipe leans on exactly these metadata fingerprints
([Open Preservation Foundation]). Failure modes are obvious — metadata is
optional, lies, and survives re-saves ("Microsoft Word" documents scanned to
PDF keep nothing; files laundered through editors keep the *editor's*
producer) — so fingerprints are:

1. recorded informationally (`producer`, `creator`, `producer_hint` in the
   scan block), and
2. used in exactly **one** decision: a document with **zero text anywhere**
   and **no recognized imagery** (the inline-image / Form-XObject blind spot
   of signal 1) escalates `native → scanned_no_text` at confidence 0.5 when
   the producer matches a scanner/OCR fingerprint.

#### 5. Scan-typical image encodings

Bilevel fax/scan codecs — `CCITTFaxDecode` (G3/G4) and `JBIG2Decode` — are
essentially exclusive to scan pipelines ([GdPicture JBIG2]; JBIG2's lossy
mode is infamous from the 2013 Xerox WorkCentre digit-substitution incident).
Full-page `DCTDecode`/`JPXDecode` images are also typical of color scans, but
those filters are equally common in born-digital photography, so filters are
**recorded per page** (`image_filters`) and surfaced in the receipt — useful
evidence for humans and future rules — without being a decision input in v1.

#### 6. Absence of embedded fonts — evaluated, not implemented

"No embedded fonts ⇒ no text ⇒ scan" sounds appealing but double-counts
signal 2 (no text already says that) and *misfires on the OCR case*: OCR
layers DO embed a font (Tesseract's GlyphLessFont). Old born-digital PDFs
using only the standard-14 fonts also embed nothing. Weak and confounded —
skipped.

#### 7. PDF/A markers — evaluated, not implemented

Archival conversions (common for scan workflows) leave XMP `pdfaid` markers,
and some shops treat PDF/A-derivation as a scan hint. But PDF/A is an archival
*profile*, applied to born-digital documents at least as often. Orthogonal to
scan-ness, requires XMP parsing we don't otherwise need — skipped.

#### 8. Cross-page consistency

Real scans are *uniform*: every page is one image of the same shape. Rather
than a bespoke uniformity metric, the per-page classification plus the
document aggregate captures this: `scanned_page_ratio ≥ 0.8 ⇒ scanned`,
`≤ 0.2 ⇒ native`, otherwise `mixed`. The 0.8/0.2 band tolerates inserted
cover/signature pages on either side.

---

## 2. Decision rules (as implemented)

Constants live in `delver_core::scan` and are the single source of truth;
this table is documentation, not a second copy to maintain.

| Constant | Value | Source/rationale |
|----------|-------|------------------|
| `COVERAGE_FULL_PAGE` | 0.75 | docling `bitmap_area_threshold` default |
| `COVERAGE_PARTIAL` | 0.5 | imagery floor for the OCR rule |
| `SPARSE_TEXT_CHARS` | 50 | Bates stamps / scanner page numbers stay under it |
| `INVISIBLE_DOMINANT_RATIO` | 0.5 | real OCR layers measure ≈ 1.0; lenient on visible stamps |
| doc scanned / native bands | ≥ 0.8 / ≤ 0.2 | tolerate inserted pages |

**Per page** (first match wins):

1. coverage ≥ 0.5 **and** text present **and** invisible ratio ≥ 0.5 → `scanned_ocr`
2. coverage ≥ 0.75 **and** visible chars < 50 → `scanned_no_text`
3. coverage ≥ 0.75 (visible text ≥ 50) → `mixed`
4. otherwise → `native` (incl. blank and vector-only pages)

Coverage is the union of drawn image-XObject envelopes (the CTM image of the
unit square, PDF 32000-1 §8.9.5.2) over a 64×64 occupancy grid — deterministic
and overlap-safe. Character counts come from the glyph decode the parser
already performs, split by the current `Tr` mode.

**Per document** (synthetic page 0 excluded):

* `scanned_page_ratio` = scanned\_\* pages / pages.
* ratio ≥ 0.8 → the dominant scanned flavor (`scanned_ocr` wins ties);
  confidence = ratio.
* ratio ≤ 0.2 and mixed pages < half → `native` (confidence = native-page
  share) — **unless** the document has zero text everywhere and a
  scanner/OCR producer fingerprint, which escalates to `scanned_no_text` at
  confidence 0.5 (the inline-image blind-spot rule).
* otherwise → `mixed`; confidence counts the pages that make it mixed
  (mixed pages + matched scanned/native pairs).

**Persistence.** `parse_document` attaches the block to
`ParsedDocument.metadata` under `"scan"`; ingest persists it in
`documents.metadata` (jsonb, no migration), `load_document` returns it, and
the `delver index` receipt echoes it:

```json
"scan": {
  "class": "scanned_no_text",
  "scanned_page_ratio": 1.0,
  "confidence": 1.0,
  "pages": [
    {"page": 1, "class": "scanned_no_text", "image_coverage": 1.0,
     "image_count": 1, "text_chars": 0, "invisible_text_ratio": 0.0,
     "image_filters": ["CCITTFaxDecode"]}
  ]
}
```

### Known limitations (v1, by design)

* **Inline images** (`BI…ID…EI`) and images drawn **inside Form XObjects**
  are not walked (a pre-existing parser scope limit), so their coverage is
  invisible; the producer escalation is the only backstop. Same blind spot
  applies to text inside Form XObjects.
* A glyph run that *changes* `Tr` mid-show-op is attributed to the mode at
  each show operator, not per glyph — fine in practice (OCR layers set
  `3 Tr` once per page).
* The legacy `ImageElement.bbox` is a known-incorrect placeholder (see
  DA-002); scan coverage deliberately computes its own envelope and does not
  inherit it.

---

## 3. Expressing scan-ness in DocQL

**Today (v1, implemented):** the document scan class is a metadata filter on
every corpus-scoped operation, sharing the partition `--where` mechanism
(jsonb containment on `documents.metadata`):

```bash
delver search "revenue" --corpus filings --where scan.class=scanned_no_text
delver query  --corpus filings --template 10k.tmpl --where scan.class=native
delver query  --corpus filings --template 10k.tmpl \
    --where state=CA --where scan.class=scanned_ocr   # AND-composes with partitions
```

`scan.class` accepts exactly `native | scanned_no_text | scanned_ocr | mixed`;
any other value or any other `scan.*` key is a hard error listing the
supported set (D-006).

**Future grammar hook (documented, NOT implemented this slice):** a
document-level predicate in the template itself, e.g.

```
Corpus(where = scan.class == "scanned_ocr") { … }     # corpus refinement
Document(scan_class="native") { Section(...) { … } }  # per-document guard
```

The natural seam is a `Document`/`Corpus`-level attribute compiled to the same
jsonb containment filter `documents_matching` already takes — no executor
changes, only grammar + compile. Deferred until the DocQL grammar grows
corpus-level constructs (Stage C SubCorpus is the adjacent precedent).

---

## References

* docling maintainers on scanned-vs-native detection and
  `bitmap_area_threshold` (0.75): <https://github.com/docling-project/docling/discussions/2755>
* Open Preservation Foundation, "Scanned vs native PDFs, how to differentiate
  them?": <https://openpreservation.org/blogs/scanned-vs-native-pdfs-how-to-differentiate-them/>
* PyMuPDF discussion — OCR text layers, `3 Tr`, Tesseract GlyphLessFont:
  <https://github.com/pymupdf/PyMuPDF/discussions/3537>
* OCRmyPDF advanced features — visible/invisible text categorization,
  `--skip-text`, `PriorOcrFoundError`:
  <https://ocrmypdf.readthedocs.io/en/latest/advanced.html>
* Acrobat Searchable Image emits invisible (mode 3) text:
  <https://answers.acrobatusers.com/HAVE-YOU-FINALLY-FIXED-OCR-SO-IT-CAN-BE-PERFORMED-ON-RENDERABLE-TEXT-q11834.aspx>
* "Scanned PDF vs Native PDF" practitioner overview:
  <https://imagetotext.online/insights/scanned-pdf-vs-native-pdf>
* USPTO 10,489,644 / 11,232,300 — image-layer-only vs image+text-layer
  document models for OCR detection:
  <https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/10489644>
* JBIG2/CCITT as scan codecs (and the Xerox WorkCentre lossy-JBIG2 incident):
  <https://www.gdpicture.com/formats-sdk/jbig2/>
* PDF 32000-1:2008 — §9.3.6 (text rendering modes), §8.9.5.2 (image unit
  square placement).
