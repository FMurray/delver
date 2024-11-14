use std::collections::HashMap;

#[derive(Debug)]
pub struct Root {
    pub elements: Vec<Element>,
}

#[derive(Debug)]
pub struct Element {
    pub name: String,
    pub attributes: HashMap<String, Value>,
    pub children: Vec<Element>,
}

#[derive(Debug)]
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

// fn match_element(
//     template_element: &Element,
//     document_elements: &[DocumentElement],
//     parent_metadata: &HashMap<String, Value>,
// ) -> Vec<MatchedElement> {
//     let mut matched_elements = Vec::new();

//     // Get matching criteria from template attributes
//     let match_criteria = template_element
//         .attributes
//         .get("match")
//         .and_then(|v| match v {
//             Value::String(s) => Some(s.clone()),
//             _ => None,
//         });

//     // Merge parent metadata with current element's metadata
//     let mut current_metadata = parent_metadata.clone();
//     if let Some(Value::String(alias)) = template_element.attributes.get("as") {
//         // Store the alias in metadata
//         current_metadata.insert(alias.clone(), Value::String(alias.clone()));
//     }

//     // Find matching document elements
//     for doc_element in document_elements {
//         if let Some(criteria) = &match_criteria {
//             if element_matches(doc_element, criteria) {
//                 // Element matches the criteria
//                 let mut children = Vec::new();
//                 for child_template in &template_element.children {
//                     let child_matches =
//                         match_element(child_template, &doc_element.children, &current_metadata);
//                     children.extend(child_matches);
//                 }

//                 let matched_element = MatchedElement {
//                     template_element: template_element.clone(),
//                     document_element: doc_element.clone(),
//                     children,
//                     metadata: current_metadata.clone(),
//                 };
//                 matched_elements.push(matched_element);
//             }
//         } else {
//             // No match criteria specified; proceed to match children
//             let mut children = Vec::new();
//             for child_template in &template_element.children {
//                 let child_matches =
//                     match_element(child_template, &doc_element.children, &current_metadata);
//                 children.extend(child_matches);
//             }

//             if !children.is_empty() {
//                 let matched_element = MatchedElement {
//                     template_element: template_element.clone(),
//                     document_element: doc_element.clone(),
//                     children,
//                     metadata: current_metadata.clone(),
//                 };
//                 matched_elements.push(matched_element);
//             }
//         }
//     }

//     matched_elements
// }

// fn element_matches(doc_element: &DocumentElement, criteria: &str) -> bool {
//     // Implement your matching logic here
//     // For example, exact match:
//     if let Some(text) = &doc_element.text {
//         text == criteria
//     } else {
//         false
//     }

//     // For regex or fuzzy matching, you'd implement those methods here
// }

// #[derive(Debug, Clone)]
// struct DocumentNode {
//     text: String,
//     is_heading: bool,
//     level: u8, // Heading level (e.g., 1 for H1, 2 for H2)
//     children: Vec<DocumentNode>,
//     font_size: f64,
// }

// #[derive(Debug, Clone)]
// pub struct TextFragment {
//     text: String,
//     font_size: f64,
//     font_name: String,
//     x: f64,
//     y: f64,
// }

// impl TextFragment {
//     fn to_string(&self) -> String {
//         format!(
//             "Text: '{}' | Font: {} (size: {:.1}) | Position: ({:.1}, {:.1})",
//             self.text, self.font_name, self.font_size, self.x, self.y
//         )
//     }
// }

// pub fn extract_text_fragments(doc: &Document) -> Vec<TextFragment> {
//     // Cache fonts for all pages at the start
//     let pages = doc.get_pages();

//     // Process pages in parallel
//     pages
//         .into_par_iter()
//         .flat_map(|(_page_number, page_id)| {
//             let content_data = match doc.get_page_content(page_id) {
//                 Ok(data) => data,
//                 Err(_) => return Vec::new(),
//             };

//             let content = match Content::decode(&content_data) {
//                 Ok(content) => content,
//                 Err(_) => return Vec::new(),
//             };

//             let fonts = doc.get_page_fonts(page_id).unwrap();

//             process_page_content(&content, &fonts, doc)
//         })
//         .collect()
// }
