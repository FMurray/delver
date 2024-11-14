use lopdf::Document;
use rayon::prelude::*;
use regex::Regex;
use std::io::{Error, ErrorKind};

use crate::parse::TextElement;

// #[derive(Debug, Clone)]
// pub struct TextElement {
//     text: String,
//     page_number: u32,
//     font_size: f32,
//     font_style: String,
//     position: (f32, f32), // (x, y) coordinates
//     is_in_outline: bool,
//     is_in_toc: bool,
//     // Add other metadata as needed
// }

pub fn perform_matching(text_elements: Vec<TextElement>, search_string: &str) -> Vec<TextElement> {
    text_elements
        .into_iter()
        .filter(|mi| mi.text.contains(search_string))
        .collect()
}

pub fn select_best_match(matched_elements: Vec<TextElement>) -> Option<TextElement> {
    matched_elements.into_iter().max_by(|a, b| {
        let score_a = score_match(a);
        let score_b = score_match(b);
        score_a
            .partial_cmp(&score_b)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn score_match(mi: &TextElement) -> f32 {
    let mut score = mi.font_size;

    // Higher positions (top of the page) may have lower Y values in PDF coordinate system
    if mi.position.1 < 200.0 {
        score += 10.0;
    }

    // Other heuristics can be added here

    score
}

fn extract_section_content(all_text_elements: &[TextElement], best_match: &TextElement) -> String {
    // Sort text elements by page number and position
    let mut sorted_elements = all_text_elements.to_vec();
    sorted_elements.sort_by(|a, b| {
        a.page_number
            .cmp(&b.page_number)
            .then_with(|| {
                a.position
                    .1
                    .partial_cmp(&b.position.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                a.position
                    .0
                    .partial_cmp(&b.position.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });

    // Find the index of the best match
    let start_index = sorted_elements
        .iter()
        .position(|mi| mi == best_match)
        .unwrap();

    // Collect text from the best match onwards
    let mut section_text = String::new();
    for mi in &sorted_elements[start_index..] {
        // Optionally, stop if you detect the start of the next section
        section_text.push_str(&mi.text);
        section_text.push(' ');
    }

    section_text
}

pub fn extract_sections(doc: &Document, sections: &[&str]) -> Vec<(String, String)> {
    let mut results = Vec::new();

    let pages: Vec<Result<(u32, Vec<String>), Error>> = doc
        .get_pages()
        .into_par_iter()
        .map(
            |(page_num, page_id): (u32, (u32, u16))| -> Result<(u32, Vec<String>), Error> {
                let text = doc.extract_text(&[page_num]).map_err(|e| {
                    Error::new(
                        ErrorKind::Other,
                        format!("Failed to extract text from page {page_num} id={page_id:?}: {e:}"),
                    )
                })?;
                Ok((
                    page_num,
                    text.split('\n')
                        .map(|s| s.trim_end().to_string())
                        .collect::<Vec<String>>(),
                ))
            },
        )
        .collect();

    // let section_titles_pattern = sections
    //     .iter()
    //     .map(|s| regex::escape(s))
    //     .collect::<Vec<String>>()
    //     .join("|");

    // let pattern = format!(r"({})\s*(.*?)", section_titles_pattern);

    // println!("Debug - Section titles to match: {:?}", sections);
    // println!("Debug - Final regex pattern: {}", pattern);

    // let re = Regex::new(&pattern).unwrap();

    // for m in re.find_iter(text) {
    //     println!(
    //         "Debug - Found match at position {}: {}",
    //         m.start(),
    //         m.as_str().chars().take(50).collect::<String>()
    //     );
    // }

    // for caps in re.captures_iter(text) {
    //     let title = caps.get(1).unwrap().as_str().to_string();
    //     let content = caps.get(2).unwrap().as_str().trim().to_string();
    //     results.push((title, content));
    // }

    results
}
