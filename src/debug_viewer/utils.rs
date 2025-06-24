use crate::layout::TextBlock;
use crate::layout::TextLine;
use eframe::egui;
use lopdf::Document;
use std::error::Error;
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
pub fn find_line_by_id(blocks: &[TextBlock], line_id: Uuid) -> Option<&TextLine> {
    blocks
        .iter()
        .flat_map(|b| &b.lines)
        .find(|l| l.id == line_id)
}

/// Renders detailed event information in a grid
pub fn render_event_details(
    ui: &mut egui::Ui,
    event: &str,
    selected_fields: &std::collections::HashSet<String>,
) {
    // Parse event string to filter fields
    let parts: Vec<&str> = event.split("; ").collect();
    for part in parts {
        if let Some((field_name, value)) = part.split_once(" = ") {
            if selected_fields.contains(field_name) {
                ui.label(field_name);
                ui.label(value);
            }
        } else {
            // Display parts without " = " as is
            ui.label(part);
        }
        ui.end_row();
    }
}
