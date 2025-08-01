use crate::stubs::DebugDataStore;
use anyhow::Result;
use delver_core::layout::TextBlock;
use delver_core::process_pdf;
use eframe::egui;
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use uuid::Uuid;

#[cfg(target_arch = "wasm32")]
use {
    futures_channel::oneshot,
    std::sync::{LazyLock, Mutex},
    wasm_bindgen::{prelude::*, JsCast},
    wasm_bindgen_futures::JsFuture,
    web_sys::{window, File},
};

use crate::event_panel;
use crate::match_panel;
use crate::rendering;
use crate::store::{DocumentState, Store};
use crate::ui_controls;
use crate::utils;

// With the `sync` feature, Pdfium is thread-safe, so we can use std::sync primitives.
#[cfg(target_arch = "wasm32")]
static APP_STATE: LazyLock<Mutex<AppState>> = LazyLock::new(|| Mutex::new(AppState::Uninitialized));

#[cfg(target_arch = "wasm32")]
enum AppState {
    Uninitialized,
    Initialized(Viewer<'static>),
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn init_pdfium(pdfium_module: JsValue, rust_module: JsValue, debug: bool) -> bool {
    let pdfium = Pdfium::default();

    let mut app_state = APP_STATE.lock().unwrap();

    if let AppState::Uninitialized = *app_state {
        *app_state = AppState::Initialized(Viewer::new_wasm(pdfium));
        return true;
    }
    false
}

#[cfg(target_arch = "wasm32")]
pub struct AppWrapper;

#[cfg(target_arch = "wasm32")]
impl AppWrapper {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(target_arch = "wasm32")]
impl eframe::App for AppWrapper {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let mut app_state = APP_STATE.lock().unwrap();
        if let AppState::Initialized(viewer) = &mut *app_state {
            viewer.update(ctx, frame);
        } else {
            // Render a loading screen?
        }
    }
}

/// Main debug viewer application
pub struct Viewer<'a> {
    pdfium: Pdfium,
    pdf_bytes: Option<Vec<u8>>,
    pub pdf_document: Option<PdfDocument<'a>>,
    pdf_path: Option<PathBuf>,
    pub pdf_name: Option<String>,
    pub blocks: Vec<TextBlock>,
    pub debug_data: DebugDataStore,
    pub current_page: usize,
    pub textures: Vec<egui::TextureHandle>,
    pub pdf_dimensions: Vec<(f32, f32)>,
    pub show_text: bool,
    pub show_lines: bool,
    pub show_blocks: bool,
    pub show_grid: bool,
    pub store: Store,
    pub current_document_id: Option<Uuid>,
    pub grid_spacing: f32,
    pub zoom: f32,
    pub pan: egui::Vec2,
    pub selected_bbox: Option<(f32, f32, f32, f32)>,
    pub selected_line: Option<Uuid>,
    pub selected_fields: HashSet<String>,
    pub selected_events: HashSet<String>,
    pub show_tree_view: bool,
    pub show_matches: bool,
    pub show_match_panel: bool,
    pub highlighted_match: Option<(Uuid, Uuid)>,
    pub match_filter_threshold: f32,
    pub file_picker_channel: (Sender<Vec<u8>>, Receiver<Vec<u8>>),
}

#[derive(Serialize, Deserialize)]
struct SerializableAppState {
    // pdf_bytes: Option<Vec<u8>>,
    pdf_name: Option<String>,
    blocks: Vec<TextBlock>,
    current_page: usize,
    pdf_dimensions: Vec<(f32, f32)>,
    show_text: bool,
    show_lines: bool,
    show_blocks: bool,
}

impl<'a> Viewer<'a> {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let pdfium = Pdfium::new(
            Pdfium::bind_to_system_library().expect("failed to bind to system library"),
        );

        Self {
            pdfium,
            pdf_bytes: None,
            pdf_document: None,
            pdf_path: None,
            pdf_name: None,
            blocks: Vec::new(),
            debug_data: DebugDataStore::default(),
            current_page: 0,
            textures: Vec::new(),
            pdf_dimensions: Vec::new(),
            show_text: true,
            show_lines: true,
            show_blocks: true,
            show_grid: false,
            grid_spacing: 50.0,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            selected_bbox: None,
            selected_line: None,
            selected_fields: HashSet::new(),
            selected_events: HashSet::new(),
            store: Store::new("delver_viewer"),
            current_document_id: None,
            show_tree_view: false,
            show_matches: false,
            show_match_panel: false,
            highlighted_match: None,
            match_filter_threshold: 0.8,
            file_picker_channel: channel(),
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn new_wasm(pdfium: Pdfium) -> Self {
        let mut store = Store::new("delver_viewer");
        let current_document_id = store.documents.first().map(|d| d.id);

        Self {
            pdfium,
            pdf_bytes: None,
            pdf_document: None,
            pdf_path: None,
            pdf_name: None,
            blocks: Vec::new(),
            debug_data: DebugDataStore::default(),
            current_page: 0,
            textures: Vec::new(),
            pdf_dimensions: Vec::new(),
            show_text: true,
            show_lines: true,
            show_blocks: true,
            show_grid: false,
            grid_spacing: 50.0,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            selected_bbox: None,
            selected_line: None,
            selected_fields: HashSet::new(),
            selected_events: HashSet::new(),
            show_tree_view: false,
            show_matches: false,
            show_match_panel: false,
            store,
            current_document_id,
            highlighted_match: None,
            match_filter_threshold: 0.8,
            file_picker_channel: channel(),
        }
    }

    fn save_state_to_current_document(&mut self) {
        if let Some(id) = self.current_document_id {
            if let Some(doc) = self.store.documents.iter_mut().find(|d| d.id == id) {
                doc.pdf_name = self.pdf_name.clone();
                doc.blocks = self.blocks.clone();
                doc.current_page = self.current_page;
                doc.pdf_dimensions = self.pdf_dimensions.clone();
                doc.show_text = self.show_text;
                doc.show_lines = self.show_lines;
                doc.show_blocks = self.show_blocks;
            }
        }
    }

    fn load_state_from_document(&mut self, id: Uuid, ctx: &egui::Context) {
        self.save_state_to_current_document();

        if let Some(doc) = self.store.documents.iter().find(|d| d.id == id).cloned() {
            if let Some(bytes) = doc.pdf_bytes {
                self.load_pdf(bytes, doc.pdf_name, ctx);
                self.blocks = doc.blocks;
                self.current_page = doc.current_page;
                self.show_text = doc.show_text;
                self.show_lines = doc.show_lines;
                self.show_blocks = doc.show_blocks;
                self.current_document_id = Some(id);
            }
        }
    }

    fn load_pdf(&mut self, bytes: Vec<u8>, name: Option<String>, ctx: &egui::Context) {
        self.pdf_name = name;
        self.pdf_bytes = Some(bytes);
        self.pdf_document = unsafe {
            self.pdfium
                .load_pdf_from_byte_slice(self.pdf_bytes.as_ref().unwrap(), None)
                .ok()
                // The lifetime of the `PdfDocument` is transmuted to the lifetime of the `Viewer`.
                // This is safe because the `pdf_bytes` are owned by the `Viewer`.
                .map(|doc| std::mem::transmute::<PdfDocument<'_>, PdfDocument<'a>>(doc))
        };

        let pd = self.pdf_document.as_ref().unwrap();

        // Initialize textures for each page
        let mut textures = Vec::new();
        let mut page_dimensions = Vec::new();

        for page_index in 0..pd.pages().len() {
            let page: PdfPage = pd
                .pages()
                .get(page_index)
                .map_err(|e| anyhow::anyhow!("Failed to get page {}: {}", page_index, e))
                .unwrap();

            let width = page.width().value as i32;
            let height = page.height().value as i32;
            page_dimensions.push((width as f32, height as f32));

            let render_config = PdfRenderConfig::new()
                .set_target_width(width)
                .set_target_height(height)
                .use_lcd_text_rendering(true)
                .render_annotations(true)
                .render_form_data(false);

            let bitmap: PdfBitmap = page
                .render_with_config(&render_config)
                .map_err(|e| anyhow::anyhow!("Failed to render page {}: {}", page_index, e))
                .unwrap();

            // Convert to RGBA - use as_rgba_bytes() which handles format conversion
            let pixels = bitmap.as_rgba_bytes();

            // Create egui texture
            let texture = ctx.load_texture(
                format!("page_{}", page_index),
                egui::ColorImage::from_rgba_unmultiplied(
                    [width as usize, height as usize],
                    &pixels,
                ),
                egui::TextureOptions::NEAREST,
            );

            textures.push(texture);
        }

        self.textures = textures;
        self.pdf_dimensions = page_dimensions;

        // // Reset state from previous PDF
        // self.blocks.clear();
        // self.debug_data = DebugDataStore::default();
        // self.current_page = 0;
        // self.textures.clear();
        // self.pdf_dimensions.clear();
        // self.zoom = 1.0;
        // self.pan = egui::Vec2::ZERO;
        // self.selected_bbox = None;
        // self.selected_line = None;
        // self.selected_fields.clear();
        // self.selected_events.clear();
        // self.highlighted_match = None;

        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&"loaded pdf".into());
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.pdf_path = Some(path);
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn open_file_dialog(&mut self) {
        let task = rfd::AsyncFileDialog::new().pick_file();
        let channel = self.file_picker_channel.0.clone();

        utils::exec_future(async move {
            let file = task.await;
            if let Some(file) = file {
                let bytes = file.read().await;
                web_sys::console::log_1(&bytes.len().into());
                let _ = channel.send(bytes);
            }
        });
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.current_document_id.is_none() {
            if let Some(doc) = self.store.documents.first() {
                let id = doc.id;
                self.load_state_from_document(id, ctx);
            }
        }

        #[cfg(target_arch = "wasm32")]
        if let Ok(bytes) = self.file_picker_channel.1.try_recv() {
            self.save_state_to_current_document();

            let template_str = "TextChunk(chunkSize=500, chunkOverlap=150)";
            let (json, blocks, _doc) = process_pdf(&bytes, template_str, None).unwrap();
            web_sys::console::log_1(&json.into());

            let new_doc = DocumentState {
                id: Uuid::new_v4(),
                pdf_bytes: Some(bytes.clone()),
                pdf_name: Some("Untitled".to_string()),
                blocks,
                current_page: 0,
                pdf_dimensions: Vec::new(),
                show_text: true,
                show_lines: true,
                show_blocks: true,
            };
            let new_id = new_doc.id;
            self.store.documents.push(new_doc);
            self.load_state_from_document(new_id, ctx);
            self.store.save();
        }

        if self.show_match_panel {
            match_panel::show_match_panel(self, ctx);
        }

        if let Some(line_id) = self.selected_line {
            event_panel::show_event_panel(self, ctx, line_id);
        }

        egui::SidePanel::left("file_panel")
            .min_width(200.0)
            .show(ctx, |ui| {
                ui.heading("File");

                if ui.button("Open PDF").clicked() {
                    self.open_file_dialog();
                    ui.ctx().request_repaint();
                }

                if ui.button("Save Documents").clicked() {
                    self.save_state_to_current_document();
                    self.store.save();
                }

                ui.separator();
                ui.heading("Documents");

                let mut new_doc_id = None;
                let mut deleted_doc_id = None;

                for doc in &self.store.documents {
                    ui.horizontal(|ui| {
                        let name = doc.pdf_name.as_deref().unwrap_or("Untitled");
                        if ui
                            .selectable_label(self.current_document_id == Some(doc.id), name)
                            .clicked()
                        {
                            new_doc_id = Some(doc.id);
                        }
                        if ui.button("X").clicked() {
                            deleted_doc_id = Some(doc.id);
                        }
                    });
                }

                if let Some(id) = new_doc_id {
                    self.load_state_from_document(id, ui.ctx());
                }

                if let Some(id) = deleted_doc_id {
                    self.store.documents.retain(|d| d.id != id);
                    if self.current_document_id == Some(id) {
                        self.current_document_id = None;
                        self.pdf_document = None;
                        self.pdf_bytes = None;
                        self.blocks.clear();
                    }
                }

                #[cfg(not(target_arch = "wasm32"))]
                if let Some(pdf_path) = &self.pdf_path {
                    if self.pdf_document.is_none() {
                        if let Ok(bytes) = std::fs::read(pdf_path) {
                            self.load_pdf(bytes, self.pdf_name.clone(), ctx);
                        }
                    }
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            // Top controls
            ui_controls::show_controls(self, ui);

            // Render the PDF with all visualizations
            rendering::render_pdf_view(self, ui);
        });
    }
}

impl<'a> eframe::App for Viewer<'a> {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update(ctx, _frame);
    }
}

impl<'a> Serialize for Viewer<'a> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let state = SerializableAppState {
            // pdf_bytes: self.pdf_bytes.clone(),
            pdf_name: self.pdf_name.clone(),
            blocks: self.blocks.clone(),
            current_page: self.current_page,
            pdf_dimensions: self.pdf_dimensions.clone(),
            show_text: self.show_text,
            show_lines: self.show_lines,
            show_blocks: self.show_blocks,
        };
        state.serialize(serializer)
    }
}
