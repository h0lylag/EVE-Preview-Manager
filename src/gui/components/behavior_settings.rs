//! Behavior settings component (per-profile settings)

use crate::config::profile::Profile;
use crate::constants::gui::*;

use eframe::egui;

/// State for behavior settings UI
pub struct BehaviorSettingsState {
    // No remaining state fields
}

impl BehaviorSettingsState {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for BehaviorSettingsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders behavior settings UI and returns true if changes were made
pub fn ui(ui: &mut egui::Ui, profile: &mut Profile, _state: &mut BehaviorSettingsState) -> bool {
    let mut changed = false;

    // Behavior Settings (Global)
    ui.group(|ui| {
        ui.label(egui::RichText::new("Behavior Settings").strong());
        ui.add_space(ITEM_SPACING);

        // Minimize clients on switch
        if ui.checkbox(&mut profile.client_minimize_on_switch,
            "Minimize EVE clients when switching focus").changed() {
            changed = true;
        }

        if profile.client_minimize_on_switch {
            ui.indent("minimize_overlay_indent", |ui| {
                if ui.checkbox(&mut profile.client_minimize_show_overlay,
                    "Show 'MINIMIZED' text overlay").changed() {
                    changed = true;
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
            changed = true;
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
            changed = true;
        }

        ui.label(egui::RichText::new(
            "When disabled, positions are only saved when you use 'Save Thumbnail Positions' from the system tray menu")
            .small()
            .weak());

        ui.add_space(ITEM_SPACING);

        // Preserve thumbnail position on character swap
        if ui.checkbox(&mut profile.thumbnail_preserve_position_on_swap,
            "New characters inherit thumbnail position").changed() {
            changed = true;
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
                changed = true;
            }
        });

        ui.label(egui::RichText::new(
            "Distance for edge/corner snapping (0 = disabled)")
            .small()
            .weak());
    });

    ui.add_space(SECTION_SPACING);

    changed
}
