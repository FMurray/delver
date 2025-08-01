use crate::app::Viewer;
use crate::stubs::{DebugDataStore, EntityEvents};
use crate::utils;
use eframe::egui;
use egui::{CollapsingHeader, ScrollArea};
use std::collections::HashSet;
use uuid::Uuid;

/// Show the event inspection panel for a selected element
pub fn show_event_panel(viewer: &mut Viewer, ctx: &egui::Context, line_id: Uuid) {
    egui::Window::new("Element Events")
        .auto_sized()
        .show(ctx, |ui| {
            // Get all events for this line
            let events = get_line_with_elements(&viewer.debug_data, line_id);

            // Show basic info about the selected line
            if let Some(line) = utils::find_line_by_id(&viewer.blocks, line_id) {
                ui.heading("Line Information");
                ui.horizontal(|ui| {
                    ui.label("Text:");
                    ui.label(&line.text);
                });
                ui.horizontal(|ui| {
                    ui.label("ID:");
                    ui.label(line.id.to_string());
                });
                ui.horizontal(|ui| {
                    ui.label("Page:");
                    ui.label(line.page_number.to_string());
                });
                ui.horizontal(|ui| {
                    ui.label("Bbox:");
                    ui.label(format!(
                        "({:.1}, {:.1}, {:.1}, {:.1})",
                        line.bbox.0, line.bbox.1, line.bbox.2, line.bbox.3
                    ));
                });
            }

            ui.separator();
            ui.heading("Events");

            // Collect all fields from the events for filtering
            let all_fields = collect_fields_from_events(&events);

            // Show field selection
            ui.collapsing("Field Selection", |ui| {
                ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    // Calculate column width based on number of fields
                    let num_columns = (all_fields.len() / 10).max(1).min(4);
                    let column_width = ui.available_width() / num_columns as f32;

                    ui.horizontal(|ui| {
                        let mut fields_by_column: Vec<Vec<String>> = vec![Vec::new(); num_columns];

                        // Distribute fields into columns
                        for (i, field) in all_fields.iter().enumerate() {
                            fields_by_column[i % num_columns].push(field.clone());
                        }

                        // Show each column
                        for column in fields_by_column {
                            ui.vertical(|ui| {
                                ui.set_min_width(column_width);
                                for field in column {
                                    let mut selected = viewer.selected_fields.contains(&field);
                                    if ui.checkbox(&mut selected, &field).changed() {
                                        if selected {
                                            viewer.selected_fields.insert(field.clone());
                                        } else {
                                            viewer.selected_fields.remove(&field);
                                        }
                                    }
                                }
                            });
                        }
                    });
                });

                ui.horizontal(|ui| {
                    if ui.button("Select All").clicked() {
                        viewer.selected_fields = all_fields.clone();
                    }
                    if ui.button("Clear All").clicked() {
                        viewer.selected_fields.clear();
                    }
                });
            });

            ui.separator();

            // Display events with filtering
            ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                let mut selected_events = HashSet::new();

                // Display messages in a tree view
                for (i, message) in events.messages.iter().enumerate() {
                    let is_selected = viewer.selected_events.contains(&i.to_string());

                    // Check if this message contains any of the selected fields
                    let contains_selected_field = !viewer.selected_fields.is_empty()
                        && viewer
                            .selected_fields
                            .iter()
                            .any(|field| message.contains(&format!("{} = ", field)));

                    // Skip if we have field filters and none match
                    if !viewer.selected_fields.is_empty() && !contains_selected_field {
                        continue;
                    }

                    CollapsingHeader::new(format!("Event {}", i))
                        .id_source(format!("event_{}_{}", line_id, i))
                        .default_open(is_selected || contains_selected_field)
                        .show(ui, |ui| {
                            // Add to selected events when expanded
                            selected_events.insert(i.to_string());

                            // Display event details in a grid
                            egui::Grid::new(format!("event_details_{}", i))
                                .num_columns(2)
                                .spacing([10.0, 2.0])
                                .striped(true)
                                .show(ui, |ui| {
                                    utils::render_event_details(
                                        ui,
                                        message,
                                        &viewer.selected_fields,
                                    );
                                });
                        });
                }

                // Update selected events
                viewer.selected_events = selected_events;
            });
        });
}

/// Get all events for a line and its elements
fn get_line_with_elements(_store: &DebugDataStore, _line_id: Uuid) -> EntityEvents {
    EntityEvents::default()
}

/// Collect all fields from events for filtering options
fn collect_fields_from_events(events: &EntityEvents) -> HashSet<String> {
    let mut fields = HashSet::new();

    for message in &events.messages {
        // Extract field names (format is usually "field_name = value; field_name2 = value2")
        for part in message.split(';') {
            if let Some((field, _)) = part.split_once(" = ") {
                fields.insert(field.trim().to_string());
            }
        }
    }

    fields
}
