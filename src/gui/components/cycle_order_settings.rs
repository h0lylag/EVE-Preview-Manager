//! Character cycle order settings component

use eframe::egui;
use crate::config::profile::Profile;
use crate::constants::gui::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorMode {
    TextEdit,
    DragDrop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewTab {
    CycleGroup,
    PerCharacterHotkeys,
}

/// State for cycle order settings UI
pub struct CycleOrderSettingsState {
    cycle_group_text: String,
    editor_mode: EditorMode,
    show_add_characters_popup: bool,
    character_selections: std::collections::HashMap<String, bool>,
    active_tab: ViewTab,
}

impl CycleOrderSettingsState {
    pub fn new() -> Self {
        Self {
            cycle_group_text: String::new(),
            editor_mode: EditorMode::DragDrop,
            show_add_characters_popup: false,
            character_selections: std::collections::HashMap::new(),
            active_tab: ViewTab::CycleGroup,
        }
    }

    /// Load cycle group from profile into text buffer
    pub fn load_from_profile(&mut self, profile: &Profile) {
        self.cycle_group_text = profile.hotkey_cycle_group.join("\n");
    }

    /// Parse text buffer back into profile's cycle group
    fn save_to_profile(&self, profile: &mut Profile) {
        profile.hotkey_cycle_group = self.cycle_group_text
            .lines()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
    }
}

impl Default for CycleOrderSettingsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders cycle order settings UI and returns true if changes were made
/// hotkey_state is optional and only needed for per-character hotkeys tab
pub fn ui(
    ui: &mut egui::Ui, 
    profile: &mut Profile, 
    state: &mut CycleOrderSettingsState,
    hotkey_state: Option<&mut crate::gui::components::hotkey_settings::HotkeySettingsState>
) -> bool {
    let mut changed = false;

    ui.group(|ui| {
        // Tab selector buttons
        ui.horizontal(|ui| {
            ui.selectable_value(&mut state.active_tab, ViewTab::CycleGroup, "Cycle Group");
            ui.selectable_value(&mut state.active_tab, ViewTab::PerCharacterHotkeys, "Per-Character Hotkeys");
        });
        
        ui.add_space(ITEM_SPACING);
        ui.separator();
        ui.add_space(ITEM_SPACING);

        // Show content based on active tab
        match state.active_tab {
            ViewTab::CycleGroup => {
                render_cycle_group_tab(ui, profile, state, &mut changed);
            }
            ViewTab::PerCharacterHotkeys => {
                render_per_character_hotkeys_tab(ui, profile, hotkey_state, &mut changed);
            }
        }
    });

    changed
}

/// Renders the cycle group order tab
fn render_cycle_group_tab(ui: &mut egui::Ui, profile: &mut Profile, state: &mut CycleOrderSettingsState, changed: &mut bool) {
    ui.label(egui::RichText::new("Character Cycle Order").strong());
    ui.add_space(ITEM_SPACING);

        // Mode selector
        ui.horizontal(|ui| {
            ui.label("Editor Mode:");

            egui::ComboBox::from_id_salt("cycle_editor_mode")
                .selected_text(match state.editor_mode {
                    EditorMode::TextEdit => "Text Editor",
                    EditorMode::DragDrop => "Drag and Drop",
                })
                .show_ui(ui, |ui| {
                    if ui.selectable_value(&mut state.editor_mode, EditorMode::TextEdit, "Text Editor").clicked() {
                        // When switching to text mode, sync from profile
                        state.load_from_profile(profile);
                    }
                    if ui.selectable_value(&mut state.editor_mode, EditorMode::DragDrop, "Drag and Drop").clicked() {
                        // When switching to drag mode, sync text to profile first
                        state.save_to_profile(profile);
                    }
                });

            // Add button to import active characters
            if ui.button("➕ Add").clicked() {
                state.show_add_characters_popup = true;
                // Initialize selections for all available characters (unchecked by default)
                state.character_selections.clear();
                for char_name in profile.character_thumbnails.keys() {
                    state.character_selections.insert(char_name.clone(), false);
                }
            }
        });

        ui.add_space(ITEM_SPACING);

        match state.editor_mode {
            EditorMode::TextEdit => {
                ui.label("Enter character names (one per line, in cycle order):");

                ui.add_space(ITEM_SPACING / 2.0);

                // Multi-line text editor for cycle group
                let text_edit = egui::TextEdit::multiline(&mut state.cycle_group_text)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .hint_text("Character Name 1\nCharacter Name 2\nCharacter Name 3");

                if ui.add(text_edit).changed() {
                    // Update profile's cycle_group on every change
                    state.save_to_profile(profile);
                    *changed = true;
                }
            }

            EditorMode::DragDrop => {
                ui.label("Drag items to reorder:");

                ui.add_space(ITEM_SPACING / 2.0);

                // Track drag-drop operations
                let mut from_idx = None;
                let mut to_idx = None;
                let mut to_delete = None;

                let frame = egui::Frame::default()
                    .inner_margin(4.0)
                    .stroke(ui.visuals().widgets.noninteractive.bg_stroke);

                // Drag-drop zone containing all items
                let (_, dropped_payload) = ui.dnd_drop_zone::<usize, ()>(frame, |ui| {
                    ui.set_min_height(100.0);

                    for (row_idx, character) in profile.hotkey_cycle_group.iter().enumerate() {
                        let item_id = egui::Id::new("cycle_character").with(row_idx);

                        // Make entire row draggable
                        let response = ui.dnd_drag_source(item_id, row_idx, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("☰").weak());
                                ui.label(character);

                                // Spacer to make row full width and fully draggable
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(" ");
                                });
                            });
                        }).response;

                        // Add separator line between items
                        if row_idx < profile.hotkey_cycle_group.len() - 1 {
                            ui.separator();
                        }

                        // Detect drops onto this item for insertion preview
                        if let (Some(pointer), Some(hovered_payload)) = (
                            ui.input(|i| i.pointer.interact_pos()),
                            response.dnd_hover_payload::<usize>(),
                        ) {
                            let rect = response.rect;
                            let stroke = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);

                            let insert_row_idx = if *hovered_payload == row_idx {
                                // Dragged onto ourselves - show line at current position
                                ui.painter().hline(rect.x_range(), rect.center().y, stroke);
                                row_idx
                            } else if pointer.y < rect.center().y {
                                // Above this item
                                ui.painter().hline(rect.x_range(), rect.top(), stroke);
                                row_idx
                            } else {
                                // Below this item
                                ui.painter().hline(rect.x_range(), rect.bottom(), stroke);
                                row_idx + 1
                            };

                            if let Some(dragged_payload) = response.dnd_release_payload::<usize>() {
                                // Item was dropped here
                                from_idx = Some(*dragged_payload);
                                to_idx = Some(insert_row_idx);
                                *changed = true;
                            }
                        }

                        // Delete button on right-click (keep context menu as alternative)
                        response.context_menu(|ui| {
                            if ui.button("🗑 Delete").clicked() {
                                to_delete = Some(row_idx);
                                *changed = true;
                                ui.close();
                            }
                        });
                    }
                });

                // Handle drop onto empty area (append to end)
                if let Some(dragged_payload) = dropped_payload {
                    from_idx = Some(*dragged_payload);
                    to_idx = Some(profile.hotkey_cycle_group.len());
                    *changed = true;
                }

                // Perform deletion
                if let Some(idx) = to_delete {
                    profile.hotkey_cycle_group.remove(idx);
                }

                // Perform reordering
                if let (Some(from), Some(mut to)) = (from_idx, to_idx) {
                    // Adjust target index if moving within same list
                    if from < to {
                        to -= 1;
                    }

                    if from != to {
                        let item = profile.hotkey_cycle_group.remove(from);
                        let insert_idx = to.min(profile.hotkey_cycle_group.len());
                        profile.hotkey_cycle_group.insert(insert_idx, item);
                    }
                }
            }
        }

        ui.add_space(ITEM_SPACING / 2.0);

        ui.label(egui::RichText::new(
            format!("Current cycle order: {} character(s)", profile.hotkey_cycle_group.len()))
            .small()
            .weak());
}

/// Renders the per-character hotkeys tab
fn render_per_character_hotkeys_tab(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    hotkey_state: Option<&mut crate::gui::components::hotkey_settings::HotkeySettingsState>,
    changed: &mut bool
) {
    ui.label(egui::RichText::new("Per-Character Hotkeys").strong());
    ui.add_space(ITEM_SPACING);

    ui.label(egui::RichText::new(
        "Assign unique hotkeys to jump directly to specific characters. Drag to reorder.")
        .small()
        .weak());

    ui.add_space(ITEM_SPACING);

    // Get character names - use custom order if available, otherwise alphabetical
    let all_char_names: std::collections::HashSet<String> = profile.character_thumbnails.keys().cloned().collect();
    
    // Build ordered list: first use saved order (filtering out removed chars), then add any new chars alphabetically
    let mut char_names: Vec<String> = profile.character_hotkey_order.iter()
        .filter(|name| all_char_names.contains(*name))
        .cloned()
        .collect();
    
    // Add any new characters not in the order list
    let mut new_chars: Vec<String> = all_char_names.iter()
        .filter(|name| !char_names.contains(name))
        .cloned()
        .collect();
    new_chars.sort();
    char_names.extend(new_chars);

    // Update the order list if it changed (new chars added or removed chars filtered)
    if char_names != profile.character_hotkey_order {
        profile.character_hotkey_order = char_names.clone();
        *changed = true;
    }

    if char_names.is_empty() {
        ui.label(egui::RichText::new("No characters configured yet")
            .weak()
            .italics());
    } else if let Some(hotkey_state) = hotkey_state {
        let mut from_idx: Option<usize> = None;
        let mut to_idx: Option<usize> = None;

        let frame = egui::Frame::default()
            .inner_margin(4.0)
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke);

        // Drag-drop zone containing all items
        let (_, dropped_payload) = ui.dnd_drop_zone::<usize, ()>(frame, |ui| {
            ui.set_min_height(100.0);

            for (idx, char_name) in char_names.iter().enumerate() {
                let item_id = egui::Id::new("per_char_hotkey").with(idx);

                // Get binding info
                let has_binding = profile.character_hotkeys.get(char_name).is_some();
                let binding_text = if let Some(binding) = profile.character_hotkeys.get(char_name) {
                    binding.display_name()
                } else {
                    "Not Set".to_string()
                };

                // Build the row with draggable handle
                let response = ui.horizontal(|ui| {
                    // Make only the drag handle draggable
                    let drag_handle = ui.dnd_drag_source(item_id, idx, |ui| {
                        ui.label(egui::RichText::new("☰").weak());
                    }).response;
                    
                    ui.label(egui::RichText::new(char_name).strong());
                    
                    ui.add_space(ITEM_SPACING);

                    // Show current binding
                    let color = if !has_binding {
                        egui::Color32::from_rgb(150, 150, 150)
                    } else {
                        ui.style().visuals.text_color()
                    };
                    ui.label(egui::RichText::new(&binding_text).color(color));

                    // Buttons on the right
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Clear button if bound
                        if has_binding {
                            if ui.button("✖").clicked() {
                                profile.character_hotkeys.remove(char_name);
                                *changed = true;
                            }
                        }

                        // Bind/Change button
                        let button_text = if has_binding {
                            "🔄 Change"
                        } else {
                            "🎹 Bind Key"
                        };

                        if ui.button(button_text).clicked() {
                            hotkey_state.start_key_capture_for_character(char_name.clone());
                        }
                    });
                    
                    drag_handle
                }).inner;

                // Add separator line between items
                if idx < char_names.len() - 1 {
                    ui.separator();
                }

                // Detect drops onto this item for insertion preview
                if let (Some(pointer), Some(hovered_payload)) = (
                    ui.input(|i| i.pointer.interact_pos()),
                    response.dnd_hover_payload::<usize>(),
                ) {
                    let rect = response.rect;
                    let stroke = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);

                    let insert_idx = if *hovered_payload == idx {
                        // Dragged onto ourselves - show line at current position
                        ui.painter().hline(rect.x_range(), rect.center().y, stroke);
                        idx
                    } else if pointer.y < rect.center().y {
                        // Above this item
                        ui.painter().hline(rect.x_range(), rect.top(), stroke);
                        idx
                    } else {
                        // Below this item
                        ui.painter().hline(rect.x_range(), rect.bottom(), stroke);
                        idx + 1
                    };

                    if let Some(dragged_payload) = response.dnd_release_payload::<usize>() {
                        // Item was dropped here
                        from_idx = Some(*dragged_payload);
                        to_idx = Some(insert_idx);
                        *changed = true;
                    }
                }
            }
        });

        // Handle drop onto empty area (append to end)
        if let Some(dragged_payload) = dropped_payload {
            from_idx = Some(*dragged_payload);
            to_idx = Some(char_names.len());
            *changed = true;
        }

        // Perform reordering if drag completed
        if let (Some(from), Some(to)) = (from_idx, to_idx) {
            if from != to && from < char_names.len() {
                let char_to_move = char_names.remove(from);
                let insert_pos = if to > from { to - 1 } else { to };
                char_names.insert(insert_pos, char_to_move);
                profile.character_hotkey_order = char_names;
            }
        }
    } else {
        ui.label(egui::RichText::new("Hotkey capture not available")
            .weak()
            .italics());
    }
}

fn handle_add_characters_popup(
    ctx: &egui::Context,
    profile: &mut Profile,
    state: &mut CycleOrderSettingsState,
    changed: &mut bool
) {
    egui::Window::new("Add Characters")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
                ui.label("Select characters to add to cycle order:");
                ui.add_space(ITEM_SPACING / 2.0);

                // Select All / None toggle
                ui.horizontal(|ui| {
                    let all_selected = state.character_selections.values().all(|&v| v);
                    let any_selected = state.character_selections.values().any(|&v| v);

                    if ui.button(if all_selected { "Deselect All" } else { "Select All" }).clicked() {
                        let new_state = !all_selected;
                        for selected in state.character_selections.values_mut() {
                            *selected = new_state;
                        }
                    }

                    if any_selected {
                        ui.label(format!("({} selected)", state.character_selections.values().filter(|&&v| v).count()));
                    }
                });

                ui.add_space(ITEM_SPACING / 2.0);
                ui.separator();
                ui.add_space(ITEM_SPACING / 2.0);

                // Scrollable list of checkboxes
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        // Sort character names for consistent display
                        let mut char_names: Vec<_> = state.character_selections.keys().cloned().collect();
                        char_names.sort();

                        for char_name in char_names {
                            if let Some(selected) = state.character_selections.get_mut(&char_name) {
                                // Show if already in cycle group
                                let already_in_cycle = profile.hotkey_cycle_group.contains(&char_name);
                                let label = if already_in_cycle {
                                    format!("{} (already in cycle)", char_name)
                                } else {
                                    char_name.clone()
                                };

                                ui.checkbox(selected, label);
                            }
                        }
                    });

                ui.add_space(ITEM_SPACING);
                ui.separator();

                // OK and Cancel buttons
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        // Add selected characters that aren't already in cycle group
                        for (char_name, selected) in &state.character_selections {
                            if *selected && !profile.hotkey_cycle_group.contains(char_name) {
                                profile.hotkey_cycle_group.push(char_name.clone());
                                *changed = true;
                            }
                        }

                        // Update text buffer if in text mode
                        if state.editor_mode == EditorMode::TextEdit {
                            state.load_from_profile(profile);
                        }

                        state.show_add_characters_popup = false;
                    }

                    if ui.button("Cancel").clicked() {
                        state.show_add_characters_popup = false;
                    }
                });
            });
}
