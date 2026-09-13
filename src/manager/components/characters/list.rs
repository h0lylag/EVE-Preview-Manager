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
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
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
                    let text_edit = egui::TextEdit::singleline(&mut state.rename_buffer)
                        .id_salt("cycle_group_rename")
                        .desired_width(120.0);
                    let response = ui.add(text_edit);

                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        state.renaming_group_idx = None;
                        state.rename_buffer.clear();
                        state.rename_error = None;
                    } else if response.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        match profile.rename_cycle_group(idx, &state.rename_buffer) {
                            Ok(renamed) => {
                                *changed |= renamed;
                                state.renaming_group_idx = None;
                                state.rename_buffer.clear();
                                state.rename_error = None;
                            }
                            Err(error) => state.rename_error = Some(error),
                        }
                    }

                    if !response.has_focus() && state.renaming_group_idx.is_some() {
                        response.request_focus();
                    }
                } else {
                    state.renaming_group_idx = None;
                    state.rename_buffer.clear();
                    state.rename_error = None;
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
                    state.rename_error = None;
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
                    new_group.name = profile.unused_cycle_group_name("New Group");
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
                    let mut new_group =
                        profile.cycle_groups[state.selected_cycle_group_index].clone();
                    new_group.name = profile
                        .unused_cycle_group_name(&format!("{} (Copy)", new_group.name.trim()));
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

        if let Some(error) = &state.rename_error {
            ui.colored_label(COLOR_ERROR, error);
        } else if let Err(error) = profile.validate_cycle_group_names() {
            ui.colored_label(COLOR_ERROR, error);
        }
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
                if ui.button("➕ Add Sources").clicked() {
                    state.show_add_characters_popup = true;
                    state.character_selections.clear();
                    // Add EVE characters
                    for char_name in profile.character_thumbnails.keys() {
                        state.character_selections.insert(
                            crate::config::profile::CycleSlot::Eve(char_name.clone()),
                            false,
                        );
                    }
                    // Add Custom Sources
                    for source in &profile.custom_windows {
                        state.character_selections.insert(
                            crate::config::profile::CycleSlot::Source(source.alias.clone()),
                            false,
                        );
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
                                                ui.colored_label(
                                                    egui::Color32::LIGHT_BLUE,
                                                    "Source",
                                                );
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
                            let stroke =
                                egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);

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
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::{CycleGroup, CycleSlot};

    fn frame(
        ctx: &egui::Context,
        profile: &mut Profile,
        state: &mut CharactersState,
        events: Vec<egui::Event>,
    ) -> (bool, egui::FullOutput) {
        let mut changed = false;
        let mut output = ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| {
                render_cycle_group_column(
                    ui,
                    profile,
                    state,
                    &mut HotkeySettingsState::new(),
                    &mut changed,
                );
            },
        );
        output.textures_delta.clear();
        (changed, output)
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn click(
        ctx: &egui::Context,
        profile: &mut Profile,
        state: &mut CharactersState,
        label: &str,
    ) -> bool {
        frame(ctx, profile, state, vec![]);
        let (_, output) = frame(ctx, profile, state, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.pos + text.galley.rect.center().to_vec2())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing button {label}"));
        frame(
            ctx,
            profile,
            state,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .0
    }

    #[test]
    fn repeated_cycle_group_copies_keep_contents_and_use_unused_names() {
        let ctx = egui::Context::default();
        let mut profile = Profile::default();
        profile.cycle_groups[0].name = "Fleet".into();
        profile.cycle_groups[0].cycle_list = vec![
            CycleSlot::Eve("Alice".into()),
            CycleSlot::Source("Browser".into()),
        ];
        profile.cycle_groups[0].hotkey_forward = Some(crate::config::HotkeyBinding::new(
            59, true, false, false, false,
        ));
        let original = profile.cycle_groups[0].clone();
        let mut state = CharactersState::new();
        for name in ["Fleet (Copy)", "Fleet (Copy) 2", "Fleet (Copy) 3"] {
            state.selected_cycle_group_index = 0;
            assert!(click(&ctx, &mut profile, &mut state, "📄 Copy"));
            let copied = &profile.cycle_groups[state.selected_cycle_group_index];
            assert_eq!(copied.name, name);
            let mut expected = original.clone();
            expected.name = name.into();
            assert_eq!(
                serde_json::to_value(copied).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        }
        profile.cycle_groups.push(CycleGroup {
            name: "new group".into(),
            ..CycleGroup::default_group()
        });
        assert!(click(&ctx, &mut profile, &mut state, "➕ New"));
        assert_eq!(profile.cycle_groups.last().unwrap().name, "New Group 2");
    }

    #[test]
    fn cycle_group_rename_preserves_rejected_drafts_and_cancels_before_submit() {
        let ctx = egui::Context::default();
        let mut profile = Profile::default();
        profile.cycle_groups[0].name = "Fleet".into();
        profile.cycle_groups.push(CycleGroup {
            name: "Other".into(),
            ..CycleGroup::default_group()
        });
        let mut state = CharactersState::new();
        for draft in [" other ", "  "] {
            state.renaming_group_idx = Some(0);
            state.rename_buffer = draft.into();
            frame(&ctx, &mut profile, &mut state, vec![]);
            let (changed, output) =
                frame(&ctx, &mut profile, &mut state, vec![key(egui::Key::Enter)]);
            assert!(!changed);
            assert_eq!(profile.cycle_groups[0].name, "Fleet");
            assert_eq!(state.rename_buffer, draft);
            assert_eq!(state.renaming_group_idx, Some(0));
            let error = state.rename_error.as_ref().unwrap();
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::epaint::Shape::Text(text) if text.galley.text() == error)));
        }
        state.rename_buffer = "New Name".into();
        assert!(
            !frame(
                &ctx,
                &mut profile,
                &mut state,
                vec![key(egui::Key::Enter), key(egui::Key::Escape)]
            )
            .0
        );
        assert_eq!(profile.cycle_groups[0].name, "Fleet");
        assert!(state.renaming_group_idx.is_none() && state.rename_error.is_none());
        assert!(state.rename_buffer.is_empty());

        for (draft, expected, changed) in [
            (" Fleet ", "Fleet", false),
            (" Main  Fleet ", "Main  Fleet", true),
        ] {
            state.renaming_group_idx = Some(0);
            state.rename_buffer = draft.into();
            frame(&ctx, &mut profile, &mut state, vec![]);
            assert_eq!(
                frame(&ctx, &mut profile, &mut state, vec![key(egui::Key::Enter)]).0,
                changed
            );
            assert_eq!(profile.cycle_groups[0].name, expected);
            assert!(state.renaming_group_idx.is_none());
        }
        frame(&ctx, &mut profile, &mut state, vec![]);
        state.renaming_group_idx = Some(0);
        state.rename_buffer = "Focus Loss".into();
        frame(&ctx, &mut profile, &mut state, vec![]);
        frame(&ctx, &mut profile, &mut state, vec![]);
        let focused = ctx.memory(|memory| memory.focused()).unwrap();
        ctx.memory_mut(|memory| memory.surrender_focus(focused));
        assert!(frame(&ctx, &mut profile, &mut state, vec![]).0);
        assert_eq!(profile.cycle_groups[0].name, "Focus Loss");
        state.renaming_group_idx = Some(1);
        state.rename_buffer = "Stale".into();
        state.rename_error = Some("Old error".into());
        state.load_from_profile(&Profile::default());
        assert!(state.renaming_group_idx.is_none() && state.rename_error.is_none());
        assert!(state.rename_buffer.is_empty());
    }
}
