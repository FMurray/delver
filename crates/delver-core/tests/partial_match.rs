//! D-024: substring-aware fuzzy `Text(...)` matching, typographic quote
//! folding, and near-miss diagnostics.
//!
//! The reported bug: `Match<Section> M { Text("Management's Discussion",
//! threshold=0.6) }` returned `[]` with zero stderr against a 10-K whose real
//! heading is "Item 7. Management's Discussion and Analysis of Financial
//! Condition and Results of Operations" — the fragment scored ~0.25 against
//! the whole element text. Per D-009 the test PDF is generated in-test via
//! lopdf (builder copied from crates/delver-core/tests/match_exec.rs by
//! design); quote-folding tests build `PageContents` directly so no PDF
//! string-encoding subtleties are involved.

use std::collections::BTreeMap;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use serde_json::Value;

use delver_core::layout::MatchContext;
use delver_core::parse::{get_page_content, PageContents, TextElement};
use delver_core::search_index::{fold_typographic, TextScorer};
use delver_core::{process_parsed, process_parsed_with_diagnostics};

const FULL_HEADING: &str =
    "Item 7. Management's Discussion and Analysis of Financial Condition and Results of Operations";
const MDA_BODY: &str =
    "Net sales grew across every operating segment while currency headwinds persisted.";
const INTRO_BODY: &str =
    "This report contains statements about future expectations and operating plans.";
const HEADING_2: &str = "Item 8. Financial Statements and Supplementary Data";
const BODY_2: &str = "The consolidated balance sheets reflect total assets of the registrant.";

/// The user's exact failing query, verbatim (ASCII apostrophe, fragment
/// pattern, threshold 0.6).
const USER_FRAGMENT_TEMPLATE: &str = r#"
Match<Section> M {
  Text("Management's Discussion", threshold=0.6)
}

Section(match=M) {
  TextChunk(chunkSize=500, chunkOverlap=150)
}
"#;

fn push_text_ops(ops: &mut Vec<Operation>, text: &str, size: f32, x: f32, y: f32) {
    ops.push(Operation::new("BT", vec![]));
    ops.push(Operation::new("Tf", vec!["F1".into(), size.into()]));
    ops.push(Operation::new("Td", vec![x.into(), y.into()]));
    ops.push(Operation::new("Tj", vec![Object::string_literal(text)]));
    ops.push(Operation::new("ET", vec![]));
}

/// One page: intro body, the full MD&A heading (24pt), MD&A body, a second
/// heading, second body. The heading is a single text element, like the
/// (unsplit) real-world case.
fn build_test_pdf() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut ops = Vec::new();
    push_text_ops(&mut ops, INTRO_BODY, 11.0, 72.0, 740.0);
    push_text_ops(&mut ops, FULL_HEADING, 24.0, 72.0, 700.0);
    push_text_ops(&mut ops, MDA_BODY, 11.0, 72.0, 660.0);
    push_text_ops(&mut ops, HEADING_2, 24.0, 72.0, 620.0);
    push_text_ops(&mut ops, BODY_2, 11.0, 72.0, 580.0);

    let content_id = doc.add_object(Stream::new(
        dictionary! {},
        Content { operations: ops }.encode().expect("encode content"),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serialize pdf to memory");
    bytes
}

fn run_on_test_pdf(template: &str) -> anyhow::Result<String> {
    let doc = Document::load_mem(&build_test_pdf()).expect("load synthetic pdf");
    let pages = get_page_content(&doc).expect("extract page content");
    process_parsed(&pages, &MatchContext::default(), template, None)
}

/// Build a single-page `PageContents` map from raw element texts — used by
/// the quote-folding tests so the exact codepoints are under test control.
fn pages_from_texts(texts: &[&str]) -> BTreeMap<u32, PageContents> {
    let mut page = PageContents::new();
    for (i, text) in texts.iter().enumerate() {
        let mut elem = TextElement::new(text.to_string());
        elem.font_size = 12.0;
        elem.font_name = Some("Helvetica".to_string());
        elem.page_number = 1;
        let y = 700.0 - 20.0 * i as f32;
        elem.bbox = (72.0, y, 500.0, y + 12.0);
        page.add_text(elem);
    }
    let mut pages = BTreeMap::new();
    pages.insert(1, page);
    pages
}

fn outputs(json: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(json)
        .expect("outputs must be JSON")
        .as_array()
        .expect("outputs must be an array")
        .clone()
}

// ── (a) the user's exact fragment query matches the full-title heading ──────

#[test]
fn fragment_pattern_matches_full_title_heading() {
    let json = run_on_test_pdf(USER_FRAGMENT_TEMPLATE).expect("query must succeed");
    let outs = outputs(&json);
    assert!(
        !outs.is_empty(),
        "fragment pattern must match the full-title heading, got []"
    );
    // Every output is attributed to the match definition's section.
    for out in &outs {
        assert_eq!(
            out["metadata"]["section"], "M",
            "outputs must carry the section attribution"
        );
    }
    // The section starts at the heading: its first chunk contains the MD&A
    // body, and the pre-heading intro stays outside.
    let all_text: String = outs
        .iter()
        .filter_map(|o| o["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        all_text.contains(MDA_BODY),
        "section content must include the MD&A body, got: {all_text}"
    );
    assert!(
        !all_text.contains(INTRO_BODY),
        "content before the heading must stay outside the section, got: {all_text}"
    );
}

// ── (b) typographic quote folding, both directions ──────────────────────────

/// ASCII apostrophe in the pattern, U+2019 in the corpus (the reported
/// combination: SEC filings carry U+2019, users type ').
#[test]
fn ascii_quote_pattern_matches_curly_quote_corpus() {
    let pages = pages_from_texts(&[
        "Item 7. Management\u{2019}s Discussion and Analysis of Financial Condition and Results of Operations",
        "Net sales grew across every operating segment.",
    ]);
    let json = process_parsed(&pages, &MatchContext::default(), USER_FRAGMENT_TEMPLATE, None)
        .expect("query must succeed");
    let outs = outputs(&json);
    assert!(
        !outs.is_empty(),
        "ASCII-quote fragment must match the U+2019 heading, got []"
    );
    assert_eq!(outs[0]["metadata"]["section"], "M");
}

/// U+2019 in the pattern, ASCII apostrophe in the corpus (the reverse).
#[test]
fn curly_quote_pattern_matches_ascii_corpus() {
    let template = "
Match<Section> M {
  Text(\"Management\u{2019}s Discussion\", threshold=0.6)
}

Section(match=M) {
  TextChunk(chunkSize=500, chunkOverlap=150)
}
";
    let pages = pages_from_texts(&[
        "Item 7. Management's Discussion and Analysis of Financial Condition and Results of Operations",
        "Net sales grew across every operating segment.",
    ]);
    let json = process_parsed(&pages, &MatchContext::default(), template, None)
        .expect("query must succeed");
    let outs = outputs(&json);
    assert!(
        !outs.is_empty(),
        "U+2019 fragment must match the ASCII-quote heading, got []"
    );
    assert_eq!(outs[0]["metadata"]["section"], "M");
}

/// Folding also rescues near-equal-length comparisons where only the quote
/// codepoint differs: pass 1 (raw, pre-D-024) misses at a strict threshold,
/// the folded rescue pass matches.
#[test]
fn quote_mismatch_rescued_on_near_equal_lengths() {
    let template = "
Match<Section> M {
  Text(\"Management's Discussion and Analysis\", threshold=0.98)
}

Section(match=M) {
  TextChunk(chunkSize=500, chunkOverlap=0)
}
";
    let pages = pages_from_texts(&[
        "Management\u{2019}s Discussion and Analysis",
        "Net sales grew across every operating segment.",
    ]);
    let json = process_parsed(&pages, &MatchContext::default(), template, None)
        .expect("query must succeed");
    assert!(
        !outputs(&json).is_empty(),
        "quote-folded comparison must score 1.0 and clear threshold 0.98"
    );
}

// ── (c) near-miss diagnostics on the library surface ────────────────────────

#[test]
fn zero_match_run_collects_near_miss_diagnostics() {
    let template = r#"
Match<Section> Missing_Section {
  Text("Zebra Hovercraft Manifest", threshold=0.6)
}

Section(match=Missing_Section) {
  TextChunk(chunkSize=500, chunkOverlap=0)
}
"#;
    let doc = Document::load_mem(&build_test_pdf()).expect("load synthetic pdf");
    let pages = get_page_content(&doc).expect("extract page content");
    let (json, diagnostics) =
        process_parsed_with_diagnostics(&pages, &MatchContext::default(), template, None)
            .expect("a non-matching template is a data condition, not an error");

    assert_eq!(outputs(&json).len(), 0, "stdout payload stays pure: []");
    assert_eq!(diagnostics.match_misses.len(), 1);
    let miss = &diagnostics.match_misses[0];
    assert_eq!(miss.match_name, "Missing_Section");
    assert_eq!(miss.pattern, "Zebra Hovercraft Manifest");
    assert!((miss.threshold - 0.6).abs() < 1e-9);
    assert!(
        !miss.near_misses.is_empty() && miss.near_misses.len() <= 3,
        "top-3 near misses expected, got {}",
        miss.near_misses.len()
    );
    for near in &miss.near_misses {
        assert!(near.text.chars().count() <= 80, "excerpt capped at 80 chars");
        assert!(near.score >= 0.0 && near.score < 0.6);
        assert_eq!(near.page, 1);
    }
    // Best first.
    for pair in miss.near_misses.windows(2) {
        assert!(pair[0].score >= pair[1].score);
    }
    // The warning line carries name, a candidate excerpt, and a score.
    let warning = miss.to_warning();
    assert!(warning.contains("match 'Missing_Section' matched nothing at threshold 0.6"));
    assert!(warning.contains(&miss.near_misses[0].text));
    assert!(warning.contains(&format!("{:.2}", miss.near_misses[0].score)));
}

#[test]
fn matching_run_collects_no_diagnostics() {
    let doc = Document::load_mem(&build_test_pdf()).expect("load synthetic pdf");
    let pages = get_page_content(&doc).expect("extract page content");
    let (json, diagnostics) = process_parsed_with_diagnostics(
        &pages,
        &MatchContext::default(),
        USER_FRAGMENT_TEMPLATE,
        None,
    )
    .expect("query must succeed");
    assert!(!outputs(&json).is_empty());
    assert!(
        diagnostics.is_empty(),
        "an all-matching run must stay quiet, got {diagnostics:?}"
    );
}

// ── (d) whole-string scoring is byte-compatible with the pre-D-024 scorer ───

#[test]
fn whole_string_scoring_unchanged_for_near_equal_lengths() {
    let cases = [
        // (pattern, candidate) pairs of near-equal length, including
        // typographic codepoints: pass 1 must score them with NO folding.
        ("Management's Discussion", "Management\u{2019}s Discussion"),
        ("PERFORMANCE BY BUSINESS SEGMENT", "PERFORMANCE BY GEOGRAPHIC AREA"),
        ("Item 7", "Item 7."),
        ("kitten", "sitting"),
        ("Quantitative and Qualitative", "Quantitative or Qualitative"),
        ("", ""),
        ("abc", ""),
    ];
    for (pattern, candidate) in cases {
        let scorer = TextScorer::new(pattern);
        assert_eq!(
            scorer.whole_score(candidate),
            strsim::normalized_levenshtein(pattern, candidate),
            "pass-1 score must equal the pre-D-024 scorer for {pattern:?} vs {candidate:?}"
        );
    }
}

#[test]
fn rescue_score_keeps_whole_string_mode_at_near_equal_lengths() {
    // 2·candidate_chars ≤ 3·pattern_chars stays whole-string even in the
    // rescue pass: an exactly-1.5× candidate gets no containment shortcut.
    let pattern = "abcdef"; // 6 chars
    let at_boundary = "abcdefghi"; // 9 chars: 2*9 == 3*6 → whole-string
    let scorer = TextScorer::new(pattern);
    assert_eq!(
        scorer.rescue_score(at_boundary),
        strsim::normalized_levenshtein(pattern, at_boundary),
        "ratio exactly 1.5 must stay in whole-string mode"
    );
    // One char longer crosses the gate: containment now scores 1.0.
    let past_boundary = "abcdefghij"; // 10 chars: 2*10 > 3*6
    assert_eq!(scorer.rescue_score(past_boundary), 1.0);
}

#[test]
fn rescue_score_windows_fragments_and_shortcuts_containment() {
    let heading = FULL_HEADING; // 94 chars
    let scorer = TextScorer::new("Management's Discussion");
    // Verbatim fragment buried in a long heading: containment shortcut.
    assert_eq!(scorer.rescue_score(heading), 1.0);
    // Near-verbatim fragment (one typo): windowed max stays high while the
    // whole-string score is hopeless.
    let typo_scorer = TextScorer::new("Managament's Discussion");
    let windowed = typo_scorer.rescue_score(heading);
    assert!(
        windowed > 0.85 && windowed < 1.0,
        "one-typo fragment should window-score high, got {windowed}"
    );
    assert!(typo_scorer.whole_score(heading) < 0.3);
    // An empty pattern never takes the containment shortcut.
    let empty = TextScorer::new("");
    assert_eq!(empty.rescue_score(heading), 0.0);
}

#[test]
fn fold_typographic_folds_quotes_and_dashes_only() {
    assert_eq!(
        fold_typographic("\u{2018}a\u{2019} \u{201C}b\u{201D} c\u{2013}d e\u{2014}f"),
        "'a' \"b\" c-d e-f"
    );
    // Allocation-free pass-through when nothing folds.
    assert!(matches!(
        fold_typographic("plain ASCII text"),
        std::borrow::Cow::Borrowed(_)
    ));
    // Case is NOT folded (Text() matching stays case-sensitive).
    assert_eq!(fold_typographic("ABC"), "ABC");
}
