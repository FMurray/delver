use crate::chunker::chunk_text_elements;
use crate::layout::{extract_section_content, perform_matching, select_best_match};
use crate::parse::{get_refs, TextElement};
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

#[derive(Debug)]
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
    println!("Parsing template: {}", template_str);
    let pairs = match TemplateParser::parse(Rule::template, template_str) {
        Ok(mut pairs) => pairs.next().unwrap(),
        Err(e) => {
            eprintln!("Failed to parse template: {}", e);
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
                        eprintln!("Unexpected rule in template: {:?}", rule);
                    }
                }
            }
        }
        rule => {
            eprintln!("Expected template rule, got: {:?}", rule);
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
            eprintln!("Unexpected value rule: {:?}", rule);
            Value::String(inner_pair.as_str().to_string())
        }
    }
}

pub fn process_template_element(
    template_element: &Element,
    text_elements: &[TextElement],
    doc: &Document,
    inherited_metadata: &HashMap<String, Value>,
) -> Vec<ChunkOutput> {
    println!("\n=== Processing {} ===", template_element.name);
    let context = get_refs(doc).unwrap();
    let mut all_chunks = Vec::new();

    // Create MatchContext with fonts
    let mut match_context = context;
    if let Some(first_element) = text_elements.first() {
        if let Ok(font_dict) = doc.get_page_fonts(first_element.page_id) {
            match_context.fonts =
                Some(font_dict.into_iter().map(|(k, v)| (k, v.clone())).collect());
        }
    }

    // Handle standalone TextChunk elements (not siblings of Section)
    if template_element.name == "TextChunk" {
        // Only process if this is a root-level TextChunk (not a sibling of Section)
        if template_element.parent.is_none() {
            return process_text_chunk(template_element, text_elements, inherited_metadata);
        }
        return Vec::new(); // Return empty vec for non-root TextChunks
    }

    // For Section elements, look for matches
    if let Some(Value::String(match_str)) = template_element.attributes.get("match") {
        println!(
            "Looking for match: '{}' in {} elements",
            match_str,
            text_elements.len()
        );

        let threshold = if let Some(Value::Number(n)) = template_element.attributes.get("threshold")
        {
            (*n as f64) / 1000.0
        } else {
            0.75
        };

        let matched_elements = perform_matching(&text_elements, match_str, threshold);

        if let Some(best_match) = select_best_match(matched_elements.clone(), &match_context, None)
        {
            println!(
                "Best match found on page {}: '{}'",
                best_match.page_number, best_match.text
            );

            // Process text before the section starts
            let pre_section_elements = text_elements
                .iter()
                .take_while(|e| {
                    // Stop at the exact position of the section header
                    e.page_number < best_match.page_number
                        || (e.page_number == best_match.page_number
                            && e.position.1 <= best_match.position.1 - best_match.font_size)
                    // Subtract font size to avoid including the header
                })
                .cloned()
                .collect::<Vec<_>>();

            // Only chunk pre-section content if there's a TextChunk sibling before this Section
            if let Some(prev_sibling) = template_element.previous_sibling() {
                if prev_sibling.as_ref().name == "TextChunk" {
                    println!(
                        "Chunking pre-section content ({} elements) up to '{}' on page {}",
                        pre_section_elements.len(),
                        best_match.text,
                        best_match.page_number
                    );
                    all_chunks.extend(process_text_chunk(
                        prev_sibling.as_ref(),
                        &pre_section_elements,
                        inherited_metadata,
                    ));
                }
            }

            let mut metadata = inherited_metadata.clone();
            if let Some(Value::String(alias)) = template_element.attributes.get("as") {
                metadata.insert(alias.clone(), Value::String(alias.clone()));
            }

            // Extract the end_match string and find the end element
            let end_match_str = template_element.attributes.get("end_match").and_then(|v| {
                if let Value::String(s) = v {
                    Some(s.as_str())
                } else {
                    None
                }
            });

            let end_element = if let Some(end_str) = end_match_str {
                let end_matched_elements = perform_matching(&text_elements, end_str, threshold);
                select_best_match(end_matched_elements, &match_context, Some(&best_match))
            } else {
                None
            };

            // Extract section content
            let section_text_elements =
                extract_section_content(text_elements, &best_match, end_element.as_ref());

            // Process child elements within the section boundaries
            for child in &template_element.children {
                all_chunks.extend(process_template_element(
                    child,
                    &section_text_elements,
                    doc,
                    &metadata,
                ));
            }

            // Process text after the section ends if there's a TextChunk sibling after this Section
            if let Some(end_elem) = end_element {
                if let Some(next_sibling) = template_element.next_sibling() {
                    if next_sibling.as_ref().name == "TextChunk" {
                        let post_section_elements = text_elements
                            .iter()
                            .skip_while(|e| {
                                e.page_number < end_elem.page_number
                                    || (e.page_number == end_elem.page_number
                                        && e.position.1 <= end_elem.position.1)
                            })
                            .cloned()
                            .collect::<Vec<_>>();

                        all_chunks.extend(process_text_chunk(
                            next_sibling.as_ref(),
                            &post_section_elements,
                            inherited_metadata,
                        ));
                    }
                }
            }
        }
    }

    all_chunks
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

    let chunks = chunk_text_elements(section_text_elements, chunk_size, chunk_overlap);

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
