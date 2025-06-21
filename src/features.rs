use crate::parse::{PageContent, TextElement};
use crate::search_index::PdfIndex;
use ordered_float::NotNan;
use strsim::normalized_levenshtein;

/// Represents a vector of features for a single TextElement or TextLine
#[derive(Debug, Clone)]
pub struct TextFeatures {
    pub text: String,
    pub is_all_caps: bool,
    pub is_title_case: bool,
    pub font_size: f32,
    pub font_z_score: f32,
    pub font_freq_rank: usize,
    pub ref_count: u32,
    pub position_percentile_y: f32,
}

impl TextFeatures {
    pub fn from_text_element(elem: &TextElement, index: &PdfIndex) -> Option<Self> {
        let text = elem.text.clone();
        let is_all_caps = text.chars().all(|c| !c.is_alphabetic() || c.is_uppercase());
        let is_title_case = text
            .split_whitespace()
            .filter(|w| w.chars().next().map(|c| c.is_uppercase()).unwrap_or(false))
            .count()
            > 0;

        let font_name = elem.font_name.as_deref().unwrap_or("").to_string();
        let font_size = elem.font_size;

        let font_z_score = {
            let mean = index.font_size_stats.mean;
            let std_dev = index.font_size_stats.std_dev;
            if std_dev > 0.0 {
                (font_size - mean) / std_dev
            } else {
                0.0
            }
        };

        let font_freq_rank = index
            .font_name_frequency_index
            .iter()
            .position(|(_, name)| name == &font_name)
            .unwrap_or(index.font_name_frequency_index.len());

        let ref_count = index
            .element_id_to_index
            .get(&elem.id)
            .and_then(|&i| index.reference_count_index.get(i))
            .map(|(count, _)| *count)
            .unwrap_or(0);

        // Page-relative Y position percentile
        let position_percentile_y = {
            let page_height = 792.0; // Default for US Letter; could be passed in
            1.0 - (elem.bbox.1.min(elem.bbox.3) / page_height).clamp(0.0, 1.0)
        };

        Some(Self {
            text,
            is_all_caps,
            is_title_case,
            font_size,
            font_z_score,
            font_freq_rank,
            ref_count,
            position_percentile_y,
        })
    }
}

/// Compute a composite similarity score between two `TextFeatures`
/// Range: 0.0 (no similarity) .. 1.0 (identical under this metric)
pub fn compute_similarity(a: &TextFeatures, b: &TextFeatures) -> f32 {
    // 1. Textual similarity (normalized Levenshtein)
    // let text_sim = normalized_levenshtein(&a.text.to_lowercase(), &b.text.to_lowercase()) as f32;

    // return text_sim;
    // 2. Font size similarity – closer z‑scores → higher similarity
    let font_sim = 1.0 - (a.font_z_score - b.font_z_score).abs().min(1.0);

    // 3. Capitalisation match
    let caps_sim = if a.is_all_caps == b.is_all_caps {
        1.0
    } else {
        0.0
    };

    // 4. Vertical position similarity (same page percentile)
    let pos_sim = 1.0
        - (a.position_percentile_y - b.position_percentile_y)
            .abs()
            .min(1.0);

    return font_sim + caps_sim + pos_sim;

    // Weighted sum – tweak weights as needed
    // 0.5 * text_sim + 0.2 * font_sim + 0.2 * caps_sim + 0.1 * pos_sim
}
