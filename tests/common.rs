use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Once;
use uuid::Uuid;

use delver_pdf::docql::{Element, ElementType, Value};
use delver_pdf::layout::MatchContext;
use delver_pdf::matcher::TemplateContentMatch;
use delver_pdf::parse::{ImageElement, PageContent, PageContents, TextElement};
use delver_pdf::search_index::PdfIndex;
use lopdf::Object;

static INIT: Once = Once::new();

pub fn setup() {
    INIT.call_once(|| {
        cleanup_all();
    });
}

pub fn cleanup_all() {
    let test_files = [
        "tests/example.pdf",
        "tests/custom.pdf",
        "tests/heading_test.pdf",
        "tests/coordinate_test.pdf",
    ];

    for file in test_files {
        if std::path::Path::new(file).exists() {
            std::fs::remove_file(file)
                .unwrap_or_else(|e| eprintln!("Failed to remove {}: {}", file, e));
        }
    }
}

pub fn get_test_pdf_path() -> PathBuf {
    PathBuf::from("tests/3M_2015_10K.pdf")
}

pub fn load_test_template() -> String {
    include_str!("./10k.tmpl").to_string()
}

// Test builders and helpers for more readable tests
#[derive(Default)]
pub struct DocumentBuilder {
    pages: BTreeMap<u32, PageContents>,
    elements: Vec<(Uuid, String, f32, f32, f32)>, // (id, text, x, y, font_size)
}

impl DocumentBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_text(&mut self, page: u32, text: &str, font_size: f32, x: f32, y: f32) -> Uuid {
        let id = Uuid::new_v4();
        let element = create_mock_text_element(
            id,
            text,
            "Arial",
            font_size,
            page,
            x,
            y,
            text.len() as f32 * 8.0,
            font_size,
        );

        self.pages
            .entry(page)
            .or_insert_with(PageContents::new)
            .add_text(element);
        self.elements.push((id, text.to_string(), x, y, font_size));
        id
    }

    pub fn add_image(&mut self, page: u32, x: f32, y: f32, width: f32, height: f32) -> Uuid {
        let id = Uuid::new_v4();
        let element = create_mock_image_element(id, page, x, y, width, height);
        self.pages
            .entry(page)
            .or_insert_with(PageContents::new)
            .add_image(element);
        id
    }

    pub fn build(&self) -> PdfIndex {
        let mock_match_context = MatchContext::default();
        PdfIndex::new(&self.pages, &mock_match_context)
    }

    pub fn get_element_text(&self, id: Uuid) -> Option<&str> {
        self.elements
            .iter()
            .find(|(elem_id, _, _, _, _)| *elem_id == id)
            .map(|(_, text, _, _, _)| text.as_str())
    }
}

pub struct TemplateBuilder {
    elements: Vec<Element>,
}

impl TemplateBuilder {
    pub fn new() -> Self {
        Self {
            elements: Vec::new(),
        }
    }

    pub fn add_section(mut self, name: &str) -> SectionBuilder {
        SectionBuilder::new(name, self)
    }

    pub fn add_textchunk(mut self, name: &str, chunk_size: i64, chunk_overlap: i64) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert("chunkSize".to_string(), Value::Number(chunk_size));
        attributes.insert("chunkOverlap".to_string(), Value::Number(chunk_overlap));

        let element = Element {
            name: name.to_string(),
            element_type: ElementType::TextChunk,
            attributes,
            children: Vec::new(),
            parent: None,
            prev_sibling: None,
            next_sibling: None,
            match_config: None,
            end_match_config: None,
        };

        self.elements.push(element);
        self
    }

    pub fn build(self) -> Vec<Element> {
        self.elements
    }
}

pub struct SectionBuilder {
    name: String,
    match_pattern: Option<Value>,
    end_match_pattern: Option<Value>,
    as_name: Option<String>,
    children: Vec<Element>,
    parent: TemplateBuilder,
}

impl SectionBuilder {
    fn new(name: &str, parent: TemplateBuilder) -> Self {
        Self {
            name: name.to_string(),
            match_pattern: None,
            end_match_pattern: None,
            as_name: None,
            children: Vec::new(),
            parent,
        }
    }

    pub fn match_pattern(mut self, pattern: &str) -> Self {
        self.match_pattern = Some(Value::String(pattern.to_string()));
        self
    }

    pub fn match_pattern_with_threshold(mut self, pattern: &str, threshold: f64) -> Self {
        // Store as an array format that as_match_config can parse
        self.match_pattern = Some(Value::Array(vec![
            Value::String(pattern.to_string()),
            Value::Number((threshold * 1000.0) as i64),
            Value::String("text".to_string()),
        ]));
        self
    }

    pub fn end_match(mut self, pattern: &str) -> Self {
        self.end_match_pattern = Some(Value::String(pattern.to_string()));
        self
    }

    pub fn as_name(mut self, name: &str) -> Self {
        self.as_name = Some(name.to_string());
        self
    }

    pub fn with_textchunk(mut self, name: &str, chunk_size: i64, chunk_overlap: i64) -> Self {
        let mut attributes = HashMap::new();
        attributes.insert("chunkSize".to_string(), Value::Number(chunk_size));
        attributes.insert("chunkOverlap".to_string(), Value::Number(chunk_overlap));

        let child = Element {
            name: name.to_string(),
            element_type: ElementType::TextChunk,
            attributes,
            children: Vec::new(),
            parent: None,
            prev_sibling: None,
            next_sibling: None,
            match_config: None,
            end_match_config: None,
        };

        self.children.push(child);
        self
    }

    pub fn with_child_section(mut self, name: &str) -> ChildSectionBuilder {
        ChildSectionBuilder::new(name, self)
    }

    pub fn build(mut self) -> TemplateBuilder {
        let mut attributes = HashMap::new();

        let mut match_config = None;
        if let Some(value) = &self.match_pattern {
            attributes.insert("match".to_string(), value.clone());
            match_config = value.as_match_config();
        }

        let mut end_match_config = None;
        if let Some(value) = &self.end_match_pattern {
            attributes.insert("end_match".to_string(), value.clone());
            end_match_config = value.as_match_config();
        }

        if let Some(as_name) = self.as_name {
            attributes.insert("as".to_string(), Value::String(as_name));
        }

        let element = Element {
            name: self.name,
            element_type: ElementType::Section,
            attributes,
            children: self.children,
            parent: None,
            prev_sibling: None,
            next_sibling: None,
            match_config,
            end_match_config,
        };

        self.parent.elements.push(element);
        self.parent
    }
}

pub struct ChildSectionBuilder {
    name: String,
    match_pattern: Option<Value>,
    end_match_pattern: Option<Value>,
    parent: SectionBuilder,
}

impl ChildSectionBuilder {
    fn new(name: &str, parent: SectionBuilder) -> Self {
        Self {
            name: name.to_string(),
            match_pattern: None,
            end_match_pattern: None,
            parent,
        }
    }

    pub fn match_pattern(mut self, pattern: &str) -> Self {
        self.match_pattern = Some(Value::String(pattern.to_string()));
        self
    }

    pub fn end_match(mut self, pattern: &str) -> Self {
        self.end_match_pattern = Some(Value::String(pattern.to_string()));
        self
    }

    pub fn build(mut self) -> SectionBuilder {
        let mut attributes = HashMap::new();

        let mut match_config = None;
        if let Some(value) = &self.match_pattern {
            attributes.insert("match".to_string(), value.clone());
            match_config = value.as_match_config();
        }

        let mut end_match_config = None;
        if let Some(value) = &self.end_match_pattern {
            attributes.insert("end_match".to_string(), value.clone());
            end_match_config = value.as_match_config();
        }

        let child = Element {
            name: self.name,
            element_type: ElementType::Section,
            attributes,
            children: Vec::new(),
            parent: None,
            prev_sibling: None,
            next_sibling: None,
            match_config,
            end_match_config,
        };

        self.parent.children.push(child);
        self.parent
    }
}

// Test assertion helpers
pub struct TestAssertions;

impl TestAssertions {
    pub fn assert_section_boundaries(
        section_match: &TemplateContentMatch,
        expected_start_id: Uuid,
        expected_end_id: Option<Uuid>,
        doc_builder: &DocumentBuilder,
    ) {
        assert!(
            section_match.section_boundaries.is_some(),
            "Section '{}' should have boundaries",
            section_match.template_element.name
        );

        let boundaries = section_match.section_boundaries.as_ref().unwrap();

        assert_eq!(
            boundaries.start_marker.id(),
            expected_start_id,
            "Section '{}' start marker mismatch. Expected: {}, Got: {}",
            section_match.template_element.name,
            doc_builder
                .get_element_text(expected_start_id)
                .unwrap_or("Unknown"),
            doc_builder
                .get_element_text(boundaries.start_marker.id())
                .unwrap_or("Unknown")
        );

        match (boundaries.end_marker.as_ref(), expected_end_id) {
            (Some(end_marker), Some(expected_id)) => {
                assert_eq!(
                    end_marker.id(),
                    expected_id,
                    "Section '{}' end marker mismatch. Expected: {}, Got: {}",
                    section_match.template_element.name,
                    doc_builder
                        .get_element_text(expected_id)
                        .unwrap_or("Unknown"),
                    doc_builder
                        .get_element_text(end_marker.id())
                        .unwrap_or("Unknown")
                );
            }
            (None, None) => {
                // Both are None, which is expected
            }
            (Some(_), None) => {
                panic!(
                    "Section '{}' has unexpected end marker",
                    section_match.template_element.name
                );
            }
            (None, Some(_)) => {
                panic!(
                    "Section '{}' missing expected end marker",
                    section_match.template_element.name
                );
            }
        }
    }

    pub fn assert_content_ids(
        match_result: &TemplateContentMatch,
        expected_ids: &[Uuid],
        doc_builder: &DocumentBuilder,
    ) {
        // Get the index from the doc builder to resolve content IDs
        let index = doc_builder.build();
        let actual_ids: Vec<Uuid> = match_result
            .matched_content
            .iter()
            .filter_map(|mc| mc.id(&index))
            .collect();

        assert_eq!(
            actual_ids.len(),
            expected_ids.len(),
            "Content length mismatch for '{}'. Expected {} items, got {}",
            match_result.template_element.name,
            expected_ids.len(),
            actual_ids.len()
        );

        for (i, (&expected_id, &actual_id)) in
            expected_ids.iter().zip(actual_ids.iter()).enumerate()
        {
            assert_eq!(
                actual_id,
                expected_id,
                "Content ID mismatch at position {} for '{}'. Expected: '{}', Got: '{}'",
                i,
                match_result.template_element.name,
                doc_builder
                    .get_element_text(expected_id)
                    .unwrap_or("Unknown"),
                doc_builder.get_element_text(actual_id).unwrap_or("Unknown")
            );
        }
    }

    pub fn assert_metadata(
        match_result: &TemplateContentMatch,
        expected_metadata: &[(&str, &str)],
    ) {
        for &(key, expected_value) in expected_metadata {
            assert!(
                match_result.metadata.contains_key(key),
                "Metadata key '{}' not found in '{}'",
                key,
                match_result.template_element.name
            );

            let actual_value = match_result.metadata.get(key).unwrap();
            let expected_value_obj = Value::String(expected_value.to_string());

            assert_eq!(
                actual_value, &expected_value_obj,
                "Metadata mismatch for key '{}' in '{}'. Expected: '{}', Got: '{:?}'",
                key, match_result.template_element.name, expected_value, actual_value
            );
        }
    }

    pub fn assert_child_count(match_result: &TemplateContentMatch, expected_count: usize) {
        assert_eq!(
            match_result.children.len(),
            expected_count,
            "Child count mismatch for '{}'. Expected {}, got {}",
            match_result.template_element.name,
            expected_count,
            match_result.children.len()
        );
    }
}

// Helper function to create mock TextElement
pub fn create_mock_text_element(
    id: Uuid,
    text: &str,
    font_name: &str,
    font_size: f32,
    page: u32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> TextElement {
    TextElement {
        id,
        text: text.to_string(),
        font_name: Some(font_name.to_string()),
        font_size,
        bbox: (x, y, x + width, y + height),
        page_number: page,
    }
}

// Helper function to create mock ImageElement
pub fn create_mock_image_element(
    id: Uuid,
    page: u32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> ImageElement {
    ImageElement {
        id,
        page_number: page,
        bbox: delver_pdf::geo::Rect {
            x0: x,
            y0: y,
            x1: x + width,
            y1: y + height,
        },
        image_object: Object::Null,
    }
}
