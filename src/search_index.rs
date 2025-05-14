use crate::{
    layout::{MatchContext, TextLine},
    parse::{ImageElement, PageContent, TextElement},
};
use lopdf::Object;
use ordered_float::NotNan;
use rstar::{RTree, RTreeObject, AABB};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

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
    pub all_ordered_content: Vec<PageContent>,
    pub by_page: BTreeMap<u32, Vec<usize>>,
    pub font_size_index: Vec<(f32, usize)>,
    pub reference_count_index: Vec<(u32, usize)>,
    pub spatial_rtree: RTree<SpatialPageContent>,
    pub element_id_to_index: HashMap<Uuid, usize>,
    pub fonts: HashMap<(String, NotNan<f32>), FontUsage>,
    pub font_name_frequency_index: Vec<(u32, String)>,
}

impl PdfIndex {
    pub fn new(page_map: &BTreeMap<u32, Vec<PageContent>>, _match_context: &MatchContext) -> Self {
        let mut all_ordered_content = Vec::new();
        let mut by_page = BTreeMap::new();
        let mut font_size_index_construction = Vec::new(); // Temp for construction before sorting
        let mut spatial_elements = Vec::new();
        let mut element_id_to_index = HashMap::new();
        let mut fonts_map: HashMap<(String, NotNan<f32>), FontUsage> = HashMap::new(); // Correctly typed

        let mut current_content_index = 0;

        for (page_number, page_contents_on_page) in page_map {
            let mut page_element_indices = Vec::new();

            for content_item in page_contents_on_page {
                all_ordered_content.push(content_item.clone());
                page_element_indices.push(current_content_index);
                element_id_to_index.insert(content_item.id(), current_content_index);
                spatial_elements.push(SpatialPageContent::new(content_item.clone()));

                if let PageContent::Text(text_elem) = content_item {
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
        }

        font_size_index_construction
            .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let font_size_index = font_size_index_construction; // Assign after sorting

        let spatial_rtree = RTree::bulk_load(spatial_elements);
        let reference_count_index = Vec::new();

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
            all_ordered_content,
            by_page,
            font_size_index,
            reference_count_index,
            spatial_rtree,
            element_id_to_index,
            fonts: fonts_map, // Use the correctly populated map
            font_name_frequency_index,
        }
    }

    /// Sort the font frequency index
    fn sort_font_frequency_index(&mut self) {
        self.font_name_frequency_index.sort_by(|a, b| {
            // Sort by frequency (descending)
            b.0.cmp(&a.0)
        });
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
        for idx in 0..self.all_ordered_content.len() {
            let count = reference_counts.get(&idx).copied().unwrap_or(0);
            self.reference_count_index.push((count, idx));
        }

        // Sort by reference count
        self.reference_count_index.sort_by_key(|&(count, _)| count);
    }

    /// Find elements on a specific page
    pub fn elements_on_page(&self, page_num: u32) -> Vec<&PageContent> {
        if let Some(indices) = self.by_page.get(&page_num) {
            indices
                .iter()
                .map(|&idx| &self.all_ordered_content[idx])
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Find elements with font size in a specific range
    pub fn elements_by_font_size(&self, min_size: f32, max_size: f32) -> Vec<&PageContent> {
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

        self.font_size_index[lower_idx..upper_idx]
            .iter()
            .map(|&(_, idx)| &self.all_ordered_content[idx])
            .collect()
    }

    /// Find elements with at least the specified number of references
    pub fn elements_by_reference_count(&self, min_count: u32) -> Vec<&PageContent> {
        // Binary search for the lower bound
        let lower_idx = self
            .reference_count_index
            .binary_search_by_key(&min_count, |&(count, _)| count)
            .unwrap_or_else(|idx| idx);

        self.reference_count_index[lower_idx..]
            .iter()
            .map(|&(_, idx)| &self.all_ordered_content[idx])
            .collect()
    }

    /// Find elements within a specified rectangular region
    pub fn elements_in_region(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<&PageContent> {
        let query_rect = AABB::from_corners([x0, y0], [x1, y1]);

        self.spatial_rtree
            .locate_in_envelope(&query_rect)
            .map(|spatial_elem| &spatial_elem.element)
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
    ) -> Vec<&PageContent> {
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

        // Convert result indices to TextElements
        match result_indices {
            Some(indices) => indices
                .into_iter()
                .map(|idx| &self.all_ordered_content[idx])
                .collect::<Vec<&PageContent>>(),
            None => self.all_ordered_content.iter().collect(), // If no filters applied, return all elements
        }
    }

    /// Find potential section headings based on font size and reference count
    // pub fn find_potential_section_headings(&self) -> Vec<&PageContent> {
    //     // Strategy: Find elements with larger than average font size and with references

    //     // Calculate average font size
    //     let avg_font_size: f32 = if self.elements.is_empty() {
    //         12.0 // Default if no elements
    //     } else {
    //         self.elements.iter().map(|e| e.font_size).sum::<f32>() / self.elements.len() as f32
    //     };

    //     // Find elements with font size > avg and at least one reference
    //     self.search(
    //         None,
    //         Some(avg_font_size * 1.2), // 20% larger than average
    //         None,
    //         Some(1), // At least one reference
    //         None,
    //     )
    // }

    /// Get TextElement by ID
    pub fn get_element_by_id(&self, id: &Uuid) -> Option<&PageContent> {
        self.element_id_to_index
            .get(id)
            .map(|&idx| &self.all_ordered_content[idx])
    }

    /// Update the index with a new MatchContext
    pub fn update_with_match_context(&mut self, match_context: &MatchContext) {
        self.update_reference_counts(match_context);
    }

    /// Find elements that might match a text string (simple search)
    pub fn find_text_matches(
        &self,
        text: &str,
        threshold: f64,
        start_content_index: Option<usize>,
    ) -> Vec<(&PageContent, f64)> {
        use strsim::normalized_levenshtein;
        let start = start_content_index.unwrap_or(0);
        if start >= self.all_ordered_content.len() {
            return Vec::new();
        }
        self.all_ordered_content[start..]
            .iter()
            .filter_map(|element| match element {
                PageContent::Text(text_elem) => {
                    let score = normalized_levenshtein(text, &text_elem.text);
                    if score >= threshold {
                        Some((element, score))
                    } else {
                        None
                    }
                }
                _ => None, // Skip images for text matching
            })
            .collect()
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

    pub fn average_font_size(&self) -> f32 {
        if self.all_ordered_content.is_empty() {
            12.0
        } else {
            self.all_ordered_content
                .iter()
                .map(|e| e.as_text().unwrap().font_size)
                .sum::<f32>()
                / self.all_ordered_content.len() as f32
        }
    }

    /// Find elements with a specific font and size range
    ///
    /// # Arguments
    ///
    /// * `font_id`: Option<&str> - The font ID to filter by
    /// * `min_size`: Option<f32> - The minimum font size to filter by. Defaults to average font size * 0.9
    /// * `max_size`: Option<f32> - The maximum font size to filter by. Defaults to average font size * 1.1
    /// * `min_frequency`: Option<u32> - The minimum frequency to filter by
    pub fn elements_by_font(
        &self,
        font_name_filter: Option<&str>,
        target_font_size: Option<f32>,
        min_size_overall: Option<f32>,
        max_size_overall: Option<f32>,
    ) -> Vec<&PageContent> {
        let mut result_indices = HashSet::new();

        let avg_font_size_for_default: f32 = {
            let text_font_sizes_vec: Vec<f32> = self
                .all_ordered_content
                .iter()
                .filter_map(|e| e.font_size())
                .collect();
            if text_font_sizes_vec.is_empty() {
                12.0
            } else {
                text_font_sizes_vec.iter().sum::<f32>() / text_font_sizes_vec.len() as f32
            }
        };

        let min_s = min_size_overall.unwrap_or(avg_font_size_for_default * 0.9);
        let max_s = max_size_overall.unwrap_or(avg_font_size_for_default * 1.1);

        for ((name, nn_size), usage_data) in &self.fonts {
            let style_size = nn_size.into_inner();
            let name_matches = font_name_filter.map_or(true, |fname| name == fname);
            let target_size_matches =
                target_font_size.map_or(true, |tsize| (style_size - tsize).abs() < 0.1); // Check specific size if provided

            if name_matches && target_size_matches && style_size >= min_s && style_size <= max_s {
                result_indices.extend(&usage_data.elements);
            }
        }

        result_indices
            .into_iter()
            .map(|idx: usize| &self.all_ordered_content[idx])
            .collect()
    }

    /// Find potential heading levels based on font analysis
    pub fn identify_heading_levels(&self, max_levels: usize) -> Vec<((String, f32), u32)> {
        // Calculate a more robust average font size, less skewed by unique large titles.
        let mut sizes_for_avg_calc: Vec<f32> = Vec::new();
        for ((_font_name, nn_font_size), usage_data) in &self.fonts {
            // Include font sizes that appear more than once, or if they are not excessively large
            // This is a heuristic to try and get a more representative "body/common" text average.
            let style_size = nn_font_size.into_inner();
            if usage_data.total_usage > 1 {
                // If used more than once, include it
                for _ in 0..usage_data.total_usage {
                    // Weight by usage for average
                    sizes_for_avg_calc.push(style_size);
                }
            } else {
                // If used once, only include if it's not extremely large (e.g., > 30pt, assuming titles are often >30pt)
                // This threshold (30.0) is arbitrary and might need tuning.
                if style_size <= 30.0 {
                    sizes_for_avg_calc.push(style_size);
                }
            }
        }

        let avg_font_size: f32 = if sizes_for_avg_calc.is_empty() {
            12.0 // Default if no suitable sizes found for averaging
        } else {
            sizes_for_avg_calc.iter().sum::<f32>() / sizes_for_avg_calc.len() as f32
        };
        println!(
            "[identify_heading_levels] Calculated avg_font_size (robust): {}",
            avg_font_size
        );

        let mut candidates = Vec::new();
        for ((font_name, nn_font_size), usage_data) in &self.fonts {
            let current_style_font_size = nn_font_size.into_inner();

            let text_elements_count = self
                .all_ordered_content
                .iter()
                .filter(|e| e.is_text())
                .count() as u32;
            let min_abs_usage = 2; // Must appear at least twice
                                   // Max 20% of text elements for a heading style, but ensure threshold is at least 1 if count > 0
            let max_rel_usage_threshold = if text_elements_count > 0 {
                std::cmp::max(1, text_elements_count / 5)
            } else {
                0
            };

            println!("[identify_heading_levels] Checking style: ({}, {}), usage: {}, avg_fs: {}, text_count: {}, max_rel_thresh: {}", 
                font_name, current_style_font_size, usage_data.total_usage, avg_font_size, text_elements_count, max_rel_usage_threshold);

            if usage_data.total_usage >= min_abs_usage
                && (max_rel_usage_threshold == 0
                    || usage_data.total_usage <= max_rel_usage_threshold)
                && current_style_font_size > avg_font_size * 1.1
            // Adjusted factor to 1.1
            {
                println!("    -> Candidate ACCEPTED");
                candidates.push((
                    (font_name.clone(), current_style_font_size),
                    usage_data.total_usage,
                ));
            } else {
                println!(
                    "    -> Candidate REJECTED (usage: {}, size_check: {}, rel_usage_check: {})",
                    usage_data.total_usage >= min_abs_usage,
                    current_style_font_size > avg_font_size * 1.1,
                    (max_rel_usage_threshold == 0
                        || usage_data.total_usage <= max_rel_usage_threshold)
                );
            }
        }
        candidates.sort_by(|a, b| {
            b.0 .1
                .partial_cmp(&a.0 .1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.cmp(&a.1))
        });
        candidates.into_iter().take(max_levels).collect()
    }

    /// Find elements that might be at a specific heading level
    pub fn find_elements_at_heading_level(&self, level: usize) -> Vec<&PageContent> {
        let heading_levels = self.identify_heading_levels(level + 1);

        if level >= heading_levels.len() {
            return Vec::new();
        }

        let ((font_name, font_size), _usage_count) = &heading_levels[level];

        self.elements_by_font(Some(font_name), Some(*font_size), None, None)
    }

    /// Get elements between two marker elements using sequential ordering
    pub fn get_elements_between_markers(
        &self,
        start_element: &PageContent,
        end_element: Option<&PageContent>,
    ) -> Vec<&PageContent> {
        let start_idx_inclusive = match self.element_id_to_index.get(&start_element.id()) {
            Some(&idx) => idx,
            None => return Vec::new(), // Start element not found in index
        };

        let end_idx_exclusive = match end_element {
            Some(end) => match self.element_id_to_index.get(&end.id()) {
                Some(&idx) => idx,                      // This index is exclusive for the slice
                None => self.all_ordered_content.len(), // End element not found, go to end of document
            },
            None => self.all_ordered_content.len(), // No end element, go to end of document
        };

        // Now, start_idx_inclusive will be used directly for the slice start.
        // Ensure start_idx_inclusive is not past end_idx_exclusive or bounds.
        if start_idx_inclusive >= end_idx_exclusive
            || start_idx_inclusive >= self.all_ordered_content.len()
        {
            return Vec::new();
        }

        // Ensure the slice end is within bounds.
        // end_idx_exclusive can be self.all_ordered_content.len(), which is fine for slicing.
        let effective_end_idx = std::cmp::min(end_idx_exclusive, self.all_ordered_content.len());

        // Slice is [start_idx_inclusive..effective_end_idx]
        self.all_ordered_content[start_idx_inclusive..effective_end_idx]
            .iter()
            .collect()
    }

    /// Get elements after a specific marker element
    pub fn get_elements_after(&self, marker: &PageContent) -> Vec<&PageContent> {
        if let Some(&idx) = self.element_id_to_index.get(&marker.id()) {
            self.all_ordered_content[idx..].iter().collect()
        } else {
            Vec::new()
        }
    }

    /// Get image by ID
    pub fn get_image_by_id(&self, id: &Uuid) -> Option<&ImageElement> {
        self.all_ordered_content.iter().find_map(|pc| match pc {
            PageContent::Image(img_elem) if img_elem.id == *id => Some(img_elem),
            _ => None,
        })
    }

    /// Calculate font size statistics including mean, standard deviation, and percentiles
    pub fn font_size_stats(&self) -> FontSizeStats {
        let mut sizes: Vec<f32> = self
            .all_ordered_content
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

        sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let mean = sizes.iter().sum::<f32>() / sizes.len() as f32;
        let variance = sizes.iter().map(|&x| (x - mean).powi(2)).sum::<f32>() / sizes.len() as f32;
        let std_dev = variance.sqrt();

        // Calculate percentiles (25th, 50th, 75th, 90th, 95th)
        let percentiles = [
            sizes[(sizes.len() as f32 * 0.25) as usize],
            sizes[(sizes.len() as f32 * 0.50) as usize],
            sizes[(sizes.len() as f32 * 0.75) as usize],
            sizes[(sizes.len() as f32 * 0.90) as usize],
            sizes[(sizes.len() as f32 * 0.95) as usize],
        ];

        FontSizeStats {
            mean,
            std_dev,
            percentiles,
        }
    }

    /// Find elements with statistically significant font sizes
    /// Returns elements whose font size is above the specified percentile
    pub fn elements_by_font_size_percentile(&self, percentile: f32) -> Vec<&PageContent> {
        let stats = self.font_size_stats();
        let threshold = stats.percentiles[3]; // Using 90th percentile as default

        self.all_ordered_content
            .iter()
            .filter(|e| e.as_text().map_or(false, |t| t.font_size >= threshold))
            .collect()
    }

    /// Find elements that are likely section boundaries based on font size distribution
    pub fn find_potential_section_boundaries(&self) -> Vec<&PageContent> {
        let stats = self.font_size_stats();
        let threshold = stats.mean + (stats.std_dev * 1.5); // Elements > 1.5 standard deviations above mean

        self.all_ordered_content
            .iter()
            .filter(|e| e.as_text().map_or(false, |t| t.font_size >= threshold))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct FontSizeStats {
    pub mean: f32,
    pub std_dev: f32,
    pub percentiles: [f32; 5], // [25th, 50th, 75th, 90th, 95th]
}
