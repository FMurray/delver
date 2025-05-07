use indexmap::IndexMap;
use lopdf::{Dictionary, Object};
use std::collections::BTreeMap;
use strsim::normalized_levenshtein;
use uuid::Uuid;
use std::collections::HashMap;
use std::fmt::Debug;

use crate::geo::Rect;
use crate::parse::TextElement;

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
        let mut combined_text = String::new();

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

        for elem in elements {
            if current_line.is_empty() {
                current_line.push(elem.clone());
                last_y = elem.bbox.1;
            } else {
                if (last_y - elem.bbox.1).abs() < line_join_threshold {
                    current_line.push(elem.clone());
                } else {
                    lines.push(TextLine::from_elements(*page_number, current_line));
                    current_line = vec![elem.clone()];
                    last_y = elem.bbox.1;
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
    let all_lines = Vec::new();

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

    for elem in elements {
        if current_line.is_empty() {
            current_line.push(elem.clone());
            last_y = elem.bbox.1;
        } else {
            if (last_y - elem.bbox.1).abs() < line_join_threshold {
                current_line.push(elem.clone());
            } else {
                if let Some(first_elem) = current_line.first() {
                    let current_page_number = first_elem.page_number;
                    lines.push(TextLine::from_elements(current_page_number, current_line));
                }
                current_line = vec![elem.clone()];
                last_y = elem.bbox.1;
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

    all_lines
}
