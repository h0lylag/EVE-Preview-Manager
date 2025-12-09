//! Behavior settings component (per-profile settings)

use eframe::egui;
use crate::config::profile::Profile;
use crate::constants::gui::*;
use crate::types::Dimensions;

/// State for behavior settings UI
pub struct BehaviorSettingsState {
    show_resize_confirmation: bool,
    pending_resize_all: Option<Dimensions>,
    current_width: u16,
    current_height: u16,
    last_target: String,
}

impl BehaviorSettingsState {
    pub fn new() -> Self {
        Self {
            show_resize_confirmation: false,
            pending_resize_all: None,
            current_width: 250,
            current_height: 141,
            last_target: "---".to_string(),
        }
    }
}

impl Default for BehaviorSettingsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders behavior settings UI and returns true if changes were made
pub fn ui(ui: &mut egui::Ui, profile: &mut Profile, state: &mut BehaviorSettingsState) -> bool {
    let mut changed = false;

    // Behavior Settings (Global)
    ui.group(|ui| {
        ui.label(egui::RichText::new("Behavior Settings").strong());
        ui.add_space(ITEM_SPACING);

        // Minimize clients on switch
        if ui.checkbox(&mut profile.client_minimize_on_switch,
            "Minimize EVE clients when switching focus").changed() {
            changed = true;
        }

        ui.label(egui::RichText::new(
            "When clicking a thumbnail, minimize all other EVE clients")
            .small()
            .weak());

        ui.add_space(ITEM_SPACING);

        // Hide when no focus
        if ui.checkbox(&mut profile.thumbnail_hide_not_focused,
            "Hide thumbnails when EVE loses focus").changed() {
            changed = true;
        }

        ui.label(egui::RichText::new(
            "When enabled, thumbnails disappear when no EVE window is focused")
            .small()
            .weak());

        ui.add_space(ITEM_SPACING);

        // Auto-save thumbnail positions
        if ui.checkbox(
            &mut profile.thumbnail_auto_save_position,
            "Automatically save thumbnail positions"
        ).changed() {
            changed = true;
        }

        ui.label(egui::RichText::new(
            "When disabled, positions are only saved when you use 'Save Thumbnail Positions' from the system tray menu")
            .small()
            .weak());

        ui.add_space(ITEM_SPACING);

        // Preserve thumbnail position on character swap
        if ui.checkbox(&mut profile.thumbnail_preserve_position_on_swap,
            "New characters inherit thumbnail position").changed() {
            changed = true;
        }

        ui.label(egui::RichText::new(
            "New characters inherit thumbnail position from the logged-out character")
            .small()
            .weak());

        ui.add_space(ITEM_SPACING);

        // Snap threshold
        ui.horizontal(|ui| {
            ui.label("Thumbnail Snap Distance:");
            if ui.add(egui::Slider::new(&mut profile.thumbnail_snap_threshold, 0..=50)
                .suffix(" px")).changed() {
                changed = true;
            }
        });

        ui.label(egui::RichText::new(
            "Distance for edge/corner snapping (0 = disabled)")
            .small()
            .weak());
    });

    ui.add_space(SECTION_SPACING);

    // Default Thumbnail Size Settings
    ui.group(|ui| {
        ui.label(egui::RichText::new("Default Thumbnail Size").strong());
        ui.add_space(ITEM_SPACING);

        // Aspect ratio preset definitions
        let aspect_ratios = [
            ("16:9", 16.0 / 9.0),
            ("16:10", 16.0 / 10.0),
            ("4:3", 4.0 / 3.0),
            ("21:9", 21.0 / 9.0),
            ("Custom", 0.0),
        ];

        // Calculate current aspect ratio and find closest matching preset
        let current_ratio = profile.thumbnail_default_width as f32 / profile.thumbnail_default_height as f32;
        let detected_preset = {
            let mut preset = "Custom";
            for (name, ratio) in &aspect_ratios[..aspect_ratios.len()-1] {
                if (current_ratio - ratio).abs() < 0.01 {
                    preset = name;
                    break;
                }
            }
            preset
        };

        // Use egui memory to persist the selected mode
        let id = ui.make_persistent_id("default_thumbnail_aspect_mode");
        let mut selected_mode = ui.data_mut(|d|
            d.get_temp::<String>(id).unwrap_or_else(|| detected_preset.to_string())
        );

        ui.horizontal(|ui| {
            ui.label("Aspect Ratio:");

            let mut mode_changed = false;
            egui::ComboBox::from_id_salt("default_thumbnail_aspect_ratio")
                .selected_text(&selected_mode)
                .show_ui(ui, |ui| {
                    for (name, ratio) in &aspect_ratios {
                        if ui.selectable_value(&mut selected_mode, name.to_string(), *name).changed() {
                            mode_changed = true;
                            if *ratio > 0.0 {
                                // Update height based on width and selected ratio
                                profile.thumbnail_default_height =
                                    (profile.thumbnail_default_width as f32 / ratio).round() as u16;
                                changed = true;
                            }
                        }
                    }
                });

            // Save the selected mode to egui memory
            if mode_changed {
                ui.data_mut(|d| d.insert_temp(id, selected_mode.clone()));
            }
        });

        ui.add_space(ITEM_SPACING / 2.0);

        // Width slider (primary control)
        ui.horizontal(|ui| {
            ui.label("Width:");
            if ui.add(egui::Slider::new(&mut profile.thumbnail_default_width, 100..=800)
                .suffix(" px")).changed() {
                // If not custom, maintain aspect ratio
                if selected_mode != "Custom" {
                    for (name, ratio) in &aspect_ratios[..aspect_ratios.len()-1] {
                        if name == &selected_mode.as_str() {
                            profile.thumbnail_default_height =
                                (profile.thumbnail_default_width as f32 / ratio).round() as u16;
                            break;
                        }
                    }
                }
                changed = true;
            }
        });

        // Height slider (locked unless custom)
        let is_custom = selected_mode == "Custom";
        ui.horizontal(|ui| {
            ui.label("Height:");

            if is_custom {
                if ui.add(egui::Slider::new(&mut profile.thumbnail_default_height, 50..=600)
                    .suffix(" px")).changed() {
                    changed = true;
                }
            } else {
                ui.add_enabled(false,
                    egui::Slider::new(&mut profile.thumbnail_default_height, 50..=600)
                        .suffix(" px"));
            }
        });

        // Preview display
        ui.horizontal(|ui| {
            ui.weak(format!(
                "Preview: {}×{} ({:.2}:1 ratio)",
                profile.thumbnail_default_width,
                profile.thumbnail_default_height,
                profile.thumbnail_default_width as f32 / profile.thumbnail_default_height as f32
            ));
        });

        ui.add_space(ITEM_SPACING / 2.0);

        ui.label(egui::RichText::new(
            "Default size for newly created character thumbnails")
            .small()
            .weak());
    });

    ui.add_space(SECTION_SPACING);

    // Thumbnail Size Adjustment
    ui.group(|ui| {
        ui.label(egui::RichText::new("Thumbnail Size Adjustment").strong());
        ui.add_space(ITEM_SPACING);

        // Target selector
        let id = ui.make_persistent_id("thumbnail_resize_target");
        let mut selected_target = ui.data_mut(|d|
            d.get_temp::<String>(id).unwrap_or_else(|| "---".to_string())
        );

        ui.horizontal(|ui| {
            ui.label("Resize:");

            egui::ComboBox::from_id_salt("thumbnail_resize_target")
                .selected_text(&selected_target)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut selected_target, "---".to_string(), "---");
                    ui.selectable_value(&mut selected_target, "All Characters".to_string(), "All Characters");
                    ui.separator();

                    // Add individual characters
                    let mut char_names: Vec<_> = profile.character_thumbnails.keys().cloned().collect();
                    char_names.sort();
                    for char_name in char_names {
                        ui.selectable_value(&mut selected_target, char_name.clone(), char_name);
                    }
                });

            ui.data_mut(|d| d.insert_temp(id, selected_target.clone()));
        });

        ui.add_space(ITEM_SPACING / 2.0);

        let is_enabled = selected_target != "---";

        // If target changed, load new dimensions
        if is_enabled && selected_target != state.last_target {
            let (width, height) = if selected_target == "All Characters" {
                // Use first character's dimensions as baseline, or default
                profile.character_thumbnails.values().next()
                    .map(|s| (s.dimensions.width, s.dimensions.height))
                    .unwrap_or((250, 141))
            } else {
                // Get specific character's dimensions
                profile.character_thumbnails.get(&selected_target)
                    .map(|s| (s.dimensions.width, s.dimensions.height))
                    .unwrap_or((250, 141)) // Default fallback if character not found (safeguard)
            };

            state.current_width = width;
            state.current_height = height;
            state.last_target = selected_target.clone();
        }

        // Aspect ratio controls (same as default settings)
        let aspect_ratios = [
            ("16:9", 16.0 / 9.0),
            ("16:10", 16.0 / 10.0),
            ("4:3", 4.0 / 3.0),
            ("21:9", 21.0 / 9.0),
            ("Custom", 0.0),
        ];

        let current_ratio = state.current_width as f32 / state.current_height as f32;
        let detected_preset = {
            let mut preset = "Custom";
            for (name, ratio) in &aspect_ratios[..aspect_ratios.len()-1] {
                if (current_ratio - ratio).abs() < 0.01 {
                    preset = name;
                    break;
                }
            }
            preset
        };

        let mode_id = ui.make_persistent_id(format!("resize_aspect_mode_{}", selected_target));
        let mut aspect_mode = ui.data_mut(|d|
            d.get_temp::<String>(mode_id).unwrap_or_else(|| detected_preset.to_string())
        );

        ui.add_enabled_ui(is_enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("Aspect Ratio:");

                let mut mode_changed = false;
                egui::ComboBox::from_id_salt(format!("resize_aspect_{}", selected_target))
                    .selected_text(&aspect_mode)
                    .show_ui(ui, |ui| {
                        for (name, ratio) in &aspect_ratios {
                            if ui.selectable_value(&mut aspect_mode, name.to_string(), *name).changed() {
                                mode_changed = true;
                                if *ratio > 0.0 {
                                    state.current_height = (state.current_width as f32 / ratio).round() as u16;
                                }
                            }
                        }
                    });

                if mode_changed {
                    ui.data_mut(|d| d.insert_temp(mode_id, aspect_mode.clone()));
                }
            });

            ui.add_space(ITEM_SPACING / 2.0);

            // Width slider
            ui.horizontal(|ui| {
                ui.label("Width:");
                if ui.add(egui::Slider::new(&mut state.current_width, 100..=800)
                    .suffix(" px")).changed() {
                    // Maintain aspect ratio if not custom
                    if aspect_mode != "Custom" {
                        for (name, ratio) in &aspect_ratios[..aspect_ratios.len()-1] {
                            if name == &aspect_mode.as_str() {
                                state.current_height = (state.current_width as f32 / ratio).round() as u16;
                                break;
                            }
                        }
                    }
                }
            });

            // Height slider
            let is_custom = aspect_mode == "Custom";
            ui.horizontal(|ui| {
                ui.label("Height:");

                if is_custom {
                    ui.add(egui::Slider::new(&mut state.current_height, 50..=600)
                        .suffix(" px"));
                } else {
                    ui.add_enabled(false,
                        egui::Slider::new(&mut state.current_height, 50..=600)
                            .suffix(" px"));
                }
            });

            ui.horizontal(|ui| {
                ui.weak(format!(
                    "Preview: {}×{} ({:.2}:1 ratio)",
                    state.current_width,
                    state.current_height,
                    state.current_width as f32 / state.current_height as f32
                ));
            });

            ui.add_space(ITEM_SPACING / 2.0);

            // Apply button
            ui.horizontal(|ui| {
                if ui.button("Apply Size").clicked() {
                    let new_dimensions = Dimensions::new(state.current_width, state.current_height);

                    if selected_target == "All Characters" {
                        // Show confirmation for "All Characters"
                        state.pending_resize_all = Some(new_dimensions);
                        state.show_resize_confirmation = true;
                    } else {
                        // Apply to single character immediately
                        if let Some(char_settings) = profile.character_thumbnails.get_mut(&selected_target) {
                            char_settings.dimensions = new_dimensions;
                            changed = true;
                        }
                    }
                }
            });
        });
    });

    // Confirmation dialog for resizing all characters
    if state.show_resize_confirmation {
        egui::Window::new("Confirm Resize")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                if let Some(dims) = state.pending_resize_all {
                    ui.label(format!(
                        "Apply {}×{} size to all {} character thumbnails?",
                        dims.width,
                        dims.height,
                        profile.character_thumbnails.len()
                    ));
                    ui.add_space(ITEM_SPACING);
                    ui.label(egui::RichText::new("This will overwrite all individual thumbnail sizes.")
                        .small()
                        .weak());
                    ui.add_space(ITEM_SPACING);

                    ui.horizontal(|ui| {
                        if ui.button("Yes, Resize All").clicked() {
                            // Apply to all characters
                            for char_settings in profile.character_thumbnails.values_mut() {
                                char_settings.dimensions = dims;
                            }
                            changed = true;
                            state.show_resize_confirmation = false;
                            state.pending_resize_all = None;
                        }

                        if ui.button("Cancel").clicked() {
                            state.show_resize_confirmation = false;
                            state.pending_resize_all = None;
                        }
                    });
                }
            });
    }

    changed
}
