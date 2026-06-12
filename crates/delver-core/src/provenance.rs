//! Provenance sidecar for template runs (D-025, viewer slice V6).
//!
//! The serialized outputs array (D-012/D-013 contract) carries page metadata
//! but never the *source element ids* that produced each output — and it must
//! stay byte-identical, so those ids cannot be added to it. Collation,
//! however, knows exactly which elements it assembled each output from. This
//! module is the additive side channel: [`RunProvenance`] is a per-output
//! array, index-aligned with the outputs array (including the D-018
//! tail-deferral of table outputs), carrying source element ids, source
//! pages, a document-order sort key, and — for outputs produced under a
//! matched `Section` — the section's name and full page span.
//!
//! Surfaces: `process_parsed_with_provenance` /
//! `run_template_on_doc_with_provenance` return it alongside the unchanged
//! outputs string; every pre-existing function keeps its exact payload. The
//! first consumer is the viewer's Ctrl+F-style results mode (DV-018).

use serde::{Deserialize, Serialize};

/// Section attribution of one output: the section's display name (the
/// `as="..."` value, falling back to the `match=` reference and then the
/// template element name — mirroring the `section` metadata key the outputs
/// themselves carry) plus the 1-based page span of the section's *entire*
/// matched range (not just the slice that fed this output).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SectionSpan {
    pub name: String,
    pub page_start: u32,
    pub page_end: u32,
}

/// Where `outputs[i]` came from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputProvenance {
    /// Source element ids (stringified UUIDs) in document order. These are
    /// the same ids the store persists, so hydrated runs (`query --doc`,
    /// the viewer) can join them straight back to stored elements.
    pub element_ids: Vec<String>,
    /// 1-based pages covered by the source elements, ascending, deduplicated.
    pub pages: Vec<u32>,
    /// Global document-order index of the first source element — the
    /// within-page sort key for "matches ordered by page, then position"
    /// (table outputs are tail-deferred in the outputs array, so array
    /// order is not document order).
    pub order: u32,
    /// Set when the output was produced under a matched `Section`.
    pub section: Option<SectionSpan>,
}

/// The per-run sidecar: `outputs[i]` describes the run's `outputs[i]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunProvenance {
    pub outputs: Vec<OutputProvenance>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_round_trips_through_json() {
        let prov = RunProvenance {
            outputs: vec![OutputProvenance {
                element_ids: vec!["a".into(), "b".into()],
                pages: vec![16, 17],
                order: 42,
                section: Some(SectionSpan {
                    name: "mda".into(),
                    page_start: 16,
                    page_end: 29,
                }),
            }],
        };
        let json = serde_json::to_string(&prov).expect("serialize");
        let back: RunProvenance = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, prov);
    }
}
