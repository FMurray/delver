use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;
use std::fs::File;
use std::io::{Error, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Parser;
use log::warn;
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Encoding, Object, ObjectId};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};

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
    "FontDescriptor",
    "ExtGState",
    "MediaBox",
    "Annot",
];

#[derive(Debug, Clone)]
struct DocumentNode {
    text: String,
    is_heading: bool,
    level: u8, // Heading level (e.g., 1 for H1, 2 for H2)
    children: Vec<DocumentNode>,
    font_size: f64,
}

#[derive(Debug, Clone)]
struct TextFragment {
    text: String,
    font_size: f64,
    font_name: String,
    x: f64,
    y: f64,
}

impl TextFragment {
    fn to_string(&self) -> String {
        format!(
            "Text: '{}' | Font: {} (size: {:.1}) | Position: ({:.1}, {:.1})",
            self.text, self.font_name, self.font_size, self.x, self.y
        )
    }
}

fn extract_text_fragments(doc: &Document) -> Vec<TextFragment> {
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

fn identify_headings(fragments: &[TextFragment]) -> Vec<DocumentNode> {
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

#[derive(Debug, Deserialize, Serialize)]
struct PdfText {
    text: BTreeMap<u32, Vec<String>>, // Key is page number
    errors: Vec<String>,
}

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about,
    long_about = "Extract TOC and write to file.",
    arg_required_else_help = true
)]
pub struct Args {
    pub pdf_path: PathBuf,

    /// Optional output directory. If omitted the directory of the PDF file will be used.
    #[clap(short, long)]
    pub output: Option<PathBuf>,

    /// Optional pretty print output.
    #[clap(short, long)]
    pub pretty: bool,

    /// Optional password for encrypted PDFs
    #[clap(long, default_value_t = String::from(""))]
    pub password: String,
}

impl Args {
    pub fn parse_args() -> Self {
        Args::parse()
    }
}

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

fn get_pdf_text(doc: &Document) -> Result<PdfText, Error> {
    let mut pdf_text: PdfText = PdfText {
        text: BTreeMap::new(),
        errors: Vec::new(),
    };
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

    for page in pages {
        match page {
            Ok((page_num, lines)) => {
                pdf_text.text.insert(page_num, lines);
            }
            Err(e) => {
                pdf_text.errors.push(e.to_string());
            }
        }
    }
    Ok(pdf_text)
}

fn pdf2text<P: AsRef<Path> + Debug>(path: P, output: P, pretty: bool) -> Result<(), Error> {
    println!("Load {path:?}");
    let mut doc = load_pdf(&path)?;
    let text = get_pdf_text(&doc)?;
    if !text.errors.is_empty() {
        eprintln!("{path:?} has {} errors:", text.errors.len());
        for error in &text.errors[..10] {
            eprintln!("  {}", error);
        }
    }
    let data = match pretty {
        true => serde_json::to_string_pretty(&text).unwrap(),
        false => serde_json::to_string(&text).unwrap(),
    };
    println!("Write {output:?}");
    let mut f = File::create(output)?;
    f.write_all(data.as_bytes())?;
    Ok(())
}

fn pdf2toc<P: AsRef<Path> + Debug>(path: P, output: P, pretty: bool) -> Result<(), Error> {
    println!("Load {path:?}");
    let doc = load_pdf(&path)?;

    let toc = doc
        .get_toc()
        .map_err(|e| Error::new(ErrorKind::Other, e.to_string()))?;
    if !toc.errors.is_empty() {
        eprintln!("{path:?} has {} errors:", toc.errors.len());
        for error in &toc.errors[..10] {
            eprintln!("{error:?}");
        }
    }
    let data = match pretty {
        true => serde_json::to_string_pretty(&toc).unwrap(),
        false => serde_json::to_string(&toc).unwrap(),
    };
    println!("Write {output:?}");
    let mut f = File::create(output)?;
    f.write_all(data.as_bytes())?;
    Ok(())
}

fn main() -> Result<(), Error> {
    let args = Args::parse_args();

    let start_time = Instant::now();
    let pdf_path = PathBuf::from(
        shellexpand::full(args.pdf_path.to_str().unwrap())
            .unwrap()
            .to_string(),
    );

    // Load document and extract fragments
    println!("Loading document and extracting text fragments...");
    let doc = load_pdf(&pdf_path)?;

    let fragments = extract_text_fragments(&doc);

    println!("\nFound {} text fragments", fragments.len());
    println!("\nSample of text fragments:");
    for fragment in fragments.iter().take(5) {
        println!("{}", fragment.to_string());
    }

    println!("\nDocument structure:");
    let nodes = identify_headings(&fragments);

    println!("\nHeadings with font size 21.0:");
    for node in nodes.iter() {
        if (node.font_size - 21.0).abs() < 0.1 && !node.text.trim().is_empty() {
            println!("{}", node.text.trim());
        }
    }

    Ok(())
}
