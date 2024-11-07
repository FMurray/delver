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
