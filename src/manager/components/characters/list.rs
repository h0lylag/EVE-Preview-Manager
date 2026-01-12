use super::CharactersState;
use crate::common::constants::manager_ui::*;
use crate::config::profile::Profile;
use crate::manager::components::hotkey_settings::HotkeySettingsState;
use eframe::egui;

pub fn render_cycle_group_column(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    state: &mut CharactersState,
    hotkey_state: &mut HotkeySettingsState,
    changed: &mut bool,
) {
    // Header Row with Cycle Group Selector
    ui.horizontal(|ui| {
        ui.heading("Cycle Group");
    });
    ui.add_space(ITEM_SPACING);

    // Group Selector & Management
    ui.horizontal(|ui| {
        // Validation: Ensure at least one group
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
                        ui.selectable_value(
                            &mut state.selected_cycle_group_index,
                            idx,
                            &group.name,
                        );
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
                state.character_selections.clear();
                // Add EVE characters
                for char_name in profile.character_thumbnails.keys() {
                    state.character_selections.insert(char_name.clone(), false);
                }
                // Add Custom Sources
                for source in &profile.custom_windows {
                    // Use alias as the identifier
                    state
                        .character_selections
                        .insert(source.alias.clone(), false);
                }
            }
        });
    });

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

                for (row_idx, slot) in current_group.cycle_list.iter().enumerate() {
                    let item_id = egui::Id::new("cycle_group_item").with(row_idx);

                    let response = ui
                        .horizontal(|ui| {
                            let drag_source = ui.dnd_drag_source(item_id, row_idx, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("::").weak());
                                    
                                    match slot {
                                        crate::config::profile::CycleSlot::Eve(name) => {
                                            ui.label(name);
                                        }
                                        crate::config::profile::CycleSlot::Source(name) => {
                                             ui.colored_label(egui::Color32::LIGHT_BLUE, "Source");
                                             ui.label(name);
                                        }
                                    }
                                });
                            });

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
                            drag_source.response
                        })
                        .inner;

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
                to_idx = Some(current_group.cycle_list.len());
                *changed = true;
            }

            if let Some(idx) = to_delete {
                current_group.cycle_list.remove(idx);
            }

            if let (Some(from), Some(mut to)) = (from_idx, to_idx) {
                if from < to {
                    to -= 1;
                }
                if from != to {
                    let item = current_group.cycle_list.remove(from);
                    let insert_idx = to.min(current_group.cycle_list.len());
                    current_group.cycle_list.insert(insert_idx, item);
                }
            }

            if current_group.cycle_list.is_empty() {
                ui.label(egui::RichText::new("No characters in this group.").weak());
            }
        });
}
