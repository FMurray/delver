use indexmap::IndexMap;
use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Debug;
use std::io::{Error, ErrorKind};
use std::path::Path;
use std::thread::current;
use uuid::Uuid;

use crate::geo::{multiply_matrices, transform_rect, Matrix, Rect, IDENTITY_MATRIX};
use crate::layout::MatchContext;
// use crate::layout::MatchContext;
use crate::logging::{PDF_BT, PDF_OPERATIONS, PDF_PARSING, PDF_TEXT_OBJECT};
use lopdf::{Dictionary, Document, Encoding, Error as LopdfError, Object, Result as LopdfResult};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};

use tracing::{error, event, instrument, trace, warn, Span};

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
    ctm: Matrix,
    text_state: TextState<'a>,
}

impl<'a> Default for GraphicsState<'a> {
    fn default() -> Self {
        GraphicsState {
            ctm: IDENTITY_MATRIX,
            text_state: TextState::default(),
        }
    }
}

#[derive(Clone)]
struct TextObjectState<'a> {
    text_matrix: Matrix,      // Tm
    text_line_matrix: Matrix, // Tlm
    font_name: Option<String>,
    glyphs: Vec<PositionedGlyph>,
    text_buffer: String,
    current_font_object: Option<Object>,
    font_metrics: Option<&'static FontMetrics>,
    current_encoding: Option<&'a Encoding<'a>>,
    current_metrics: Option<&'static FontMetrics>,
    operator_log: Vec<String>,
    char_bbox: Option<Rect>,
    char_tx: f32,
    char_ty: f32,
}

impl<'a> Default for TextObjectState<'a> {
    fn default() -> Self {
        TextObjectState {
            font_name: None,
            text_matrix: IDENTITY_MATRIX,
            text_line_matrix: IDENTITY_MATRIX,
            glyphs: Vec::new(),
            text_buffer: String::new(),
            current_font_object: None,
            font_metrics: None,
            current_encoding: None,
            current_metrics: None,
            operator_log: Vec::new(),
            char_tx: 0.0,
            char_ty: 0.0,
            char_bbox: None,
        }
    }
}

impl<'a> TextObjectState<'a> {
    fn reset(&mut self) {
        self.text_matrix = IDENTITY_MATRIX;
        self.text_line_matrix = self.text_matrix;
        self.glyphs.clear();
        self.text_buffer.clear();
        self.operator_log.clear();
    }
}

impl<'a> fmt::Debug for TextObjectState<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextState")
            .field("text_matrix", &self.text_matrix)
            .field("font_name", &self.font_name)
            .field("font_metrics", &self.font_metrics.map(|m| (m.ascent, m.descent)))
            .field("ctm", &self.text_matrix) // Assuming you have access to CTM via GraphicsState
            .finish()
    }
}

#[derive(Clone, Debug)]
struct TextState<'a> {
    char_space: f32,
    word_space: f32,
    scale: f32,
    leading: f32,
    font: Option<&'a FontMetrics>,
    fontname: String,
    encoding: Option<&'a Encoding<'a>>,
    size: f32,
    render: u8,
    rise: f32,
}

impl<'a> Default for TextState<'a> {
    fn default() -> Self {
        TextState {
            char_space: 0.0,
            word_space: 0.0,
            scale: 1.0,
            leading: 0.0,
            font: None,
            fontname: String::new(),
            encoding: None,
            size: 0.0,
            render: 0,
            rise: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct PositionedGlyph {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextElement {
    pub id: Uuid,
    pub text: String,
    pub font_size: f32,
    pub font_name: Option<String>,
    pub bbox: (f32, f32, f32, f32),
    pub page_number: u32,
    pub operators: Vec<String>,
}

impl TextElement {
    pub fn new(text: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            text,
            font_size: 0.0,
            font_name: None,
            bbox: (0.0, 0.0, 0.0, 0.0),
            page_number: 0,
            operators: Vec::new(),
        }
    }
}

impl fmt::Display for TextElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TextElement {{\n  text: \"{}\",\n  bbox: {:?},\n  font: {}pt{},\n  operators: [\n    {}\n  ]\n}}",
            self.text,
            self.bbox,
            self.font_size,
            self.font_name.as_deref().unwrap_or("unknown"),
            self.operators.join(",\n    ")
        )
    }
}

fn collect_text_glyphs(
    text_object_state: &mut TextObjectState,
    text_state: &mut TextState,
    operands: &[Object],
    ctm: Matrix,
    trm: &mut Matrix,
) -> LopdfResult<()> {
    // Process each text operand.
    let process_operand = |tos: &mut TextObjectState, text_state: &mut TextState, operand: &Object| -> LopdfResult<()> {
        let metrics = text_state.font;
        let encoding = text_state
            .encoding
            .as_ref()
            .ok_or(LopdfError::ContentDecode)?;

        match operand {
            Object::String(bytes, _) => {
                let decoded_text = Document::decode_text(encoding, bytes)?;
                let encoded_bytes = Document::encode_text(encoding, &decoded_text);

                for (byte, ch) in encoded_bytes.iter().zip(decoded_text.chars()) {
                    let ucs = *byte as u32;
                    let tsm = Matrix {
                        a: text_state.size * text_state.scale,
                        b: 0.0,
                        c: 0.0,
                        d: text_state.size,
                        e: 0.0,
                        f: text_state.rise,
                    };

                    // Compute the horizontal advance in text space.
                    let advance = if let Some(metrics) = metrics {
                        metrics
                            .glyph_widths
                            .get(&ucs)
                            .map(|w| (w / 1000.0) * text_state.size)
                            .unwrap_or(0.0)
                    } else {
                        0.0
                    };

                    text_object_state.char_tx = (advance * text_state.size + text_state.char_space) * text_state.scale;
                    text_object_state.char_ty = 0.0;

                    *trm = multiply_matrices(&tsm, &text_object_state.text_matrix);

                    // Retrieve the ascent and descent from the font metrics.
                    // Many fonts (like Times-Roman) provide a positive ascent and a negative descent.
                    let (asc, desc) = if let Some(metrics) = text_state.font {
                        (
                            (metrics.ascent as f32 / 1000.0) * text_state.size,
                            (metrics.descent as f32 / 1000.0) * text_state.size,
                        )
                    } else {
                        (0.0, 0.0)
                    };

                    let char_bbox = glyph_bound(text_object_state.font_metrics.unwrap(), ucs, trm);

                    // Append the character to the text buffer.
                    if let Some(last_char) = text_object_state.text_buffer.chars().last() {
                        if !(last_char == ' ' && ch == ' ') {
                            text_object_state.text_buffer.push(ch);
                        }
                    } else {
                        text_object_state.text_buffer.push(ch);
                    }

                    // Save the current text-space baseline position.
                    let base_x = text_state.current_pos.0;
                    let base_y = text_state.current_pos.1;
                    
                    // Compute the four corners in text space.
                    // In text space the top of the glyph is at baseline + effective_ascent,
                    // and the bottom is at baseline + effective_descent.
                    let p_lt = transform_point(&ctm, &text_state.text_matrix, base_x, base_y + effective_asc); // top-left
                    let p_lb = transform_point(&ctm, &text_state.text_matrix, base_x, base_y + effective_desc); // bottom-left
                    let p_rt = transform_point(&ctm, &text_state.text_matrix, base_x + advance, base_y + effective_asc); // top-right
                    let p_rb = transform_point(&ctm, &text_state.text_matrix, base_x + advance, base_y + effective_desc); // bottom-right

                    // Compute the bounding box from the four transformed corners.
                    let x_min = p_lt.0.min(p_lb.0).min(p_rt.0).min(p_rb.0);
                    let x_max = p_lt.0.max(p_lb.0).max(p_rt.0).max(p_rb.0);
                    let y_min = p_lt.1.min(p_lb.1).min(p_rt.1).min(p_rb.1);
                    let y_max = p_lt.1.max(p_lb.1).max(p_rt.1).max(p_rb.1);

                    text_state.glyphs.push(PositionedGlyph {
                        x_min,
                        y_min,
                        x_max,
                        y_max,
                    });

                    // Update the text-space x position by the advance.
                    text_state.current_pos.0 += advance;
                }
            }
            Object::Integer(i) => {
                let offset = -*i as f32 * (text_state.font_size / 1000.0);
                text_state.current_pos.0 += offset;
            }
            Object::Real(f) => {
                let offset = -*f as f32 * (text_state.font_size / 1000.0);
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

#[tracing::instrument()]
fn finalize_text_run(state: &TextObjectState, page_number: u32) -> TextElement {
    if state.glyphs.is_empty() {
        return TextElement {
            id: Uuid::new_v4(),
            text: String::new(),
            font_size: state.font_size,
            font_name: state.font_name.clone(),
            bbox: (0.0, 0.0, 0.0, 0.0),
            page_number,
            operators: Vec::new(),
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

    let text_element = TextElement {
        id: Uuid::new_v4(),
        text: text_run,
        font_size: state.font_size,
        font_name: state.font_name.clone(),
        bbox: (x_min, y_min, x_max, y_max),
        page_number,
        operators: state.operator_log.clone(),
    };

    tracing::debug!(
        element_id = %text_element.id,
        line_id = tracing::field::Empty,
        text_element = ?text_element,
        state = ?state,
        "Created text element"
    );

    text_element
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

fn matrix_from_operands(op: &lopdf::content::Operation) -> Matrix {
    op.operands
        .iter()
        .map(|obj| match obj {
            Object::Integer(i) => *i as f32,
            Object::Real(f) => *f,
            _ => 0.0,
        })
        .collect::<Vec<f32>>()
        .try_into()
        .unwrap_or(IDENTITY_MATRIX)
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

#[tracing::instrument(
    skip_all,
    fields(
        operator = %op.operator,
        params = ?op.operands,
        in_text_object 
    )
)]
fn handle_operator<'a>(
    gs_stack: &mut Vec<GraphicsState<'a>>,
    op: &lopdf::content::Operation,
    mut in_text_object: bool,
    text_elements: &mut Vec<TextElement>,
    page_number: u32,
    fonts: &BTreeMap<Vec<u8>, &Dictionary>,
    encodings: &'a BTreeMap<Vec<u8>, Encoding<'a>>,
    media_box: [f32; 4],
) -> Result<(), LopdfError> {
    let current_gs = gs_stack.last_mut().unwrap();

    match op.operator.as_ref() {
        // Graphics State
        "q" => push_graphics_state(gs_stack),
        "Q" => pop_graphics_state(gs_stack),
        "cm" => {
            if !current_gs.text_state.text_buffer.is_empty() {
                let text_element = finalize_text_run(&current_gs.text_state, page_number);
                text_elements.push(text_element);
                current_gs.text_state.glyphs.clear();
                current_gs.text_state.text_buffer.clear();
                current_gs.text_state.operator_log.clear();
            }

            let matrix = matrix_from_operands(op);
            current_gs.ctm = multiply_matrices(&matrix, &current_gs.ctm);
        }
        // Text Object
        "BT" => {
            tracing::debug!("Begin text object");
            in_text_object = true;
            current_gs.text_state.text_matrix = IDENTITY_MATRIX;
            current_gs.text_state.text_line_matrix = IDENTITY_MATRIX;
        }
        "ET" => {
            if let Some(element) = text_elements.last_mut() {
                for op in &current_gs.text_state.operator_log {
                    tracing::debug!(
                        element_id = %element.id,
                        line_id = tracing::field::Empty,
                        "PDF operator: {}",
                        op
                    );
                }
            }

            if !current_gs.text_state.text_buffer.is_empty() {
                let text_element = finalize_text_run(&current_gs.text_state, page_number);
                text_elements.push(text_element);
                current_gs.text_state.glyphs.clear();
                current_gs.text_state.text_buffer.clear();
                current_gs.text_state.operator_log.clear();
            }
            in_text_object = false;
        }
        // Text State
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
            let matrix = matrix_from_operands(op);
            current_gs.text_state.text_matrix = matrix;
            current_gs.text_state.text_line_matrix = matrix;

            current_gs.text_state.operator_log.push(format!("Tm {:?}", matrix));
        }
        // Text Positioning
        "Td" => {
            if let (Some(tx_obj), Some(ty_obj)) = (op.operands.get(0), op.operands.get(1)) {
                let tx = operand_as_float(tx_obj);
                let ty = operand_as_float(ty_obj);
                current_gs.text_state.text_line_matrix =
                    pre_translate(current_gs.text_state.text_line_matrix, tx, ty);
                current_gs.text_state.text_matrix = current_gs.text_state.text_line_matrix;
            }
        }
        "TD" => {
            // Move text pos and set leading
            if let (Some(tx_obj), Some(ty_obj)) = (op.operands.get(0), op.operands.get(1)) {
                let tx = operand_as_float(tx_obj);
                let ty = operand_as_float(ty_obj);
                current_gs.text_state.leading = -ty;
                current_gs.text_state.text_line_matrix =
                    pre_translate(current_gs.text_state.text_line_matrix, tx, ty);
                current_gs.text_state.text_matrix = current_gs.text_state.text_line_matrix;
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
        // Text Showing
        "Tj" | "TJ" | "'" | "\"" => {
            let operator = op.operator.clone();
            let operands = op.operands.iter().map(|o| format!("{:?}", o)).collect::<Vec<_>>().join(", ");
            current_gs.text_state.operator_log.push(format!("{} [{}]", operator, operands));
            
            collect_text_glyphs(
                &mut current_gs.text_state,
                &op.operands,
                current_gs.ctm,
                media_box,
            )?;

            let text_element = finalize_text_run(&current_gs.text_state, page_number);
            text_elements.push(text_element);

            current_gs.text_state.glyphs.clear();
            current_gs.text_state.text_buffer.clear();
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

    let media_box = page_dict
        .get(b"MediaBox")
        .and_then(|obj| obj.as_array())
        .map(|arr| {
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
    pub id: Uuid,
    pub text: String,
    pub page_number: u32,
    pub elements: Vec<TextElement>,
    /// A bounding box for the entire line (x_min, y_min, x_max, y_max).
    pub bbox: (f32, f32, f32, f32),
}

impl TextLine {
    pub fn from_elements(page_number: u32, items: Vec<TextElement>) -> Self {
        let id = Uuid::new_v4();
        let mut line_min_x = f32::MAX;
        let mut line_min_y = f32::MAX;
        let mut line_max_x = f32::MIN;
        let mut line_max_y = f32::MIN;
        let mut combined_text = String::new();

        for (_, it) in items.iter().enumerate() {
            line_min_x = line_min_x.min(it.bbox.0);
            line_max_x = line_max_x.max(it.bbox.2);
            line_min_y = line_min_y.min(it.bbox.1);
            line_max_y = line_max_y.max(it.bbox.3);

            // TODO: Add gap calculation if necessary
            // if i > 0 {
            //     let prev = &items[i - 1];
            //     l_gapgap = it.bbox.0 - (prev.bbox.2);
            // }
            combined_text.push_str(&it.text);
        }

        let line = TextLine {
            id,
            text: combined_text,
            page_number,
            elements: items,
            bbox: (line_min_x, line_min_y, line_max_x, line_max_y),
        };

        tracing::debug!(
            line_id = %line.id,
            parent = %line.id,
            children = %serde_json::to_string(&line.elements.iter().map(|e| e.id).collect::<Vec<_>>()).unwrap(),
            rel_type = "line_to_elements",
            "Created text line with {} elements",
            line.elements.len()
        );
        
        line
    }
}



/// Represents a "block" of consecutive lines that are close in vertical spacing.
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub id: Uuid,
    pub page_number: u32,
    pub lines: Vec<TextLine>,
    /// A bounding box for the entire block (x_min, y_min, x_max, y_max).
    pub bbox: (f32, f32, f32, f32),
}

impl TextBlock {
    pub fn from_lines(page_number: u32, lines: Vec<TextLine>) -> Self {
        let id = Uuid::new_v4();
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

        let block = Self {
            id: id,
            page_number,
            lines,
            bbox: (x_min, y_min, x_max, y_max),
        };
        
        tracing::debug!(
            block_id = %block.id,
            "Created text block with {} lines",
            block.lines.len()
        );

        block
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


/// The transformed bounding box as a `Rect`.
pub fn glyph_bound(font: &FontMetrics, glyph: u32, trm: &Matrix) -> Rect {
    // Look up the glyph width; if not present, default to 0.0.
    let glyph_width = font.glyph_widths.get(&glyph).cloned().unwrap_or(0.0);
    // Define a base bounding box:
    //   x: 0 to glyph_width, y: from the font's overall bbox y-min to y-max.
    let base_bbox = Rect {
        x0: 0.0,
        y0: font.bbox.y0,
        x1: glyph_width,
        y1: font.bbox.y1,
    };
    // Transform the base bbox by the text rendering matrix.
    let transformed_bbox = transform_rect(&base_bbox, trm);
    // Optionally expand the box slightly (similar to MuPDF's fz_expand_rect).
    transformed_bbox.expand(1.0)
}