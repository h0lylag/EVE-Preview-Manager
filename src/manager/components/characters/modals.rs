use super::CharactersState;
use crate::common::constants::manager_ui::*;
use crate::config::profile::Profile;
use eframe::egui;

pub fn render_add_characters_modal(
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
                            let is_custom_source =
                                profile.custom_windows.iter().any(|r| r.alias == name);

                            let label_text = if already_in_cycle {
                                format!("{} (already in this group)", name)
                            } else if is_custom_source {
                                format!("{} [Source]", name)
                            } else {
                                name.clone()
                            };

                            ui.checkbox(selected, label_text);
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
