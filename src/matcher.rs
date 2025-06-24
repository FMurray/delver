//! # Template Matching Engine
//!
//! This module implements the core template matching algorithm that aligns template elements
//! with document content. It uses a two-pass algorithm to handle sections and text chunks
//! efficiently while maintaining document order and spatial relationships.

use crate::dom::{Element, ElementType, MatchConfig, MatchType, Value};
use crate::layout::{group_text_into_lines, TextBlock, TextLine};
use crate::logging::TEMPLATE_MATCH;
use crate::parse::{ContentHandle, ImageElement, PageContent, TextElement};
use crate::search_index::PdfIndex;
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use strsim::normalized_levenshtein;
use tracing::{event, warn, Level};
use uuid::Uuid;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

/// Maximum recursion depth to prevent runaway recursion during template matching
const MAX_RECURSION_DEPTH: usize = 10;

/// Global counters for preventing infinite loops during matching operations
static MATCH_SECTION_CALLS: AtomicUsize = AtomicUsize::new(0);
static ALIGN_TEMPLATE_CALLS: AtomicUsize = AtomicUsize::new(0);
static FIND_START_BOUNDARY_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Progress tracking structure for template matching operations
#[derive(Debug, Clone)]
struct MatchingProgress {
    start_search_index: usize,
    template_name: String,
    call_count: usize,
    visited_positions: std::collections::HashSet<usize>,
}

/// Represents a successful match between a template element and document content
///
/// This structure maintains the relationship between template elements and their
/// matched content, including hierarchical relationships and metadata.
#[derive(Debug, Clone)]
pub struct TemplateContentMatch<'a> {
    /// Reference to the template element that was matched
    pub template_element: &'a Element,
    /// Content that was matched to this template element
    pub matched_content: Vec<MatchedContent>,
    /// Child matches for nested template elements
    pub children: Vec<TemplateContentMatch<'a>>,
    /// Metadata extracted during the matching process
    pub metadata: HashMap<String, Value>,
    /// Section boundaries for Section-type elements
    pub section_boundaries: Option<SectionBoundaries>,
}

/// Defines the start and end boundaries of a document section
#[derive(Debug, Clone)]
pub struct SectionBoundaries {
    /// The content element that marks the start of the section
    pub start_marker: PageContent,
    /// Optional content element that marks the end of the section
    pub end_marker: Option<PageContent>,
}

/// Represents matched content that can be resolved through the document index
#[derive(Debug, Clone)]
pub enum MatchedContent {
    /// Document-order index that can be resolved through PdfIndex
    Index(usize),
    /// No content matched
    None,
}

impl MatchedContent {
    pub fn id(&self, index: &PdfIndex) -> Option<Uuid> {
        match self {
            MatchedContent::Index(doc_idx) => {
                index.content_at(*doc_idx).map(|content| content.id())
            }
            MatchedContent::None => None,
        }
    }
    
    /// Get the actual content by resolving the index through the PdfIndex
    pub fn resolve<'a>(&self, index: &'a PdfIndex) -> Option<PageContent> {
        match self {
            MatchedContent::Index(doc_idx) => index.content_at(*doc_idx),
            MatchedContent::None => None,
        }
    }
    
    /// Check if this represents text content without materializing it
    pub fn is_text(&self, index: &PdfIndex) -> bool {
        match self {
            MatchedContent::Index(doc_idx) => {
                if let Some(handle) = index.get_handle(*doc_idx) {
                    matches!(handle, ContentHandle::Text(_))
                } else {
                    false
                }
            }
            MatchedContent::None => false,
        }
    }
    
    /// Check if this represents image content without materializing it
    pub fn is_image(&self, index: &PdfIndex) -> bool {
        match self {
            MatchedContent::Index(doc_idx) => {
                if let Some(handle) = index.get_handle(*doc_idx) {
                    matches!(handle, ContentHandle::Image(_))
                } else {
                    false
                }
            }
            MatchedContent::None => false,
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

    pub fn with_content(template_element: &'a Element, content: Vec<MatchedContent>) -> Self {
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
        start_marker: PageContent,
        end_marker: Option<PageContent>,
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

/// Aligns template elements with document content using a two-pass algorithm
///
/// This is the main entry point for template matching. It uses a sophisticated two-pass
/// algorithm that first identifies section boundaries, then processes text chunks within
/// those boundaries while maintaining document order.
///
/// # Arguments
/// * `template_elements` - The template elements to match against the document
/// * `index` - The document index containing searchable content
/// * `inherited_metadata` - Metadata inherited from parent template elements
/// * `parent_or_prev_sibling_match_context` - Context from previous matches for positioning
///
/// # Returns
/// * `Some(Vec<TemplateContentMatch>)` if any matches were found
/// * `None` if no matches were found
pub fn align_template_with_content<'a>(
    template_elements: &'a [Element],
    index: &'a PdfIndex,
    inherited_metadata: Option<&HashMap<String, Value>>,
    parent_or_prev_sibling_match_context: Option<&TemplateContentMatch<'a>>,
) -> Option<Vec<TemplateContentMatch<'a>>> {
    align_template_with_content_with_depth(
        template_elements,
        index,
        inherited_metadata,
        parent_or_prev_sibling_match_context,
        0,
    )
}

/// Internal function that tracks recursion depth
fn align_template_with_content_with_depth<'a>(
    template_elements: &'a [Element],
    index: &'a PdfIndex,
    inherited_metadata: Option<&HashMap<String, Value>>,
    parent_or_prev_sibling_match_context: Option<&TemplateContentMatch<'a>>,
    recursion_depth: usize,
) -> Option<Vec<TemplateContentMatch<'a>>> {
    // Increment and check call counter for infinite loop detection
    let call_count = ALIGN_TEMPLATE_CALLS.fetch_add(1, AtomicOrdering::SeqCst);
    if call_count > 10000 {
        panic!("Infinite loop detected: align_template_with_content called {} times", call_count);
    }

    if template_elements.is_empty() {
        return None;
    }

    // Recursion depth guard
    if recursion_depth > MAX_RECURSION_DEPTH {
        warn!(
            "Recursion depth limit ({}) exceeded, terminating template matching to prevent runaway recursion",
            MAX_RECURSION_DEPTH
        );
        return None;
    }



    let default_metadata = HashMap::new();
    let actual_inherited_metadata = inherited_metadata.unwrap_or(&default_metadata);

    let mut elements_by_page_view: BTreeMap<u32, Vec<PageContent>> = BTreeMap::new();
    for (page_num, page_elements) in index.by_page.iter() {
        let page_content = index.elements_on_page(*page_num);
        if !page_content.is_empty() {
            elements_by_page_view.insert(*page_num, page_content);
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
                    .as_ref()
                    .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
                    .unwrap_or(index.doc_len());


                (start_idx, end_idx)
            } else {
                // Sibling elements start after previous match
                let sibling_start = get_next_match_index(Some(context_match), index);
                (sibling_start, index.doc_len())
            }
        } else {
            (0, index.doc_len())
        };

    // TWO-PASS ALGORITHM:

    // PASS 1: Find all Section boundaries to partition content space
    let mut section_matches = Vec::new();
    let mut content_partitions = Vec::new(); // (start_idx, end_idx)
    let mut last_section_end_index: Option<usize> = None; // Track position progression

    for template_element in template_elements {
        if template_element.element_type == ElementType::Section {
            // Determine which context to pass: previous sibling for most sections,
            // but for the *last* section pass the parent so it can inherit the
            // parent's end boundary when it has none of its own.
            let is_last_section = template_elements
                .iter()
                .skip_while(|e| !std::ptr::eq(*e, template_element)) // slice from current
                .skip(1)                                             // look ahead
                .find(|e| e.element_type == ElementType::Section)
                .is_none();

            let context_for_child = if is_last_section {
                parent_or_prev_sibling_match_context
            } else {
                section_matches.last()
            };

            // Ensure we start search after the previous section to prevent infinite loops
            let mut effective_start_index = if let Some(prev_section) = section_matches.last() {
                if let Some(boundaries) = &prev_section.section_boundaries {
                    // If the previous section has an end marker, start from that end marker
                    // Otherwise, start after the previous section's start marker
                    if let Some(end_marker) = &boundaries.end_marker {
                        index.element_id_to_index.get(&end_marker.id()).copied().unwrap_or(start_search_index)
                    } else {
                        index.element_id_to_index.get(&boundaries.start_marker.id()).copied()
                            .map(|idx| idx + 1)
                            .unwrap_or(start_search_index)
                    }
                } else {
                    start_search_index
                }
            } else {
                start_search_index
            };
            
            // Allow the section to start after the previous section's end marker
            // Often Siblings are adjacent
            if let Some(prev_end) = last_section_end_index {
                effective_start_index = effective_start_index.max(prev_end);
            }

            // ─── Guard: we must always advance the scan window ─────────────
            if let Some(prev_end) = last_section_end_index {
                debug_assert!(
                    effective_start_index >= prev_end,
                    "effective_start_index {} < last_section_end_index {} — re‑matching same slice!",
                    effective_start_index,
                    prev_end
                );
            }



            if let Some(section_match) = match_section(
                template_element,
                index,
                &elements_by_page_view,
                actual_inherited_metadata,
                context_for_child,
                effective_start_index,
                recursion_depth,
            ) {


                // Extract partition boundaries and update position tracking
                if let Some(boundaries) = &section_match.section_boundaries {
                    let start_idx = index
                        .element_id_to_index
                        .get(&boundaries.start_marker.id())
                        .copied()
                        .unwrap_or(0);
                    let end_idx = boundaries
                        .end_marker
                        .as_ref()
                        .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
                        .unwrap_or(max_content_boundary); // Use max_content_boundary instead of full document
                                                          // The section partition includes content UP TO but NOT INCLUDING the end marker
                                                          // TextChunks after sections should start AFTER the end marker
                    content_partitions.push((start_idx, end_idx));
                    
                    // Update position tracking to prevent backtracking
                    last_section_end_index = Some(end_idx);
                }

                section_matches.push(section_match);
            }
        }
    }

    // PASS 2: Assign TextChunks to appropriate content partitions
    let mut textchunk_matches = Vec::new();

    for (template_idx, template_element) in template_elements.iter().enumerate() {
        if template_element.element_type == ElementType::TextChunk {


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

                textchunk_matches.push(textchunk_match);
            } else {

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
struct BoundaryCandidate {
    content: PageContent,
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
    page_map_view: &'map_lt BTreeMap<u32, Vec<PageContent>>,
    inherited_metadata: &HashMap<String, Value>,
    prev_match_for_context: Option<&TemplateContentMatch<'a>>,
    current_search_start_index: usize,
    recursion_depth: usize,
) -> Option<TemplateContentMatch<'a>> {
    let match_config = template.attributes.get("match")?.as_match_config()?;

    let effective_search_start_index = current_search_start_index;

    
    // ------------------------------------------------------------------
    // Limit the start‑marker text search to the parent section’s boundary,
    // if we are inside a parent.  This prevents a child from matching text
    // that actually belongs to the next sibling or further in the doc.
    let max_search_index = prev_match_for_context
        .and_then(|parent| parent.section_boundaries.as_ref())
        .and_then(|sb| sb.end_marker.as_ref())
        .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
        .filter(|&end_idx| end_idx > effective_search_start_index);


    // 1. Find start boundary candidates
    let start_candidates = find_start_boundary_candidates(
        template,
        index,
        effective_search_start_index,
        &match_config,
        prev_match_for_context,
        max_search_index,
    )?;

    let selected_start_candidate = start_candidates.first()?.clone();
    let start_marker: &PageContent = &selected_start_candidate.content;

    // 2. Find end boundary candidates
    let end_candidates_opt = find_end_boundary_candidates(
        start_marker, // Use the PageContent from candidates
        template,
        index,
        &template.children,
        &match_config, // Pass the match_config for consistent threshold handling
        prev_match_for_context,
    );



    // Choose end marker:
    //   a) the best explicit/style candidate, OR
    //   b) if none, and we are nested inside a parent section,
    //      fall back to the parent's end marker (so the last child
    //      runs until the parent boundary).
    //   c) if still none, use document end or next natural boundary
    let end_marker_option: Option<&PageContent> = end_candidates_opt
        .as_ref()
        .and_then(|v| v.first())
        .map(|c| &c.content)
        .or_else(|| {
            prev_match_for_context
                .and_then(|parent| parent.section_boundaries.as_ref())
                .and_then(|sb| sb.end_marker.as_ref())
                .and_then(|end| {
                    let start_idx = index.element_id_to_index[&start_marker.id()];
                    let end_idx = index.element_id_to_index[&end.id()];
                    (end_idx > start_idx).then_some(end)
                })
        });
    
    // If we still have no end marker, create a virtual end marker at document end
    // or find the next significant element that could serve as a natural boundary
    let end_marker_final = if let Some(end_marker) = end_marker_option {
        Some(end_marker.clone())
    } else {
        // Try to find the next element that could serve as a natural boundary
        let start_idx = index.element_id_to_index.get(&start_marker.id()).copied().unwrap_or(0);
        let next_boundary_idx = start_idx + 1;
        
        // Look for the next element that has similar characteristics to the start marker
        if let Some(next_content) = index.content_slice(next_boundary_idx, index.doc_len()).first() {
            if let PageContent::Text(next_text) = next_content {
                if let PageContent::Text(start_text) = start_marker {
                    // If next element has similar font size, use it as boundary
                    if (next_text.font_size - start_text.font_size).abs() < 2.0 {
                        Some(next_content.clone())
                    } else {
                        None // No similar element found, section extends to document end
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None // No more content, section extends to document end
        }
    };

    let section_content_handles = extract_section_content_handles(
        page_map_view,
        start_marker.page_number(), // page_number from &'a PageContent is fine
        start_marker,
        end_marker_final.as_ref(),
        index,
    );

    debug_assert!(
        !(section_content_handles.is_empty() && template.element_type == ElementType::Section),
        "Section {} produced empty handle slice: start {:?} end {:?}",
        template.name, start_marker, end_marker_final
    );
    

    // Calculate indices before moving end_marker_final
    let start_idx = index.element_id_to_index.get(&start_marker.id()).copied().unwrap_or(0);
    let end_idx = end_marker_final
        .as_ref()
        .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
        .unwrap_or(index.doc_len());

    // Create section match
    let mut result = TemplateContentMatch::with_section_boundaries(
        template,
        start_marker.clone(),       // Clone for storage
        end_marker_final,           // May be None if section extends to document end
    );
    
    // Generate MatchedContent with document indices
    result.matched_content = (start_idx..end_idx)
        .map(|idx| MatchedContent::Index(idx))
        .collect();

    result.metadata = inherited_metadata.clone();

    // Add section-specific metadata
    if let Some(as_value) = template.attributes.get("as") {
        // If 'as' attribute is defined, use its value
        result
            .metadata
            .insert("section".to_string(), as_value.clone());
    } else {
        // If 'as' attribute is not defined, fall back to the 'match' attribute value or null
        if let Some(match_value) = template.attributes.get("match") {
            result
                .metadata
                .insert("section".to_string(), match_value.clone());
        } else {
            // Remove any inherited "section" value and set to null
            result.metadata.remove("section");
        }
    }

    // Add the section name as well for reference
    result.metadata.insert(
        "section_name".to_string(),
        Value::String(template.name.clone()),
    );

    // Handle child elements
    if !template.children.is_empty() {

        if let Some(child_matches) = align_template_with_content_with_depth(
            &template.children,
            index,
            Some(&result.metadata), // Pass the updated metadata including section info
            Some(&result),
            recursion_depth + 1, // Increment depth for child processing
        ) {
    
            result.children = child_matches;
        } else {
    
        }
        
    } else {

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
    max_search_index: Option<usize>,
) -> Option<Vec<BoundaryCandidate>> {
    let mut candidates = Vec::new();


    // 1. Text-based candidates
    let text_matches = index.find_text_matches(
        &match_config.pattern,
        match_config.threshold,
        Some(start_index),
        max_search_index,
    );


    for (text_handle, score) in text_matches {
        let txt_ref = index.text(text_handle);
        let element = PageContent::Text(TextElement {
            id: txt_ref.id,
            text: txt_ref.text.to_string(),
            font_size: txt_ref.font_size,
            font_name: txt_ref.font_name.map(|s| s.to_string()),
            bbox: txt_ref.bbox,
            page_number: txt_ref.page_number,
        });
        candidates.push(score_candidate(
            &element, index, template, score, prev_match,
        ));
    }

    if candidates.is_empty() {

        None
    } else {
        candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));

        Some(candidates)
    }
}

/// Finds potential end boundary candidates
/// Returns a list of candidates sorted by score
fn find_end_boundary_candidates<'a>(
    start_content: &'a PageContent,
    template: &Element,
    index: &'a PdfIndex,
    children: &[Element],
    match_config: &MatchConfig,
    prev_match: Option<&TemplateContentMatch<'a>>,
) -> Option<Vec<BoundaryCandidate>> {


    let mut candidates = Vec::new();
    let mut has_end_match = false;

    // Get the start marker's index so we can search after it
    let start_marker_index = index.element_id_to_index.get(&start_content.id()).copied();


    // 1. Template-based end markers
    if let Some(end_match_attr) = template.attributes.get("end_match") {
        if let Some(end_str) = end_match_attr.as_string() {


            // Start search after the start marker, not from the beginning of the document
            let search_start_index = start_marker_index.map(|idx| idx + 1);


            let end_text_matches = index.find_text_matches(
                &end_str,
                match_config.threshold,
                search_start_index, // Use match_config.threshold instead of hardcoded value
                None
            );

            for (text_handle, score) in end_text_matches {
                let txt_ref = index.text(text_handle);
                let element = PageContent::Text(TextElement {
                    id: txt_ref.id,
                    text: txt_ref.text.to_string(),
                    font_size: txt_ref.font_size,
                    font_name: txt_ref.font_name.map(|s| s.to_string()),
                    bbox: txt_ref.bbox,
                    page_number: txt_ref.page_number,
                });


                // Boost explicit end markers, but allow high-quality similarity matches to compete
                has_end_match = true;
                let mut bc = score_candidate(
                    &element, index, template, score, prev_match,
                );
                bc.score += 0.5; // Moderate boost to prioritize explicit markers while allowing high-similarity competition
                bc.reasons.push("Explicit end marker".to_string());

                candidates.push(bc);
            }
        } else {

        }
    } else {

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

    // --- Structural similarity driven candidates ---------------------------
    // Pull top‑k (e.g., 5) similar text elements after the start marker up to the max boundary.
    const K_SIMILAR: usize = 5;
    if let PageContent::Text(start_text) = start_content {
        if let Some(start_idx) = index.element_id_to_index.get(&start_text.id).copied() {
            // Determine search boundary based on previous match section boundaries
            let max_content_boundary = if let Some(prev) = prev_match {
                if let Some(boundaries) = &prev.section_boundaries {
                    boundaries
                        .end_marker
                        .as_ref()
                        .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
                        .or_else(|| {
                            index
                                .element_id_to_index
                                .get(&boundaries.start_marker.id())
                                .copied()
                        })
                        .unwrap_or(0)
                } else {
                    index.doc_len()
                }
            } else {
                index.doc_len()
            };



            let similar = index.top_k_similar_text(
                start_text,
                start_idx + 1,        // search after the start marker
                max_content_boundary, // bounded by previous section boundaries
                K_SIMILAR,
            );

            for (text_handle, sim) in similar {
                let txt_ref = index.text(text_handle);
                let pc = PageContent::Text(TextElement {
                    id: txt_ref.id,
                    text: txt_ref.text.to_string(),
                    font_size: txt_ref.font_size,
                    font_name: txt_ref.font_name.map(|s| s.to_string()),
                    bbox: txt_ref.bbox,
                    page_number: txt_ref.page_number,
                });


                // Avoid duplicates – if already present, just update its score
                if let Some(existing) = candidates.iter_mut().find(|c| c.content.id() == pc.id()) {
                    // For explicit end markers, add similarity as a tiebreaker bonus
                    existing.score += 0.1 * sim; // smaller weight to preserve explicit marker priority
                    existing
                        .reasons
                        .push(format!("Top‑k similarity {:.2}", sim));
                } else {
                    // Similarity-only matches get much lower base scores to ensure explicit markers rank higher
                    candidates.push(BoundaryCandidate {
                        content: pc,
                        score: 0.1 * sim, // much lower base score to stay below explicit markers
                        reasons: vec![format!("Top‑k similarity {:.2}", sim)],
                    });
                }
            }
        }
    }

    if candidates.is_empty() {

        None
    } else {
        // Order by document position to pick the first valid boundary.
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    let a_idx = index.element_id_to_index[&a.content.id()];
                    let b_idx = index.element_id_to_index[&b.content.id()];
                    a_idx.cmp(&b_idx)
                })
        });


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
) -> BoundaryCandidate {
    let mut score = base_score as f32;
    let mut reasons = Vec::new();

    // Consider previous match if available
    if let Some(prev) = prev_match {
        if let Some(sb) = prev.section_boundaries.as_ref() {
            if let Some(ref end) = sb.end_marker {
                if end.page_number() != content.page_number() {
                    score += 0.2;
                    reasons.push("End marker on different page".to_string());
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
        content: content.clone(),
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
    candidate: &BoundaryCandidate,
    flow: &ContentFlow<'a>,
) -> bool {
    // Implement child position validation logic
    // This is a placeholder - actual implementation would depend on specific requirements
    true
}

/// Selects the best boundary from candidates
fn select_best_boundary<'a>(
    candidates: Vec<BoundaryCandidate>,
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
                if content_types_compatible(prev, &candidate.content) {
                    score += 0.2;
                }
            }

            // Consider child element requirements
            if satisfies_child_requirements(&candidate, children, index) {
                score += 0.3;
            }

            // Consider document flow
            if let Some(prev) = &previous_content {
                if maintains_document_flow(prev, &candidate.content, index) {
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
    candidate: &BoundaryCandidate,
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


    // Extract content slice based on explicit boundaries
    let content_slice =
        if content_start_idx < content_end_idx && content_start_idx < index.doc_len() {
            let end_idx = content_end_idx.min(index.doc_len());
            index.content_slice(content_start_idx, end_idx)
        } else {
            Vec::new()
        };



    let mut matched_content_for_chunk: Vec<MatchedContent> = Vec::new();
    let mut has_text_content = false;

    for i in content_start_idx..content_end_idx.min(index.doc_len()) {
        if let Some(handle) = index.get_handle(i) {
            match handle {
                ContentHandle::Text(_) => {
                    matched_content_for_chunk.push(MatchedContent::Index(i));
                    has_text_content = true;
                }
                ContentHandle::Image(_) => {
                    // TextChunk specifically ignores images
                }
            }
        }
    }

    if !has_text_content {

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

/// Extracts the content handles between a start element and an optional end element
pub fn extract_section_content_handles<'a>(
    _page_map_view: &BTreeMap<u32, Vec<PageContent>>,
    _start_marker_page_num: u32,
    start_marker: &'a PageContent,
    end_marker_option: Option<&'a PageContent>,
    index: &'a PdfIndex,
) -> Vec<ContentHandle> {
    // Debugging output
    let start_debug_info = match start_marker {
        PageContent::Text(t) => format!("Text('{}', ID: {})", t.text, t.id),
        PageContent::Image(i) => format!("Image(ID: {})", i.id),
    };
    let end_debug_info = end_marker_option.map_or("None".to_string(), |em| match em {
        PageContent::Text(t) => format!("Text('{}', ID: {})", t.text, t.id),
        PageContent::Image(i) => format!("Image(ID: {})", i.id),
    });


    // Get the start and end indices in the document
    let start_idx = index.element_id_to_index.get(&start_marker.id()).copied().unwrap_or(0);
    let end_idx = end_marker_option
        .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
        .unwrap_or(index.doc_len());



    // Extract handles directly from the index order
    let mut handles = Vec::new();
    for i in start_idx..end_idx {
        if let Some(handle) = index.get_handle(i) {
            handles.push(handle);
        }
    }



    handles
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
            MatchedContent::Index(doc_idx) => {
                // Start after the last processed element
                *doc_idx
            }
            MatchedContent::None => {
                // If last element was None, use section boundaries
                prev.section_boundaries
                    .as_ref()
                    .and_then(|sb| {
                        sb.end_marker
                            .as_ref()
                            .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
                            .or_else(|| {
                                index
                                    .element_id_to_index
                                    .get(&sb.start_marker.id())
                                    .copied()
                            })
                    })
                    .map_or(0, |idx| idx)
            }
        }
    } else {
        // If no matched content, use section boundaries
        prev.section_boundaries
            .as_ref()
            .and_then(|sb| {
                sb.end_marker
                    .as_ref()
                    .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
                    .or_else(|| {
                        index
                            .element_id_to_index
                            .get(&sb.start_marker.id())
                            .copied()
                    })
            })
            .map_or(0, |idx| idx)
    }
}

