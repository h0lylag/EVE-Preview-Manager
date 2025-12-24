//! Character cycle order settings component

use crate::config::profile::Profile;
use crate::constants::gui::*;
use eframe::egui;

/// State for character management UI
pub struct CharactersState {
    show_add_characters_popup: bool,
    character_selections: std::collections::HashMap<String, bool>,
    // Track expanded rows for editing: character_name -> expanded
    expanded_rows: std::collections::HashMap<String, bool>,
    // Cache for saving override values when they are temporarily disabled
    cached_overrides: std::collections::HashMap<String, CachedOverrides>,
    // Index of the currently selected cycle group
    selected_cycle_group_index: usize,
    // If Some(idx), we are currently renaming the group at this index
    renaming_group_idx: Option<usize>,
    // Temporary string for renaming
    rename_buffer: String,
}

#[derive(Debug, Default, Clone)]
struct CachedOverrides {
    active_border_color: Option<String>,
    inactive_border_color: Option<String>,
    active_border_size: Option<u16>,
    inactive_border_size: Option<u16>,
    text_color: Option<String>,
}

impl CharactersState {
    pub fn new() -> Self {
        Self {
            show_add_characters_popup: false,
            character_selections: std::collections::HashMap::new(),
            expanded_rows: std::collections::HashMap::new(),
            cached_overrides: std::collections::HashMap::new(),
            selected_cycle_group_index: 0,
            renaming_group_idx: None,
            rename_buffer: String::new(),
        }
    }

    /// Load cycle group from profile - no-op for now as we read directly
    pub fn load_from_profile(&mut self, _profile: &Profile) {
        self.cached_overrides.clear();
    }
}

impl Default for CharactersState {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders cycle order settings UI with integrated per-character hotkeys
/// Returns true if changes were made
pub fn ui(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    state: &mut CharactersState,
    hotkey_state: &mut crate::gui::components::hotkey_settings::HotkeySettingsState,
) -> bool {
    let mut changed = false;

    // Ensure selected index is valid
    if state.selected_cycle_group_index >= profile.cycle_groups.len() {
        state.selected_cycle_group_index = 0;
    }

    render_two_column_layout(ui, profile, state, hotkey_state, &mut changed);

    if state.show_add_characters_popup {
        render_add_characters_modal(ui.ctx(), profile, state, &mut changed);
    }

    // Call shared modal rendering logic if dialog is active
    if hotkey_state.is_dialog_open() {
        changed |= crate::gui::components::hotkey_settings::render_key_capture_modal(
            ui,
            profile,
            hotkey_state,
        );
    }

    changed
}

fn render_two_column_layout(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    state: &mut CharactersState,
    hotkey_state: &mut crate::gui::components::hotkey_settings::HotkeySettingsState,
    changed: &mut bool,
) {
    // Two columns: Left (Editor) 60%, Right (Cycle Group) 40%
    let available_width = ui.available_width();
    let left_width = (available_width * 0.55).max(300.0);

    ui.horizontal_top(|ui| {
        // --- Left Column: Character Editor ---
        ui.allocate_ui(egui::vec2(left_width, ui.available_height()), |ui| {
            ui.vertical(|ui| {
                render_character_editor_column(ui, profile, state, hotkey_state, changed);
            });
        });

        ui.add_space(ITEM_SPACING);
        ui.separator();
        ui.add_space(ITEM_SPACING);

        // --- Right Column: Cycle Group ---
        // Using remaining width for right column
        ui.allocate_ui(
            egui::vec2(ui.available_width(), ui.available_height()),
            |ui| {
                ui.vertical(|ui| {
                    render_cycle_group_column(ui, profile, state, hotkey_state, changed);
                });
            },
        );
    });
}

struct ThemeDefaults {
    active_border_color: String,
    active_border_size: u16,
    inactive_border_color: String,
    inactive_border_size: u16,
    text_color: String,
}

fn render_character_editor_column(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    state: &mut CharactersState,
    hotkey_state: &mut crate::gui::components::hotkey_settings::HotkeySettingsState,
    changed: &mut bool,
) {
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

            for character in char_names {
                // Ensure CharacterSettings entry exists (it should, since we pulled keys from it,
                // but good for safety if we change source later)
                let settings = profile
                    .character_thumbnails
                    .entry(character.clone())
                    .or_insert_with(|| crate::types::CharacterSettings::new(0, 0, 0, 0));

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

                    // Add padding on the right edge
                    ui.add_space(20.0);
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
                                    if let Some(binding) = profile.character_hotkeys.get(&character)
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

                                    let bind_text = if hotkey_state.is_capturing_for(&character) {
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
                }
                ui.add_space(4.0); // Small space between items instead of large ITEM_SPACING
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
}

fn render_cycle_group_column(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    state: &mut CharactersState,
    hotkey_state: &mut crate::gui::components::hotkey_settings::HotkeySettingsState,
    changed: &mut bool,
) {
    // Header Row with Cycle Group Selector
    ui.horizontal(|ui| {
        ui.heading("Cycle Group");
    });
    ui.add_space(ITEM_SPACING);

    // Group Selector & Management
    ui.horizontal(|ui| {
        // Validation: Ensure at least one group (done in profile.rs, but safe check)
        if profile.cycle_groups.is_empty() {
            profile
                .cycle_groups
                .push(crate::config::profile::CycleGroup::default_group());
            *changed = true;
        }

        // Renaming Logic
        if let Some(idx) = state.renaming_group_idx {
            if idx < profile.cycle_groups.len() {
                let text_edit =
                    egui::TextEdit::singleline(&mut state.rename_buffer).desired_width(120.0);
                let response = ui.add(text_edit);

                if response.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    profile.cycle_groups[idx].name = state.rename_buffer.clone();
                    state.renaming_group_idx = None;
                    *changed = true;
                } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    state.renaming_group_idx = None;
                }

                // Keep focus
                if response.changed() {
                    // buffer updated
                }
                if !response.has_focus() && state.renaming_group_idx.is_some() {
                    response.request_focus();
                }
            } else {
                state.renaming_group_idx = None;
            }
        } else {
            // ComboBox Selector
            egui::ComboBox::from_id_salt("cycle_group_selector")
                .width(140.0)
                .selected_text(&profile.cycle_groups[state.selected_cycle_group_index].name)
                .show_ui(ui, |ui| {
                    for (idx, group) in profile.cycle_groups.iter().enumerate() {
                        if ui
                            .selectable_value(
                                &mut state.selected_cycle_group_index,
                                idx,
                                &group.name,
                            )
                            .clicked()
                        {
                            // Handle selection change if needed
                        }
                    }
                });

            // Rename Button
            if ui.small_button("✏").on_hover_text("Rename Group").clicked() {
                state.renaming_group_idx = Some(state.selected_cycle_group_index);
                state.rename_buffer = profile.cycle_groups[state.selected_cycle_group_index]
                    .name
                    .clone();
            }

            ui.add_space(8.0);

            // New Button
            if ui
                .button("➕ New")
                .on_hover_text("Create New Group")
                .clicked()
            {
                let mut new_group = crate::config::profile::CycleGroup::default_group();
                // Generate unique name
                let mut counter = 1;
                let mut name = "New Group".to_string();
                while profile.cycle_groups.iter().any(|g| g.name == name) {
                    counter += 1;
                    name = format!("New Group {}", counter);
                }
                new_group.name = name;
                profile.cycle_groups.push(new_group);
                state.selected_cycle_group_index = profile.cycle_groups.len() - 1;
                *changed = true;
            }

            // Duplicate Button
            if ui
                .button("📄 Copy")
                .on_hover_text("Duplicate Group")
                .clicked()
            {
                let mut new_group = profile.cycle_groups[state.selected_cycle_group_index].clone();
                new_group.name = format!("{} (Copy)", new_group.name);
                profile.cycle_groups.push(new_group);
                state.selected_cycle_group_index = profile.cycle_groups.len() - 1;
                *changed = true;
            }

            // Delete Button
            ui.add_enabled_ui(profile.cycle_groups.len() > 1, |ui| {
                if ui
                    .button("🗑 Delete")
                    .on_hover_text("Delete Group")
                    .clicked()
                {
                    profile
                        .cycle_groups
                        .remove(state.selected_cycle_group_index);
                    // Clamp index
                    if state.selected_cycle_group_index >= profile.cycle_groups.len() {
                        state.selected_cycle_group_index =
                            profile.cycle_groups.len().saturating_sub(1);
                    }
                    *changed = true;
                }
            });
        }
    });

    ui.add_space(ITEM_SPACING);
    ui.separator();
    ui.add_space(ITEM_SPACING);

    // Cycle Hotkeys for this Group
    let current_group = &mut profile.cycle_groups[state.selected_cycle_group_index];

    ui.label(egui::RichText::new("Group Hotkeys").strong());

    ui.horizontal(|ui| {
        // Forward
        ui.label("Forward:");

        // Logic copied/adapted from hotkey_settings.rs
        if let Some(binding) = &current_group.hotkey_forward {
            ui.label(egui::RichText::new(binding.display_name()).strong());
        } else {
            ui.label(egui::RichText::new("Not set").weak());
        }

        let id_str_fwd = format!("GROUP:{}:FWD", state.selected_cycle_group_index);
        let bind_text_fwd = if hotkey_state.is_capturing_for(&id_str_fwd) {
            "Capturing..."
        } else {
            "⌨ Bind"
        };

        if ui.button(bind_text_fwd).clicked() {
            hotkey_state.start_key_capture_for_character(id_str_fwd, profile.hotkey_backend);
        }

        if current_group.hotkey_forward.is_some() && ui.small_button("✖").clicked() {
            current_group.hotkey_forward = None;
            *changed = true;
        }

        ui.add_space(24.0);

        // Backward
        ui.label("Backward:");

        if let Some(binding) = &current_group.hotkey_backward {
            ui.label(egui::RichText::new(binding.display_name()).strong());
        } else {
            ui.label(egui::RichText::new("Not set").weak());
        }

        let id_str_bwd = format!("GROUP:{}:BWD", state.selected_cycle_group_index);
        let bind_text_bwd = if hotkey_state.is_capturing_for(&id_str_bwd) {
            "Capturing..."
        } else {
            "⌨ Bind"
        };

        if ui.button(bind_text_bwd).clicked() {
            hotkey_state.start_key_capture_for_character(id_str_bwd, profile.hotkey_backend);
        }

        if current_group.hotkey_backward.is_some() && ui.small_button("✖").clicked() {
            current_group.hotkey_backward = None;
            *changed = true;
        }
    });

    ui.add_space(ITEM_SPACING);
    ui.separator();
    ui.add_space(ITEM_SPACING);

    // Character List Header
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Characters").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("➕ Add Chars").clicked() {
                state.show_add_characters_popup = true;
                // Initialize selections
                state.character_selections.clear();
                for char_name in profile.character_thumbnails.keys() {
                    state.character_selections.insert(char_name.clone(), false);
                }
            }
        });
    });

    // Draggable List (modified to use selected group)
    let current_group = &mut profile.cycle_groups[state.selected_cycle_group_index];

    egui::ScrollArea::vertical()
        .id_salt("cycle_group_scroll")
        .show(ui, |ui| {
            let mut from_idx = None;
            let mut to_idx = None;
            let mut to_delete = None;

            let frame = egui::Frame::default()
                .inner_margin(4.0)
                .stroke(ui.visuals().widgets.noninteractive.bg_stroke);

            let (_, dropped_payload) = ui.dnd_drop_zone::<usize, ()>(frame, |ui| {
                ui.set_min_height(100.0);

                for (row_idx, character) in current_group.characters.iter().enumerate() {
                    let item_id = egui::Id::new("cycle_group_item").with(row_idx);

                    let response = ui
                        .horizontal(|ui| {
                            let drag_response = ui
                                .dnd_drag_source(item_id, row_idx, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new("::").weak());

                                        ui.label(character);
                                    });
                                })
                                .response;

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .small_button("✖")
                                        .on_hover_text("Remove from cycle group")
                                        .clicked()
                                    {
                                        to_delete = Some(row_idx);
                                        *changed = true;
                                    }
                                },
                            );
                            drag_response
                        })
                        .inner;

                    // Detect drops
                    if let (Some(pointer), Some(hovered_payload)) = (
                        ui.input(|i| i.pointer.interact_pos()),
                        response.dnd_hover_payload::<usize>(),
                    ) {
                        let rect = response.rect;
                        let stroke = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);

                        let insert_row_idx = if *hovered_payload == row_idx {
                            ui.painter().hline(rect.x_range(), rect.center().y, stroke);
                            row_idx
                        } else if pointer.y < rect.center().y {
                            ui.painter().hline(rect.x_range(), rect.top(), stroke);
                            row_idx
                        } else {
                            ui.painter().hline(rect.x_range(), rect.bottom(), stroke);
                            row_idx + 1
                        };

                        if let Some(dragged_payload) = response.dnd_release_payload::<usize>() {
                            from_idx = Some(*dragged_payload);
                            to_idx = Some(insert_row_idx);
                            *changed = true;
                        }
                    }
                }
            });

            if let Some(dragged_payload) = dropped_payload {
                from_idx = Some(*dragged_payload);
                to_idx = Some(current_group.characters.len());
                *changed = true;
            }

            if let Some(idx) = to_delete {
                // Re-borrow to satisfy borrow checker if needed, but current_group is mutable borrow of profile
                // We need to access profile.cycle_groups again? No, current_group IS the reference.
                current_group.characters.remove(idx);
            }

            if let (Some(from), Some(mut to)) = (from_idx, to_idx) {
                if from < to {
                    to -= 1;
                }
                if from != to {
                    let item = current_group.characters.remove(from);
                    let insert_idx = to.min(current_group.characters.len());
                    current_group.characters.insert(insert_idx, item);
                }
            }

            if current_group.characters.is_empty() {
                ui.label(egui::RichText::new("No characters in this group.").weak());
            }
        });
}

fn render_overrides_section(
    ui: &mut egui::Ui,
    character_name: &str,
    settings: &mut crate::types::CharacterSettings,
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
                    if let Ok(mut color) = crate::gui::utils::parse_hex_color(&color_str)
                        && ui.color_edit_button_srgba(&mut color).changed()
                    {
                        let new_hex = crate::gui::utils::format_hex_color(color);
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

                    if let Ok(mut color) = crate::gui::utils::parse_hex_color(&color_str)
                        && ui.color_edit_button_srgba(&mut color).changed()
                    {
                        let new_hex = crate::gui::utils::format_hex_color(color);
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

                    if let Ok(mut color) = crate::gui::utils::parse_hex_color(&color_str)
                        && ui.color_edit_button_srgba(&mut color).changed()
                    {
                        let new_hex = crate::gui::utils::format_hex_color(color);
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
                crate::types::PreviewMode::Static { .. }
            );

            if ui.checkbox(&mut is_static, "Enabled").changed() {
                if is_static {
                    // Enable Static Mode (Default to Black)
                    settings.preview_mode = crate::types::PreviewMode::Static {
                        color: "#000000".to_string(),
                    };
                } else {
                    // Disable Static Mode (Revert to Live)
                    settings.preview_mode = crate::types::PreviewMode::Live;
                }
                *changed = true;
            }
        });

        // Static Mode Settings (Indented)
        if let crate::types::PreviewMode::Static { ref mut color } = settings.preview_mode {
            ui.indent("static_mode_details", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Color:");
                    let mut color_str = color.clone();
                    let text_edit = egui::TextEdit::singleline(&mut color_str).desired_width(100.0);

                    if ui.add(text_edit).changed() {
                        *color = color_str.clone();
                        *changed = true;
                    }

                    if let Ok(mut c) = crate::gui::utils::parse_hex_color(&color_str)
                        && ui.color_edit_button_srgba(&mut c).changed()
                    {
                        let new_hex = crate::gui::utils::format_hex_color(c);
                        *color = new_hex;
                        *changed = true;
                    }
                });
            });
        }
    });
}

fn render_add_characters_modal(
    ctx: &egui::Context,
    profile: &mut Profile,
    state: &mut CharactersState,
    changed: &mut bool,
) {
    let mut open = true;
    egui::Window::new("Add Characters to Cycle Group")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.set_min_width(300.0);
            ui.label("Select characters to add to cycle order:");
            ui.add_space(ITEM_SPACING / 2.0);

            // Select All / Deselect All toggle
            ui.horizontal(|ui| {
                let all_selected = state.character_selections.values().all(|&v| v);
                let any_selected = state.character_selections.values().any(|&v| v);

                if ui
                    .button(if all_selected {
                        "Deselect All"
                    } else {
                        "Select All"
                    })
                    .clicked()
                {
                    let new_state = !all_selected;
                    for selected in state.character_selections.values_mut() {
                        *selected = new_state;
                    }
                }

                if any_selected {
                    ui.label(format!(
                        "({} selected)",
                        state.character_selections.values().filter(|&&v| v).count()
                    ));
                }
            });

            ui.add_space(ITEM_SPACING / 2.0);
            ui.separator();
            ui.add_space(ITEM_SPACING / 2.0);

            egui::ScrollArea::vertical()
                .max_height(300.0)
                .show(ui, |ui| {
                    // Collect and sort names for stable display
                    let mut char_names: Vec<String> =
                        state.character_selections.keys().cloned().collect();
                    char_names.sort();

                    for name in char_names {
                        if let Some(selected) = state.character_selections.get_mut(&name) {
                            // Show if already in cycle group
                            let current_group =
                                &profile.cycle_groups[state.selected_cycle_group_index];
                            let already_in_cycle = current_group.characters.contains(&name);
                            let label = if already_in_cycle {
                                format!("{} (already in this group)", name)
                            } else {
                                name.clone()
                            };

                            ui.checkbox(selected, label);
                        }
                    }
                });

            ui.add_space(ITEM_SPACING);
            ui.separator();

            ui.horizontal(|ui| {
                if ui.button("Add Selected").clicked() {
                    let mut added_any = false;
                    let current_group = &mut profile.cycle_groups[state.selected_cycle_group_index];

                    for (name, selected) in &state.character_selections {
                        if *selected && !current_group.characters.contains(name) {
                            current_group.characters.push(name.clone());
                            added_any = true;
                        }
                    }

                    if added_any {
                        *changed = true;
                    }
                    state.show_add_characters_popup = false;
                }

                if ui.button("Cancel").clicked() {
                    state.show_add_characters_popup = false;
                }
            });
        });

    if !open {
        state.show_add_characters_popup = false;
    }
}
