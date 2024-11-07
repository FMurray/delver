pub fn extract_text_fragments(doc: &Document) -> Vec<TextFragment> {
    // Cache fonts for all pages at the start
    let pages = doc.get_pages();

    // Process pages in parallel
    pages
        .into_par_iter()
        .flat_map(|(_page_number, page_id)| {
            let content_data = match doc.get_page_content(page_id) {
                Ok(data) => data,
                Err(_) => return Vec::new(),
            };

            let content = match Content::decode(&content_data) {
                Ok(content) => content,
                Err(_) => return Vec::new(),
            };

            let fonts = doc.get_page_fonts(page_id).unwrap();

            process_page_content(&content, &fonts, doc)
        })
        .collect()
}

// Helper function to process a single page's content
fn process_page_content(
    content: &Content,
    fonts: &BTreeMap<Vec<u8>, &Dictionary>,
    doc: &Document,
) -> Vec<TextFragment> {
    let mut fragments = Vec::new();
    let mut current_fragment: Option<TextFragment> = None;
    let mut current_font = String::new();
    let mut current_font_size = 0.0;
    let mut text_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut text_line_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

    for operation in &content.operations {
        match operation.operator.as_ref() {
            "BT" => {
                text_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                text_line_matrix = text_matrix;
                // Clear current fragment when starting new text object
                if let Some(fragment) = current_fragment.take() {
                    if !fragment.text.trim().is_empty() {
                        fragments.push(fragment);
                    }
                }
            }
            "ET" => {
                // Push current fragment when ending text object
                if let Some(fragment) = current_fragment.take() {
                    if !fragment.text.trim().is_empty() {
                        fragments.push(fragment);
                    }
                }
            }
            "Tf" => {
                if let (Some(font_name), Some(font_size)) =
                    (operation.operands.get(0), operation.operands.get(1))
                {
                    // println!("Font operation found:");
                    // println!("  Font name: {:?}", font_name);
                    // println!("  Raw font size: {:?}", font_size);

                    current_font = font_name.as_name_str().unwrap_or("").to_string();
                    // Try different methods to extract the font size
                    current_font_size = match font_size {
                        Object::Integer(size) => *size as f64,
                        Object::Real(size) => *size as f64,
                        _ => {
                            // Try to convert to string and parse
                            font_size.as_i64().map(|n| n as f64).unwrap_or(0.0)
                        }
                    };

                    // println!("  Current font size set to: {}", current_font_size);
                }
            }
            "Td" | "TD" => {
                if let (Some(tx), Some(ty)) = (operation.operands.get(0), operation.operands.get(1))
                {
                    let tx = tx.as_f32().unwrap_or(0.0);
                    let ty = ty.as_f32().unwrap_or(0.0);
                    text_line_matrix[4] += tx;
                    text_line_matrix[5] += ty;
                    text_matrix = text_line_matrix.clone();
                    // Clear current fragment on new line
                    if ty != 0.0 {
                        if let Some(fragment) = current_fragment.take() {
                            if !fragment.text.trim().is_empty() {
                                fragments.push(fragment);
                            }
                        }
                    }
                }
            }
            "Tm" => {
                if operation.operands.len() == 6 {
                    for i in 0..6 {
                        text_matrix[i] = operation.operands[i].as_f32().unwrap_or(0.0);
                    }
                    text_line_matrix = text_matrix.clone();
                    // Clear current fragment when text matrix changes
                    if let Some(fragment) = current_fragment.take() {
                        if !fragment.text.trim().is_empty() {
                            fragments.push(fragment);
                        }
                    }
                }
            }
            "Tj" | "'" | "\"" => {
                if let Some(text_object) = operation.operands.get(0) {
                    if let Ok(bytes) = text_object.as_string() {
                        if let Some(font_dict) = fonts.get(current_font.as_bytes()) {
                            if let Ok(font_encoding) = font_dict.get_font_encoding(doc) {
                                match Document::decode_text(&font_encoding, bytes.as_bytes()) {
                                    Ok(decoded) => {
                                        if !decoded.trim().is_empty() {
                                            let x = text_matrix[4] as f64;
                                            let y = text_matrix[5] as f64;
                                            // println!(
                                            //     "Creating fragment with font size: {}",
                                            //     current_font_size
                                            // );
                                            fragments.push(TextFragment {
                                                text: decoded,
                                                font_size: current_font_size,
                                                font_name: current_font.clone(),
                                                x,
                                                y,
                                            });
                                        }
                                    }
                                    Err(_) => {}
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fragments
}

pub fn identify_headings(fragments: &[TextFragment]) -> Vec<DocumentNode> {
    let mut nodes = Vec::new();

    // Collect font sizes to find the most common size (body text size)
    let mut font_size_counts = HashMap::new();
    for fragment in fragments {
        // Skip empty fragments
        if fragment.text.trim().is_empty() {
            continue;
        }
        *font_size_counts
            .entry((fragment.font_size * 10.0).round() as i32)
            .or_insert(0) += 1;
    }

    // Debug print font sizes and their frequencies
    println!("\nFont size distribution:");
    for (size, count) in &font_size_counts {
        println!(
            "Font size {:.1}: {} occurrences",
            *size as f64 / 10.0,
            count
        );
    }

    let body_font_size = font_size_counts
        .iter()
        .max_by_key(|&(_, count)| count)
        .map(|(&size, _)| size as f64 / 10.0)
        .unwrap_or(12.0);
    println!("Detected body font size: {:.1}", body_font_size);

    for fragment in fragments {
        // Skip empty fragments
        if fragment.text.trim().is_empty() {
            continue;
        }

        // More lenient heading detection
        let is_heading = fragment.font_size >= (body_font_size * 1.1); // 10% larger than body text
        let level = if is_heading {
            // Determine heading level based on font size ratio
            let size_ratio = fragment.font_size / body_font_size;
            if size_ratio >= 1.5 {
                1 // H1
            } else if size_ratio >= 1.3 {
                2 // H2
            } else {
                3 // H3
            }
        } else {
            0 // Not a heading
        };

        nodes.push(DocumentNode {
            text: fragment.text.clone(),
            is_heading,
            level,
            children: Vec::new(),
            font_size: fragment.font_size,
        });
    }

    nodes
}
