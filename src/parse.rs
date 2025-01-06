use indexmap::IndexMap;
use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Debug;
use std::io::{Error, ErrorKind};
use std::path::Path;

use log::{error, warn};

use crate::layout::MatchContext;
use lopdf::{Document, Encoding, Error as LopdfError, Object, Result as LopdfResult};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};

#[cfg(feature = "async")]
use tokio::runtime::Builder;

static IGNORE: &[&str] = &[
    "Length",
    "BBox",
    "FormType",
    "Matrix",
    "Type",
    "XObject",
    "Subtype",
    "Filter",
    "ColorSpace",
    "Width",
    "Height",
    "BitsPerComponent",
    "Length1",
    "Length2",
    "Length3",
    "PTEX.FileName",
    "PTEX.PageNumber",
    "PTEX.InfoDict",
    // "FontDescriptor",
    "ExtGState",
    "MediaBox",
    "Annot",
];

fn filter_func(object_id: (u32, u16), object: &mut Object) -> Option<((u32, u16), Object)> {
    if IGNORE.contains(&object.type_name().unwrap_or_default()) {
        return None;
    }
    if let Ok(d) = object.as_dict_mut() {
        d.remove(b"Producer");
        d.remove(b"ModDate");
        d.remove(b"Creator");
        d.remove(b"ProcSet");
        d.remove(b"Procset");
        d.remove(b"XObject");
        d.remove(b"MediaBox");
        d.remove(b"Annots");
        if d.is_empty() {
            return None;
        }
    }
    Some((object_id, object.to_owned()))
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PdfText {
    pub text: BTreeMap<u32, Vec<String>>, // Key is page number
    pub errors: Vec<String>,
}

#[cfg(not(feature = "async"))]
pub fn load_pdf<P: AsRef<Path>>(path: P) -> Result<Document, Error> {
    Document::load_filtered(path, filter_func)
        .map_err(|e| Error::new(ErrorKind::Other, e.to_string()))
}

#[cfg(feature = "async")]
fn load_pdf<P: AsRef<Path>>(path: P) -> Result<Document, Error> {
    Ok(Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async move {
            Document::load_filtered(path, filter_func)
                .await
                .map_err(|e| Error::new(ErrorKind::Other, e.to_string()))
        })?)
}

/// Struct for how the text is tokenized
/// Defaults to lines for now
#[derive(Debug)]
pub struct DocumentLine {
    pub line: String,
    pub page: u32,
}

#[derive(Clone, Debug)]
struct TextState {
    font_name: Option<String>,
    font_size: f32,
    text_matrix: [f32; 6],
    // text_line_matrix: [f32; 6],
    position: (f32, f32),
    text_buffer: String,
}

impl Default for TextState {
    fn default() -> Self {
        TextState {
            font_name: None,
            font_size: 0.0,
            text_matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            // text_line_matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            position: (0.0, 0.0),
            text_buffer: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextElement {
    pub text: String,
    pub page_number: u32,
    pub page_id: (u32, u16),
    pub font_size: f32,
    pub font_name: Option<String>,
    pub position: (f32, f32), // (x, y) coordinates
}

impl fmt::Display for TextElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Text Element:\n\
            \tText: \"{}\"\n\
            \tPage Number: {}\n\
            \tFont Size: {:.2}\n\
            \tFont Name: {:?}\n\
            \tPosition: ({:.2}, {:.2})\n",
            self.text,
            self.page_number,
            self.font_size,
            self.font_name,
            self.position.0,
            self.position.1
        )
    }
}

fn collect_text(
    text_buffer: &mut String,
    encoding: &Encoding,
    operands: &[Object],
    page_number: u32,
) -> LopdfResult<()> {
    for operand in operands.iter() {
        match operand {
            Object::String(bytes, _) => {
                let decoded_text = Document::decode_text(encoding, bytes)?;
                text_buffer.push_str(&decoded_text);
            }
            Object::Array(arr) => {
                collect_text(text_buffer, encoding, arr, page_number)?;
            }
            Object::Integer(_i) => {
                // Handle text positioning adjustments if necessary
            }
            _ => {}
        }
    }
    Ok(())
}

fn get_page_text_elements(
    doc: &Document,
    page_number: u32,
    page_id: (u32, u16),
) -> Result<Vec<TextElement>, LopdfError> {
    let mut text_elements = Vec::new();
    let mut text_state = TextState::default();

    let content_data = match doc.get_and_decode_page_content(page_id) {
        Ok(content) => content,
        Err(e) => {
            error!("Failed to decode content for page {}: {}", page_number, e);
            panic!("Failed to decode content for page {}", e);
        }
    };

    // Map of font resources
    let fonts = match doc.get_page_fonts(page_id) {
        Ok(f) => f,
        Err(e) => {
            error!("Failed to get fonts for page {}: {}", page_number, e);
            return Err(e);
        }
    };

    let encodings: BTreeMap<Vec<u8>, Encoding> = fonts
        .into_iter()
        .map(|(name, font)| font.get_font_encoding(doc).map(|it| (name, it)))
        .collect::<LopdfResult<BTreeMap<Vec<u8>, Encoding>>>()?;

    let mut current_encoding: Option<&Encoding> = None;

    for (i, op) in content_data.operations.iter().enumerate() {
        match op.operator.as_ref() {
            "BT" => {
                text_state = TextState::default();
                text_state.text_buffer = String::new();
                text_state.position = (0.0, 0.0);
            }
            "Tf" => {
                if let (Some(Object::Name(font_name)), Some(font_size_obj)) =
                    (op.operands.get(0), op.operands.get(1))
                {
                    let font_size = match font_size_obj {
                        Object::Integer(i) => *i as f32,
                        Object::Real(f) => *f,
                        _ => {
                            warn!("Unexpected font size type: {:?}", font_size_obj);
                            0.0
                        }
                    };
                    text_state.font_name = Some(String::from_utf8_lossy(font_name).into_owned());
                    text_state.font_size = font_size;
                    current_encoding = encodings.get(font_name);
                }
            }
            "Tj" | "TJ" | "'" | "\"" => {
                if let Some(encoding) = current_encoding {
                    collect_text(
                        &mut text_state.text_buffer,
                        encoding,
                        &op.operands,
                        page_number,
                    )?;
                } else {
                    warn!("No current encoding for text extraction at operation {}", i);
                }
            }
            "ET" => {
                if !text_state.text_buffer.is_empty() {
                    let text_element = TextElement {
                        text: text_state.text_buffer.clone(),
                        page_number,
                        page_id,
                        font_size: text_state.font_size,
                        font_name: text_state.font_name.clone(),
                        position: text_state.position,
                    };
                    text_elements.push(text_element);
                }
                // Reset text buffer
                text_state.text_buffer.clear();
            }
            "Td" | "TD" => {
                let args = &op.operands;
                if args.len() == 2 {
                    // Handle both Integer and Real PDF objects
                    let tx = match &args[0] {
                        Object::Integer(i) => *i as f32,
                        Object::Real(f) => *f,
                        _ => 0.0,
                    };
                    let ty = match &args[1] {
                        Object::Integer(i) => *i as f32,
                        Object::Real(f) => *f,
                        _ => 0.0,
                    };

                    text_state.position.0 += tx;
                    text_state.position.1 += ty;
                }
            }
            "Tm" => {
                let args = &op.operands;
                if args.len() == 6 {
                    // Convert all matrix values handling both Integer and Real
                    let matrix: Vec<f32> = args
                        .iter()
                        .map(|arg| match arg {
                            Object::Integer(i) => *i as f32,
                            Object::Real(f) => *f,
                            _ => 0.0,
                        })
                        .collect();

                    text_state.text_matrix = [
                        matrix[0], matrix[1], matrix[2], matrix[3], matrix[4], matrix[5],
                    ];
                    // The last two elements of the matrix are the translation
                    text_state.position = (matrix[4], matrix[5]);
                }
            }
            _ => {
                // Handle other operators if needed
            }
        }
    }

    if !text_state.text_buffer.is_empty() {
        let text_element = TextElement {
            text: text_state.text_buffer.clone(),
            page_number,
            page_id,
            font_size: text_state.font_size,
            font_name: text_state.font_name.clone(),
            position: text_state.position,
        };
        text_elements.push(text_element);
    }

    Ok(text_elements)
}

pub fn get_pdf_text(doc: &Document) -> Result<Vec<TextElement>, LopdfError> {
    let mut all_text_elements = Vec::new();

    let page_matches: Vec<Result<(u32, Vec<TextElement>), Error>> = doc
        .get_pages()
        .into_par_iter()
        .map(
            |(page_num, page_id): (u32, (u32, u16))| -> Result<(u32, Vec<TextElement>), Error> {
                let text_elements =
                    get_page_text_elements(doc, page_num, page_id).map_err(|e| {
                        Error::new(
                            ErrorKind::Other,
                            format!(
                                "Failed to extract text from page {page_num} id={page_id:?}: {e:?}"
                            ),
                        )
                    })?;
                Ok((page_num, text_elements))
            },
        )
        .collect();

    // for (page_number, page_id) in pages.into_par_iter() {
    //     let page_number = page_number; // Ensure the page number is the actual number

    //     // Parse the content stream
    //     let text_elements = get_page_text_elements(doc, page_number, page_id)?;

    //     all_text_elements.extend(text_elements);
    // }

    for page_match in page_matches {
        all_text_elements.extend(page_match?.1);
    }

    Ok(all_text_elements)
}

pub fn get_refs(doc: &Document) -> Result<MatchContext, LopdfError> {
    let mut destinations: IndexMap<String, Object> = IndexMap::new();

    if let Ok(catalog) = doc.catalog() {
        if let Ok(dests_ref) = catalog.get(b"Dests") {
            if let Ok(ref_id) = dests_ref.as_reference() {
                if let Ok(dests_dict) = doc.get_object(ref_id) {
                    if let Ok(dict) = dests_dict.as_dict() {
                        for (key, value) in dict.iter() {
                            let dest_name = String::from_utf8_lossy(key).to_string();

                            // Resolve the destination reference if it exists
                            let dest_obj = if let Ok(dest_ref) = value.as_reference() {
                                doc.get_object(dest_ref).unwrap_or(value)
                            } else {
                                value
                            };

                            destinations.insert(dest_name, dest_obj.to_owned());
                        }
                    }
                }
            }
        }
    }

    // Create the match context with owned destinations
    let context = MatchContext {
        destinations, // Transfer ownership instead of taking reference
        fonts: None,
    };

    Ok(context)
}

// pub fn pdf2toc<P: AsRef<Path> + Debug>(path: P, output: P, pretty: bool) -> Result<(), Error> {
//     let mut destinations: IndexMap<String, Object> = IndexMap::new();

//     if let Ok(catalog) = doc.catalog() {
//         if let Ok(dests_ref) = catalog.get(b"Dests") {
//             println!("\nFound Dests in catalog");
//             if let Ok(ref_id) = dests_ref.as_reference() {
//                 if let Ok(dests_dict) = doc.get_object(ref_id) {
//                     if let Ok(dict) = dests_dict.as_dict() {
//                         for (key, value) in dict.iter() {
//                             let dest_name = String::from_utf8_lossy(key).to_string();

//                             // Resolve the destination reference if it exists
//                             let dest_obj = if let Ok(dest_ref) = value.as_reference() {
//                                 doc.get_object(dest_ref).unwrap_or(value)
//                             } else {
//                                 value
//                             };

//                             destinations.insert(dest_name, dest_obj.to_owned());
//                         }

//                         println!("Found {} destinations", destinations.len());
//                     }
//                 }
//             }
//         }
//     }

//     // Create the match context
//     let context = MatchContext {
//         destinations: &destinations,
//     };

// TODO: Support documents without Outlines
// let mut destinations: IndexMap<String, Object> = IndexMap::new();

// if let Ok(catalog) = doc.catalog() {
//     if let Ok(dests_ref) = catalog.get(b"Dests") {
//         println!("\nFound Dests in catalog");
//         if let Ok(ref_id) = dests_ref.as_reference() {
//             if let Ok(dests_dict) = doc.get_object(ref_id) {
//                 if let Ok(dict) = dests_dict.as_dict() {
//                     for (key, value) in dict.iter() {
//                         let dest_name = String::from_utf8_lossy(key).to_string();

//                         // Resolve the destination reference if it exists
//                         let dest_obj = if let Ok(dest_ref) = value.as_reference() {
//                             doc.get_object(dest_ref).unwrap_or(value)
//                         } else {
//                             value
//                         };

//                         destinations.insert(dest_name, dest_obj.to_owned());
//                     }

//                     println!("Found {} destinations", destinations.len());
//                     // Print first few destinations as sample
//                     for (i, (name, dest)) in destinations.iter().enumerate() {
//                         println!("{}. {} -> {:?}", i + 1, name, dest);
//                     }
//                 }
//             }
//         }
//     } else {
//         println!("No Dests dictionary found in catalog");
//     }
// }

//     Ok(())
// }
