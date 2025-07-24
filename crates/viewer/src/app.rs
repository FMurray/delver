use crate::stubs::DebugDataStore;
use anyhow::Result;
use delver_core::layout::TextBlock;
use eframe::egui;
use pdfium_render::prelude::*;
use std::collections::HashSet;
use std::path::PathBuf;
use uuid::Uuid;

#[cfg(target_arch = "wasm32")]
use {
    futures_channel::oneshot,
    wasm_bindgen::{prelude::*, JsCast},
    wasm_bindgen_futures::JsFuture,
    web_sys::{window, File},
};

use crate::event_panel;
use crate::match_panel;
use crate::rendering;
use crate::ui_controls;

/// Main debug viewer application
pub struct Viewer<'a> {
    pdfium: Pdfium,
    pdf_bytes: Option<Vec<u8>>,
    pub pdf_document: Option<PdfDocument<'a>>,
    pdf_path: Option<PathBuf>,
    pub blocks: Vec<TextBlock>,
    pub debug_data: DebugDataStore,
    pub current_page: usize,
    pub textures: Vec<egui::TextureHandle>,
    pub pdf_dimensions: Vec<(f32, f32)>,
    pub show_text: bool,
    pub show_lines: bool,
    pub show_blocks: bool,
    pub show_grid: bool,
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
    #[cfg(target_arch = "wasm32")]
    file_picker_channel: Option<oneshot::Receiver<Vec<u8>>>,
}

impl<'a> Viewer<'a> {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let pdfium = Pdfium::new(
            Pdfium::bind_to_system_library().expect("failed to bind to system library"),
        );

        #[cfg(target_arch = "wasm32")]
        let pdfium = Pdfium::default();

        Self {
            pdfium,
            pdf_bytes: None,
            pdf_document: None,
            pdf_path: None,
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
            highlighted_match: None,
            match_filter_threshold: 0.8,
            #[cfg(target_arch = "wasm32")]
            file_picker_channel: None,
        }
    }

    fn load_pdf(&mut self, bytes: Vec<u8>) {
        self.pdf_bytes = Some(bytes);
        self.pdf_document = unsafe {
            self.pdfium
                .load_pdf_from_byte_slice(self.pdf_bytes.as_ref().unwrap(), None)
                .ok()
                // The lifetime of the `PdfDocument` is transmuted to the lifetime of the `Viewer`.
                // This is safe because the `pdf_bytes` are owned by the `Viewer`.
                .map(|doc| std::mem::transmute::<PdfDocument<'_>, PdfDocument<'a>>(doc))
        };
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.pdf_path = Some(path);
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn open_file_dialog(&mut self) {
        let (sender, receiver) = oneshot::channel();
        self.file_picker_channel = Some(receiver);

        let file_picker_task = async {
            let file_handle = rfd::AsyncFileDialog::new().pick_file().await;
            if let Some(file_handle) = file_handle {
                let bytes = file_handle.read().await;
                let _ = sender.send(bytes);
            }
        };
        wasm_bindgen_futures::spawn_local(file_picker_task);
    }

    fn poll_for_file(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(mut channel) = self.file_picker_channel.take() {
            if let Ok(Some(bytes)) = channel.try_recv() {
                self.load_pdf(bytes);
            } else {
                self.file_picker_channel = Some(channel);
            }
        }
    }
}

impl<'a> eframe::App for Viewer<'a> {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_for_file();

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

                if let Some(path) = self.pdf_path.as_ref() {
                    ui.label(format!("PDF: {}", path.display()));
                }

                if ui.button("Open PDF").clicked() {
                    self.open_file_dialog();
                }

                #[cfg(not(target_arch = "wasm32"))]
                if let Some(pdf_path) = &self.pdf_path {
                    if self.pdf_document.is_none() {
                        if let Ok(bytes) = std::fs::read(pdf_path) {
                            self.load_pdf(bytes);
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
