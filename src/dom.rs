use crate::chunker::{chunk_text_elements, ChunkingStrategy};
use crate::layout::{MatchContext, TextBlock, TextLine};
use crate::matcher::{align_template_with_content, MatchedContent, TemplateContentMatch};
use crate::parse::{get_refs, TextElement};
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
    pub attributes: HashMap<String, Value>,
    pub children: Vec<Element>,
    pub parent: Option<Weak<Element>>,
    pub prev_sibling: Option<Weak<Element>>,
    pub next_sibling: Option<Weak<Element>>,
}

impl Element {
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

#[derive(Debug)]
pub enum ElementType {
    Section,
    Paragraph,
    TextChunk,
    // Add other types as needed
}

#[derive(Debug)]
pub struct MatchedElement {
    pub template_element: Element,
    pub document_element: DocumentElement,
    pub children: Vec<MatchedElement>,
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, serde::Serialize)]
pub struct ChunkOutput {
    pub text: String,
    pub metadata: HashMap<String, Value>,
    pub chunk_index: usize,
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

pub fn process_template_element(
    template_element: &Element,
    text_lines: &[TextLine],
    text_elements: &[TextElement],
    doc: &Document,
    inherited_metadata: &HashMap<String, Value>,
) -> Vec<ChunkOutput> {
    let context = get_refs(doc).unwrap();
    let mut all_chunks = Vec::new();

    // Use the matcher to align the template with content
    if let Some(matched) = align_template_with_content(
        template_element,
        text_lines,
        text_elements,
        &context,
        inherited_metadata,
    ) {
        // Process the matched content to generate chunks
        all_chunks.extend(process_matched_content(&matched));
    }

    all_chunks
}

// Process the matched content to generate chunks
pub fn process_matched_content(matched: &TemplateContentMatch) -> Vec<ChunkOutput> {
    let mut chunks = Vec::new();

    match &matched.matched_content {
        MatchedContent::Chunk { content } => {
            // Convert elements to chunks directly
            chunks.extend(process_text_chunk_elements(
                content,
                &matched.template_element,
                &matched.metadata,
            ));
        }
        MatchedContent::Section { content, .. } => {
            // Process child matches first
            for child in &matched.children {
                chunks.extend(process_matched_content(child));
            }

            // If no children processed the content, process it as chunks
            if chunks.is_empty() && matched.template_element.children.is_empty() {
                chunks.extend(process_text_chunk_elements(
                    content,
                    &matched.template_element,
                    &matched.metadata,
                ));
            }
        }
        _ => {}
    }

    chunks
}

// Helper function to convert elements to chunks
fn process_text_chunk_elements(
    elements: &[TextElement],
    template_element: &Element,
    metadata: &HashMap<String, Value>,
) -> Vec<ChunkOutput> {
    // Similar to the existing process_text_chunk function
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
                .join(" ");

            ChunkOutput {
                text: chunk_text,
                metadata: metadata.clone(),
                chunk_index: i,
            }
        })
        .collect()
}

// Helper function to process TextChunk elements
fn process_text_chunk(
    template_element: &Element,
    section_text_elements: &[TextElement],
    metadata: &HashMap<String, Value>,
) -> Vec<ChunkOutput> {
    let chunk_size = if let Some(Value::Number(n)) = template_element.attributes.get("chunkSize") {
        *n as usize
    } else {
        500
    };

    let chunk_overlap =
        if let Some(Value::Number(n)) = template_element.attributes.get("chunkOverlap") {
            *n as usize
        } else {
            150
        };

    // Default to character-based chunking
    let strategy = ChunkingStrategy::Characters {
        max_chars: chunk_size,
    };

    let chunks = chunk_text_elements(section_text_elements, &strategy, chunk_overlap);

    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let chunk_text = chunk
                .iter()
                .map(|e| e.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");

            ChunkOutput {
                text: chunk_text,
                metadata: metadata.clone(),
                chunk_index: i,
            }
        })
        .collect()
}
