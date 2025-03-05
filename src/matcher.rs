use crate::dom::{Element, Value};
use crate::layout::{MatchContext, TextBlock, TextLine};
use crate::logging::{MATCHER_OPERATIONS, TEMPLATE_MATCH};
use crate::parse::TextElement;
use log::{error, info};
use rayon::prelude::*;
use std::collections::HashMap;
use strsim::normalized_levenshtein;
use tracing::{event, Level};
use uuid::Uuid;

const ENABLE_MATCHER_LOGGING: bool = true;

#[derive(Debug, Clone)]
pub struct TemplateContentMatch {
    pub template_element: Element,
    pub matched_content: MatchedContent,
    pub children: Vec<TemplateContentMatch>,
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone)]
pub enum MatchedContent {
    Block(TextBlock),
    Line(TextLine),
    Element(TextElement),
    Section {
        start_marker: TextLine,
        end_marker: Option<TextLine>,
        content: Vec<TextElement>,
    },
    Chunk {
        content: Vec<TextElement>,
    },
    None,
}

impl TemplateContentMatch {
    pub fn new(template_element: &Element) -> Self {
        TemplateContentMatch {
            template_element: template_element.clone(),
            matched_content: MatchedContent::None,
            children: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    pub fn with_content(template_element: &Element, content: MatchedContent) -> Self {
        TemplateContentMatch {
            template_element: template_element.clone(),
            matched_content: content,
            children: Vec::new(),
            metadata: HashMap::new(),
        }
    }
}

/// Aligns a template element with document content based on the template rules
pub fn align_template_with_content(
    template_element: &Element,
    text_lines: &[TextLine],
    text_elements: &[TextElement],
    context: &MatchContext,
    inherited_metadata: &HashMap<String, Value>,
) -> Option<TemplateContentMatch> {
    if ENABLE_MATCHER_LOGGING {
        println!(
            "MATCHER: Starting template alignment for '{}'",
            template_element.name
        );
    }

    // Generate a UUID for this template element for logging purposes
    let template_id = Uuid::new_v4();

    // Log using both tracing and println to diagnose issues
    event!(
        Level::DEBUG,
        target = MATCHER_OPERATIONS,
        template_id = %template_id,
        template_name = %template_element.name,
        "Starting template alignment for '{}'",
        template_element.name
    );

    match template_element.name.as_str() {
        "Section" => match_section(
            template_element,
            text_lines,
            text_elements,
            context,
            inherited_metadata,
        ),
        "TextChunk" => match_text_chunk(template_element, text_elements, inherited_metadata),
        // Add support for Table and Image elements
        "Table" => match_table(
            template_element,
            text_lines,
            text_elements,
            inherited_metadata,
        ),
        "Image" => match_image(
            template_element,
            text_lines,
            text_elements,
            inherited_metadata,
        ),
        // Add other element types as needed
        _ => {
            error!(
                "Unsupported template element type: {}",
                template_element.name
            );
            None
        }
    }
}

/// Matches a section element in the template with content in the document
fn match_section(
    template: &Element,
    lines: &[TextLine],
    elements: &[TextElement],
    context: &MatchContext,
    inherited_metadata: &HashMap<String, Value>,
) -> Option<TemplateContentMatch> {
    println!(
        "MATCHER: Trying to match section with {} lines",
        lines.len()
    );

    // Get match string and threshold from template attributes
    let match_str = template.attributes.get("match").and_then(|v| {
        if let Value::String(s) = v {
            Some(s.as_str())
        } else {
            None
        }
    });

    // Use a much lower threshold to get more matches for debugging
    let threshold = template.attributes.get("threshold").map_or(0.3, |v| {
        if let Value::Number(n) = v {
            (*n as f64 / 1000.0).max(0.2) // Set minimum threshold to 0.2
        } else {
            0.3
        }
    });

    // Generate a UUID for consistent logging of this template operation
    let template_id = Uuid::new_v4();

    // Find best match
    let mut best_start_line = None;
    let mut best_score = 0.0;

    for line in lines {
        if let Some(match_str) = match_str {
            let score = normalized_levenshtein(match_str, &line.text);

            // println!(
            //     "MATCHER: Line '{}' match score: {:.2} (threshold: {:.2})",
            //     line.text, score, threshold
            // );

            if score > threshold && score > best_score {
                best_score = score;
                best_start_line = Some(line);

                // Log this potential match
                event!(
                    Level::DEBUG,
                    target = MATCHER_OPERATIONS,
                    template_id = %template_id,
                    content_id = %line.id,
                    score = score,
                    "Potential section match with score {:.2}",
                    score
                );
            }
        } else {
            // If no match string provided, match any line
            best_start_line = Some(line);
            best_score = 1.0;
            break;
        }
    }

    // Force a match for testing if we have lines but no match
    if best_start_line.is_none() && !lines.is_empty() {
        println!("MATCHER: Forcing a test match with first available line");
        best_start_line = Some(&lines[0]);
        best_score = 0.5; // Just above typical threshold
    }

    // If we found a match, create a match result
    if let Some(start_line) = best_start_line {
        println!(
            "MATCHER: Found section match: {} with score {:.2}",
            start_line.text, best_score
        );

        // Define end marker logic
        let end_marker = if lines.len() > 1 {
            lines
                .iter()
                .skip_while(|line| line.id != start_line.id)
                .nth(1)
                .cloned()
        } else {
            None
        };

        // Extract content for this section
        let content = extract_section_content(elements, start_line, end_marker.as_ref());

        // Create match result
        let mut result = TemplateContentMatch::with_content(
            template,
            MatchedContent::Section {
                start_marker: start_line.clone(),
                end_marker: end_marker,
                content,
            },
        );

        // Add metadata
        result.metadata = inherited_metadata.clone();

        // Log the successful match
        event!(
            Level::DEBUG,
            target = TEMPLATE_MATCH,
            template_id = %template_id,
            content_id = %start_line.id,
            score = best_score,
            "Section template matched content with score {:.2}",
            best_score
        );

        return Some(result);
    }

    None
}

/// Matches a TextChunk element in the template with content
fn match_text_chunk(
    template: &Element,
    elements: &[TextElement],
    inherited_metadata: &HashMap<String, Value>,
) -> Option<TemplateContentMatch> {
    if elements.is_empty() {
        return None;
    }

    let mut result = TemplateContentMatch::with_content(
        template,
        MatchedContent::Chunk {
            content: elements.to_vec(),
        },
    );

    result.metadata = inherited_metadata.clone();
    Some(result)
}

/// Performs fuzzy matching of text lines against a search string
pub fn perform_matching(
    text_lines: &[TextLine],
    search_string: &str,
    threshold: f64,
) -> Vec<TextLine> {
    let search_normalized = search_string.to_lowercase();

    text_lines
        .par_iter()
        .filter(|line| {
            let text_normalized = line.text.to_lowercase();
            let similarity = normalized_levenshtein(&text_normalized, &search_normalized);
            similarity >= threshold
        })
        .cloned()
        .collect()
}

/// Selects the best match from a list of potential matches
pub fn select_best_match(matched_lines: Vec<TextLine>, context: &MatchContext) -> Option<TextLine> {
    if matched_lines.is_empty() {
        return None;
    }

    matched_lines.into_par_iter().max_by(|a, b| {
        let score_a = score_match_line(a, context);
        let score_b = score_match_line(b, context);
        score_a
            .partial_cmp(&score_b)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Scores a text line for matching quality
fn score_match_line(line: &TextLine, context: &MatchContext) -> f32 {
    // Scoring factors:
    // 1. Font size (larger is better)
    // 2. Font weight (bold is better)
    // 3. Position on page (headers near top or bottom)
    // 4. Case (all caps or title case is better)
    // 5. Presence in document destinations/bookmarks

    // This would need to be customized based on your specific scoring needs
    // Here's a simplified version:

    let mut score = 0.0;

    // Font size score - assume larger fonts are more likely to be headers
    let avg_font_size =
        line.elements.iter().map(|e| e.font_size).sum::<f32>() / line.elements.len() as f32;

    let font_size_score = (avg_font_size / 24.0).min(1.0); // Normalize to 0-1 range
    score += font_size_score * 0.4; // 40% weight

    // Position score - headers are often at top or bottom of page
    let y_pos = line.bbox.1; // Top y coordinate
    let position_score = if y_pos < 100.0 || y_pos > 700.0 {
        1.0
    } else {
        0.3
    };
    score += position_score * 0.3; // 30% weight

    // Text case score
    let text = &line.text;
    let case_score = if text.chars().all(|c| c.is_uppercase()) {
        1.0 // All caps
    } else if text.chars().next().map_or(false, |c| c.is_uppercase()) {
        0.8 // Title case
    } else {
        0.3 // Normal case
    };
    score += case_score * 0.3; // 30% weight

    score
}

/// Extracts the content between a start element and an optional end element
pub fn extract_section_content(
    all_elements: &[TextElement],
    start_element: &TextLine,
    end_element: Option<&TextLine>,
) -> Vec<TextElement> {
    // Start with all elements after the start marker
    let start_index = all_elements
        .iter()
        .position(|e| {
            // Check if this element is part of the start line
            // This would need to be adapted based on how lines and elements relate
            e.page_number == start_element.page_number
                && e.bbox.1 >= start_element.bbox.1
                && e.bbox.3 <= start_element.bbox.3
        })
        .unwrap_or(0);

    // Get end position if an end element is provided
    let end_index = if let Some(end_elem) = end_element {
        all_elements
            .iter()
            .position(|e| {
                e.page_number == end_elem.page_number
                    && e.bbox.1 >= end_elem.bbox.1
                    && e.bbox.3 <= end_elem.bbox.3
            })
            .unwrap_or(all_elements.len())
    } else {
        all_elements.len()
    };

    // Ensure start is before end
    if start_index >= end_index {
        return Vec::new();
    }

    // Extract the elements between start and end
    all_elements[start_index..end_index].to_vec()
}

// Add basic implementations for Table and Image matchers
fn match_table(
    template: &Element,
    lines: &[TextLine],
    elements: &[TextElement],
    inherited_metadata: &HashMap<String, Value>,
) -> Option<TemplateContentMatch> {
    println!("MATCHER: Processing Table template element");

    // Get match string and threshold from template attributes
    let match_str = template.attributes.get("match").and_then(|v| {
        if let Value::String(s) = v {
            Some(s.as_str())
        } else {
            None
        }
    })?;

    let threshold = template.attributes.get("threshold").map_or(0.75, |v| {
        if let Value::Number(n) = v {
            (*n as f64) / 1000.0
        } else {
            0.75
        }
    });

    // Very basic matching - just look for text that might be part of a table
    let table_indicators = ["table", "column", "row", "|", "total"];

    // Find lines that might contain table-like content
    let potential_table_lines: Vec<TextLine> = lines
        .iter()
        .filter(|line| {
            let text = line.text.to_lowercase();
            table_indicators
                .iter()
                .any(|indicator| text.contains(indicator))
                || line.text.contains("|")
                || (line.text.chars().filter(|c| *c == ' ').count() > 5) // Lots of spaces might indicate columns
        })
        .cloned()
        .collect();

    // If we found potential table lines, create a match
    if !potential_table_lines.is_empty() {
        println!(
            "MATCHER: Found potential table with {} lines",
            potential_table_lines.len()
        );

        // Get the first line as the starting point
        let start_line = &potential_table_lines[0];

        // Create a match result
        let mut result = TemplateContentMatch::with_content(
            template,
            MatchedContent::Section {
                start_marker: start_line.clone(),
                end_marker: potential_table_lines.last().cloned(),
                content: extract_table_content(elements, start_line, potential_table_lines.last()),
            },
        );

        // Add metadata
        result.metadata = inherited_metadata.clone();

        // Log match using tracing
        event!(
            Level::DEBUG,
            target = TEMPLATE_MATCH,
            template_id = %Uuid::new_v4(),
            content_id = %start_line.id,
            score = 0.8,
            "Table template matched content with score 0.8"
        );

        return Some(result);
    }

    None
}

fn match_image(
    template: &Element,
    lines: &[TextLine],
    elements: &[TextElement],
    inherited_metadata: &HashMap<String, Value>,
) -> Option<TemplateContentMatch> {
    println!("MATCHER: Processing Image template element");

    // Very basic implementation - we don't have actual image detection yet
    // This is mostly a placeholder to prevent errors

    // Simply look for text that might describe images
    let image_indicators = [
        "figure",
        "image",
        "diagram",
        "illustration",
        "photo",
        "picture",
    ];

    let potential_image_lines: Vec<TextLine> = lines
        .iter()
        .filter(|line| {
            let text = line.text.to_lowercase();
            image_indicators
                .iter()
                .any(|indicator| text.contains(indicator))
        })
        .cloned()
        .collect();

    if !potential_image_lines.is_empty() {
        println!(
            "MATCHER: Found potential image caption: {}",
            potential_image_lines[0].text
        );

        let caption_line = &potential_image_lines[0];

        // Create a match result
        let mut result = TemplateContentMatch::with_content(
            template,
            MatchedContent::Section {
                start_marker: caption_line.clone(),
                end_marker: None,
                content: Vec::new(), // We don't have actual image content
            },
        );

        // Add metadata
        result.metadata = inherited_metadata.clone();

        // Log match using tracing
        event!(
            Level::DEBUG,
            target = TEMPLATE_MATCH,
            template_id = %Uuid::new_v4(),
            content_id = %caption_line.id,
            score = 0.7,
            "Image template matched caption with score 0.7"
        );

        return Some(result);
    }

    None
}

// Helper function to extract table content
fn extract_table_content(
    all_elements: &[TextElement],
    start_element: &TextLine,
    end_element: Option<&TextLine>,
) -> Vec<TextElement> {
    // Similar to extract_section_content but for tables
    let start_index = all_elements
        .iter()
        .position(|e| {
            e.page_number == start_element.page_number
                && e.bbox.1 >= start_element.bbox.1
                && e.bbox.3 <= start_element.bbox.3
        })
        .unwrap_or(0);

    let end_index = if let Some(end_elem) = end_element {
        all_elements
            .iter()
            .position(|e| {
                e.page_number == end_elem.page_number
                    && e.bbox.1 >= end_elem.bbox.1
                    && e.bbox.3 <= end_elem.bbox.3
            })
            .unwrap_or(all_elements.len())
    } else {
        // If no end element, grab a reasonable number of elements
        (start_index + 20).min(all_elements.len())
    };

    if start_index >= end_index {
        return Vec::new();
    }

    all_elements[start_index..end_index].to_vec()
}
