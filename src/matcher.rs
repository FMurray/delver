use crate::dom::{Element, ElementType, MatchConfig, MatchType, Value};
use crate::layout::{group_text_into_lines, TextBlock, TextLine};
use crate::logging::TEMPLATE_MATCH;
use crate::parse::{ImageElement, PageContent, TextElement};
use crate::search_index::PdfIndex;
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use strsim::normalized_levenshtein;
use tracing::{event, warn, Level};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct TemplateContentMatch<'a> {
    pub template_element: &'a Element,
    pub matched_content: Vec<MatchedContent<'a>>,
    pub children: Vec<TemplateContentMatch<'a>>,
    pub metadata: HashMap<String, Value>,
    pub section_boundaries: Option<SectionBoundaries<'a>>,
}

#[derive(Debug, Clone)]
pub struct SectionBoundaries<'a> {
    pub start_marker: &'a PageContent,
    pub end_marker: Option<&'a PageContent>,
}

#[derive(Debug, Clone)]
pub enum MatchedContent<'a> {
    Text(&'a TextElement),
    Image(&'a ImageElement),
    None,
}

impl<'a> MatchedContent<'a> {
    pub fn id(&self) -> Uuid {
        match self {
            MatchedContent::Text(text_elem) => text_elem.id,
            MatchedContent::Image(image_elem) => image_elem.id,
            MatchedContent::None => Uuid::new_v4(),
        }
    }
}

impl<'a> TemplateContentMatch<'a> {
    pub fn new(template_element: &'a Element) -> Self {
        TemplateContentMatch {
            template_element,
            matched_content: Vec::new(),
            children: Vec::new(),
            metadata: HashMap::new(),
            section_boundaries: None,
        }
    }

    pub fn with_content(template_element: &'a Element, content: Vec<MatchedContent<'a>>) -> Self {
        TemplateContentMatch {
            template_element,
            matched_content: content,
            children: Vec::new(),
            metadata: HashMap::new(),
            section_boundaries: None,
        }
    }

    pub fn with_section_boundaries(
        template_element: &'a Element,
        start_marker: &'a PageContent,
        end_marker: Option<&'a PageContent>,
    ) -> Self {
        TemplateContentMatch {
            template_element,
            matched_content: Vec::new(),
            children: Vec::new(),
            metadata: HashMap::new(),
            section_boundaries: Some(SectionBoundaries {
                start_marker,
                end_marker,
            }),
        }
    }
}

/// Aligns template elements with document content sequentially
pub fn align_template_with_content<'a>(
    template_elements: &'a [Element],
    index: &'a PdfIndex,
    inherited_metadata: Option<&HashMap<String, Value>>,
    parent_or_prev_sibling_match_context: Option<&TemplateContentMatch<'a>>,
) -> Option<Vec<TemplateContentMatch<'a>>> {
    if template_elements.is_empty() {
        return None;
    }

    println!(
        "MATCHER: align_template_with_content called for {} elements. Context: {}",
        template_elements.len(),
        parent_or_prev_sibling_match_context.map_or("None", |m| m.template_element.name.as_str())
    );

    let default_metadata = HashMap::new();
    let actual_inherited_metadata = inherited_metadata.unwrap_or(&default_metadata);

    let mut elements_by_page_view: BTreeMap<u32, Vec<&'a PageContent>> = BTreeMap::new();
    for (page_num, element_indices) in index.by_page.iter() {
        let mut page_elements: Vec<&'a PageContent> = Vec::new();
        for &idx in element_indices {
            if let Some(element) = index.all_ordered_content.get(idx) {
                page_elements.push(element);
            }
        }
        if !page_elements.is_empty() {
            elements_by_page_view.insert(*page_num, page_elements);
        }
    }

    // Determine starting search position and constraints based on context
    let (start_search_index, max_content_boundary) =
        if let Some(context_match) = parent_or_prev_sibling_match_context {
            // Simple invariant: if context has section_boundaries, we're processing children
            // Otherwise, we're processing siblings
            if let Some(section_boundaries) = &context_match.section_boundaries {
                // Child elements are constrained to parent section boundaries
                let start_idx = index
                    .element_id_to_index
                    .get(&section_boundaries.start_marker.id())
                    .copied()
                    .unwrap_or(0);
                let end_idx = section_boundaries
                    .end_marker
                    .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
                    .unwrap_or(index.all_ordered_content.len());

                println!(
                    "MATCHER: Processing children within section boundaries {} to {}",
                    start_idx, end_idx
                );
                (start_idx, end_idx)
            } else {
                // Sibling elements start after previous match
                let sibling_start = get_next_match_index(Some(context_match), index);
                (sibling_start, index.all_ordered_content.len())
            }
        } else {
            (0, index.all_ordered_content.len())
        };

    // TWO-PASS ALGORITHM:

    // PASS 1: Find all Section boundaries to partition content space
    let mut section_matches = Vec::new();
    let mut content_partitions = Vec::new(); // (start_idx, end_idx)

    for template_element in template_elements {
        if template_element.element_type == ElementType::Section {
            if let Some(section_match) = match_section(
                template_element,
                index,
                &elements_by_page_view,
                actual_inherited_metadata,
                section_matches.last(),
                start_search_index,
            ) {
                println!(
                    "  PASS 1: Found Section '{}' boundaries",
                    template_element.name
                );

                // Extract partition boundaries
                if let Some(boundaries) = &section_match.section_boundaries {
                    let start_idx = index
                        .element_id_to_index
                        .get(&boundaries.start_marker.id())
                        .copied()
                        .unwrap_or(0);
                    let end_idx = boundaries
                        .end_marker
                        .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
                        .unwrap_or(max_content_boundary); // Use max_content_boundary instead of full document
                                                          // The section partition includes content UP TO but NOT INCLUDING the end marker
                                                          // TextChunks after sections should start AFTER the end marker
                    content_partitions.push((start_idx, end_idx));
                }

                section_matches.push(section_match);
            }
        }
    }

    // PASS 2: Assign TextChunks to appropriate content partitions
    let mut textchunk_matches = Vec::new();
    let mut partition_cursor = 0; // Which partition we're currently processing

    for (template_idx, template_element) in template_elements.iter().enumerate() {
        if template_element.element_type == ElementType::TextChunk {
            println!(
                "  PASS 2: Processing TextChunk '{}' (template order: {})",
                template_element.name, template_idx
            );

            // Determine which content partition this TextChunk should process
            let (content_start, content_end) = if content_partitions.is_empty() {
                // No sections found, process all content within constraints
                (start_search_index, max_content_boundary)
            } else {
                // Check if this TextChunk comes before the first section
                let first_section_template_idx = template_elements
                    .iter()
                    .position(|e| e.element_type == ElementType::Section);

                if let Some(first_section_idx) = first_section_template_idx {
                    if template_idx < first_section_idx {
                        // TextChunk comes before first section - process content before first section
                        let first_partition_start = content_partitions[0].0;
                        println!(
                            "    TextChunk '{}' processes content BEFORE first section: {} to {}",
                            template_element.name, start_search_index, first_partition_start
                        );
                        (start_search_index, first_partition_start)
                    } else {
                        // TextChunk comes after sections - process content after last section
                        let last_partition_end = content_partitions
                            .last()
                            .map(|(_, end)| *end)
                            .unwrap_or(max_content_boundary);
                        // Start after the section end marker (partition end is the end marker index)
                        let content_start_after_section =
                            if last_partition_end < max_content_boundary {
                                last_partition_end + 1 // Start after the end marker
                            } else {
                                last_partition_end
                            };
                        println!(
                            "    TextChunk '{}' processes content AFTER sections: {} to {}",
                            template_element.name,
                            content_start_after_section,
                            max_content_boundary
                        );
                        (content_start_after_section, max_content_boundary)
                    }
                } else {
                    // No sections in template (shouldn't happen if we have partitions, but fallback)
                    (start_search_index, max_content_boundary)
                }
            };

            // Match TextChunk with determined content boundaries
            if let Some(textchunk_match) = match_text_chunk_with_boundaries(
                template_element,
                index,
                actual_inherited_metadata,
                content_start,
                content_end,
            ) {
                println!("    SUCCESS: Matched TextChunk '{}'", template_element.name);
                textchunk_matches.push(textchunk_match);
            } else {
                println!(
                    "    FAILURE: No match for TextChunk '{}'",
                    template_element.name
                );
            }
        }
    }

    // Combine results maintaining original template order
    let mut all_results = Vec::new();
    for template_element in template_elements {
        if template_element.element_type == ElementType::Section {
            if let Some(section_match) = section_matches
                .iter()
                .find(|m| std::ptr::eq(m.template_element, template_element))
            {
                all_results.push(section_match.clone());
            }
        } else if template_element.element_type == ElementType::TextChunk {
            if let Some(textchunk_match) = textchunk_matches
                .iter()
                .find(|m| std::ptr::eq(m.template_element, template_element))
            {
                all_results.push(textchunk_match.clone());
            }
        }
    }

    if all_results.is_empty() {
        None
    } else {
        Some(all_results)
    }
}

/// Represents a potential section boundary with scoring information
#[derive(Debug, Clone)]
struct BoundaryCandidate<'a> {
    content: &'a PageContent,
    score: f32,
    reasons: Vec<String>,
}

/// Represents the flow of content between elements
#[derive(Debug)]
struct ContentFlow<'a> {
    elements: Vec<&'a PageContent>,
    relationships: Vec<(usize, usize, RelationshipType)>,
}

#[derive(Debug)]
enum RelationshipType {
    Before,
    After,
    Contains,
    ReferencedBy,
}

/// Finds section match that comes after prev_match
fn match_section<'a, 'map_lt>(
    template: &'a Element,
    index: &'a PdfIndex,
    page_map_view: &'map_lt BTreeMap<u32, Vec<&'a PageContent>>,
    inherited_metadata: &HashMap<String, Value>,
    prev_match_for_context: Option<&TemplateContentMatch<'a>>,
    current_search_start_index: usize,
) -> Option<TemplateContentMatch<'a>> {
    let match_config = template.attributes.get("match")?.as_match_config()?;

    let effective_search_start_index = current_search_start_index;
    println!(
        "[match_section] For '{}', using effective_search_start_index: {}. Prev_match_context: {}",
        template.name,
        effective_search_start_index,
        prev_match_for_context.map_or("None", |m| m.template_element.name.as_str())
    );

    // 1. Find start boundary candidates
    let start_candidates = find_start_boundary_candidates(
        template,
        index,
        effective_search_start_index,
        &match_config,
        prev_match_for_context,
    )?;

    let selected_start_candidate = start_candidates.first()?;
    let start_marker: &'a PageContent = selected_start_candidate.content;

    // 2. Find end boundary candidates
    let end_candidates = find_end_boundary_candidates(
        start_marker, // Use the &'a PageContent from index
        template,
        index,
        &template.children,
        &match_config, // Pass the match_config for consistent threshold handling
        prev_match_for_context,
    )?;

    let selected_end_candidate = end_candidates.first()?;
    let end_marker_option: Option<&'a PageContent> = Some(selected_end_candidate.content);

    let section_content_elements = extract_section_content(
        page_map_view,
        start_marker.page_number(), // page_number from &'a PageContent is fine
        start_marker,               // Pass &'a PageContent from index
        end_marker_option,          // Pass Option<&'a PageContent> from index
        index,
    );

    // Create section match
    let mut result = TemplateContentMatch::with_section_boundaries(
        template,
        start_marker,      // This is &'a PageContent from index
        end_marker_option, // This is Option<&'a PageContent> from index
    );

    // Add the extracted content as matched content
    if !section_content_elements.is_empty() {
        result.matched_content = section_content_elements
            .iter()
            .map(|&content| match content {
                // content is &'a PageContent
                PageContent::Text(text) => MatchedContent::Text(text),
                PageContent::Image(image) => MatchedContent::Image(image),
            })
            .collect();
    }

    result.metadata = inherited_metadata.clone();

    // Add section-specific metadata
    if let Some(as_value) = template.attributes.get("as") {
        result
            .metadata
            .insert("section".to_string(), as_value.clone());
    }

    // Add the section name as well for reference
    result.metadata.insert(
        "section_name".to_string(),
        Value::String(template.name.clone()),
    );

    // Handle child elements
    if !template.children.is_empty() {
        println!(
            "[match_section] Parent '{}' has {} children in template.",
            template.name,
            template.children.len()
        );
        if let Some(child_matches) = align_template_with_content(
            &template.children,
            index,
            Some(&result.metadata), // Pass the updated metadata including section info
            Some(&result),
        ) {
            println!("[match_section] Parent '{}' got Some(child_matches) with len: {}. Assigning to result.children.", template.name, child_matches.len());
            result.children = child_matches;
        } else {
            println!("[match_section] Parent '{}' got None for child_matches. result.children will be empty.", template.name);
        }
        println!(
            "[match_section] After child processing, Parent '{}' result.children.len is now: {}",
            template.name,
            result.children.len()
        );
    } else {
        println!(
            "[match_section] Parent '{}' has no children in template.",
            template.name
        );
    }

    Some(result)
}

/// Finds potential start boundary candidates using multiple indices
fn find_start_boundary_candidates<'a>(
    template: &Element,
    index: &'a PdfIndex,
    start_index: usize,
    match_config: &MatchConfig,
    prev_match: Option<&TemplateContentMatch<'a>>,
) -> Option<Vec<BoundaryCandidate<'a>>> {
    let mut candidates = Vec::new();

    println!("[find_start_boundary_candidates] Template: {}, Match pattern: '{}', Threshold: {}, Start index: {}", template.name, match_config.pattern, match_config.threshold, start_index);
    // 1. Text-based candidates
    let text_candidates = index.find_text_matches(
        &match_config.pattern,
        match_config.threshold,
        Some(start_index),
    );
    println!(
        "[find_start_boundary_candidates] Text-based candidates found: {}",
        text_candidates.len()
    );

    for (element, score) in text_candidates {
        candidates.push(score_candidate(
            &element, index, template, score, prev_match,
        ));
    }

    // Will use these later
    // let font_candidates =
    //     index.elements_by_font(element.as_text().unwrap().font_id, None, None, None);

    // for element in font_candidates {
    //     candidates.push(score_candidate(
    //         element.clone(),
    //         index,
    //         template,
    //         0.0,
    //         prev_match,
    //     ));
    // }

    // // 3. Spatial candidates (elements at top of page)
    // for (page_num, elements) in index.by_page.iter() {
    //     if let Some(first_element) = elements.first() {
    //         if let Some(element) = index.elements.get(*first_element) {
    //             candidates.push(score_candidate(
    //                 element.clone(),
    //                 index,
    //                 template,
    //                 0.0,
    //                 prev_match,
    //             ));
    //         }
    //     }
    // }

    if candidates.is_empty() {
        println!("[find_start_boundary_candidates] No candidates found. Returning None.");
        None
    } else {
        candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
        println!(
            "[find_start_boundary_candidates] Returning {} sorted candidates.",
            candidates.len()
        );
        Some(candidates)
    }
}

/// Finds potential end boundary candidates
/// Returns a list of candidates sorted by score
fn find_end_boundary_candidates<'a>(
    start_content: &PageContent,
    template: &Element,
    index: &'a PdfIndex,
    children: &[Element],
    match_config: &MatchConfig,
    prev_match: Option<&TemplateContentMatch<'a>>,
) -> Option<Vec<BoundaryCandidate<'a>>> {
    println!(
        "[find_end_boundary_candidates] Looking for end for section starting with: {:?}",
        start_content.text()
    );
    println!(
        "[find_end_boundary_candidates] Template name: '{}', attributes: {:?}",
        template.name, template.attributes
    );
    let mut candidates = Vec::new();

    // Get the start marker's index so we can search after it
    let start_marker_index = index.element_id_to_index.get(&start_content.id()).copied();
    println!(
        "[find_end_boundary_candidates] Start marker index: {:?}",
        start_marker_index
    );

    // 1. Template-based end markers
    if let Some(end_match_attr) = template.attributes.get("end_match") {
        if let Some(end_str) = end_match_attr.as_string() {
            println!(
                "[find_end_boundary_candidates] Using end_match attribute: '{}', threshold: {}",
                end_str, match_config.threshold
            );

            // Start search after the start marker, not from the beginning of the document
            let search_start_index = start_marker_index.map(|idx| idx + 1);
            println!("[find_end_boundary_candidates] Searching for end markers starting from index: {:?}", search_start_index);

            let end_text_candidates = index.find_text_matches(
                &end_str,
                match_config.threshold,
                search_start_index, // Use match_config.threshold instead of hardcoded value
            );
            println!(
                "[find_end_boundary_candidates] Found {} text candidates for end_match.",
                end_text_candidates.len()
            );
            for (element, score) in end_text_candidates {
                candidates.push(score_candidate(
                    &element, index, template, score, prev_match,
                ));
            }
        } else {
            println!(
                "[find_end_boundary_candidates] end_match attribute found but not a string value."
            );
        }
    } else {
        println!("[find_end_boundary_candidates] No 'end_match' attribute key found in template attributes.");
        // If no end_match is specified, we might want a default behavior,
        // for example, consider all elements after start_content on the same page,
        // or up to the start of a *next* identifiable section if one exists soon.
        // For now, if no end_match, it will likely result in candidates being empty.
    }

    // 2. Natural boundaries (Currently Commented Out)
    // println!("[find_end_boundary_candidates] Considering natural boundaries...");
    // candidates.extend(find_natural_boundaries(start_content, index, children));

    // 3. Filter based on child elements (Currently Commented Out)
    // println!("[find_end_boundary_candidates] Validating boundary candidates based on children...");
    // candidates = validate_boundary_candidates(&candidates, children, index);

    if candidates.is_empty() {
        println!("[find_end_boundary_candidates] No end candidates found. Returning None.");
        None
    } else {
        candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
        println!(
            "[find_end_boundary_candidates] Returning {} sorted end candidates.",
            candidates.len()
        );
        Some(candidates)
    }
}

/// Scores a potential boundary candidate
fn score_candidate<'a>(
    content: &'a PageContent,
    index: &PdfIndex,
    template: &Element,
    base_score: f64,
    prev_match: Option<&TemplateContentMatch<'a>>,
) -> BoundaryCandidate<'a> {
    let mut score = base_score as f32;
    let mut reasons = Vec::new();

    // Consider previous match if available
    if let Some(prev) = prev_match {
        if let Some(sb) = prev.section_boundaries.as_ref() {
            if let Some(end) = sb.end_marker {
                match content {
                    PageContent::Text(text) => {
                        // Check if this candidate comes after the previous section
                        if let Some(idx) = index.element_id_to_index.get(&text.id) {
                            if let Some(prev_idx) = index.element_id_to_index.get(&end.id()) {
                                if idx > prev_idx {
                                    score += 0.3;
                                    reasons.push("Follows previous section".to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    match content {
        PageContent::Text(text) => {
            // Font size scoring using statistical analysis
            let stats = index.font_size_stats();
            let z_score = (text.font_size - stats.mean) / stats.std_dev;

            if z_score > 1.5 {
                score += 0.3;
                reasons.push("Statistically significant font size".to_string());
            }

            // Position scoring
            let y_pos = text.bbox.1;
            if y_pos < 100.0 {
                score += 0.2;
                reasons.push("Top of page".to_string());
            }

            // Reference count scoring
            if let Some(element_idx) = index.element_id_to_index.get(&text.id) {
                if let Some(&(count, _)) = index
                    .reference_count_index
                    .iter()
                    .find(|&&(_, idx)| idx == *element_idx)
                {
                    if count > 0 {
                        score += 0.2 * (count as f32).min(5.0) / 5.0;
                        reasons.push("Referenced element".to_string());
                    }
                }
            }
        }
        PageContent::Image(_) => {
            // Image-specific scoring
            score += 0.4;
            reasons.push("Image content".to_string());
        }
    }

    BoundaryCandidate {
        content,
        score,
        reasons,
    }
}

/// Finds natural boundaries based on content changes
// fn find_natural_boundaries<'a>(
//     start_content: &'a PageContent,
//     index: &'a PdfIndex,
//     children: &[Element],
// ) -> Vec<BoundaryCandidate> {
//     let mut candidates = Vec::new();

//     match start_content {
//         PageContent::Text(start_text) => {
//             // Find font changes
//             let avg_font_size = calculate_average_font_size(index);
//             let font_candidates =
//                 index.elements_by_font(None, Some(avg_font_size * 1.2), None, Some(1));

//             for element in font_candidates {
//                 candidates.push(score_candidate(
//                     element.clone(),
//                     index,
//                     &Element::new("Section".to_string(), ElementType::Section),
//                     0.0,
//                     None,
//                 ));
//             }
//         }
//         PageContent::Image(_) => (),
//     }

//     candidates
// }

/// Validates boundary candidates based on child element requirements
// fn validate_boundary_candidates<'a>(
//     candidates: &[BoundaryCandidate],
//     children: &[Element],
//     index: &PdfIndex,
// ) -> Vec<BoundaryCandidate> {
//     // Build content flow graph
//     let flow = build_content_flow(candidates, children, index);

//     // Filter candidates based on child element requirements
//     candidates
//         .iter()
//         .filter(|candidate| {
//             // Check if candidate respects child element positions
//             children
//                 .iter()
//                 .all(|child| validate_child_position(child, candidate, &flow))
//         })
//         .cloned()
//         .collect()
// }

/// Builds a graph of content relationships
// fn build_content_flow<'a>(
//     candidates: &[BoundaryCandidate],
//     children: &[Element],
//     index: &PdfIndex,
// ) -> ContentFlow<'a> {
//     let mut elements = Vec::new();
//     let mut relationships = Vec::new();

//     // Add all candidate elements
//     for candidate in candidates {
//         elements.push(candidate.content);
//     }

//     // Build relationships
//     for (i, elem1) in elements.iter().enumerate() {
//         for (j, elem2) in elements.iter().enumerate() {
//             if i != j {
//                 if let (PageContent::Text(t1), PageContent::Text(t2)) = (elem1, elem2) {
//                     // Check if t2 comes after t1
//                     if let (Some(idx1), Some(idx2)) = (
//                         index.element_id_to_index.get(&t1.id),
//                         index.element_id_to_index.get(&t2.id),
//                     ) {
//                         if idx2 > idx1 {
//                             relationships.push((i, j, RelationshipType::After));
//                         }
//                     }
//                 }
//             }
//         }
//     }

//     ContentFlow {
//         elements,
//         relationships,
//     }
// }

/// Validates if a candidate respects child element positions
fn validate_child_position<'a>(
    child: &Element,
    candidate: &BoundaryCandidate<'a>,
    flow: &ContentFlow<'a>,
) -> bool {
    // Implement child position validation logic
    // This is a placeholder - actual implementation would depend on specific requirements
    true
}

/// Selects the best boundary from candidates
fn select_best_boundary<'a>(
    candidates: Vec<BoundaryCandidate<'a>>,
    previous_content: Option<&PageContent>,
    children: &[Element],
    index: &PdfIndex,
) -> Option<PageContent> {
    candidates
        .into_iter()
        .map(|candidate| {
            let mut score = candidate.score;

            // Consider content type compatibility
            if let Some(prev) = &previous_content {
                if content_types_compatible(prev, candidate.content) {
                    score += 0.2;
                }
            }

            // Consider child element requirements
            if satisfies_child_requirements(&candidate, children, index) {
                score += 0.3;
            }

            // Consider document flow
            if let Some(prev) = &previous_content {
                if maintains_document_flow(prev, candidate.content, index) {
                    score += 0.2;
                }
            }

            (candidate, score)
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal))
        .map(|(candidate, _)| candidate.content.clone())
}

/// Checks if two content types are compatible
fn content_types_compatible(a: &PageContent, b: &PageContent) -> bool {
    match (a, b) {
        (PageContent::Text(_), PageContent::Text(_)) => true,
        (PageContent::Image(_), PageContent::Image(_)) => true,
        (PageContent::Image(_), _) | (_, PageContent::Image(_)) => false,
    }
}

/// Checks if a candidate satisfies child element requirements
fn satisfies_child_requirements<'a>(
    candidate: &BoundaryCandidate<'a>,
    children: &[Element],
    index: &PdfIndex,
) -> bool {
    // Implement child requirement validation
    // This is a placeholder - actual implementation would depend on specific requirements
    true
}

/// Checks if document flow is maintained between two content elements
fn maintains_document_flow<'a>(
    prev: &PageContent,
    current: &PageContent,
    index: &PdfIndex,
) -> bool {
    match (prev, current) {
        (PageContent::Text(t1), PageContent::Text(t2)) => {
            if let (Some(idx1), Some(idx2)) = (
                index.element_id_to_index.get(&t1.id),
                index.element_id_to_index.get(&t2.id),
            ) {
                idx2 > idx1
            } else {
                false
            }
        }
        _ => true,
    }
}

/// Matches a TextChunk element with explicit content boundaries
fn match_text_chunk_with_boundaries<'a>(
    template: &'a Element,
    index: &'a PdfIndex,
    inherited_metadata: &HashMap<String, Value>,
    content_start_idx: usize,
    content_end_idx: usize,
) -> Option<TemplateContentMatch<'a>> {
    println!(
        "[match_text_chunk_with_boundaries] Template: '{}', boundaries: {} to {}",
        template.name, content_start_idx, content_end_idx
    );

    // Extract content slice based on explicit boundaries
    let content_slice = if content_start_idx < content_end_idx
        && content_start_idx < index.all_ordered_content.len()
    {
        let end_idx = content_end_idx.min(index.all_ordered_content.len());
        &index.all_ordered_content[content_start_idx..end_idx]
    } else {
        &[]
    };

    println!(
        "[match_text_chunk_with_boundaries] Processing {} elements",
        content_slice.len()
    );

    let mut matched_content_for_chunk: Vec<MatchedContent<'a>> = Vec::new();
    let mut has_text_content = false;

    for pc_ref in content_slice {
        match pc_ref {
            PageContent::Text(text_elem_ref) => {
                matched_content_for_chunk.push(MatchedContent::Text(text_elem_ref));
                has_text_content = true;
            }
            PageContent::Image(_) => {
                // TextChunk specifically ignores images
            }
        }
    }

    if !has_text_content {
        println!("[match_text_chunk_with_boundaries] No text content found");
        return None;
    }

    let mut result = TemplateContentMatch::with_content(template, matched_content_for_chunk);
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
// pub fn select_best_match<'a>(
//     matched_elements: Vec<&'a TextElement>,
//     index: &'a PdfIndex,
// ) -> Option<Uuid> {
//     if matched_elements.is_empty() {
//         return None;
//     }

//     matched_elements
//         .into_iter()
//         .max_by(|a, b| {
//             let score_a = score_match_line(a, index);
//             let score_b = score_match_line(b, index);
//             score_a
//                 .partial_cmp(&score_b)
//                 .unwrap_or(std::cmp::Ordering::Equal)
//         })
//         .map(|best_element| best_element.id)
// }

// /// Scores a text line for matching quality
// fn score_match_line(line: &TextElement, index: &PdfIndex) -> f32 {
//     let mut score = 0.0;

//     let avg_font_size =
//         index.elements.iter().map(|e| e.font_size).sum::<f32>() / index.elements.len() as f32;

//     let font_size_score = ((line.font_size / avg_font_size) - 1.0).max(0.0).min(1.0);
//     score += font_size_score * 0.4;

//     let y_pos = line.bbox.1;
//     let position_score = if y_pos < 100.0 || y_pos > 700.0 {
//         1.0
//     } else {
//         0.3
//     };
//     score += position_score * 0.3;

//     let text = &line.text;
//     let case_score = if text.chars().all(|c| c.is_uppercase()) {
//         1.0
//     } else if text.chars().next().map_or(false, |c| c.is_uppercase()) {
//         0.8
//     } else {
//         0.3
//     };
//     score += case_score * 0.3;

//     if let Some(element_idx) = index.element_id_to_index.get(&line.id) {
//         let ref_count = index
//             .reference_count_index
//             .iter()
//             .find(|&&(_, idx)| idx == *element_idx)
//             .map(|&(count, _)| count)
//             .unwrap_or(0);

//         if ref_count > 0 {
//             score += 0.2 * (ref_count as f32).min(5.0) / 5.0;
//         }
//     }

//     score
// }

/// Extracts the content between a start element and an optional end element
pub fn extract_section_content<'a>(
    _page_map_view: &BTreeMap<u32, Vec<&'a PageContent>>,
    _start_marker_page_num: u32,
    start_marker: &'a PageContent,
    end_marker_option: Option<&'a PageContent>,
    index: &'a PdfIndex,
) -> Vec<&'a PageContent> {
    // Debugging output
    let start_debug_info = match start_marker {
        PageContent::Text(t) => format!("Text('{}', ID: {})", t.text, t.id),
        PageContent::Image(i) => format!("Image(ID: {})", i.id),
    };
    let end_debug_info = end_marker_option.map_or("None".to_string(), |em| match em {
        PageContent::Text(t) => format!("Text('{}', ID: {})", t.text, t.id),
        PageContent::Image(i) => format!("Image(ID: {})", i.id),
    });
    println!(
        "[extract_section_content] Start: {}, End: {}",
        start_debug_info, end_debug_info
    );

    // Now that PdfIndex::get_elements_between_markers handles all content types in order,
    // this function becomes a direct call.
    let collected_content = index.get_elements_between_markers(start_marker, end_marker_option);

    println!(
        "[extract_section_content] Total collected content items from index: {}",
        collected_content.len()
    );
    for (i, item) in collected_content.iter().enumerate() {
        let item_debug_info = match item {
            PageContent::Text(t) => format!("Text('{}', ID: {})", t.text, t.id),
            PageContent::Image(i) => format!("Image(ID: {})", i.id),
        };
        println!("  - Item {}: {}", i, item_debug_info);
    }

    collected_content
}

// Add basic implementations for Table and Image matchers
// fn match_table<'a, 'map_lt>(
//     template: &'a Element,
//     _page_map_view: &'map_lt BTreeMap<u32, Vec<&'a PageContent>>,
//     inherited_metadata: &HashMap<String, Value>,
// ) -> Option<TemplateContentMatch<'a>> {
//     println!("MATCHER: Processing Table template element");

//     let _match_config = template.attributes.get("match")?.as_match_config()?;

//     let table_indicators = ["table", "column", "row", "|", "total"];

//     let potential_table_elements: Vec<&'a PageContent> = _page_map_view
//         .values()
//         .flatten()
//         .copied()
//         .filter(|element| {
//             let text = element.text().unwrap_or("").to_lowercase();
//             table_indicators
//                 .iter()
//                 .any(|indicator| text.contains(indicator))
//                 || text.contains("|")
//                 || (text.chars().filter(|c| *c == ' ').count() > 5)
//         })
//         .collect();

//     if !potential_table_elements.is_empty() {
//         let start_marker = potential_table_elements.first().copied()?;
//         let end_marker = potential_table_elements.last().copied();

//         println!(
//             "MATCHER: Found potential table starting with element: {:?}",
//             start_marker.text()
//         );

//         let table_content: Vec<&PageContent> = Vec::new();

//         let mut result = TemplateContentMatch::with_content(
//             template,
//             MatchedContent::Section {
//                 start_marker,
//                 end_marker,
//                 content: table_content,
//             },
//         );
//         result.metadata = inherited_metadata.clone();
//         event!(
//             Level::DEBUG,
//             target = TEMPLATE_MATCH,
//             template_id = %Uuid::new_v4(),
//             content_id = %start_marker.id,
//             template_name = %template.name,
//             score = 0.8,
//             "Table template matched content (placeholder)"
//         );
//         return Some(result);
//     }

//     None
// }

// fn match_image<'a>(
//     template: &'a Element,
//     index: &'a PdfIndex,
//     inherited_metadata: &HashMap<String, Value>,
//     start_image_index: usize,
// ) -> Option<TemplateContentMatch<'a>> {
//     println!(
//         "MATCHER: Processing Image template element, starting search from index {}",
//         start_image_index
//     );

//     index.images.get(start_image_index).map(|image_elem| {
//         println!("MATCHER: Found image with ID {}", image_elem.id);
//         let mut result =
//             TemplateContentMatch::with_content(template, MatchedContent::Image(image_elem.clone()));
//         result.metadata = inherited_metadata.clone();
//         event!(
//             Level::DEBUG,
//             target = TEMPLATE_MATCH,
//             template_id = %Uuid::new_v4(),
//             content_id = %image_elem.id,
//             template_name = %template.name,
//             score = 0.9,
//             "Image template matched content"
//         );
//         result
//     })
// }

/// Gets the next element index to start matching from
fn get_next_match_index<'a>(
    prev_match: Option<&TemplateContentMatch<'a>>,
    index: &'a PdfIndex,
) -> usize {
    // If no previous match, start from beginning
    let Some(prev) = prev_match else { return 0 };

    // Find the last element we processed in the previous match
    if let Some(last_content) = prev.matched_content.last() {
        match last_content {
            MatchedContent::Text(text) => {
                // Start after the last text element
                index
                    .element_id_to_index
                    .get(&text.id)
                    .map_or(0, |&idx| idx + 1)
            }
            MatchedContent::Image(_) | MatchedContent::None => {
                // If last element was an image or None, use section boundaries
                prev.section_boundaries
                    .as_ref()
                    .and_then(|sb| {
                        sb.end_marker
                            .or(Some(sb.start_marker))
                            .and_then(|elem| match elem {
                                PageContent::Text(text) => {
                                    index.element_id_to_index.get(&text.id).copied()
                                }
                                PageContent::Image(_) => None,
                            })
                    })
                    .map_or(0, |idx| idx + 1)
            }
        }
    } else {
        // If no matched content, use section boundaries
        prev.section_boundaries
            .as_ref()
            .and_then(|sb| {
                sb.end_marker
                    .or(Some(sb.start_marker))
                    .and_then(|elem| match elem {
                        PageContent::Text(text) => index.element_id_to_index.get(&text.id).copied(),
                        PageContent::Image(_) => None,
                    })
            })
            .map_or(0, |idx| idx + 1)
    }
}

/// Gets the next image index to start matching from
fn get_next_image_index<'a>(
    prev_match: Option<&TemplateContentMatch<'a>>,
    index: &'a PdfIndex,
) -> usize {
    let Some(prev) = prev_match else { return 0 };

    prev.matched_content
        .iter()
        .filter_map(|content| {
            if let MatchedContent::Image(img_elem_ref) = content {
                // Now use element_id_to_index as image_id_to_index was removed from PdfIndex
                // and all_ordered_content is indexed by element_id_to_index.
                // We need the ID of the PageContent::Image that contains this ImageElement
                // This requires a bit of a search or a change in how MatchedContent::Image stores info.
                // For now, assuming MatchedContent::Image still gives direct ImageElement & its ID is the PageContent ID.
                index.element_id_to_index.get(&img_elem_ref.id).copied()
            } else {
                None
            }
        })
        .last() // Get the last image found in the previous match
        .map_or(0, |idx| idx + 1) // Start after it, or from 0 if no image was found
}
