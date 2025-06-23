use crate::debug_viewer::{event_panel, match_panel, rendering, ui_controls, utils};
use crate::layout::{TextBlock, TextLine};
use crate::persistent_store::{PersistentDebugStore, EntityEvents};
use eframe::egui;
use lopdf::Document;
use pdfium_render::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use uuid::Uuid;
use anyhow::{Context as _, Result};

/// Cached data from async queries
#[derive(Default, Clone)]
struct CachedData {
    templates: Vec<(Uuid, String)>,
    template_matches: HashMap<Uuid, Vec<(Uuid, f32)>>,
    entity_events: HashMap<Uuid, EntityEvents>,
    events_by_target: HashMap<String, Vec<String>>,
    last_update: Option<Instant>,
}

/// Messages sent from UI to background task
#[derive(Debug)]
enum AsyncRequest {
    LoadTemplates,
    LoadTemplateMatches(Uuid),
    LoadEntityEvents(Uuid),
    LoadEventsByTarget(String),
    RefreshAll,
}

/// Messages sent from background task to UI
#[derive(Debug, Clone)]
enum AsyncResponse {
    TemplatesLoaded(Vec<(Uuid, String)>),
    TemplateMatchesLoaded(Uuid, Vec<(Uuid, f32)>),
    EntityEventsLoaded(Uuid, EntityEvents),
    EventsByTargetLoaded(String, Vec<String>),
    Error(String),
}

/// Async-compatible debug viewer
pub struct AsyncDebugViewer {
    // Document data
    pub blocks: Vec<TextBlock>,
    
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
    pub highlighted_match: Option<(Uuid, Uuid)>,
    pub match_filter_threshold: f32,

    // Async communication
    request_sender: Sender<AsyncRequest>,
    response_receiver: Receiver<AsyncResponse>,
    cached_data: Arc<Mutex<CachedData>>,
    refresh_timer: Instant,
}

impl AsyncDebugViewer {
    /// Create a new async debug viewer
    pub fn new(
        ctx: &eframe::egui::Context,
        mut doc: Document,
        blocks: &[TextBlock],
        debug_store: PersistentDebugStore,
    ) -> Result<Self> {
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
                .map_err(|e| anyhow::anyhow!("Failed to get page {}: {}", page_index, e))?;

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
                .map_err(|e| anyhow::anyhow!("Failed to render page {}: {}", page_index, e))?;

            let pixels = bitmap.as_rgba_bytes();

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

        // Setup async communication
        let (request_sender, request_receiver) = mpsc::channel();
        let (response_sender, response_receiver) = mpsc::channel();
        let cached_data = Arc::new(Mutex::new(CachedData::default()));
        let cached_data_clone = cached_data.clone();

        // Spawn background async task
        thread::spawn(move || {
            let rt = Runtime::new().expect("Failed to create async runtime");
            rt.block_on(async move {
                Self::async_task_loop(debug_store, request_receiver, response_sender, cached_data_clone).await;
            });
        });

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
            selected_bbox: None,
            selected_line: None,
            selected_fields: HashSet::new(),
            selected_events: HashSet::new(),
            show_tree_view: false,
            show_matches: true,
            show_match_panel: true,
            highlighted_match: None,
            match_filter_threshold: 0.5,
            request_sender,
            response_receiver,
            cached_data,
            refresh_timer: Instant::now(),
        })
    }

    /// Background async task loop
    async fn async_task_loop(
        store: PersistentDebugStore,
        request_receiver: Receiver<AsyncRequest>,
        response_sender: Sender<AsyncResponse>,
        cached_data: Arc<Mutex<CachedData>>,
    ) {
        loop {
            // Check for requests
            if let Ok(request) = request_receiver.try_recv() {
                let response = match request {
                    AsyncRequest::LoadTemplates => {
                        match store.get_templates().await {
                            Ok(templates) => {
                                // Update cache
                                if let Ok(mut cache) = cached_data.lock() {
                                    cache.templates = templates.clone();
                                    cache.last_update = Some(Instant::now());
                                }
                                AsyncResponse::TemplatesLoaded(templates)
                            }
                            Err(e) => AsyncResponse::Error(format!("Failed to load templates: {}", e)),
                        }
                    }
                    AsyncRequest::LoadTemplateMatches(template_id) => {
                        match store.get_template_matches(template_id).await {
                            Ok(matches) => {
                                // Update cache
                                if let Ok(mut cache) = cached_data.lock() {
                                    cache.template_matches.insert(template_id, matches.clone());
                                    cache.last_update = Some(Instant::now());
                                }
                                AsyncResponse::TemplateMatchesLoaded(template_id, matches)
                            }
                            Err(e) => AsyncResponse::Error(format!("Failed to load template matches: {}", e)),
                        }
                    }
                    AsyncRequest::LoadEntityEvents(entity_id) => {
                        let events = store.get_entity_events(entity_id).await;
                        // Update cache
                        if let Ok(mut cache) = cached_data.lock() {
                            cache.entity_events.insert(entity_id, events.clone());
                            cache.last_update = Some(Instant::now());
                        }
                        AsyncResponse::EntityEventsLoaded(entity_id, events)
                    }
                    AsyncRequest::LoadEventsByTarget(target) => {
                        let events = store.get_events_by_target(&target).await;
                        // Update cache
                        if let Ok(mut cache) = cached_data.lock() {
                            cache.events_by_target.insert(target.clone(), events.clone());
                            cache.last_update = Some(Instant::now());
                        }
                        AsyncResponse::EventsByTargetLoaded(target, events)
                    }
                    AsyncRequest::RefreshAll => {
                        // Load all data
                        if let Ok(templates) = store.get_templates().await {
                            let _ = response_sender.send(AsyncResponse::TemplatesLoaded(templates));
                        }
                        continue;
                    }
                };

                let _ = response_sender.send(response);
            }

            // Small delay to prevent busy waiting
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Process async responses
    fn process_async_responses(&mut self) {
        while let Ok(response) = self.response_receiver.try_recv() {
            match response {
                AsyncResponse::TemplatesLoaded(templates) => {
                    // Templates loaded - UI can now display them
                    tracing::debug!("Received {} templates from async task", templates.len());
                }
                AsyncResponse::TemplateMatchesLoaded(template_id, matches) => {
                    tracing::debug!("Received {} matches for template {}", matches.len(), template_id);
                }
                AsyncResponse::EntityEventsLoaded(entity_id, events) => {
                    tracing::debug!("Received {} events for entity {}", events.messages.len(), entity_id);
                }
                AsyncResponse::EventsByTargetLoaded(target, events) => {
                    tracing::debug!("Received {} events for target {}", events.len(), target);
                }
                AsyncResponse::Error(error) => {
                    tracing::error!("Async task error: {}", error);
                }
            }
        }
    }

    /// Request templates to be loaded
    pub fn request_templates(&self) {
        let _ = self.request_sender.send(AsyncRequest::LoadTemplates);
    }

    /// Request template matches to be loaded
    pub fn request_template_matches(&self, template_id: Uuid) {
        let _ = self.request_sender.send(AsyncRequest::LoadTemplateMatches(template_id));
    }

    /// Request entity events to be loaded
    pub fn request_entity_events(&self, entity_id: Uuid) {
        let _ = self.request_sender.send(AsyncRequest::LoadEntityEvents(entity_id));
    }

    /// Request events by target to be loaded
    pub fn request_events_by_target(&self, target: String) {
        let _ = self.request_sender.send(AsyncRequest::LoadEventsByTarget(target));
    }

    /// Get cached templates
    pub fn get_cached_templates(&self) -> Vec<(Uuid, String)> {
        if let Ok(cache) = self.cached_data.lock() {
            cache.templates.clone()
        } else {
            Vec::new()
        }
    }

    /// Get cached template matches
    pub fn get_cached_template_matches(&self, template_id: &Uuid) -> Vec<(Uuid, f32)> {
        if let Ok(cache) = self.cached_data.lock() {
            cache.template_matches.get(template_id).cloned().unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    /// Get cached entity events
    pub fn get_cached_entity_events(&self, entity_id: &Uuid) -> Option<EntityEvents> {
        if let Ok(cache) = self.cached_data.lock() {
            cache.entity_events.get(entity_id).cloned()
        } else {
            None
        }
    }

    /// Get cached events by target
    pub fn get_cached_events_by_target(&self, target: &str) -> Vec<String> {
        if let Ok(cache) = self.cached_data.lock() {
            cache.events_by_target.get(target).cloned().unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    /// Calculate the PDF view rectangle and transform
    pub fn calculate_pdf_view_rect(
        &self,
        ui: &egui::Ui,
        _texture: &egui::TextureHandle,
    ) -> (egui::Rect, utils::ViewTransform) {
        let available_size = ui.available_size();
        let (pdf_width, pdf_height) = self.pdf_dimensions[self.current_page];
        let aspect_ratio = pdf_width / pdf_height;

        let scaled_width = available_size.x.min(available_size.y * aspect_ratio);
        let scaled_height = scaled_width / aspect_ratio;

        let rect = egui::Rect::from_min_size(
            ui.available_rect_before_wrap().min,
            egui::vec2(scaled_width, scaled_height),
        );

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
        for block in &self.blocks {
            for line in &block.lines {
                if line.id == content_id {
                    self.current_page = (block.page_number - 1) as usize;
                    let center_x = (line.bbox.0 + line.bbox.2) / 2.0;
                    let center_y = (line.bbox.1 + line.bbox.3) / 2.0;
                    self.pan = egui::vec2(-center_x * self.zoom, -center_y * self.zoom);
                    return;
                }
            }
        }
    }

    /// Display template matches using cached data
    pub fn display_template_matches(&mut self, ui: &mut egui::Ui) {
        // Auto-refresh every 5 seconds
        if self.refresh_timer.elapsed() > Duration::from_secs(5) {
            self.request_templates();
            self.request_events_by_target("MATCHER_OPERATIONS".to_string());
            self.request_events_by_target("TEMPLATE_MATCH".to_string());
            self.refresh_timer = Instant::now();
        }

        let templates = self.get_cached_templates();
        let matcher_ops = self.get_cached_events_by_target("MATCHER_OPERATIONS");
        let template_matches = self.get_cached_events_by_target("TEMPLATE_MATCH");

        ui.label(format!("Templates found: {}", templates.len()));
        ui.label(format!("Matcher operations: {}", matcher_ops.len()));
        ui.label(format!("Template matches: {}", template_matches.len()));

        if templates.is_empty() && matcher_ops.is_empty() && template_matches.is_empty() {
            ui.label("No template data found. Possible reasons:");
            ui.label("• Jaeger is not running or not accessible");
            ui.label("• No traces have been sent yet");
            ui.label("• Service name mismatch");
            ui.label("• OpenTelemetry not properly configured");
        } else {
            ui.collapsing("Templates", |ui| {
                for (template_id, template_name) in &templates {
                    ui.horizontal(|ui| {
                        ui.label(format!("• {}", template_name));
                        if ui.button("Load Matches").clicked() {
                            self.request_template_matches(*template_id);
                        }
                    });

                    // Show cached matches if available
                    let matches = self.get_cached_template_matches(template_id);
                    if !matches.is_empty() {
                        ui.label(format!("  Matches: {}", matches.len()));
                    }
                }
            });

            if !matcher_ops.is_empty() {
                ui.collapsing("Recent Matcher Operations", |ui| {
                    for op in matcher_ops.iter().take(10) {
                        ui.label(format!("• {}", op));
                    }
                });
            }
        }
    }
}

impl eframe::App for AsyncDebugViewer {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        // Process any async responses
        self.process_async_responses();

        // Show match panel if enabled
        if self.show_match_panel {
            // Use a modified match panel that works with async data
            self.show_async_match_panel(ctx);
        }

        // Main central panel with PDF view
        egui::CentralPanel::default().show(ctx, |ui| {
            // Top controls
            self.show_async_controls(ui);

            // Render the PDF with all visualizations
            rendering::render_pdf_view_for_async(self, ui);

            // Show event panel for selected elements
            if let Some(line_id) = self.selected_line {
                self.show_async_event_panel(ctx, line_id);
            }

            // Display template matches
            self.display_template_matches(ui);
        });

        // Request repaint to keep UI responsive
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

impl AsyncDebugViewer {
    /// Show async-compatible match panel
    fn show_async_match_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("async_template_matches_panel")
            .resizable(true)
            .default_width(300.0)
            .show(ctx, |ui| {
                ui.heading("Template Matches (Async)");
                self.display_template_matches(ui);
            });
    }

    /// Show async-compatible controls
    fn show_async_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("⟲ Refresh Data").clicked() {
                let _ = self.request_sender.send(AsyncRequest::RefreshAll);
            }

            ui.separator();
            ui.checkbox(&mut self.show_text, "Text");
            ui.checkbox(&mut self.show_lines, "Lines");
            ui.checkbox(&mut self.show_blocks, "Blocks");
            ui.checkbox(&mut self.show_grid, "Grid");

            ui.separator();
            if ui.button("◀").clicked() && self.current_page > 0 {
                self.current_page -= 1;
            }

            ui.label(format!("Page {}/{}", self.current_page + 1, self.textures.len()));

            if ui.button("▶").clicked() && self.current_page < self.textures.len() - 1 {
                self.current_page += 1;
            }

            ui.separator();
            ui.add(egui::Slider::new(&mut self.zoom, 0.1..=3.0).text("Zoom"));
        });
    }

    /// Show async-compatible event panel
    fn show_async_event_panel(&mut self, ctx: &egui::Context, line_id: Uuid) {
        // Request events if not cached
        if self.get_cached_entity_events(&line_id).is_none() {
            self.request_entity_events(line_id);
        }

        egui::Window::new("Entity Events (Async)")
            .resizable(true)
            .default_width(400.0)
            .show(ctx, |ui| {
                ui.label(format!("Entity: {}", line_id));

                if let Some(events) = self.get_cached_entity_events(&line_id) {
                    ui.label(format!("Messages: {}", events.messages.len()));
                    ui.label(format!("Children: {}", events.children.len()));

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for (i, message) in events.messages.iter().enumerate() {
                            ui.label(format!("{}: {}", i, message));
                        }
                    });
                } else {
                    ui.label("Loading events...");
                }
            });
    }
}

/// Launch the async debug viewer
pub fn launch_async_viewer(
    doc: &Document,
    blocks: &[TextBlock],
    debug_store: PersistentDebugStore,
) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 1000.0])
            .with_min_inner_size([800.0, 1000.0]),
        ..Default::default()
    };

    eframe::run_native(
        "PDF Debug Viewer (Async)",
        options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);

            let viewer = AsyncDebugViewer::new(&cc.egui_ctx, doc.clone(), blocks, debug_store)
                .context("Failed to create AsyncDebugViewer")
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            Ok(Box::new(viewer) as Box<dyn eframe::App>)
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe::run_native failed: {:?}", e))?;

    Ok(())
}