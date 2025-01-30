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
        transform_matrix: [f32; 6],
        scale_x: f32,
        scale_y: f32,
        x_offset: f32,
        y_offset: f32,
        show_text: bool,
        show_lines: bool,
        show_blocks: bool,
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
            let mut m_cumulative_matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
            let mut matrix_stack: Vec<[f32; 6]> = Vec::new();

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

            let mut initial_ctm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

            for (page_num, page_id) in doc.get_pages().iter().take(1) {
                let page_dict = doc.get_object(*page_id)?.as_dict()?;
                initial_ctm = get_page_matrix(page_dict)?;
            }

            println!("initial_ctm: {:?}", initial_ctm);

            Ok(Self {
                blocks: blocks.to_vec(),
                current_page: 0,
                textures,
                pdf_dimensions: page_dimensions,
                transform_matrix: initial_ctm, // Store the initial CTM
                scale_x: 1.0,
                scale_y: 1.0,
                x_offset: 0.0,
                y_offset: 0.0,
                show_text: false,
                show_lines: true,
                show_blocks: true,
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
                });

                egui::ScrollArea::both().show(ui, |ui| {
                    if let Some(texture) = self.textures.get(self.current_page) {
                        let (pdf_width, pdf_height) = self.pdf_dimensions[self.current_page];
                        println!("pdf_width: {}, pdf_height: {}", pdf_width, pdf_height);
                        let size = egui::vec2(pdf_width, pdf_height);

                        let image = egui::Image::new(texture)
                            .fit_to_exact_size(size)
                            .sense(egui::Sense::click_and_drag());
                        // .maintain_aspect_ratio(true);

                        let response = ui.add(image);

                        // Draw bounding boxes with different scale factors for debugging
                        let painter = ui.painter();

                        for block in self.blocks.iter() {
                            if block.page_number as usize == self.current_page + 1 {
                                for line in block.lines.iter() {
                                    let displayed_rect = response.rect;

                                    let rect = egui::Rect::from_points(&[
                                        egui::pos2(line.bbox.0, line.bbox.1),
                                        egui::pos2(line.bbox.2, line.bbox.3),
                                    ]);
                                    painter.rect_stroke(
                                        rect,
                                        0.0,
                                        egui::Stroke::new(1.0, egui::Color32::RED),
                                    );

                                    if self.show_lines {
                                        painter.rect_stroke(
                                            rect,
                                            0.0,
                                            egui::Stroke::new(1.0, egui::Color32::RED),
                                        );
                                    }

                                    let x1 = egui::pos2(block.bbox.0, block.bbox.1);
                                    let x2 = egui::pos2(block.bbox.2, block.bbox.3);
                                    let rect = egui::Rect::from_points(&[x1, x2]);

                                    if self.show_blocks {
                                        painter.rect_stroke(
                                            rect,
                                            0.0,
                                            egui::Stroke::new(1.0, egui::Color32::BLUE),
                                        );
                                    }

                                    if self.show_text {
                                        // Draw the text for debugging
                                        painter.text(
                                            rect.min,
                                            egui::Align2::LEFT_TOP,
                                            &format!("{} ({:.1}, {:.1})", &line.text, x1, x2),
                                            egui::FontId::monospace(8.0),
                                            egui::Color32::RED,
                                        );
                                    }
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

    fn get_page_matrix(page_dict: &lopdf::Dictionary) -> Result<[f32; 6], Box<dyn Error>> {
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
        let mb0 = media_box.0;
        let mb1 = media_box.1;
        let mb2 = media_box.2;
        let mb3 = media_box.3;

        let crop_box = match page_dict.get(b"CropBox") {
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
        let cb0 = crop_box.0;
        let cb1 = crop_box.1;
        let cb2 = crop_box.2;
        let cb3 = crop_box.3;

        let crop_rect = [cb0, cb1, cb2, cb3];

        // Calculate transformation matrix that maps crop box to media box
        let scale_x = (mb2 - mb0) / (cb2 - cb0);
        let scale_y = (mb3 - mb1) / (cb3 - cb1);
        let tx = mb0 - cb0 * scale_x;
        let ty = mb1 - cb1 * scale_y;

        Ok([scale_x, 0.0, 0.0, scale_y, tx, ty])
    }

    fn pdf_to_gui(
        (pdf_x, pdf_y): (f32, f32),
        pdf_width: f32,
        pdf_height: f32,
        scale_x: f32,
        scale_y: f32,
        offset_x: f32,
        offset_y: f32,
        initial_ctm: [f32; 6], // Add initial CTM parameter
    ) -> (f32, f32) {
        // Apply initial transformation matrix
        let (x, y) = apply_transform_matrix(&initial_ctm, &[pdf_x, pdf_y]);

        // Convert to GUI coordinates
        let gui_x = offset_x + x * scale_x;
        let gui_y = offset_y + (pdf_height - y) * scale_y;
        (gui_x, gui_y)
    }

    pub fn apply_transform_matrix(matrix: &[f32; 6], point: &[f32; 2]) -> (f32, f32) {
        // Apply the transformation matrix to the point
        let x = matrix[0] * point[0] + matrix[1] * point[1] + matrix[4];
        let y = matrix[2] * point[0] + matrix[3] * point[1] + matrix[5];
        (x, y)
    }
}

#[cfg(feature = "debug-viewer")]
pub use viewer::*;
