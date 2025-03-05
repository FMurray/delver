use crate::{
    debug_viewer::{app::DebugViewer, utils},
    logging::DebugDataStore,
};
use eframe::egui;
use egui::{CollapsingHeader, ScrollArea};
use std::collections::HashMap;
use uuid::Uuid;

/// Show the template matches panel
pub fn show_match_panel(viewer: &mut DebugViewer, ctx: &egui::Context) {
    egui::SidePanel::right("template_matches_panel")
        .resizable(true)
        .default_width(300.0)
        .show(ctx, |ui| {
            ui.heading("Template Matches");

            // Template matching trace events - ADD THIS SECTION
            ui.collapsing("Matching Operations", |ui| {
                ui.label("Recent matching operations:");

                // Display trace events with target=MATCHER_OPERATIONS
                let matcher_ops = viewer.debug_data.get_events_by_target("MATCHER_OPERATIONS");
                let template_matches = viewer.debug_data.get_events_by_target("TEMPLATE_MATCH");

                if matcher_ops.is_empty() && template_matches.is_empty() {
                    ui.label("No template matching operations found in logs");

                    // Add some diagnostic tips
                    ui.separator();
                    ui.label("Possible reasons for no matches:");
                    ui.label("• No templates defined in your code");
                    ui.label("• Templates not being matched against content");
                    ui.label("• Tracing not capturing matcher events");
                    ui.label("• Threshold too high for fuzzy matching");
                } else {
                    // Show matcher operations
                    ui.label(format!("Found {} matcher operations", matcher_ops.len()));
                    ui.label(format!("Found {} template matches", template_matches.len()));

                    // Show sample of recent operations
                    ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                        for event in matcher_ops.iter().take(10) {
                            ui.label(format!("• {}", event));
                        }

                        ui.separator();
                        for event in template_matches.iter().take(10) {
                            ui.label(format!("• {}", event));
                        }
                    });
                }
            });

            // Diagnostic information - ADD THIS SECTION
            ui.collapsing("Debug Info", |ui| {
                // Count total matches
                let total_matches = count_all_template_matches(&viewer.debug_data);
                ui.label(format!("Total template matches found: {}", total_matches));

                // Show all templates
                if let Some(template_ids) = list_all_templates(&viewer.debug_data) {
                    ui.label(format!("Templates available: {}", template_ids.len()));
                    for id in template_ids {
                        let name = viewer
                            .debug_data
                            .get_template_name(id)
                            .unwrap_or_else(|| format!("Template {}", id));
                        ui.label(format!("• {} ({})", name, id));

                        // Show matches for this template
                        let matches = viewer.debug_data.get_template_matches(id);
                        ui.indent("template_matches", |ui| {
                            ui.label(format!("Matches: {}", matches.len()));
                            for (content_id, score) in matches {
                                ui.label(format!(
                                    "  - Content {} (score: {:.2})",
                                    content_id, score
                                ));
                            }
                        });
                    }
                } else {
                    ui.label("No templates registered in debug data");
                }

                // Check if the content items are actually being recorded
                ui.separator();
                ui.label("Checking content items:");
                let content_count = count_content_items(viewer);
                ui.label(format!("Total content items: {}", content_count));
            });

            ui.separator();

            // Threshold slider
            ui.horizontal(|ui| {
                ui.label("Score threshold:");
                ui.add(egui::Slider::new(
                    &mut viewer.match_filter_threshold,
                    0.0..=1.0,
                ));
            });

            ui.separator();

            // Collect all template matches
            let all_matches = collect_template_matches(viewer);

            if all_matches.is_empty() {
                ui.label("No template matches found");
                return;
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                // For each template
                for (template_id, matches) in &all_matches {
                    // Get template name if available
                    let template_name = viewer
                        .debug_data
                        .get_template_name(*template_id)
                        .unwrap_or_else(|| format!("Template {}", template_id));

                    // Show template section with collapsing header
                    egui::CollapsingHeader::new(format!("📄 {}", template_name))
                        .default_open(true)
                        .show(ui, |ui| {
                            // Display template DOM structure
                            display_template_structure(viewer, ui, *template_id);

                            ui.separator();
                            ui.label("Matches:");

                            // Show all matches for this template
                            for &(content_id, score) in matches {
                                if score < viewer.match_filter_threshold {
                                    continue;
                                }

                                // Get content text if available
                                let content_text = get_content_text(viewer, content_id)
                                    .unwrap_or_else(|| "Unknown content".to_string());

                                ui.horizontal(|ui| {
                                    // Score display
                                    ui.label(format!("{:.2}", score));

                                    // Highlight current match
                                    let is_highlighted = viewer.highlighted_match
                                        == Some((*template_id, content_id));

                                    // Button to navigate to match
                                    if ui
                                        .button(if is_highlighted {
                                            "➡️ Go to match"
                                        } else {
                                            "Go to match"
                                        })
                                        .on_hover_text(format!("Navigate to: {}", content_text))
                                        .clicked()
                                    {
                                        // Navigate to this match
                                        viewer.highlighted_match = Some((*template_id, content_id));
                                        navigate_to_match(viewer, content_id);
                                    }
                                });

                                // Preview of content text
                                let preview = if content_text.len() > 50 {
                                    format!("{}...", &content_text[..47])
                                } else {
                                    content_text.clone()
                                };

                                ui.indent("match_content", |ui| {
                                    ui.label(preview).on_hover_text(content_text);
                                });

                                ui.separator();
                            }
                        });
                }
            });
        });
}

// Display the DOM structure of a template
fn display_template_structure(viewer: &DebugViewer, ui: &mut egui::Ui, template_id: Uuid) {
    // Get template structure from debug data
    if let Some(structure) = viewer.debug_data.get_template_structure(template_id) {
        ui.label("Template structure:");
        ui.indent("structure", |ui| {
            // Render hierarchical structure
            for (i, item) in structure.iter().enumerate() {
                ui.label(format!("• {}", item));
            }
        });
    } else {
        ui.label("No template structure available");
    }
}

// Helper function to navigate to a matching element
fn navigate_to_match(viewer: &mut DebugViewer, content_id: Uuid) {
    // Find the page number and coordinates for this content
    for block in &viewer.blocks {
        for line in &block.lines {
            if line.id == content_id {
                // Set current page
                viewer.current_page = (block.page_number - 1) as usize;

                // Set selected line
                viewer.selected_line = Some(content_id);
                viewer.selected_bbox = Some(line.bbox);

                // Calculate center position for panning
                let center_x = (line.bbox.0 + line.bbox.2) / 2.0;
                let center_y = (line.bbox.1 + line.bbox.3) / 2.0;

                // Set pan to center on this element
                viewer.pan = egui::vec2(
                    -center_x * viewer.zoom
                        + viewer.pdf_dimensions[viewer.current_page].0 * viewer.zoom / 2.0,
                    -center_y * viewer.zoom
                        + viewer.pdf_dimensions[viewer.current_page].1 * viewer.zoom / 2.0,
                );

                return;
            }
        }
    }
}

// Collect all template matches from the debug data
fn collect_template_matches(viewer: &DebugViewer) -> HashMap<Uuid, Vec<(Uuid, f32)>> {
    let mut result = HashMap::new();

    // For each line in all blocks, check if it has a template match
    for block in &viewer.blocks {
        for line in &block.lines {
            if let Some((template_id, score)) = viewer.debug_data.get_matching_template(line.id) {
                result
                    .entry(template_id)
                    .or_insert_with(Vec::new)
                    .push((line.id, score));
            }
        }
    }

    // Sort matches by score (descending)
    for matches in result.values_mut() {
        matches.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    }

    result
}

// Get text content for a line
fn get_content_text(viewer: &DebugViewer, line_id: Uuid) -> Option<String> {
    for block in &viewer.blocks {
        for line in &block.lines {
            if line.id == line_id {
                return Some(line.text.clone());
            }
        }
    }
    None
}

// Helper functions for diagnostics
fn count_all_template_matches(store: &DebugDataStore) -> usize {
    let matches = store.template_matches.lock().unwrap();
    matches.len()
}

fn list_all_templates(store: &DebugDataStore) -> Option<Vec<Uuid>> {
    let names = store.template_names.lock().unwrap();
    if names.is_empty() {
        None
    } else {
        Some(names.keys().cloned().collect())
    }
}

fn count_content_items(viewer: &DebugViewer) -> usize {
    let mut count = 0;
    for block in &viewer.blocks {
        count += block.lines.len();
    }
    count
}
