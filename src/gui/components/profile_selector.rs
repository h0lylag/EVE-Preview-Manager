use crate::config::profile::{Config, Profile};
use crate::constants::gui::*;
use eframe::egui;

pub struct ProfileSelector {
    edit_profile_name: String,
    edit_profile_desc: String,
    show_new_dialog: bool,
    show_duplicate_dialog: bool,
    show_delete_confirm: bool,
    show_edit_dialog: bool,
    pending_profile_idx: Option<usize>,
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
        }
    }

    /// Clear any pending profile selection.
    /// This is called when a profile switch is confirmed, or when the tray icon triggers an external profile switch,
    /// ensuring the GUI dropdown state remains consistent with the active config.
    pub fn clear_pending(&mut self) {
        self.pending_profile_idx = None;
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
                let mut display_idx = self.pending_profile_idx.unwrap_or(*selected_idx);
                let display_profile = &config.profiles[display_idx];

                egui::ComboBox::from_id_salt("profile_selector")
                    .selected_text(&display_profile.profile_name)
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

                            if ui.selectable_value(&mut display_idx, idx, label).clicked() {
                                self.pending_profile_idx = Some(display_idx);
                            }
                        }
                    });

                // Load button - only enabled if a different profile is selected
                let has_pending_change = self.pending_profile_idx.is_some()
                    && self.pending_profile_idx != Some(*selected_idx);

                if ui
                    .add_enabled(has_pending_change, egui::Button::new("⬇ Load"))
                    .clicked()
                    && let Some(new_idx) = self.pending_profile_idx
                {
                    *selected_idx = new_idx;
                    config.global.selected_profile = config.profiles[new_idx].profile_name.clone();
                    self.pending_profile_idx = None;
                    action = ProfileAction::SwitchProfile;
                }
            });
        });

        action
    }

    /// Render the profile management buttons (New, Duplicate, Edit, Delete)
    pub fn render_buttons(&mut self, ui: &mut egui::Ui, config: &Config, selected_idx: usize) {
        ui.horizontal(|ui| {
            if ui.button("➕ New").clicked() {
                self.show_new_dialog = true;
                self.edit_profile_name.clear();
                self.edit_profile_desc.clear();
            }

            if ui.button("📋 Duplicate").clicked() {
                self.show_duplicate_dialog = true;
                let current = &config.profiles[selected_idx];
                self.edit_profile_name = format!("{} (copy)", current.profile_name);
                self.edit_profile_desc = current.profile_description.clone();
            }

            if ui.button("✏ Edit").clicked() {
                self.show_edit_dialog = true;
                let current = &config.profiles[selected_idx];
                self.edit_profile_name = current.profile_name.clone();
                self.edit_profile_desc = current.profile_description.clone();
            }

            if ui.button("🗑 Delete").clicked() && config.profiles.len() > 1 {
                self.show_delete_confirm = true;
            }

            if config.profiles.len() == 1 {
                ui.label("(Cannot delete last profile)");
            }
        });
    }

    /// Render just the modal dialogs (called separately from context level)
    pub fn render_dialogs(
        &mut self,
        ctx: &egui::Context,
        config: &mut Config,
        selected_idx: &mut usize,
    ) -> ProfileAction {
        let mut action = ProfileAction::None;

        // Modal dialogs
        if self.show_new_dialog {
            action = self.new_profile_dialog(ctx, config);
        }

        if self.show_duplicate_dialog {
            action = self.duplicate_profile_dialog(ctx, config, *selected_idx);
        }

        if self.show_edit_dialog {
            action = self.edit_profile_dialog(ctx, config, *selected_idx);
        }

        if self.show_delete_confirm {
            action = self.delete_confirm_dialog(ctx, config, selected_idx);
        }

        // Clear pending selection after profile modifications
        match action {
            ProfileAction::ProfileCreated
            | ProfileAction::ProfileDeleted
            | ProfileAction::ProfileUpdated => {
                self.pending_profile_idx = None;
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

                ui.label("Description (optional):");
                ui.text_edit_singleline(&mut self.edit_profile_desc);

                ui.add_space(ITEM_SPACING);

                ui.horizontal(|ui| {
                    if ui.button("Create").clicked() && !self.edit_profile_name.is_empty() {
                        // Create new profile from default template
                        let new_profile = Profile::default_with_name(
                            self.edit_profile_name.clone(),
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
        let mut action = ProfileAction::None;

        egui::Window::new("Duplicate Profile")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("New Profile Name:");
                ui.text_edit_singleline(&mut self.edit_profile_name);

                ui.label("Description (optional):");
                ui.text_edit_singleline(&mut self.edit_profile_desc);

                ui.add_space(ITEM_SPACING);

                ui.horizontal(|ui| {
                    if ui.button("Duplicate").clicked() && !self.edit_profile_name.is_empty() {
                        let mut new_profile = config.profiles[source_idx].clone();
                        new_profile.profile_name = self.edit_profile_name.clone();
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
        selected_idx: usize,
    ) -> ProfileAction {
        let mut action = ProfileAction::None;

        egui::Window::new("Edit Profile")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Profile Name:");
                ui.text_edit_singleline(&mut self.edit_profile_name);

                ui.label("Description (optional):");
                ui.text_edit_singleline(&mut self.edit_profile_desc);

                ui.add_space(ITEM_SPACING);

                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() && !self.edit_profile_name.is_empty() {
                        let profile = &mut config.profiles[selected_idx];
                        profile.profile_name = self.edit_profile_name.clone();
                        profile.profile_description = self.edit_profile_desc.clone();
                        config.global.selected_profile = profile.profile_name.clone();
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
        selected_idx: &mut usize,
    ) -> ProfileAction {
        let mut action = ProfileAction::None;

        egui::Window::new("Confirm Delete")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Delete profile '{}'?",
                    config.profiles[*selected_idx].profile_name
                ));
                ui.colored_label(egui::Color32::from_rgb(200, 0, 0), "This cannot be undone!");

                ui.add_space(ITEM_SPACING);

                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        config.profiles.remove(*selected_idx);
                        if *selected_idx >= config.profiles.len() {
                            *selected_idx = config.profiles.len() - 1;
                        }
                        config.global.selected_profile =
                            config.profiles[*selected_idx].profile_name.clone();
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
    SwitchProfile,
    ProfileCreated,
    ProfileDeleted,
    ProfileUpdated,
}
