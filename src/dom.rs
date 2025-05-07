use crate::chunker::{chunk_text_elements, ChunkingStrategy};
use crate::matcher::{MatchedContent, TemplateContentMatch};
use crate::parse::{TextElement, PageContent};
use log::{error, info};
use lopdf::Document;
use pest::iterators::Pair;
use pest::Parser as PestParser;
use pest_derive::Parser as PestParserDerive;
use serde::Serialize;
use std::io::ErrorKind;
use std::sync::{Arc, Weak};
use std::{collections::HashMap, io::Error};

#[derive(PestParserDerive)]
#[grammar = "template.pest"]
pub struct TemplateParser;

#[derive(Debug)]
pub struct Root {
    pub elements: Vec<Element>,
}

#[derive(Debug, Clone)]
pub struct Element {
    pub name: String,
    pub element_type: ElementType,
    pub attributes: HashMap<String, Value>,
    pub children: Vec<Element>,
    pub parent: Option<Weak<Element>>,
    pub prev_sibling: Option<Weak<Element>>,
    pub next_sibling: Option<Weak<Element>>,
}

impl Element {
    pub fn new(name: String, element_type: ElementType) -> Self {
        Element {
            name,
            element_type,
            attributes: HashMap::new(),
            children: Vec::new(),
            parent: None,
            prev_sibling: None,
            next_sibling: None,
        }
    }

    pub fn previous_sibling(&self) -> Option<Arc<Element>> {
        self.prev_sibling.as_ref().and_then(|w| w.upgrade())
    }

    pub fn next_sibling(&self) -> Option<Arc<Element>> {
        self.next_sibling.as_ref().and_then(|w| w.upgrade())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Value {
    String(String),
    Number(i64),
    Boolean(bool),
    Array(Vec<Value>),
    Identifier(String),
}

#[derive(Debug)]
pub struct DocumentElement {
    pub element_type: ElementType,
    pub text: Option<String>,
    pub children: Vec<DocumentElement>,
    pub metadata: HashMap<String, String>, // Additional info like font size, position
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub enum ElementType {
    Section,
    Paragraph,
    TextChunk,
    Table,
    Image,
    // Image-specific processing children
    ImageSummary,
    ImageBytes,
    ImageCaption,
    ImageEmbedding,
    Unknown,
    // Add other types as needed
}

#[derive(Debug)]
pub struct MatchedElement {
    pub template_element: Element,
    pub document_element: DocumentElement,
    pub children: Vec<MatchedElement>,
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, serde::Serialize, Clone)]
pub struct ChunkOutput {
    pub text: String,
    pub metadata: HashMap<String, Value>,
    pub chunk_index: usize,
}

#[derive(Debug, serde::Serialize, Clone)]
pub struct ImageOutput {
    pub id: String, // Use UUID as String for JSON compatibility
    pub page_number: u32,
    pub bbox: (f32, f32, f32, f32),
    pub caption: Option<String>,
    pub bytes_base64: Option<String>,
    pub summary: Option<String>,
    pub embedding: Option<Vec<f32>>, // Assuming embedding is Vec<f32>
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, serde::Serialize, Clone)]
#[serde(tag = "type")] // Add type field for distinguishing in JSON
pub enum ProcessedOutput {
    Text(ChunkOutput),
    Image(ImageOutput),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatchType {
    Text,           // Simple text matching
    Semantic,       // Vector embedding similarity
    Regex,          // Regular expression matching
    Custom(String), // For future extension
}

#[derive(Debug, Clone)]
pub struct MatchConfig {
    pub match_type: MatchType,
    pub pattern: String,                 // Text to match or regex pattern
    pub threshold: f64,                  // Similarity threshold (0.0-1.0)
    pub options: HashMap<String, Value>, // Additional match-specific options
}

impl Default for MatchConfig {
    fn default() -> Self {
        Self {
            match_type: MatchType::Text,
            pattern: String::new(),
            threshold: 0.6,
            options: HashMap::new(),
        }
    }
}

impl Value {
    // Add a helper to extract match config from attributes
    pub fn as_match_config(&self) -> Option<MatchConfig> {
        if let Value::String(s) = self {
            Some(MatchConfig {
                match_type: MatchType::Text,
                pattern: s.clone(),
                threshold: 0.6,
                options: HashMap::new(),
            })
        } else if let Value::Array(values) = self {
            if values.len() >= 2 {
                let pattern = values[0].as_string()?;
                let threshold = values[1].as_number().map_or(600, |n| n) as f64 / 1000.0;

                let match_type = if values.len() >= 3 {
                    match values[2].as_string() {
                        Some(t) if t == "semantic" => MatchType::Semantic,
                        Some(t) if t == "regex" => MatchType::Regex,
                        Some(t) => MatchType::Custom(t),
                        None => MatchType::Text,
                    }
                } else {
                    MatchType::Text
                };

                let mut options = HashMap::new();
                if values.len() >= 4 {
                    if let Value::Array(opts) = &values[3] {
                        for i in (0..opts.len()).step_by(2) {
                            if i + 1 < opts.len() {
                                if let (Some(key), value) = (opts[i].as_string(), &opts[i + 1]) {
                                    options.insert(key, (*value).clone());
                                }
                            }
                        }
                    }
                }

                Some(MatchConfig {
                    match_type,
                    pattern,
                    threshold,
                    options,
                })
            } else {
                None
            }
        } else {
            None
        }
    }

    // Existing methods...
    pub fn as_string(&self) -> Option<String> {
        match self {
            Value::String(s) => Some(s.clone()),
            Value::Identifier(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<i64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_boolean(&self) -> Option<bool> {
        match self {
            Value::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<Vec<Value>> {
        match self {
            Value::Array(a) => Some(a.clone()),
            _ => None,
        }
    }
}

pub fn parse_template(template_str: &str) -> Result<Root, Error> {
    info!("Parsing template: {}", template_str);
    let pairs = match TemplateParser::parse(Rule::template, template_str) {
        Ok(mut pairs) => pairs.next().unwrap(),
        Err(e) => {
            error!("Failed to parse template: {}", e);
            return Err(Error::new(ErrorKind::InvalidData, e.to_string()));
        }
    };
    Ok(_parse_template(pairs))
}

fn _parse_template(pair: Pair<Rule>) -> Root {
    let mut elements = Vec::new();

    match pair.as_rule() {
        Rule::template => {
            for inner_pair in pair.into_inner() {
                match inner_pair.as_rule() {
                    Rule::expression => {
                        let element = process_element(inner_pair);
                        elements.push(element);
                    }
                    Rule::EOI => {}
                    rule => {
                        error!("Unexpected rule in template: {:?}", rule);
                    }
                }
            }
        }
        rule => {
            error!("Expected template rule, got: {:?}", rule);
        }
    }

    Root { elements }
}

fn process_element(pair: Pair<Rule>) -> Element {
    // If we receive an expression, get the element inside it
    let element_pair = if pair.as_rule() == Rule::expression {
        pair.into_inner().next().unwrap()
    } else {
        pair
    };

    let mut inner_rules = element_pair.into_inner();
    let identifier = inner_rules.next().unwrap().as_str().to_string();

    // Determine element type based on identifier
    let element_type = match identifier.as_str() {
        "Section" => ElementType::Section,
        "Paragraph" => ElementType::Paragraph,
        "TextChunk" => ElementType::TextChunk,
        "Table" => ElementType::Table,
        "Image" => ElementType::Image,
        "ImageSummary" => ElementType::ImageSummary,
        "ImageBytes" => ElementType::ImageBytes,
        "ImageCaption" => ElementType::ImageCaption,
        "ImageEmbedding" => ElementType::ImageEmbedding,
        _ => ElementType::Unknown,
    };

    let mut attributes = HashMap::new();
    let mut children = Vec::new();

    // Process remaining rules
    for inner_pair in inner_rules {
        match inner_pair.as_rule() {
            Rule::attributes => {
                attributes = process_attributes(inner_pair);
            }
            Rule::element_body => {
                for expr in inner_pair.into_inner() {
                    if expr.as_rule() == Rule::expression {
                        children.push(process_element(expr));
                    }
                }
            }
            _ => {}
        }
    }

    Element {
        name: identifier,
        element_type,
        attributes,
        children,
        parent: None,
        prev_sibling: None,
        next_sibling: None,
    }
}

fn process_attributes(pair: Pair<Rule>) -> HashMap<String, Value> {
    let mut attributes = HashMap::new();

    for inner_pair in pair.into_inner() {
        if inner_pair.as_rule() == Rule::attribute_list {
            for attr_pair in inner_pair.into_inner() {
                if attr_pair.as_rule() == Rule::attribute {
                    let mut attr_inner = attr_pair.into_inner();
                    let key = attr_inner.next().unwrap().as_str().to_string();
                    let value = process_value(attr_inner.next().unwrap());
                    attributes.insert(key, value);
                }
            }
        }
    }
    attributes
}

fn process_value(pair: Pair<Rule>) -> Value {
    // Get the inner value if this is a value wrapper
    let inner_pair = if pair.as_rule() == Rule::value {
        pair.into_inner().next().unwrap()
    } else {
        pair
    };

    match inner_pair.as_rule() {
        Rule::string => {
            let s = inner_pair.as_str();
            // Remove the surrounding quotes
            Value::String(s[1..s.len() - 1].to_string())
        }
        Rule::number => {
            // Parse as f64 first, then convert to i64 if it's a whole number
            let n = inner_pair.as_str().parse::<f64>().unwrap();
            if n.fract() == 0.0 {
                Value::Number(n as i64)
            } else {
                Value::Number((n * 1000.0) as i64) // Store float as fixed-point with 3 decimal places
            }
        }
        Rule::boolean => {
            let b = inner_pair.as_str().parse::<bool>().unwrap();
            Value::Boolean(b)
        }
        Rule::identifier => Value::Identifier(inner_pair.as_str().to_string()),
        Rule::array => {
            let values: Vec<Value> = inner_pair
                .into_inner()
                .filter(|p| p.as_rule() != Rule::array_values)
                .map(process_value)
                .collect();
            Value::Array(values)
        }
        rule => {
            error!("Unexpected value rule: {:?}", rule);
            Value::String(inner_pair.as_str().to_string())
        }
    }
}

// Process the matched content to generate chunks or image data
pub fn process_matched_content(matched: &Vec<TemplateContentMatch>) -> Vec<ProcessedOutput> {
    let mut output_elements = Vec::new();

    for match_item in matched {
        match &match_item.matched_content {
            MatchedContent::TextChunk { content } => {
                let owned_content: Vec<TextElement> = content.iter().map(|el_ref_ref| (**el_ref_ref).clone()).collect();
                let chunk_outputs = process_text_chunk_elements(
                    &owned_content,
                    &match_item.template_element,
                    &match_item.metadata,
                );
                for chunk_output in chunk_outputs {
                    output_elements.push(ProcessedOutput::Text(chunk_output));
                }
            }
            MatchedContent::Section { content, .. } => {
                if !match_item.children.is_empty() {
                    output_elements.extend(process_matched_content(&match_item.children));
                } else {
                    // Filter for Text elements only before collecting
                    let owned_content: Vec<TextElement> = content.iter().filter_map(|pc| match pc {
                        PageContent::Text(t) => Some((*t).clone()),
                        _ => None,
                    }).collect();
                    
                    if !owned_content.is_empty() {
                        let chunk_outputs = process_text_chunk_elements(
                            &owned_content,
                            &match_item.template_element,
                            &match_item.metadata,
                        );
                        for chunk_output in chunk_outputs {
                            output_elements.push(ProcessedOutput::Text(chunk_output));
                        }
                    } // else: Section contained only non-text, produce no output
                }
            }
            MatchedContent::Image(image_element) => {
                 output_elements.push(process_image_element(
                     image_element,
                     &match_item.template_element,
                     &match_item.metadata,
                 ));
            }
            _ => {} 
        }
    }

    output_elements
}

// Helper function to process a matched Image element based on its children
fn process_image_element(
    image_element: &crate::parse::ImageElement,
    template_element: &Element,
    metadata: &HashMap<String, Value>,
) -> ProcessedOutput {
    let mut image_output = ImageOutput {
        id: image_element.id.to_string(),
        page_number: image_element.page_number,
        // Manually convert geo::Rect to tuple
        bbox: (
            image_element.bbox.x0,
            image_element.bbox.y0,
            image_element.bbox.x1,
            image_element.bbox.y1,
        ),
        caption: None,
        bytes_base64: None,
        summary: None,
        embedding: None,
        metadata: metadata.clone(),
    };

    // Iterate through children of the Image template element (e.g., ImageBytes, ImageCaption)
    for child_template in &template_element.children {
        match child_template.element_type {
            ElementType::ImageBytes => {
                println!("Placeholder: Need to implement image decoding for ImageBytes");
                image_output.bytes_base64 = Some("PLACEHOLDER_BASE64_IMAGE_DATA".to_string());
            }
            ElementType::ImageCaption => {
                println!("Placeholder: Need to implement caption finding for ImageCaption");
                image_output.caption = Some("PLACEHOLDER_IMAGE_CAPTION".to_string());
            }
            ElementType::ImageSummary => {
                let model = child_template.attributes.get("model").and_then(|v| v.as_string()).unwrap_or_default();
                let prompt = child_template.attributes.get("prompt").and_then(|v| v.as_string()).unwrap_or_default();
                println!("Placeholder: Call external summary model ('{}') with prompt: {}", model, prompt);
                image_output.summary = Some(format!("PLACEHOLDER_SUMMARY_FROM_{}", model));
            }
            ElementType::ImageEmbedding => {
                let model = child_template.attributes.get("model").and_then(|v| v.as_string()).unwrap_or_default();
                 println!("Placeholder: Call external embedding model ('{}')", model);
                image_output.embedding = Some(vec![0.1, 0.2, 0.3]); // Placeholder embedding
            }
            _ => {}
        }
    }

    ProcessedOutput::Image(image_output)
}

// Helper function to convert elements to text chunks (remains largely the same)
fn process_text_chunk_elements(
    elements: &[TextElement],
    template_element: &Element,
    metadata: &HashMap<String, Value>,
) -> Vec<ChunkOutput> {
    let chunk_size = template_element
        .attributes
        .get("chunkSize")
        .map_or(500, |v| {
            if let Value::Number(n) = v {
                *n as usize
            } else {
                500
            }
        });

    let chunk_overlap = template_element
        .attributes
        .get("chunkOverlap")
        .map_or(150, |v| {
            if let Value::Number(n) = v {
                *n as usize
            } else {
                150
            }
        });

    // Use your existing chunking logic
    let strategy = ChunkingStrategy::Characters {
        max_chars: chunk_size,
    };
    let chunks = chunk_text_elements(elements, &strategy, chunk_overlap);

    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let chunk_text = chunk
                .iter()
                .map(|e| e.text.as_str())
                .collect::<Vec<_>>()
                .join("");

            ChunkOutput {
                text: chunk_text,
                metadata: metadata.clone(),
                chunk_index: i,
            }
        })
        .collect()
}
