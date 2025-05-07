use crate::debug_viewer::{event_panel, match_panel, rendering, ui_controls, utils};
use crate::layout::{TextBlock, TextLine};
use crate::logging::{DebugDataStore, EntityEvents};
use eframe::egui;
use egui::cache::{ComputerMut, FrameCache};
use lopdf::Document;
use pdfium_render::prelude::*;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::Arc;
use uuid::Uuid;

/// Main debug viewer application
pub struct DebugViewer {
    // Document data
    pub blocks: Vec<TextBlock>,
    pub debug_data: DebugDataStore,

    // PDF rendering state
    pub current_page: usize,
    pub textures: Vec<egui::TextureHandle>,
    pub pdf_dimensions: Vec<(f32, f32)>,

    // View settings
    pub show_text: bool,
    pub show_lines: bool,
    pub show_blocks: bool,
    pub show_grid: bool,
    pub grid_spacing: f32,
    pub zoom: f32,
    pub pan: egui::Vec2,

    // Selection and highlighting
    pub selected_bbox: Option<(f32, f32, f32, f32)>,
    pub selected_line: Option<Uuid>,
    pub selected_fields: HashSet<String>,
    pub selected_events: HashSet<String>,

    // Panel visibility
    pub show_tree_view: bool,
    pub show_matches: bool,
    pub show_match_panel: bool,

    // Template match settings
    pub highlighted_match: Option<(Uuid, Uuid)>, // (template_id, content_id)
    pub match_filter_threshold: f32,
}

impl DebugViewer {
    /// Create a new debug viewer
    pub fn new(
        ctx: &eframe::egui::Context,
        mut doc: Document,
        blocks: &[TextBlock],
        debug_store: DebugDataStore,
    ) -> Result<Self, Box<dyn Error>> {
        // Create PDF renderer
        let pdfium = Pdfium::new(
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./"))
                .or_else(|_| Pdfium::bind_to_system_library())?,
        );

        // Load PDF from memory
        let mut pdf_bytes = Vec::new();
        doc.save_to(&mut pdf_bytes)?;
        let document = pdfium.load_pdf_from_byte_slice(&pdf_bytes, None)?;

        // Initialize textures for each page
        let mut textures = Vec::new();
        let mut page_dimensions = Vec::new();

        for page_index in 0..document.pages().len() {
            let page: PdfPage = document
                .pages()
                .get(page_index)
                .map_err(|e| Box::new(e) as Box<dyn Error>)?;

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
                .map_err(|e| Box::new(e) as Box<dyn Error>)?;

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

        Ok(Self {
            blocks: blocks.to_vec(),
            current_page: 0,
            textures,
            pdf_dimensions: page_dimensions,
            show_text: true,
            show_lines: true,
            show_blocks: true,
            show_grid: false,
            grid_spacing: 10.0,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            debug_data: debug_store,
            selected_bbox: None,
            selected_line: None,
            selected_fields: HashSet::new(),
            selected_events: HashSet::new(),
            show_tree_view: false,
            show_matches: true,
            show_match_panel: true,
            highlighted_match: None,
            match_filter_threshold: 0.5,
        })
    }

    /// Calculate the PDF view rectangle and transform based on current state
    pub fn calculate_pdf_view_rect(
        &self,
        ui: &egui::Ui,
        texture: &egui::TextureHandle,
    ) -> (egui::Rect, utils::ViewTransform) {
        // Calculate view area and transformation
        let available_size = ui.available_size();

        // Calculate the scaled size
        let (pdf_width, pdf_height) = self.pdf_dimensions[self.current_page];
        let aspect_ratio = pdf_width / pdf_height;

        let scaled_width = available_size.x.min(available_size.y * aspect_ratio);
        let scaled_height = scaled_width / aspect_ratio;

        let rect = egui::Rect::from_min_size(
            ui.available_rect_before_wrap().min,
            egui::vec2(scaled_width, scaled_height),
        );

        // Calculate transformation parameters
        let scale = self.zoom * scaled_width / pdf_width;
        let x_offset = rect.min.x + self.pan.x;
        let y_offset = rect.min.y + self.pan.y;

        (
            rect,
            utils::ViewTransform {
                scale,
                x_offset,
                y_offset,
            },
        )
    }

    /// Get content text for a specific line ID
    pub fn get_content_text(&self, line_id: Uuid) -> Option<String> {
        for block in &self.blocks {
            for line in &block.lines {
                if line.id == line_id {
                    return Some(line.text.clone());
                }
            }
        }
        None
    }

    /// Scroll to show a specific content element
    pub fn scroll_to_content(&mut self, content_id: Uuid) {
        // Find the page number and coordinates for this content
        for block in &self.blocks {
            for line in &block.lines {
                if line.id == content_id {
                    // Set current page
                    self.current_page = (block.page_number - 1) as usize;

                    // Calculate center position for panning
                    let center_x = (line.bbox.0 + line.bbox.2) / 2.0;
                    let center_y = (line.bbox.1 + line.bbox.3) / 2.0;

                    // Set pan to center on this element (with coordinate system adjustment)
                    self.pan = egui::vec2(-center_x * self.zoom, -center_y * self.zoom);

                    return;
                }
            }
        }
    }

    fn display_template_matches(&self, ui: &mut egui::Ui) {
        let total_matches;
        let templates_copy;

        // Scope locks to minimize lock duration
        {
            let template_matches = self.debug_data.template_matches.lock().unwrap();
            total_matches = template_matches.len();

            // Create a snapshot of template_matches for debugging
            let matching_data: Vec<_> = template_matches
                .iter()
                .take(10) // Limit to avoid overloading the UI
                .map(|(cid, (tid, score))| (cid.to_string(), tid.to_string(), *score))
                .collect();

            if !matching_data.is_empty() {
                ui.label("Sample matches (limited to 10):");
                for (content_id, template_id, score) in matching_data {
                    ui.label(format!(
                        "• Template {} matched {} with score {:.2}",
                        template_id, content_id, score
                    ));
                }
            }
        }

        // Get templates in a separate lock scope
        {
            let templates = self.debug_data.get_templates();
            templates_copy = templates.into_iter().take(20).collect::<Vec<_>>();
            // Limit to 20 templates
        }

        ui.label(format!("Total template matches found: {}", total_matches));
        ui.label(format!("Templates available: {}", templates_copy.len()));

        // Show each template with minimal locking
        for (template_id, template_name) in templates_copy {
            // Count matches for this template - careful with locking here
            let match_count = {
                let matches = self.debug_data.template_matches.lock().unwrap();
                matches
                    .iter()
                    .filter(|(_, (tid, _))| *tid == template_id)
                    .take(100) // Limit counting to avoid performance issues
                    .count()
            };

            ui.horizontal(|ui| {
                ui.label(format!(
                    "• {} ({})",
                    if template_name.len() > 30 {
                        &template_name[..30]
                    } else {
                        &template_name
                    },
                    template_id
                ));
            });
            ui.label(format!("Matches: {}", match_count));
        }
    }
}

impl eframe::App for DebugViewer {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        // Show match panel if enabled
        if self.show_match_panel {
            match_panel::show_match_panel(self, ctx);
        }

        // Main central panel with PDF view
        egui::CentralPanel::default().show(ctx, |ui| {
            // Top controls
            ui_controls::show_controls(self, ui);

            // Render the PDF with all visualizations
            rendering::render_pdf_view(self, ui);

            // Show event panel for selected elements
            if let Some(line_id) = self.selected_line {
                event_panel::show_event_panel(self, ctx, line_id);
            }

            // Display template matches
            self.display_template_matches(ui);
        });
    }
}

/// Launch the debug viewer with the given document, blocks, and debug data
pub fn launch_viewer(
    doc: &Document,
    blocks: &[TextBlock],
    debug_store: DebugDataStore,
) -> Result<(), Box<dyn Error>> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 1000.0])
            .with_min_inner_size([800.0, 1000.0]),
        ..Default::default()
    };

    eframe::run_native(
        "PDF Debug Viewer",
        options,
        Box::new(|cc| {
            // Install image loaders
            egui_extras::install_image_loaders(&cc.egui_ctx);

            let viewer = DebugViewer::new(&cc.egui_ctx, doc.clone(), blocks, debug_store).unwrap();
            Ok(Box::new(viewer) as Box<dyn eframe::App>)
        }),
    )?;

    Ok(())
}
