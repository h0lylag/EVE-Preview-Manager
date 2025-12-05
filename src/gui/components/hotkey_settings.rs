//! Hotkey settings component for profile configuration

use eframe::egui;
use crate::config::profile::Profile;
use crate::constants::gui::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorMode {
    TextEdit,
    DragDrop,
}

/// State for hotkey settings UI
pub struct HotkeySettingsState {
    cycle_group_text: String,
    editor_mode: EditorMode,
    show_add_characters_popup: bool,
    character_selections: std::collections::HashMap<String, bool>,
}

impl HotkeySettingsState {
    pub fn new() -> Self {
        Self {
            cycle_group_text: String::new(),
            editor_mode: EditorMode::DragDrop,
            show_add_characters_popup: false,
            character_selections: std::collections::HashMap::new(),
        }
    }

    /// Load cycle group from profile into text buffer
    pub fn load_from_profile(&mut self, profile: &Profile) {
        self.cycle_group_text = profile.cycle_group.join("\n");
    }

    /// Parse text buffer back into profile's cycle group
    fn save_to_profile(&self, profile: &mut Profile) {
        profile.cycle_group = self.cycle_group_text
            .lines()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
    }
}

impl Default for HotkeySettingsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders hotkey settings UI and returns true if changes were made
pub fn ui(ui: &mut egui::Ui, profile: &mut Profile, state: &mut HotkeySettingsState) -> bool {
    let mut changed = false;

    ui.group(|ui| {
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
                for char_name in profile.character_positions.keys() {
                    state.character_selections.insert(char_name.clone(), false);
                }
            }
        });
        
        ui.add_space(ITEM_SPACING);
        
        match state.editor_mode {
            EditorMode::TextEdit => {
                ui.label("Enter character names (one per line, Tab/Shift+Tab to cycle):");
                
                ui.add_space(ITEM_SPACING / 2.0);
                
                // Multi-line text editor for cycle group
                let text_edit = egui::TextEdit::multiline(&mut state.cycle_group_text)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .hint_text("Character Name 1\nCharacter Name 2\nCharacter Name 3");
                
                if ui.add(text_edit).changed() {
                    // Update profile's cycle_group on every change
                    state.save_to_profile(profile);
                    changed = true;
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
                    
                    for (row_idx, character) in profile.cycle_group.iter().enumerate() {
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
                        if row_idx < profile.cycle_group.len() - 1 {
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
                                changed = true;
                            }
                        }
                        
                        // Delete button on right-click (keep context menu as alternative)
                        response.context_menu(|ui| {
                            if ui.button("🗑 Delete").clicked() {
                                to_delete = Some(row_idx);
                                changed = true;
                                ui.close();
                            }
                        });
                    }
                });
                
                // Handle drop onto empty area (append to end)
                if let Some(dragged_payload) = dropped_payload {
                    from_idx = Some(*dragged_payload);
                    to_idx = Some(profile.cycle_group.len());
                    changed = true;
                }
                
                // Perform deletion
                if let Some(idx) = to_delete {
                    profile.cycle_group.remove(idx);
                }
                
                // Perform reordering
                if let (Some(from), Some(mut to)) = (from_idx, to_idx) {
                    // Adjust target index if moving within same list
                    if from < to {
                        to -= 1;
                    }
                    
                    if from != to {
                        let item = profile.cycle_group.remove(from);
                        let insert_idx = to.min(profile.cycle_group.len());
                        profile.cycle_group.insert(insert_idx, item);
                    }
                }
            }
        }
        
        ui.add_space(ITEM_SPACING / 2.0);

        ui.label(egui::RichText::new(
            format!("Current cycle order: {} character(s)", profile.cycle_group.len()))
            .small()
            .weak());
    });
    
    // Add Characters popup modal
    if state.show_add_characters_popup {
        egui::Window::new("Add Characters")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
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
                                let already_in_cycle = profile.cycle_group.contains(&char_name);
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
                            if *selected && !profile.cycle_group.contains(char_name) {
                                profile.cycle_group.push(char_name.clone());
                                changed = true;
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

    changed
}
