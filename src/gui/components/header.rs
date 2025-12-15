use eframe::egui;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use tokio::sync::Notify;
use tracing::error;

use crate::constants::gui::*;
use crate::gui::components::profile_selector::{ProfileAction, ProfileSelector};
use crate::gui::state::{GuiTab, SharedState, StatusMessage};

/// Renders the global header panel containing daemon status, tabs, and profile controls
pub fn render(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    state: &mut SharedState,
    active_tab: &mut GuiTab,
    profile_selector: &mut ProfileSelector,
    #[cfg(target_os = "linux")] update_signal: &Arc<Notify>,
) -> ProfileAction {
    let mut action = ProfileAction::None;

    // Row 0: Daemon Status (Left) | Tabs (Right)
    ui.horizontal(|ui| {
        // Left side: Status indicators
        ui.colored_label(state.daemon_status.color(), state.daemon_status.label());
        if let Some(child) = &state.daemon {
            ui.label(format!("(PID: {})", child.id()));
        }
        if let Some(message) = &state.status_message {
            ui.add_space(10.0);
            ui.colored_label(message.color, &message.text);
        }

        // Right side: Navigation Tabs
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(5.0); // Padding from right edge
            if ui
                .add(egui::Button::new("Advanced").selected(*active_tab == GuiTab::Advanced))
                .clicked()
            {
                *active_tab = GuiTab::Advanced;
            }
            ui.add_space(5.0);
            if ui
                .add(egui::Button::new("General").selected(*active_tab == GuiTab::General))
                .clicked()
            {
                *active_tab = GuiTab::General;
            }
        });
    });
    ui.separator();

    // Row 1: Control Bar - Profile Selector (Left) | Save Actions (Right)
    ui.horizontal(|ui| {
        // Ensure the row has enough height for standard buttons/dropdowns
        ui.set_min_height(30.0);

        // 1. Left: Profile Dropdown
        action = profile_selector.render_dropdown(
            ui,
            &mut state.config,
            &mut state.selected_profile_idx,
        );

        // 2. Right: Save & Discard Buttons
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Discard button
            if ui.button("✖ Discard Changes").clicked() {
                state.discard_changes();
            }

            // Save button
            if ui.button("💾 Save & Apply").clicked() {
                if let Err(err) = state.save_config() {
                    error!(error = ?err, "Failed to save config");
                    state.status_message = Some(StatusMessage {
                        text: format!("Save failed: {err}"),
                        color: COLOR_ERROR,
                    });
                } else {
                    state.reload_daemon_config();
                    #[cfg(target_os = "linux")]
                    update_signal.notify_one();
                }
            }
        });
    });

    // Row 2: Profile Actions | Config Status
    ui.horizontal(|ui| {
        profile_selector.render_buttons(ui, &state.config, state.selected_profile_idx);

        // Status text aligned to the right
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(message) = &state.config_status_message {
                ui.colored_label(message.color, &message.text);
            } else if state.settings_changed {
                ui.colored_label(COLOR_WARNING, "Unsaved changes");
            }
        });
    });

    ui.add_space(5.0);

    // Handle Dialogs (Context level)
    let dialog_action =
        profile_selector.render_dialogs(ctx, &mut state.config, &mut state.selected_profile_idx);

    if !matches!(dialog_action, ProfileAction::None) {
        action = dialog_action;
    }

    action
}
