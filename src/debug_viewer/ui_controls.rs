use crate::debug_viewer::app::DebugViewer;
use eframe::egui;

/// Display the main UI controls for the debug viewer
pub fn show_controls(viewer: &mut DebugViewer, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        // Page navigation
        ui.label("Page:");
        if ui.button("◀").clicked() && viewer.current_page > 0 {
            viewer.current_page -= 1;
        }
        ui.label((viewer.current_page + 1).to_string());
        if ui.button("▶").clicked() && viewer.current_page < viewer.textures.len() - 1 {
            viewer.current_page += 1;
        }

        ui.separator();

        // View settings
        ui.checkbox(&mut viewer.show_text, "Text");
        ui.checkbox(&mut viewer.show_lines, "Lines");
        ui.checkbox(&mut viewer.show_blocks, "Blocks");
        ui.checkbox(&mut viewer.show_grid, "Grid");
        if viewer.show_grid {
            ui.add(egui::Slider::new(&mut viewer.grid_spacing, 5.0..=50.0).text("Grid Size"));
        }

        ui.separator();

        // Zoom and pan controls
        if ui.button("Zoom +").clicked() {
            viewer.zoom *= 1.2;
        }
        if ui.button("Zoom -").clicked() {
            viewer.zoom /= 1.2;
        }
        if ui.button("Reset View").clicked() {
            viewer.zoom = 1.0;
            viewer.pan = egui::Vec2::ZERO;
        }

        ui.separator();

        // Template match controls
        ui.checkbox(&mut viewer.show_matches, "Highlight Matches");
        ui.checkbox(&mut viewer.show_match_panel, "Match Panel");
        if viewer.show_matches {
            ui.add(
                egui::Slider::new(&mut viewer.match_filter_threshold, 0.0..=1.0)
                    .text("Match Threshold")
                    .trailing_fill(true),
            );
        }
    });
}
