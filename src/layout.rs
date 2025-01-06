use indexmap::IndexMap;
use lopdf::Dictionary;
use lopdf::Object;
use rayon::prelude::*;
use std::collections::BTreeMap;
use strsim::normalized_levenshtein;

use crate::parse::TextElement;

// Add this struct at the module level
#[derive(Debug, Default)]
pub struct MatchContext {
    pub destinations: IndexMap<String, Object>,
    pub fonts: Option<BTreeMap<Vec<u8>, Dictionary>>,
}

pub fn perform_matching(
    text_elements: &[TextElement],
    search_string: &str,
    threshold: f64,
) -> Vec<TextElement> {
    let search_normalized = search_string.to_lowercase();

    text_elements
        .par_iter()
        .filter(|mi| {
            let text_normalized = mi.text.to_lowercase();
            let similarity = normalized_levenshtein(&text_normalized, &search_normalized);
            similarity >= threshold
        })
        .cloned()
        .collect()
}

pub fn select_best_match(
    matched_elements: Vec<TextElement>,
    context: &MatchContext,
) -> Option<TextElement> {
    matched_elements.into_par_iter().max_by(|a, b| {
        let score_a = score_match(a, context);
        let score_b = score_match(b, context);
        score_a
            .partial_cmp(&score_b)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn score_match(mi: &TextElement, context: &MatchContext) -> f32 {
    let mut score = 0.0;

    // Font size score (normalize assuming max font size of 72pt)
    let font_size_score = (mi.font_size / 72.0).min(1.0);
    score += font_size_score * 0.3; // 30% weight

    // Font characteristics score
    let font_score = if let Some(font_ref) = &mi.font_name {
        let font_bytes = font_ref.as_bytes().to_vec();
        if let Some(fonts) = &context.fonts {
            if let Some(font_dict) = fonts.get(&font_bytes) {
                // Get the actual font name from BaseFont
                let base_font = font_dict
                    .get(b"BaseFont")
                    .and_then(|bf| bf.as_name())
                    .map(|n| String::from_utf8_lossy(n).to_string())
                    .unwrap_or_default();

                let font_lower = base_font.to_lowercase();

                if font_lower.contains("bold")
                    || font_lower.contains("heavy")
                    || font_lower.contains("black")
                    || font_lower.ends_with("-b")
                    || font_lower.ends_with(".b")
                {
                    1.0
                } else if font_lower.contains("medium")
                    || font_lower.contains("semibold")
                    || font_lower.contains("demi")
                {
                    0.7
                } else if font_lower.contains("regular") || font_lower.contains("roman") {
                    0.5
                } else {
                    0.3
                }
            } else {
                0.0
            }
        } else {
            0.0
        }
    } else {
        0.0
    };
    score += font_score * 0.2; // 20% weight

    // Vertical position score (normalize based on typical page height ~842pt)
    let position_score = if mi.position.1 > 700.0 || mi.position.1 < 200.0 {
        1.0
    } else {
        0.0
    };
    score += position_score * 0.1; // 10% weight

    // Left alignment score (normalize based on typical page width ~595pt)
    let left_align_score = if mi.position.0 < 100.0 { 1.0 } else { 0.0 };
    score += left_align_score * 0.1; // 10% weight

    // Text case score
    let case_score = if mi.text.chars().all(|c| c.is_uppercase()) {
        1.0 // All caps
    } else if mi.text.chars().next().map_or(false, |c| c.is_uppercase()) {
        0.7 // Title case
    } else {
        0.3 // Normal case
    };
    score += case_score * 0.1; // 10% weight

    // Reference count score (normalize assuming max 10 references)
    let mut reference_count = 0;
    for (_name, dest_obj) in context.destinations.iter() {
        if let Object::Array(dest_array) = dest_obj {
            if dest_array.len() >= 4 {
                let dest_page = match &dest_array[0] {
                    Object::Integer(page) => (*page as u32) + 1,
                    _ => continue,
                };

                if dest_page == mi.page_number {
                    let dest_y = match &dest_array[3] {
                        Object::Real(y) => *y,
                        Object::Integer(y) => *y as f32,
                        _ => continue,
                    };

                    // Tighter vertical position matching
                    if (dest_y - mi.position.1).abs() < 20.0 {
                        reference_count += 1;
                    }
                }
            }
        }
    }
    let reference_score = (reference_count as f32 / 10.0).min(1.0);
    score += reference_score * 0.2; // 20% weight

    // Debug logging with improved formatting
    println!(
        "Debug - Text Element:\n\
        \tText: \"{}\"\n\
        \tPage Number: {}\n\
        \tFont Size: {:.2}\n\
        \tFont Name: {:?}\n\
        \tPosition: ({:.2}, {:.2})\n\
        \tScores:\n\
        \t\tFont Size Score: {:.2}\n\
        \t\tFont Score: {:.2}\n\
        \t\tPosition Score: {:.2}\n\
        \t\tLeft Align Score: {:.2}\n\
        \t\tCase Score: {:.2}\n\
        \t\tReference Score: {:.2}\n\
        \tTotal Score: {:.2}\n",
        mi.text,
        mi.page_number,
        mi.font_size,
        mi.font_name,
        mi.position.0,
        mi.position.1,
        font_size_score,
        font_score,
        position_score,
        left_align_score,
        case_score,
        reference_score,
        score
    );

    score
}

pub fn extract_section_content(
    all_text_elements: &[TextElement],
    start_element: &TextElement,
    end_element: Option<&TextElement>,
) -> Vec<TextElement> {
    // Find the index of the start element
    let start_index = all_text_elements
        .iter()
        .position(|e| e == start_element)
        .expect("Start element not found in text elements");

    // Determine the end index
    let end_index = if let Some(end_elem) = end_element {
        let idx = all_text_elements
            .iter()
            .position(|e| e == end_elem)
            .unwrap_or(all_text_elements.len());

        // Ensure end_index is after start_index
        if idx <= start_index {
            println!("End element occurs before start element or not found. Using end of document as end index.");
            all_text_elements.len()
        } else {
            idx
        }
    } else {
        all_text_elements.len()
    };

    // Extract elements between start_index and end_index
    all_text_elements[start_index + 1..end_index].to_vec()
}

// pub fn extract_sections(doc: &Document, _sections: &[&str]) -> Vec<(String, String)> {
//     let results = Vec::new();

//     let pages: Vec<Result<(u32, Vec<String>), Error>> = doc
//         .get_pages()
//         .into_par_iter()
//         .map(
//             |(page_num, page_id): (u32, (u32, u16))| -> Result<(u32, Vec<String>), Error> {
//                 let text = doc.extract_text(&[page_num]).map_err(|e| {
//                     Error::new(
//                         ErrorKind::Other,
//                         format!("Failed to extract text from page {page_num} id={page_id:?}: {e:}"),
//                     )
//                 })?;
//                 Ok((
//                     page_num,
//                     text.split('\n')
//                         .map(|s| s.trim_end().to_string())
//                         .collect::<Vec<String>>(),
//                 ))
//             },
//         )
//         .collect();

//     // let section_titles_pattern = sections
//     //     .iter()
//     //     .map(|s| regex::escape(s))
//     //     .collect::<Vec<String>>()
//     //     .join("|");

//     // let pattern = format!(r"({})\s*(.*?)", section_titles_pattern);

//     // println!("Debug - Section titles to match: {:?}", sections);
//     // println!("Debug - Final regex pattern: {}", pattern);

//     // let re = Regex::new(&pattern).unwrap();

//     // for m in re.find_iter(text) {
//     //     println!(
//     //         "Debug - Found match at position {}: {}",
//     //         m.start(),
//     //         m.as_str().chars().take(50).collect::<String>()
//     //     );
//     // }

//     // for caps in re.captures_iter(text) {
//     //     let title = caps.get(1).unwrap().as_str().to_string();
//     //     let content = caps.get(2).unwrap().as_str().trim().to_string();
//     //     results.push((title, content));
//     // }

//     results
// }
