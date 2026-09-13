use crate::common::constants::manager_ui::*;
use crate::config::profile::{Config, Profile};
use crate::manager::state::SharedState;
use eframe::egui;

pub struct ProfileSelector {
    edit_profile_name: String,
    edit_profile_desc: String,
    show_new_dialog: bool,
    show_duplicate_dialog: bool,
    show_delete_confirm: bool,
    show_edit_dialog: bool,
    pending_profile_idx: Option<usize>,
    /// Index of the profile we are performing an action on (Edit/Duplicate/Delete)
    /// This might be different from selected_idx (active profile) if user is editing a non-active profile
    action_target_idx: Option<usize>,
}

impl ProfileSelector {
    pub fn new() -> Self {
        Self {
            edit_profile_name: String::new(),
            edit_profile_desc: String::new(),
            show_new_dialog: false,
            show_duplicate_dialog: false,
            show_delete_confirm: false,
            show_edit_dialog: false,
            pending_profile_idx: None,
            action_target_idx: None,
        }
    }

    /// Replace config and its profile UI state together, only after a successful read.
    pub fn reload_config(&mut self, state: &mut SharedState) -> anyhow::Result<()> {
        state.discard_changes()?;
        *self = Self::new();
        Ok(())
    }

    fn display_index(&mut self, config: &Config, selected_idx: usize) -> Option<usize> {
        if self
            .pending_profile_idx
            .is_some_and(|idx| idx == selected_idx || config.profiles.get(idx).is_none())
        {
            self.pending_profile_idx = None;
        }
        let idx = self.pending_profile_idx.unwrap_or(selected_idx);
        config.profiles.get(idx).map(|_| idx)
    }

    /// Render just the dropdown group box with Load button
    pub fn render_dropdown(
        &mut self,
        ui: &mut egui::Ui,
        config: &mut Config,
        selected_idx: &mut usize,
    ) -> ProfileAction {
        let mut action = ProfileAction::None;

        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Profile:").strong());

                // Profile dropdown - use pending index if set, otherwise use current
                let mut display_idx = self.display_index(config, *selected_idx);
                let display_name = display_idx
                    .and_then(|idx| config.profiles.get(idx))
                    .map_or("No profile selected", |profile| {
                        profile.profile_name.as_str()
                    });

                egui::ComboBox::from_id_salt(("profile_selector", config.profiles.len()))
                    .selected_text(display_name)
                    .show_ui(ui, |ui| {
                        for (idx, profile) in config.profiles.iter().enumerate() {
                            let label = if profile.profile_description.is_empty() {
                                profile.profile_name.clone()
                            } else {
                                format!(
                                    "{} - {}",
                                    profile.profile_name, profile.profile_description
                                )
                            };

                            if ui
                                .selectable_value(&mut display_idx, Some(idx), label)
                                .clicked()
                            {
                                self.pending_profile_idx = display_idx;
                            }
                        }
                    });

                // Load button - only enabled if a different profile is selected
                let has_pending_change = self.pending_profile_idx.is_some()
                    && self.pending_profile_idx != Some(*selected_idx)
                    && config.validate_profile_names().is_ok();

                if ui
                    .add_enabled(has_pending_change, egui::Button::new("⬇ Load"))
                    .clicked()
                    && let Some(new_idx) = self.pending_profile_idx
                    && config.profiles.get(new_idx).is_some()
                {
                    self.pending_profile_idx = None;
                    action = ProfileAction::SwitchProfile(new_idx);
                }
            });
        });

        action
    }

    /// Render the profile management buttons (New, Duplicate, Edit, Delete)
    pub fn render_buttons(&mut self, ui: &mut egui::Ui, config: &Config, selected_idx: usize) {
        // Determine which profile is visually selected in the dropdown
        // If pending_profile_idx is None, it means the dropdown shows the active profile (selected_idx)
        let target_idx = self.display_index(config, selected_idx);
        let target = target_idx.and_then(|idx| config.profiles.get(idx));

        ui.horizontal(|ui| {
            if ui.button("➕ New").clicked() {
                self.show_new_dialog = true;
                self.edit_profile_name.clear();
                self.edit_profile_desc.clear();
                // New profile doesn't target an existing index
                self.action_target_idx = None;
            }

            if ui
                .add_enabled(target.is_some(), egui::Button::new("📋 Duplicate"))
                .clicked()
                && let Some(current) = target
            {
                self.show_duplicate_dialog = true;
                self.edit_profile_name = format!("{} (copy)", current.profile_name);
                self.edit_profile_desc = current.profile_description.clone();
                self.action_target_idx = target_idx;
            }

            if ui
                .add_enabled(target.is_some(), egui::Button::new("✏ Edit"))
                .clicked()
                && let Some(current) = target
            {
                self.show_edit_dialog = true;
                self.edit_profile_name = current.profile_name.clone();
                self.edit_profile_desc = current.profile_description.clone();
                self.action_target_idx = target_idx;
            }

            // Can delete if we have > 1 profile
            if ui
                .add_enabled(
                    target.is_some() && config.profiles.len() > 1,
                    egui::Button::new("🗑 Delete"),
                )
                .clicked()
            {
                self.show_delete_confirm = true;
                self.action_target_idx = target_idx;
            }

            if config.profiles.len() == 1 {
                ui.label("(Cannot delete last profile)");
            }
        });

        if let Err(err) = config.validate_profile_names() {
            ui.colored_label(egui::Color32::RED, err);
        }
    }

    /// Render just the modal dialogs (called separately from context level)
    pub fn render_dialogs(
        &mut self,
        ctx: &egui::Context,
        config: &mut Config,
        selected_idx: &mut usize,
    ) -> ProfileAction {
        let mut action = ProfileAction::None;

        // A missing target must never silently redirect a dialog to the active profile.
        if self
            .action_target_idx
            .and_then(|idx| config.profiles.get(idx))
            .is_none()
        {
            self.show_duplicate_dialog = false;
            self.show_edit_dialog = false;
            self.show_delete_confirm = false;
            self.action_target_idx = None;
        }

        // Modal dialogs
        if self.show_new_dialog {
            action = self.new_profile_dialog(ctx, config);
        }

        if self.show_duplicate_dialog
            && let Some(target_idx) = self.action_target_idx
        {
            action = self.duplicate_profile_dialog(ctx, config, target_idx);
        }

        if self.show_edit_dialog
            && let Some(target_idx) = self.action_target_idx
        {
            action = self.edit_profile_dialog(ctx, config, selected_idx, target_idx);
        }

        if self.show_delete_confirm
            && let Some(target_idx) = self.action_target_idx
        {
            action = self.delete_confirm_dialog(ctx, config, selected_idx, target_idx);
        }

        // Clear pending selection/target after profile modifications
        match action {
            ProfileAction::ProfileCreated
            | ProfileAction::ProfileDeleted
            | ProfileAction::ProfileUpdated
            | ProfileAction::SwitchProfile(_) => {
                self.pending_profile_idx = None;
                self.action_target_idx = None;
            }
            _ => {}
        }

        action
    }

    fn new_profile_dialog(&mut self, ctx: &egui::Context, config: &mut Config) -> ProfileAction {
        let mut action = ProfileAction::None;

        egui::Window::new("New Profile")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Profile Name:");
                ui.text_edit_singleline(&mut self.edit_profile_name);
                let profile_name = config.validate_profile_name(None, &self.edit_profile_name);
                if let Err(err) = &profile_name
                    && !self.edit_profile_name.is_empty()
                {
                    ui.colored_label(egui::Color32::RED, err);
                }

                ui.label("Description (optional):");
                ui.text_edit_singleline(&mut self.edit_profile_desc);

                ui.add_space(ITEM_SPACING);

                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(profile_name.is_ok(), egui::Button::new("Create"))
                        .clicked()
                        && let Ok(profile_name) = profile_name
                    {
                        // Create new profile from default template
                        let new_profile = Profile::default_with_name(
                            profile_name,
                            self.edit_profile_desc.clone(),
                        );
                        config.profiles.push(new_profile);
                        action = ProfileAction::ProfileCreated;
                        self.show_new_dialog = false;
                    }

                    if ui.button("Cancel").clicked() {
                        self.show_new_dialog = false;
                    }
                });
            });

        action
    }

    fn duplicate_profile_dialog(
        &mut self,
        ctx: &egui::Context,
        config: &mut Config,
        source_idx: usize,
    ) -> ProfileAction {
        if config.profiles.get(source_idx).is_none() {
            self.show_duplicate_dialog = false;
            self.action_target_idx = None;
            return ProfileAction::None;
        }
        let mut action = ProfileAction::None;

        egui::Window::new("Duplicate Profile")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("New Profile Name:");
                ui.text_edit_singleline(&mut self.edit_profile_name);
                let profile_name = config.validate_profile_name(None, &self.edit_profile_name);
                if let Err(err) = &profile_name
                    && !self.edit_profile_name.is_empty()
                {
                    ui.colored_label(egui::Color32::RED, err);
                }

                ui.label("Description (optional):");
                ui.text_edit_singleline(&mut self.edit_profile_desc);

                ui.add_space(ITEM_SPACING);

                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(profile_name.is_ok(), egui::Button::new("Duplicate"))
                        .clicked()
                        && let Ok(profile_name) = profile_name
                        && let Some(source) = config.profiles.get(source_idx)
                    {
                        let mut new_profile = source.clone();
                        new_profile.profile_name = profile_name;
                        new_profile.profile_description = self.edit_profile_desc.clone();
                        config.profiles.push(new_profile);

                        action = ProfileAction::ProfileCreated;
                        self.show_duplicate_dialog = false;
                    }

                    if ui.button("Cancel").clicked() {
                        self.show_duplicate_dialog = false;
                    }
                });
            });

        action
    }

    fn edit_profile_dialog(
        &mut self,
        ctx: &egui::Context,
        config: &mut Config,
        active_idx: &mut usize,
        target_idx: usize,
    ) -> ProfileAction {
        if config.profiles.get(target_idx).is_none() {
            self.show_edit_dialog = false;
            self.action_target_idx = None;
            return ProfileAction::None;
        }
        let mut action = ProfileAction::None;

        egui::Window::new("Edit Profile")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Profile Name:");
                ui.text_edit_singleline(&mut self.edit_profile_name);
                let profile_name =
                    config.validate_profile_name(Some(target_idx), &self.edit_profile_name);
                if let Err(err) = &profile_name
                    && !self.edit_profile_name.is_empty()
                {
                    ui.colored_label(egui::Color32::RED, err);
                }

                ui.label("Description (optional):");
                ui.text_edit_singleline(&mut self.edit_profile_desc);

                ui.add_space(ITEM_SPACING);

                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(profile_name.is_ok(), egui::Button::new("Save"))
                        .clicked()
                        && let Ok(profile_name) = profile_name
                        && let Some(profile) = config.profiles.get_mut(target_idx)
                    {
                        profile.profile_name = profile_name;
                        profile.profile_description = self.edit_profile_desc.clone();

                        // Only update global selection if we modified the active profile
                        if target_idx == *active_idx {
                            config.global.selected_profile = profile.profile_name.clone();
                        }

                        action = ProfileAction::ProfileUpdated;
                        self.show_edit_dialog = false;
                    }

                    if ui.button("Cancel").clicked() {
                        self.show_edit_dialog = false;
                    }
                });
            });

        action
    }

    fn delete_confirm_dialog(
        &mut self,
        ctx: &egui::Context,
        config: &mut Config,
        active_idx: &mut usize,
        target_idx: usize,
    ) -> ProfileAction {
        let Some(target) = config
            .profiles
            .get(target_idx)
            .filter(|_| config.profiles.len() > 1)
        else {
            self.show_delete_confirm = false;
            self.action_target_idx = None;
            return ProfileAction::None;
        };
        let target_name = target.profile_name.clone();
        let mut action = ProfileAction::None;

        egui::Window::new("Confirm Delete")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("Delete profile '{}'?", target_name));
                ui.colored_label(egui::Color32::from_rgb(200, 0, 0), "This cannot be undone!");

                ui.add_space(ITEM_SPACING);

                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked()
                        && config.profiles.len() > 1
                        && config.profiles.get(target_idx).is_some()
                    {
                        config.profiles.remove(target_idx);

                        // Adjust active index if needed
                        if target_idx < *active_idx {
                            // Deleted profile was before active one, shift active index down
                            *active_idx -= 1;
                        } else if target_idx == *active_idx {
                            // Deleted the active profile
                            if *active_idx >= config.profiles.len() {
                                *active_idx = config.profiles.len().saturating_sub(1);
                            }
                            // Update global name only if active was touched/shifted
                            if let Some(active) = config.profiles.get(*active_idx) {
                                config.global.selected_profile = active.profile_name.clone();
                            }
                        }

                        action = ProfileAction::ProfileDeleted;
                        self.show_delete_confirm = false;
                    }

                    if ui.button("Cancel").clicked() {
                        self.show_delete_confirm = false;
                    }
                });
            });

        action
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProfileAction {
    None,
    SwitchProfile(usize),
    ProfileCreated,
    ProfileDeleted,
    ProfileUpdated,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_names(names: &[&str]) -> Config {
        let mut config = Config {
            profiles: names
                .iter()
                .map(|name| Profile::default_with_name((*name).into(), String::new()))
                .collect(),
            ..Config::default()
        };
        config.global.selected_profile = names.first().copied().unwrap_or_default().into();
        config
    }

    fn pending_selector() -> ProfileSelector {
        let mut selector = ProfileSelector::new();
        selector.pending_profile_idx = Some(2);
        selector.action_target_idx = Some(2);
        selector.edit_profile_name = "Draft".into();
        selector.edit_profile_desc = "Unsaved description".into();
        selector.show_new_dialog = true;
        selector.show_duplicate_dialog = true;
        selector.show_edit_dialog = true;
        selector.show_delete_confirm = true;
        selector
    }

    fn frame(
        ctx: &egui::Context,
        selector: &mut ProfileSelector,
        config: &mut Config,
        active: &mut usize,
        events: Vec<egui::Event>,
    ) -> (ProfileAction, egui::FullOutput) {
        let mut action = ProfileAction::None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| {
                action = selector.render_dropdown(ui, config, active);
                selector.render_buttons(ui, config, *active);
                let dialog = selector.render_dialogs(ui.ctx(), config, active);
                if dialog != ProfileAction::None {
                    action = dialog;
                }
            },
        );
        output.textures_delta.clear();
        (action, output)
    }

    fn click(
        ctx: &egui::Context,
        selector: &mut ProfileSelector,
        config: &mut Config,
        active: &mut usize,
        label: &str,
    ) -> ProfileAction {
        // Let newly opened windows finish their sizing pass before clicking.
        frame(ctx, selector, config, active, vec![]);
        let (_, output) = frame(ctx, selector, config, active, vec![]);
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.pos + text.galley.rect.center().to_vec2())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing UI label: {label}"));
        let events = vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        frame(ctx, selector, config, active, events).0
    }

    #[test]
    fn reload_clears_profile_targets_for_shorter_and_same_length_configs() {
        for names in [
            &["Restored"][..],
            &[
                "First replacement",
                "Second replacement",
                "Third replacement",
            ][..],
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.json");
            let mut state =
                SharedState::at_path(config_with_names(&["First", "Second", "Third"]), &path);
            let mut selector = pending_selector();
            let replacement = config_with_names(names);
            replacement.save_to(&path).unwrap();
            let saved = std::fs::read(&path).unwrap();

            selector.reload_config(&mut state).unwrap();
            let (action, _) = frame(
                &egui::Context::default(),
                &mut selector,
                &mut state.config,
                &mut state.selected_profile_idx,
                vec![],
            );

            assert_eq!(selector.pending_profile_idx, None);
            assert_eq!(selector.action_target_idx, None);
            assert!(selector.edit_profile_name.is_empty() && selector.edit_profile_desc.is_empty());
            assert!(
                !selector.show_new_dialog
                    && !selector.show_duplicate_dialog
                    && !selector.show_edit_dialog
                    && !selector.show_delete_confirm
            );
            assert_eq!(state.selected_profile_idx, 0);
            assert_eq!(action, ProfileAction::None);
            assert_eq!(
                serde_json::to_value(&state.config).unwrap(),
                serde_json::to_value(replacement).unwrap()
            );
            assert_eq!(std::fs::read(path).unwrap(), saved);
        }
    }

    #[test]
    fn failed_reload_preserves_profile_ui_and_unsaved_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, b"{broken").unwrap();
        let mut state =
            SharedState::at_path(config_with_names(&["First", "Second", "Third"]), &path);
        state.settings_changed = true;
        let before = serde_json::to_value(&state.config).unwrap();
        let mut selector = pending_selector();

        assert!(selector.reload_config(&mut state).is_err());

        assert_eq!(selector.pending_profile_idx, Some(2));
        assert_eq!(selector.action_target_idx, Some(2));
        assert_eq!(selector.edit_profile_name, "Draft");
        assert_eq!(selector.edit_profile_desc, "Unsaved description");
        assert!(
            selector.show_new_dialog
                && selector.show_duplicate_dialog
                && selector.show_edit_dialog
                && selector.show_delete_confirm
        );
        assert_eq!(state.selected_profile_idx, 0);
        assert_eq!(serde_json::to_value(&state.config).unwrap(), before);
        assert!(state.settings_changed && state.config_load_error.is_some());
        assert_eq!(std::fs::read(path).unwrap(), b"{broken");
    }

    #[test]
    fn invalid_selections_and_dialog_targets_do_not_mutate_profiles() {
        for names in [&[][..], &["First", "Second"][..]] {
            for target in [None, Some(usize::MAX)] {
                let ctx = egui::Context::default();
                let mut config = config_with_names(names);
                let before = serde_json::to_value(&config).unwrap();
                let mut selector = pending_selector();
                selector.show_new_dialog = false;
                selector.pending_profile_idx = Some(usize::MAX);
                selector.action_target_idx = target;
                let mut active = 0;

                let (action, _) = frame(&ctx, &mut selector, &mut config, &mut active, vec![]);
                assert_eq!(action, ProfileAction::None);
                assert_eq!(selector.pending_profile_idx, None);
                assert_eq!(selector.action_target_idx, None);
                assert!(
                    !selector.show_duplicate_dialog
                        && !selector.show_edit_dialog
                        && !selector.show_delete_confirm
                );
                assert_eq!(serde_json::to_value(&config).unwrap(), before);

                active = usize::MAX;
                frame(&ctx, &mut selector, &mut config, &mut active, vec![]);
                for label in ["📋 Duplicate", "✏ Edit", "🗑 Delete", "⬇ Load"] {
                    assert_eq!(
                        click(&ctx, &mut selector, &mut config, &mut active, label),
                        ProfileAction::None
                    );
                }
                assert_eq!(serde_json::to_value(&config).unwrap(), before);
                click(&ctx, &mut selector, &mut config, &mut active, "➕ New");
                assert!(selector.show_new_dialog);
            }
        }
    }

    #[test]
    fn valid_profile_actions_still_work_and_keep_the_last_profile() {
        let ctx = egui::Context::default();
        let mut config = config_with_names(&["First", "Second"]);
        let mut active = 0;
        let mut selector = ProfileSelector::new();
        selector.pending_profile_idx = Some(1);
        assert_eq!(
            click(&ctx, &mut selector, &mut config, &mut active, "⬇ Load"),
            ProfileAction::SwitchProfile(1)
        );
        active = 1;
        config.global.selected_profile = "Second".into();

        click(
            &ctx,
            &mut selector,
            &mut config,
            &mut active,
            "📋 Duplicate",
        );
        assert_eq!(selector.action_target_idx, Some(1));
        assert_eq!(
            click(&ctx, &mut selector, &mut config, &mut active, "Duplicate"),
            ProfileAction::ProfileCreated
        );
        assert_eq!(config.profiles[2].profile_name, "Second (copy)");

        click(&ctx, &mut selector, &mut config, &mut active, "✏ Edit");
        selector.edit_profile_name = "Renamed".into();
        assert_eq!(
            click(&ctx, &mut selector, &mut config, &mut active, "Save"),
            ProfileAction::ProfileUpdated
        );
        assert_eq!(config.profiles[active].profile_name, "Renamed");
        assert_eq!(config.global.selected_profile, "Renamed");

        for remaining in [2, 1] {
            click(&ctx, &mut selector, &mut config, &mut active, "🗑 Delete");
            assert_eq!(
                click(&ctx, &mut selector, &mut config, &mut active, "Delete"),
                ProfileAction::ProfileDeleted
            );
            assert_eq!(config.profiles.len(), remaining);
            assert_eq!(
                config.profiles[active].profile_name,
                config.global.selected_profile
            );
        }
        // Even a previously opened delete dialog cannot remove the last profile.
        selector.show_delete_confirm = true;
        selector.action_target_idx = Some(0);
        assert_eq!(
            frame(&ctx, &mut selector, &mut config, &mut active, vec![]).0,
            ProfileAction::None
        );
        assert!(!selector.show_delete_confirm);
        assert_eq!(config.profiles.len(), 1);
    }
}
