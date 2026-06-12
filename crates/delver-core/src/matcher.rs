use crate::diagnostics::{MatchMiss, NearMiss, RunDiagnostics, NEAR_MISS_TOP_K};
use crate::docql::{
    ComparisonExpr, ComparisonOp, ComparisonValue, Element, ElementType, HeuristicProperty,
    MatchConfig, MatchType, Value,
};
use crate::layout::TextLine;
use crate::parse::{AuxKind, ContentHandle, PageContent, TextElement};
use crate::search_index::{PdfIndex, TextElemRef, TextHandle};
use anyhow::{anyhow, bail, Result};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use strsim::normalized_levenshtein;
use tracing::warn;
use uuid::Uuid;

// use crate::logging::TEMPLATE_MATCH;

// Maximum recursion depth to prevent runaway recursion
const MAX_RECURSION_DEPTH: usize = 10;

#[derive(Debug, Clone)]
pub struct TemplateContentMatch<'a> {
    pub template_element: &'a Element,
    pub matched_content: Vec<MatchedContent>,
    pub children: Vec<TemplateContentMatch<'a>>,
    pub metadata: HashMap<String, Value>,
    pub section_boundaries: Option<SectionBoundaries>,
}

#[derive(Debug, Clone)]
pub struct SectionBoundaries {
    pub start_marker: PageContent,
    pub end_marker: Option<PageContent>,
}

#[derive(Debug, Clone)]
pub enum MatchedContent {
    Index(usize), // Document-order index that can be resolved through PdfIndex
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

/// Aligns template elements with document content sequentially.
///
/// `Ok(None)` means the template simply matched nothing; `Err` is a match
/// execution failure (unconfigured embedder, unsupported match type, backend
/// error) that must surface to the caller instead of degrading silently
/// (D-006).
pub fn align_template_with_content<'a>(
    template_elements: &'a [Element],
    index: &'a PdfIndex,
    inherited_metadata: Option<&HashMap<String, Value>>,
    parent_or_prev_sibling_match_context: Option<&TemplateContentMatch<'a>>,
) -> Result<Option<Vec<TemplateContentMatch<'a>>>> {
    let mut discarded = RunDiagnostics::default();
    align_template_with_content_diag(
        template_elements,
        index,
        inherited_metadata,
        parent_or_prev_sibling_match_context,
        &mut discarded,
    )
}

/// [`align_template_with_content`] plus near-miss observability (D-024):
/// every match config that yields zero candidates above its threshold records
/// a [`MatchMiss`] (with the top-3 closest fuzzy-text candidates in the
/// searched scope) into `diagnostics`. Matching behavior is identical.
pub fn align_template_with_content_diag<'a>(
    template_elements: &'a [Element],
    index: &'a PdfIndex,
    inherited_metadata: Option<&HashMap<String, Value>>,
    parent_or_prev_sibling_match_context: Option<&TemplateContentMatch<'a>>,
    diagnostics: &mut RunDiagnostics,
) -> Result<Option<Vec<TemplateContentMatch<'a>>>> {
    align_template_with_content_with_depth(
        template_elements,
        index,
        inherited_metadata,
        parent_or_prev_sibling_match_context,
        0,
        diagnostics,
    )
}

/// Internal function that tracks recursion depth
fn align_template_with_content_with_depth<'a>(
    template_elements: &'a [Element],
    index: &'a PdfIndex,
    inherited_metadata: Option<&HashMap<String, Value>>,
    parent_or_prev_sibling_match_context: Option<&TemplateContentMatch<'a>>,
    recursion_depth: usize,
    diagnostics: &mut RunDiagnostics,
) -> Result<Option<Vec<TemplateContentMatch<'a>>>> {
    if template_elements.is_empty() {
        return Ok(None);
    }

    tracing::debug!(
        "align_template_with_content_with_depth: {:?}",
        template_elements
    );

    // Recursion depth guard
    if recursion_depth > MAX_RECURSION_DEPTH {
        warn!(
            "Recursion depth limit ({}) exceeded, terminating template matching to prevent runaway recursion",
            MAX_RECURSION_DEPTH
        );
        return Ok(None);
    }

    let default_metadata = HashMap::new();
    let actual_inherited_metadata = inherited_metadata.unwrap_or(&default_metadata);

    let mut elements_by_page_view: BTreeMap<u32, Vec<PageContent>> = BTreeMap::new();
    for (page_num, _page_elements) in index.by_page.iter() {
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

                tracing::debug!(
                    "MATCHER: Processing children within section boundaries {} to {}",
                    start_idx,
                    end_idx
                );
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
            tracing::debug!("  PASS 1: Processing Section '{:#?}'", template_element);

            // Determine which context to pass: previous sibling for most sections,
            // but for the *last* section pass the parent so it can inherit the
            // parent's end boundary when it has none of its own.
            let is_last_section = template_elements
                .iter()
                .skip_while(|e| !std::ptr::eq(*e, template_element)) // slice from current
                .skip(1) // look ahead
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
                        index
                            .element_id_to_index
                            .get(&end_marker.id())
                            .copied()
                            .unwrap_or(start_search_index)
                    } else {
                        index
                            .element_id_to_index
                            .get(&boundaries.start_marker.id())
                            .copied()
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
                diagnostics,
            )? {
                tracing::debug!(
                    "  PASS 1: Found Section '{}' boundaries",
                    template_element.name
                );

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

    // PASS 2: Assign non-structural elements (TextChunk, Annotation, Figure)
    // to appropriate content partitions
    let mut pass2_matches = Vec::new();

    for (template_idx, template_element) in template_elements.iter().enumerate() {
        if is_pass2_element(&template_element.element_type) {
            tracing::debug!(
                "  PASS 2: Processing {:?} '{}' (template order: {})",
                template_element.element_type,
                template_element.name,
                template_idx
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
                        tracing::debug!(
                            "    '{}' processes content BEFORE first section: {} to {}",
                            template_element.name,
                            start_search_index,
                            first_partition_start
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
                        tracing::debug!(
                            "    '{}' processes content AFTER sections: {} to {}",
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

            // Match the element against the determined content boundaries
            let pass2_match = match template_element.element_type {
                ElementType::TextChunk => match_text_chunk_with_boundaries(
                    template_element,
                    index,
                    actual_inherited_metadata,
                    content_start,
                    content_end,
                ),
                // Annotation / Figure selectors collect every aux element of
                // their kind in the assigned range (D-016).
                ElementType::Annotation => match_aux_kind_with_boundaries(
                    template_element,
                    index,
                    actual_inherited_metadata,
                    content_start,
                    content_end,
                    AuxKind::Annotation,
                ),
                ElementType::Figure => match_aux_kind_with_boundaries(
                    template_element,
                    index,
                    actual_inherited_metadata,
                    content_start,
                    content_end,
                    AuxKind::Figure,
                ),
                // Table selector collects every detected table in the
                // assigned range (D-018), same routing as Annotation/Figure.
                ElementType::Table => match_aux_kind_with_boundaries(
                    template_element,
                    index,
                    actual_inherited_metadata,
                    content_start,
                    content_end,
                    AuxKind::Table,
                ),
                _ => unreachable!("is_pass2_element gates the variants above"),
            };

            if let Some(pass2_match) = pass2_match {
                tracing::debug!("    SUCCESS: Matched '{}'", template_element.name);
                pass2_matches.push(pass2_match);
            } else {
                tracing::debug!("    FAILURE: No match for '{}'", template_element.name);
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
        } else if is_pass2_element(&template_element.element_type) {
            if let Some(pass2_match) = pass2_matches
                .iter()
                .find(|m| std::ptr::eq(m.template_element, template_element))
            {
                all_results.push(pass2_match.clone());
            }
        }
    }

    if all_results.is_empty() {
        Ok(None)
    } else {
        Ok(Some(all_results))
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
#[allow(dead_code)]
struct ContentFlow<'a> {
    elements: Vec<&'a PageContent>,
    relationships: Vec<(usize, usize, RelationshipType)>,
}

#[derive(Debug)]
#[allow(dead_code)]
enum RelationshipType {
    Before,
    After,
    Contains,
    ReferencedBy,
}

/// Finds section match that comes after prev_match
#[allow(clippy::too_many_arguments)]
fn match_section<'a, 'map_lt>(
    template: &'a Element,
    index: &'a PdfIndex,
    page_map_view: &'map_lt BTreeMap<u32, Vec<PageContent>>,
    inherited_metadata: &HashMap<String, Value>,
    prev_match_for_context: Option<&TemplateContentMatch<'a>>,
    current_search_start_index: usize,
    recursion_depth: usize,
    diagnostics: &mut RunDiagnostics,
) -> Result<Option<TemplateContentMatch<'a>>> {
    let Some(match_config) = template.match_config.as_ref() else {
        return Ok(None);
    };

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
    let Some(start_candidates) = find_start_boundary_candidates(
        template,
        index,
        effective_search_start_index,
        match_config,
        prev_match_for_context,
        max_search_index,
    )?
    else {
        // Zero candidates above threshold: the section will silently not
        // match. Record the near-miss diagnostic (D-024) before returning.
        record_match_miss(
            diagnostics,
            index,
            template,
            match_config,
            effective_search_start_index,
            max_search_index,
        );
        return Ok(None);
    };

    let Some(selected_start_candidate) = start_candidates.first().cloned() else {
        return Ok(None);
    };
    let start_marker: &PageContent = &selected_start_candidate.content;

    // 2. Find end boundary candidates
    let end_candidates_opt = find_end_boundary_candidates(
        start_marker, // Use the PageContent from candidates
        template,
        index,
        &template.children,
        match_config, // Pass the match_config for consistent threshold handling
        prev_match_for_context,
        diagnostics,
    )?;

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
        let start_idx = index
            .element_id_to_index
            .get(&start_marker.id())
            .copied()
            .unwrap_or(0);
        let next_boundary_idx = start_idx + 1;

        // Look for the next element that has similar characteristics to the
        // start marker. Aux elements (annotations/paths/figures/blobs) are
        // transparent here: they never participate in boundary heuristics,
        // so text/image-only documents behave exactly as before (D-016).
        let next_content = (next_boundary_idx..index.doc_len())
            .find(|&i| !matches!(index.get_handle(i), Some(ContentHandle::Aux(_))))
            .and_then(|i| index.content_at(i));
        if let Some(next_content) = next_content {
            if let PageContent::Text(next_text) = &next_content {
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
        template.name,
        start_marker,
        end_marker_final
    );

    // Calculate indices before moving end_marker_final
    let start_idx = index
        .element_id_to_index
        .get(&start_marker.id())
        .copied()
        .unwrap_or(0);
    let end_idx = end_marker_final
        .as_ref()
        .and_then(|end| index.element_id_to_index.get(&end.id()).copied())
        .unwrap_or(index.doc_len());

    // Create section match
    let mut result = TemplateContentMatch::with_section_boundaries(
        template,
        start_marker.clone(), // Clone for storage
        end_marker_final,     // May be None if section extends to document end
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
            diagnostics,
        )? {
            result.children = child_matches;
        }
    }

    Ok(Some(result))
}

/// Diagnostic name for a match config: the match-definition name when the
/// config came from one, otherwise the template element's name (D-006 errors
/// must name the match block).
fn match_owner(template: &Element, config: &MatchConfig) -> String {
    config
        .name
        .clone()
        .unwrap_or_else(|| template.name.clone())
}

/// Record a near-miss diagnostic for a match config that yielded zero
/// candidates above its threshold (D-024): the top-3 closest fuzzy-text
/// candidates in the same `[start, max)` scope the match searched. Only
/// `Text(...)` clauses have graded scores to rank (recursing into
/// `FirstMatch` alternatives); other matcher types record the miss with no
/// candidate ranking.
fn record_match_miss(
    diagnostics: &mut RunDiagnostics,
    index: &PdfIndex,
    template: &Element,
    config: &MatchConfig,
    start_index: usize,
    max_index: Option<usize>,
) {
    // The fuzzy-text clauses reachable from this config (self, or FirstMatch
    // alternatives), in declaration order.
    fn text_clauses<'c>(config: &'c MatchConfig, out: &mut Vec<&'c MatchConfig>) {
        match &config.match_type {
            MatchType::Text => out.push(config),
            MatchType::FirstMatch(alternatives) => {
                for alternative in alternatives {
                    text_clauses(alternative, out);
                }
            }
            _ => {}
        }
    }
    let mut clauses = Vec::new();
    text_clauses(config, &mut clauses);

    // Best-scoring candidates across all text clauses (each clause scores
    // against its own pattern), best first, top-3 overall.
    let mut near_misses: Vec<NearMiss> = Vec::new();
    for clause in &clauses {
        for (handle, score) in index.top_text_match_scores(
            &clause.pattern,
            Some(start_index),
            max_index,
            NEAR_MISS_TOP_K,
        ) {
            let text = index.text(handle);
            near_misses.push(NearMiss::new(text.text, score, text.page_number));
        }
    }
    near_misses.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
    near_misses.truncate(NEAR_MISS_TOP_K);

    // Report the first text clause's pattern/threshold (the only graded
    // clause kind); fall back to the config's own fields for Regex/Heuristic/
    // EmbeddingSim-only definitions.
    let (pattern, threshold) = clauses
        .first()
        .map(|c| (c.pattern.clone(), c.threshold))
        .unwrap_or_else(|| (config.pattern.clone(), config.threshold));

    diagnostics.record(MatchMiss {
        match_name: match_owner(template, config),
        pattern,
        threshold,
        near_misses,
    });
}

/// Finds potential start boundary candidates using multiple indices
fn find_start_boundary_candidates<'a>(
    template: &Element,
    index: &'a PdfIndex,
    start_index: usize,
    match_config: &MatchConfig,
    prev_match: Option<&TemplateContentMatch<'a>>,
    max_search_index: Option<usize>,
) -> Result<Option<Vec<BoundaryCandidate>>> {
    let mut candidates = Vec::new();

    tracing::debug!("[find_start_boundary_candidates] Template: {}, Match pattern: '{}', Threshold: {}, Start index: {}", template.name, match_config.pattern, match_config.threshold, start_index);
    // 1. Match-config candidates (Text/Regex/Heuristic/EmbeddingSim/FirstMatch)
    let text_matches = find_config_matches(
        index,
        match_config,
        &match_owner(template, match_config),
        start_index,
        max_search_index,
    )?;
    tracing::debug!(
        "[find_start_boundary_candidates] Match-config candidates found: {}",
        text_matches.len()
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
        tracing::debug!("[find_start_boundary_candidates] No candidates found. Returning None.");
        Ok(None)
    } else {
        candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
        tracing::debug!(
            "[find_start_boundary_candidates] Returning {} sorted candidates.",
            candidates.len()
        );
        Ok(Some(candidates))
    }
}

/// Finds potential end boundary candidates
/// Returns a list of candidates sorted by score
#[allow(clippy::too_many_arguments)]
fn find_end_boundary_candidates<'a>(
    start_content: &'a PageContent,
    template: &Element,
    index: &'a PdfIndex,
    _children: &[Element],
    _match_config: &MatchConfig,
    prev_match: Option<&TemplateContentMatch<'a>>,
    diagnostics: &mut RunDiagnostics,
) -> Result<Option<Vec<BoundaryCandidate>>> {
    let mut candidates = Vec::new();

    // Get the start marker's index so we can search after it
    let start_marker_index = index.element_id_to_index.get(&start_content.id()).copied();

    // 1. Template-based end markers
    if let Some(config) = template.end_match_config.as_ref() {
        // Start search after the start marker, not from the beginning of the document
        let search_start_index = start_marker_index.map(|idx| idx + 1).unwrap_or(0);

        let end_text_matches = find_config_matches(
            index,
            config,
            &match_owner(template, config),
            search_start_index,
            None,
        )?;
        if end_text_matches.is_empty() {
            // The explicit end marker missed entirely; the section will fall
            // back to a parent/natural boundary. Surface why (D-024).
            record_match_miss(diagnostics, index, template, config, search_start_index, None);
        }
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
            let mut bc = score_candidate(&element, index, template, score, prev_match);
            bc.score += 0.5; // Moderate boost to prioritize explicit markers while allowing high-similarity competition
            bc.reasons.push("Explicit end marker".to_string());

            candidates.push(bc);
        }
    } else {
        tracing::debug!("[find_end_boundary_candidates] No 'end_match' attribute key found in template attributes.");
        // If no end_match is specified, we might want a default behavior,
        // for example, consider all elements after start_content on the same page,
        // or up to the start of a *next* identifiable section if one exists soon.
        // For now, if no end_match, it will likely result in candidates being empty.
    }

    // 2. Natural boundaries (Currently Commented Out)
    // tracing::debug!("[find_end_boundary_candidates] Considering natural boundaries...");
    // candidates.extend(find_natural_boundaries(start_content, index, children));

    // 3. Filter based on child elements (Currently Commented Out)
    // tracing::debug!("[find_end_boundary_candidates] Validating boundary candidates based on children...");
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
        Ok(None)
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

        Ok(Some(candidates))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Match-config execution (D-014). Every MatchType either executes or errors;
// nothing is silently skipped (D-006).
// ─────────────────────────────────────────────────────────────────────────────

/// Execute one `MatchConfig` over the scoped document range, dispatching on
/// its match type. Returns `(handle, score)` candidates; scores are the
/// matcher's native confidence (Levenshtein / cosine similarity, or 1.0 for
/// the exact Regex/Heuristic matchers).
fn find_config_matches(
    index: &PdfIndex,
    config: &MatchConfig,
    owner: &str,
    start_index: usize,
    max_index: Option<usize>,
) -> Result<Vec<(TextHandle, f64)>> {
    match &config.match_type {
        MatchType::Text => Ok(index.find_text_matches(
            &config.pattern,
            config.threshold,
            Some(start_index),
            max_index,
        )),
        MatchType::Regex => {
            // Compiled at template-compile time; fall back to compiling here
            // for configs constructed programmatically (tests, embedders).
            let re = match &config.compiled_regex {
                Some(re) => re.clone(),
                None => regex::Regex::new(&config.pattern).map_err(|e| {
                    anyhow!(
                        "match '{owner}': invalid regex pattern '{}': {e}",
                        config.pattern
                    )
                })?,
            };
            Ok(index.find_regex_matches(&re, Some(start_index), max_index))
        }
        MatchType::Heuristic(comparisons) => {
            find_heuristic_matches(index, comparisons, owner, start_index, max_index)
        }
        MatchType::EmbeddingSim => {
            find_embedding_matches(index, config, owner, start_index, max_index)
        }
        MatchType::FirstMatch(alternatives) => {
            for alternative in alternatives {
                let matches =
                    find_config_matches(index, alternative, owner, start_index, max_index)?;
                if !matches.is_empty() {
                    return Ok(matches);
                }
            }
            Ok(Vec::new())
        }
        MatchType::Custom(name) => bail!(
            "match '{owner}': unsupported match type '{name}' cannot execute (D-006: no \
             silent skip)"
        ),
    }
}

/// `Heuristic(...)`: evaluate the comparisons (ANDed) against each scoped
/// element's properties. Matching elements score 1.0, in document order.
fn find_heuristic_matches(
    index: &PdfIndex,
    comparisons: &[ComparisonExpr],
    owner: &str,
    start_index: usize,
    max_index: Option<usize>,
) -> Result<Vec<(TextHandle, f64)>> {
    let end = max_index.unwrap_or(index.doc_len());
    let mut results = Vec::new();
    for handle in index.text_handles_in_range(start_index, end) {
        let element = index.text(handle);
        if evaluate_heuristic(comparisons, &element, owner)? {
            results.push((handle, 1.0));
        }
    }
    Ok(results)
}

fn evaluate_heuristic(
    comparisons: &[ComparisonExpr],
    element: &TextElemRef<'_>,
    owner: &str,
) -> Result<bool> {
    for comparison in comparisons {
        if !evaluate_comparison(comparison, element, owner)? {
            return Ok(false); // comparisons AND together
        }
    }
    Ok(true)
}

fn evaluate_comparison(
    comp: &ComparisonExpr,
    element: &TextElemRef<'_>,
    owner: &str,
) -> Result<bool> {
    let prop = HeuristicProperty::parse(&comp.left).ok_or_else(|| {
        anyhow!(
            "match '{owner}': Heuristic(...) references unknown property '{}'; supported \
             properties: {}",
            comp.left,
            HeuristicProperty::supported_list()
        )
    })?;

    if prop.is_string() {
        let actual = match prop {
            HeuristicProperty::FontName => crate::fonts::canonicalize::canonicalize_font_name(
                element.font_name.unwrap_or_default(),
            ),
            HeuristicProperty::Text => element.text.to_string(),
            _ => unreachable!("is_string() covers exactly FontName and Text"),
        };
        let expected = match &comp.right {
            ComparisonValue::String(s) | ComparisonValue::Identifier(s) => s.clone(),
            other => bail!(
                "match '{owner}': string property '{}' compared with non-string {:?}",
                comp.left,
                other
            ),
        };
        // Font names compare canonicalized + case-insensitively (raw PDF
        // names carry subset prefixes like "ABCDEF+"); text compares exactly.
        let equal = match prop {
            HeuristicProperty::FontName => {
                let expected =
                    crate::fonts::canonicalize::canonicalize_font_name(expected.as_str());
                actual.eq_ignore_ascii_case(&expected)
            }
            _ => actual == expected,
        };
        return match comp.op {
            ComparisonOp::Equal => Ok(equal),
            ComparisonOp::NotEqual => Ok(!equal),
            _ => bail!(
                "match '{owner}': property '{}' is a string; only == and != are supported",
                comp.left
            ),
        };
    }

    let actual: f64 = match prop {
        HeuristicProperty::FontSize => element.font_size as f64,
        HeuristicProperty::Page => element.page_number as f64,
        HeuristicProperty::X0 => element.bbox.0 as f64,
        HeuristicProperty::Y0 => element.bbox.1 as f64,
        HeuristicProperty::X1 => element.bbox.2 as f64,
        HeuristicProperty::Y1 => element.bbox.3 as f64,
        HeuristicProperty::TextLength => element.text.chars().count() as f64,
        HeuristicProperty::FontName | HeuristicProperty::Text => {
            unreachable!("string properties handled above")
        }
    };
    let expected = match &comp.right {
        ComparisonValue::Number(n) => *n,
        other => bail!(
            "match '{owner}': numeric property '{}' compared with non-number {:?}",
            comp.left,
            other
        ),
    };
    Ok(match comp.op {
        ComparisonOp::GreaterThan => actual > expected,
        ComparisonOp::LessThan => actual < expected,
        ComparisonOp::GreaterThanOrEqual => actual >= expected,
        ComparisonOp::LessThanOrEqual => actual <= expected,
        ComparisonOp::Equal => (actual - expected).abs() < f64::EPSILON,
        ComparisonOp::NotEqual => (actual - expected).abs() >= f64::EPSILON,
    })
}

/// `EmbeddingSim("query", threshold=..)`: embed the query once and the scoped
/// candidates as one batch, keep candidates with cosine similarity >=
/// threshold, ranked by similarity. No configured embedder is a hard error
/// naming the match block (D-006).
fn find_embedding_matches(
    index: &PdfIndex,
    config: &MatchConfig,
    owner: &str,
    start_index: usize,
    max_index: Option<usize>,
) -> Result<Vec<(TextHandle, f64)>> {
    let Some(embedder) = index.embedder() else {
        let endpoint_hint = config
            .endpoint
            .as_deref()
            .map(|e| format!(" (template names endpoint \"{e}\")"))
            .unwrap_or_default();
        bail!(
            "match '{owner}': template uses EmbeddingSim(\"{}\") but no embedder is \
             configured{endpoint_hint}; pass --embed-endpoint <name-or-url> or set \
             DELVER_EMBED_ENDPOINT (D-006: no silent skip)",
            config.pattern
        );
    };

    let end = max_index.unwrap_or(index.doc_len());
    let handles = index.text_handles_in_range(start_index, end);
    if handles.is_empty() {
        return Ok(Vec::new());
    }

    let query_vec = embedder
        .embed(&[&config.pattern])
        .map_err(|e| anyhow!("match '{owner}': embedding the query failed: {e}"))?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("match '{owner}': embedder returned no vector for the query"))?;

    let texts: Vec<&str> = handles.iter().map(|h| index.text(*h).text).collect();
    let vectors = embedder
        .embed(&texts)
        .map_err(|e| anyhow!("match '{owner}': embedding {} candidates failed: {e}", texts.len()))?;
    if vectors.len() != handles.len() {
        bail!(
            "match '{owner}': embedder returned {} vectors for {} texts",
            vectors.len(),
            handles.len()
        );
    }

    let mut results = Vec::new();
    for (handle, vector) in handles.into_iter().zip(vectors) {
        let similarity = cosine_similarity(&query_vec, &vector).ok_or_else(|| {
            anyhow!(
                "match '{owner}': embedding dimension mismatch (query {} vs candidate {})",
                query_vec.len(),
                vector.len()
            )
        })?;
        if similarity >= config.threshold {
            results.push((handle, similarity));
        }
    }
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
    Ok(results)
}

/// Cosine similarity in f64; `None` on dimension mismatch, 0.0 when either
/// vector has zero norm. Shared with semantic chunking (D-020).
pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() {
        return None;
    }
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += (*x as f64) * (*y as f64);
        norm_a += (*x as f64) * (*x as f64);
        norm_b += (*y as f64) * (*y as f64);
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return Some(0.0);
    }
    Some(dot / (norm_a.sqrt() * norm_b.sqrt()))
}

/// Scores a potential boundary candidate
fn score_candidate<'a>(
    content: &'a PageContent,
    index: &PdfIndex,
    _template: &Element,
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
        // Aux elements (annotations/paths/figures/blobs) never become
        // boundary candidates; no score adjustment.
        PageContent::Aux(_) => {}
    }

    BoundaryCandidate {
        content: content.clone(),
        score,
        reasons,
    }
}

/// Checks if two content types are compatible
#[allow(dead_code)]
fn content_types_compatible(a: &PageContent, b: &PageContent) -> bool {
    match (a, b) {
        (PageContent::Text(_), PageContent::Text(_)) => true,
        (PageContent::Image(_), PageContent::Image(_)) => true,
        (PageContent::Aux(x), PageContent::Aux(y)) => x.kind == y.kind,
        _ => false,
    }
}

/// Checks if document flow is maintained between two content elements
#[allow(dead_code)]
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

/// Non-structural template types assigned in Pass 2 (within section
/// boundaries / between section partitions): TextChunk plus the Annotation,
/// Figure (D-016) and Table (D-018) selectors.
fn is_pass2_element(element_type: &ElementType) -> bool {
    matches!(
        element_type,
        ElementType::TextChunk
            | ElementType::Annotation
            | ElementType::Figure
            | ElementType::Table
    )
}

/// Collect every aux element of `kind` inside `[content_start_idx,
/// content_end_idx)` — the aux mirror of `match_text_chunk_with_boundaries`.
fn match_aux_kind_with_boundaries<'a>(
    template: &'a Element,
    index: &'a PdfIndex,
    inherited_metadata: &HashMap<String, Value>,
    content_start_idx: usize,
    content_end_idx: usize,
    kind: AuxKind,
) -> Option<TemplateContentMatch<'a>> {
    let matched: Vec<MatchedContent> = (content_start_idx..content_end_idx.min(index.doc_len()))
        .filter(|&i| index.aux_at(i).map_or(false, |aux| aux.kind == kind))
        .map(MatchedContent::Index)
        .collect();

    if matched.is_empty() {
        tracing::debug!(
            "[match_aux_kind_with_boundaries] No {:?} content in [{}, {})",
            kind,
            content_start_idx,
            content_end_idx
        );
        return None;
    }

    let mut result = TemplateContentMatch::with_content(template, matched);
    result.metadata = inherited_metadata.clone();
    Some(result)
}

/// Matches a TextChunk element with explicit content boundaries
fn match_text_chunk_with_boundaries<'a>(
    template: &'a Element,
    index: &'a PdfIndex,
    inherited_metadata: &HashMap<String, Value>,
    content_start_idx: usize,
    content_end_idx: usize,
) -> Option<TemplateContentMatch<'a>> {
    let mut matched_content_for_chunk: Vec<MatchedContent> = Vec::new();
    let mut has_text_content = false;

    for i in content_start_idx..content_end_idx.min(index.doc_len()) {
        if let Some(handle) = index.get_handle(i) {
            match handle {
                ContentHandle::Text(_) => {
                    matched_content_for_chunk.push(MatchedContent::Index(i));
                    has_text_content = true;
                }
                ContentHandle::Image(_) | ContentHandle::Aux(_) => {
                    // TextChunk specifically ignores images and aux elements
                }
            }
        }
    }

    if !has_text_content {
        tracing::debug!("[match_text_chunk_with_boundaries] No text content found");
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
    let describe = |content: &PageContent| match content {
        PageContent::Text(t) => format!("Text('{}', ID: {})", t.text, t.id),
        PageContent::Image(i) => format!("Image(ID: {})", i.id),
        PageContent::Aux(a) => format!("Aux({:?}, ID: {})", a.kind, a.id),
    };
    tracing::debug!(
        "[extract_section_content] Start: {}, End: {}",
        describe(start_marker),
        end_marker_option.map_or("None".to_string(), describe)
    );

    // Get the start and end indices in the document
    let start_idx = index
        .element_id_to_index
        .get(&start_marker.id())
        .copied()
        .unwrap_or(0);
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
//     tracing::debug!("MATCHER: Processing Table template element");

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

//         tracing::debug!(
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
//     tracing::debug!(
//         "MATCHER: Processing Image template element, starting search from index {}",
//         start_image_index
//     );

//     index.images.get(start_image_index).map(|image_elem| {
//         tracing::debug!("MATCHER: Found image with ID {}", image_elem.id);
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
