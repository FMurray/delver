//! # Document Search Index
//!
//! This module provides efficient indexing and search capabilities for PDF document content.
//! It uses multiple index structures including spatial R-trees, font-based indexes, and
//! style-based buckets to enable fast similarity searches and content retrieval.
//!
//! The search index is designed to support the template matching algorithm by providing
//! O(1) and O(log n) access patterns for common search operations.

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

const DEFAULT_PAGE_WIDTH: f32 = 612.0;

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

// ─────────────────────────────────────────────────────────────────────────────
// NEW: Efficient style-based similarity search structures
// ─────────────────────────────────────────────────────────────────────────────

/// StyleKey – 64‑bit packed signature used as inverted‑index key for efficient similarity search
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
struct StyleKey(u64);

impl StyleKey {
    #[inline]
    fn new(
        font_id: u16,
        size_bin: u16,
        z_bin: i8,
        pos_bin: u8,
        caps: bool,
        title: bool,
    ) -> Self {
        let mut v = 0u64;
        v |= font_id as u64;               // bits 0‑15
        v |= (size_bin as u64) << 16;      // 16‑31
        v |= ((z_bin as i16) as u64) << 32;// 32‑39 (sign‑extended)
        v |= (pos_bin as u64) << 40;       // 40‑43
        v |= (caps as u64) << 48;          // 48
        v |= (title as u64) << 49;         // 49
        StyleKey(v)
    }

    #[inline] pub fn font_id(self)  -> u16 {  (self.0        & 0xFFFF) as u16 }
    #[inline] pub fn size_bin(self) -> u16 { ((self.0 >> 16) & 0xFFFF) as u16 }
    #[inline] pub fn z_bin(self)    -> i8  { ((self.0 >> 32) as i16) as i8 }
    #[inline] pub fn pos_bin(self)  -> u8  { ((self.0 >> 40) & 0x0F)  as u8 }
    #[inline] pub fn caps(self)     -> bool{ ((self.0 >> 48) & 0x1) != 0 }
    #[inline] pub fn title(self)    -> bool{ ((self.0 >> 49) & 0x1) != 0 }

    #[inline]
    fn with_bins(self, new_z: i8, new_pos: u8) -> Self {
        StyleKey::new(
            self.font_id(),
            self.size_bin(),
            new_z,
            new_pos,
            self.caps(),
            self.title(),
        )
    }
}

/// Very small font interner for efficient font ID assignment
fn intern_font(opt_name: &Option<String>) -> u16 {
    use once_cell::sync::Lazy;
    use std::sync::Mutex;
    static INTERN: Lazy<Mutex<HashMap<String,u16>>> = Lazy::new(|| Mutex::new(HashMap::new()));
    let name = crate::fonts::canonicalize::canonicalize_font_name(opt_name.as_deref().unwrap_or(""));
    let mut map = INTERN.lock().unwrap();
    if let Some(&id) = map.get(&name) { id }
    else {
        let id = map.len() as u16;
        map.insert(name, id);
        id
    }
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

/// Multi-index structure for efficient PDF document content search and retrieval
///
/// The PdfIndex uses a Struct-of-Arrays (SoA) design for cache efficiency and supports
/// multiple query patterns including spatial, textual, and style-based searches.
#[derive(Debug)]
pub struct PdfIndex {
    /// Elements organized by page number for page-based queries
    pub by_page: BTreeMap<u32, Vec<usize>>,
    /// Font size index for range queries, sorted by size
    pub font_size_index: Vec<(f32, usize)>,
    /// Reference count index for finding frequently referenced elements
    pub reference_count_index: Vec<(u32, usize)>,
    /// Spatial R-tree for region-based queries
    pub spatial_rtree: RTree<SpatialPageContent>,
    /// Maps element UUIDs to document indices for fast lookup
    pub element_id_to_index: HashMap<Uuid, usize>,
    /// Document sequence preserving original order (SoA handles)
    pub order: Vec<ContentHandle>,
    /// Structure-of-Arrays storage for text elements
    pub text_store: TextStore,
    /// Structure-of-Arrays storage for image elements
    pub image_store: ImageStore,
    /// Font usage analysis by (name, size) pairs
    pub fonts: HashMap<(String, NotNan<f32>), FontUsage>,
    /// Font names sorted by usage frequency
    pub font_name_frequency_index: Vec<(u32, String)>,
    /// Precomputed font size statistics
    pub font_size_stats: FontSizeStats,
    /// Style keys for each text element (private)
    style_key: Vec<StyleKey>,
    /// Style-based buckets for O(1) similarity lookup (private)
    style_buckets: HashMap<StyleKey, Vec<TextHandle>>,
    /// Total usage count per font name (private)
    font_name_totals: HashMap<String, u32>,
    /// Sorted Y positions per page for percentile calculations (private)
    page_y_values: HashMap<u32, Vec<f32>>,
}

impl PdfIndex {
    pub fn new(page_map: &BTreeMap<u32, PageContents>, _match_context: &MatchContext) -> Self {
        // PASS 1: Ingest raw content, collect per‑row basics and aggregates
        let mut by_page = BTreeMap::new();
        let mut font_size_index_construction = Vec::new();
        let mut spatial_elements = Vec::new();
        let mut element_id_to_index = HashMap::new();
        let mut fonts_map: HashMap<(String, NotNan<f32>), FontUsage> = HashMap::new();

        let mut current_content_index = 0;

        // Aggregate all SoA data from PageContents
        let mut order: Vec<ContentHandle> = Vec::new();
        let mut text_store = TextStore::default();
        let mut image_store = ImageStore::default();
        
        // NEW: Collect data for statistics
        let mut font_sizes = Vec::new();
        let mut page_y_values: HashMap<u32, Vec<f32>> = HashMap::new();
        let mut font_name_totals: HashMap<String, u32> = HashMap::new();

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
                        canonical_font_name.clone(), // Store canonical name in FontUsage
                        NotNan::new(current_font_size).unwrap(),
                    );
                    let font_entry = fonts_map.entry(font_style_key).or_insert_with(|| {
                        FontUsage::new(
                            canonical_font_name.clone(), // Store canonical name in FontUsage
                            text_elem.font_name.clone(), // Store original name option
                            current_font_size,           // Store this specific size
                        )
                    });
                    font_entry.add_usage(current_content_index);
                    
                    // NEW: Collect statistics data
                    font_sizes.push(current_font_size);
                    page_y_values.entry(*page_number).or_default()
                                 .push((text_elem.bbox.1 + text_elem.bbox.3) * 0.5); // center Y
                    *font_name_totals.entry(canonical_font_name).or_insert(0) += 1;
                }
                current_content_index += 1;
            }

            if !page_element_indices.is_empty() {
                by_page.insert(*page_number, page_element_indices);
            }

            // Aggregate SoA data from PageContents
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

        // PASS 2: Compute statistics and build efficient style buckets
        let font_size_stats = FontSizeStats::from_sizes(&font_sizes);
        let mean = font_size_stats.mean;
        let sd = font_size_stats.std_dev.max(1e-6); // avoid div‑by‑zero

        // Sort Y values for percentile calculations
        for ys in page_y_values.values_mut() { 
            ys.sort_by(|a, b| a.partial_cmp(b).unwrap()); 
        }

        let perc_of = |page: u32, y: f32| -> f32 {
            if let Some(vec) = page_y_values.get(&page) {
                match vec.binary_search_by(|val| val.partial_cmp(&y).unwrap()) {
                    Ok(idx) | Err(idx) => idx as f32 / vec.len() as f32,
                }
            } else { 
                0.0 
            }
        };

        // Build style keys and buckets for efficient similarity search
        let mut style_key = Vec::with_capacity(text_store.id.len());
        let mut style_buckets: HashMap<StyleKey, Vec<TextHandle>> = HashMap::new();

        for row in 0..text_store.id.len() {
            let size = text_store.font_size[row];
            let font_id = intern_font(&text_store.font_name[row]);
            let size_bin = ((size * 2.0).round() as u16).min(400);
            let z_bin = (((size - mean) / sd) * 2.0).round().clamp(-8.0, 8.0) as i8;

            let x_cent = (text_store.bbox[row].0 + text_store.bbox[row].2) * 0.5;
            let horiz_bin = ((x_cent / DEFAULT_PAGE_WIDTH) * 10.0)
                .floor()
                .clamp(0.0, 9.0) as u8;

            let txt = &text_store.text[row];
            let caps = txt.chars().all(|c| !c.is_lowercase());
            let title = txt.split_whitespace()
                           .filter(|w| w.len() > 3)
                           .all(|w| w.chars().next().unwrap_or_default().is_uppercase());

            let key = StyleKey::new(font_id, size_bin, z_bin, horiz_bin, caps, title);
            style_key.push(key);
            style_buckets.entry(key)
                         .or_default()
                         .push(TextHandle(row as u32));
        }

        font_size_index_construction
            .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let font_size_index = font_size_index_construction;

        let spatial_rtree = RTree::bulk_load(spatial_elements);
        let reference_count_index = Vec::new();

        // Build font_name_frequency_index (total usage of a font name across all its sizes)
        let mut font_name_frequency_index: Vec<(u32, String)> = font_name_totals
            .iter()
            .map(|(name, &count)| (count, name.clone()))
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
            fonts: fonts_map,
            font_name_frequency_index,
            font_size_stats,
            
            style_key,
            style_buckets,
            font_name_totals,
            page_y_values,
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
        max_content_index: Option<usize>,
    ) -> Vec<(TextHandle, f64)> {
        use strsim::normalized_levenshtein;
        let start = start_content_index.unwrap_or(0);

        // Cache-efficient: iterate through text store directly
        let mut results = Vec::new();

        // Debug: print what we're searching for


        // Iterate through the text column only (cache-friendly)
        for (text_store_idx, text_content) in self.text_store.text.iter().enumerate() {
            // Early check: if we can determine this element is before our start position, skip expensive scoring
            if let Some(doc_idx) = self.find_doc_index_for_text(text_store_idx) {
                if doc_idx < start {
                    continue; // Skip elements before start position
                }
            }
            
            let score = normalized_levenshtein(text, text_content);
            
            
            if score >= threshold {
                // Find the corresponding document index for this text element
                if let Some(doc_idx) = self.find_doc_index_for_text(text_store_idx) {
                    // Check if we're within the allowed range
                    if doc_idx >= start {
                        // Check if we've exceeded the max_content_index limit
                        if let Some(max_idx) = max_content_index {
                            if doc_idx >= max_idx {
    
                                break;
                            }
                        }
                        

                        results.push((TextHandle(text_store_idx as u32), score));
                    } else {

                    }
                } else {

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
    pub fn top_k_similar_text(
        &self,
        seed: &TextElement,
        start_idx: usize,
        end_idx: usize,
        k: usize,
    ) -> Vec<(TextHandle, f32)> {
        if start_idx >= end_idx || start_idx >= self.doc_len() {
            return Vec::new();
        }

        // --- 1. seed StyleKey ---------------------------------------------
        let seed_row = self
            .element_id_to_index
            .get(&seed.id)
            .and_then(|&doc_idx| self.text_row_to_text_idx(doc_idx))
            .unwrap_or(usize::MAX);

        let seed_key = if seed_row != usize::MAX {
            self.style_key[seed_row]
        } else {
            let font_id = intern_font(&seed.font_name);
            let size_bin = ((seed.font_size * 2.0).round() as u16).min(400);
            StyleKey::new(font_id, size_bin, 0, 0, false, false)
        };

        // --- 2. probe neighbouring bins (Δz ∈ {-1,0,1}, Δpos ∈ {-1,0,1}) ---
        let mut bucket_keys = std::collections::HashSet::<StyleKey>::new();
        for dz in -1..=1 {
            for dp in -1..=1 {
                let nz = seed_key.z_bin().saturating_add(dz);
                let pos_i = seed_key.pos_bin() as i8 + dp;
                if pos_i < 0 || pos_i > 9 {
                    continue;
                }
                let np = pos_i as u8;
                bucket_keys.insert(seed_key.with_bins(nz, np));
            }
        }

        // --- 3. gather handles --------------------------------------------
        let mut handles = Vec::new();
        for key in bucket_keys {
            if let Some(v) = self.style_buckets.get(&key) {
                handles.extend_from_slice(v);
            }
        }

        // --- 4. slice filter + de-dup -------------------------------------
        let mut seen = std::collections::HashSet::<u32>::new();
        handles.retain(|h| {
            if !seen.insert(h.0) { return false; }
            if let Some(doc_idx) = self.text_idx_to_doc_idx(h.0 as usize) {
                doc_idx >= start_idx && doc_idx < end_idx
            } else { false }
        });

        // --- 5. cap to k ---------------------------------------------------
        handles.into_iter().take(k).map(|h| (h, 1.0)).collect()
    }

    /// Compute style key for any TextElement (not necessarily in the index)
    fn compute_style_key_for_element(&self, element: &TextElement) -> StyleKey {
        let font_id = intern_font(&element.font_name);
        let size_bin = ((element.font_size * 2.0).round() as u16).min(400);
        
        let mean = self.font_size_stats.mean;
        let sd = self.font_size_stats.std_dev.max(1e-6);
        let z_bin = (((element.font_size - mean) / sd) * 2.0).round().clamp(-8.0, 8.0) as i8;

        let y_cent = (element.bbox.1 + element.bbox.3) * 0.5;
        let pos_bin = if let Some(ys) = self.page_y_values.get(&element.page_number) {
            match ys.binary_search_by(|val| val.partial_cmp(&y_cent).unwrap()) {
                Ok(idx) | Err(idx) => {
                    let percentile = idx as f32 / ys.len() as f32;
                    (percentile * 10.0).floor().clamp(0.0, 9.0) as u8
                }
            }
        } else { 
            0 
        };

        let txt = &element.text;
        let caps = txt.chars().all(|c| !c.is_lowercase());
        let title = txt.split_whitespace()
                       .filter(|w| w.len() > 3)
                       .all(|w| w.chars().next().unwrap_or_default().is_uppercase());

        StyleKey::new(font_id, size_bin, z_bin, pos_bin, caps, title)
    }

    /// Helper method to translate document index to text store index
    #[inline] 
    fn text_row_to_text_idx(&self, doc_idx: usize) -> Option<usize> {
        match self.order.get(doc_idx)? {
            ContentHandle::Text(text_idx) => Some(*text_idx),
            ContentHandle::Image(_) => None,
        }
    }
    
    /// Helper method to translate text store index to document index
    #[inline] 
    fn text_idx_to_doc_idx(&self, text_idx: usize) -> Option<usize> {
        let text_id = self.text_store.id.get(text_idx)?;
        self.element_id_to_index.get(text_id).copied()
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



        let start_idx_inclusive = match self.element_id_to_index.get(&start_id) {
            Some(&idx) => idx,
            None => return Vec::new(), // Start element not found in index
        };

        let end_idx_exclusive = match end_element {
            Some(end) => {
                let end_id = end.id();
                match self.element_id_to_index.get(&end_id) {
                    Some(&idx) => idx, // This index is exclusive for the slice
                    None => self.order.len(), // End element not found, go to end of document
                }
            }
            None => self.order.len(), // No end element, go to end of document
        };

        // Now, start_idx_inclusive will be used directly for the slice start.
        // Ensure start_idx_inclusive is not past end_idx_exclusive or bounds.
        if start_idx_inclusive >= end_idx_exclusive || start_idx_inclusive >= self.order.len() {
            return Vec::new();
        }

        // Ensure the slice end is within bounds.
        let effective_end_idx = std::cmp::min(end_idx_exclusive, self.order.len());



        // Use cache-efficient content_slice method
        let result = self.content_slice(start_idx_inclusive, effective_end_idx);


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

    /// Get elements that match a similar style to the given text element (NEW: efficient style-based lookup)
    pub fn elements_by_similar_style(&self, seed: &TextElement, max_results: Option<usize>) -> Vec<PageContent> {
        // Get the seed element's style key
        let seed_doc_idx = match self.element_id_to_index.get(&seed.id) {
            Some(&idx) => idx,
            None => return Vec::new(),
        };
        
        let seed_text_idx = match self.text_row_to_text_idx(seed_doc_idx) {
            Some(idx) => idx,
            None => return Vec::new(),
        };
        
        let seed_key = self.style_key[seed_text_idx];
        
        // O(1) lookup of elements with same style signature
        let candidates = self.style_buckets.get(&seed_key)
            .map(|v| v.clone())
            .unwrap_or_default();
        
        let max_results = max_results.unwrap_or(candidates.len());
        let mut results = Vec::new();
        
        for text_handle in candidates.into_iter().take(max_results) {
            let txt_ref = self.text(text_handle);
            if txt_ref.id == seed.id {
                continue; // skip self
            }
            
            let txt = TextElement {
                id: txt_ref.id,
                text: txt_ref.text.to_string(),
                font_size: txt_ref.font_size,
                font_name: txt_ref.font_name.map(|s| s.to_string()),
                bbox: txt_ref.bbox,
                page_number: txt_ref.page_number,
            };
            
            results.push(PageContent::Text(txt));
        }
        
        results
    }

    /// Get style statistics for the efficient style buckets (NEW: for analytics)
    pub fn get_style_bucket_stats(&self) -> Vec<(u64, usize)> {
        self.style_buckets
            .iter()
            .map(|(key, handles)| (key.0, handles.len()))
            .collect()
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

    pub fn from_sizes(sizes: &[f32]) -> Self {
        if sizes.is_empty() {
            return Self { 
                mean: 12.0, 
                std_dev: 0.0, 
                percentiles: [12.0; 5] 
            };
        }
        let mut v: Vec<f32> = sizes.to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32;
        let sd = var.sqrt();
        let idx = |p: f32| v[((p * (v.len() as f32)) as usize).min(v.len() - 1)];
        Self {
            mean, 
            std_dev: sd,
            percentiles: [idx(0.25), idx(0.50), idx(0.75), idx(0.90), idx(0.95)],
        }
    }
}
