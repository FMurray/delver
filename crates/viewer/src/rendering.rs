use crate::app::Viewer;
use crate::utils;
use eframe::{egui, epaint};
use pdfium_render::prelude::*;

/// Represents a transformation from PDF coordinates to screen coordinates
pub struct ViewTransform {
    pub scale: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

// Draw blocks
fn draw_blocks(viewer: &Viewer, painter: &egui::Painter, transform: &utils::ViewTransform) {
    for block in &viewer.blocks {
        if block.page_number as usize == viewer.current_page + 1 {
            // Find min/max coordinates for the whole block
            let mut x_min = f32::MAX;
            let mut y_min = f32::MAX;
            let mut x_max = f32::MIN;
            let mut y_max = f32::MIN;

            for line in &block.lines {
                x_min = x_min.min(line.bbox.0);
                y_min = y_min.min(line.bbox.1);
                x_max = x_max.max(line.bbox.2);
                y_max = y_max.max(line.bbox.3);
            }

            // Convert to screen coordinates
            let x_min = transform.x_offset + x_min * transform.scale;
            let y_min = transform.y_offset + y_min * transform.scale;
            let x_max = transform.x_offset + x_max * transform.scale;
            let y_max = transform.y_offset + y_max * transform.scale;

            // Draw rectangle
            painter.rect_stroke(
                egui::Rect::from_min_max(egui::pos2(x_min, y_min), egui::pos2(x_max, y_max)),
                2.0,
                egui::Stroke::new(1.0, egui::Color32::BLUE),
                egui::StrokeKind::Inside,
            );
        }
    }
}

// Draw lines
fn draw_lines(viewer: &Viewer, painter: &egui::Painter, transform: &utils::ViewTransform) {
    for block in &viewer.blocks {
        if block.page_number as usize == viewer.current_page + 1 {
            for line in &block.lines {
                // Convert to screen coordinates
                let x_min = transform.x_offset + line.bbox.0 * transform.scale;
                let y_min = transform.y_offset + line.bbox.1 * transform.scale;
                let x_max = transform.x_offset + line.bbox.2 * transform.scale;
                let y_max = transform.y_offset + line.bbox.3 * transform.scale;

                // Check if this is the selected line
                let is_selected = viewer.selected_line == Some(line.id);

                // Draw rectangle
                painter.rect_stroke(
                    egui::Rect::from_min_max(egui::pos2(x_min, y_min), egui::pos2(x_max, y_max)),
                    0.0,
                    egui::Stroke::new(
                        if is_selected { 2.0 } else { 1.0 },
                        if is_selected {
                            egui::Color32::RED
                        } else {
                            egui::Color32::YELLOW
                        },
                    ),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }
}

pub fn calculate_pdf_view_rect(
    viewer: &Viewer,
    ui: &egui::Ui,
    texture: &egui::TextureHandle,
) -> (egui::Rect, utils::ViewTransform) {
    // Calculate view area and transformation
    let available_size = ui.available_size();

    // Calculate the scaled size
    let (pdf_width, pdf_height) = viewer.pdf_dimensions[viewer.current_page];
    let aspect_ratio = pdf_width / pdf_height;

    let scaled_width = available_size.x.min(available_size.y * aspect_ratio);
    let scaled_height = scaled_width / aspect_ratio;

    let rect = egui::Rect::from_min_size(
        ui.available_rect_before_wrap().min,
        egui::vec2(scaled_width, scaled_height),
    );

    // Calculate transformation parameters
    let scale = viewer.zoom * scaled_width / pdf_width;
    let x_offset = rect.min.x + viewer.pan.x;
    let y_offset = rect.min.y + viewer.pan.y;

    (
        rect,
        utils::ViewTransform {
            scale,
            x_offset,
            y_offset,
        },
    )
}

/// Render the PDF view with all visualizations
pub fn render_pdf_view(viewer: &mut Viewer, ui: &mut egui::Ui) {
    if let Some(doc) = &viewer.pdf_document {
        if viewer.pdf_dimensions.len() != doc.pages().len() as usize {
            viewer.pdf_dimensions = doc
                .pages()
                .iter()
                .map(|page| (page.width().value, page.height().value))
                .collect();
        }

        if let Some(page) = doc.pages().get(viewer.current_page as u16).ok() {
            let texture =
                render_page_to_texture(&page, 1.0, ui.ctx(), egui::TextureOptions::LINEAR);
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&"rendered page".into());
            let (rect, transform) = calculate_pdf_view_rect(viewer, ui, &texture);
            utils::draw_pdf_page(ui, &texture, rect);

            let painter = ui.painter_at(rect);

            // Draw blocks if enabled
            if viewer.show_blocks {
                draw_blocks(viewer, &painter, &transform);
            }

            // Draw lines if enabled
            if viewer.show_lines {
                draw_lines(viewer, &painter, &transform);
            }
        }
    }
}

fn render_page_to_texture(
    page: &PdfPage,
    pixels_per_point: f32,
    ctx: &egui::Context,
    texture_options: egui::TextureOptions,
) -> egui::TextureHandle {
    let width = (page.width().value * pixels_per_point) as i32;
    let height = (page.height().value * pixels_per_point) as i32;
    let image_buffer = page
        .render_with_config(&PdfRenderConfig::new().set_target_size(width, height))
        .unwrap()
        .as_image()
        .to_rgba8();

    let color_image = egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &image_buffer.into_raw(),
    );

    ctx.load_texture(
        "pdf_page",
        egui::ImageData::from(color_image),
        texture_options,
    )
}
