use crate::dom::{Element, ElementType, MatchType, Value};
use crate::layout::{group_text_into_lines, TextBlock, TextLine};
use crate::logging::TEMPLATE_MATCH;
use crate::parse::TextElement;
use crate::search_index::PdfIndex;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use strsim::normalized_levenshtein;
use tracing::{event, Level};
use uuid::Uuid;

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

    let mut results: Vec<TemplateContentMatch<'a>> = Vec::new();
    let mut current_prev_holder;

    let default_metadata = HashMap::new();
    let actual_inherited_metadata = inherited_metadata.unwrap_or(&default_metadata);

    let mut elements_by_page_view: BTreeMap<u32, Vec<&'a TextElement>> = BTreeMap::new();
    for (page_num, element_indices) in index.by_page.iter() {
        let mut page_elements: Vec<&'a TextElement> = Vec::new();
        for &idx in element_indices {
            if let Some(element) = index.elements.get(idx) {
                page_elements.push(element);
            }
        }
        if !page_elements.is_empty() {
            elements_by_page_view.insert(*page_num, page_elements);
        }
    }
    
    let mut current_prev_option = prev_match;

    for template_element in template_elements {
        let maybe_match = match template_element.element_type {
            ElementType::Section => {
                match_section(template_element, index, &elements_by_page_view, actual_inherited_metadata, current_prev_option)
            }
            ElementType::TextChunk => {
                match_text_chunk(template_element, &elements_by_page_view, index, actual_inherited_metadata, current_prev_option)
            }
            ElementType::Table => match_table(template_element, &elements_by_page_view, actual_inherited_metadata),
            ElementType::Image => match_image(template_element, &elements_by_page_view, actual_inherited_metadata),
            _ => None,
        };
        
        if let Some(matched_val) = maybe_match {
            results.push(matched_val);
            current_prev_holder = results.last().unwrap();
            current_prev_option = Some(current_prev_holder);
        }
    }

    if results.is_empty() {
        None
    } else {
        Some(results)
    }
}

/// Finds section match that comes after prev_match
fn match_section<'a, 'map_lt>(
    template: &'a Element,
    index: &'a PdfIndex,
    _page_map_view: &'map_lt BTreeMap<u32, Vec<&'a TextElement>>,
    inherited_metadata: &HashMap<String, Value>,
    prev_match: Option<&TemplateContentMatch<'a>>,
) -> Option<TemplateContentMatch<'a>> {
    let _match_config = template.attributes.get("match")?.as_match_config()?;
    
    let mut candidates: Vec<TextLine> = match _match_config.match_type {
        MatchType::Text => {
            let owned_elements_for_grouping: Vec<TextElement> = index.elements.iter().map(|el_ref| el_ref.clone()).collect();
            let all_lines = group_text_into_lines(&owned_elements_for_grouping, 5.0);
            index.find_line_text_matches(
                &_match_config.pattern, 
                _match_config.threshold,
                &all_lines 
            )
            .into_iter()
            .map(|line_ref| line_ref.clone())
            .collect()
        },
        _ => return None,
    };
    
    if candidates.is_empty() {
        return None;
    }
    
    if let Some(prev) = prev_match {
        let min_pos = match &prev.matched_content {
            MatchedContent::Section { end_marker, start_marker, .. } => {
                end_marker.as_ref().copied().or_else(|| Some(*start_marker))
                    .and_then(|elem| index.element_id_to_index.get(&elem.id).copied())
            },
            MatchedContent::TextChunk { content } => {
                content.last().copied()
                    .and_then(|last| index.element_id_to_index.get(&last.id).copied())
            },
            _ => None
        };
        
        if let Some(pos) = min_pos {
            candidates.retain(|line| {
                if let Some(first_elem) = line.elements.first() {
                    if let Some(&elem_idx) = index.element_id_to_index.get(&first_elem.id) {
                        elem_idx > pos
                    } else {
                        false
                    }
                } else {
                    false
                }
            });
        }
    }
    
    if candidates.is_empty() {
        return None;
    }

    let potential_matched_elements: Vec<&TextElement> = candidates.iter()
        .flat_map(|line| line.elements.iter())
        .collect();

    let best_match_id = select_best_match(potential_matched_elements, index)?;
    
    let best_match_element_ref = index.elements.iter().find(|el| el.id == best_match_id)?;
    
    let end_element = if let Some(end_match_str) = template
        .attributes
        .get("end_match")
        .and_then(|v| v.as_string())
    {
        let end_matches = index.find_text_matches(&end_match_str, _match_config.threshold);
        
        let end_candidates: Vec<_> = end_matches.iter()
            .filter(|(elem, _)| {
                (elem.page_number == best_match_element_ref.page_number && elem.bbox.1 > best_match_element_ref.bbox.1) || 
                elem.page_number > best_match_element_ref.page_number
            })
            .collect();
        
        end_candidates.into_iter()
            .min_by(|(a, _), (b, _)| {
                if a.page_number != b.page_number {
                    a.page_number.cmp(&b.page_number)
                } else {
                    a.bbox.1.partial_cmp(&b.bbox.1).unwrap_or(std::cmp::Ordering::Equal)
                }
            })
            .map(|(elem_ref_ref, _)| *elem_ref_ref)
    } else {
        None
    };
    
    let section_content = extract_section_content(
        _page_map_view,
        best_match_element_ref.page_number, 
        best_match_element_ref, 
        end_element
    );

    let mut result = TemplateContentMatch::with_content(
        template,
        MatchedContent::Section {
            start_marker: best_match_element_ref,
            end_marker: end_element,
            content: section_content,
        },
    );
    
    result.metadata = inherited_metadata.clone();

    if !template.children.is_empty() {
        if let Some(child_matches) = align_template_with_content(
            &template.children,
            index,
            Some(&result.metadata),
            Some(&result),
        ) {
            result.children = child_matches;
        }
    }

    if index.elements.is_empty() && end_element.is_none() {
        return None;
    }

    Some(result)
}

/// Matches a TextChunk element in the template with content
fn match_text_chunk<'a, 'map_lt>(
    template: &'a Element,
    _page_map_view: &'map_lt BTreeMap<u32, Vec<&'a TextElement>>,
    index: &'a PdfIndex,
    inherited_metadata: &HashMap<String, Value>,
    prev_match: Option<&TemplateContentMatch<'a>>,
) -> Option<TemplateContentMatch<'a>> {
    if index.elements.is_empty() {
        return None;
    }
    
    let prev_end_element: Option<&TextElement> = prev_match.and_then(|pm| match &pm.matched_content {
        MatchedContent::Block(b) => b.lines.last().and_then(|l| l.elements.last()),
        MatchedContent::Line(l) => l.elements.last(),
        MatchedContent::Element(e) => Some(e),
        MatchedContent::Section { end_marker, start_marker, .. } => {
            end_marker.as_ref().copied().or_else(|| Some(*start_marker))
        }
        MatchedContent::TextChunk { content } => content.last().copied(),
        MatchedContent::None => None,
    });

    if index.elements.is_empty() && prev_end_element.is_none() {
        return None;
    }
    
    let content = match prev_end_element {
        Some(start_marker) => index.get_elements_between_markers(start_marker, None),
        None => { 
            if let Some(first_doc_element) = index.elements.first() {
                index.get_elements_between_markers(first_doc_element, None)
            } else {
                Vec::new()
            }
        }
    };
    
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

/// Selects the best match from a list of potential matches, returning its ID
pub fn select_best_match<'a>(
    matched_elements: Vec<&'a TextElement>,
    index: &'a PdfIndex
) -> Option<Uuid> {
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
    .map(|best_element| best_element.id)
}

/// Scores a text line for matching quality
fn score_match_line(line: &TextElement, index: &PdfIndex) -> f32 {
    let mut score = 0.0;

    let avg_font_size = index.elements.iter()
        .map(|e| e.font_size)
        .sum::<f32>() / index.elements.len() as f32;

    let font_size_score = ((line.font_size / avg_font_size) - 1.0).max(0.0).min(1.0);
    score += font_size_score * 0.4;

    let y_pos = line.bbox.1;
    let position_score = if y_pos < 100.0 || y_pos > 700.0 {
        1.0
    } else {
        0.3
    };
    score += position_score * 0.3;

    let text = &line.text;
    let case_score = if text.chars().all(|c| c.is_uppercase()) {
        1.0
    } else if text.chars().next().map_or(false, |c| c.is_uppercase()) {
        0.8
    } else {
        0.3
    };
    score += case_score * 0.3;

    if let Some(element_idx) = index.element_id_to_index.get(&line.id) {
        let ref_count = index.reference_count_index.iter()
            .find(|&&(_, idx)| idx == *element_idx)
            .map(|&(count, _)| count)
            .unwrap_or(0);
            
        if ref_count > 0 {
            score += 0.2 * (ref_count as f32).min(5.0) / 5.0;
        }
    }

    score
}

/// Extracts the content between a start element and an optional end element
pub fn extract_section_content<'a, 'map_lt>(
    _page_map_view: &'map_lt BTreeMap<u32, Vec<&'a TextElement>>,
    start_page: u32,
    _start_element: &TextElement,
    end_element: Option<&TextElement>,
) -> Vec<&'a TextElement> {
    let mut content = Vec::new();
    let mut capturing = false;

    for (page_num, elements_on_page) in _page_map_view {
        if *page_num < start_page {
            continue;
        }
        if let Some(end) = end_element {
            if *page_num > end.page_number {
                break;
            }
        }
        for element_ref in elements_on_page {
            if !capturing {
                if (*element_ref).id == _start_element.id {
                    capturing = true;
                    content.push(*element_ref);
                }
            } else {
                if let Some(end) = end_element {
                    if (*element_ref).id == end.id {
                        return content;
                    }
                }
                content.push(*element_ref);
            }
        }
    }

    content
}

// Add basic implementations for Table and Image matchers
fn match_table<'a, 'map_lt>(
    template: &'a Element,
    _page_map_view: &'map_lt BTreeMap<u32, Vec<&'a TextElement>>,
    inherited_metadata: &HashMap<String, Value>,
) -> Option<TemplateContentMatch<'a>> {
    println!("MATCHER: Processing Table template element");

    let _match_config = template.attributes.get("match")?.as_match_config()?;

    let table_indicators = ["table", "column", "row", "|", "total"];

    let potential_table_elements: Vec<&'a TextElement> = _page_map_view
        .values()
        .flatten()
        .copied()
        .filter(|element| {
            let text = element.text.to_lowercase();
            table_indicators
                .iter()
                .any(|indicator| text.contains(indicator))
                || text.contains("|")
                || (text.chars().filter(|c| *c == ' ').count() > 5)
        })
        .collect();

    if !potential_table_elements.is_empty() {
        let start_marker = potential_table_elements.first().copied()?;
        let end_marker = potential_table_elements.last().copied();

        println!(
            "MATCHER: Found potential table starting with element: {}",
            start_marker.text
        );
        
        let table_content = extract_section_content(
            _page_map_view,
            start_marker.page_number,
            start_marker,
            end_marker,
        );

        let mut result = TemplateContentMatch::with_content(
            template,
            MatchedContent::Section {
                start_marker,
                end_marker,
                content: table_content,
            },
        );
        result.metadata = inherited_metadata.clone();
        event!(
            Level::DEBUG,
            target = TEMPLATE_MATCH,
            template_id = %Uuid::new_v4(),
            content_id = %start_marker.id,
            template_name = %template.name,
            score = 0.8,
            "Table template matched content"
        );
        return Some(result);
    }

    None
}

fn match_image<'a, 'map_lt>(
    template: &'a Element,
    _page_map_view: &'map_lt BTreeMap<u32, Vec<&'a TextElement>>,
    inherited_metadata: &HashMap<String, Value>,
) -> Option<TemplateContentMatch<'a>> {
    println!("MATCHER: Processing Image template element");

    let image_indicators = [
        "figure",
        "image",
        "diagram",
        "illustration",
        "photo",
        "picture",
    ];

    let potential_image_elements: Vec<&'a TextElement> = _page_map_view
        .values()
        .flatten()
        .copied()
        .filter(|element| {
            let text = element.text.to_lowercase();
            image_indicators
                .iter()
                .any(|indicator| text.contains(indicator))
        })
        .collect();

    if !potential_image_elements.is_empty() {
        let caption_element = potential_image_elements.first().copied()?;
        println!(
            "MATCHER: Found potential image caption: {}",
            caption_element.text
        );
        let mut result = TemplateContentMatch::with_content(
            template,
            MatchedContent::Section {
                start_marker: caption_element,
                end_marker: None,
                content: Vec::new(),
            },
        );
        result.metadata = inherited_metadata.clone();
        event!(
            Level::DEBUG,
            target = TEMPLATE_MATCH,
            template_id = %Uuid::new_v4(),
            content_id = %caption_element.id,
            template_name = %template.name,
            score = 0.7,
            "Image template matched caption"
        );
        return Some(result);
    }

    None
}
