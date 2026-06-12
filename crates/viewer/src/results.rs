//! Results-mode model (slice V6, DV-018): Ctrl+F-style navigation over a
//! run's provenance sidecar (D-025).
//!
//! Everything here is pure data + functions so match ordering, section-span
//! filtering, and highlight-set computation unit-test without a reactive
//! runtime. The reactive shell ([`crate::app::ResultsBus`] + the doc view)
//! holds three signals: the latest [`RunResults`] (client-side, post-run —
//! DV-013: runs never happen during SSR, so this is always `None` at
//! hydration), the current match position, and the section page-filter.

use std::collections::HashSet;
use std::sync::Arc;

use delver_core::diagnostics::MatchMiss;
use delver_core::provenance::{RunProvenance, SectionSpan};

/// One navigable match: an output of the run, addressed by its sidecar entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultMatch {
    /// Index into the run's outputs array (and sidecar).
    pub output_index: usize,
    /// First source page, 1-based store page — the page Prev/Next lands on.
    pub page: u32,
    /// All source pages (ascending; multi-page chunks highlight on each).
    pub pages: Vec<u32>,
    /// Source element ids (store element ids — overlays join on these).
    pub element_ids: Vec<String>,
    /// Document-order index of the first source element (within-page order).
    pub order: u32,
    /// Index into [`RunResults::sections`] when section-attributed.
    pub section: Option<usize>,
}

/// Everything the doc view needs to render results mode for one run.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResults {
    /// The document the run executed against; the bar and highlights render
    /// only while this document is open.
    pub doc_id: String,
    /// Monotonic run identity (the run-request nonce): the doc view re-seeds
    /// the current match exactly when this changes.
    pub run_id: u64,
    /// Matches ordered by (page, document order, output index) — the
    /// outputs array itself is NOT page-ordered (D-018 tail-defers tables).
    pub matches: Vec<ResultMatch>,
    /// Distinct section attributions (chip per entry), ordered by
    /// (page_start, page_end, name).
    pub sections: Vec<SectionSpan>,
    /// Near-miss diagnostics of the run (D-024) — rendered as warnings with
    /// a click-to-jump page reference.
    pub misses: Vec<MatchMiss>,
}

/// Build the navigable results for one finished run.
///
/// Outputs whose sidecar entry has no pages (defensive: nothing to navigate
/// to) are skipped. Sections are deduplicated by value; each match points at
/// its section's index in the deduplicated list.
pub fn build_results(
    doc_id: &str,
    run_id: u64,
    provenance: &RunProvenance,
    misses: Vec<MatchMiss>,
) -> RunResults {
    let mut sections: Vec<SectionSpan> = provenance
        .outputs
        .iter()
        .filter_map(|p| p.section.clone())
        .collect();
    sections.sort_by(|a, b| {
        (a.page_start, a.page_end, &a.name).cmp(&(b.page_start, b.page_end, &b.name))
    });
    sections.dedup();

    let mut matches: Vec<ResultMatch> = provenance
        .outputs
        .iter()
        .enumerate()
        .filter_map(|(output_index, p)| {
            let page = *p.pages.first()?;
            Some(ResultMatch {
                output_index,
                page,
                pages: p.pages.clone(),
                element_ids: p.element_ids.clone(),
                order: p.order,
                section: p
                    .section
                    .as_ref()
                    .and_then(|s| sections.iter().position(|known| known == s)),
            })
        })
        .collect();
    matches.sort_by_key(|m| (m.page, m.order, m.output_index));

    RunResults {
        doc_id: doc_id.to_string(),
        run_id,
        matches,
        sections,
        misses,
    }
}

/// Indices (into `results.matches`) visible under the section page-filter.
///
/// The owner's design: a section chip "filters the whole document to the
/// matching pages" — so the filter is by PAGE SPAN, not by attribution: any
/// match landing on a page inside the selected section's span stays
/// navigable (e.g. a top-level table on one of its pages). `None` = all.
pub fn visible_indices(results: &RunResults, section_filter: Option<usize>) -> Vec<usize> {
    let span = section_filter.and_then(|i| results.sections.get(i));
    results
        .matches
        .iter()
        .enumerate()
        .filter(|(_, m)| match span {
            Some(span) => m.page >= span.page_start && m.page <= span.page_end,
            None => true,
        })
        .map(|(i, _)| i)
        .collect()
}

/// Element ids to highlight on `page`: (every visible match touching that
/// page, the current match when it touches that page). The current match's
/// ids are also present in the first set; the second drives the emphasis
/// style. A multi-page match contributes ALL its element ids — the sidecar
/// is per-output, not per-element — and the page's own element list does
/// the final filtering: ids living on other pages simply never join against
/// the rendered overlays.
pub fn highlight_sets(
    results: &RunResults,
    visible: &[usize],
    current: usize,
    page: u32,
) -> (HashSet<String>, HashSet<String>) {
    let mut all = HashSet::new();
    let mut current_ids = HashSet::new();
    for (pos, &match_idx) in visible.iter().enumerate() {
        let Some(m) = results.matches.get(match_idx) else {
            continue;
        };
        if !m.pages.contains(&page) {
            continue;
        }
        for id in &m.element_ids {
            all.insert(id.clone());
            if pos == current {
                current_ids.insert(id.clone());
            }
        }
    }
    (all, current_ids)
}

/// Where the current-match cursor starts after a run: the first visible
/// match on the page being viewed (running never yanks navigation away —
/// DV-018), else the first match overall.
pub fn initial_current(results: &RunResults, visible: &[usize], page: u32) -> usize {
    visible
        .iter()
        .position(|&i| results.matches.get(i).is_some_and(|m| m.page == page))
        .unwrap_or(0)
}

/// Clamp a current-match position to the visible list (filter changes can
/// shrink it).
pub fn clamp_current(current: usize, visible_len: usize) -> usize {
    if visible_len == 0 {
        0
    } else {
        current.min(visible_len - 1)
    }
}

/// "n of N" positions wrap Ctrl+F-style.
pub fn next_pos(current: usize, visible_len: usize) -> usize {
    if visible_len == 0 {
        0
    } else {
        (current + 1) % visible_len
    }
}

pub fn prev_pos(current: usize, visible_len: usize) -> usize {
    if visible_len == 0 {
        0
    } else {
        (current + visible_len - 1) % visible_len
    }
}

/// Chip label: `md&a · p16–29` (single-page spans collapse to `p16`).
pub fn section_chip_label(span: &SectionSpan) -> String {
    if span.page_start == span.page_end {
        format!("{} · p{}", span.name, span.page_start)
    } else {
        format!("{} · p{}–{}", span.name, span.page_start, span.page_end)
    }
}

/// Page indicator: "page 17 of 16–29" inside a section filter, "page 17 of
/// 158" otherwise. `page` is the 1-based store page.
pub fn page_indicator(page: u32, total_pages: usize, span: Option<&SectionSpan>) -> String {
    match span {
        Some(span) => format!("page {page} of {}–{}", span.page_start, span.page_end),
        None => format!("page {page} of {total_pages}"),
    }
}

/// Results shared through context: `Arc` so signal reads stay cheap at
/// thousands of matches.
pub type SharedResults = Arc<RunResults>;

#[cfg(test)]
mod tests {
    use super::*;
    use delver_core::provenance::OutputProvenance;

    fn prov(
        ids: &[&str],
        pages: &[u32],
        order: u32,
        section: Option<(&str, u32, u32)>,
    ) -> OutputProvenance {
        OutputProvenance {
            element_ids: ids.iter().map(|s| s.to_string()).collect(),
            pages: pages.to_vec(),
            order,
            section: section.map(|(name, page_start, page_end)| SectionSpan {
                name: name.to_string(),
                page_start,
                page_end,
            }),
        }
    }

    /// Outputs arrive in array order: section chunks, a top-level chunk,
    /// then tail-deferred tables (D-018) whose document position is earlier.
    fn fixture() -> RunResults {
        let provenance = RunProvenance {
            outputs: vec![
                prov(&["a", "b"], &[16, 17], 100, Some(("mda", 16, 29))), // 0
                prov(&["c"], &[18], 130, Some(("mda", 16, 29))),          // 1
                prov(&["d"], &[40], 400, None),                           // 2 top-level chunk
                prov(&["t1"], &[17], 110, Some(("mda", 16, 29))),         // 3 deferred table
                prov(&["t2"], &[33], 300, Some(("risk", 30, 33))),        // 4 deferred table
            ],
        };
        build_results("doc-1", 7, &provenance, Vec::new())
    }

    #[test]
    fn matches_are_ordered_by_page_then_document_order() {
        let results = fixture();
        let order: Vec<usize> = results.matches.iter().map(|m| m.output_index).collect();
        // p16(#0) < p17(#3 table, order 110) < p18(#1) < p33(#4) < p40(#2):
        // the deferred table re-interleaves by document position.
        assert_eq!(order, vec![0, 3, 1, 4, 2]);
        assert_eq!(results.run_id, 7);
        assert_eq!(results.doc_id, "doc-1");
    }

    #[test]
    fn sections_dedupe_and_matches_point_at_them() {
        let results = fixture();
        assert_eq!(results.sections.len(), 2);
        assert_eq!(results.sections[0].name, "mda");
        assert_eq!(results.sections[1].name, "risk");
        // All three mda-attributed outputs share section index 0.
        let mda_count = results
            .matches
            .iter()
            .filter(|m| m.section == Some(0))
            .count();
        assert_eq!(mda_count, 3);
        // The top-level chunk has no section.
        assert!(results
            .matches
            .iter()
            .any(|m| m.output_index == 2 && m.section.is_none()));
    }

    #[test]
    fn section_filter_is_by_page_span() {
        let results = fixture();
        let all = visible_indices(&results, None);
        assert_eq!(all.len(), 5);
        // mda span 16–29: outputs 0, 3, 1 (in match order).
        let mda = visible_indices(&results, Some(0));
        let outputs: Vec<usize> = mda
            .iter()
            .map(|&i| results.matches[i].output_index)
            .collect();
        assert_eq!(outputs, vec![0, 3, 1]);
        // risk span 30–33: just the second table.
        let risk = visible_indices(&results, Some(1));
        assert_eq!(risk.len(), 1);
        assert_eq!(results.matches[risk[0]].output_index, 4);
        // Out-of-range filter index falls back to "all".
        assert_eq!(visible_indices(&results, Some(9)).len(), 5);
    }

    #[test]
    fn highlight_sets_split_all_vs_current() {
        let results = fixture();
        let visible = visible_indices(&results, None);
        // Page 17 is touched by match 0 (chunk spanning 16–17: BOTH its ids
        // ride along — the page's element join drops "a", which lives on
        // p16) and the deferred table "t1" (current position 1).
        let (all, current) = highlight_sets(&results, &visible, 1, 17);
        let expect = |ids: &[&str]| -> HashSet<String> {
            ids.iter().map(|s| s.to_string()).collect()
        };
        assert_eq!(all, expect(&["a", "b", "t1"]));
        assert_eq!(current, expect(&["t1"]));
        // Matches NOT touching the page contribute nothing: on p18 only
        // match "c" highlights, and the current match (position 1 = the p17
        // table) leaves the emphasis set empty.
        let (p18, p18_cur) = highlight_sets(&results, &visible, 1, 18);
        assert_eq!(p18, expect(&["c"]));
        assert!(p18_cur.is_empty());
        // A page with no matches highlights nothing.
        let (none, none_cur) = highlight_sets(&results, &visible, 0, 99);
        assert!(none.is_empty() && none_cur.is_empty());
    }

    #[test]
    fn initial_current_prefers_the_open_page() {
        let results = fixture();
        let visible = visible_indices(&results, None);
        // Viewing p18 → the third match (position 2) is on it.
        assert_eq!(initial_current(&results, &visible, 18), 2);
        // Viewing a page with no match → first match overall.
        assert_eq!(initial_current(&results, &visible, 2), 0);
    }

    #[test]
    fn nav_positions_wrap_and_clamp() {
        assert_eq!(next_pos(4, 5), 0);
        assert_eq!(next_pos(1, 5), 2);
        assert_eq!(prev_pos(0, 5), 4);
        assert_eq!(prev_pos(3, 5), 2);
        assert_eq!(next_pos(0, 0), 0);
        assert_eq!(prev_pos(0, 0), 0);
        assert_eq!(clamp_current(9, 3), 2);
        assert_eq!(clamp_current(1, 3), 1);
        assert_eq!(clamp_current(0, 0), 0);
    }

    #[test]
    fn labels_render_spans_and_indicator() {
        let span = SectionSpan {
            name: "mda".into(),
            page_start: 16,
            page_end: 29,
        };
        assert_eq!(section_chip_label(&span), "mda · p16–29");
        let single = SectionSpan {
            name: "cover".into(),
            page_start: 1,
            page_end: 1,
        };
        assert_eq!(section_chip_label(&single), "cover · p1");
        assert_eq!(page_indicator(17, 158, Some(&span)), "page 17 of 16–29");
        assert_eq!(page_indicator(17, 158, None), "page 17 of 158");
    }

    #[test]
    fn outputs_without_pages_are_skipped() {
        let provenance = RunProvenance {
            outputs: vec![prov(&[], &[], 0, None), prov(&["x"], &[3], 5, None)],
        };
        let results = build_results("d", 0, &provenance, Vec::new());
        assert_eq!(results.matches.len(), 1);
        assert_eq!(results.matches[0].output_index, 1);
    }
}
