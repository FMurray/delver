//! Near-miss observability for match execution (D-024).
//!
//! A match definition that yields zero candidates above its threshold used to
//! vanish silently: the section simply did not match and the run printed `[]`
//! with no explanation. The matcher now records one [`MatchMiss`] per missed
//! match config — naming the match block (D-006 spirit) and carrying the
//! top-3 closest fuzzy-text candidates — into a [`RunDiagnostics`] that rides
//! alongside the run result. Stdout payloads never change; the CLI prints
//! [`MatchMiss::to_warning`] lines on stderr, and library callers
//! (`process_parsed_with_diagnostics` / `process_pdf_with_diagnostics`) get
//! the struct so other surfaces (e.g. the viewer) can render it later.

use serde::{Deserialize, Serialize};

/// Maximum characters of candidate text echoed in a near miss.
pub const NEAR_MISS_EXCERPT_CHARS: usize = 80;

/// Number of closest candidates reported per missed match.
pub const NEAR_MISS_TOP_K: usize = 3;

/// One below-threshold candidate for a missed match: where the engine looked,
/// how close it got.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NearMiss {
    /// Candidate element text, truncated to [`NEAR_MISS_EXCERPT_CHARS`]
    /// chars (ellipsis included when truncated).
    pub text: String,
    /// The candidate's fuzzy-match score against the pattern.
    pub score: f64,
    /// Page the candidate sits on.
    pub page: u32,
}

impl NearMiss {
    /// Build a near miss, applying the excerpt cap.
    pub fn new(text: &str, score: f64, page: u32) -> Self {
        let mut excerpt: String = text.chars().take(NEAR_MISS_EXCERPT_CHARS - 1).collect();
        if excerpt.chars().count() < text.chars().count() {
            excerpt.push('…');
        }
        NearMiss {
            text: excerpt,
            score,
            page,
        }
    }
}

/// A match config that produced zero candidates above its threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchMiss {
    /// Match-definition name when the config came from one (`match=M`),
    /// otherwise the owning template element's name.
    pub match_name: String,
    /// The fuzzy-text pattern that missed (first `Text(...)` clause for
    /// `FirstMatch` definitions; empty for matchers with no text pattern).
    pub pattern: String,
    pub threshold: f64,
    /// Closest candidates in the searched scope, best first (top
    /// [`NEAR_MISS_TOP_K`]). Empty when the matcher has no fuzzy-text clause
    /// to rank against (Regex/Heuristic/EmbeddingSim) or the scope held no
    /// text at all.
    pub near_misses: Vec<NearMiss>,
}

impl MatchMiss {
    /// Human-readable one-line warning, e.g.
    /// `match 'M' matched nothing at threshold 0.6 — closest: 'Item 7.
    /// Management's Discussion and…' (0.26, p16); …`.
    pub fn to_warning(&self) -> String {
        let mut line = format!(
            "match '{}' matched nothing at threshold {}",
            self.match_name, self.threshold
        );
        if self.near_misses.is_empty() {
            line.push_str(" — no fuzzy-text candidates in scope");
        } else {
            let closest: Vec<String> = self
                .near_misses
                .iter()
                .map(|m| format!("'{}' ({:.2}, p{})", m.text, m.score, m.page))
                .collect();
            line.push_str(" — closest: ");
            line.push_str(&closest.join("; "));
        }
        line
    }
}

/// Diagnostics accumulated over one template run. Additive surface: runs that
/// match everything produce an empty value and zero stderr (the D-017/D-018
/// quiet-by-default contract holds).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunDiagnostics {
    /// Match configs that yielded zero candidates, in encounter order.
    pub match_misses: Vec<MatchMiss>,
}

impl RunDiagnostics {
    pub fn is_empty(&self) -> bool {
        self.match_misses.is_empty()
    }

    /// Record a miss, deduplicating identical (name, pattern, threshold)
    /// repeats (the same definition can be attempted under several parents).
    pub fn record(&mut self, miss: MatchMiss) {
        let duplicate = self.match_misses.iter().any(|m| {
            m.match_name == miss.match_name
                && m.pattern == miss.pattern
                && m.threshold == miss.threshold
        });
        if !duplicate {
            self.match_misses.push(miss);
        }
    }

    /// One warning line per miss, ready for stderr.
    pub fn warnings(&self) -> Vec<String> {
        self.match_misses.iter().map(MatchMiss::to_warning).collect()
    }
}
