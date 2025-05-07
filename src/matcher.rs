use crate::dom::{Element, ElementType, MatchType, Value};
use crate::layout::{group_text_into_lines, TextBlock, TextLine};
use crate::logging::TEMPLATE_MATCH;
use crate::parse::{TextElement, ImageElement, PageContent};
use crate::search_index::PdfIndex;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use strsim::normalized_levenshtein;
use tracing::{event, Level, warn};
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
    Image(ImageElement),
    Section {
        start_marker: &'a TextElement,
        end_marker: Option<&'a TextElement>,
        content: Vec<&'a PageContent>,
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
    let mut current_element_idx = prev_match
        .and_then(|pm| get_last_element_index(pm, index))
        .map_or(0, |idx| idx + 1);
    let mut current_image_idx = prev_match
        .and_then(|pm| get_last_image_index(pm, index))
        .map_or(0, |idx| idx + 1);

    for template_element in template_elements {
        let maybe_match = match template_element.element_type {
            ElementType::Section => {
                match_section(template_element, index, &elements_by_page_view, actual_inherited_metadata, current_prev_option, current_element_idx)
            }
            ElementType::TextChunk => {
                match_text_chunk(template_element, index, actual_inherited_metadata, current_prev_option, current_element_idx)
            }
            ElementType::Image => {
                match_image(template_element, index, actual_inherited_metadata, current_image_idx)
            }
            ElementType::Table => match_table(template_element, &elements_by_page_view, actual_inherited_metadata),
            ElementType::ImageSummary | ElementType::ImageBytes | ElementType::ImageCaption | ElementType::ImageEmbedding => None,
            _ => None,
        };
        
        if let Some(matched_val) = maybe_match {
            current_element_idx = get_last_element_index(&matched_val, index).map_or(current_element_idx, |idx| idx + 1);
            current_image_idx = get_last_image_index(&matched_val, index).map_or(current_image_idx, |idx| idx + 1);
            
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
    _prev_match: Option<&TemplateContentMatch<'a>>,
    start_element_index: usize,
) -> Option<TemplateContentMatch<'a>> {
    let _match_config = template.attributes.get("match")?.as_match_config()?;
    
    let relevant_elements: Vec<&TextElement> = index.elements[start_element_index..].iter().collect();
    if relevant_elements.is_empty() { return None; }

    let owned_elements_for_grouping: Vec<TextElement> = relevant_elements.into_iter().cloned().collect();
    let potential_lines = group_text_into_lines(&owned_elements_for_grouping, 5.0);
    if potential_lines.is_empty() { return None; }

    let candidates: Vec<TextLine> = match _match_config.match_type {
        MatchType::Text => {
            index.find_line_text_matches(
                &_match_config.pattern, 
                _match_config.threshold,
                &potential_lines
            )
            .into_iter()
            .cloned()
            .collect()
        },
        _ => return None,
    };
    
    if candidates.is_empty() { return None; }

    let best_match_line = candidates.iter()
        .min_by(|a, b| {
             let first_a_idx = a.elements.first().and_then(|e| index.element_id_to_index.get(&e.id));
             let first_b_idx = b.elements.first().and_then(|e| index.element_id_to_index.get(&e.id));
             match (first_a_idx, first_b_idx) {
                 (Some(idx_a), Some(idx_b)) => idx_a.cmp(idx_b),
                 (Some(_), None) => std::cmp::Ordering::Less,
                 (None, Some(_)) => std::cmp::Ordering::Greater,
                 (None, None) => std::cmp::Ordering::Equal,
             }
         })?;

    let best_match_line_first_elem = best_match_line.elements.first()?;
    let start_marker_ref = index.get_element_by_id(&best_match_line_first_elem.id)?;
    
    let end_element_search_start_index = index.element_id_to_index.get(&start_marker_ref.id).map_or(start_element_index, |idx| idx + 1);
    let end_element_ref = if let Some(end_match_str) = template
        .attributes
        .get("end_match")
        .and_then(|v| v.as_string())
    {
        index.find_text_matches(&end_match_str, _match_config.threshold, Some(end_element_search_start_index))
            .into_iter()
            .min_by_key(|(elem, _score)| index.element_id_to_index.get(&elem.id))
            .map(|(elem_ref, _)| elem_ref)
    } else {
        None
    };
    
    let section_content: Vec<&PageContent> = Vec::new();

    let mut result = TemplateContentMatch::with_content(
        template,
        MatchedContent::Section {
            start_marker: start_marker_ref,
            end_marker: end_element_ref,
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

    if index.elements.is_empty() && end_element_ref.is_none() {
        return None;
    }

    Some(result)
}

/// Matches a TextChunk element in the template with content
fn match_text_chunk<'a>(
    template: &'a Element,
    index: &'a PdfIndex,
    inherited_metadata: &HashMap<String, Value>,
    _prev_match: Option<&TemplateContentMatch<'a>>,
    start_element_index: usize,
) -> Option<TemplateContentMatch<'a>> {
    
    let content = if start_element_index < index.elements.len() {
        index.elements[start_element_index..].iter().collect()
    } else {
        Vec::new()
    };

    if content.is_empty() { return None; }
    
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
/// TODO: Update this function to handle Vec<PageContent> instead of just TextElement
pub fn extract_section_content<'a>(
    _page_map_view: &BTreeMap<u32, Vec<&'a TextElement>>,
    _start_page: u32,
    _start_element: &TextElement,
    _end_element: Option<&TextElement>,
) -> Vec<&'a TextElement> {
    warn!("extract_section_content needs update to handle PageContent");
    Vec::new()
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
        
        let table_content: Vec<&PageContent> = Vec::new();

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
            "Table template matched content (placeholder)"
        );
        return Some(result);
    }

    None
}

fn match_image<'a>(
    template: &'a Element,
    index: &'a PdfIndex,
    inherited_metadata: &HashMap<String, Value>,
    start_image_index: usize,
) -> Option<TemplateContentMatch<'a>> {
    println!("MATCHER: Processing Image template element, starting search from index {}", start_image_index);

    index.images.get(start_image_index).map(|image_elem| {
        println!("MATCHER: Found image with ID {}", image_elem.id);
        let mut result = TemplateContentMatch::with_content(
            template,
            MatchedContent::Image(image_elem.clone()),
        );
        result.metadata = inherited_metadata.clone();
        event!(
            Level::DEBUG,
            target = TEMPLATE_MATCH,
            template_id = %Uuid::new_v4(),
            content_id = %image_elem.id,
            template_name = %template.name,
            score = 0.9,
            "Image template matched content"
        );
        result
    })
}

// Helper to get the index of the last TextElement used by a match
fn get_last_element_index<'a>(match_item: &TemplateContentMatch<'a>, index: &'a PdfIndex) -> Option<usize> {
    match &match_item.matched_content {
        MatchedContent::Section { end_marker, start_marker, .. } => 
            end_marker.as_ref().copied().or(Some(*start_marker))
                .and_then(|elem| index.element_id_to_index.get(&elem.id).copied()),
        MatchedContent::TextChunk { content } => 
            content.last().copied()
                .and_then(|elem| index.element_id_to_index.get(&elem.id).copied()),
        MatchedContent::Element(e) => index.element_id_to_index.get(&e.id).copied(),
        MatchedContent::Image(_) => None, 
        _ => None,
    }
}

// Helper to get the index of the last ImageElement used by a match
fn get_last_image_index<'a>(match_item: &TemplateContentMatch<'a>, index: &'a PdfIndex) -> Option<usize> {
    match &match_item.matched_content {
        MatchedContent::Image(img_elem) => index.image_id_to_index.get(&img_elem.id).copied(),
        _ => None,
    }
}
