#[cfg(feature = "debug-viewer")]
mod viewer {
    use super::*;
    use crate::{
        logging::DebugDataStore,
        parse::{multiply_matrices, TextBlock, TextLine},
    };
    use eframe::egui;
    use eframe::egui::{CollapsingHeader, ScrollArea};
    use lopdf::{Document, Object};
    use pdfium_render::prelude::*;
    use std::collections::HashSet;
    use std::error::Error;
    use uuid::Uuid;

    pub struct DebugViewer {
        blocks: Vec<TextBlock>,
        current_page: usize,
        textures: Vec<egui::TextureHandle>,
        pdf_dimensions: Vec<(f32, f32)>,
        show_text: bool,
        show_lines: bool,
        show_blocks: bool,
        show_grid: bool,
        grid_spacing: f32,
        zoom: f32,
        pan: egui::Vec2,
        debug_data: crate::logging::DebugDataStore,
        selected_bbox: Option<(f32, f32, f32, f32)>,
        selected_line: Option<Uuid>,
        selected_fields: HashSet<String>,
        show_tree_view: bool,
    }

    impl DebugViewer {
        pub fn new(
            ctx: &eframe::egui::Context,
            doc: &Document,
            blocks: &[TextBlock],
            debug_store: crate::logging::DebugDataStore,
        ) -> Result<Self, Box<dyn Error>> {
            // Get all pages' MediaBoxes
            let pages = doc.get_pages();
            let mut page_dimensions = Vec::new();

            for (page_num, page_id) in pages.iter().take(1) {
                let page_dict = doc.get_object(*page_id)?.as_dict()?;

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

                // Get actual MediaBox dimensions including origin
                let x0 = media_box.0;
                let y0 = media_box.1;
                let x1 = media_box.2;
                let y1 = media_box.3;

                // Store actual width and height
                let width = x1 - x0;
                let height = y1 - y0;
                page_dimensions.push((width, height));
            }

            // Initialize pdfium and convert pages
            let pdfium = Pdfium::default();
            let mut bytes = Vec::new();
            let mut doc = doc.clone();
            doc.save_to(&mut bytes)?;
            let pdf_document = pdfium.load_pdf_from_byte_vec(bytes, None)?;

            // Convert each page to a texture
            let mut textures = Vec::new();
            for (i, page) in pdf_document.pages().iter().take(1).enumerate() {
                let (width, height) = page_dimensions[i];

                // Use PDF units directly for rendering
                let render_config = PdfRenderConfig::new()
                    .set_target_width(width as i32)
                    .set_target_height(height as i32);
                // .set_render_annotations(true)
                // .set_render_form_data(true);

                let bitmap = page.render_with_config(&render_config)?;
                let image = bitmap.as_image();

                println!(
                    "Page {}: PDF units {} x {}, Rendered pixels {} x {}",
                    i + 1,
                    width,
                    height,
                    image.width(),
                    image.height()
                );

                let size = [image.width() as _, image.height() as _];
                let pixels = image.to_rgba8();
                let image = egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_raw());
                let texture = ctx.load_texture(
                    &format!("page_{}", textures.len()),
                    image,
                    egui::TextureOptions::LINEAR,
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
                selected_fields: HashSet::from_iter(vec![
                    "message".into(),
                    "element_id".into(),
                    "line_id".into(),
                    "element".into(),
                ]),
                show_tree_view: false,
            })
        }
    }

    impl eframe::App for DebugViewer {
        fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Previous").clicked() && self.current_page > 0 {
                        self.current_page -= 1;
                    }
                    ui.label(format!(
                        "Page {} of {}",
                        self.current_page + 1,
                        self.textures.len()
                    ));
                    if ui.button("Next").clicked() && self.current_page < self.textures.len() - 1 {
                        self.current_page += 1;
                    }
                    ui.add(egui::Checkbox::new(&mut self.show_text, "Show Text"));
                    ui.add(egui::Checkbox::new(&mut self.show_lines, "Show Lines"));
                    ui.add(egui::Checkbox::new(&mut self.show_blocks, "Show Blocks"));
                    ui.add(egui::Checkbox::new(&mut self.show_grid, "Show Grid"));
                    if ui.button("Reset View").clicked() {
                        self.zoom = 1.0;
                        self.pan = egui::Vec2::ZERO;
                    }
                    ui.label(format!("Zoom: {:.2}x", self.zoom));
                });

                // Add field selection toolbar
                ui.horizontal(|ui| {
                    ui.label("Show fields:");
                    let fields = ["message", "element_id", "line_id", "element"];
                    for field in fields {
                        let mut checked = self.selected_fields.contains(field);
                        if ui.checkbox(&mut checked, field).changed() {
                            if checked {
                                self.selected_fields.insert(field.into());
                            } else {
                                self.selected_fields.remove(field);
                            }
                        }
                    }
                });

                // PDF Page Scroll Area with persistent ID
                egui::ScrollArea::both()
                    .id_salt("pdf_page_scroll_area")
                    .show(ui, |ui| {
                        if let Some(texture) = self.textures.get(self.current_page) {
                            let (pdf_width, pdf_height) = self.pdf_dimensions[self.current_page];
                            let size = egui::vec2(pdf_width, pdf_height) * self.zoom;

                            // Handle zoom and pan
                            if ui.rect_contains_pointer(ui.max_rect()) {
                                ui.input(|i| {
                                    // Handle zoom
                                    let zoom_factor = i.zoom_delta();
                                    if zoom_factor != 1.0 {
                                        // Get mouse position relative to the image for zoom centering
                                        if let Some(pointer_pos) = i.pointer.hover_pos() {
                                            let old_zoom = self.zoom;
                                            self.zoom =
                                                (self.zoom * zoom_factor).max(0.1).min(10.0);

                                            // Adjust pan to keep the point under cursor fixed
                                            if self.zoom != old_zoom {
                                                let zoom_factor = self.zoom / old_zoom;
                                                let pointer_delta = pointer_pos - self.pan;
                                                self.pan =
                                                    pointer_pos - pointer_delta * zoom_factor;
                                            }
                                        }
                                    }

                                    // Handle smooth scrolling for pan
                                    let scroll_delta = i.smooth_scroll_delta;
                                    if scroll_delta != egui::Vec2::ZERO {
                                        self.pan += scroll_delta;
                                    }
                                });
                            }

                            // Handle panning with mouse drag
                            let response = ui.allocate_response(size, egui::Sense::drag());
                            if response.dragged() {
                                self.pan += response.drag_delta();
                            }

                            // Apply pan and zoom to the image position
                            let image_rect =
                                egui::Rect::from_min_size(response.rect.min + self.pan, size);

                            // Draw the image
                            let im_response = ui.painter().image(
                                texture.id(),
                                image_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );

                            let y_offset = image_rect.min.y;
                            let x_offset = image_rect.min.x;

                            // Draw grid if enabled
                            if self.show_grid {
                                let spacing = self.grid_spacing * self.zoom;

                                // Draw vertical lines
                                for x in
                                    (0..(pdf_width * self.zoom) as i32).step_by(spacing as usize)
                                {
                                    let x = x as f32;
                                    ui.painter().line_segment(
                                        [
                                            egui::pos2(x_offset + x, y_offset),
                                            egui::pos2(
                                                x_offset + x,
                                                y_offset + pdf_height * self.zoom,
                                            ),
                                        ],
                                        egui::Stroke::new(0.5, egui::Color32::GRAY),
                                    );
                                }

                                // Draw horizontal lines
                                for y in
                                    (0..(pdf_height * self.zoom) as i32).step_by(spacing as usize)
                                {
                                    let y = y as f32;
                                    ui.painter().line_segment(
                                        [
                                            egui::pos2(x_offset, y_offset + y),
                                            egui::pos2(
                                                x_offset + pdf_width * self.zoom,
                                                y_offset + y,
                                            ),
                                        ],
                                        egui::Stroke::new(0.5, egui::Color32::GRAY),
                                    );
                                }
                            }

                            // Draw bounding boxes
                            let painter = ui.painter();
                            for block in self.blocks.iter() {
                                if block.page_number as usize == self.current_page + 1 {
                                    for line in block.lines.iter() {
                                        if self.show_lines {
                                            let rect = egui::Rect {
                                                min: egui::pos2(
                                                    x_offset + line.bbox.0 * self.zoom,
                                                    y_offset + line.bbox.1 * self.zoom,
                                                ),
                                                max: egui::pos2(
                                                    x_offset + line.bbox.2 * self.zoom,
                                                    y_offset + line.bbox.3 * self.zoom,
                                                ),
                                            };

                                            let line_id = ui.make_persistent_id((
                                                line.page_number,
                                                line.bbox.0.to_bits(),
                                                line.bbox.1.to_bits(),
                                                line.bbox.2.to_bits(),
                                                line.bbox.3.to_bits(),
                                            ));

                                            let response =
                                                ui.interact(rect, line_id, egui::Sense::click());

                                            if response.clicked() {
                                                self.selected_line = Some(line.id);
                                            }

                                            painter.rect_stroke(
                                                rect,
                                                0.0,
                                                egui::Stroke::new(1.0, egui::Color32::RED),
                                            );

                                            if self.show_text {
                                                painter.text(
                                                    rect.min,
                                                    egui::Align2::LEFT_TOP,
                                                    &line.text,
                                                    egui::FontId::monospace(8.0 * self.zoom),
                                                    egui::Color32::RED,
                                                );
                                            }
                                        }
                                    }

                                    if self.show_blocks {
                                        let block_rect = egui::Rect {
                                            min: egui::pos2(
                                                x_offset + block.bbox.0 * self.zoom,
                                                y_offset + block.bbox.1 * self.zoom,
                                            ),
                                            max: egui::pos2(
                                                x_offset + block.bbox.2 * self.zoom,
                                                y_offset + block.bbox.3 * self.zoom,
                                            ),
                                        };

                                        painter.rect_stroke(
                                            block_rect,
                                            0.0,
                                            egui::Stroke::new(1.0, egui::Color32::BLUE),
                                        );
                                    }
                                }
                            }

                            if let Some(line_id) = self.selected_line {
                                if let Some(line) = find_line_by_id(&self.blocks, line_id) {
                                    let events = self.debug_data.get_events_for_line(line.id);
                                    egui::Window::new("Line Construction Details").show(
                                        ctx,
                                        |ui| {
                                            ui.label(format!("Line BBox: {:?}", line.bbox));
                                            ui.separator();
                                            for (index, event) in events.iter().enumerate() {
                                                CollapsingHeader::new(format!(
                                                    "Event {}",
                                                    index + 1
                                                ))
                                                .default_open(false)
                                                .show(ui, |ui| {
                                                    // Parse event string to filter fields
                                                    let parts: Vec<&str> =
                                                        event.split("; ").collect();
                                                    for part in parts {
                                                        if let Some((field_name, value)) =
                                                            part.split_once(" = ")
                                                        {
                                                            if self
                                                                .selected_fields
                                                                .contains(field_name)
                                                            {
                                                                ui.label(field_name);
                                                                ui.label(value);
                                                            }
                                                        } else {
                                                            // Display parts without " = " as is (e.g., "Begin text object")
                                                            ui.label(part);
                                                        }
                                                        ui.end_row();
                                                    }
                                                });
                                            }
                                        },
                                    );
                                }
                            }
                        }
                    });

                if self.show_tree_view {
                    // Tree View Scroll Area with persistent ID
                    ScrollArea::vertical()
                        .id_salt("tree_view_scroll_area")
                        .show(ui, |ui| {
                            for block in &self.blocks {
                                CollapsingHeader::new(format!("Block {}", block.id))
                                    .default_open(false)
                                    .show(ui, |ui| {
                                        for line in &block.lines {
                                            CollapsingHeader::new(format!("Line {}", line.id))
                                                .default_open(false)
                                                .show(ui, |ui| {
                                                    ui.label("Test Content");
                                                });
                                        }
                                    });
                            }
                        });
                }
            });
        }
    }

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

                // let debug_store = crate::logging::DebugDataStore::default();
                let viewer = DebugViewer::new(&cc.egui_ctx, doc, blocks, debug_store).unwrap();
                Ok(Box::new(viewer) as Box<dyn eframe::App>)
            }),
        )?;

        Ok(())
    }
}

use uuid::Uuid;
#[cfg(feature = "debug-viewer")]
pub use viewer::*;

use crate::parse::{TextBlock, TextLine};

fn find_line_by_id(blocks: &[TextBlock], line_id: Uuid) -> Option<&TextLine> {
    blocks
        .iter()
        .flat_map(|b| &b.lines)
        .find(|l| l.id == line_id)
}
