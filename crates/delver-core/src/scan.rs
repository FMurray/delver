//! Scanned-PDF detection (slice P1; docs/scanned-detection.md,
//! docs/DECISIONS-aiparse.md).
//!
//! Classification is computed at parse time from signals the content-stream
//! walk already produces — image XObject placements (CTM-transformed unit
//! squares), shown-glyph counts split by text rendering mode (`Tr 3` =
//! invisible, the OCR-text-layer convention), and image stream filters — and
//! lands in `ParsedDocument.metadata` under the `"scan"` key. Hydration never
//! re-runs it (the block persists in `documents.metadata`).
//!
//! Classes:
//! * `native`           — born-digital page/document.
//! * `scanned_no_text`  — page image(s) cover the page and there is (almost)
//!                        no extractable text: a raw scan.
//! * `scanned_ocr`      — page image plus a dominant *invisible* text layer:
//!                        a scan that went through OCR (Acrobat "Searchable
//!                        Image", ocrmypdf, ABBYY, tesseract all emit `3 Tr`).
//! * `mixed`            — full-bleed imagery *and* substantial visible text
//!                        (page level), or a document whose pages disagree.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::parse::PageContents;

// ───────────────────────────── thresholds ─────────────────────────────
// Rationale and references in docs/scanned-detection.md.

/// Image coverage at/above which a page counts as image-dominated. Matches
/// docling's `bitmap_area_threshold` default (0.75).
pub const COVERAGE_FULL_PAGE: f32 = 0.75;
/// Coverage floor for the OCR-layer rule: invisible text only indicates a
/// scan when it sits over substantial imagery.
pub const COVERAGE_PARTIAL: f32 = 0.5;
/// Fewer visible characters than this is "textually empty" — page numbers,
/// scanner stamps, and Bates numbering stay under it.
pub const SPARSE_TEXT_CHARS: u64 = 50;
/// Share of invisible (`Tr 3`) characters at/above which the text layer is
/// considered an OCR layer.
pub const INVISIBLE_DOMINANT_RATIO: f32 = 0.5;
/// Document verdict: at/above this share of scanned pages the document is
/// scanned; at/below `1 - this` it is native; in between it is mixed.
const DOC_SCANNED_MIN_RATIO: f32 = 0.8;
const DOC_NATIVE_MAX_RATIO: f32 = 0.2;
/// Occupancy-grid resolution per axis for the image-coverage union
/// (64×64 cells ⇒ coverage is exact to ~1.6% per axis, deterministic, and
/// overlap-safe — no inclusion–exclusion needed).
const COVERAGE_GRID: usize = 64;
/// Cap on captured image rects per page (defensive; coverage of pathological
/// pages saturates long before this).
const MAX_IMAGE_RECTS: usize = 4096;

/// Producer/Creator substrings (lowercase) that fingerprint scanner hardware
/// or OCR software. Used (a) informationally in the scan block and (b) to
/// escalate a zero-text document to `scanned_no_text` when the walk saw no
/// imagery it understands (inline images / Form-XObject-wrapped scans).
const SCANNER_PRODUCER_FINGERPRINTS: &[&str] = &[
    "scan", // covers "Scanner", "Epson Scan", "HP Scan", "ScanSnap", "NAPS2 (scan...)"
    "abbyy",
    "finereader",
    "tesseract",
    "ocrmypdf",
    "paper capture", // Acrobat's OCR pipeline
    "paperport",
    "omnipage",
    "readiris",
    "kofax",
    "capturesto",
    "xerox workcentre",
    "ricoh",
    "ir-adv", // Canon imageRUNNER ADVANCE
    "km_", // Konica Minolta bizhub job names
    "e-studio", // Toshiba
];

// ───────────────────────────── classes ─────────────────────────────

/// Page- and document-level scan classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanClass {
    Native,
    ScannedNoText,
    ScannedOcr,
    Mixed,
}

impl ScanClass {
    /// Every legal class string, for fail-loud error messages.
    pub const ALL: [&'static str; 4] = ["native", "scanned_no_text", "scanned_ocr", "mixed"];

    pub fn as_str(self) -> &'static str {
        match self {
            ScanClass::Native => "native",
            ScanClass::ScannedNoText => "scanned_no_text",
            ScanClass::ScannedOcr => "scanned_ocr",
            ScanClass::Mixed => "mixed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "native" => Some(ScanClass::Native),
            "scanned_no_text" => Some(ScanClass::ScannedNoText),
            "scanned_ocr" => Some(ScanClass::ScannedOcr),
            "mixed" => Some(ScanClass::Mixed),
            _ => None,
        }
    }

    /// True for the classes that mean "this content exists only as pixels"
    /// (`auto` engine routing keys off this).
    pub fn is_scanned(self) -> bool {
        matches!(self, ScanClass::ScannedNoText | ScanClass::ScannedOcr)
    }
}

impl std::fmt::Display for ScanClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ───────────────────────────── raw signals ─────────────────────────────

/// Raw per-page signals accumulated during the content-stream walk
/// (transient, like `cell_fragments`: empty on hydrated pages, never
/// persisted per page — the classified summary persists in document
/// metadata instead).
#[derive(Debug, Clone, Default)]
pub struct PageScanSignals {
    /// Page MediaBox `(x0, y0, x1, y1)` in PDF units.
    pub page_box: (f32, f32, f32, f32),
    /// Device-space envelopes of drawn image XObjects (CTM-transformed unit
    /// squares; same coordinate space as `page_box`, flip-invariant for
    /// coverage purposes).
    pub image_rects: Vec<(f32, f32, f32, f32)>,
    /// Number of image XObject draws (also counts rects past the capture cap).
    pub image_count: u32,
    /// Characters shown with a visible text rendering mode (Tr != 3).
    pub visible_chars: u64,
    /// Characters shown invisibly (`Tr 3` — the OCR-layer convention).
    pub invisible_chars: u64,
    /// Distinct stream filter names of drawn images (`DCTDecode`,
    /// `CCITTFaxDecode`, `JBIG2Decode`, `JPXDecode`, `FlateDecode`, ...).
    pub image_filters: BTreeSet<String>,
}

impl PageScanSignals {
    /// Record one image XObject draw with its device-space envelope and
    /// stream filter names.
    pub fn record_image(
        &mut self,
        rect: (f32, f32, f32, f32),
        filters: impl IntoIterator<Item = String>,
    ) {
        self.image_count += 1;
        if self.image_rects.len() < MAX_IMAGE_RECTS {
            self.image_rects.push(rect);
        }
        self.image_filters.extend(filters);
    }

    /// Record `count` glyphs shown under text rendering mode `render_mode`.
    pub fn record_glyphs(&mut self, count: u64, render_mode: u8) {
        if render_mode == 3 {
            self.invisible_chars += count;
        } else {
            self.visible_chars += count;
        }
    }

    /// Fraction of the page area covered by the union of image rects,
    /// computed on a 64×64 occupancy grid (deterministic; overlapping rects
    /// never double-count). 0.0 when the page box is degenerate.
    pub fn image_coverage(&self) -> f32 {
        let (px0, py0, px1, py1) = self.page_box;
        let (w, h) = (px1 - px0, py1 - py0);
        if w <= 0.0 || h <= 0.0 || self.image_rects.is_empty() {
            return 0.0;
        }
        let n = COVERAGE_GRID;
        let mut rows = [0u64; COVERAGE_GRID];
        for &(x0, y0, x1, y1) in &self.image_rects {
            let cx0 = (((x0.min(x1) - px0) / w) * n as f32).floor().clamp(0.0, n as f32) as usize;
            let cx1 = (((x0.max(x1) - px0) / w) * n as f32).ceil().clamp(0.0, n as f32) as usize;
            let cy0 = (((y0.min(y1) - py0) / h) * n as f32).floor().clamp(0.0, n as f32) as usize;
            let cy1 = (((y0.max(y1) - py0) / h) * n as f32).ceil().clamp(0.0, n as f32) as usize;
            if cx1 <= cx0 || cy1 <= cy0 {
                continue;
            }
            let width = cx1 - cx0;
            let mask: u64 = if width >= 64 {
                u64::MAX
            } else {
                ((1u64 << width) - 1) << cx0
            };
            for row in &mut rows[cy0..cy1] {
                *row |= mask;
            }
        }
        let covered: u32 = rows.iter().map(|r| r.count_ones()).sum();
        covered as f32 / (n * n) as f32
    }
}

// ───────────────────────────── classified output ─────────────────────────────

/// One page's classification (persisted in the document scan block).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageScan {
    pub page: u32,
    pub class: ScanClass,
    pub image_coverage: f32,
    pub image_count: u32,
    /// Total characters shown (visible + invisible).
    pub text_chars: u64,
    /// invisible / (visible + invisible); 0.0 for textless pages.
    pub invisible_text_ratio: f32,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub image_filters: Vec<String>,
}

/// Document-level aggregate (persisted under `documents.metadata.scan`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentScan {
    pub class: ScanClass,
    /// Share of real pages classed `scanned_no_text` or `scanned_ocr`.
    pub scanned_page_ratio: f32,
    /// Share of pages supporting the verdict (see docs/scanned-detection.md).
    pub confidence: f32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub producer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub creator: Option<String>,
    /// The scanner/OCR fingerprint substring matched in Producer/Creator,
    /// when any (informational, plus the zero-text escalation rule).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub producer_hint: Option<String>,
    pub pages: Vec<PageScan>,
}

/// Classify one page from its raw walk signals.
///
/// Decision order (first match wins; rationale in docs/scanned-detection.md):
/// 1. invisible-dominant text over substantial imagery → `scanned_ocr`
/// 2. image-dominated page with (almost) no visible text → `scanned_no_text`
/// 3. image-dominated page with real visible text → `mixed`
/// 4. everything else (incl. blank and pure-vector pages) → `native`
pub fn classify_page(page: u32, signals: &PageScanSignals) -> PageScan {
    let coverage = signals.image_coverage();
    let total = signals.visible_chars + signals.invisible_chars;
    let invisible_ratio = if total == 0 {
        0.0
    } else {
        signals.invisible_chars as f32 / total as f32
    };

    let class = if coverage >= COVERAGE_PARTIAL && total > 0 && invisible_ratio >= INVISIBLE_DOMINANT_RATIO
    {
        ScanClass::ScannedOcr
    } else if coverage >= COVERAGE_FULL_PAGE && signals.visible_chars < SPARSE_TEXT_CHARS {
        ScanClass::ScannedNoText
    } else if coverage >= COVERAGE_FULL_PAGE {
        ScanClass::Mixed
    } else {
        ScanClass::Native
    };

    PageScan {
        page,
        class,
        image_coverage: coverage,
        image_count: signals.image_count,
        text_chars: total,
        invisible_text_ratio: invisible_ratio,
        image_filters: signals.image_filters.iter().cloned().collect(),
    }
}

/// Classify a whole parsed document: per-page classes plus the aggregate
/// verdict. Synthetic page 0 (document-level blobs) is excluded.
pub fn classify_document(
    pages: &BTreeMap<u32, PageContents>,
    producer: Option<String>,
    creator: Option<String>,
) -> DocumentScan {
    let page_scans: Vec<PageScan> = pages
        .iter()
        .filter(|(page, _)| **page != 0)
        .map(|(page, contents)| classify_page(*page, &contents.scan))
        .collect();
    aggregate(page_scans, producer, creator)
}

/// Aggregate per-page classifications into the document verdict.
pub fn aggregate(
    pages: Vec<PageScan>,
    producer: Option<String>,
    creator: Option<String>,
) -> DocumentScan {
    let n = pages.len();
    let count = |class: ScanClass| pages.iter().filter(|p| p.class == class).count();
    let no_text = count(ScanClass::ScannedNoText);
    let ocr = count(ScanClass::ScannedOcr);
    let native = count(ScanClass::Native);
    let mixed = count(ScanClass::Mixed);
    let scanned = no_text + ocr;
    let ratio = if n == 0 { 0.0 } else { scanned as f32 / n as f32 };
    let producer_hint = scanner_fingerprint(producer.as_deref(), creator.as_deref());

    let (class, confidence) = if n == 0 {
        (ScanClass::Native, 0.0)
    } else if ratio >= DOC_SCANNED_MIN_RATIO {
        // Dominant scanned flavor: OCR wins ties (an OCR layer anywhere
        // usually means the whole doc went through the same pipeline).
        let flavor = if ocr >= no_text {
            ScanClass::ScannedOcr
        } else {
            ScanClass::ScannedNoText
        };
        (flavor, ratio)
    } else if ratio <= DOC_NATIVE_MAX_RATIO && (mixed as f32) < 0.5 * n as f32 {
        let total_text: u64 = pages.iter().map(|p| p.text_chars).sum();
        if total_text == 0 && native == n && producer_hint.is_some() {
            // The walk saw neither text nor image XObjects it understands,
            // but the producer is scanner/OCR software: almost certainly a
            // scan drawn via inline images or Form XObjects (both outside
            // the walk). Low confidence by design.
            (ScanClass::ScannedNoText, 0.5)
        } else {
            (ScanClass::Native, native as f32 / n as f32)
        }
    } else {
        // Genuinely split documents: confidence counts the pages that make
        // it mixed — mixed pages themselves plus matched scanned/native
        // page pairs.
        let conf = (mixed + 2 * scanned.min(native)) as f32 / n as f32;
        (ScanClass::Mixed, conf.clamp(0.0, 1.0))
    };

    DocumentScan {
        class,
        scanned_page_ratio: ratio,
        confidence,
        producer,
        creator,
        producer_hint,
        pages,
    }
}

/// First scanner/OCR fingerprint substring found in Producer/Creator
/// (case-insensitive), if any.
fn scanner_fingerprint(producer: Option<&str>, creator: Option<&str>) -> Option<String> {
    let haystack = format!(
        "{} {}",
        producer.unwrap_or_default().to_lowercase(),
        creator.unwrap_or_default().to_lowercase()
    );
    SCANNER_PRODUCER_FINGERPRINTS
        .iter()
        .find(|fp| haystack.contains(**fp))
        .map(|fp| (*fp).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals(
        page_box: (f32, f32, f32, f32),
        rects: &[(f32, f32, f32, f32)],
        visible: u64,
        invisible: u64,
    ) -> PageScanSignals {
        let mut s = PageScanSignals {
            page_box,
            ..Default::default()
        };
        for rect in rects {
            s.record_image(*rect, ["DCTDecode".to_string()]);
        }
        s.visible_chars = visible;
        s.invisible_chars = invisible;
        s
    }

    const LETTER: (f32, f32, f32, f32) = (0.0, 0.0, 612.0, 792.0);

    #[test]
    fn coverage_full_page_image() {
        let s = signals(LETTER, &[(0.0, 0.0, 612.0, 792.0)], 0, 0);
        assert!((s.image_coverage() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn coverage_unions_overlapping_rects() {
        // Two copies of the same half-page rect must not sum to 1.0.
        let half = (0.0, 0.0, 612.0, 396.0);
        let s = signals(LETTER, &[half, half], 0, 0);
        let coverage = s.image_coverage();
        assert!(
            (0.45..=0.55).contains(&coverage),
            "expected ~0.5 union coverage, got {coverage}"
        );
    }

    #[test]
    fn coverage_empty_or_degenerate() {
        assert_eq!(signals(LETTER, &[], 0, 0).image_coverage(), 0.0);
        let degenerate = signals((0.0, 0.0, 0.0, 0.0), &[(0.0, 0.0, 10.0, 10.0)], 0, 0);
        assert_eq!(degenerate.image_coverage(), 0.0);
    }

    #[test]
    fn page_full_image_no_text_is_scanned_no_text() {
        let s = signals(LETTER, &[(0.0, 0.0, 612.0, 792.0)], 0, 0);
        assert_eq!(classify_page(1, &s).class, ScanClass::ScannedNoText);
    }

    #[test]
    fn page_scanner_stamp_stays_scanned_no_text() {
        // Bates numbers / scanner stamps: a few visible chars under the
        // sparse-text floor must not flip the page to mixed.
        let s = signals(LETTER, &[(0.0, 0.0, 612.0, 792.0)], SPARSE_TEXT_CHARS - 1, 0);
        assert_eq!(classify_page(1, &s).class, ScanClass::ScannedNoText);
    }

    #[test]
    fn page_invisible_text_over_image_is_scanned_ocr() {
        let s = signals(LETTER, &[(0.0, 0.0, 612.0, 792.0)], 0, 1200);
        let scan = classify_page(1, &s);
        assert_eq!(scan.class, ScanClass::ScannedOcr);
        assert!((scan.invisible_text_ratio - 1.0).abs() < 1e-6);
    }

    #[test]
    fn page_ocr_rule_needs_imagery() {
        // Invisible text without imagery (accessibility layers, watermark
        // tricks) is not scan evidence.
        let s = signals(LETTER, &[], 0, 800);
        assert_eq!(classify_page(1, &s).class, ScanClass::Native);
    }

    #[test]
    fn page_text_page_is_native() {
        let s = signals(LETTER, &[], 1800, 0);
        assert_eq!(classify_page(1, &s).class, ScanClass::Native);
    }

    #[test]
    fn page_full_bleed_image_with_real_text_is_mixed() {
        // Born-digital marketing PDFs: full-bleed background image plus a
        // real (visible) text layer — the canonical false positive for the
        // bare image-coverage hypothesis.
        let s = signals(LETTER, &[(0.0, 0.0, 612.0, 792.0)], 900, 0);
        assert_eq!(classify_page(1, &s).class, ScanClass::Mixed);
    }

    #[test]
    fn page_half_page_photo_with_text_is_native() {
        let s = signals(LETTER, &[(0.0, 396.0, 612.0, 792.0)], 1200, 0);
        assert_eq!(classify_page(1, &s).class, ScanClass::Native);
    }

    fn page(class: ScanClass, text_chars: u64) -> PageScan {
        PageScan {
            page: 1,
            class,
            image_coverage: 0.0,
            image_count: 0,
            text_chars,
            invisible_text_ratio: 0.0,
            image_filters: Vec::new(),
        }
    }

    #[test]
    fn doc_all_scanned_pages() {
        let scan = aggregate(
            vec![
                page(ScanClass::ScannedNoText, 0),
                page(ScanClass::ScannedNoText, 0),
                page(ScanClass::ScannedNoText, 0),
            ],
            None,
            None,
        );
        assert_eq!(scan.class, ScanClass::ScannedNoText);
        assert!((scan.scanned_page_ratio - 1.0).abs() < 1e-6);
        assert!((scan.confidence - 1.0).abs() < 1e-6);
    }

    #[test]
    fn doc_ocr_wins_ties_between_scanned_flavors() {
        let scan = aggregate(
            vec![
                page(ScanClass::ScannedOcr, 900),
                page(ScanClass::ScannedNoText, 0),
            ],
            None,
            None,
        );
        assert_eq!(scan.class, ScanClass::ScannedOcr);
    }

    #[test]
    fn doc_native_with_one_scanned_insert_is_native() {
        let mut pages = vec![page(ScanClass::Native, 2000); 9];
        pages.push(page(ScanClass::ScannedNoText, 0));
        let scan = aggregate(pages, None, None);
        assert_eq!(scan.class, ScanClass::Native);
        assert!((scan.scanned_page_ratio - 0.1).abs() < 1e-6);
    }

    #[test]
    fn doc_split_is_mixed_with_high_confidence() {
        let mut pages = vec![page(ScanClass::Native, 2000); 5];
        pages.extend(vec![page(ScanClass::ScannedNoText, 0); 5]);
        let scan = aggregate(pages, None, None);
        assert_eq!(scan.class, ScanClass::Mixed);
        assert!((scan.confidence - 1.0).abs() < 1e-6);
    }

    #[test]
    fn doc_zero_text_with_scanner_producer_escalates() {
        // Inline-image / Form-XObject scans: the walk sees nothing, the
        // producer string is the only evidence.
        let pages = vec![page(ScanClass::Native, 0); 4];
        let scan = aggregate(pages, Some("Canon iR-ADV C5550 PDF".to_string()), None);
        assert_eq!(scan.class, ScanClass::ScannedNoText);
        assert!((scan.confidence - 0.5).abs() < 1e-6);
        assert_eq!(scan.producer_hint.as_deref(), Some("ir-adv"));

        // Same shape with a word-processor producer stays native.
        let pages = vec![page(ScanClass::Native, 0); 4];
        let scan = aggregate(pages, Some("Microsoft Word 2016".to_string()), None);
        assert_eq!(scan.class, ScanClass::Native);
    }

    #[test]
    fn doc_empty_has_zero_confidence() {
        let scan = aggregate(Vec::new(), None, None);
        assert_eq!(scan.class, ScanClass::Native);
        assert_eq!(scan.confidence, 0.0);
    }

    #[test]
    fn class_serde_round_trip_snake_case() {
        for class in [
            ScanClass::Native,
            ScanClass::ScannedNoText,
            ScanClass::ScannedOcr,
            ScanClass::Mixed,
        ] {
            let json = serde_json::to_value(class).unwrap();
            assert_eq!(json, serde_json::json!(class.as_str()));
            assert_eq!(ScanClass::parse(class.as_str()), Some(class));
        }
        assert!(ScanClass::parse("bogus").is_none());
    }
}
