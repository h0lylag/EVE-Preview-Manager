//! Manager-wide settings, profile behavior, and configuration backup controls.

use crate::common::constants::manager_ui::*;
use crate::config::backup::BackupManager;
use crate::config::profile::{GlobalSettings, LoggedOutUnidentifiedCycleMode, Profile};

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

fn helper_label(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).small().weak()).wrap());
}

fn setting_row(
    ui: &mut egui::Ui,
    value: &mut bool,
    label: &str,
    helper: &str,
    action: &mut BehaviorSettingsAction,
) {
    if ui.checkbox(value, label).changed() {
        *action = BehaviorSettingsAction::SettingsChanged;
    }
    helper_label(ui, helper);
    ui.add_space(ITEM_SPACING);
}

fn behavior_settings(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    global: &mut GlobalSettings,
    action: &mut BehaviorSettingsAction,
) {
    ui.label(egui::RichText::new("Behavior Settings").strong());
    ui.add_space(ITEM_SPACING);

    if ui
        .checkbox(&mut global.minimize_to_tray, "Minimize to system tray")
        .changed()
    {
        *action = BehaviorSettingsAction::SettingsChanged;
    }
    if global.minimize_to_tray {
        ui.indent("start_minimized_to_tray_indent", |ui| {
            if ui
                .checkbox(
                    &mut global.start_minimized_to_tray,
                    "Start minimized to system tray",
                )
                .changed()
            {
                *action = BehaviorSettingsAction::SettingsChanged;
            }
        });
    }
    helper_label(
        ui,
        "When enabled, minimizing the window hides it to the system tray",
    );
    ui.add_space(ITEM_SPACING);

    if ui
        .checkbox(
            &mut profile.client_minimize_on_switch,
            "Minimize EVE clients when switching focus",
        )
        .changed()
    {
        *action = BehaviorSettingsAction::SettingsChanged;
    }
    if profile.client_minimize_on_switch {
        ui.indent("minimize_overlay_indent", |ui| {
            if ui
                .checkbox(
                    &mut profile.client_minimize_show_overlay,
                    "Show 'MINIMIZED' text overlay",
                )
                .changed()
            {
                *action = BehaviorSettingsAction::SettingsChanged;
            }
        });
    }
    helper_label(
        ui,
        "When clicking a thumbnail, minimize all other EVE clients",
    );
    ui.add_space(ITEM_SPACING);

    setting_row(
        ui,
        &mut profile.thumbnail_hide_not_focused,
        "Hide thumbnails when EVE loses focus",
        "When enabled, thumbnails disappear when no EVE window is focused",
        action,
    );

    setting_row(
        ui,
        &mut profile.thumbnail_auto_save_position,
        "Automatically save thumbnail positions",
        "When disabled, positions are only saved when you use 'Save Thumbnail Positions' from the system tray menu",
        action,
    );

    setting_row(
        ui,
        &mut profile.hotkey_cycle_reset_index,
        "Reset cycle order when switching groups",
        "When enabled, cycling through separate groups always starts at the first character",
        action,
    );

    setting_row(
        ui,
        &mut profile.thumbnail_preserve_position_on_swap,
        "New characters inherit thumbnail position",
        "New characters inherit thumbnail position from the logged-out character",
        action,
    );

    setting_row(
        ui,
        &mut profile.hotkey_logged_out_cycle,
        "Include remembered logged-out clients in cycle hotkeys",
        "Clients that showed a character earlier in this session stay reachable.",
        action,
    );

    setting_row(
        ui,
        &mut profile.thumbnail_show_logged_out_character_name,
        "Show remembered names on logged-out clients",
        "Display the last character name; this does not enable cycling.",
        action,
    );

    let mut append_unidentified = profile.hotkey_logged_out_unidentified_cycle_mode
        == LoggedOutUnidentifiedCycleMode::AppendToGroups;
    if ui
        .checkbox(
            &mut append_unidentified,
            "Append unidentified clients to cycle groups",
        )
        .changed()
    {
        profile.hotkey_logged_out_unidentified_cycle_mode = if append_unidentified {
            LoggedOutUnidentifiedCycleMode::AppendToGroups
        } else {
            LoggedOutUnidentifiedCycleMode::SeparateHotkeys
        };
        *action = BehaviorSettingsAction::SettingsChanged;
    }
    helper_label(
        ui,
        "Include login-screen clients with no remembered character after group entries. Otherwise, use their dedicated hotkeys in Hotkeys.",
    );
    ui.add_space(ITEM_SPACING);

    ui.horizontal(|ui| {
        ui.label("Thumbnail Snap Distance:");
        if ui
            .add(egui::Slider::new(&mut profile.thumbnail_snap_threshold, 0..=50).suffix(" px"))
            .changed()
        {
            *action = BehaviorSettingsAction::SettingsChanged;
        }
    });
    helper_label(ui, "Distance for edge/corner snapping (0 = disabled)");
}

fn backup_restore_settings(
    ui: &mut egui::Ui,
    global: &mut GlobalSettings,
    state: &mut BehaviorSettingsState,
    action: &mut BehaviorSettingsAction,
) {
    ui.label(egui::RichText::new("Backup & Restore").strong());
    ui.add_space(ITEM_SPACING);

    if ui
        .checkbox(&mut global.backup_enabled, "Enable Automatic Backups")
        .changed()
    {
        *action = BehaviorSettingsAction::SettingsChanged;
    }

    if global.backup_enabled {
        ui.add_space(ITEM_SPACING / 2.0);
        ui.indent("auto_backup_indent", |ui| {
            egui::Grid::new("auto_backup_settings_grid")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Interval (Days):");
                    if ui
                        .add(egui::Slider::new(&mut global.backup_interval_days, 1..=30))
                        .changed()
                    {
                        *action = BehaviorSettingsAction::SettingsChanged;
                    }
                    ui.end_row();

                    ui.label("Retention Count:");
                    if ui
                        .add(egui::Slider::new(
                            &mut global.backup_retention_count,
                            1..=100,
                        ))
                        .changed()
                    {
                        *action = BehaviorSettingsAction::SettingsChanged;
                    }
                    ui.end_row();
                });
            helper_label(ui, "Auto-backups only.");
        });
    }

    ui.add_space(ITEM_SPACING);
    ui.separator();
    ui.add_space(ITEM_SPACING);
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
    ui.label("Configuration Backups");
    ui.add_space(ITEM_SPACING / 2.0);

    ui.horizontal(|ui| {
        let combo_width = (ui.available_width() - 38.0).max(180.0);
        egui::ComboBox::from_id_salt("backup_selector")
            .selected_text(
                state
                    .selected_backup
                    .as_ref()
                    .and_then(|sel| state.backup_list.iter().find(|(f, _)| f == sel))
                    .map(|(_, d)| d.as_str())
                    .unwrap_or("Select a backup..."),
            )
            .width(combo_width)
            .show_ui(ui, |ui| {
                for (filename, display) in &state.backup_list {
                    ui.selectable_value(
                        &mut state.selected_backup,
                        Some(filename.clone()),
                        display,
                    );
                }
            });

        if ui
            .button("🔄")
            .on_hover_text("Refresh backup list")
            .clicked()
        {
            state.refresh_backups();
        }
    });

    let selected_opt = state.selected_backup.clone();
    if let Some(selected) = selected_opt {
        ui.add_space(ITEM_SPACING / 2.0);
        if state.show_restore_confirm {
            ui.horizontal(|ui| {
                if ui
                    .button(egui::RichText::new("YES, RESTORE").color(COLOR_ERROR))
                    .clicked()
                {
                    match BackupManager::restore_backup(&selected, None) {
                        Ok(_) => {
                            state.status_message = Some(
                                "Backup restored on disk. Reloading configuration.".to_string(),
                            );
                            state.status_type = Some(COLOR_SUCCESS);
                            state.show_restore_confirm = false;
                            *action = BehaviorSettingsAction::RestoreTriggered;
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
        } else if state.show_delete_confirm {
            ui.horizontal(|ui| {
                if ui
                    .button(egui::RichText::new("YES, DELETE").color(COLOR_ERROR))
                    .clicked()
                {
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
        } else {
            ui.horizontal(|ui| {
                if ui.button("📥 Restore").clicked() {
                    state.show_restore_confirm = true;
                    state.show_delete_confirm = false;
                    state.status_message = Some("WARNING: Overwrite current config?".to_string());
                    state.status_type = Some(COLOR_WARNING);
                }
                if ui.button("🗑 Delete").clicked() {
                    state.show_delete_confirm = true;
                    state.show_restore_confirm = false;
                    state.status_message = Some("WARNING: Delete file?".to_string());
                    state.status_type = Some(COLOR_WARNING);
                }
            });
        }
    }

    if let Some(msg) = &state.status_message {
        ui.add_space(ITEM_SPACING);
        let color = state.status_type.unwrap_or(egui::Color32::WHITE);
        ui.label(egui::RichText::new(msg).color(color));
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
        columns[0].group(|ui| {
            ui.set_min_width(ui.available_width());
            behavior_settings(ui, profile, global, &mut action);
        });

        columns[1].group(|ui| {
            ui.set_min_width(ui.available_width());
            backup_restore_settings(ui, global, state, &mut action);
        });
    });

    ui.add_space(SECTION_SPACING);

    action
}
