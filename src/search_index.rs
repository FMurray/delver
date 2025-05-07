use crate::{
    layout::{MatchContext, TextLine},
    parse::TextElement,
};
use lopdf::Object;
use multi_index_map::MultiIndexMap;
use rstar::{RTree, RTreeObject, AABB};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

// Wrapper for TextElement to implement RTreeObject
#[derive(Clone, Debug)]
pub struct SpatialElement {
    element: TextElement,
}

impl RTreeObject for SpatialElement {
    type Envelope = AABB<[f32; 2]>;

    fn envelope(&self) -> Self::Envelope {
        let bbox = &self.element.bbox;
        AABB::from_corners([bbox.0, bbox.1], [bbox.2, bbox.3])
    }
}

impl SpatialElement {
    fn new(element: TextElement) -> Self {
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

#[derive(Debug, MultiIndexMap)]
pub struct PdfIndex {
    pub elements: Vec<TextElement>,
    pub by_page: BTreeMap<u32, Vec<usize>>, // Indices by page number
    pub font_size_index: Vec<(f32, usize)>, // Sorted vector for binary search
    pub reference_count_index: Vec<(u32, usize)>, // Count of references (destinations pointing to element)
    pub rtree: RTree<SpatialElement>,             // Spatial index for bounding box queries
    pub element_id_to_index: HashMap<Uuid, usize>, // Map from element ID to index
    pub fonts: HashMap<String, FontUsage>,        // Font usage analysis
    pub font_frequency_index: Vec<(u32, String)>, // Fonts sorted by frequency
}

impl PdfIndex {
    pub fn new(page_map: &BTreeMap<u32, Vec<TextElement>>, match_context: &MatchContext) -> Self {
        let mut elements = Vec::new();
        let mut by_page = BTreeMap::new();
        let mut font_size_index = Vec::new();
        let mut spatial_elements = Vec::new();
        let mut element_id_to_index = HashMap::new();
        let mut fonts = HashMap::new();

        // First, collect all elements across all pages
        for (page_num, page_elements) in page_map {
            let start_idx = elements.len();
            let indices: Vec<usize> = (start_idx..start_idx + page_elements.len()).collect();
            by_page.insert(*page_num, indices);

            for element in page_elements {
                let element_idx = elements.len();

                // Add to element ID map
                element_id_to_index.insert(element.id, element_idx);

                // Add to font size index
                font_size_index.push((element.font_size, element_idx));

                // Track font usage
                let font_name = element.font_name.clone().unwrap_or_default();
                let font_key = format!("{}_{:.2}", font_name, element.font_size);

                let font_usage = fonts.entry(font_key).or_insert_with(|| {
                    FontUsage::new(
                        font_name.clone(),
                        element.font_name.clone(),
                        element.font_size,
                    )
                });
                font_usage.add_usage(element_idx);

                // Create a spatial element for RTree
                spatial_elements.push(SpatialElement::new(element.clone()));

                // Add to main elements vector
                elements.push(element.clone());
            }
        }

        // Sort indices by their values for binary search
        font_size_index.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        // Create font frequency index
        let font_frequency_index = fonts
            .iter()
            .map(|(id, usage)| (usage.total_usage, id.clone()))
            .collect::<Vec<_>>();

        // Build the RTree
        let rtree = RTree::bulk_load(spatial_elements);

        // Create the index first so we can use its methods
        let mut index = Self {
            elements,
            by_page,
            font_size_index,
            reference_count_index: Vec::new(),
            rtree,
            element_id_to_index,
            fonts,
            font_frequency_index,
        };

        // Sort the font frequency index
        index.sort_font_frequency_index();

        // Build the reference count index using the correct destination matching logic
        index.update_reference_counts(match_context);

        index
    }

    /// Sort the font frequency index
    fn sort_font_frequency_index(&mut self) {
        self.font_frequency_index.sort_by(|a, b| {
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
                        .rtree
                        .locate_in_envelope(&search_region)
                        .filter(|spatial_elem| spatial_elem.element.page_number == dest_page)
                        .filter_map(|spatial_elem| {
                            self.element_id_to_index
                                .get(&spatial_elem.element.id)
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
        for idx in 0..self.elements.len() {
            let count = reference_counts.get(&idx).copied().unwrap_or(0);
            self.reference_count_index.push((count, idx));
        }

        // Sort by reference count
        self.reference_count_index.sort_by_key(|&(count, _)| count);
    }

    /// Find elements on a specific page
    pub fn elements_on_page(&self, page_num: u32) -> Vec<&TextElement> {
        if let Some(indices) = self.by_page.get(&page_num) {
            indices.iter().map(|&idx| &self.elements[idx]).collect()
        } else {
            Vec::new()
        }
    }

    /// Find elements with font size in a specific range
    pub fn elements_by_font_size(&self, min_size: f32, max_size: f32) -> Vec<&TextElement> {
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
            .map(|&(_, idx)| &self.elements[idx])
            .collect()
    }

    /// Find elements with at least the specified number of references
    pub fn elements_by_reference_count(&self, min_count: u32) -> Vec<&TextElement> {
        // Binary search for the lower bound
        let lower_idx = self
            .reference_count_index
            .binary_search_by_key(&min_count, |&(count, _)| count)
            .unwrap_or_else(|idx| idx);

        self.reference_count_index[lower_idx..]
            .iter()
            .map(|&(_, idx)| &self.elements[idx])
            .collect()
    }

    /// Find elements within a specified rectangular region
    pub fn elements_in_region(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<&TextElement> {
        let query_rect = AABB::from_corners([x0, y0], [x1, y1]);

        self.rtree
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
    ) -> Vec<&TextElement> {
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
                .rtree
                .locate_in_envelope(&query_rect)
                .filter_map(|spatial_elem| {
                    self.element_id_to_index
                        .get(&spatial_elem.element.id)
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
                .map(|idx| &self.elements[idx])
                .collect::<Vec<&TextElement>>(),
            None => self.elements.iter().collect(), // If no filters applied, return all elements
        }
    }

    /// Find potential section headings based on font size and reference count
    pub fn find_potential_section_headings(&self) -> Vec<&TextElement> {
        // Strategy: Find elements with larger than average font size and with references

        // Calculate average font size
        let avg_font_size: f32 = if self.elements.is_empty() {
            12.0 // Default if no elements
        } else {
            self.elements.iter().map(|e| e.font_size).sum::<f32>() / self.elements.len() as f32
        };

        // Find elements with font size > avg and at least one reference
        self.search(
            None,
            Some(avg_font_size * 1.2), // 20% larger than average
            None,
            Some(1), // At least one reference
            None,
        )
    }

    /// Get TextElement by ID
    pub fn get_element_by_id(&self, id: &Uuid) -> Option<&TextElement> {
        self.element_id_to_index
            .get(id)
            .map(|&idx| &self.elements[idx])
    }

    /// Update the index with a new MatchContext
    pub fn update_with_match_context(&mut self, match_context: &MatchContext) {
        self.update_reference_counts(match_context);
    }

    /// Find elements that might match a text string (simple search)
    pub fn find_text_matches(&self, text: &str, threshold: f64) -> Vec<(&TextElement, f64)> {
        use strsim::normalized_levenshtein;

        self.elements
            .iter()
            .map(|element| {
                let score = normalized_levenshtein(text, &element.text);
                (element, score)
            })
            .filter(|(_, score)| *score >= threshold)
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

    /// Find elements with a specific font and size range
    pub fn elements_by_font(
        &self,
        font_id: Option<&str>,
        min_size: Option<f32>,
        max_size: Option<f32>,
        min_frequency: Option<u32>,
    ) -> Vec<&TextElement> {
        let mut result_indices = HashSet::<usize>::new();

        // First, filter by font ID if specified
        let candidate_fonts: Vec<&FontUsage> = if let Some(id) = font_id {
            if let Some(font) = self.fonts.get(id) {
                vec![font]
            } else {
                return Vec::new(); // Font ID not found
            }
        } else {
            // If no specific font ID, filter by frequency if requested
            if let Some(min_freq) = min_frequency {
                self.font_frequency_index
                    .iter()
                    .filter(|(freq, _)| *freq >= min_freq)
                    .filter_map(|(_, id)| self.fonts.get(id))
                    .collect()
            } else {
                self.fonts.values().collect()
            }
        };

        // For each candidate font, check if it matches the size criteria
        for font in candidate_fonts {
            // Check if this font's size is in range
            if (min_size.is_none() || font.font_size >= min_size.unwrap())
                && (max_size.is_none() || font.font_size <= max_size.unwrap_or(f32::MAX))
            {
                // Add all elements that use this font+size
                result_indices.extend(&font.elements);
            }
        }

        // Convert indices to elements
        result_indices
            .into_iter()
            .map(|idx| &self.elements[idx])
            .collect::<Vec<&TextElement>>()
    }

    /// Find potential heading levels based on font analysis
    pub fn identify_heading_levels(&self, max_levels: usize) -> Vec<(String, f32, u32)> {
        let avg_font_size: f32 = if self.elements.is_empty() {
            12.0
        } else {
            self.elements.iter().map(|e| e.font_size).sum::<f32>() / self.elements.len() as f32
        };

        let mut candidates = Vec::new();
        for (font_id, usage) in &self.fonts {
            if usage.total_usage >= 3
                && usage.total_usage <= self.elements.len() as u32 / 10
                && usage.font_size > avg_font_size * 1.1
            {
                candidates.push((font_id.clone(), usage.font_size, usage.total_usage));
            }
        }

        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        candidates.into_iter().take(max_levels).collect()
    }

    /// Find elements that might be at a specific heading level
    pub fn find_elements_at_heading_level(&self, level: usize) -> Vec<&TextElement> {
        let heading_levels = self.identify_heading_levels(6); // Assume up to 6 heading levels

        if level >= heading_levels.len() {
            return Vec::new(); // Level not found
        }

        let (font_id, size, _) = &heading_levels[level];

        // Find elements with this font and size
        self.elements_by_font(
            Some(font_id),
            Some(*size * 0.98), // Allow small variations
            Some(*size * 1.02),
            None,
        )
    }

    /// Get elements between two marker elements using sequential ordering
    pub fn get_elements_between_markers(
        &self,
        start_element: &TextElement,
        end_element: Option<&TextElement>,
    ) -> Vec<&TextElement> {
        // Find indices of markers
        let start_idx = match self.element_id_to_index.get(&start_element.id) {
            Some(&idx) => idx,
            None => return Vec::new(),
        };

        let end_idx = match end_element {
            Some(end) => match self.element_id_to_index.get(&end.id) {
                Some(&idx) => idx,
                None => self.elements.len(),
            },
            None => self.elements.len(),
        };

        // Return elements between these indices (inclusive start, exclusive end)
        self.elements[start_idx..end_idx].iter().collect()
    }

    /// Get elements after a specific marker element
    pub fn get_elements_after(&self, marker: &TextElement) -> Vec<&TextElement> {
        if let Some(&idx) = self.element_id_to_index.get(&marker.id) {
            self.elements[idx..].iter().collect()
        } else {
            Vec::new()
        }
    }
}
