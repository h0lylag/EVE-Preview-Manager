//! Character cycle order settings component

use crate::config::profile::Profile;
use crate::constants::gui::*;
use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorMode {
    TextEdit,
    DragDrop,
}

/// State for cycle order settings UI
pub struct CycleOrderSettingsState {
    cycle_group_text: String,
    editor_mode: EditorMode,
    show_add_characters_popup: bool,
    character_selections: std::collections::HashMap<String, bool>,
}

impl CycleOrderSettingsState {
    pub fn new() -> Self {
        Self {
            cycle_group_text: String::new(),
            editor_mode: EditorMode::DragDrop,
            show_add_characters_popup: false,
            character_selections: std::collections::HashMap::new(),
        }
    }

    /// Load cycle group from profile into text buffer with hotkey suffixes
    pub fn load_from_profile(&mut self, profile: &Profile) {
        self.cycle_group_text = profile
            .hotkey_cycle_group
            .iter()
            .map(|char_name| {
                if let Some(binding) = profile.character_hotkeys.get(char_name) {
                    format!("{} [{}]", char_name, binding.display_name())
                } else {
                    char_name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    }

    /// Parse text buffer back into profile's cycle group and character hotkeys
    /// Format: "CharacterName \[HOTKEY\]" or just "CharacterName"
    fn save_to_profile(&self, profile: &mut Profile) {
        profile.hotkey_cycle_group = self
            .cycle_group_text
            .lines()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|line| {
                // Parse "CharacterName [HOTKEY]" format
                if let Some(bracket_pos) = line.rfind('[') {
                    // Extract character name before bracket
                    line[..bracket_pos].trim().to_string()
                } else {
                    line.to_string()
                }
            })
            .filter(|s| !s.is_empty()) // Filter again after extracting name (handles " [HOTKEY]" case)
            .collect();

        // Note: Hotkey bindings are updated through the Bind button, not text parsing
        // Text mode shows hotkeys but doesn't allow editing them
    }
}

impl Default for CycleOrderSettingsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders cycle order settings UI with integrated per-character hotkeys
/// Returns true if changes were made
pub fn ui(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    state: &mut CycleOrderSettingsState,
    hotkey_state: &mut crate::gui::components::hotkey_settings::HotkeySettingsState,
) -> bool {
    let mut changed = false;

    ui.group(|ui| {
        render_unified_cycle_group_tab(ui, profile, state, hotkey_state, &mut changed);
    });

    changed
}

/// Renders the cycle group editor with integrated per-character hotkey bindings
fn render_unified_cycle_group_tab(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    state: &mut CycleOrderSettingsState,
    hotkey_state: &mut crate::gui::components::hotkey_settings::HotkeySettingsState,
    changed: &mut bool,
) {
    ui.label(egui::RichText::new("Character Cycle Order & Hotkeys").strong());
    ui.add_space(ITEM_SPACING);

    // Mode selector
    ui.horizontal(|ui| {
        ui.label("Editor Mode:");

        egui::ComboBox::from_id_salt("cycle_editor_mode")
            .selected_text(match state.editor_mode {
                EditorMode::TextEdit => "Text Editor", // Simple multi-line text area for bulk editing
                EditorMode::DragDrop => "Drag and Drop", // Interactive list for visual reordering
            })
            .show_ui(ui, |ui| {
                if ui
                    .selectable_value(&mut state.editor_mode, EditorMode::TextEdit, "Text Editor")
                    .clicked()
                {
                    // When switching to text mode, sync from profile
                    state.load_from_profile(profile);
                }
                if ui
                    .selectable_value(
                        &mut state.editor_mode,
                        EditorMode::DragDrop,
                        "Drag and Drop",
                    )
                    .clicked()
                {
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

                    // Build row with draggable handle + name, and non-draggable buttons
                    let response = ui
                        .horizontal(|ui| {
                            // Draggable section: handle + character name
                            let drag_response = ui
                                .dnd_drag_source(item_id, row_idx, |ui| {
                                    ui.label(egui::RichText::new("☰").weak());
                                    ui.label(character);
                                })
                                .response;

                            // Right-aligned buttons (not draggable)
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let has_binding =
                                        profile.character_hotkeys.contains_key(character);

                                    // Delete hotkey button (only when binding exists)
                                    if has_binding
                                        && ui
                                            .small_button("✖")
                                            .on_hover_text("Remove hotkey")
                                            .clicked()
                                    {
                                        profile.character_hotkeys.remove(character);
                                        *changed = true;
                                    }

                                    // Show hotkey if assigned, OR show bind button if no hotkey
                                    if let Some(binding) = profile.character_hotkeys.get(character)
                                    {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "[{}]",
                                                binding.display_name()
                                            ))
                                            .weak(),
                                        );
                                    } else {
                                        // Bind button (only when no binding exists)
                                        let bind_text = if hotkey_state.is_capturing_for(character)
                                        {
                                            "Capturing..."
                                        } else {
                                            "Bind"
                                        };

                                        if ui
                                            .small_button(bind_text)
                                            .on_hover_text("Click to capture hotkey")
                                            .clicked()
                                        {
                                            hotkey_state
                                                .start_key_capture_for_character(character.clone());
                                        }
                                    }
                                },
                            );

                            drag_response
                        })
                        .inner;

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

    ui.label(
        egui::RichText::new(format!(
            "Current cycle order: {} character(s)",
            profile.hotkey_cycle_group.len()
        ))
        .small()
        .weak(),
    );
}
