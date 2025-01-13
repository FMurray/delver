use crate::parse::TextElement;
use tokenizers::Tokenizer;

#[derive(Debug, Clone)]
pub enum ChunkingStrategy {
    Characters {
        max_chars: usize,
    },
    Tokens {
        max_tokens: usize,
        tokenizer: Tokenizer,
    },
}

impl Default for ChunkingStrategy {
    fn default() -> Self {
        ChunkingStrategy::Characters { max_chars: 1000 }
    }
}

pub fn chunk_text_elements(
    text_elements: &[TextElement],
    strategy: &ChunkingStrategy,
    chunk_overlap: usize,
) -> Vec<Vec<TextElement>> {
    match strategy {
        ChunkingStrategy::Characters { max_chars } => {
            chunk_by_characters(text_elements, *max_chars, chunk_overlap)
        }
        ChunkingStrategy::Tokens {
            max_tokens,
            tokenizer,
        } => chunk_by_tokens(text_elements, *max_tokens, chunk_overlap, tokenizer),
    }
}

fn chunk_by_characters(
    text_elements: &[TextElement],
    char_limit: usize,
    chunk_overlap: usize,
) -> Vec<Vec<TextElement>> {
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

        chunks.push(text_elements[start_idx..end_idx].to_vec());

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

fn chunk_by_tokens(
    text_elements: &[TextElement],
    token_limit: usize,
    chunk_overlap: usize,
    tokenizer: &Tokenizer,
) -> Vec<Vec<TextElement>> {
    let mut chunks = Vec::new();
    let mut start_idx = 0;

    while start_idx < text_elements.len() {
        let mut current_tokens = 0;
        let mut end_idx = start_idx;

        // Find how many elements we can include within token_limit
        while end_idx < text_elements.len() && current_tokens < token_limit {
            let encoding = tokenizer
                .encode(text_elements[end_idx].text.as_str(), false)
                .expect("Failed to encode text");
            let num_tokens = encoding.get_ids().len();

            current_tokens += num_tokens;
            if current_tokens <= token_limit {
                end_idx += 1;
            }
        }

        // Always include at least one element even if it exceeds token_limit
        if end_idx == start_idx && start_idx < text_elements.len() {
            end_idx = start_idx + 1;
        }

        chunks.push(text_elements[start_idx..end_idx].to_vec());

        if end_idx == text_elements.len() {
            break;
        }

        // Calculate overlap based on tokens
        let mut new_start_idx = end_idx;
        let mut overlap_tokens = 0;
        while new_start_idx > start_idx && overlap_tokens < chunk_overlap {
            new_start_idx -= 1;
            let encoding = tokenizer
                .encode(text_elements[new_start_idx].text.as_str(), false)
                .expect("Failed to encode text");
            overlap_tokens += encoding.get_ids().len();
        }

        start_idx = new_start_idx;
    }

    chunks
}
