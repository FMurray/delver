use crate::dom::{Element, ElementType, MatchType, Value};
use crate::layout::{elements_from_lines, group_text_into_lines, MatchContext, TextBlock, TextLine};
use crate::logging::{MATCHER_OPERATIONS, TEMPLATE_MATCH};
use crate::parse::TextElement;
use crate::search_index::PdfIndex;
use log::{error, info};
use ordered_float::OrderedFloat;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use strsim::normalized_levenshtein;
use tracing::{event, Level};
use uuid::Uuid;

const ENABLE_MATCHER_LOGGING: bool = true;

#[derive(Debug, Clone)]
pub struct TemplateContentMatch<'a> {
    pub template_element: &'a Element,
    pub matched_content: MatchedContent<'a>,
    pub children: Vec<TemplateContentMatch<'a>>,
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone)]
pub enum MatchedContent<'a> {
    Block(TextBlock),
    Line(TextLine),
    Element(TextElement),
    Section {
        start_marker: &'a TextElement,
        end_marker: Option<&'a TextElement>,
        content: Vec<&'a TextElement>,
    },
    TextChunk {
        content: Vec<&'a TextElement>,
    },
    None,
}

impl<'a> TemplateContentMatch<'a> {
    pub fn new(template_element: &'a Element) -> Self {
        TemplateContentMatch {
            template_element: template_element,
            matched_content: MatchedContent::None,
            children: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    pub fn with_content(template_element: &'a Element, content: MatchedContent<'a>) -> Self {
        TemplateContentMatch {
            template_element: template_element,
            matched_content: content,
            children: Vec::new(),
            metadata: HashMap::new(),
        }
    }
}

/// Aligns template elements with document content sequentially
pub fn align_template_with_content<'a>(
    template_elements: &'a [Element],
    index: &'a PdfIndex,
    inherited_metadata: Option<&HashMap<String, Value>>,
    prev_match: Option<&TemplateContentMatch<'a>>,
) -> Option<Vec<TemplateContentMatch<'a>>> {
    if template_elements.is_empty() {
        return None;
    }

    let mut results = Vec::new();
    let mut current_prev = prev_match;
    
    let default_metadata = HashMap::new(); // Default for Option
    let actual_inherited_metadata = inherited_metadata.unwrap_or(&default_metadata);
    
    // Process each template element in sequence
    for template_element in template_elements {
        let maybe_match = match template_element.element_type {
            ElementType::Section => {
                match_section(template_element, index, actual_inherited_metadata, current_prev)
            }
            ElementType::TextChunk => {
                // Assuming match_text_chunk primarily uses index.
                // The original signature was (template, page_map, index, inherited_metadata, prev_match)
                // Current implementation of match_text_chunk uses index.get_elements_between_markers()
                // Passing index.page_map for page_map argument for now.
                match_text_chunk(template_element, index.page_map, index, actual_inherited_metadata, current_prev)
            }
            ElementType::Table => match_table(template_element, index.page_map, actual_inherited_metadata),
            ElementType::Image => match_image(template_element, index.page_map, actual_inherited_metadata),
            _ => None,
        };
        
        // If we found a match, add it to results and update current_prev
        if let Some(matched) = maybe_match {
            current_prev = Some(&matched);
            results.push(matched);
        }
    }

    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}

/// Finds section match that comes after prev_match
fn match_section<'a>(
    template: &'a Element,
    index: &'a PdfIndex,
    inherited_metadata: &HashMap<String, Value>,
    prev_match: Option<&TemplateContentMatch<'a>>,
) -> Option<TemplateContentMatch<'a>> {
    // Extract matching config from template
    let match_config = template.attributes.get("match")?.as_match_config()?;
    
    // Find candidate matches
    let mut candidates: Vec<&TextLine> = match match_config.match_type {
        MatchType::Text => {
            // Group elements into text lines using index.page_map
            let lines = group_text_into_lines(index.page_map, 5.0);
            
            // Find matches in these lines using the new index method
            index.find_line_text_matches(
                &match_config.pattern, 
                match_config.threshold,
                &lines
            )
        },
        // Other match types could go here
        _ => return None,
    };
    
    if candidates.is_empty() {
        return None;
    }
    
    // If we have a previous match, filter candidates to only those after it
    if let Some(prev) = prev_match {
        // Get the end position of previous match
        let min_pos = match &prev.matched_content {
            MatchedContent::Section { end_marker, start_marker, .. } => {
                if let Some(end) = end_marker {
                    // If there's an end marker, start after it
                    index.element_id_to_index.get(&end.id).cloned()
                } else {
                    // Otherwise use start marker
                    index.element_id_to_index.get(&start_marker.id).cloned()
                }
            },
            MatchedContent::TextChunk { content } => {
                // For text chunks, use the last element
                content.last().and_then(|last| 
                    index.element_id_to_index.get(&last.id).cloned())
            },
            _ => None
        };
        
        // Filter candidates to only those after min_pos
        if let Some(pos) = min_pos {
            candidates = candidates.into_iter()
                .filter(|line| {
                    // Get the first element from the line for position comparison
                    if let Some(first_elem) = line.elements.first() {
                        if let Some(&elem_idx) = index.element_id_to_index.get(&first_elem.id) {
                            elem_idx > pos
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                })
                .collect();
        }
    }
    
    if candidates.is_empty() {
        return None;
    }

    // Convert our line candidates to individual elements
    let matched_elements: Vec<&TextElement> = candidates.iter()
        .flat_map(|line| line.elements.iter())
        .collect();

    // Score and select best remaining candidate
    let best_match = select_best_match(matched_elements, index)?;
    
    // Find end marker if specified
    let end_element = if let Some(end_match_str) = template
        .attributes
        .get("end_match")
        .and_then(|v| v.as_string())
    {
        // Find potential end markers using the index
        let end_matches = index.find_text_matches(&end_match_str, match_config.threshold);
        
        // Filter to keep only elements after the start element
        let candidates: Vec<_> = end_matches.iter()
            .filter(|(elem, _)| {
                // Elements after start_element (same page, lower Y position)
                (elem.page_number == best_match.page_number && elem.bbox.1 > best_match.bbox.1) || 
                // Or on later pages
                elem.page_number > best_match.page_number
            })
            .collect();
        
        // Get the closest candidate (first element after start)
        candidates.into_iter()
            .min_by(|(a, _), (b, _)| {
                if a.page_number != b.page_number {
                    a.page_number.cmp(&b.page_number)
                } else {
                    a.bbox.1.partial_cmp(&b.bbox.1).unwrap_or(std::cmp::Ordering::Equal)
                }
            })
            .map(|(elem, _)| elem.clone())
    } else {
        None
    };
    
    // Extract content between markers
    let section_content = extract_section_content(
        index.page_map, 
        best_match.page_number, 
        best_match, 
        end_element
    );

    // Create match result
    let mut result = TemplateContentMatch::with_content(
        template,
        MatchedContent::Section {
            start_marker: best_match,
            end_marker: end_element,
            content: section_content,
        },
    );
    
    // Set metadata from inherited_metadata
    result.metadata = inherited_metadata.clone();

    // Process children recursively, passing the current match as prev_match
    if !template.children.is_empty() {
        if let Some(child_matches) = align_template_with_content(
            &template.children,
            index, // Pass index
            Some(&result.metadata), // Pass Option<&HashMap>
            Some(&result),  // Pass current match as prev_match for children
        ) {
            result.children = child_matches;
        }
    }

    Some(result)
}

/// Matches a TextChunk element in the template with content
fn match_text_chunk<'a>(
    template: &'a Element,
    page_map: &'a BTreeMap<u32, Vec<TextElement>>, // Kept for signature consistency, though might not be fully used by current impl
    index: &'a PdfIndex,
    inherited_metadata: &HashMap<String, Value>,
    prev_match: Option<&TemplateContentMatch<'a>>,
) -> Option<TemplateContentMatch<'a>> {
    if index.elements.is_empty() {
        return None;
    }
    
    let prev_end_element: Option<&TextElement> = prev_match.and_then(|pm| match &pm.matched_content {
        MatchedContent::Block(b) => b.lines.last().and_then(|l| l.elements.last()), // Assuming TextLine elements are &'a TextElement or can provide one
        MatchedContent::Line(l) => l.elements.last(), // Same assumption
        MatchedContent::Element(e) => Some(e),
        MatchedContent::Section { end_marker, start_marker, .. } => {
            end_marker.as_ref().copied().or_else(|| Some(*start_marker))
        }
        MatchedContent::TextChunk { content } => content.last().copied(),
        MatchedContent::None => None,
    });

    // Assuming get_elements_between_markers takes Option<&TextElement>, Option<&TextElement>
    let content = index.get_elements_between_markers(prev_end_element, None);
    
    let mut result = TemplateContentMatch::with_content(
        template,
        MatchedContent::TextChunk {
            content,
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
pub fn select_best_match<'a>(matched_elements: Vec<&'a TextElement>, index: &PdfIndex) -> Option<&'a TextElement> {
    if matched_elements.is_empty() {
        return None;
    }

    matched_elements.into_iter().max_by(|a, b| {
        let score_a = score_match_line(a, index);
        let score_b = score_match_line(b, index);
        score_a
            .partial_cmp(&score_b)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Scores a text line for matching quality
fn score_match_line(line: &TextElement, index: &PdfIndex) -> f32 {
    // Scoring factors:
    // 1. Font size (larger is better)
    // 2. Font weight (bold is better) - not directly available
    // 3. Position on page (headers near top or bottom)
    // 4. Case (all caps or title case is better)
    // 5. Presence in document destinations/bookmarks

    let mut score = 0.0;

    // Font size score - larger fonts are more likely to be headers
    let avg_font_size = index.elements.iter()
        .map(|e| e.font_size)
        .sum::<f32>() / index.elements.len() as f32;

    let font_size_score = ((line.font_size / avg_font_size) - 1.0).max(0.0).min(1.0);
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

    // Check for references - if this element has destinations pointing to it
    if let Some(element_idx) = index.element_id_to_index.get(&line.id) {
        let ref_count = index.reference_count_index.iter()
            .find(|&&(_, idx)| idx == *element_idx)
            .map(|&(count, _)| count)
            .unwrap_or(0);
            
        if ref_count > 0 {
            score += 0.2 * (ref_count as f32).min(5.0) / 5.0; // Max boost of 0.2
        }
    }

    score
}

/// Extracts the content between a start element and an optional end element
pub fn extract_section_content<'a>(
    page_map: &'a BTreeMap<u32, Vec<TextElement>>,
    start_page: u32,
    start_element: &TextElement,
    end_element: Option<&TextElement>,
) -> Vec<&'a TextElement> {
    let mut content = Vec::new();
    let mut capturing = false;

    for (page_num, elements) in page_map {
        // Skip pages before start page
        if *page_num < start_page {
            continue;
        }

        // Skip pages after end page (if known)
        if let Some(end) = end_element {
            if *page_num > end.page_number {
                break;
            }
        }

        for element in elements {
            if !capturing {
                // Start capturing when we find the start element
                if element.id == start_element.id {
                    capturing = true;
                    content.push(element);
                }
            } else {
                // Stop capturing if we hit the end element
                if let Some(end) = end_element {
                    if element.id == end.id {
                        return content;
                    }
                }

                content.push(element);
            }
        }
    }

    content
}

// Convert a flat list of elements back into a page map for child processing
fn create_section_page_map(elements: &[TextElement]) -> BTreeMap<u32, Vec<TextElement>> {
    let mut page_map = BTreeMap::new();

    for element in elements {
        page_map
            .entry(element.page_number) // Changed from element.page
            .or_insert_with(Vec::new)
            .push(element.clone());
    }

    page_map
}

// Add basic implementations for Table and Image matchers
fn match_table<'a>( // Added lifetime 'a
    template: &'a Element,
    page_map: &'a BTreeMap<u32, Vec<TextElement>>,
    inherited_metadata: &HashMap<String, Value>,
) -> Option<TemplateContentMatch<'a>> { // Added lifetime 'a to return type
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

    // Find elements that might indicate a table
    let potential_table_elements: Vec<&'a TextElement> = page_map
        .values()
        .flatten()
        .filter(|element| {
            let text = element.text.to_lowercase();
            table_indicators
                .iter()
                .any(|indicator| text.contains(indicator))
                || text.contains("|")
                || (text.chars().filter(|c| *c == ' ').count() > 5)
        })
        .collect();

    // If we found potential table elements, create a match
    if !potential_table_elements.is_empty() {
        let start_marker = potential_table_elements.first().copied()?; // Use copied to get Option<&'a TextElement>
        let end_marker = potential_table_elements.last().copied();

        println!(
            "MATCHER: Found potential table starting with element: {}",
            start_marker.text
        );
        
        let table_content = extract_section_content(
            page_map,
            start_marker.page_number,
            start_marker,
            end_marker,
        );

        // Create a match result
        let mut result = TemplateContentMatch::with_content(
            template,
            MatchedContent::Section {
                start_marker, // Now &'a TextElement
                end_marker,   // Now Option<&'a TextElement>
                content: table_content, // Now Vec<&'a TextElement>
            },
        );

        // Add metadata
        result.metadata = inherited_metadata.clone();

        // Log match using tracing
        event!(
            Level::DEBUG,
            target = TEMPLATE_MATCH,
            template_id = %Uuid::new_v4(),
            content_id = %start_marker.id,
            template_name = %template.name,
            score = 0.8,
            "Table template matched content with score 0.8"
        );

        return Some(result);
    }

    None
}

fn match_image<'a>( // Added lifetime 'a
    template: &'a Element,
    page_map: &'a BTreeMap<u32, Vec<TextElement>>,
    inherited_metadata: &HashMap<String, Value>,
) -> Option<TemplateContentMatch<'a>> { // Added lifetime 'a to return type
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

    let potential_image_elements: Vec<&'a TextElement> = page_map
        .values()
        .flatten()
        .filter(|element| {
            let text = element.text.to_lowercase();
            image_indicators
                .iter()
                .any(|indicator| text.contains(indicator))
        })
        .collect();

    if !potential_image_elements.is_empty() {
        let caption_element = potential_image_elements.first().copied()?; // Use copied

        println!(
            "MATCHER: Found potential image caption: {}",
            caption_element.text
        );

        // Create a match result
        let mut result = TemplateContentMatch::with_content(
            template,
            MatchedContent::Section {
                start_marker: caption_element, // Now &'a TextElement
                end_marker: None,
                content: Vec::new(), // We don't have actual image content yet from elements
            },
        );

        // Add metadata
        result.metadata = inherited_metadata.clone();

        // Log match using tracing
        event!(
            Level::DEBUG,
            target = TEMPLATE_MATCH,
            template_id = %Uuid::new_v4(),
            content_id = %caption_element.id,
            template_name = %template.name,
            score = 0.7,
            "Image template matched caption with score 0.7"
        );

        return Some(result);
    }

    None
}

// Helper function to extract table content
fn extract_table_content<'a>( // Added lifetime 'a
    page_map: &'a BTreeMap<u32, Vec<TextElement>>,
    start_element: &TextLine, // This signature is problematic if we switched to TextElement markers
    end_element: Option<&TextLine>,
) -> Vec<&'a TextElement> { // Changed return type
    // This function needs significant rework if start_element/end_element are TextLine
    // For now, let's assume it should take TextElements or adapt its logic
    // Given the errors, this function is called with TextLine arguments from old code path.
    // The new path in match_table uses TextElement markers and extract_section_content.
    // This function might be dead code or needs to be aligned.
    // To fix the immediate compile error (line 622 type annotation for collect)
    // and return type, I will make it collect Vec<&'a TextElement>.

    // Find start_index based on start_element (TextLine)
    // This logic is kept from original, but is flawed if TextLine is not &'a.
    // For now, to pass compilation, we make it return Vec<&'a TextElement>
    // but its usage should be reviewed.
    let mut all_elements = Vec::new();
    for elements_on_page in page_map.values() {
        all_elements.extend(elements_on_page.iter());
    }


    let start_text_element = start_element.elements.first(); // Assuming TextLine has elements: Vec<TextElement> or Vec<&'a TextElement>

    let start_index = start_text_element.and_then(|ste| all_elements.iter().position(|e| e.id == ste.id)).unwrap_or(0);

    let end_index = if let Some(end_elem_line) = end_element {
        end_elem_line.elements.last().and_then(|ete| all_elements.iter().position(|e| e.id == ete.id)).unwrap_or(all_elements.len())
    } else {
        (start_index + 20).min(all_elements.len())
    };
    
    if start_index >= end_index {
        return Vec::new();
    }

    all_elements[start_index..end_index].to_vec() // This clones references, which is fine.
}
