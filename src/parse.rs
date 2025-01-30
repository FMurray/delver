use indexmap::IndexMap;
use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Debug;
use std::io::{Error, ErrorKind};
use std::path::Path;
use std::thread::current;

use crate::layout::MatchContext;
// use crate::layout::MatchContext;
use crate::logging::{PDF_BT, PDF_OPERATIONS, PDF_TEXT_OBJECT};
use lopdf::{Dictionary, Document, Encoding, Error as LopdfError, Object, Result as LopdfResult};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};

use tracing::{error, event, trace, warn, Span};

#[cfg(feature = "async")]
use tokio::runtime::Builder;

use crate::fonts::{sanitize_font_name, FontMetrics, FONT_METRICS};

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
struct GraphicsState<'a> {
    ctm: [f32; 6],
    text_state: TextState<'a>,
}

impl<'a> Default for GraphicsState<'a> {
    fn default() -> Self {
        GraphicsState {
            ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            text_state: TextState::default(),
        }
    }
}

#[derive(Clone, Debug)]
struct TextState<'a> {
    text_matrix: [f32; 6],      // Tm
    text_line_matrix: [f32; 6], // Tlm
    font_name: Option<String>,
    font_size: f32,
    character_spacing: f32,  // Tc
    word_spacing: f32,       // Tw
    horizontal_scaling: f32, // Tz (expressed as fraction, e.g. 1.0=100%)
    leading: f32,            // TL
    rise: f32,               // Ts
    render_mode: u8,         // Tr
    start_pos: Option<(f32, f32)>,
    current_pos: (f32, f32),
    glyphs: Vec<PositionedGlyph>,
    text_buffer: String,
    current_font_object: Option<Object>,
    current_font_metrics: Option<&'static FontMetrics>,
    current_encoding: Option<&'a Encoding<'a>>,
    current_metrics: Option<&'static FontMetrics>,
}

impl<'a> Default for TextState<'a> {
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
            rise: 0.0,
            start_pos: None,
            current_pos: (0.0, 0.0),
            render_mode: 0,
            glyphs: Vec::new(),
            text_buffer: String::new(),
            current_font_object: None,
            current_font_metrics: None,
            current_encoding: None,
            current_metrics: None,
        }
    }
}

impl<'a> TextState<'a> {
    fn reset(&mut self) {
        self.text_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        self.text_line_matrix = self.text_matrix;
        self.start_pos = None;
        self.current_pos = (0.0, 0.0);
        self.glyphs.clear();
        self.text_buffer.clear();
    }
}

#[derive(Clone, Debug)]
struct PositionedGlyph {
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
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

fn collect_text_glyphs(
    text_state: &mut TextState,
    operands: &[Object],
    ctm: [f32; 6],
    media_box: [f32; 4],
) -> LopdfResult<()> {
    // Isolate the initial immutable borrows in a nested scope
    let process_operand = |text_state: &mut TextState, operand: &Object| -> LopdfResult<()> {
        let metrics = text_state.current_metrics;
        let encoding = text_state
            .current_encoding
            .as_ref()
            .ok_or(LopdfError::ContentDecode)?;

        match operand {
            Object::String(bytes, _) => {
                let decoded_text = Document::decode_text(encoding, bytes)?;

                for ch in decoded_text.chars() {
                    let advance = if let Some(metrics) = metrics {
                        // Use metrics-based calculation
                        metrics
                            .glyph_widths
                            .get(&(ch as u8))
                            .map(|w| (w / 1000.0) * text_state.font_size)
                            .unwrap_or(0.0)
                    } else {
                        0.0
                    };

                    let ascent = if let Some(metrics) = text_state.current_font_metrics {
                        (metrics.ascent as f32 / 1000.0) * text_state.font_size
                    } else {
                        0.0
                    };

                    println!("ascent: {}", ascent);

                    println!("ctm: {:?}", ctm);
                    println!("text_matrix: {:?}", text_state.text_matrix);
                    println!("current_pos: {:?}", text_state.current_pos);

                    let (user_x, user_y) = transform_point(
                        &ctm,
                        &text_state.text_matrix,
                        text_state.current_pos.0,
                        text_state.current_pos.1 + text_state.rise,
                    );

                    println!("user_x: {}", user_x);
                    println!("user_y: {}", user_y);

                    if let Some(last_char) = text_state.text_buffer.chars().last() {
                        if !(last_char == ' ' && ch == ' ') {
                            text_state.text_buffer.push(ch);
                        }
                    } else {
                        text_state.text_buffer.push(ch); // Handle empty buffer case
                    }

                    text_state.current_pos.0 += advance;

                    let glyph_w = advance;
                    let glyph_h = text_state.font_size;

                    println!("glyph_w: {}", glyph_w);
                    println!("glyph_h: {}", glyph_h);

                    text_state.glyphs.push(PositionedGlyph {
                        x_min: user_x,
                        y_min: user_y + glyph_h,
                        x_max: user_x + glyph_w,
                        y_max: user_y + glyph_h + ascent,
                    });

                    println!("text state glyphs: {:?}", text_state.glyphs);
                }
            }
            Object::Integer(i) => {
                let offset = -*i as f32 * (text_state.font_size / 1000.0); // Note the negative sign
                text_state.current_pos.0 += offset;
            }
            Object::Real(f) => {
                let offset = -*f as f32 * (text_state.font_size / 1000.0); // Note the negative sign
                text_state.current_pos.0 += offset;
            }
            Object::Array(arr) => {
                let elements = arr.clone();
                collect_text_glyphs(text_state, &elements, ctm, media_box)?;
            }
            _ => {}
        }
        Ok(())
    };

    for operand in operands {
        process_operand(text_state, operand)?;
    }
    Ok(())
}

fn finalize_text_run(state: &TextState, page_number: u32) -> TextElement {
    if state.glyphs.is_empty() {
        return TextElement {
            text: String::new(),
            font_size: state.font_size,
            font_name: state.font_name.clone(),
            bbox: (0.0, 0.0, 0.0, 0.0),
            page_number,
        };
    }

    let mut x_min = f32::MAX;
    let mut y_min = f32::MAX;
    let mut x_max = f32::MIN;
    let mut y_max = f32::MIN;

    for g in &state.glyphs {
        x_min = x_min.min(g.x_min);
        y_min = y_min.min(g.y_min);
        x_max = x_max.max(g.x_max);
        y_max = y_max.max(g.y_max);
    }

    let text_run = state.text_buffer.clone();

    TextElement {
        text: text_run,
        font_size: state.font_size,
        font_name: state.font_name.clone(),
        bbox: (x_min, y_min, x_max, y_max),
        page_number,
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

fn push_graphics_state(gs_stack: &mut Vec<GraphicsState>) {
    if let Some(current) = gs_stack.last() {
        gs_stack.push(current.clone());
    }
}

fn pop_graphics_state(gs_stack: &mut Vec<GraphicsState>) {
    if gs_stack.len() > 1 {
        gs_stack.pop();
    }
}

fn matrix_from_operands(op: &lopdf::content::Operation) -> [f32; 6] {
    op.operands
        .iter()
        .map(|obj| match obj {
            Object::Integer(i) => *i as f32,
            Object::Real(f) => *f,
            _ => 0.0,
        })
        .collect::<Vec<f32>>()
        .try_into()
        .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
}

fn operand_as_float(obj: &Object) -> f32 {
    match obj {
        Object::Integer(i) => *i as f32,
        Object::Real(f) => *f,
        _ => 0.0,
    }
}

fn operand_as_u8(obj: &Object) -> u8 {
    match obj {
        Object::Integer(i) => *i as u8,
        Object::Real(f) => *f as u8,
        _ => 0,
    }
}

fn handle_operator<'a>(
    gs_stack: &mut Vec<GraphicsState<'a>>,
    op: &lopdf::content::Operation,
    mut in_text_object: bool,
    collected_text: &mut Vec<TextElement>,
    page_number: u32,
    fonts: &BTreeMap<Vec<u8>, &Dictionary>,
    encodings: &'a BTreeMap<Vec<u8>, Encoding<'a>>,
    media_box: [f32; 4],
) -> Result<(), LopdfError> {
    let current_gs = gs_stack.last_mut().unwrap();

    match op.operator.as_ref() {
        "q" => push_graphics_state(gs_stack),
        "Q" => pop_graphics_state(gs_stack),
        "cm" => {
            let matrix = matrix_from_operands(op);
            gs_stack.last_mut().unwrap().ctm = multiply_matrices(&matrix, &current_gs.ctm);
        }
        "BT" => {
            in_text_object = true;
            current_gs.text_state.text_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            current_gs.text_state.text_line_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        }
        "ET" => {
            in_text_object = false;
            current_gs.text_state.reset();
        }
        "Tf" => {
            if let (Some(Object::Name(font_name)), Some(font_size_obj)) =
                (op.operands.get(0), op.operands.get(1))
            {
                let font_size = operand_as_float(font_size_obj);

                // Get the font dictionary first
                if let Some(dict) = fonts.get(font_name) {
                    // Get base font name from dictionary
                    let base_font = dict
                        .get(b"BaseFont")
                        .and_then(Object::as_name)
                        .map(|name| String::from_utf8_lossy(name))
                        .map(|name| sanitize_font_name(&name).to_string())
                        .unwrap_or("".to_string());

                    current_gs.text_state.font_name = Some(base_font.to_string());
                    current_gs.text_state.font_size = font_size;
                    current_gs.text_state.current_font_object =
                        Some(Object::Dictionary((*dict).clone()));

                    // Use base_font for metrics lookup
                    current_gs.text_state.current_font_metrics =
                        FONT_METRICS.get(base_font.as_str()).copied();
                    // Use original font_name for encoding lookup
                    current_gs.text_state.current_encoding = encodings.get(font_name).clone();
                }
            }
        }
        "Tc" => {
            if let Some(spacing) = op.operands.first() {
                current_gs.text_state.character_spacing = operand_as_float(spacing)
            }
        }
        "Tw" => {
            if let Some(spacing) = op.operands.first() {
                current_gs.text_state.word_spacing = operand_as_float(spacing)
            }
        }
        "Tz" => {
            if let Some(scale_percent) = op.operands.first() {
                current_gs.text_state.horizontal_scaling = operand_as_float(scale_percent) / 100.0
            }
        }
        "TL" => {
            if let Some(leading) = op.operands.first() {
                current_gs.text_state.leading = operand_as_float(leading)
            }
        }
        "Tr" => {
            if let Some(render_mode) = op.operands.first() {
                current_gs.text_state.render_mode = operand_as_u8(render_mode)
            }
        }
        "Ts" => {
            if let Some(rise) = op.operands.first() {
                current_gs.text_state.rise = operand_as_float(rise)
            }
        }
        "Tm" => {
            let m = matrix_from_operands(op);
            current_gs.text_state.text_matrix = m;
            current_gs.text_state.text_line_matrix = m;
        }
        "Td" => {
            // Move text position
            if let (Some(tx_obj), Some(ty_obj)) = (op.operands.get(0), op.operands.get(1)) {
                let tx = operand_as_float(tx_obj);
                let ty = operand_as_float(ty_obj);
                let tm = translate_matrix(tx, ty);
                current_gs.text_state.text_matrix =
                    multiply_matrices(&current_gs.text_state.text_line_matrix, &tm);
                current_gs.text_state.text_line_matrix = current_gs.text_state.text_matrix;
            }
        }
        "TD" => {
            // Move text pos and set leading
            if let (Some(tx_obj), Some(ty_obj)) = (op.operands.get(0), op.operands.get(1)) {
                let tx = operand_as_float(tx_obj);
                let ty = operand_as_float(ty_obj);
                current_gs.text_state.leading = -ty;
                let tm = translate_matrix(tx, ty);
                current_gs.text_state.text_matrix =
                    multiply_matrices(&current_gs.text_state.text_line_matrix, &tm);
                current_gs.text_state.text_line_matrix = current_gs.text_state.text_matrix;
            }
        }
        "T*" => {
            let tx = 0.0;
            let ty = -current_gs.text_state.leading;
            let tm: [f32; 6] = translate_matrix(tx, ty);
            current_gs.text_state.text_matrix =
                multiply_matrices(&current_gs.text_state.text_line_matrix, &tm);
            current_gs.text_state.text_line_matrix = current_gs.text_state.text_matrix;
        }
        "Tj" | "TJ" | "'" | "\"" => {
            // Check if we have a valid encoding for the current font
            if let Some(encoding) = current_gs.text_state.current_encoding {
                collect_text_glyphs(
                    &mut current_gs.text_state,
                    &op.operands,
                    current_gs.ctm,
                    media_box,
                )?;

                let text_element = finalize_text_run(&current_gs.text_state, page_number);
                collected_text.push(text_element);

                current_gs.text_state.glyphs.clear();
                current_gs.text_state.text_buffer.clear();
            }
        }
        _ => {}
    }
    Ok(())
}

fn get_page_text_elements(
    doc: &Document,
    page_number: u32,
    page_id: (u32, u16),
) -> Result<Vec<TextElement>, LopdfError> {
    let mut text_elements = Vec::new();
    let mut gs_stack = vec![GraphicsState::default()];

    let content_data = match doc.get_and_decode_page_content(page_id) {
        Ok(content) => content,
        Err(e) => {
            error!("Failed to decode content for page {}: {}", page_number, e);
            panic!("Failed to decode content for page {}", e);
        }
    };
    let page_dict = doc.get_dictionary(page_id).unwrap();
    println!("page_dict: {:?}", page_dict);
    let media_box = page_dict
        .get(b"MediaBox")
        .and_then(|obj| obj.as_array())
        .map(|arr| {
            println!("arr: {:?}", arr);
            let mut media_box = [0.0; 4];
            for (i, obj) in arr.iter().take(4).enumerate() {
                media_box[i] = match obj {
                    Object::Integer(i) => *i as f32,
                    Object::Real(f) => *f,
                    _ => 0.0,
                };
            }
            media_box
        })
        .unwrap_or([0.0; 4]);

    let page_rotation = page_dict
        .get(b"Rotate")
        .and_then(|obj| obj.as_i64())
        .unwrap_or(0);

    println!("media_box: {:?}", media_box);
    println!("page_rotation: {}", page_rotation);

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

    let mut in_text_object = false;
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
        // println!("op: {:?}", op);
        handle_operator(
            &mut gs_stack,
            &op,
            in_text_object,
            &mut text_elements,
            page_number,
            &fonts,
            &encodings,
            media_box,
        )?;
    }

    Ok(text_elements)
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

    let context = MatchContext {
        destinations,
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

            if i > 0 {
                let prev = &items[i - 1];
                let gap = it.bbox.0 - (prev.bbox.2);
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

    for (page_number, elements) in pages_map.into_iter() {
        let mut elements = elements.clone();
        elements.sort_by(|a, b| {
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

        if !current_line.is_empty() {
            lines.push(TextLine::from_elements(*page_number, current_line));
        }

        for line in &mut lines {
            line.elements.sort_by(|a, b| {
                a.bbox
                    .0
                    .partial_cmp(&b.bbox.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        let mut blocks = Vec::new();
        let mut current_block_lines = Vec::new();

        let mut prev_line_y: Option<f32> = None;
        for line in lines {
            let line_y_top = line.bbox.1.min(line.bbox.3);
            if let Some(py) = prev_line_y {
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

        if !current_block_lines.is_empty() {
            blocks.push(TextBlock::from_lines(*page_number, current_block_lines));
        }

        all_blocks.extend(blocks);
    }

    all_blocks
}

fn transform_point(ctm: &[f32; 6], text_matrix: &[f32; 6], x: f32, y: f32) -> (f32, f32) {
    // First apply text matrix
    let tx = text_matrix[0] * x + text_matrix[2] * y + text_matrix[4];
    let ty = text_matrix[1] * x + text_matrix[3] * y + text_matrix[5];

    // Then apply CTM scaling (but not translation yet)
    let px = ctm[0] * tx + ctm[2] * ty + ctm[4];
    let py = ctm[1] * tx + ctm[3] * ty; // Note: omitting ctm[5] (772.5)

    let user_y = -(ctm[5] - (py + ctm[5]));
    // Convert to device space by subtracting from CTM's Y translation
    (px, user_y)
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

pub fn translate_matrix(x: f32, y: f32) -> [f32; 6] {
    [1.0, 0.0, 0.0, 1.0, x, y]
}
