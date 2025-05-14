use crate::debug_viewer::{app::DebugViewer, utils};
use eframe::egui;

/// Render the PDF view with all visualizations
pub fn render_pdf_view(viewer: &mut DebugViewer, ui: &mut egui::Ui) {
    // Get the current texture
    if let Some(texture) = viewer.textures.get(viewer.current_page) {
        // Calculate view rectangle and transform
        let (rect, transform) = viewer.calculate_pdf_view_rect(ui, texture);

        // Create a response for interactions
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

        // Handle panning
        if response.dragged() {
            viewer.pan += response.drag_delta();
        }

        // Handle zooming with scroll
        if response.hovered() {
            let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll_delta != 0.0 {
                // Get mouse position relative to the image for zoom centering
                let mouse_pos = ui.input(|i| i.pointer.hover_pos());
                if let Some(mouse_pos) = mouse_pos {
                    // Adjust zoom
                    let old_zoom = viewer.zoom;
                    viewer.zoom *= 1.0 + (scroll_delta * 0.001).clamp(-0.1, 0.1);
                    viewer.zoom = viewer.zoom.max(0.1).min(10.0);

                    // Adjust pan to zoom toward cursor
                    if old_zoom != viewer.zoom {
                        let zoom_factor = viewer.zoom / old_zoom;
                        let mouse_rel_x = mouse_pos.x - rect.min.x - viewer.pan.x;
                        let mouse_rel_y = mouse_pos.y - rect.min.y - viewer.pan.y;
                        viewer.pan.x -= mouse_rel_x * (zoom_factor - 1.0);
                        viewer.pan.y -= mouse_rel_y * (zoom_factor - 1.0);
                    }
                }
            }
        }

        // Draw the PDF image
        let painter = ui.painter_at(rect);
        painter.image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        // Draw grid if enabled
        if viewer.show_grid {
            draw_grid(viewer, &painter, rect, &transform);
        }

        // Draw blocks if enabled
        if viewer.show_blocks {
            draw_blocks(viewer, &painter, &transform);
        }

        // Draw lines if enabled
        if viewer.show_lines {
            draw_lines(viewer, &painter, &transform);
        }

        // Draw text if enabled
        if viewer.show_text {
            draw_text(viewer, &painter, &transform);
        }

        // Draw matches if enabled
        if viewer.show_matches {
            draw_matches(viewer, &painter, &transform);
        }

        // Handle element selection
        if response.clicked() {
            if let Some(mouse_pos) = response.hover_pos() {
                select_element_at_position(viewer, mouse_pos, &transform);
            }
        }
    } else {
        ui.centered_and_justified(|ui| {
            ui.heading("No page available");
        });
    }
}

// Draw a grid overlay
fn draw_grid(
    viewer: &DebugViewer,
    painter: &egui::Painter,
    rect: egui::Rect,
    transform: &utils::ViewTransform,
) {
    let grid_size = viewer.grid_spacing * transform.scale;

    // Draw vertical lines
    let mut x = rect.min.x + (transform.x_offset % grid_size);
    while x < rect.max.x {
        painter.line_segment(
            [egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)],
            egui::Stroke::new(
                0.5,
                egui::Color32::from_rgba_premultiplied(100, 100, 100, 50),
            ),
        );
        x += grid_size;
    }

    // Draw horizontal lines
    let mut y = rect.min.y + (transform.y_offset % grid_size);
    while y < rect.max.y {
        painter.line_segment(
            [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
            egui::Stroke::new(
                0.5,
                egui::Color32::from_rgba_premultiplied(100, 100, 100, 50),
            ),
        );
        y += grid_size;
    }
}

// Draw blocks
fn draw_blocks(viewer: &DebugViewer, painter: &egui::Painter, transform: &utils::ViewTransform) {
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
            );
        }
    }
}

// Draw lines
fn draw_lines(viewer: &DebugViewer, painter: &egui::Painter, transform: &utils::ViewTransform) {
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
                );
            }
        }
    }
}

// Draw text
fn draw_text(viewer: &DebugViewer, painter: &egui::Painter, transform: &utils::ViewTransform) {
    for block in &viewer.blocks {
        if block.page_number as usize == viewer.current_page + 1 {
            for line in &block.lines {
                for element in &line.elements {
                    // Convert to screen coordinates
                    let x = transform.x_offset + element.bbox.0 * transform.scale;
                    let y = transform.y_offset + element.bbox.1 * transform.scale;

                    // Draw element text
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
}

// Draw template matches
fn draw_matches(viewer: &DebugViewer, painter: &egui::Painter, transform: &utils::ViewTransform) {
    for block in &viewer.blocks {
        if block.page_number as usize == viewer.current_page + 1 {
            for line in &block.lines {
                // Check if this line has a template match
                if let Some((template_id, score)) = viewer.debug_data.get_matching_template(line.id)
                {
                    // Skip if below threshold (except for highlighted match)
                    if score < viewer.match_filter_threshold
                        && viewer.highlighted_match != Some((template_id, line.id))
                    {
                        continue;
                    }

                    // Calculate screen coordinates for the line
                    let x_min = transform.x_offset + line.bbox.0 * transform.scale;
                    let y_min = transform.y_offset + line.bbox.1 * transform.scale;
                    let x_max = transform.x_offset + line.bbox.2 * transform.scale;
                    let y_max = transform.y_offset + line.bbox.3 * transform.scale;

                    let rect = egui::Rect {
                        min: egui::pos2(x_min, y_min),
                        max: egui::pos2(x_max, y_max),
                    };

                    // Determine highlight color and style
                    let color = utils::match_score_to_color(score);
                    let stroke_width = if viewer.highlighted_match == Some((template_id, line.id)) {
                        3.0 // Thicker stroke for highlighted match
                    } else {
                        1.5
                    };

                    // Draw highlight
                    painter.rect_stroke(
                        rect,
                        3.0, // corner radius
                        egui::Stroke::new(stroke_width, color),
                    );

                    // Add label for highlighted match
                    if viewer.highlighted_match == Some((template_id, line.id)) {
                        painter.text(
                            rect.right_top(),
                            egui::Align2::RIGHT_TOP,
                            format!("Match: {:.2}", score),
                            egui::FontId::proportional(12.0),
                            egui::Color32::WHITE,
                        );
                    }
                }
            }
        }
    }
}

// Select an element at the given screen position
fn select_element_at_position(
    viewer: &mut DebugViewer,
    pos: egui::Pos2,
    transform: &utils::ViewTransform,
) {
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
                    if let Some((template_id, _)) = viewer.debug_data.get_matching_template(line.id)
                    {
                        viewer.highlighted_match = Some((template_id, line.id));
                    }

                    return;
                }
            }
        }
    }

    // If no line was found, clear selection
    viewer.selected_line = None;
    viewer.selected_bbox = None;
}
