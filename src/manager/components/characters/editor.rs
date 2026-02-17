use super::CharactersState;
use crate::common::constants::manager_ui::*;
use crate::config::profile::Profile;
use crate::manager::components::hotkey_settings::HotkeySettingsState;
use eframe::egui;

pub struct ThemeDefaults {
    pub active_border_color: String,
    pub active_border_size: u16,
    pub inactive_border_color: String,
    pub inactive_border_size: u16,
    pub text_color: String,
}

pub fn render_character_editor_column(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    state: &mut CharactersState,
    hotkey_state: &mut HotkeySettingsState,
    changed: &mut bool,
) {
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        ui.heading("Character Manager");
        ui.label(
            egui::RichText::new("Edit settings for all known characters.")
                .weak()
                .small(),
        );
        ui.add_space(ITEM_SPACING);

        // Capture defaults before mutable borrow of profile
        let defaults = ThemeDefaults {
            active_border_color: profile.thumbnail_active_border_color.clone(),
            active_border_size: profile.thumbnail_active_border_size,
            inactive_border_color: profile.thumbnail_inactive_border_color.clone(),
            inactive_border_size: profile.thumbnail_inactive_border_size,
            text_color: profile.thumbnail_text_color.clone(),
        };

        egui::ScrollArea::vertical()
            .id_salt("char_editor_scroll")
            .show(ui, |ui| {
                // Get all known characters (keys from character_thumbnails)
                let mut char_names: Vec<String> =
                    profile.character_thumbnails.keys().cloned().collect();
                // Case-insensitive sort
                char_names.sort_by_key(|a| a.to_lowercase());
                let mut to_delete = Vec::new();

                for character in char_names {
                    // Ensure CharacterSettings entry exists
                    let settings = profile
                        .character_thumbnails
                        .entry(character.clone())
                        .or_insert_with(|| {
                            crate::common::types::CharacterSettings::new(0, 0, 0, 0)
                        });

                    let is_expanded = *state.expanded_rows.get(&character).unwrap_or(&false);

                    // Minimalist Layout
                    ui.horizontal(|ui| {
                        let icon = if is_expanded { "v" } else { ">" };
                        if ui.small_button(icon).clicked() {
                            state.expanded_rows.insert(character.clone(), !is_expanded);
                        }

                        ui.label(&character);

                        // Show Alias in parentheses
                        if let Some(alias) = &settings.alias
                            && !alias.is_empty()
                        {
                            ui.label(egui::RichText::new(format!("({})", alias)));
                        }

                        // Show Hotkey in brackets
                        if let Some(binding) = profile.character_hotkeys.get(&character) {
                            ui.label(egui::RichText::new(format!("[{}]", binding.display_name())));
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button("🗑")
                                .on_hover_text("Remove Character")
                                .clicked()
                            {
                                to_delete.push(character.clone());
                                *changed = true;
                            }
                        });
                    });

                    if is_expanded {
                        ui.indent("details", |ui| {
                            ui.add_space(4.0);

                            egui::Grid::new(format!("grid_edit_{}", character))
                                .num_columns(2)
                                .spacing([10.0, 4.0])
                                .show(ui, |ui| {
                                    // Alias
                                    ui.label("Alias:");
                                    let mut alias = settings.alias.clone().unwrap_or_default();
                                    if ui
                                        .add(
                                            egui::TextEdit::singleline(&mut alias)
                                                .hint_text("Display Name"),
                                        )
                                        .changed()
                                    {
                                        settings.alias =
                                            if alias.is_empty() { None } else { Some(alias) };
                                        *changed = true;
                                    }
                                    ui.end_row();

                                    // Notes
                                    ui.label("Notes:");
                                    let mut notes = settings.notes.clone().unwrap_or_default();
                                    if ui
                                        .add(
                                            egui::TextEdit::multiline(&mut notes)
                                                .desired_rows(2)
                                                .hint_text("Optional notes..."),
                                        )
                                        .changed()
                                    {
                                        settings.notes =
                                            if notes.is_empty() { None } else { Some(notes) };
                                        *changed = true;
                                    }
                                    ui.end_row();

                                    // Hotkey Binding
                                    ui.label("Hotkey:");
                                    ui.horizontal(|ui| {
                                        if let Some(binding) =
                                            profile.character_hotkeys.get(&character)
                                        {
                                            ui.label(
                                                egui::RichText::new(binding.display_name())
                                                    .strong()
                                                    .color(ui.style().visuals.text_color()),
                                            );
                                        } else {
                                            ui.label(
                                                egui::RichText::new("Not set")
                                                    .strong()
                                                    .color(ui.style().visuals.weak_text_color()),
                                            );
                                        }

                                        let bind_text = if hotkey_state.is_capturing_for(&character)
                                        {
                                            "Capturing..."
                                        } else {
                                            "⌨ Bind"
                                        };

                                        if ui.button(bind_text).clicked() {
                                            hotkey_state.start_key_capture_for_character(
                                                character.clone(),
                                                profile.hotkey_backend,
                                            );
                                        }

                                        if profile.character_hotkeys.contains_key(&character)
                                            && ui
                                                .small_button("✖")
                                                .on_hover_text("Clear binding")
                                                .clicked()
                                        {
                                            profile.character_hotkeys.remove(&character);
                                            *changed = true;
                                        }
                                    });
                                    ui.end_row();

                                    // Overrides Section
                                    render_overrides_section(
                                        ui, &character, settings, &defaults, state, changed,
                                    );
                                });
                            ui.add_space(8.0);
                        });
                        ui.add_space(4.0);
                    }
                }

                // Perform deferred deletion
                for char_to_delete in to_delete {
                    profile.character_thumbnails.remove(&char_to_delete);
                    profile.character_hotkeys.remove(&char_to_delete);
                    for group in &mut profile.cycle_groups {
                        group.cycle_list.retain(|slot| match slot {
                            crate::config::profile::CycleSlot::Eve(name) => name != &char_to_delete,
                            crate::config::profile::CycleSlot::Source(name) => {
                                name != &char_to_delete
                            }
                        });
                    }
                }

                if profile.character_thumbnails.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No characters found.\nLog in to EVE Online clients to populate this list.",
                        )
                        .weak()
                        .italics(),
                    );
                }
            });
        });
}

pub fn render_overrides_section(
    ui: &mut egui::Ui,
    character_name: &str,
    settings: &mut crate::common::types::CharacterSettings,
    defaults: &ThemeDefaults,
    state: &mut CharactersState,
    changed: &mut bool,
) {
    ui.label("Overrides:");
    ui.vertical(|ui| {
        // Active Border
        ui.horizontal(|ui| {
            ui.label("Active Border:");
            let mut active_custom = settings.override_active_border_color.is_some()
                || settings.override_active_border_size.is_some();
            let cached = state
                .cached_overrides
                .entry(character_name.to_string())
                .or_default();

            if ui.checkbox(&mut active_custom, "Enabled").changed() {
                if active_custom {
                    if settings.override_active_border_color.is_none() {
                        settings.override_active_border_color = cached
                            .active_border_color
                            .clone()
                            .or_else(|| Some(defaults.active_border_color.clone()));
                    }
                    if settings.override_active_border_size.is_none() {
                        settings.override_active_border_size = cached
                            .active_border_size
                            .or(Some(defaults.active_border_size));
                    }
                } else {
                    cached.active_border_color = settings.override_active_border_color.clone();
                    cached.active_border_size = settings.override_active_border_size;
                    settings.override_active_border_color = None;
                    settings.override_active_border_size = None;
                }
                *changed = true;
            }
        });

        // Active Border Settings (Indented)
        if settings.override_active_border_color.is_some()
            || settings.override_active_border_size.is_some()
        {
            ui.indent("active_border_details", |ui| {
                // Color
                ui.horizontal(|ui| {
                    ui.label("Color:");
                    let mut color_str = settings
                        .override_active_border_color
                        .clone()
                        .unwrap_or_default();
                    let text_edit = egui::TextEdit::singleline(&mut color_str).desired_width(100.0);

                    if ui.add(text_edit).changed() {
                        settings.override_active_border_color = Some(color_str.clone());
                        *changed = true;
                    }

                    // Color picker button
                    if let Ok(mut color) = crate::manager::utils::parse_hex_color(&color_str)
                        && ui.color_edit_button_srgba(&mut color).changed()
                    {
                        let new_hex = crate::manager::utils::format_hex_color(color);
                        settings.override_active_border_color = Some(new_hex);
                        *changed = true;
                    }
                });

                // Size
                ui.horizontal(|ui| {
                    ui.label("Size:");
                    if let Some(ref mut size) = settings.override_active_border_size
                        && ui.add(egui::DragValue::new(size).range(1..=20)).changed()
                    {
                        *changed = true;
                    }
                });
            });
        }

        // Inactive Border
        ui.horizontal(|ui| {
            ui.label("Inactive Border:");
            let mut inactive_custom = settings.override_inactive_border_color.is_some()
                || settings.override_inactive_border_size.is_some();
            let cached = state
                .cached_overrides
                .entry(character_name.to_string())
                .or_default();

            if ui.checkbox(&mut inactive_custom, "Enabled").changed() {
                if inactive_custom {
                    if settings.override_inactive_border_color.is_none() {
                        settings.override_inactive_border_color = cached
                            .inactive_border_color
                            .clone()
                            .or_else(|| Some(defaults.inactive_border_color.clone()));
                    }
                    if settings.override_inactive_border_size.is_none() {
                        settings.override_inactive_border_size = cached
                            .inactive_border_size
                            .or(Some(defaults.inactive_border_size));
                    }
                } else {
                    cached.inactive_border_color = settings.override_inactive_border_color.clone();
                    cached.inactive_border_size = settings.override_inactive_border_size;
                    settings.override_inactive_border_color = None;
                    settings.override_inactive_border_size = None;
                }
                *changed = true;
            }
        });

        // Inactive Border Settings (Indented)
        if settings.override_inactive_border_color.is_some()
            || settings.override_inactive_border_size.is_some()
        {
            ui.indent("inactive_border_details", |ui| {
                // Color
                ui.horizontal(|ui| {
                    ui.label("Color:");
                    let mut color_str = settings
                        .override_inactive_border_color
                        .clone()
                        .unwrap_or_default();
                    let text_edit = egui::TextEdit::singleline(&mut color_str).desired_width(100.0);

                    if ui.add(text_edit).changed() {
                        settings.override_inactive_border_color = Some(color_str.clone());
                        *changed = true;
                    }

                    if let Ok(mut color) = crate::manager::utils::parse_hex_color(&color_str)
                        && ui.color_edit_button_srgba(&mut color).changed()
                    {
                        let new_hex = crate::manager::utils::format_hex_color(color);
                        settings.override_inactive_border_color = Some(new_hex);
                        *changed = true;
                    }
                });

                // Size
                ui.horizontal(|ui| {
                    ui.label("Size:");
                    if let Some(ref mut size) = settings.override_inactive_border_size
                        && ui.add(egui::DragValue::new(size).range(1..=20)).changed()
                    {
                        *changed = true;
                    }
                });
            });
        }

        // Text Color
        ui.horizontal(|ui| {
            ui.label("Text Color:");
            let mut text_color_enabled = settings.override_text_color.is_some();
            let cached = state
                .cached_overrides
                .entry(character_name.to_string())
                .or_default();

            if ui.checkbox(&mut text_color_enabled, "Enabled").changed() {
                if text_color_enabled {
                    settings.override_text_color = cached
                        .text_color
                        .clone()
                        .or_else(|| Some(defaults.text_color.clone()));
                } else {
                    cached.text_color = settings.override_text_color.clone();
                    settings.override_text_color = None;
                }
                *changed = true;
            }
        });

        // Text Color Settings (Indented)
        if settings.override_text_color.is_some() {
            ui.indent("text_color_details", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Color:");
                    let mut color_str = settings.override_text_color.clone().unwrap_or_default();
                    let text_edit = egui::TextEdit::singleline(&mut color_str).desired_width(100.0);

                    if ui.add(text_edit).changed() {
                        settings.override_text_color = Some(color_str.clone());
                        *changed = true;
                    }

                    if let Ok(mut color) = crate::manager::utils::parse_hex_color(&color_str)
                        && ui.color_edit_button_srgba(&mut color).changed()
                    {
                        let new_hex = crate::manager::utils::format_hex_color(color);
                        settings.override_text_color = Some(new_hex);
                        *changed = true;
                    }
                });
            });
        }

        // Preview Mode (Static Mode)
        ui.horizontal(|ui| {
            ui.label("Static Mode:");
            let mut is_static = matches!(
                settings.preview_mode,
                crate::common::types::PreviewMode::Static { .. }
            );

            if ui.checkbox(&mut is_static, "Enabled").changed() {
                if is_static {
                    // Enable Static Mode (Default to Black)
                    settings.preview_mode = crate::common::types::PreviewMode::Static {
                        color: "#000000".to_string(),
                    };
                } else {
                    // Disable Static Mode (Revert to Live)
                    settings.preview_mode = crate::common::types::PreviewMode::Live;
                }
                *changed = true;
            }
        });

        // Static Mode Settings (Indented)
        if let crate::common::types::PreviewMode::Static { ref mut color } = settings.preview_mode {
            ui.indent("static_mode_details", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Color:");
                    let mut color_str = color.clone();
                    let text_edit = egui::TextEdit::singleline(&mut color_str).desired_width(100.0);

                    if ui.add(text_edit).changed() {
                        *color = color_str.clone();
                        *changed = true;
                    }

                    if let Ok(mut c) = crate::manager::utils::parse_hex_color(&color_str)
                        && ui.color_edit_button_srgba(&mut c).changed()
                    {
                        let new_hex = crate::manager::utils::format_hex_color(c);
                        *color = new_hex;
                        *changed = true;
                    }
                });
            });
        }

        ui.add_space(4.0);

        // Minimize Exemption
        ui.horizontal(|ui| {
            ui.label("Minimize Exemption:");
            let mut is_exempt = settings.exempt_from_minimize;

            if ui.checkbox(&mut is_exempt, "Enabled").changed() {
                settings.exempt_from_minimize = is_exempt;
                *changed = true;
            }
        });

        // Preview Visibility Override
        ui.horizontal(|ui| {
            ui.label("Preview Visibility:");

            // Map Option<bool> to a display label
            let current_label = match settings.override_render_preview {
                None => "Default",
                Some(true) => "Always Show",
                Some(false) => "Always Hide",
            };

            egui::ComboBox::from_id_salt(format!("preview_vis_{}", character_name))
                .selected_text(current_label)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(&mut settings.override_render_preview, None, "Default")
                        .changed()
                    {
                        *changed = true;
                    }
                    if ui
                        .selectable_value(
                            &mut settings.override_render_preview,
                            Some(true),
                            "Always Show",
                        )
                        .changed()
                    {
                        *changed = true;
                    }
                    if ui
                        .selectable_value(
                            &mut settings.override_render_preview,
                            Some(false),
                            "Always Hide",
                        )
                        .changed()
                    {
                        *changed = true;
                    }
                });
        });
    });

    ui.add_space(ITEM_SPACING);
}
