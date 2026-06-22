use super::CharactersState;
use crate::common::constants::manager_ui::*;
use crate::config::profile::{CycleSlot, Profile};
use eframe::egui;

fn slot_label(slot: &CycleSlot) -> String {
    match slot {
        CycleSlot::Eve(name) => name.clone(),
        CycleSlot::Source(name) => format!("[Source] {}", name),
    }
}

pub fn render_add_characters_modal(
    ctx: &egui::Context,
    profile: &mut Profile,
    state: &mut CharactersState,
    changed: &mut bool,
) {
    let mut open = true;
    egui::Window::new("Add Sources to Cycle Group")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.set_min_width(300.0);
            ui.label("Select sources to add to cycle order:");
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
                    let mut slots: Vec<CycleSlot> =
                        state.character_selections.keys().cloned().collect();
                    slots.sort_by_key(slot_label);

                    for slot in slots {
                        if let Some(selected) = state.character_selections.get_mut(&slot) {
                            // Show if already in cycle group
                            let current_group =
                                &profile.cycle_groups[state.selected_cycle_group_index];

                            let already_in_cycle = current_group.cycle_list.contains(&slot);
                            let display_name = slot_label(&slot);

                            let label_text = if already_in_cycle {
                                format!("{} (already in this group)", display_name)
                            } else {
                                display_name
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

                    for (slot, selected) in &state.character_selections {
                        if *selected {
                            let already_exists = current_group.cycle_list.contains(slot);

                            if !already_exists {
                                current_group.cycle_list.push(slot.clone());
                                added_any = true;
                            }
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
