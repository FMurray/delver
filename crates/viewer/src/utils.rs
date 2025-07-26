use crate::app::Viewer;
use delver_core::layout::{TextBlock, TextLine};
use eframe::egui;
use std::future::Future;
use uuid::Uuid;

/// Represents a transformation from PDF coordinates to screen coordinates
pub struct ViewTransform {
    pub scale: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

/// Extracts a template name from a log message
pub fn extract_template_name(message: &str) -> Option<String> {
    // Look for template_name = "something" in the message
    if let Some(start) = message.find("template_name = ") {
        let start = start + "template_name = ".len();
        if message[start..].starts_with('"') {
            let content_start = start + 1;
            if let Some(end) = message[content_start..].find('"') {
                return Some(message[content_start..(content_start + end)].to_string());
            }
        }
    }
    None
}

/// Converts a match score (0.0-1.0) to a color (red to green)
pub fn match_score_to_color(score: f32) -> egui::Color32 {
    // Green for high scores, red for low scores
    let r = (255.0 * (1.0 - score)).min(255.0).max(0.0) as u8;
    let g = (255.0 * score).min(255.0).max(0.0) as u8;
    let b = 0;
    egui::Color32::from_rgb(r, g, b)
}

/// Finds a text line by its UUID
pub fn find_line_by_id(blocks: &[TextBlock], id: Uuid) -> Option<&TextLine> {
    for block in blocks {
        for line in &block.lines {
            if line.id == id {
                return Some(line);
            }
        }
    }
    None
}

/// Renders detailed event information in a grid
pub fn render_event_details(
    ui: &mut egui::Ui,
    message: &str,
    selected_fields: &std::collections::HashSet<String>,
) {
    for part in message.split(';') {
        if let Some((field, value)) = part.split_once(" = ") {
            if selected_fields.is_empty() || selected_fields.contains(field.trim()) {
                ui.label(field.trim());
                ui.label(value.trim());
                ui.end_row();
            }
        }
    }
}

pub fn calculate_pdf_view_rect(
    viewer: &Viewer,
    ui: &egui::Ui,
    texture: &egui::TextureHandle,
) -> (egui::Rect, ViewTransform) {
    let available_size = ui.available_size();
    let (pdf_width, pdf_height) = viewer.pdf_dimensions[viewer.current_page];
    let aspect_ratio = pdf_width / pdf_height;
    let scaled_width = available_size.x.min(available_size.y * aspect_ratio);
    let scaled_height = scaled_width / aspect_ratio;
    let rect = egui::Rect::from_min_size(
        ui.available_rect_before_wrap().min,
        egui::vec2(scaled_width, scaled_height),
    );
    let scale = viewer.zoom * scaled_width / pdf_width;
    let x_offset = rect.min.x + viewer.pan.x;
    let y_offset = rect.min.y + viewer.pan.y;
    (
        rect,
        ViewTransform {
            scale,
            x_offset,
            y_offset,
        },
    )
}

pub fn draw_pdf_page(ui: &mut egui::Ui, texture: &egui::TextureHandle, rect: egui::Rect) {
    let image = egui::Image::from_texture(texture);
    ui.put(rect, image);
}

pub fn draw_grid(ui: &mut egui::Ui, rect: egui::Rect, grid_spacing: f32) {
    let painter = ui.painter_at(rect);
    let stroke = egui::Stroke::new(
        0.5,
        egui::Color32::from_rgba_premultiplied(100, 100, 100, 50),
    );
    let mut x = rect.min.x;
    while x < rect.max.x {
        painter.line_segment(
            [egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)],
            stroke,
        );
        x += grid_spacing;
    }
    let mut y = rect.min.y;
    while y < rect.max.y {
        painter.line_segment(
            [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
            stroke,
        );
        y += grid_spacing;
    }
}

pub fn draw_text_blocks(ui: &mut egui::Ui, blocks: &[TextBlock], transform: &ViewTransform) {
    let painter = ui.painter_at(ui.clip_rect());
    for block in blocks {
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
        let x_min = transform.x_offset + x_min * transform.scale;
        let y_min = transform.y_offset + y_min * transform.scale;
        let x_max = transform.x_offset + x_max * transform.scale;
        let y_max = transform.y_offset + y_max * transform.scale;
        painter.rect(
            egui::Rect::from_min_max(egui::pos2(x_min, y_min), egui::pos2(x_max, y_max)),
            2.0,
            egui::Color32::TRANSPARENT,
            egui::Stroke::new(1.0, egui::Color32::BLUE),
            egui::StrokeKind::Inside,
        );
    }
}

pub fn draw_lines(ui: &mut egui::Ui, blocks: &[TextBlock], transform: &ViewTransform) {
    let painter = ui.painter_at(ui.clip_rect());
    for block in blocks {
        for line in &block.lines {
            let x_min = transform.x_offset + line.bbox.0 * transform.scale;
            let y_min = transform.y_offset + line.bbox.1 * transform.scale;
            let x_max = transform.x_offset + line.bbox.2 * transform.scale;
            let y_max = transform.y_offset + line.bbox.3 * transform.scale;
            painter.rect(
                egui::Rect::from_min_max(egui::pos2(x_min, y_min), egui::pos2(x_max, y_max)),
                0.0,
                egui::Color32::TRANSPARENT,
                egui::Stroke::new(1.0, egui::Color32::YELLOW),
                egui::StrokeKind::Inside,
            );
        }
    }
}

pub fn draw_bboxes(ui: &mut egui::Ui, blocks: &[TextBlock], transform: &ViewTransform) {
    let painter = ui.painter_at(ui.clip_rect());
    for block in blocks {
        for line in &block.lines {
            for element in &line.elements {
                let x = transform.x_offset + element.bbox.0 * transform.scale;
                let y = transform.y_offset + element.bbox.1 * transform.scale;
                painter.text(
                    egui::pos2(x, y),
                    egui::Align2::LEFT_TOP,
                    &element.text,
                    egui::FontId::monospace(10.0 * transform.scale),
                    egui::Color32::BLACK,
                );
            }
        }
    }
}

fn select_element_at_position(viewer: &mut Viewer, pos: egui::Pos2, transform: ViewTransform) {
    // Convert screen position to PDF coordinates
    let pdf_x = (pos.x - transform.x_offset) / transform.scale;
    let pdf_y = (pos.y - transform.y_offset) / transform.scale;

    // Check if any line contains this point
    for block in &viewer.blocks {
        if block.page_number as usize == viewer.current_page + 1 {
            for line in &block.lines {
                if pdf_x >= line.bbox.0
                    && pdf_x <= line.bbox.2
                    && pdf_y >= line.bbox.1
                    && pdf_y <= line.bbox.3
                {
                    // Set as selected line
                    viewer.selected_line = Some(line.id);
                    viewer.selected_bbox = Some(line.bbox);

                    // If this line has a match, highlight it
                    // if let Some((template_id, _)) = viewer.debug_data.get_matching_template(line.id)
                    // {
                    //     viewer.highlighted_match = Some((template_id, line.id));
                    // }

                    return;
                }
            }
        }
    }

    // If no line was found, clear selection
    viewer.selected_line = None;
    viewer.selected_bbox = None;
}

#[cfg(target_arch = "wasm32")]
pub fn exec_future<F: Future<Output = ()> + 'static>(f: F) {
    wasm_bindgen_futures::spawn_local(f);
}
