//! Behavior settings component (per-profile settings)

use crate::common::constants::manager_ui::*;
use crate::config::backup::BackupManager;
use crate::config::profile::{GlobalSettings, Profile};

use chrono::{DateTime, Local};
use eframe::egui;

#[derive(PartialEq)]
pub enum BehaviorSettingsAction {
    None,
    SettingsChanged,
    RestoreTriggered,
}

/// State for behavior settings UI
pub struct BehaviorSettingsState {
    pub backup_list: Vec<(String, String)>, // (filename, display_name)
    pub selected_backup: Option<String>,
    pub show_restore_confirm: bool,
    pub show_delete_confirm: bool, // For manual deletion
    pub status_message: Option<String>,
    pub status_type: Option<egui::Color32>,
}

impl BehaviorSettingsState {
    pub fn new() -> Self {
        Self {
            backup_list: Vec::new(),
            selected_backup: None,
            show_restore_confirm: false,
            show_delete_confirm: false,
            status_message: None,
            status_type: None,
        }
    }

    pub fn refresh_backups(&mut self) {
        match BackupManager::list_backups(None) {
            Ok(backups) => {
                self.backup_list = backups
                    .into_iter()
                    .map(|b| {
                        let datetime: DateTime<Local> = b.timestamp.into();
                        let display = format!(
                            "{} ({})",
                            datetime.format("%Y-%m-%d %H:%M:%S"),
                            if b.is_manual { "Manual" } else { "Auto" }
                        );
                        (b.filename, display)
                    })
                    .collect();

                // If selected backup is no longer in list, clear selection
                let selection_invalid = self
                    .selected_backup
                    .as_ref()
                    .is_some_and(|selected| !self.backup_list.iter().any(|(f, _)| f == selected));

                if selection_invalid || self.selected_backup.is_none() {
                    // Default to the first (newest) backup if available
                    self.selected_backup = self.backup_list.first().map(|(f, _)| f.clone());
                }
            }
            Err(e) => {
                self.status_message = Some(format!("Failed to list backups: {}", e));
                self.status_type = Some(COLOR_ERROR);
            }
        }
    }
}

impl Default for BehaviorSettingsState {
    fn default() -> Self {
        let mut state = Self::new();
        state.refresh_backups();
        state
    }
}

pub fn ui(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    global: &mut GlobalSettings,
    state: &mut BehaviorSettingsState,
) -> BehaviorSettingsAction {
    let mut action = BehaviorSettingsAction::None;

    ui.columns(2, |columns| {
        // Left Column: Behavior Settings
        columns[0].group(|ui| {
            ui.label(egui::RichText::new("Behavior Settings").strong());
            ui.add_space(ITEM_SPACING);

            // Minimize clients on switch
            if ui.checkbox(&mut profile.client_minimize_on_switch,
                "Minimize EVE clients when switching focus").changed() {
                action = BehaviorSettingsAction::SettingsChanged;
            }

            if profile.client_minimize_on_switch {
                ui.indent("minimize_overlay_indent", |ui| {
                    if ui.checkbox(&mut profile.client_minimize_show_overlay,
                        "Show 'MINIMIZED' text overlay").changed() {
                        action = BehaviorSettingsAction::SettingsChanged;
                    }
                });
            }

            ui.label(egui::RichText::new(
                "When clicking a thumbnail, minimize all other EVE clients")
                .small()
                .weak());

            ui.add_space(ITEM_SPACING);

            // Hide when no focus
            if ui.checkbox(&mut profile.thumbnail_hide_not_focused,
                "Hide thumbnails when EVE loses focus").changed() {
                action = BehaviorSettingsAction::SettingsChanged;
            }

            ui.label(egui::RichText::new(
                "When enabled, thumbnails disappear when no EVE window is focused")
                .small()
                .weak());

            ui.add_space(ITEM_SPACING);

            // Auto-save thumbnail positions
            if ui.checkbox(
                &mut profile.thumbnail_auto_save_position,
                "Automatically save thumbnail positions"
            ).changed() {
                action = BehaviorSettingsAction::SettingsChanged;
            }

            ui.label(egui::RichText::new(
                "When disabled, positions are only saved when you use 'Save Thumbnail Positions' from the system tray menu")
                .small()
                .weak());

            ui.add_space(ITEM_SPACING);

            // Preserve thumbnail position on character swap
            if ui.checkbox(&mut profile.thumbnail_preserve_position_on_swap,
                "New characters inherit thumbnail position").changed() {
                action = BehaviorSettingsAction::SettingsChanged;
            }

            ui.label(egui::RichText::new(
                "New characters inherit thumbnail position from the logged-out character")
                .small()
                .weak());

            ui.add_space(ITEM_SPACING);

            // Snap threshold
            ui.horizontal(|ui| {
                ui.label("Thumbnail Snap Distance:");
                if ui.add(egui::Slider::new(&mut profile.thumbnail_snap_threshold, 0..=50)
                    .suffix(" px")).changed() {
                    action = BehaviorSettingsAction::SettingsChanged;
                }
            });

            ui.label(egui::RichText::new(
                "Distance for edge/corner snapping (0 = disabled)")
                .small()
                .weak());
        });

        // Right Column: Backup Settings
        columns[1].group(|ui| {
            ui.label(egui::RichText::new("Backup & Restore").strong());
            ui.add_space(ITEM_SPACING);

            // Auto Backup Settings
            if ui.checkbox(&mut global.backup_enabled, "Enable Automatic Backups").changed() {
                action = BehaviorSettingsAction::SettingsChanged;
            }

            if global.backup_enabled {
                ui.indent("auto_backup_indent", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Interval (Days):");
                        if ui.add(egui::Slider::new(&mut global.backup_interval_days, 1..=30)).changed() {
                            action = BehaviorSettingsAction::SettingsChanged;
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("Retention Count:");
                        if ui.add(egui::Slider::new(&mut global.backup_retention_count, 1..=100)).changed() {
                            action = BehaviorSettingsAction::SettingsChanged;
                        }
                    });
                    ui.label(egui::RichText::new("(Auto-backups only)").small().weak());
                });

            }

            ui.add_space(ITEM_SPACING);
            ui.separator();
            ui.add_space(ITEM_SPACING);

            // Manual Backup
            ui.horizontal(|ui| {
                if ui.button("📤 Create Backup").clicked() {
                    match BackupManager::create_backup(true, None) {
                        Ok(_) => {
                            state.status_message = Some("Manual backup created successfully".to_string());
                            state.status_type = Some(COLOR_SUCCESS);
                            state.refresh_backups();
                        }
                        Err(e) => {
                            state.status_message = Some(format!("Backup failed: {}", e));
                            state.status_type = Some(COLOR_ERROR);
                        }
                    }
                }
            });

            ui.add_space(ITEM_SPACING);
            ui.separator();
            ui.add_space(ITEM_SPACING);

            // Restore / Management
            ui.label("Configuration Backups");

            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("backup_selector")
                    .selected_text(
                        state.selected_backup.as_ref()
                            .and_then(|sel| state.backup_list.iter().find(|(f, _)| f == sel))
                            .map(|(_, d)| d.as_str())
                            .unwrap_or("Select a backup...")
                    )
                    .width(250.0)
                    .show_ui(ui, |ui| {
                        for (filename, display) in &state.backup_list {
                            ui.selectable_value(&mut state.selected_backup, Some(filename.clone()), display);
                        }
                    });

                if ui.button("🔄").on_hover_text("Refresh backup list").clicked() {
                    state.refresh_backups();
                }
            });

            // Clone selected backup to avoid holding a borrow on state
            let selected_opt = state.selected_backup.clone();
            if let Some(selected) = selected_opt {
                 ui.add_space(5.0);
                 ui.horizontal(|ui| {
                    // Restore Button flow
                    if state.show_restore_confirm {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                if ui.button(egui::RichText::new("YES, RESTORE").color(COLOR_ERROR)).clicked() {
                                    match BackupManager::restore_backup(&selected, None) {
                                        Ok(_) => {
                                            state.status_message = Some("Restored successfully. Configuration reloaded.".to_string());
                                            state.status_type = Some(COLOR_SUCCESS);
                                            state.show_restore_confirm = false;
                                            action = BehaviorSettingsAction::RestoreTriggered;
                                        }
                                        Err(e) => {
                                            state.status_message = Some(format!("Restore failed: {}", e));
                                            state.status_type = Some(COLOR_ERROR);
                                            state.show_restore_confirm = false;
                                        }
                                    }
                                }
                                if ui.button("Cancel").clicked() {
                                    state.show_restore_confirm = false;
                                    state.status_message = None;
                                }
                            });
                        });
                    } else if ui.button("📥 Restore").clicked() {
                        state.show_restore_confirm = true;
                        state.show_delete_confirm = false;
                        state.status_message = Some("WARNING: Overwrite current config?".to_string());
                        state.status_type = Some(COLOR_WARNING);
                    }

                    if !state.show_restore_confirm {
                        ui.add_space(20.0);

                        // Delete Button flow
                        if state.show_delete_confirm {
                            ui.vertical(|ui| {
                                 ui.horizontal(|ui| {
                                    if ui.button(egui::RichText::new("YES, DELETE").color(COLOR_ERROR)).clicked() {
                                        match BackupManager::delete_backup(&selected, None) {
                                            Ok(_) => {
                                                state.status_message = Some("Backup deleted.".to_string());
                                                state.status_type = Some(COLOR_SUCCESS);
                                                state.refresh_backups();
                                                state.show_delete_confirm = false;
                                            }
                                            Err(e) => {
                                                state.status_message = Some(format!("Delete failed: {}", e));
                                                state.status_type = Some(COLOR_ERROR);
                                            }
                                        }
                                    }
                                    if ui.button("Cancel").clicked() {
                                        state.show_delete_confirm = false;
                                        state.status_message = None;
                                    }
                                });
                            });
                        } else if ui.button("🗑 Delete").clicked() {
                            state.show_delete_confirm = true;
                            state.show_restore_confirm = false;
                            state.status_message = Some("WARNING: Delete file?".to_string());
                            state.status_type = Some(COLOR_WARNING);
                        }
                    }
                 });
            }

            if let Some(msg) = &state.status_message {
                 let color = state.status_type.unwrap_or(egui::Color32::WHITE);
                 ui.label(egui::RichText::new(msg).color(color));
            }
        });
    });

    ui.add_space(SECTION_SPACING);

    action
}
