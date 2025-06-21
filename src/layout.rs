use crate::parse::{PageContent, TextElement};
use indexmap::IndexMap;
use lopdf::Object;

use std::collections::BTreeMap;
use std::fmt::Debug;
use uuid::Uuid;

// Add this struct at the module level
#[derive(Debug, Default)]
pub struct MatchContext {
    pub destinations: IndexMap<String, Object>,
}

/// Represents a single line of text on the page after grouping TextElements.
#[derive(Debug, Clone)]
pub struct TextLine {
    pub id: Uuid,
    pub text: String,
    pub page_number: u32,
    pub elements: Vec<TextElement>,
    /// A bounding box for the entire line (x_min, y_min, x_max, y_max).
    pub bbox: (f32, f32, f32, f32),
}

impl TextLine {
    pub fn from_elements(page_number: u32, items: Vec<TextElement>) -> Self {
        let id = Uuid::new_v4();
        let mut line_min_x = f32::MAX;
        let mut line_min_y = f32::MAX;
        let mut line_max_x = f32::MIN;
        let mut line_max_y = f32::MIN;
        let mut combined_text = String::with_capacity(items.iter().map(|e| e.text.len()).sum());

        for (_, it) in items.iter().enumerate() {
            line_min_x = line_min_x.min(it.bbox.0);
            line_max_x = line_max_x.max(it.bbox.2);
            line_min_y = line_min_y.min(it.bbox.1);
            line_max_y = line_max_y.max(it.bbox.3);

            combined_text.push_str(&it.text);
        }

        let line = TextLine {
            id,
            text: combined_text,
            page_number,
            elements: items,
            bbox: (line_min_x, line_min_y, line_max_x, line_max_y),
        };

        tracing::debug!(
            line_id = %line.id,
            parent = %line.id,
            children = %serde_json::to_string(&line.elements.iter().map(|e| e.id).collect::<Vec<_>>()).unwrap(),
            rel_type = "line_to_elements",
            "Created text line with {} elements",
            line.elements.len()
        );

        line
    }

    /// Get all elements in this line
    pub fn elements(&self) -> &[TextElement] {
        &self.elements
    }

    /// Get the first element in this line
    pub fn first_element(&self) -> Option<&TextElement> {
        self.elements.first()
    }

    /// Get the font name for this line (from the first element)
    /// Since lines are now split by font changes, all elements should have the same font
    pub fn font_name(&self) -> Option<&str> {
        self.elements
            .first()
            .and_then(|elem| elem.font_name.as_deref())
    }

    /// Get the font size for this line (from the first element)
    /// Since lines are now split by font changes, all elements should have the same font size
    pub fn font_size(&self) -> f32 {
        self.elements
            .first()
            .map(|elem| elem.font_size)
            .unwrap_or(0.0)
    }

    /// Check if all elements in this line have the same font metadata
    pub fn has_consistent_font(&self) -> bool {
        if self.elements.len() <= 1 {
            return true;
        }

        let first_font_name = &self.elements[0].font_name;
        let first_font_size = self.elements[0].font_size;

        self.elements.iter().skip(1).all(|elem| {
            elem.font_name == *first_font_name && (elem.font_size - first_font_size).abs() < 0.1
        })
    }
}

impl<'a> From<&'a TextLine> for Vec<&'a TextElement> {
    fn from(line: &'a TextLine) -> Self {
        line.elements.iter().collect()
    }
}

// Collection utility for multiple lines
pub fn elements_from_lines<'a>(lines: &[&'a TextLine]) -> Vec<&'a TextElement> {
    lines.iter().flat_map(|line| line.elements.iter()).collect()
}

/// Represents a "block" of consecutive lines that are close in vertical spacing.
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub id: Uuid,
    pub page_number: u32,
    pub lines: Vec<TextLine>,
    /// A bounding box for the entire block (x_min, y_min, x_max, y_max).
    pub bbox: (f32, f32, f32, f32),
}

impl TextBlock {
    pub fn from_lines(page_number: u32, lines: Vec<TextLine>) -> Self {
        let id = Uuid::new_v4();
        let (x_min, y_min, x_max, y_max) = lines.iter().fold(
            (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
            |(xmin, ymin, xmax, ymax), line| {
                (
                    xmin.min(line.bbox.0),
                    ymin.min(line.bbox.1),
                    xmax.max(line.bbox.2),
                    ymax.max(line.bbox.3),
                )
            },
        );

        let block = Self {
            id,
            page_number,
            lines,
            bbox: (x_min, y_min, x_max, y_max),
        };

        tracing::debug!(
            block_id = %block.id,
            "Created text block with {} lines",
            block.lines.len()
        );

        block
    }
}

/// Group text elements into lines and blocks based on spatial relationships
pub fn group_text_into_lines_and_blocks(
    pages_map: &BTreeMap<u32, Vec<TextElement>>,
    line_join_threshold: f32,
    block_join_threshold: f32,
) -> Vec<TextBlock> {
    let mut all_blocks = Vec::new();

    for (page_number, elements) in pages_map.into_iter() {
        let mut elements = elements.clone();
        elements.sort_by(|a, b| {
            b.bbox
                .1
                .partial_cmp(&a.bbox.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.bbox
                        .0
                        .partial_cmp(&b.bbox.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        let mut lines = Vec::new();
        let mut current_line = Vec::new();

        let mut last_y = f32::MAX;
        let mut last_font_name: Option<String> = None;
        let mut last_font_size: Option<f32> = None;

        for elem in elements {
            let current_font_name = elem.font_name.clone();
            let current_font_size = Some(elem.font_size);

            if current_line.is_empty() {
                current_line.push(elem.clone());
                last_y = elem.bbox.1;
                last_font_name = current_font_name;
                last_font_size = current_font_size;
            } else {
                let y_close = (last_y - elem.bbox.1).abs() < line_join_threshold;
                let font_matches = last_font_name == current_font_name
                    && last_font_size.map_or(false, |last_size| {
                        current_font_size
                            .map_or(false, |curr_size| (last_size - curr_size).abs() < 0.1)
                    });

                if y_close && font_matches {
                    current_line.push(elem.clone());
                } else {
                    lines.push(TextLine::from_elements(*page_number, current_line));
                    current_line = vec![elem.clone()];
                    last_y = elem.bbox.1;
                    last_font_name = current_font_name;
                    last_font_size = current_font_size;
                }
            }
        }

        if !current_line.is_empty() {
            lines.push(TextLine::from_elements(*page_number, current_line));
        }

        for line in &mut lines {
            line.elements.sort_by(|a, b| {
                a.bbox
                    .0
                    .partial_cmp(&b.bbox.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        let mut blocks = Vec::new();
        let mut current_block_lines = Vec::new();

        let mut prev_line_y: Option<f32> = None;
        for line in lines {
            let line_y_top = line.bbox.1.min(line.bbox.3);
            if let Some(py) = prev_line_y {
                if (py - line_y_top).abs() > block_join_threshold {
                    if !current_block_lines.is_empty() {
                        blocks.push(TextBlock::from_lines(*page_number, current_block_lines));
                        current_block_lines = Vec::new();
                    }
                }
            }
            prev_line_y = Some(line_y_top);
            current_block_lines.push(line);
        }

        if !current_block_lines.is_empty() {
            blocks.push(TextBlock::from_lines(*page_number, current_block_lines));
        }

        all_blocks.extend(blocks);
    }

    all_blocks
}

// Additional layout utility functions that focus on spatial relationships
pub fn is_vertically_aligned(elem1: &TextElement, elem2: &TextElement, threshold: f32) -> bool {
    let center1 = (elem1.bbox.0 + elem1.bbox.2) / 2.0;
    let center2 = (elem2.bbox.0 + elem2.bbox.2) / 2.0;
    (center1 - center2).abs() < threshold
}

pub fn is_horizontally_aligned(elem1: &TextElement, elem2: &TextElement, threshold: f32) -> bool {
    let center1 = (elem1.bbox.1 + elem1.bbox.3) / 2.0;
    let center2 = (elem2.bbox.1 + elem2.bbox.3) / 2.0;
    (center1 - center2).abs() < threshold
}

// Other spatial utilities as needed

/// Group text elements into lines without creating blocks
pub fn group_text_into_lines(
    text_elements: &Vec<TextElement>,
    line_join_threshold: f32,
) -> Vec<TextLine> {
    let mut elements = text_elements.clone();
    elements.sort_by(|a, b| {
        b.bbox
            .1
            .partial_cmp(&a.bbox.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.bbox
                    .0
                    .partial_cmp(&b.bbox.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    let mut lines = Vec::new();
    let mut current_line = Vec::new();
    let mut last_y = f32::MAX;
    let mut last_font_name: Option<String> = None;
    let mut last_font_size: Option<f32> = None;

    for elem in elements {
        let current_font_name = elem.font_name.clone();
        let current_font_size = Some(elem.font_size);

        if current_line.is_empty() {
            current_line.push(elem.clone());
            last_y = elem.bbox.1;
            last_font_name = current_font_name;
            last_font_size = current_font_size;
        } else {
            let y_close = (last_y - elem.bbox.1).abs() < line_join_threshold;
            let font_matches = last_font_name == current_font_name
                && last_font_size.map_or(false, |last_size| {
                    current_font_size.map_or(false, |curr_size| (last_size - curr_size).abs() < 0.1)
                });

            if y_close && font_matches {
                current_line.push(elem.clone());
            } else {
                if let Some(first_elem) = current_line.first() {
                    let current_page_number = first_elem.page_number;
                    lines.push(TextLine::from_elements(current_page_number, current_line));
                }
                current_line = vec![elem.clone()];
                last_y = elem.bbox.1;
                last_font_name = current_font_name;
                last_font_size = current_font_size;
            }
        }
    }

    if !current_line.is_empty() {
        if let Some(first_elem) = current_line.first() {
            let current_page_number = first_elem.page_number;
            lines.push(TextLine::from_elements(current_page_number, current_line));
        }
    }

    // Sort elements within each line
    for line in &mut lines {
        line.elements.sort_by(|a, b| {
            a.bbox
                .0
                .partial_cmp(&b.bbox.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    lines
}

// FontUsage struct is now in search_index.rs

/// Analyze the font usage patterns in a document to identify potential heading levels
/// Uses the PdfIndex for efficient font usage analysis
pub fn identify_heading_levels(
    index: &crate::search_index::PdfIndex,
    max_levels: usize,
    start_index: Option<usize>,
    end_index: Option<usize>,
) -> Vec<((String, f32), u32)> {
    // Get font statistics for the scope
    let (mean, std_dev) = index.get_font_size_stats(start_index, end_index);
    let text_element_count = index.get_text_element_count(start_index, end_index);

    println!(
        "[identify_heading_levels] Calculated avg_font_size: {}, std_dev: {}",
        mean, std_dev
    );

    // Find fonts with moderate z-scores (not extreme outliers like titles)
    // but above average (potential headings)
    let font_candidates = index.find_fonts_by_z_score(0.1, start_index, end_index); // 0.1 std devs above mean

    let mut candidates = Vec::new();

    for ((font_name, font_size), usage_count, z_score) in font_candidates {
        // Criteria for heading detection:
        // 1. Must appear multiple times (not unique like titles)
        let min_abs_usage = 2;
        // 2. Must not be too frequent (not more than 50% of text elements)
        let max_rel_usage_threshold = if text_element_count > 0 {
            std::cmp::max(2, text_element_count / 2)
        } else {
            0
        };

        println!("[identify_heading_levels] Checking style: ({}, {}), usage: {}, z_score: {:.2}, text_count: {}, max_rel_thresh: {}", 
            font_name, font_size, usage_count, z_score, text_element_count, max_rel_usage_threshold);

        // Check if this font/size combination could be a heading:
        if usage_count >= min_abs_usage
            && (max_rel_usage_threshold == 0 || usage_count <= max_rel_usage_threshold)
            && z_score > 0.0
        // Must be above average
        {
            println!("    -> Candidate ACCEPTED");
            candidates.push(((font_name, font_size), usage_count));
        } else {
            println!(
                "    -> Candidate REJECTED (usage: {}, z_score_check: {}, rel_usage_check: {})",
                usage_count >= min_abs_usage,
                z_score > 0.0,
                (max_rel_usage_threshold == 0 || usage_count <= max_rel_usage_threshold)
            );
        }
    }

    // Sort by font size (descending) then by usage count (descending)
    candidates.sort_by(|a, b| {
        b.0 .1
            .partial_cmp(&a.0 .1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.cmp(&a.1))
    });

    candidates.into_iter().take(max_levels).collect()
}

/// Find elements in the document that match a specific font and size
pub fn find_elements_by_font(
    pages_map: &BTreeMap<u32, Vec<PageContent>>,
    font_name_filter: Option<&str>,
    target_font_size: Option<f32>,
) -> Vec<PageContent> {
    pages_map
        .values()
        .flat_map(|page_contents| page_contents.iter())
        .filter_map(|content| match content {
            PageContent::Text(text_elem) => {
                let canonical_font_name = crate::fonts::canonicalize::canonicalize_font_name(
                    text_elem.font_name.as_deref().unwrap_or_default(),
                );

                let name_matches =
                    font_name_filter.map_or(true, |fname| canonical_font_name == fname);
                let size_matches = target_font_size
                    .map_or(true, |tsize| (text_elem.font_size - tsize).abs() < 0.1);

                if name_matches && size_matches {
                    Some(content.clone())
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect()
}

/// Find elements that are likely to be at a specific heading level using PdfIndex
pub fn find_elements_at_heading_level(
    index: &crate::search_index::PdfIndex,
    level: usize,
    start_index: Option<usize>,
    end_index: Option<usize>,
) -> Vec<&PageContent> {
    let heading_levels = identify_heading_levels(index, level + 1, start_index, end_index);

    if level >= heading_levels.len() {
        return Vec::new();
    }

    let ((font_name, font_size), _usage_count) = &heading_levels[level];

    // Get elements by font and filter by scope if needed
    let elements = index.elements_by_font(Some(font_name), Some(*font_size), None, None);

    if let (Some(start), Some(end)) = (start_index, end_index) {
        elements
            .into_iter()
            .filter(|elem| {
                if let Some(&idx) = index.element_id_to_index.get(&elem.id()) {
                    idx >= start && idx < end
                } else {
                    false
                }
            })
            .collect()
    } else {
        elements
    }
}

/// Convenience function that creates a temporary index for backward compatibility
pub fn find_elements_at_heading_level_from_pages(
    pages_map: &BTreeMap<u32, Vec<PageContent>>,
    level: usize,
) -> Vec<PageContent> {
    let match_context = MatchContext::default();
    let index = crate::search_index::PdfIndex::new(pages_map, &match_context);
    find_elements_at_heading_level(&index, level, None, None)
        .into_iter()
        .cloned()
        .collect()
}
