//! # PDF Parsing and Content Extraction
//!
//! This module handles the low-level parsing of PDF documents, extracting text elements,
//! images, and other content while preserving spatial relationships and metadata.
//! 
//! The parsing process converts PDF content streams into structured data that can be
//! efficiently searched and matched against templates.

use indexmap::IndexMap;
use std::collections::BTreeMap;
use std::fmt;
use std::io::{Error, ErrorKind};
use std::path::Path;
use uuid::Uuid;

use crate::geo::{multiply_matrices, pre_translate, transform_rect, Matrix, Rect, IDENTITY_MATRIX};
use crate::layout::MatchContext;
use lopdf::{
    Dictionary, Document, Encoding, Error as LopdfError, Object, Result as LopdfResult, Stream,
};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

#[cfg(feature = "extension-module")]
use tokio::runtime::Builder;

use crate::fonts::{canonicalize_font_name, FontMetrics, FONT_METRICS};

static IGNORE: &[&[u8]] = &[
    b"Length",
    b"BBox",
    b"FormType",
    b"Matrix",
    b"Type",
    b"XObject",
    b"Subtype",
    b"Filter",
    b"ColorSpace",
    b"Width",
    b"Height",
    b"BitsPerComponent",
    b"Length1",
    b"Length2",
    b"Length3",
    b"PTEX.FileName",
    b"PTEX.PageNumber",
    b"PTEX.InfoDict",
    // "FontDescriptor",
    b"ExtGState",
    // "MediaBox",
    b"Annot",
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
    }
    Some((object_id, object.to_owned()))
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PdfText {
    pub text: BTreeMap<u32, Vec<String>>, // Key is page number
    pub errors: Vec<String>,
}

#[cfg(not(feature = "extension-module"))]
pub fn load_pdf<P: AsRef<Path>>(path: P) -> Result<Document, Error> {
    // Restore original logic
    if !cfg!(debug_assertions) {
        Document::load(path).map_err(|e| Error::new(ErrorKind::Other, e.to_string()))
    } else {
        Document::load_filtered(path, filter_func)
            .map_err(|e| Error::new(ErrorKind::Other, e.to_string()))
    }
}

#[cfg(feature = "extension-module")]
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
    font_name: Option<String>,
    text_matrix: Matrix,
    text_line_matrix: Matrix,
    glyphs: Vec<PositionedGlyph>,
    text_buffer: String,
    font_metrics: Option<&'static FontMetrics>,
    _current_encoding: Option<&'a Encoding<'a>>,
    _current_metrics: Option<&'static FontMetrics>,
    operator_log: Vec<String>,
    _char_bbox: Option<Rect>,
    _char_tx: f32,
    _char_ty: f32,
}

impl<'a> Default for TextObjectState<'a> {
    fn default() -> Self {
        TextObjectState {
            font_name: None,
            text_matrix: IDENTITY_MATRIX,
            text_line_matrix: IDENTITY_MATRIX,
            glyphs: Vec::new(),
            text_buffer: String::new(),
            font_metrics: None,
            _current_encoding: None,
            _current_metrics: None,
            operator_log: Vec::new(),
            _char_bbox: None,
            _char_tx: 0.0,
            _char_ty: 0.0,
        }
    }
}

impl<'a> fmt::Debug for TextObjectState<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextState")
            .field("text_matrix", &self.text_matrix)
            .field("font_name", &self.font_name)
            .field(
                "font_metrics",
                &self.font_metrics.map(|m| (m.ascent, m.descent)),
            )
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
    font: Option<&'static FontMetrics>,
    font_dict: Option<Object>,
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
            font_dict: None,
            fontname: String::new(),
            encoding: None,
            size: 0.0,
            render: 0,
            rise: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
struct PositionedGlyph {
    _cid: u32,
    _unicode: char,
    _text_matrix: Matrix,
    _device_matrix: Matrix,
    bbox: Rect,
    _advance: f32,
}

/// Represents a text element extracted from a PDF document
///
/// Each text element corresponds to a piece of text found in the PDF with its
/// associated formatting and positioning information.
#[derive(Debug, Clone, PartialEq)]
pub struct TextElement {
    /// Unique identifier for this text element
    pub id: Uuid,
    /// The actual text content
    pub text: String,
    /// Font size in points
    pub font_size: f32,
    /// Name of the font family, if available
    pub font_name: Option<String>,
    /// Bounding box coordinates (x0, y0, x1, y1) in PDF coordinate space
    pub bbox: (f32, f32, f32, f32),
    /// Page number where this element appears (1-indexed)
    pub page_number: u32,
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
        }
    }
}

impl fmt::Display for TextElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TextElement {{\n  text: \"{}\",\n  bbox: {:?},\n  font: {}pt{}}}",
            self.text,
            self.bbox,
            self.font_size,
            self.font_name.as_deref().unwrap_or("unknown"),
        )
    }
}

/// Represents an image element extracted from a PDF document
#[derive(Debug, Clone)]
pub struct ImageElement {
    /// Unique identifier for this image element
    pub id: Uuid,
    /// Page number where this image appears (1-indexed)
    pub page_number: u32,
    /// Bounding box coordinates in PDF coordinate space
    pub bbox: Rect,
    /// Raw PDF image object containing the image data
    pub image_object: Object,
}

/// Lightweight handle that preserves document order
#[derive(Copy, Clone, Debug)]
pub enum ContentHandle {
    Text(usize),
    Image(usize),
}

/// Column-oriented storage for text elements
#[derive(Debug, Default, Clone)]
pub struct TextStore {
    pub bbox: Vec<(f32, f32, f32, f32)>,
    pub font_size: Vec<f32>,
    pub font_name: Vec<Option<String>>,
    pub id: Vec<Uuid>,
    pub text: Vec<String>,
    pub page_number: Vec<u32>,
}

impl TextStore {
    pub fn push(&mut self, elem: TextElement) -> usize {
        let idx = self.id.len();
        self.bbox.push(elem.bbox);
        self.font_size.push(elem.font_size);
        self.font_name.push(elem.font_name);
        self.id.push(elem.id);
        self.text.push(elem.text);
        self.page_number.push(elem.page_number);
        idx
    }

    pub fn get(&self, idx: usize) -> Option<TextElement> {
        if idx < self.id.len() {
            Some(TextElement {
                id: self.id[idx],
                text: self.text[idx].clone(),
                font_size: self.font_size[idx],
                font_name: self.font_name[idx].clone(),
                bbox: self.bbox[idx],
                page_number: self.page_number[idx],
            })
        } else {
            None
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = TextElement> + '_ {
        (0..self.id.len()).map(move |i| TextElement {
            id: self.id[i],
            text: self.text[i].clone(),
            font_size: self.font_size[i],
            font_name: self.font_name[i].clone(),
            bbox: self.bbox[i],
            page_number: self.page_number[i],
        })
    }
}

/// Column-oriented storage for image elements
#[derive(Debug, Default, Clone)]
pub struct ImageStore {
    pub bbox: Vec<crate::geo::Rect>,
    pub id: Vec<Uuid>,
    pub page_number: Vec<u32>,
    pub image_object: Vec<Object>,
}

impl ImageStore {
    pub fn push(&mut self, elem: ImageElement) -> usize {
        let idx = self.id.len();
        self.bbox.push(elem.bbox);
        self.id.push(elem.id);
        self.page_number.push(elem.page_number);
        self.image_object.push(elem.image_object);
        idx
    }

    pub fn get(&self, idx: usize) -> Option<ImageElement> {
        if idx < self.id.len() {
            Some(ImageElement {
                id: self.id[idx],
                page_number: self.page_number[idx],
                bbox: self.bbox[idx],
                image_object: self.image_object[idx].clone(),
            })
        } else {
            None
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = ImageElement> + '_ {
        (0..self.id.len()).map(move |i| ImageElement {
            id: self.id[i],
            page_number: self.page_number[i],
            bbox: self.bbox[i],
            image_object: self.image_object[i].clone(),
        })
    }
}

/// Struct-of-Arrays for efficient content storage with preserved ordering
#[derive(Debug, Clone)]
pub struct PageContents {
    pub order: Vec<ContentHandle>,
    pub text_store: TextStore,
    pub image_store: ImageStore,
}

impl PageContents {
    pub fn new() -> Self {
        Self {
            order: Vec::new(),
            text_store: TextStore::default(),
            image_store: ImageStore::default(),
        }
    }

    pub fn add_text(&mut self, text_elem: TextElement) {
        let idx = self.text_store.push(text_elem);
        self.order.push(ContentHandle::Text(idx));
    }

    pub fn add_image(&mut self, image_elem: ImageElement) {
        let idx = self.image_store.push(image_elem);
        self.order.push(ContentHandle::Image(idx));
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Iterate through content in document order
    pub fn iter_ordered(&self) -> impl Iterator<Item = PageContent> + '_ {
        self.order.iter().filter_map(move |handle| match handle {
            ContentHandle::Text(idx) => self.text_store.get(*idx).map(PageContent::Text),
            ContentHandle::Image(idx) => self.image_store.get(*idx).map(PageContent::Image),
        })
    }

    /// Get content by index in document order
    pub fn get_content(&self, idx: usize) -> Option<PageContent> {
        self.order.get(idx).and_then(|handle| match handle {
            ContentHandle::Text(text_idx) => self.text_store.get(*text_idx).map(PageContent::Text),
            ContentHandle::Image(img_idx) => self.image_store.get(*img_idx).map(PageContent::Image),
        })
    }

    /// Get all text elements as a Vec (for compatibility)
    pub fn text_elements(&self) -> Vec<TextElement> {
        (0..self.text_store.id.len())
            .filter_map(|idx| self.text_store.get(idx))
            .collect()
    }
}

// Define enum to hold either TextElement or ImageElement (for backwards compatibility where needed)
#[derive(Debug, Clone)]
pub enum PageContent {
    Text(TextElement),
    Image(ImageElement),
}

impl PageContent {
    pub fn id(&self) -> Uuid {
        match self {
            PageContent::Text(element) => element.id,
            PageContent::Image(element) => element.id,
        }
    }

    pub fn bbox(&self) -> Rect {
        match self {
            PageContent::Text(element) => element.bbox.into(),
            PageContent::Image(element) => element.bbox.into(),
        }
    }

    pub fn page_number(&self) -> u32 {
        match self {
            PageContent::Text(element) => element.page_number,
            PageContent::Image(element) => element.page_number,
        }
    }

    pub fn is_text(&self) -> bool {
        matches!(self, PageContent::Text(_))
    }

    pub fn is_image(&self) -> bool {
        matches!(self, PageContent::Image(_))
    }

    // Add text-specific helper methods
    pub fn as_text(&self) -> Option<&TextElement> {
        match self {
            PageContent::Text(text) => Some(text),
            PageContent::Image(_) => None,
        }
    }

    pub fn as_image(&self) -> Option<&ImageElement> {
        match self {
            PageContent::Text(_) => None,
            PageContent::Image(image) => Some(image),
        }
    }

    pub fn text(&self) -> Option<&str> {
        self.as_text().map(|t| t.text.as_str())
    }

    pub fn font_size(&self) -> Option<f32> {
        self.as_text().map(|t| t.font_size)
    }

    pub fn font_name(&self) -> Option<&str> {
        self.as_text().and_then(|t| t.font_name.as_deref())
    }
}

fn process_glyph(
    tos: &mut TextObjectState,
    ts: &mut TextState,
    operand: &Object,
    ctm: Matrix,
) -> LopdfResult<()> {
    let encoding = ts.encoding.as_ref().ok_or(LopdfError::CharacterEncoding)?;

    match operand {
        Object::String(bytes, _) => {
            // Current assumiptions:
            // 1. The encoding is either a one-byte encoding or a Unicode map encoding (WinAnsi, MacRoman, etc.)
            // 2. Font uses identity CMap (CID = byte value)
            // 3. No vertical text layouts
            let decoded_text = Document::decode_text(encoding, bytes)?;

            for ch in decoded_text.chars() {
                let cid = ch as u32;

                let metrics = ts.font.unwrap();

                let tsm = Matrix {
                    a: ts.size * ts.scale / 1000.0,
                    b: 0.0,
                    c: 0.0,
                    d: ts.size / 1000.0,
                    e: 0.0,
                    f: ts.rise,
                };

                let mut advance = metrics
                    .glyph_widths
                    .get(&cid)
                    .map(|w| (w / 1000.0) * ts.size)
                    .unwrap_or(0.0);

                if ch == ' ' {
                    advance += ts.word_space;
                }
                advance += ts.char_space;

                // Calculate TRM = TSM × Tm (PDF spec order)
                let trm_temp = multiply_matrices(&tsm, &tos.text_matrix);
                let trm = multiply_matrices(&trm_temp, &ctm);

                let char_bbox = glyph_bound(metrics, cid, &trm);

                tos.glyphs.push(PositionedGlyph {
                    _cid: cid,
                    _unicode: ch,
                    _text_matrix: tos.text_matrix,
                    _device_matrix: trm,
                    bbox: char_bbox,
                    _advance: advance,
                });

                // Only add the character to the text buffer
                tos.text_buffer.push(ch);
            }
        }
        Object::Integer(i) => {
            let offset = -*i as f32 * (ts.size / 1000.0);
            tos.text_matrix.e += offset;
        }
        Object::Real(f) => {
            let offset = -*f as f32 * (ts.size / 1000.0);
            tos.text_matrix.e += offset;
        }
        Object::Array(arr) => {
            collect_text_glyphs(tos, ts, arr, ctm)?;
        }
        _ => {}
    }
    Ok(())
}

fn collect_text_glyphs(
    text_object_state: &mut TextObjectState,
    text_state: &mut TextState,
    operands: &[Object],
    ctm: Matrix,
) -> LopdfResult<()> {
    for operand in operands {
        process_glyph(text_object_state, text_state, operand, ctm)?;
    }
    Ok(())
}

#[tracing::instrument()]
fn finalize_text_run(
    tos: &mut TextObjectState,
    ts: &TextState,
    page_number: u32,
) -> Option<TextElement> {
    // If both glyphs and text buffer are empty, there's nothing to return
    if tos.glyphs.is_empty()
    // && tos.text_buffer.trim().is_empty()
    {
        return None;
    }

    // For empty glyphs but with text content, create a simple text element
    if tos.glyphs.is_empty() {
        // Preserve text content from the buffer
        let text = std::mem::take(&mut tos.text_buffer);

        return Some(TextElement {
            id: Uuid::new_v4(),
            text,
            font_size: ts.size,
            font_name: Some(ts.fontname.clone()),
            bbox: (0.0, 0.0, ts.size, ts.size),
            page_number,
        });
    }

    let mut x_min = f32::MAX;
    let mut y_min = f32::MAX;
    let mut x_max = f32::MIN;
    let mut y_max = f32::MIN;

    for g in &tos.glyphs {
        x_min = x_min.min(g.bbox.x0);
        y_min = y_min.min(g.bbox.y0);
        x_max = x_max.max(g.bbox.x1);
        y_max = y_max.max(g.bbox.y1);
    }

    let text_run = std::mem::take(&mut tos.text_buffer);
    tos.glyphs.clear();

    let text_element = TextElement {
        id: Uuid::new_v4(),
        text: text_run,
        font_size: ts.size,
        font_name: Some(ts.fontname.clone()),
        bbox: (x_min, y_min, x_max, y_max), // Bbox as tuple
        page_number,
    };

    debug!(
        element_id = %text_element.id,
        line_id = tracing::field::Empty,
        text_element = ?text_element,
        state = ?tos,
        "Created text element"
    );

    Some(text_element)
}

pub fn get_page_content(doc: &Document) -> Result<BTreeMap<u32, PageContents>, Error> {
    let mut pages_map: BTreeMap<u32, PageContents> = BTreeMap::new();

    let results: Result<Vec<(u32, PageContents)>, Error> = doc
        .get_pages()
        .into_par_iter()
        .map(|(page_num, page_id)| {
            let page_contents = get_page_elements(doc, page_num, page_id).map_err(|e| {
                Error::new(
                    ErrorKind::Other,
                    format!("Failed to extract content from page {page_num} id={page_id:?}: {e:?}"),
                )
            })?;
            Ok((page_num, page_contents))
        })
        .collect();

    for (page_num, contents) in results? {
        pages_map.insert(page_num, contents);
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

// New helper struct to avoid passing the entire document
struct PageObjects {
    font_objects: BTreeMap<Vec<u8>, Object>,
    xobject_streams: BTreeMap<Vec<u8>, Object>,
}

impl PageObjects {
    // Create a new PageObjects by preloading objects from the document
    fn new(doc: &Document, resources: &Dictionary) -> Result<Self, LopdfError> {
        let mut font_objects = BTreeMap::new();
        let mut xobject_streams = BTreeMap::new();

        // Preload font objects
        if let Ok(fonts_dict) = resources.get(b"Font").and_then(Object::as_dict) {
            for (name, obj) in fonts_dict.iter() {
                if let Ok(ref_id) = obj.as_reference() {
                    if let Ok(font_obj) = doc.get_object(ref_id) {
                        font_objects.insert(name.clone(), font_obj.clone());
                    }
                }
            }
        }

        // Preload XObject streams
        if let Ok(xobjects_dict) = resources.get(b"XObject").and_then(Object::as_dict) {
            for (name, obj) in xobjects_dict.iter() {
                if let Ok(ref_id) = obj.as_reference() {
                    if let Ok(xobject) = doc.get_object(ref_id) {
                        xobject_streams.insert(name.clone(), xobject.clone());
                    }
                }
            }
        }

        Ok(Self {
            font_objects,
            xobject_streams,
        })
    }

    // Get a font object by name
    fn get_font(&self, name: &[u8]) -> Option<&Object> {
        self.font_objects.get(name)
    }

    // Get an XObject stream by name
    fn get_xobject(&self, name: &[u8]) -> Option<&Object> {
        self.xobject_streams.get(name)
    }
}

#[tracing::instrument(
    skip_all,
    fields(
        operator = %op.operator,
        params = ?op.operands,
        in_text_object = tracing::field::Empty // Placeholder
    )
)]
fn handle_operator<'a>(
    gs_stack: &mut Vec<GraphicsState<'a>>,
    op: &lopdf::content::Operation,
    text_object_state: &mut TextObjectState,
    page_contents: &mut PageContents,
    page_number: u32,
    page_objects: &PageObjects, // Replace doc with preloaded objects
    encodings: &'a BTreeMap<Vec<u8>, Encoding<'a>>,
) -> Result<(), LopdfError> {
    let current_gs = gs_stack.last_mut().unwrap();
    let in_text_object =
        !text_object_state.text_buffer.is_empty() || !text_object_state.glyphs.is_empty();
    tracing::Span::current().record("in_text_object", &in_text_object);

    match op.operator.as_ref() {
        // Graphics State
        "q" => push_graphics_state(gs_stack),
        "Q" => pop_graphics_state(gs_stack),
        "cm" => {
            // Finalize any pending text run before CTM change
            if let Some(text_elem) =
                finalize_text_run(text_object_state, &current_gs.text_state, page_number)
            {
                page_contents.add_text(text_elem);
            }
            let matrix = matrix_from_operands(op);
            current_gs.ctm = multiply_matrices(&matrix, &current_gs.ctm);
        }
        // Text Object
        "BT" => {
            // Finalize any pending graphics element (if any were handled outside text object)
            debug!("Begin text object");
            text_object_state.text_matrix = IDENTITY_MATRIX;
            text_object_state.text_line_matrix = IDENTITY_MATRIX;
        }
        "ET" => {
            // Finalize the last text run within the text object
            if let Some(text_elem) =
                finalize_text_run(text_object_state, &current_gs.text_state, page_number)
            {
                page_contents.add_text(text_elem);
            }
            // Clear text state specifics
            text_object_state.glyphs.clear();
            text_object_state.text_buffer.clear();
            text_object_state.operator_log.clear();
            debug!("End text object");
        }
        // Text State
        "Tf" => {
            if let Some(text_elem) =
                finalize_text_run(text_object_state, &current_gs.text_state, page_number)
            {
                page_contents.add_text(text_elem);
            }
            if let (Some(Object::Name(font_name_bytes)), Some(font_size_obj)) =
                (op.operands.get(0), op.operands.get(1))
            {
                let font_size = operand_as_float(font_size_obj);
                // Use preloaded fonts instead of querying document
                if let Some(font_obj) = page_objects.get_font(font_name_bytes) {
                    if let Ok(dict) = font_obj.as_dict() {
                        let base_font = dict
                            .get(b"BaseFont")
                            .and_then(Object::as_name)
                            .map(|name| String::from_utf8_lossy(name).into_owned())
                            .map(|name_string| canonicalize_font_name(name_string.as_str()))
                            .unwrap_or_else(|_| "".to_string());

                        current_gs.text_state.fontname = base_font.to_string();
                        current_gs.text_state.size = font_size;
                        current_gs.text_state.font_dict = Some(font_obj.clone());
                        current_gs.text_state.font = FONT_METRICS.get(base_font.as_str()).copied();
                        current_gs.text_state.encoding = encodings.get(font_name_bytes);
                        text_object_state.font_name = Some(current_gs.text_state.fontname.clone());
                        text_object_state.font_metrics = current_gs.text_state.font;
                    } else {
                        warn!(font_name=?String::from_utf8_lossy(font_name_bytes), "Font object is not a dictionary");
                    }
                } else {
                    warn!(font_name=?String::from_utf8_lossy(font_name_bytes), "Font not found in preloaded objects");
                }
            } else {
                warn!("Tf operator missing font name or size operand");
            }
        }
        "Tc" => {
            if let Some(spacing) = op.operands.first() {
                current_gs.text_state.char_space = operand_as_float(spacing)
            }
        }
        "Tw" => {
            if let Some(spacing) = op.operands.first() {
                current_gs.text_state.word_space = operand_as_float(spacing)
            }
        }
        "Tz" => {
            if let Some(scale_percent) = op.operands.first() {
                current_gs.text_state.scale = operand_as_float(scale_percent) / 100.0
            }
        }
        "TL" => {
            if let Some(leading) = op.operands.first() {
                current_gs.text_state.leading = operand_as_float(leading)
            }
        }
        "Tr" => {
            if let Some(render_mode) = op.operands.first() {
                current_gs.text_state.render = operand_as_u8(render_mode)
            }
        }
        "Ts" => {
            if let Some(rise) = op.operands.first() {
                current_gs.text_state.rise = operand_as_float(rise)
            }
        }
        "Tm" => {
            // Finalize pending text before matrix change
            if let Some(text_elem) =
                finalize_text_run(text_object_state, &current_gs.text_state, page_number)
            {
                page_contents.add_text(text_elem);
            }
            let matrix = matrix_from_operands(op);
            text_object_state.text_matrix = matrix;
            text_object_state.text_line_matrix = matrix;
            text_object_state
                .operator_log
                .push(format!("Tm {:?}", matrix));
        }
        "Td" => {
            if let (Some(tx_obj), Some(ty_obj)) = (op.operands.get(0), op.operands.get(1)) {
                let tx = operand_as_float(tx_obj);
                let ty = operand_as_float(ty_obj);
                text_object_state.text_line_matrix =
                    pre_translate(text_object_state.text_line_matrix, tx, ty);
                text_object_state.text_matrix = text_object_state.text_line_matrix;
            }
        }
        "TD" => {
            // Move text pos and set leading
            if let (Some(tx_obj), Some(ty_obj)) = (op.operands.get(0), op.operands.get(1)) {
                let tx = operand_as_float(tx_obj);
                let ty = operand_as_float(ty_obj);
                current_gs.text_state.leading = -ty;
                text_object_state.text_line_matrix =
                    pre_translate(text_object_state.text_line_matrix, tx, ty);
                text_object_state.text_matrix = text_object_state.text_line_matrix;
            }
        }
        "T*" => {
            let tx = 0.0;
            let ty = -current_gs.text_state.leading;
            text_object_state.text_line_matrix =
                pre_translate(text_object_state.text_line_matrix, tx, ty);
            text_object_state.text_matrix = text_object_state.text_line_matrix;
        }
        // Text Showing
        "Tj" | "TJ" | "'" | "\"" => {
            text_object_state
                .operator_log
                .push(format!("{} {:?}", op.operator, op.operands));
            collect_text_glyphs(
                text_object_state,
                &mut current_gs.text_state,
                &op.operands,
                current_gs.ctm,
            )?;
            // NOTE: Don't finalize here, wait for ET or explicit text state change
        }
        // Handling XObjects (Images)
        "Do" => {
            // Finalize any pending text run before handling graphics object
            if let Some(text_elem) =
                finalize_text_run(text_object_state, &current_gs.text_state, page_number)
            {
                page_contents.add_text(text_elem);
            }

            if let Some(Object::Name(name)) = op.operands.first() {
                // Use preloaded XObjects instead of querying document
                if let Some(xobject) = page_objects.get_xobject(name) {
                    if let Ok(stream) = xobject.as_stream() {
                        if stream.dict.get(b"Subtype").and_then(Object::as_name).ok()
                            == Some(b"Image".as_ref())
                        {
                            debug!(xobject_name = ?String::from_utf8_lossy(name), "Found Image XObject");
                            // --- Image Found ---
                            // Calculate BBox - Placeholder: Assume image is 100x100 pts at current origin
                            // Real implementation needs CTM and image dimensions
                            let origin = multiply_matrices(&IDENTITY_MATRIX, &current_gs.ctm);
                            let corner = multiply_matrices(
                                &Matrix {
                                    a: 1.0,
                                    b: 0.0,
                                    c: 0.0,
                                    d: 1.0,
                                    e: 100.0,
                                    f: 100.0,
                                },
                                &current_gs.ctm,
                            );
                            let bbox = Rect {
                                x0: origin.e,
                                y0: origin.f,
                                x1: corner.e, // Simplified - needs proper transform
                                y1: corner.f, // Simplified - needs proper transform
                            };

                            let image_element = ImageElement {
                                id: Uuid::new_v4(),
                                page_number,
                                bbox,
                                image_object: xobject.clone(), // Clone the object (Stream)
                            };
                            page_contents.add_image(image_element);
                        }
                    } else {
                        warn!(xobject_name=?String::from_utf8_lossy(name), "XObject is not a stream");
                    }
                } else {
                    warn!(xobject_name=?String::from_utf8_lossy(name), "XObject not found in preloaded objects");
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn pdf_page_transform(page_dict: &Dictionary) -> (Rect, Matrix) {
    // Get MediaBox
    let mediabox = page_dict
        .get(b"MediaBox")
        .and_then(|obj| obj.as_array())
        .map(|arr| {
            let mut box_rect = [0.0; 4];
            for (i, obj) in arr.iter().take(4).enumerate() {
                box_rect[i] = match obj {
                    Object::Integer(i) => *i as f32,
                    Object::Real(f) => *f,
                    _ => 0.0,
                };
            }
            Rect {
                x0: box_rect[0],
                y0: box_rect[1],
                x1: box_rect[2],
                y1: box_rect[3],
            }
        })
        .unwrap_or(Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 612.0,
            y1: 792.0,
        });

    // Check for rotation
    let rotate = page_dict
        .get(b"Rotate")
        .and_then(|obj| obj.as_i64())
        .unwrap_or(0) as i32;

    // Calculate the transform matrix
    let mut ctm = IDENTITY_MATRIX;

    // Apply rotation if present
    if rotate != 0 {
        let rx = (mediabox.x0 + mediabox.x1) * 0.5;
        let ry = (mediabox.y0 + mediabox.y1) * 0.5;

        // Translate to origin, rotate, translate back
        ctm = pre_translate(ctm, -rx, -ry);
        ctm = multiply_matrices(
            &Matrix {
                a: (rotate == 90 || rotate == -270) as i32 as f32 * -1.0
                    + (rotate == 0 || rotate == 180) as i32 as f32,
                b: (rotate == 90 || rotate == -270) as i32 as f32,
                c: (rotate == 270 || rotate == -90) as i32 as f32,
                d: (rotate == 270 || rotate == -90) as i32 as f32 * -1.0
                    + (rotate == 0 || rotate == 180) as i32 as f32,
                e: 0.0,
                f: 0.0,
            },
            &ctm,
        );
        ctm = pre_translate(ctm, rx, ry);
    }

    (mediabox, ctm)
}

/// Group small text elements into larger ones based on spatial proximity and font consistency
/// Similar to layout.rs group_text_into_lines but creates consolidated TextElements instead of TextLines
fn group_text_elements(
    text_elements: Vec<TextElement>,
    line_join_threshold: f32,
) -> Vec<TextElement> {
    if text_elements.is_empty() {
        return text_elements;
    }

    let mut elements = text_elements;
    // Sort by y-coordinate (top to bottom) then x-coordinate (left to right)
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

    let mut grouped_elements = Vec::new();
    let mut current_group = Vec::new();
    let mut last_y = f32::MAX;
    let mut last_font_name: Option<String> = None;
    let mut last_font_size: Option<f32> = None;

    for elem in elements {
        let current_font_name = elem.font_name.clone();
        let current_font_size = Some(elem.font_size);

        if current_group.is_empty() {
            current_group.push(elem);
            last_y = current_group[0].bbox.1;
            last_font_name = current_font_name;
            last_font_size = current_font_size;
        } else {
            let y_close = (last_y - elem.bbox.1).abs() < line_join_threshold;
            let font_matches = last_font_name == current_font_name
                && last_font_size.map_or(false, |last_size| {
                    current_font_size.map_or(false, |curr_size| (last_size - curr_size).abs() < 0.1)
                });

            if y_close && font_matches {
                current_group.push(elem);
            } else {
                // Finalize current group and start new one
                if !current_group.is_empty() {
                    grouped_elements.push(create_consolidated_text_element(current_group));
                }
                current_group = vec![elem];
                last_y = current_group[0].bbox.1;
                last_font_name = current_font_name;
                last_font_size = current_font_size;
            }
        }
    }

    // Don't forget the last group
    if !current_group.is_empty() {
        grouped_elements.push(create_consolidated_text_element(current_group));
    }

    grouped_elements
}

/// Create a single consolidated TextElement from a group of TextElements
fn create_consolidated_text_element(mut elements: Vec<TextElement>) -> TextElement {
    if elements.is_empty() {
        return TextElement::new(String::new());
    }

    if elements.len() == 1 {
        return elements.into_iter().next().unwrap();
    }

    // Sort elements by x-coordinate for proper text ordering
    elements.sort_by(|a, b| {
        a.bbox
            .0
            .partial_cmp(&b.bbox.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Calculate consolidated bounding box
    let mut x_min = f32::MAX;
    let mut y_min = f32::MAX;
    let mut x_max = f32::MIN;
    let mut y_max = f32::MIN;

    for elem in &elements {
        x_min = x_min.min(elem.bbox.0);
        y_min = y_min.min(elem.bbox.1);
        x_max = x_max.max(elem.bbox.2);
        y_max = y_max.max(elem.bbox.3);
    }

    // Combine text content
    let combined_text = elements
        .iter()
        .map(|e| e.text.as_str())
        .collect::<Vec<_>>()
        .join("");

    // Use properties from the first element (since they should be homogeneous due to grouping logic)
    let first_elem = &elements[0];

    TextElement {
        id: Uuid::new_v4(),
        text: combined_text,
        font_size: first_elem.font_size,
        font_name: first_elem.font_name.clone(),
        bbox: (x_min, y_min, x_max, y_max),
        page_number: first_elem.page_number,
    }
}

fn get_page_elements(
    doc: &Document,
    page_number: u32,
    page_id: (u32, u16),
) -> Result<PageContents, LopdfError> {
    let mut page_contents = PageContents::new();
    let mut text_object_state = TextObjectState::default();

    let content_data = match doc.get_and_decode_page_content(page_id) {
        Ok(content) => content,
        Err(e) => {
            error!(page=%page_number, "Failed to decode content: {}", e);
            return Err(e);
        }
    };
    let page_dict = doc.get_dictionary(page_id)?;
    let resources = page_dict
        .get(b"Resources")
        .and_then(|o| doc.get_object(o.as_reference()?))
        .and_then(|o| o.as_dict())?;

    // Calculate page transform and mediabox
    let (mediabox, page_ctm) = pdf_page_transform(page_dict);

    // Initialize graphics state with this transform
    let mut gs_stack = vec![GraphicsState {
        ctm: page_ctm,
        text_state: TextState::default(),
    }];

    // Create PageObjects to preload fonts and XObjects
    let page_objects = PageObjects::new(doc, resources)?;

    // Create encodings map for font processing
    let mut encodings: BTreeMap<Vec<u8>, Encoding> = BTreeMap::new();

    // Process fonts to extract encodings
    if let Ok(fonts_dict) = resources.get(b"Font").and_then(Object::as_dict) {
        for (name, obj) in fonts_dict.iter() {
            if let Ok(ref_id) = obj.as_reference() {
                if let Ok(font_obj) = doc.get_object(ref_id) {
                    if let Ok(font_dict) = font_obj.as_dict() {
                        if let Ok(encoding) = font_dict.get_font_encoding(doc) {
                            encodings.insert(name.clone(), encoding);
                        }
                    }
                }
            }
        }
    }

    for (_i, op) in content_data.operations.iter().enumerate() {
        // Filter relevant operators (expanded to include graphics state)
        if matches!(
            op.operator.as_ref(),
            "BT" | "ET"
                | "Tm"
                | "Td"
                | "TD"
                | "T*"
                | "Tf"
                | "Tc"
                | "Tw"
                | "Tz"
                | "TL"
                | "Tr"
                | "Ts"
                | "Tj"
                | "TJ"
                | "'"
                | "\""
                | "cm"
                | "q"
                | "Q"
                | "Do"
        ) {
            if let Err(e) = handle_operator(
                &mut gs_stack,
                &op,
                &mut text_object_state,
                &mut page_contents,
                page_number,
                &page_objects,
                &encodings,
            ) {
                error!(page=%page_number, operator=%op.operator, error=?e, "Error handling operator");
                // Decide whether to continue or return error
                // return Err(e);
            }
        }
    }

    // Finalize any pending text object state after processing all operators
    if let Some(text_elem) = finalize_text_run(
        &mut text_object_state,
        &gs_stack.last().unwrap().text_state,
        page_number,
    ) {
        page_contents.add_text(text_elem);
    }

    // Group small text elements into larger ones before coordinate transformation
    let text_elements: Vec<TextElement> = page_contents.text_elements();
    let line_join_threshold = 5.0; // Adjust this threshold as needed
    let grouped_elements = group_text_elements(text_elements, line_join_threshold);

    // Replace the existing text elements with grouped ones
    page_contents.text_store = TextStore::default();
    page_contents
        .order
        .retain(|handle| matches!(handle, ContentHandle::Image(_)));

    for grouped_elem in grouped_elements {
        page_contents.add_text(grouped_elem);
    }

    // After processing content, convert coordinates to top-left based system
    for bbox in &mut page_contents.text_store.bbox {
        let (x0, y0, x1, y1) = *bbox;
        let top_left_bbox = (
            x0,
            mediabox.y1 - y1, // Top = page_height - bottom
            x1,
            mediabox.y1 - y0, // Bottom = page_height - top
        );
        *bbox = top_left_bbox;
    }

    for bbox in &mut page_contents.image_store.bbox {
        // Transform image bbox as well
        let transformed_bbox = transform_rect(bbox, &IDENTITY_MATRIX); // Using Identity, assumes bbox is already in page space?
                                                                       // TODO: Verify CTM usage for image bbox
        let top_left_bbox = Rect {
            x0: transformed_bbox.x0,
            y0: mediabox.y1 - transformed_bbox.y1,
            x1: transformed_bbox.x1,
            y1: mediabox.y1 - transformed_bbox.y0,
        };
        *bbox = top_left_bbox;
    }

    Ok(page_contents)
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

    let context = MatchContext { destinations };

    Ok(context)
}

/// The transformed bounding box as a `Rect`.
pub fn glyph_bound(font: &FontMetrics, glyph_id: u32, trm: &Matrix) -> Rect {
    // Look up the glyph width; if not present, default to 0.0.
    let glyph_width = font.glyph_widths.get(&glyph_id).cloned().unwrap_or(0.0);

    let base_bbox = Rect {
        x0: 0.0,
        y0: font.descent as f32,
        x1: glyph_width,
        y1: font.ascent as f32,
    };

    let transformed_bbox = transform_rect(&base_bbox, trm);

    transformed_bbox
}
