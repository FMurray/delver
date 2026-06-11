use crate::embed::{EmbedError, Embedder};
use crate::parse::TextElement;
use tokenizers::Tokenizer;

/// `TextChunk(method=...)` strategy selector (Stage B slice 4, D-020).
/// `Tokens` is the pre-slice behavior — token budget when a tokenizer is
/// configured, character budget otherwise — and stays the default when the
/// attribute is absent. `Semantic` is embedding-driven valley splitting via
/// [`chunk_semantic`]. Unknown values are a template-compile error (D-006).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkMethod {
    Tokens,
    Semantic,
}

impl ChunkMethod {
    /// Supported attribute values, for fail-loud error messages (D-006).
    pub const SUPPORTED: &'static str = "\"tokens\" (default), \"semantic\"";

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tokens" => Some(Self::Tokens),
            "semantic" => Some(Self::Semantic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ChunkingStrategy {
    Characters {
        max_chars: usize,
    },
    Tokens {
        max_tokens: usize,
        chunk_overlap: usize,
        tokenizer: Tokenizer,
    },
}

impl Default for ChunkingStrategy {
    fn default() -> Self {
        ChunkingStrategy::Characters { max_chars: 1000 }
    }
}

pub fn chunk_text_elements<'a>(
    text_elements: &'a [TextElement],
    strategy: &ChunkingStrategy,
    chunk_overlap: usize,
) -> Vec<&'a [TextElement]> {
    match strategy {
        ChunkingStrategy::Characters { max_chars } => {
            chunk_by_characters(text_elements, *max_chars, chunk_overlap)
        }
        ChunkingStrategy::Tokens {
            max_tokens,
            chunk_overlap,
            tokenizer,
        } => chunk_by_tokens(text_elements, *max_tokens, *chunk_overlap, tokenizer),
    }
}

fn chunk_by_characters<'a>(
    text_elements: &'a [TextElement],
    char_limit: usize,
    chunk_overlap: usize,
) -> Vec<&'a [TextElement]> {
    let mut chunks = Vec::new();
    let mut start_idx = 0;

    while start_idx < text_elements.len() {
        let mut current_length = 0;
        let mut end_idx = start_idx;

        while end_idx < text_elements.len() {
            let element_len = text_elements[end_idx].text.len();
            if current_length > 0 && current_length + element_len > char_limit {
                break;
            }
            current_length += element_len;
            end_idx += 1;
        }

        if end_idx == start_idx && start_idx < text_elements.len() {
            end_idx = start_idx + 1;
        }

        chunks.push(&text_elements[start_idx..end_idx]);

        if end_idx == text_elements.len() {
            break;
        }

        let mut new_start_idx = end_idx;
        let mut overlap_chars = 0;
        while new_start_idx > start_idx && overlap_chars < chunk_overlap {
            new_start_idx -= 1;
            overlap_chars += text_elements[new_start_idx].text.len();
        }

        if new_start_idx > start_idx {
            start_idx = new_start_idx;
        } else {
            start_idx = end_idx;
        }
    }

    chunks
}

fn chunk_by_tokens<'a>(
    text_elements: &'a [TextElement],
    token_limit: usize,
    chunk_overlap: usize,
    tokenizer: &Tokenizer,
) -> Vec<&'a [TextElement]> {
    let mut chunks = Vec::new();

    if text_elements.is_empty() {
        return chunks;
    }

    // Pre-compute token counts for all elements using batch encoding for efficiency
    let token_counts = element_token_counts(text_elements, tokenizer);

    let mut start_idx = 0;

    while start_idx < text_elements.len() {
        let mut current_tokens = 0;
        let mut end_idx = start_idx;

        // Find how many elements we can include within token_limit
        while end_idx < text_elements.len() && current_tokens < token_limit {
            current_tokens += token_counts[end_idx];
            if current_tokens <= token_limit {
                end_idx += 1;
            }
        }

        // Always include at least one element even if it exceeds token_limit
        if end_idx == start_idx && start_idx < text_elements.len() {
            end_idx = start_idx + 1;
        }

        chunks.push(&text_elements[start_idx..end_idx]);

        if end_idx == text_elements.len() {
            break;
        }

        // Calculate overlap based on tokens
        let mut new_start_idx = end_idx;
        let mut overlap_tokens = 0;
        while new_start_idx > start_idx && overlap_tokens < chunk_overlap {
            new_start_idx -= 1;
            overlap_tokens += token_counts[new_start_idx];
        }

        if new_start_idx > start_idx {
            start_idx = new_start_idx;
        } else {
            start_idx = end_idx;
        }
    }

    chunks
}

/// Per-element token counts via one batch encode, falling back to individual
/// encoding if the batch call fails. Shared by the Tokens and Semantic
/// strategies so both meter the same budget.
fn element_token_counts(text_elements: &[TextElement], tokenizer: &Tokenizer) -> Vec<usize> {
    let texts: Vec<&str> = text_elements.iter().map(|e| e.text.as_str()).collect();
    match tokenizer.encode_batch(texts, false) {
        Ok(encodings) => encodings.iter().map(|e| e.get_ids().len()).collect(),
        Err(_) => text_elements
            .iter()
            .map(|e| {
                tokenizer
                    .encode(e.text.as_str(), false)
                    .map(|enc| enc.get_ids().len())
                    .unwrap_or(0)
            })
            .collect(),
    }
}

/// One semantic chunk: a contiguous slice of the input elements plus the
/// number of sentence-ish segments it contains (overlap-carried segments
/// included). Produced by [`chunk_semantic`].
#[derive(Debug, Clone)]
pub struct SemanticChunk<'a> {
    pub elements: &'a [TextElement],
    pub segment_count: usize,
}

/// `TextChunk(method="semantic")` (Stage B slice 4, D-020). Deterministic
/// given a deterministic embedder. The exact rules:
///
/// **Segmentation (sentence-ish units).** Elements are walked in document
/// order and accumulated into the current segment; the segment closes after
/// any element whose text [`ends_sentence`]: trailing ASCII whitespace is
/// trimmed, trailing closing delimiters (`"`, `'`, `”`, `’`, `)`, `]`) are
/// stripped, and the remainder must end with `.`, `!`, or `?`. The final
/// segment closes at end of input unconditionally. Sentence boundaries
/// strictly inside one element's text are not split candidates — chunks stay
/// contiguous element slices, exactly like the other strategies.
///
/// **Embedding.** Segment text is the element texts joined with a single
/// space (the same joiner chunk-text assembly uses). All segments are
/// embedded in one batch call; a vector-count mismatch is an error.
///
/// **Valleys (percentile rule).** For boundaries between adjacent segments,
/// similarity `s[i] = cosine(v[i], v[i+1])`. The breakpoint threshold is the
/// `breakpoint_percentile`-th percentile of all boundary similarities
/// (ascending sort, index `P * (len-1) / 100` in integer arithmetic); a
/// boundary is a breakpoint iff its similarity is *strictly* below that
/// threshold. Consequences: all-equal similarities (homogeneous text)
/// produce no breakpoints, and `P = 0` disables valley splitting (the
/// budget cap alone splits).
///
/// **Budget (`chunkSize`).** Per-chunk budget is token count when a
/// tokenizer is configured (same batch-encode path as the Tokens strategy),
/// else character count (sum of element text lengths; joiner spaces are not
/// counted — the Characters strategy's accounting). Enforced at segment
/// granularity: a segment that would overflow a non-empty chunk closes the
/// chunk first; a single segment larger than the whole budget still forms
/// its own over-budget chunk (the Tokens strategy's single-element overflow
/// rule, one level up).
///
/// **Overlap (`chunkOverlap`).** On every close except the last, the next
/// chunk starts with the closed chunk's trailing whole segments: step back
/// while the carried cost is below `chunk_overlap` (the segment that crosses
/// the limit is included — mirroring the Tokens strategy's element rule),
/// never back to the closed chunk's own start. Each similarity valley closes
/// at most one chunk: a watermark advances past a consumed valley so the
/// overlap-carried tail does not immediately re-split at the same boundary.
pub fn chunk_semantic<'a>(
    text_elements: &'a [TextElement],
    embedder: &dyn Embedder,
    budget: usize,
    chunk_overlap: usize,
    breakpoint_percentile: u8,
    tokenizer: Option<&Tokenizer>,
) -> Result<Vec<SemanticChunk<'a>>, EmbedError> {
    if text_elements.is_empty() {
        return Ok(Vec::new());
    }

    let costs: Vec<usize> = match tokenizer {
        Some(tokenizer) => element_token_counts(text_elements, tokenizer),
        None => text_elements.iter().map(|e| e.text.len()).collect(),
    };

    // Segment k spans elements [segment_ends[k-1] (or 0), segment_ends[k]).
    let mut segment_ends: Vec<usize> = Vec::new();
    for (i, element) in text_elements.iter().enumerate() {
        if ends_sentence(&element.text) || i + 1 == text_elements.len() {
            segment_ends.push(i + 1);
        }
    }
    let n_seg = segment_ends.len();
    let seg_start = |k: usize| if k == 0 { 0 } else { segment_ends[k - 1] };
    let seg_costs: Vec<usize> = (0..n_seg)
        .map(|k| costs[seg_start(k)..segment_ends[k]].iter().sum())
        .collect();

    let seg_texts: Vec<String> = (0..n_seg)
        .map(|k| {
            text_elements[seg_start(k)..segment_ends[k]]
                .iter()
                .map(|e| e.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    let seg_text_refs: Vec<&str> = seg_texts.iter().map(String::as_str).collect();
    let vectors = embedder.embed(&seg_text_refs)?;
    if vectors.len() != n_seg {
        return Err(EmbedError(format!(
            "embedder returned {} vectors for {} segments",
            vectors.len(),
            n_seg
        )));
    }

    let mut is_breakpoint = vec![false; n_seg.saturating_sub(1)];
    if n_seg >= 2 {
        let mut sims = Vec::with_capacity(n_seg - 1);
        for i in 0..n_seg - 1 {
            let sim = crate::matcher::cosine_similarity(&vectors[i], &vectors[i + 1])
                .ok_or_else(|| {
                    EmbedError(format!(
                        "embedding dimension mismatch between segments {} ({}) and {} ({})",
                        i,
                        vectors[i].len(),
                        i + 1,
                        vectors[i + 1].len()
                    ))
                })?;
            sims.push(sim);
        }
        let mut sorted = sims.clone();
        sorted.sort_by(f64::total_cmp);
        let threshold = sorted[(breakpoint_percentile as usize) * (sorted.len() - 1) / 100];
        for (flag, sim) in is_breakpoint.iter_mut().zip(&sims) {
            *flag = *sim < threshold;
        }
    }

    let mut chunks = Vec::new();
    let mut start_seg = 0usize;
    // Boundaries below this index have already closed a chunk; the overlap
    // tail must not re-split at them.
    let mut next_breakable = 0usize;
    loop {
        let mut cost = 0usize;
        let mut end_seg = start_seg;
        while end_seg < n_seg {
            if end_seg > start_seg && cost + seg_costs[end_seg] > budget {
                break; // budget close (a chunk always keeps >= 1 segment)
            }
            cost += seg_costs[end_seg];
            end_seg += 1;
            if end_seg < n_seg && end_seg - 1 >= next_breakable && is_breakpoint[end_seg - 1] {
                next_breakable = end_seg;
                break; // valley close at the boundary just crossed
            }
        }

        chunks.push(SemanticChunk {
            elements: &text_elements[seg_start(start_seg)..segment_ends[end_seg - 1]],
            segment_count: end_seg - start_seg,
        });

        if end_seg == n_seg {
            break;
        }

        let mut new_start = end_seg;
        let mut overlap_cost = 0usize;
        while new_start > start_seg && overlap_cost < chunk_overlap {
            new_start -= 1;
            overlap_cost += seg_costs[new_start];
        }
        start_seg = if new_start > start_seg { new_start } else { end_seg };
    }

    Ok(chunks)
}

/// Sentence-ish terminator rule (D-020): trim trailing ASCII whitespace,
/// strip trailing closing delimiters (`"`, `'`, `”`, `’`, `)`, `]`), then the
/// element ends a sentence iff the remainder ends with `.`, `!`, or `?`.
/// Abbreviation handling is deliberately out of scope: an element ending in
/// "U.S." closes a segment — a deterministic approximation, not a parser.
fn ends_sentence(text: &str) -> bool {
    text.trim_end()
        .trim_end_matches(['"', '\'', '\u{201D}', '\u{2019}', ')', ']'])
        .ends_with(['.', '!', '?'])
}
