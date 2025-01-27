use indexmap::IndexMap;
use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Debug;
use std::io::{Error, ErrorKind};
use std::path::Path;

use crate::layout::MatchContext;
// use crate::layout::MatchContext;
use crate::logging::{PDF_BT, PDF_OPERATIONS, PDF_TEXT_OBJECT};
use lopdf::{Dictionary, Document, Encoding, Error as LopdfError, Object, Result as LopdfResult};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};

use tracing::{error, event, trace, warn, Span};

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
    // "MediaBox",
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
        // d.remove(b"MediaBox");
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
    // Document::load_filtered(path, filter_func)
    //     .map_err(|e| Error::new(ErrorKind::Other, e.to_string()))
    if !cfg!(debug_assertions) {
        Document::load(path).map_err(|e| Error::new(ErrorKind::Other, e.to_string()))
    } else {
        Document::load_filtered(path, filter_func)
            .map_err(|e| Error::new(ErrorKind::Other, e.to_string()))
    }
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
    text_line_matrix: [f32; 6],
    character_spacing: f32,
    word_spacing: f32,
    horizontal_scaling: f32,
    leading: f32,
    descender: f32,
    rise: f32,
    start_pos: Option<(f32, f32)>,
    current_pos: (f32, f32),
    text_buffer: String,
    current_font_object: Option<Object>,
}

impl Default for TextState {
    fn default() -> Self {
        TextState {
            font_name: None,
            font_size: 0.0,
            text_matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            text_line_matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            character_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scaling: 100.0,
            leading: 0.0,
            descender: 0.0,
            rise: 0.0,
            start_pos: None,
            current_pos: (0.0, 0.0),
            text_buffer: String::new(),
            current_font_object: None,
        }
    }
}

impl TextState {
    fn reset(&mut self) {
        // Critical: Reset ALL transform-related state
        self.text_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        self.text_line_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        self.start_pos = None;
        self.current_pos = (0.0, 0.0);
        self.text_buffer.clear();
        // Keep font/font_size between text objects unless explicitly changed
    }

    fn begin_text_object(&mut self) {
        self.start_pos = Some(self.current_pos);
    }

    fn update_position(&mut self, tx: f32, ty: f32) {
        self.current_pos = (tx, ty);
        if self.start_pos.is_none() {
            self.start_pos = Some(self.current_pos);
        }
    }

    fn add_glyph(&mut self, advance: f32) {
        self.current_pos.0 += advance;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextElement {
    pub text: String,
    pub font_size: f32,
    pub font_name: Option<String>,
    pub bbox: (f32, f32, f32, f32),
    pub page_number: u32,
}

impl fmt::Display for TextElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "\"{}\" {:?} {}pt{}",
            self.text,
            self.bbox,
            self.font_size,
            self.font_name
                .as_ref()
                .map(|n| format!(" {}", n))
                .unwrap_or_default()
        )
    }
}

pub fn get_pdf_text(doc: &Document) -> Result<BTreeMap<u32, Vec<TextElement>>, Error> {
    let mut pages_map: BTreeMap<u32, Vec<TextElement>> = BTreeMap::new();

    for (page_num, page_id) in doc.get_pages().into_iter().take(1) {
        let text_elements = get_page_text_elements(doc, page_num, page_id).map_err(|e| {
            Error::new(
                ErrorKind::Other,
                format!("Failed to extract text from page {page_num} id={page_id:?}: {e:?}"),
            )
        })?;
        pages_map.insert(page_num, text_elements);
    }

    Ok(pages_map)
}

fn get_page_text_elements(
    doc: &Document,
    page_number: u32,
    page_id: (u32, u16),
) -> Result<Vec<TextElement>, LopdfError> {
    let mut text_elements = Vec::new();
    let mut text_state = TextState::default();

    // Get the page's MediaBox
    let page_dict = doc.get_object(page_id)?.as_dict()?;
    let media_box = match page_dict.get(b"MediaBox") {
        Ok(Object::Array(array)) => {
            let values: Vec<f32> = array
                .iter()
                .map(|obj| match obj {
                    Object::Integer(i) => *i as f32,
                    Object::Real(f) => *f,
                    _ => 0.0,
                })
                .collect();
            if values.len() == 4 {
                (values[0], values[1], values[2], values[3])
            } else {
                (0.0, 0.0, 612.0, 792.0) // Default Letter size
            }
        }
        _ => (0.0, 0.0, 612.0, 792.0), // Default Letter size
    };

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
        .iter()
        .map(|(name, font)| font.get_font_encoding(doc).map(|it| (name.clone(), it)))
        .collect::<LopdfResult<BTreeMap<Vec<u8>, Encoding>>>()?;

    let mut current_encoding: Option<&Encoding> = None;

    let mut ctm: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut ctm_stack: Vec<[f32; 6]> = vec![ctm];

    let mut in_text_block = false;
    let mut text_block_span: Option<Span> = None;

    for (i, op) in content_data
        .operations
        .iter()
        .filter(|op| {
            matches!(
                op.operator.as_ref(),
                "BT" | "ET" | "Tm" | "Td" | "Tf" | "TJ" | "Tj" | "'" | "\"" | "cm" | "q" | "Q"
            )
        })
        .enumerate()
    {
        match op.operator.as_ref() {
            "BT" => {
                // Begin text object - create a new span with minimal info
                in_text_block = true;
                let text_block_span = tracing::span!(tracing::Level::DEBUG, PDF_TEXT_OBJECT);
                let _enter = text_block_span.enter();
                text_state = TextState::default();
            }
            "ET" => {
                if !text_state.text_buffer.is_empty() {
                    text_block_span = None;
                    in_text_block = false;

                    let text_element =
                        finalize_text_element(&text_state, ctm, media_box, page_number);
                    event!(target: PDF_TEXT_OBJECT, tracing::Level::DEBUG, element = %text_element);
                    text_elements.push(text_element);
                }
                text_state.reset();
            }
            "q" => {
                ctm_stack.push(ctm.clone());
                println!("q ctm_stack: {:?}", ctm_stack);
            }
            "Q" => {
                if let Some(saved_ctm) = ctm_stack.pop() {
                    ctm = saved_ctm;
                } else {
                    warn!("Q operator with empty CTM stack");
                }
                println!("Q ctm_stack: {:?}", ctm_stack);
            }
            "cm" => {
                if op.operands.len() == 6 {
                    let matrix: [f32; 6] = op
                        .operands
                        .iter()
                        .map(|obj| match obj {
                            Object::Integer(i) => *i as f32,
                            Object::Real(f) => *f,
                            _ => 0.0,
                        })
                        .collect::<Vec<f32>>()
                        .try_into()
                        .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

                    println!("cm: {:?}", matrix);
                    // Multiply new matrix with current CTM
                    ctm = multiply_matrices(&matrix, &ctm);
                    println!("cm * current ctm: {:?}", ctm);
                }
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
                    text_state.rise = 0.8 * text_state.font_size;
                    text_state.descender = -0.2 * text_state.font_size;
                    current_encoding = encodings.get(font_name);

                    // Update the font object
                    if let Some(dict) = fonts.get(font_name) {
                        text_state.current_font_object = Some(Object::Dictionary((*dict).clone()));
                    } else {
                        text_state.current_font_object = None;
                    }
                }
            }
            "Tm" => {
                if op.operands.len() == 6 {
                    let matrix: [f32; 6] = op
                        .operands
                        .iter()
                        .map(|obj| match obj {
                            Object::Integer(i) => *i as f32,
                            Object::Real(f) => *f,
                            _ => 0.0,
                        })
                        .collect::<Vec<f32>>()
                        .try_into()
                        .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

                    // don't call update_position here, just set the position
                    text_state.position = (matrix[4], matrix[5]);
                }
            }
            "Td" => {
                if op.operands.len() == 2 {
                    let tx = match &op.operands[0] {
                        Object::Integer(i) => *i as f32,
                        Object::Real(f) => *f,
                        _ => 0.0,
                    };
                    let ty = match &op.operands[1] {
                        Object::Integer(i) => *i as f32,
                        Object::Real(f) => *f,
                        _ => 0.0,
                    };

                    let t = [1.0, 0.0, 0.0, 1.0, tx, ty];
                    text_state.text_matrix = multiply_matrices(&text_state.text_line_matrix, &t);
                    text_state.text_line_matrix = text_state.text_matrix;
                    text_state.position = (tx, ty);
                }
            }
            "TD" => {
                // Move to start of next line with offset and set leading
                if op.operands.len() == 2 {
                    let tx = match &op.operands[0] {
                        Object::Integer(i) => *i as f32,
                        Object::Real(f) => *f,
                        _ => 0.0,
                    };
                    let ty = match &op.operands[1] {
                        Object::Integer(i) => *i as f32,
                        Object::Real(f) => *f,
                        _ => 0.0,
                    };

                    text_state.leading = -ty;
                    // Then same as Td
                    let _t = [1.0, 0.0, 0.0, 1.0, tx, ty];
                    text_state.text_matrix = multiply_matrices(&text_state.text_line_matrix, &_t);
                    text_state.text_line_matrix = text_state.text_matrix;
                    text_state.update_position(tx, ty);
                }
            }
            "T*" => {
                // Move to start of next line
                let translation = [1.0, 0.0, 0.0, 1.0, 0.0, -text_state.leading];
                text_state.text_matrix =
                    multiply_matrices(&text_state.text_line_matrix, &translation);
                text_state.text_line_matrix = text_state.text_matrix;
                text_state.update_position(0.0, -text_state.leading);
            }
            "Tc" => {
                // Set character spacing
                if let Some(spacing) = op.operands.first() {
                    text_state.character_spacing = spacing.as_i64().unwrap_or(0) as f32;
                }
            }
            "Tw" => {
                // Set word spacing
                if let Some(spacing) = op.operands.first() {
                    text_state.word_spacing = spacing.as_i64().unwrap_or(0) as f32;
                }
            }
            "Tz" => {
                // Set horizontal scaling
                if let Some(scaling) = op.operands.first() {
                    text_state.horizontal_scaling = scaling.as_i64().unwrap_or(100) as f32;
                }
            }
            "TL" => {
                // Set leading
                if let Some(leading) = op.operands.first() {
                    text_state.leading = leading.as_i64().unwrap_or(0) as f32;
                }
            }
            "Ts" => {
                // Set rise
                if let Some(rise) = op.operands.first() {
                    text_state.rise = rise.as_i64().unwrap_or(0) as f32;
                }
            }
            "TJ" | "Tj" | "'" | "\"" => {
                if let Some(encoding) = current_encoding {
                    collect_text(
                        &mut text_state.text_buffer,
                        encoding,
                        &op.operands,
                        page_number,
                    )?;

                    let text_element =
                        finalize_text_element(&text_state, ctm, media_box, page_number);
                    event!(target: PDF_TEXT_OBJECT, tracing::Level::DEBUG, element = %text_element);
                    text_elements.push(text_element);
                    text_state.reset();
                }
            }
            _ => {
                // For all other operations within a text block, record them in the current span
                if in_text_block {
                    if let Some(span) = &text_block_span {
                        span.in_scope(|| {
                            trace!(
                                target: PDF_TEXT_OBJECT,
                                "Text operation: {} {:?}",
                                op.operator,
                                op.operands
                            );
                        });
                    }
                }
            }
        }
    }

    if !text_state.text_buffer.is_empty() {
        let text_element = TextElement {
            text: text_state.text_buffer.clone(),
            font_size: text_state.font_size,
            font_name: text_state.font_name.clone(),
            bbox: (0.0, 0.0, 0.0, 0.0),
            page_number,
        };
        text_elements.push(text_element);
    }

    event!(target: PDF_TEXT_OBJECT, tracing::Level::DEBUG, "remaining gs stack: {:?}", ctm_stack);

    Ok(text_elements)
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
                // let offset = *_i as f32;
            }
            _ => {}
        }
    }
    Ok(())
}

fn finalize_text_element(
    ts: &TextState,
    ctm: [f32; 6],
    media_box: (f32, f32, f32, f32),
    page_number: u32,
) -> TextElement {
    // Combine CTM and text matrix
    let final_matrix = multiply_matrices(&ctm, &ts.text_matrix);

    // Transform all coordinates using the final matrix
    let mut min_x = f32::MAX;
    let mut max_x = f32::MIN;
    let mut min_y = f32::MAX;
    let mut max_y = f32::MIN;

    let approx_char_width = 0.6 * ts.font_size;
    // TODO - get glyph advance here instead
    let run_width = approx_char_width * (ts.text_buffer.chars().count() as f32);

    for (local_x, local_y) in &ts.positions {
        let (x1, y1) = transform_point(&final_matrix, *local_x, *local_y);

        // Critical fix: Flip Y relative to MediaBox top
        let y1_flipped = media_box.3 - (y1 - media_box.1);

        min_x = min_x.min(x1);
        max_x = max_x.max(x1);
        min_y = min_y.min(y1_flipped);
        max_y = max_y.max(y1_flipped);
    }

    TextElement {
        bbox: (min_x - media_box.0, min_y, max_x - media_box.0, max_y),
        text: ts.text_buffer.clone(),
        font_size: ts.font_size,
        font_name: ts.font_name.clone(),
        page_number,
    }
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

/// Represents a single line of text on the page after grouping TextElements.
#[derive(Debug, Clone)]
pub struct TextLine {
    pub text: String,
    pub page_number: u32,
    pub elements: Vec<TextElement>,
    /// A bounding box for the entire line (x_min, y_min, x_max, y_max).
    pub bbox: (f32, f32, f32, f32),
}

impl TextLine {
    /// Construct a TextLine from a set of TextElement-sorted by their x position.
    pub fn from_elements(page_number: u32, items: Vec<TextElement>) -> Self {
        let mut line_min_x = f32::MAX;
        let mut line_min_y = f32::MAX;
        let mut line_max_x = f32::MIN;
        let mut line_max_y = f32::MIN;
        let mut combined_text = String::new();

        for (i, it) in items.iter().enumerate() {
            line_min_x = line_min_x.min(it.bbox.0);
            line_max_x = line_max_x.max(it.bbox.2);
            line_min_y = line_min_y.min(it.bbox.1);
            line_max_y = line_max_y.max(it.bbox.3);

            // Optionally insert space if the gap from previous item is big:
            if i > 0 {
                let prev = &items[i - 1];
                let gap = it.bbox.0 - (prev.bbox.2);
                // if gap > it.font_size * 0.2 {
                //     // heuristic: a gap bigger than 0.2 * font_size => new 'space'
                //     combined_text.push(' ');
                // }
            }
            combined_text.push_str(&it.text);
        }

        TextLine {
            text: combined_text,
            bbox: (line_min_x, line_min_y, line_max_x, line_max_y),
            elements: items,
            page_number,
        }
    }
}

/// Represents a "block" of consecutive lines that are close in vertical spacing.
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub page_number: u32,
    pub lines: Vec<TextLine>,
    /// A bounding box for the entire block (x_min, y_min, x_max, y_max).
    pub bbox: (f32, f32, f32, f32),
}

impl TextBlock {
    pub fn from_lines(page_number: u32, lines: Vec<TextLine>) -> Self {
        let (x_min, y_min, x_max, y_max) = lines.iter().fold(
            (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
            |(xmin, ymin, xmax, ymax), line| {
                (
                    xmin.min(line.bbox.0),
                    ymin.min(line.bbox.1),
                    xmax.max(line.bbox.2),
                    ymax.max(line.bbox.3),
                )
            },
        );

        TextBlock {
            page_number,
            lines,
            bbox: (x_min, y_min, x_max, y_max),
        }
    }
}

/// Example grouping function that demonstrates how to:
/// 1) Separate text by page
/// 2) Sort by descending y (top to bottom), then ascending x
/// 3) Group into lines based on a "y-threshold" and spacing
/// 4) Group lines into blocks based on vertical proximity
pub fn group_text_into_lines_and_blocks(
    pages_map: &BTreeMap<u32, Vec<TextElement>>,
    line_join_threshold: f32,
    block_join_threshold: f32,
) -> Vec<TextBlock> {
    let mut all_blocks = Vec::new();

    // 2) Process each page separately
    for (page_number, elements) in pages_map.into_iter() {
        // Sort elements top-to-bottom, then left-to-right
        // PDF coordinates often have y increasing from bottom to top, so you might invert if needed
        let mut elements = elements.clone();
        elements.sort_by(|a, b| {
            // Sort primarily by descending y, then ascending x
            b.bbox
                .1
                .partial_cmp(&a.bbox.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.bbox
                        .0
                        .partial_cmp(&b.bbox.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        // 3) Group elements into lines
        let mut lines = Vec::new();
        let mut current_line = Vec::new();

        let mut last_y = f32::MAX;

        for elem in elements {
            if current_line.is_empty() {
                current_line.push(elem.clone());
                last_y = elem.bbox.1;
            } else {
                if (last_y - elem.bbox.1).abs() < line_join_threshold {
                    current_line.push(elem.clone());
                } else {
                    lines.push(TextLine::from_elements(*page_number, current_line));
                    current_line = vec![elem.clone()];
                    last_y = elem.bbox.1;
                }
            }
        }

        // Handle leftover
        if !current_line.is_empty() {
            lines.push(TextLine::from_elements(*page_number, current_line));
        }

        // Optional: Sort each line's elements by ascending x (since we sorted globally, but re-check)
        for line in &mut lines {
            line.elements.sort_by(|a, b| {
                a.bbox
                    .0
                    .partial_cmp(&b.bbox.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        // 4) Group lines into blocks (paragraphs) based on vertical proximity
        let mut blocks = Vec::new();
        let mut current_block_lines = Vec::new();

        let mut prev_line_y: Option<f32> = None;
        for line in lines {
            // if the text matrix is inverted, the min y value is the top of the bbox
            // TODO: we should check the text matrix to see if it is inverted
            let line_y_top = line.bbox.1.min(line.bbox.3);
            if let Some(py) = prev_line_y {
                // If lines are too far apart vertically, start a new block
                if (py - line_y_top).abs() > block_join_threshold {
                    if !current_block_lines.is_empty() {
                        blocks.push(TextBlock::from_lines(*page_number, current_block_lines));
                        current_block_lines = Vec::new();
                    }
                }
            }
            prev_line_y = Some(line_y_top);
            current_block_lines.push(line);
        }

        // Handle leftover block
        if !current_block_lines.is_empty() {
            blocks.push(TextBlock::from_lines(*page_number, current_block_lines));
        }

        // Now we have a set of blocks for this page
        all_blocks.extend(blocks);
    }

    all_blocks
}

fn transform_point(m: &[f32; 6], x: f32, y: f32) -> (f32, f32) {
    let new_x = m[0] * x + m[2] * y + m[4];
    let new_y = m[1] * x + m[3] * y + m[5];
    (new_x, new_y)
}

pub fn multiply_matrices(a: &[f32; 6], b: &[f32; 6]) -> [f32; 6] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}
