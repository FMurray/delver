use crate::parse::TextElement;
use tokenizers::Tokenizer;

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

        // Find how many elements we can include within char_limit
        while end_idx < text_elements.len() && current_length < char_limit {
            current_length += text_elements[end_idx].text.len();
            if current_length <= char_limit {
                end_idx += 1;
            }
        }

        // Always include at least one element even if it exceeds char_limit
        if end_idx == start_idx && start_idx < text_elements.len() {
            end_idx = start_idx + 1;
        }

        chunks.push(&text_elements[start_idx..end_idx]);

        if end_idx == text_elements.len() {
            break;
        }

        // Calculate overlap based on characters
        let mut new_start_idx = end_idx;
        let mut overlap_chars = 0;
        while new_start_idx > start_idx && overlap_chars < chunk_overlap {
            new_start_idx -= 1;
            overlap_chars += text_elements[new_start_idx].text.len();
        }

        start_idx = new_start_idx;
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
    let texts: Vec<&str> = text_elements.iter().map(|e| e.text.as_str()).collect();
    let token_counts: Vec<usize> = match tokenizer.encode_batch(texts, false) {
        Ok(encodings) => encodings.iter().map(|e| e.get_ids().len()).collect(),
        Err(_) => {
            // Fallback to individual encoding if batch fails
            text_elements
                .iter()
                .map(|e| {
                    tokenizer
                        .encode(e.text.as_str(), false)
                        .map(|enc| enc.get_ids().len())
                        .unwrap_or(0)
                })
                .collect()
        }
    };

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

        start_idx = new_start_idx;
    }

    chunks
}
