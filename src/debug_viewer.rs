#[cfg(feature = "debug-viewer")]
mod viewer {
    use super::*;
    use crate::parse::{multiply_matrices, TextBlock};
    use eframe::egui;
    use lopdf::{Document, Object};
    use pdfium_render::prelude::*;
    use std::error::Error;

    pub struct DebugViewer {
        blocks: Vec<TextBlock>,
        current_page: usize,
        textures: Vec<egui::TextureHandle>,
        pdf_dimensions: Vec<(f32, f32)>,
        scale_x: f32,
        scale_y: f32,
        x_offset: f32,
        y_offset: f32,
        show_text: bool,
        show_lines: bool,
        show_blocks: bool,
        zoom: f32,
        pan: egui::Vec2,
    }

    impl DebugViewer {
        pub fn new(
            ctx: &eframe::egui::Context,
            doc: &Document,
            blocks: &[TextBlock],
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
                scale_x: 1.0,
                scale_y: 1.0,
                x_offset: 0.0,
                y_offset: 0.0,
                show_text: false,
                show_lines: true,
                show_blocks: true,
                zoom: 1.0,
                pan: egui::Vec2::ZERO,
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
                    ui.add(egui::Slider::new(&mut self.scale_x, 0.1..=1.5).text("Scale X"));
                    ui.add(egui::Slider::new(&mut self.scale_y, 0.1..=1.5).text("Scale Y"));
                    ui.add(egui::Slider::new(&mut self.x_offset, -100.0..=500.0).text("X Offset"));
                    ui.add(
                        egui::Slider::new(&mut self.y_offset, -2000.0..=2000.0).text("Y Offset"),
                    );
                    ui.add(egui::Checkbox::new(&mut self.show_text, "Show Text"));
                    ui.add(egui::Checkbox::new(&mut self.show_lines, "Show Lines"));
                    ui.add(egui::Checkbox::new(&mut self.show_blocks, "Show Blocks"));
                    if ui.button("Reset View").clicked() {
                        self.zoom = 1.0;
                        self.pan = egui::Vec2::ZERO;
                    }
                    ui.label(format!("Zoom: {:.2}x", self.zoom));
                });

                egui::ScrollArea::both().show(ui, |ui| {
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
                                        self.zoom = (self.zoom * zoom_factor).max(0.1).min(10.0);

                                        // Adjust pan to keep the point under cursor fixed
                                        if self.zoom != old_zoom {
                                            let zoom_factor = self.zoom / old_zoom;
                                            let pointer_delta = pointer_pos - self.pan;
                                            self.pan = pointer_pos - pointer_delta * zoom_factor;
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
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );

                        let y_offset = image_rect.min.y;
                        let x_offset = image_rect.min.x;

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
                    }
                });
            });
        }
    }

    pub fn launch_viewer(doc: &Document, blocks: &[TextBlock]) -> Result<(), Box<dyn Error>> {
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

                let viewer = DebugViewer::new(&cc.egui_ctx, doc, blocks).unwrap();
                Ok(Box::new(viewer) as Box<dyn eframe::App>)
            }),
        )?;

        Ok(())
    }
}

#[cfg(feature = "debug-viewer")]
pub use viewer::*;
