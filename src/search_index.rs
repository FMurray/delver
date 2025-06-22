// TODO(step‑2): remove AoS `all_ordered_content` and migrate callers to SoA accessors.

use crate::features::{compute_similarity, TextFeatures};
use crate::{
    layout::{MatchContext, TextLine},
    parse::{
        ContentHandle, ImageElement, ImageStore, PageContent, PageContents, TextElement, TextStore,
    },
};
use lopdf::Object;
use ordered_float::NotNan;
use rstar::{RTree, RTreeObject, AABB};
use std::collections::BinaryHeap;
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

/// Typed handle for text elements - prevents mixing with image handles
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextHandle(pub u32);

/// Typed handle for image elements
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImageHandle(pub u32);

/// Narrow borrow: contains refs into *one* row; can't out-live `PdfIndex`.
#[derive(Debug)]
pub struct TextElemRef<'a> {
    pub id: Uuid,
    pub text: &'a str,
    pub font_size: f32,
    pub font_name: Option<&'a str>,
    pub bbox: (f32, f32, f32, f32),
    pub page_number: u32,
}

/// Narrow borrow for image elements
#[derive(Debug)]
pub struct ImageElemRef<'a> {
    pub id: Uuid,
    pub bbox: crate::geo::Rect,
    pub page_number: u32,
    pub image_object: &'a Object,
}

// Wrapper for TextElement to implement RTreeObject
#[derive(Clone, Debug)]
pub struct SpatialPageContent {
    element: PageContent,
}

impl RTreeObject for SpatialPageContent {
    type Envelope = AABB<[f32; 2]>;

    fn envelope(&self) -> Self::Envelope {
        let bbox = &self.element.bbox();
        AABB::from_corners([bbox.x0, bbox.y0], [bbox.x1, bbox.y1])
    }
}

impl SpatialPageContent {
    fn new(element: PageContent) -> Self {
        Self { element }
    }
}

// -----------------------------------------------------------------------------

/// Font usage analysis structure
#[derive(Debug, Clone)]
pub struct FontUsage {
    pub font_name: String,
    pub font_name_opt: Option<String>,
    pub font_size: f32,
    pub total_usage: u32,
    pub elements: Vec<usize>,
}

impl FontUsage {
    pub fn new(font_name: String, font_name_opt: Option<String>, font_size: f32) -> Self {
        Self {
            font_name,
            font_name_opt,
            font_size,
            total_usage: 0,
            elements: Vec::new(),
        }
    }

    pub fn add_usage(&mut self, element_idx: usize) {
        self.total_usage += 1;
        self.elements.push(element_idx);
    }
}

#[derive(Debug)]
pub struct PdfIndex {
    pub by_page: BTreeMap<u32, Vec<usize>>,
    pub font_size_index: Vec<(f32, usize)>,
    pub reference_count_index: Vec<(u32, usize)>,
    pub spatial_rtree: RTree<SpatialPageContent>,
    pub element_id_to_index: HashMap<Uuid, usize>,
    pub order: Vec<ContentHandle>, // document sequence (SoA handle)
    pub text_store: TextStore,     // SoA payload ‑ text
    pub image_store: ImageStore,   // SoA payload ‑ images
    pub fonts: HashMap<(String, NotNan<f32>), FontUsage>,
    pub font_name_frequency_index: Vec<(u32, String)>,
    pub font_size_stats: FontSizeStats,
    pub feature_cache: dashmap::DashMap<Uuid, TextFeatures>,
}

impl PdfIndex {
    pub fn new(page_map: &BTreeMap<u32, PageContents>, _match_context: &MatchContext) -> Self {
        let mut by_page = BTreeMap::new();
        let mut font_size_index_construction = Vec::new(); // Temp for construction before sorting
        let mut spatial_elements = Vec::new();
        let mut element_id_to_index = HashMap::new();
        let mut fonts_map: HashMap<(String, NotNan<f32>), FontUsage> = HashMap::new(); // Correctly typed
        let feature_cache = dashmap::DashMap::new();

        let mut current_content_index = 0;

        // Aggregate all SoA data from PageContents
        let mut order: Vec<ContentHandle> = Vec::new();
        let mut text_store = TextStore::default();
        let mut image_store = ImageStore::default();

        for (page_number, page_contents) in page_map {
            let mut page_element_indices = Vec::new();

            // Process content in document order using the SoA
            for content_item in page_contents.iter_ordered() {
                page_element_indices.push(current_content_index);
                element_id_to_index.insert(content_item.id(), current_content_index);
                spatial_elements.push(SpatialPageContent::new(content_item.clone()));

                if let PageContent::Text(ref text_elem) = content_item {
                    let current_font_size = text_elem.font_size;
                    let canonical_font_name = crate::fonts::canonicalize::canonicalize_font_name(
                        text_elem.font_name.as_deref().unwrap_or_default(),
                    );

                    font_size_index_construction.push((current_font_size, current_content_index));

                    // Use (font_name, font_size) as the key for fonts_map
                    let font_style_key = (
                        canonical_font_name.clone(),
                        NotNan::new(current_font_size).unwrap(),
                    );
                    let font_entry = fonts_map.entry(font_style_key).or_insert_with(|| {
                        FontUsage::new(
                            canonical_font_name,         // Store canonical name in FontUsage
                            text_elem.font_name.clone(), // Store original name option
                            current_font_size,           // Store this specific size
                        )
                    });
                    font_entry.add_usage(current_content_index);
                }
                current_content_index += 1;
            }

            if !page_element_indices.is_empty() {
                by_page.insert(*page_number, page_element_indices);
            }

            // Aggregate SoA data from PageContents
            // We need to update ContentHandle indices when copying to global stores
            let text_store_offset = text_store.id.len();
            let image_store_offset = image_store.id.len();

            // Copy text and image stores first
            for i in 0..page_contents.text_store.id.len() {
                if let Some(elem) = page_contents.text_store.get(i) {
                    text_store.push(elem);
                }
            }
            for i in 0..page_contents.image_store.id.len() {
                if let Some(elem) = page_contents.image_store.get(i) {
                    image_store.push(elem);
                }
            }

            // Update ContentHandle indices and add to global order
            for handle in &page_contents.order {
                let updated_handle = match handle {
                    ContentHandle::Text(local_idx) => {
                        ContentHandle::Text(text_store_offset + local_idx)
                    }
                    ContentHandle::Image(local_idx) => {
                        ContentHandle::Image(image_store_offset + local_idx)
                    }
                };
                order.push(updated_handle);
            }
        }

        font_size_index_construction
            .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let font_size_index = font_size_index_construction; // Assign after sorting

        let spatial_rtree = RTree::bulk_load(spatial_elements);
        let reference_count_index = Vec::new();

        // Compute font size stats from SoA text store
        let font_size_stats = {
            let mut sizes: Vec<f32> = text_store.font_size.clone();
            if sizes.is_empty() {
                FontSizeStats {
                    mean: 12.0,
                    std_dev: 0.0,
                    percentiles: [12.0; 5],
                }
            } else {
                sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let mean = sizes.iter().sum::<f32>() / sizes.len() as f32;
                let variance =
                    sizes.iter().map(|&x| (x - mean).powi(2)).sum::<f32>() / sizes.len() as f32;
                let std_dev = variance.sqrt();
                let idx = |p: f32| sizes[((sizes.len() as f32) * p) as usize];
                FontSizeStats {
                    mean,
                    std_dev,
                    percentiles: [idx(0.25), idx(0.50), idx(0.75), idx(0.90), idx(0.95)],
                }
            }
        };

        // Build font_name_frequency_index (total usage of a font name across all its sizes)
        let mut font_name_totals: HashMap<String, u32> = HashMap::new();
        for ((name, _size), usage_data) in &fonts_map {
            *font_name_totals.entry(name.clone()).or_insert(0) += usage_data.total_usage;
        }
        let mut font_name_frequency_index: Vec<(u32, String)> = font_name_totals
            .into_iter()
            .map(|(name, count)| (count, name))
            .collect();
        font_name_frequency_index.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

        PdfIndex {
            by_page,
            font_size_index,
            reference_count_index,
            spatial_rtree,
            element_id_to_index,
            order,
            text_store,
            image_store,
            fonts: fonts_map, // Use the correctly populated map
            font_name_frequency_index,
            font_size_stats,
            feature_cache,
        }
    }

    // Helper method to reconstruct PageContent from SoA based on ContentHandle
    pub fn content_from_handle(&self, idx: usize) -> Option<PageContent> {
        self.order.get(idx).and_then(|handle| match handle {
            ContentHandle::Text(text_idx) => self.text_store.get(*text_idx).map(PageContent::Text),
            ContentHandle::Image(image_idx) => {
                self.image_store.get(*image_idx).map(PageContent::Image)
            }
        })
    }

    // Helper method to get multiple content items efficiently
    fn content_from_indices(&self, indices: &[usize]) -> Vec<PageContent> {
        indices
            .iter()
            .filter_map(|&idx| self.content_from_handle(idx))
            .collect()
    }

    /// Update reference counts based on destinations in MatchContext
    pub fn update_reference_counts(&mut self, context: &MatchContext) {
        let mut reference_counts = HashMap::<usize, u32>::new();

        // Go through all destinations and count references to each element
        for (_, dest_obj) in context.destinations.iter() {
            if let Object::Array(dest_array) = dest_obj {
                if dest_array.len() >= 4 {
                    // Extract page number (add 1 because PDF page numbers start at 0)
                    let dest_page = match &dest_array[0] {
                        Object::Integer(page) => (*page as u32) + 1,
                        _ => continue,
                    };

                    // Extract Y coordinate
                    let dest_y = match &dest_array[3] {
                        Object::Real(y) => *y,
                        Object::Integer(y) => *y as f32,
                        _ => continue,
                    };

                    // Optional: Extract X coordinate if available (position 2)
                    let dest_x = if dest_array.len() >= 3 {
                        match &dest_array[2] {
                            Object::Real(x) => Some(*x),
                            Object::Integer(x) => Some(*x as f32),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    // Use the RTree to find elements near the destination coordinates
                    // Create a search region around the destination point
                    let search_region = match dest_x {
                        // If we have both X and Y, create a small box around the point
                        Some(x) => {
                            // Use a small search radius (10 points)
                            let radius = 10.0;
                            AABB::from_corners(
                                [x - radius, dest_y - radius],
                                [x + radius, dest_y + radius],
                            )
                        }
                        // If we only have Y, create a horizontal band
                        None => {
                            // Use a narrow vertical band (± 10 points)
                            let y_radius = 10.0;
                            // But cover the whole page horizontally
                            AABB::from_corners(
                                [0.0, dest_y - y_radius],
                                [2000.0, dest_y + y_radius], // 2000 is just a large value to cover most page widths
                            )
                        }
                    };

                    // Find elements that match the page and the spatial query
                    let matching_elements = self
                        .spatial_rtree
                        .locate_in_envelope(&search_region)
                        .filter(|spatial_elem| spatial_elem.element.page_number() == dest_page)
                        .filter_map(|spatial_elem| {
                            self.element_id_to_index
                                .get(&spatial_elem.element.id())
                                .copied()
                        });

                    // Increment reference count for each matching element
                    for idx in matching_elements {
                        *reference_counts.entry(idx).or_insert(0) += 1;
                    }
                }
            }
        }

        // Build the reference count index
        self.reference_count_index.clear();
        for idx in 0..self.order.len() {
            let count = reference_counts.get(&idx).copied().unwrap_or(0);
            self.reference_count_index.push((count, idx));
        }

        // Sort by reference count
        self.reference_count_index.sort_by_key(|&(count, _)| count);
    }

    /// Find elements on a specific page
    pub fn elements_on_page(&self, page_num: u32) -> Vec<PageContent> {
        if let Some(indices) = self.by_page.get(&page_num) {
            self.content_from_indices(indices)
        } else {
            Vec::new()
        }
    }

    /// Find elements with font size in a specific range - uses SoA for cache efficiency
    pub fn elements_by_font_size(&self, min_size: f32, max_size: f32) -> Vec<PageContent> {
        // Binary search for the lower and upper bounds
        let lower_idx = self
            .font_size_index
            .binary_search_by(|&(size, _)| {
                size.partial_cmp(&min_size)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or_else(|idx| idx);

        let upper_idx = self
            .font_size_index
            .binary_search_by(|&(size, _)| {
                size.partial_cmp(&max_size)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or_else(|idx| idx);

        let indices: Vec<usize> = self.font_size_index[lower_idx..upper_idx]
            .iter()
            .map(|&(_, idx)| idx)
            .collect();

        self.content_from_indices(&indices)
    }

    /// Find elements with at least the specified number of references
    pub fn elements_by_reference_count(&self, min_count: u32) -> Vec<PageContent> {
        // Binary search for the lower bound
        let lower_idx = self
            .reference_count_index
            .binary_search_by_key(&min_count, |&(count, _)| count)
            .unwrap_or_else(|idx| idx);

        let indices: Vec<usize> = self.reference_count_index[lower_idx..]
            .iter()
            .map(|&(_, idx)| idx)
            .collect();

        self.content_from_indices(&indices)
    }

    /// Find elements within a specified rectangular region
    pub fn elements_in_region(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<PageContent> {
        let query_rect = AABB::from_corners([x0, y0], [x1, y1]);

        self.spatial_rtree
            .locate_in_envelope(&query_rect)
            .map(|spatial_elem| spatial_elem.element.clone())
            .collect()
    }

    /// Find elements that match multiple criteria
    pub fn search(
        &self,
        page: Option<u32>,
        min_font_size: Option<f32>,
        max_font_size: Option<f32>,
        min_references: Option<u32>,
        region: Option<(f32, f32, f32, f32)>,
    ) -> Vec<PageContent> {
        let mut result_indices: Option<HashSet<usize>> = None;

        // Filter by page
        if let Some(page_num) = page {
            let page_indices: HashSet<usize> = self
                .by_page
                .get(&page_num)
                .map_or_else(HashSet::new, |indices| indices.iter().copied().collect());

            result_indices = Some(page_indices);
        }

        // Filter by font size
        if min_font_size.is_some() || max_font_size.is_some() {
            let min_size = min_font_size.unwrap_or(0.0);
            let max_size = max_font_size.unwrap_or(f32::MAX);

            let font_size_indices: HashSet<usize> = self
                .font_size_index
                .iter()
                .filter(|&&(size, _)| size >= min_size && size <= max_size)
                .map(|&(_, idx)| idx)
                .collect();

            result_indices = match result_indices {
                Some(indices) => Some(indices.intersection(&font_size_indices).copied().collect()),
                None => Some(font_size_indices),
            };
        }

        // Filter by reference count
        if let Some(min_references) = min_references {
            let reference_indices: HashSet<usize> = self
                .reference_count_index
                .iter()
                .filter(|&&(count, _)| count >= min_references)
                .map(|&(_, idx)| idx)
                .collect();

            result_indices = match result_indices {
                Some(indices) => Some(indices.intersection(&reference_indices).copied().collect()),
                None => Some(reference_indices),
            };
        }

        // Filter by region
        if let Some((x0, y0, x1, y1)) = region {
            let query_rect = AABB::from_corners([x0, y0], [x1, y1]);

            let region_elements: HashSet<usize> = self
                .spatial_rtree
                .locate_in_envelope(&query_rect)
                .filter_map(|spatial_elem| {
                    self.element_id_to_index
                        .get(&spatial_elem.element.id())
                        .copied()
                })
                .collect();

            result_indices = match result_indices {
                Some(indices) => Some(indices.intersection(&region_elements).copied().collect()),
                None => Some(region_elements),
            };
        }

        // Convert result indices to PageContent
        match result_indices {
            Some(indices) => {
                let indices_vec: Vec<usize> = indices.into_iter().collect();
                self.content_from_indices(&indices_vec)
            }
            None => {
                // If no filters applied, return all elements
                (0..self.order.len())
                    .filter_map(|idx| self.content_from_handle(idx))
                    .collect()
            }
        }
    }

    /// Get PageContent by ID
    pub fn get_element_by_id(&self, id: &Uuid) -> Option<PageContent> {
        self.element_id_to_index
            .get(id)
            .and_then(|&idx| self.content_from_handle(idx))
    }

    /// Update the index with a new MatchContext
    pub fn update_with_match_context(&mut self, match_context: &MatchContext) {
        self.update_reference_counts(match_context);
    }

    /// Find elements that might match a text string - cache-efficient SoA access
    /// Returns typed handles and scores for zero-copy access
    pub fn find_text_matches(
        &self,
        text: &str,
        threshold: f64,
        start_content_index: Option<usize>,
    ) -> Vec<(TextHandle, f64)> {
        use strsim::normalized_levenshtein;
        let start = start_content_index.unwrap_or(0);

        // Cache-efficient: iterate through text store directly
        let mut results = Vec::new();

        // Iterate through the text column only (cache-friendly)
        for (text_idx, text_content) in self.text_store.text.iter().enumerate() {
            let score = normalized_levenshtein(text, text_content);
            if score >= threshold {
                // Find the corresponding document index for this text element
                if let Some(doc_idx) = self.find_doc_index_for_text(text_idx) {
                    if doc_idx >= start {
                        results.push((TextHandle(text_idx as u32), score));
                    }
                }
            }
        }
        results
    }

    /// Helper method to find document index for a text store index
    fn find_doc_index_for_text(&self, text_idx: usize) -> Option<usize> {
        // Get the ID of the text element
        let text_id = self.text_store.id.get(text_idx)?;
        // Look up the document index by ID
        self.element_id_to_index.get(text_id).copied()
    }

    /// Get text element by document index - cache efficient
    pub fn get_text_at(&self, doc_idx: usize) -> Option<TextElement> {
        match self.order.get(doc_idx)? {
            ContentHandle::Text(text_idx) => self.text_store.get(*text_idx),
            ContentHandle::Image(_) => None,
        }
    }

    /// Find lines that might match a text string
    pub fn find_line_text_matches<'a>(
        &self,
        text: &str,
        threshold: f64,
        lines: &'a [TextLine],
    ) -> Vec<&'a TextLine> {
        use strsim::normalized_levenshtein;

        lines
            .iter()
            .map(|line| {
                let score = normalized_levenshtein(text, &line.text);
                (line, score)
            })
            .filter(|(_, score)| *score >= threshold)
            .map(|(line, _)| line)
            .collect()
    }

    /// Cache-efficient average font size calculation using SoA
    pub fn average_font_size(&self) -> f32 {
        if self.text_store.font_size.is_empty() {
            12.0
        } else {
            self.text_store.font_size.iter().sum::<f32>() / self.text_store.font_size.len() as f32
        }
    }

    /// Return the top‑k most similar text elements to `seed` between [start_idx, end_idx).
    /// Similarity is computed via `features::compute_similarity`.
    /// Returns typed handles and similarity scores for zero-copy access.
    pub fn top_k_similar_text<'a>(
        &'a self,
        seed: &'a TextElement,
        start_idx: usize,
        end_idx: usize,
        k: usize,
    ) -> Vec<(TextHandle, f32)> {
        {
            if start_idx >= end_idx || start_idx >= self.doc_len() {
                return Vec::new();
            }
            let end_idx = end_idx.min(self.doc_len());

            // --- Seed feature -----------------------------------------------------
            let seed_feat = match TextFeatures::from_text_element(seed, self) {
                Some(f) => f,
                None => return Vec::new(),
            };
            let canonical_name = crate::fonts::canonicalize::canonicalize_font_name(
                seed.font_name.as_deref().unwrap_or_default(),
            );
            let seed_size = seed.font_size;
            const SIZE_TOLERANCE: f32 = 0.6;

            // --- Gather candidate indices by font bucket --------------------------
            let mut candidate_indices: Vec<usize> = Vec::new();
            for ((name, nn_size), usage) in &self.fonts {
                if name != &canonical_name {
                    continue;
                }
                if (nn_size.into_inner() - seed_size).abs() <= SIZE_TOLERANCE {
                    candidate_indices.extend(&usage.elements);
                }
            }

            // Optional: if we found too few, fall back to neighbour pages
            if candidate_indices.is_empty() {
                // Fallback to full slice (rare) - only text elements
                for i in start_idx..end_idx {
                    if let Some(ContentHandle::Text(_)) = self.order.get(i) {
                        candidate_indices.push(i);
                    }
                }
            }

            // --- Similarity heap --------------------------------------------------
            let mut heap: BinaryHeap<(std::cmp::Reverse<NotNan<f32>>, TextHandle)> =
                BinaryHeap::with_capacity(k);

            for &abs_idx in &candidate_indices {
                // Respect section bounds
                if abs_idx < start_idx || abs_idx >= end_idx {
                    continue;
                }
                if let Some(ContentHandle::Text(text_idx)) = self.order.get(abs_idx) {
                    let text_handle = TextHandle(*text_idx as u32);
                    let txt_ref = self.text(text_handle);
                    if txt_ref.id == seed.id {
                        continue; // skip self
                    }
                    // Convert TextElemRef to TextElement for feature computation
                    let txt = TextElement {
                        id: txt_ref.id,
                        text: txt_ref.text.to_string(),
                        font_size: txt_ref.font_size,
                        font_name: txt_ref.font_name.map(|s| s.to_string()),
                        bbox: txt_ref.bbox,
                        page_number: txt_ref.page_number,
                    };
                    let feat = match TextFeatures::from_text_element(&txt, self) {
                        Some(f) => f,
                        None => continue,
                    };
                    let sim = compute_similarity(&seed_feat, &feat);
                    let sim_notnan = match NotNan::new(sim) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if heap.len() < k {
                        heap.push((std::cmp::Reverse(sim_notnan), text_handle));
                    } else if let Some(&(std::cmp::Reverse(lowest), _)) = heap.peek() {
                        if sim_notnan.into_inner() > lowest.into_inner() {
                            heap.pop();
                            heap.push((std::cmp::Reverse(sim_notnan), text_handle));
                        }
                    }
                }
            }

            // Collect and sort descending - return handles for efficiency
            let mut results: Vec<(TextHandle, f32)> = heap
                .into_iter()
                .map(|(std::cmp::Reverse(sim), handle)| (handle, sim.into_inner()))
                .collect();
            results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            results
        }
    }

    /// Total number of sequential content items in document order
    #[inline]
    pub fn doc_len(&self) -> usize {
        self.order.len()
    }

    /// Get a single content item by its sequential document index
    #[inline]
    pub fn content_at(&self, idx: usize) -> Option<PageContent> {
        self.content_from_handle(idx)
    }

    /// Borrow a slice of content by sequential indices
    #[inline]
    pub fn content_slice(&self, start: usize, end: usize) -> Vec<PageContent> {
        (start..end.min(self.order.len()))
            .filter_map(|idx| self.content_from_handle(idx))
            .collect()
    }

    /// Find elements with a specific font and size range - cache-efficient using font index
    pub fn elements_by_font(
        &self,
        font_name_filter: Option<&str>,
        target_font_size: Option<f32>,
        min_size_overall: Option<f32>,
        max_size_overall: Option<f32>,
    ) -> Vec<PageContent> {
        let mut result_indices = HashSet::new();

        // Cache-efficient: use pre-computed average from SoA
        let avg_font_size_for_default = self.average_font_size();

        // Only apply overall size filters if no specific target size is provided
        let use_overall_size_filters = target_font_size.is_none();
        let min_s = if use_overall_size_filters {
            min_size_overall.unwrap_or(avg_font_size_for_default * 0.9)
        } else {
            0.0 // Don't filter if we have a specific target
        };
        let max_s = if use_overall_size_filters {
            max_size_overall.unwrap_or(avg_font_size_for_default * 1.1)
        } else {
            f32::MAX // Don't filter if we have a specific target
        };

        for ((name, nn_size), usage_data) in &self.fonts {
            let style_size = nn_size.into_inner();
            let name_matches = font_name_filter.map_or(true, |fname| name == fname);
            let target_size_matches =
                target_font_size.map_or(true, |tsize| (style_size - tsize).abs() < 0.1); // Check specific size if provided

            if name_matches && target_size_matches && style_size >= min_s && style_size <= max_s {
                result_indices.extend(&usage_data.elements);
            }
        }

        let indices_vec: Vec<usize> = result_indices.into_iter().collect();
        self.content_from_indices(&indices_vec)
    }

    /// Get font usage distribution, optionally scoped to a range of content indices
    pub fn get_font_usage_distribution(
        &self,
        start_index: Option<usize>,
        end_index: Option<usize>,
    ) -> HashMap<(String, NotNan<f32>), FontUsage> {
        let start = start_index.unwrap_or(0);
        let end = end_index.unwrap_or(self.order.len());

        let mut scoped_fonts: HashMap<(String, NotNan<f32>), FontUsage> = HashMap::new();

        // If no scoping, return the full index
        if start == 0 && end == self.order.len() {
            return self.fonts.clone();
        }

        // Cache-efficient: iterate over SoA text store in the specified range
        for (global_idx, text_elem) in self.text_store.iter().enumerate() {
            if global_idx >= start && global_idx < end {
                let canonical_font_name = crate::fonts::canonicalize::canonicalize_font_name(
                    text_elem.font_name.as_deref().unwrap_or_default(),
                );

                let font_style_key = (
                    canonical_font_name.clone(),
                    NotNan::new(text_elem.font_size).unwrap(),
                );

                let font_entry = scoped_fonts.entry(font_style_key).or_insert_with(|| {
                    FontUsage::new(
                        canonical_font_name,
                        text_elem.font_name.clone(),
                        text_elem.font_size,
                    )
                });
                font_entry.add_usage(global_idx);
            }
        }

        scoped_fonts
    }

    /// Get font size statistics - cache-efficient using SoA
    pub fn get_font_size_stats(
        &self,
        start_index: Option<usize>,
        end_index: Option<usize>,
    ) -> (f32, f32) {
        let start = start_index.unwrap_or(0);
        let end = end_index
            .unwrap_or(self.text_store.font_size.len())
            .min(self.text_store.font_size.len());

        if start >= end || self.text_store.font_size.is_empty() {
            return (12.0, 0.0); // Default mean, no deviation
        }

        // Cache-efficient: direct access to font_size column
        let font_sizes = &self.text_store.font_size[start..end];

        let mean = font_sizes.iter().sum::<f32>() / font_sizes.len() as f32;
        let variance = font_sizes
            .iter()
            .map(|&size| (size - mean).powi(2))
            .sum::<f32>()
            / font_sizes.len() as f32;
        let std_dev = variance.sqrt();

        (mean, std_dev)
    }

    /// Find fonts by z-score threshold (how many standard deviations above/below mean)
    pub fn find_fonts_by_z_score(
        &self,
        min_z_score: f32,
        start_index: Option<usize>,
        end_index: Option<usize>,
    ) -> Vec<((String, f32), u32, f32)> {
        let (mean, std_dev) = self.get_font_size_stats(start_index, end_index);
        let fonts_map = self.get_font_usage_distribution(start_index, end_index);

        fonts_map
            .into_iter()
            .filter_map(|((font_name, nn_font_size), usage_data)| {
                let font_size = nn_font_size.into_inner();
                let z_score = if std_dev > 0.0 {
                    (font_size - mean) / std_dev
                } else {
                    0.0
                };

                if z_score >= min_z_score {
                    Some(((font_name, font_size), usage_data.total_usage, z_score))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find fonts by usage frequency range
    pub fn find_fonts_by_usage_range(
        &self,
        min_usage: u32,
        max_usage: Option<u32>,
        start_index: Option<usize>,
        end_index: Option<usize>,
    ) -> Vec<((String, f32), u32)> {
        let fonts_map = self.get_font_usage_distribution(start_index, end_index);

        fonts_map
            .into_iter()
            .filter_map(|((font_name, nn_font_size), usage_data)| {
                let font_size = nn_font_size.into_inner();
                let usage = usage_data.total_usage;

                let meets_min = usage >= min_usage;
                let meets_max = max_usage.map_or(true, |max| usage <= max);

                if meets_min && meets_max {
                    Some(((font_name, font_size), usage))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get total text element count in scope - cache-efficient
    pub fn get_text_element_count(
        &self,
        start_index: Option<usize>,
        end_index: Option<usize>,
    ) -> u32 {
        let start = start_index.unwrap_or(0);
        let end = end_index.unwrap_or(self.order.len()).min(self.order.len());

        self.order[start..end]
            .iter()
            .filter(|handle| matches!(handle, ContentHandle::Text(_)))
            .count() as u32
    }

    /// Get elements between two marker elements using sequential ordering
    pub fn get_elements_between_markers(
        &self,
        start_element: &PageContent,
        end_element: Option<&PageContent>,
    ) -> Vec<PageContent> {
        let start_id = start_element.id();
        let end_id = end_element.map(|e| e.id());

        println!(
            "[get_elements_between_markers] Looking for start_id: {}",
            start_id
        );
        if let Some(end_id) = end_id {
            println!(
                "[get_elements_between_markers] Looking for end_id: {}",
                end_id
            );
        } else {
            println!("[get_elements_between_markers] No end element specified");
        }

        println!(
            "[get_elements_between_markers] element_id_to_index contains {} mappings",
            self.element_id_to_index.len()
        );

        let start_idx_inclusive = match self.element_id_to_index.get(&start_id) {
            Some(&idx) => {
                println!(
                    "[get_elements_between_markers] Found start_id at index: {}",
                    idx
                );
                idx
            }
            None => {
                println!(
                    "[get_elements_between_markers] Start element ID {} not found in index",
                    start_id
                );
                return Vec::new(); // Start element not found in index
            }
        };

        let end_idx_exclusive = match end_element {
            Some(end) => {
                let end_id = end.id();
                match self.element_id_to_index.get(&end_id) {
                    Some(&idx) => {
                        println!(
                            "[get_elements_between_markers] Found end_id at index: {}",
                            idx
                        );
                        idx // This index is exclusive for the slice
                    }
                    None => {
                        println!("[get_elements_between_markers] End element ID {} not found in index, using document end", end_id);
                        self.order.len() // End element not found, go to end of document
                    }
                }
            }
            None => {
                println!("[get_elements_between_markers] No end element, using document end");
                self.order.len() // No end element, go to end of document
            }
        };

        println!("[get_elements_between_markers] start_idx_inclusive: {}, end_idx_exclusive: {}, total_content_len: {}", 
                 start_idx_inclusive, end_idx_exclusive, self.order.len());

        // Now, start_idx_inclusive will be used directly for the slice start.
        // Ensure start_idx_inclusive is not past end_idx_exclusive or bounds.
        if start_idx_inclusive >= end_idx_exclusive || start_idx_inclusive >= self.order.len() {
            println!("[get_elements_between_markers] Invalid range: start {} >= end {} or start >= content_len {}", 
                     start_idx_inclusive, end_idx_exclusive, self.order.len());
            return Vec::new();
        }

        // Ensure the slice end is within bounds.
        let effective_end_idx = std::cmp::min(end_idx_exclusive, self.order.len());

        println!(
            "[get_elements_between_markers] Effective slice: [{}..{}]",
            start_idx_inclusive, effective_end_idx
        );

        // Use cache-efficient content_slice method
        let result = self.content_slice(start_idx_inclusive, effective_end_idx);

        println!(
            "[get_elements_between_markers] Returning {} elements",
            result.len()
        );
        result
    }

    /// Get elements after a specific marker element
    pub fn get_elements_after(&self, marker: &PageContent) -> Vec<PageContent> {
        if let Some(&idx) = self.element_id_to_index.get(&marker.id()) {
            self.content_slice(idx, self.order.len())
        } else {
            Vec::new()
        }
    }

    /// Get image by ID - cache-efficient using SoA
    pub fn get_image_by_id(&self, id: &Uuid) -> Option<ImageElement> {
        // Search through image store directly
        for img_elem in self.image_store.iter() {
            if img_elem.id == *id {
                return Some(img_elem);
            }
        }
        None
    }

    /// Calculate font size statistics - cache-efficient using SoA
    pub fn font_size_stats(&self) -> FontSizeStats {
        // Use pre-computed stats if available, otherwise compute from SoA
        self.font_size_stats.clone()
    }

    /// Find elements with statistically significant font sizes
    pub fn elements_by_font_size_percentile(&self, percentile: f32) -> Vec<PageContent> {
        let stats = self.font_size_stats();
        let threshold = stats.percentiles[3]; // Using 90th percentile as default

        // Cache-efficient: iterate through font_size column directly
        let mut results = Vec::new();
        for (idx, &font_size) in self.text_store.font_size.iter().enumerate() {
            if font_size >= threshold {
                if let Some(text_elem) = self.text_store.get(idx) {
                    results.push(PageContent::Text(text_elem));
                }
            }
        }
        results
    }

    /// Find elements that are likely section boundaries - cache-efficient
    pub fn find_potential_section_boundaries(&self) -> Vec<PageContent> {
        let stats = self.font_size_stats();
        let threshold = stats.mean + (stats.std_dev * 1.5); // Elements > 1.5 standard deviations above mean

        // Cache-efficient: iterate through font_size column directly
        let mut results = Vec::new();
        for (idx, &font_size) in self.text_store.font_size.iter().enumerate() {
            if font_size >= threshold {
                if let Some(text_elem) = self.text_store.get(idx) {
                    results.push(PageContent::Text(text_elem));
                }
            }
        }
        results
    }

    #[inline]
    pub fn features_for<'a>(&'a self, txt: &'a TextElement) -> TextFeatures {
        if let Some(f) = self.feature_cache.get(&txt.id) {
            return f.clone();
        }
        let feat = TextFeatures::from_text_element(txt, self)
            .expect("feature extraction should never fail");
        self.feature_cache.insert(txt.id, feat.clone());
        feat
    }

    /// Zero-copy access to text element via typed handle
    #[inline]
    pub fn text(&self, h: TextHandle) -> TextElemRef<'_> {
        let i = h.0 as usize;
        TextElemRef {
            id: self.text_store.id[i],
            text: &self.text_store.text[i],
            font_size: self.text_store.font_size[i],
            font_name: self.text_store.font_name[i].as_deref(),
            bbox: self.text_store.bbox[i],
            page_number: self.text_store.page_number[i],
        }
    }

    /// Zero-copy access to image element via typed handle
    #[inline]
    pub fn image(&self, h: ImageHandle) -> ImageElemRef<'_> {
        let i = h.0 as usize;
        ImageElemRef {
            id: self.image_store.id[i],
            bbox: self.image_store.bbox[i],
            page_number: self.image_store.page_number[i],
            image_object: &self.image_store.image_object[i],
        }
    }

    /// Get typed handle from document index
    pub fn get_handle(&self, doc_idx: usize) -> Option<ContentHandle> {
        self.order.get(doc_idx).copied()
    }

    /// Convert ContentHandle to typed handles
    pub fn as_text_handle(&self, handle: ContentHandle) -> Option<TextHandle> {
        match handle {
            ContentHandle::Text(idx) => Some(TextHandle(idx as u32)),
            ContentHandle::Image(_) => None,
        }
    }

    pub fn as_image_handle(&self, handle: ContentHandle) -> Option<ImageHandle> {
        match handle {
            ContentHandle::Text(_) => None,
            ContentHandle::Image(idx) => Some(ImageHandle(idx as u32)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FontSizeStats {
    pub mean: f32,
    pub std_dev: f32,
    pub percentiles: [f32; 5], // [25th, 50th, 75th, 90th, 95th]
}

impl FontSizeStats {
    pub fn compute(content: &[PageContent]) -> Self {
        let mut sizes: Vec<f32> = content
            .iter()
            .filter_map(|e| e.as_text().map(|t| t.font_size))
            .collect();
        if sizes.is_empty() {
            return FontSizeStats {
                mean: 12.0,
                std_dev: 0.0,
                percentiles: [12.0; 5],
            };
        }
        sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mean = sizes.iter().sum::<f32>() / sizes.len() as f32;
        let var = sizes.iter().map(|&x| (x - mean).powi(2)).sum::<f32>() / sizes.len() as f32;
        let sd = var.sqrt();
        let idx = |p: f32| sizes[((sizes.len() as f32) * p) as usize];
        FontSizeStats {
            mean,
            std_dev: sd,
            percentiles: [idx(0.25), idx(0.50), idx(0.75), idx(0.90), idx(0.95)],
        }
    }
}
