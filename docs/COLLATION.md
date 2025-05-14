# Content Collation and Section Matching

## Overview

The content collation system is designed to align flat content elements (like text chunks and images) with a nested DOM-like structure defined by templates. This document outlines the approach for matching and structuring content, with a particular focus on section handling.

## Core Components

### 1. Template System
Constructs a user-defined DOM using the template syntax defined in [this doc](./TEMPLATE_SYNTAX.md). A parsed template contains Elements, potentially with sibling and children Elements.

The template will also allow the user to configure how these elements should be matched.

### 2. Content Processing
[Parsing](./PARSER.md) produces a flat list of elements from the document pages. The elements structs will typically contain both content (text, image bytes), uuids and bounding boxes. 

### 3. Matching System
Matching produces `TemplateContentMatch` structs which represent the `Element`'s alignment to the document content objects. 

- Uses a flexible matching configuration system
- Supports different matching types:
  - Text matching (fuzzy text matching)
  - Semantic matching (conceptual similarity)
  - Regex matching (pattern-based)
  - Custom matching strategies
- Includes threshold-based matching for quality control

### 4. Indexing System

The `PdfIndex` provides efficient access to document content through multiple specialized indices:

#### Core Indices
- `elements`: Flat list of all text elements
- `images`: List of image elements
- `by_page`: Maps page numbers to element indices
- `element_id_to_index`: Maps element IDs to their positions
- `image_id_to_index`: Maps image IDs to their positions

#### Specialized Indices
- `font_size_index`: Sorted index of elements by font size
  - Used for identifying headings and section boundaries
  - Helps in font-based content organization
  - Supports font size-based queries

- `text_rtree`: Spatial index for quick region-based queries
  - Enables efficient spatial searches
  - Used for finding content within specific regions
  - Helps in maintaining document layout relationships

- `reference_count_index`: Tracks element reference counts
  - Identifies frequently referenced elements (often headings)
  - Helps in determining content importance
  - Used for scoring potential matches

- `fonts`: Font usage statistics
  - Tracks font frequency and characteristics
  - Helps in identifying heading levels
  - Supports font-based content organization

#### Index Usage in Matching

1. **Section Start Detection**
   ```rust
   // Find potential section starts using multiple indices
   let candidates = index.search(
       page: Some(start_page),
       min_font_size: Some(avg_font_size * 1.2), // 20% larger than average
       min_references: Some(1), // At least one reference
       region: None
   );
   ```

2. **Content Extraction**
   ```rust
   // Get content within a region using spatial index
   let region_content = index.elements_in_region(
       start_x, start_y,
       end_x, end_y
   );
   ```

3. **Reference-based Scoring**
   ```rust
   // Score matches based on reference count
   let ref_count = index.reference_count_index
       .get(&element_id)
       .map_or(0, |count| *count);
   ```

4. **Font-based Analysis**
   ```rust
   // Find elements with specific font characteristics
   let heading_candidates = index.elements_by_font(
       font_id: Some("HeadingFont"),
       min_size: Some(14.0),
       max_size: Some(24.0),
       min_frequency: Some(5)
   );
   ```

## Section Matching Approach

### 1. Section Boundary Detection

#### Start Boundary Detection
The system uses a multi-index approach to find section start boundaries:

1. **Text-based Candidates**
   ```rust
   // Find text matches using the template's match pattern
   let text_candidates = index.find_text_matches(
       &match_config.pattern,
       match_config.threshold,
       Some(start_index)
   );
   ```

2. **Font-based Candidates**
   ```rust
   // Find elements with heading-like characteristics
   let font_candidates = index.elements_by_font(
       None, // any font
       Some(avg_font_size * 1.2), // larger than average
       None,
       Some(1) // at least one reference
   );
   ```

3. **Spatial Candidates**
   ```rust
   // Find elements in specific regions (e.g., top of page)
   let spatial_candidates = index.elements_in_region(
       start_x, start_y,
       end_x, end_y
   );
   ```

4. **Candidate Scoring**
   ```rust
   struct BoundaryCandidate {
       content: &PageContent,
       score: f32,
       reasons: Vec<String>,
   }

   fn score_candidate(
       candidate: &PageContent,
       index: &PdfIndex,
       template: &Element,
   ) -> BoundaryCandidate {
       let mut score = 0.0;
       let mut reasons = Vec::new();

       match candidate {
           PageContent::Text(text) => {
               // Text-specific scoring
               if text.font_size > avg_font_size * 1.2 {
                   score += 0.3;
                   reasons.push("Larger font size".to_string());
               }
               // ... other text scoring factors
           },
           PageContent::Image(image) => {
               // Image-specific scoring
               if is_heading_image(image) {
                   score += 0.4;
                   reasons.push("Heading image".to_string());
               }
           },
           // ... handle other content types
       }

       BoundaryCandidate {
           content: candidate,
           score,
           reasons,
       }
   }
   ```

#### End Boundary Detection
End boundaries are determined through a combination of template rules and content analysis:

1. **Template-based End Markers**
   ```rust
   // If template specifies an end marker
   if let Some(end_match) = template.attributes.get("end_match") {
       let end_candidates = index.find_text_matches(
           &end_match.as_string()?,
           0.8,
           Some(start_index + 1)
       );
   }
   ```

2. **Natural Boundary Detection**
   ```rust
   fn find_natural_boundaries(
       start_content: &PageContent,
       index: &PdfIndex,
       children: &[Element],
   ) -> Vec<BoundaryCandidate> {
       let mut candidates = Vec::new();
       
       // Find potential boundaries based on content changes
       match start_content {
           PageContent::Text(start_text) => {
               // Look for font changes
               candidates.extend(find_font_changes(start_text, index));
               
               // Look for spacing changes
               candidates.extend(find_spacing_changes(start_text, index));
               
               // Look for content type changes
               candidates.extend(find_content_type_changes(start_text, index));
           },
           // ... handle other content types
       }
       
       candidates
   }
   ```

3. **Child Element Filtering**
   ```rust
   struct ContentFlow {
       elements: Vec<&PageContent>,
       relationships: Vec<(usize, usize, RelationshipType)>,
   }

   enum RelationshipType {
       Before,
       After,
       Contains,
       ReferencedBy,
   }

   fn validate_boundary_candidates(
       candidates: &[BoundaryCandidate],
       children: &[Element],
       index: &PdfIndex,
   ) -> Vec<BoundaryCandidate> {
       // Build content flow graph
       let flow = build_content_flow(candidates, children, index);
       
       // Filter candidates based on child element requirements
       candidates.iter()
           .filter(|candidate| {
               // Check if candidate respects child element positions
               children.iter().all(|child| {
                   validate_child_position(child, candidate, &flow)
               })
           })
           .cloned()
           .collect()
   }
   ```

4. **Final Boundary Selection**
   ```rust
   fn select_best_boundary(
       candidates: Vec<BoundaryCandidate>,
       start_content: &PageContent,
       children: &[Element],
       index: &PdfIndex,
   ) -> Option<&PageContent> {
       // Score candidates based on multiple factors
       candidates.iter()
           .map(|candidate| {
               let mut score = candidate.score;
               
               // Consider content type compatibility
               if content_types_compatible(start_content, candidate.content) {
                   score += 0.2;
               }
               
               // Consider child element requirements
               if satisfies_child_requirements(candidate, children, index) {
                   score += 0.3;
               }
               
               // Consider document flow
               if maintains_document_flow(start_content, candidate.content, index) {
                   score += 0.2;
               }
               
               (candidate, score)
           })
           .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal))
           .map(|(candidate, _)| candidate.content)
   }
   ```

### 2. Content Extraction

#### Content Collection
- Gathers all content between section boundaries
- Handles multiple content types:
  - Text elements
  - Images
  - Tables
  - Other structured content
- Maintains content order and flow

#### Content Organization
- Preserves hierarchical structure
- Handles nested sections
- Maintains metadata inheritance
- Supports content filtering and transformation

### 3. Matching Process

1. **Template Processing**
   - Parse template structure
   - Extract matching rules and attributes
   - Identify section boundaries

2. **Content Analysis**
   - Analyze document structure
   - Identify potential section markers
   - Calculate content relationships

3. **Section Matching**
   - Find start markers using multiple criteria
   - Identify end markers or natural boundaries
   - Extract and organize section content
   - Handle nested sections recursively

4. **Content Structuring**
   - Create hierarchical content structure
   - Apply metadata and attributes
   - Handle special content types
   - Maintain document flow

## Implementation Details

### Key Data Structures

```rust
struct TemplateContentMatch<'a> {
    template_element: &'a Element,
    matched_content: MatchedContent<'a>,
    children: Vec<TemplateContentMatch<'a>>,
    metadata: HashMap<String, Value>,
}

enum MatchedContent<'a> {
    Section {
        start_marker: &'a TextElement,
        end_marker: Option<&'a TextElement>,
        content: Vec<&'a PageContent>,
    },
    // Other content types...
}
```

### Matching Algorithm

1. **Section Start Detection**
   ```rust
   fn find_section_start(
       template: &Element,
       index: &PdfIndex,
       start_index: usize,
   ) -> Option<&TextElement>
   ```

2. **Section End Detection**
   ```rust
   fn find_section_end(
       start_element: &TextElement,
       template: &Element,
       index: &PdfIndex,
   ) -> Option<&TextElement>
   ```

3. **Content Extraction**
   ```rust
   fn extract_section_content(
       start_element: &TextElement,
       end_element: Option<&TextElement>,
       index: &PdfIndex,
   ) -> Vec<&PageContent>
   ```

## Future Improvements

1. **Enhanced Section Detection**
   - Machine learning-based heading detection
   - Improved natural language processing
   - Better handling of document structure

2. **Content Processing**
   - Advanced table detection and parsing
   - Better image caption handling
   - Improved metadata extraction

3. **Performance Optimization**
   - Parallel processing of sections
   - Caching of intermediate results
   - Optimized spatial queries

4. **Quality Assurance**
   - Automated testing of section matching
   - Validation of content structure
   - Quality metrics for matches
