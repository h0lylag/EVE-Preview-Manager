use crate::config::profile::Profile;
use crate::constants::gui::*;
use crate::types::Dimensions;
use eframe::egui;

/// State for visual settings UI
pub struct VisualSettingsState {
    available_fonts: Vec<String>,
    font_load_error: Option<String>,
    // Resizing state
    show_resize_confirmation: bool,
    pending_resize_all: Option<Dimensions>,
    current_width: u16,
    current_height: u16,
    last_target: String,
}

impl VisualSettingsState {
    pub fn new() -> Self {
        // Load available fonts at GUI startup
        let (available_fonts, font_load_error) = match crate::preview::list_fonts() {
            Ok(fonts) => (fonts, None),
            Err(e) => {
                tracing::warn!(error = ?e, "Failed to load font list from fontconfig");
                (vec!["Monospace".to_string()], Some(e.to_string()))
            }
        };

        Self {
            available_fonts,
            font_load_error,
            show_resize_confirmation: false,
            pending_resize_all: None,
            current_width: 250,
            current_height: 141,
            last_target: "---".to_string(),
        }
    }
}

impl Default for VisualSettingsState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn ui(ui: &mut egui::Ui, profile: &mut Profile, state: &mut VisualSettingsState) -> bool {
    let mut changed = false;

    ui.columns(2, |columns| {
        // Column 1: Visual Settings
        columns[0].group(|ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new("Visual Settings").strong());
            ui.add_space(ITEM_SPACING);

            // Enable/disable thumbnail rendering
            if ui
                .checkbox(&mut profile.thumbnail_enabled, "Enable thumbnail previews")
                .changed()
            {
                changed = true;
            }
            ui.label(
                egui::RichText::new(
                    "When disabled, daemon still runs for hotkeys but thumbnails are not rendered",
                )
                .small()
                .weak(),
            );

            ui.add_space(ITEM_SPACING);

            // Remaining settings are grayed out when thumbnails disabled
            ui.add_enabled_ui(profile.thumbnail_enabled, |ui| {
                // Opacity
                ui.horizontal(|ui| {
                    ui.label("Opacity:");
                    if ui
                        .add(egui::Slider::new(&mut profile.thumbnail_opacity, 0..=100).suffix("%"))
                        .changed()
                    {
                        changed = true;
                    }
                });

                // Border toggle
                ui.horizontal(|ui| {
                    ui.label("Borders:");
                    if ui
                        .checkbox(&mut profile.thumbnail_border, "Enabled")
                        .changed()
                    {
                        changed = true;
                    }
                });

                // Border settings (greyed out if disabled)
                ui.indent("border_settings", |ui| {
                    ui.add_enabled_ui(profile.thumbnail_border, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Border Size:");
                            let _ = ui
                                .add(
                                    egui::DragValue::new(&mut profile.thumbnail_border_size)
                                        .range(1..=20),
                                );
                        });

                        ui.horizontal(|ui| {
                            ui.label("Border Color:");
                            let text_edit =
                                egui::TextEdit::singleline(&mut profile.thumbnail_border_color)
                                    .desired_width(100.0);
                            if ui.add(text_edit).changed() {
                                changed = true;
                            }

                            // Color picker button
                            if let Ok(mut color) = parse_hex_color(&profile.thumbnail_border_color)
                                && ui.color_edit_button_srgba(&mut color).changed()
                            {
                                profile.thumbnail_border_color = format_hex_color(color);
                                changed = true;
                            }
                        });
                    });
                });

                ui.add_space(ITEM_SPACING);

                // Text settings
                ui.horizontal(|ui| {
                    ui.label("Text Size:");
                    if ui
                        .add(egui::DragValue::new(&mut profile.thumbnail_text_size).range(8..=48))
                        .changed()
                    {
                        changed = true;
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Text Position:");
                    ui.label("X:");
                    if ui
                        .add(egui::DragValue::new(&mut profile.thumbnail_text_x).range(0..=100))
                        .changed()
                    {
                        changed = true;
                    }
                    ui.label("Y:");
                    if ui
                        .add(egui::DragValue::new(&mut profile.thumbnail_text_y).range(0..=100))
                        .changed()
                    {
                        changed = true;
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Text Color:");
                    let text_edit = egui::TextEdit::singleline(&mut profile.thumbnail_text_color)
                        .desired_width(100.0);
                    if ui.add(text_edit).changed() {
                        changed = true;
                    }

                    // Color picker button
                    if let Ok(mut color) = parse_hex_color(&profile.thumbnail_text_color)
                        && ui.color_edit_button_srgba(&mut color).changed()
                    {
                        profile.thumbnail_text_color = format_hex_color(color);
                        changed = true;
                    }
                });

                // Font family selector
                ui.horizontal(|ui| {
                    ui.label("Font:");

                    if let Some(ref error) = state.font_load_error {
                        ui.colored_label(egui::Color32::RED, "⚠")
                            .on_hover_text(format!("Failed to load fonts: {}", error));
                    }

                    egui::ComboBox::from_id_salt("text_font_family")
                        .selected_text(&profile.thumbnail_text_font)
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for font_family in &state.available_fonts {
                                if ui
                                    .selectable_value(
                                        &mut profile.thumbnail_text_font,
                                        font_family.clone(),
                                        font_family,
                                    )
                                    .changed()
                                {
                                    changed = true;
                                }
                            }
                        });
                });
            }); // Close add_enabled_ui
        }); // Close group

        // Column 2: Default Thumbnail Size Settings & Adjustments
        columns[1].vertical(|ui| {
            // Default Size Group
            ui.group(|ui| {
                ui.set_min_width(ui.available_width());
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
                let current_ratio = profile.thumbnail_default_width as f32
                    / profile.thumbnail_default_height as f32;
                let detected_preset = {
                    let mut preset = "Custom";
                    for (name, ratio) in &aspect_ratios[..aspect_ratios.len() - 1] {
                        if (current_ratio - ratio).abs() < 0.01 {
                            preset = name;
                            break;
                        }
                    }
                    preset
                };

                // Use egui memory to persist the selected mode
                let id = ui.make_persistent_id("default_thumbnail_aspect_mode");
                let mut selected_mode = ui.data_mut(|d| {
                    d.get_temp::<String>(id)
                        .unwrap_or_else(|| detected_preset.to_string())
                });

                ui.horizontal(|ui| {
                    ui.label("Aspect Ratio:");

                    let mut mode_changed = false;
                    egui::ComboBox::from_id_salt("default_thumbnail_aspect_ratio")
                        .selected_text(&selected_mode)
                        .show_ui(ui, |ui| {
                            for (name, ratio) in &aspect_ratios {
                                if ui
                                    .selectable_value(&mut selected_mode, name.to_string(), *name)
                                    .changed()
                                {
                                    mode_changed = true;
                                    if *ratio > 0.0 {
                                        // Update height based on width and selected ratio
                                        profile.thumbnail_default_height =
                                            (profile.thumbnail_default_width as f32 / ratio).round()
                                                as u16;
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
                    if ui
                        .add(
                            egui::Slider::new(
                                &mut profile.thumbnail_default_width,
                                crate::constants::defaults::thumbnail::MIN_WIDTH
                                    ..=crate::constants::defaults::thumbnail::MAX_WIDTH,
                            )
                            .suffix(" px"),
                        )
                        .changed()
                    {
                        // If not custom, maintain aspect ratio
                        if selected_mode != "Custom" {
                            for (name, ratio) in &aspect_ratios[..aspect_ratios.len() - 1] {
                                if name == &selected_mode.as_str() {
                                    profile.thumbnail_default_height =
                                        (profile.thumbnail_default_width as f32 / ratio).round()
                                            as u16;
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
                        if ui
                            .add(
                                egui::Slider::new(
                                    &mut profile.thumbnail_default_height,
                                    crate::constants::defaults::thumbnail::MIN_HEIGHT
                                        ..=crate::constants::defaults::thumbnail::MAX_HEIGHT,
                                )
                                .suffix(" px"),
                            )
                            .changed()
                        {
                            changed = true;
                        }
                    } else {
                        ui.add_enabled(
                            false,
                            egui::Slider::new(
                                &mut profile.thumbnail_default_height,
                                crate::constants::defaults::thumbnail::MIN_HEIGHT
                                    ..=crate::constants::defaults::thumbnail::MAX_HEIGHT,
                            )
                            .suffix(" px"),
                        );
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

                ui.label(
                    egui::RichText::new("Default size for newly created character thumbnails")
                        .small()
                        .weak(),
                );
            });

            ui.add_space(SECTION_SPACING);

            // Thumbnail Size Adjustment Group
            ui.group(|ui| {
                ui.set_min_width(ui.available_width());
                ui.label(egui::RichText::new("Thumbnail Size Adjustment").strong());
                ui.add_space(ITEM_SPACING);

                // Target selector
                let id = ui.make_persistent_id("thumbnail_resize_target");
                let mut selected_target = ui.data_mut(|d| {
                    d.get_temp::<String>(id)
                        .unwrap_or_else(|| "---".to_string())
                });

                ui.horizontal(|ui| {
                    ui.label("Resize:");

                    // Use character count in salt to force refresh when characters are added
                    egui::ComboBox::from_id_salt(format!(
                        "thumbnail_resize_target_{}",
                        profile.character_thumbnails.len()
                    ))
                    .selected_text(&selected_target)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut selected_target, "---".to_string(), "---");
                        ui.selectable_value(
                            &mut selected_target,
                            "All Characters".to_string(),
                            "All Characters",
                        );
                        ui.separator();

                        // Add individual characters
                        let mut char_names: Vec<_> =
                            profile.character_thumbnails.keys().cloned().collect();
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
                        profile
                            .character_thumbnails
                            .values()
                            .next()
                            .map(|s| (s.dimensions.width, s.dimensions.height))
                            .unwrap_or((250, 141))
                    } else {
                        // Get specific character's dimensions
                        profile
                            .character_thumbnails
                            .get(&selected_target)
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
                    for (name, ratio) in &aspect_ratios[..aspect_ratios.len() - 1] {
                        if (current_ratio - ratio).abs() < 0.01 {
                            preset = name;
                            break;
                        }
                    }
                    preset
                };

                let mode_id =
                    ui.make_persistent_id(format!("resize_aspect_mode_{}", selected_target));
                let mut aspect_mode = ui.data_mut(|d| {
                    d.get_temp::<String>(mode_id)
                        .unwrap_or_else(|| detected_preset.to_string())
                });

                ui.add_enabled_ui(is_enabled, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Aspect Ratio:");

                        let mut mode_changed = false;
                        egui::ComboBox::from_id_salt(format!("resize_aspect_{}", selected_target))
                            .selected_text(&aspect_mode)
                            .show_ui(ui, |ui| {
                                for (name, ratio) in &aspect_ratios {
                                    if ui
                                        .selectable_value(&mut aspect_mode, name.to_string(), *name)
                                        .changed()
                                    {
                                        mode_changed = true;
                                        if *ratio > 0.0 {
                                            state.current_height =
                                                (state.current_width as f32 / ratio).round() as u16;
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
                        if ui
                            .add(
                                egui::Slider::new(
                                    &mut state.current_width,
                                    crate::constants::defaults::thumbnail::MIN_WIDTH
                                        ..=crate::constants::defaults::thumbnail::MAX_WIDTH,
                                )
                                .suffix(" px"),
                            )
                            .changed()
                        {
                            // Maintain aspect ratio if not custom
                            if aspect_mode != "Custom" {
                                for (name, ratio) in &aspect_ratios[..aspect_ratios.len() - 1] {
                                    if name == &aspect_mode.as_str() {
                                        state.current_height =
                                            (state.current_width as f32 / ratio).round() as u16;
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
                            ui.add(
                                egui::Slider::new(
                                    &mut state.current_height,
                                    crate::constants::defaults::thumbnail::MIN_HEIGHT
                                        ..=crate::constants::defaults::thumbnail::MAX_HEIGHT,
                                )
                                .suffix(" px"),
                            );
                        } else {
                            ui.add_enabled(
                                false,
                                egui::Slider::new(
                                    &mut state.current_height,
                                    crate::constants::defaults::thumbnail::MIN_HEIGHT
                                        ..=crate::constants::defaults::thumbnail::MAX_HEIGHT,
                                )
                                .suffix(" px"),
                            );
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
                            let new_dimensions =
                                Dimensions::new(state.current_width, state.current_height);

                            if selected_target == "All Characters" {
                                // Show confirmation for "All Characters"
                                state.pending_resize_all = Some(new_dimensions);
                                state.show_resize_confirmation = true;
                            } else {
                                // Apply to single character immediately
                                if let Some(char_settings) =
                                    profile.character_thumbnails.get_mut(&selected_target)
                                {
                                    char_settings.dimensions = new_dimensions;
                                    changed = true;
                                }
                            }
                        }
                    });
                });
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
                    ui.label(
                        egui::RichText::new("This will overwrite all individual thumbnail sizes.")
                            .small()
                            .weak(),
                    );
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

/// Parse hex color string - supports both #RRGGBB and #AARRGGBB formats.
/// Returns a Color32 if parsing succeeds, treating 6-digit hex as full-opacity RGB.
fn parse_hex_color(hex: &str) -> Result<egui::Color32, ()> {
    let hex = hex.trim_start_matches('#');

    match hex.len() {
        6 => {
            // RGB format - assume full opacity
            let rr = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ())?;
            let gg = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ())?;
            let bb = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ())?;
            Ok(egui::Color32::from_rgba_unmultiplied(rr, gg, bb, 255))
        }
        8 => {
            // ARGB format
            let aa = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ())?;
            let rr = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ())?;
            let gg = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ())?;
            let bb = u8::from_str_radix(&hex[6..8], 16).map_err(|_| ())?;
            Ok(egui::Color32::from_rgba_unmultiplied(rr, gg, bb, aa))
        }
        _ => Err(()),
    }
}

/// Format egui Color32 to hex string (#AARRGGBB or #RRGGBB)
fn format_hex_color(color: egui::Color32) -> String {
    if color.a() == 255 {
        // Full opacity - use shorter RGB format
        format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
    } else {
        // Has transparency - use ARGB format
        format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            color.a(),
            color.r(),
            color.g(),
            color.b()
        )
    }
}
