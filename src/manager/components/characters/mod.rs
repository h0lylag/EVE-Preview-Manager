use crate::config::profile::Profile;
use eframe::egui;

mod editor;
mod list;
mod modals;

/// State for character management UI
pub struct CharactersState {
    pub(crate) show_add_characters_popup: bool,
    pub(crate) character_selections:
        std::collections::HashMap<crate::config::profile::CycleSlot, bool>,
    pub(crate) expanded_rows: std::collections::HashMap<String, bool>,
    pub(crate) cached_overrides: std::collections::HashMap<String, CachedOverrides>,
    pub(crate) selected_cycle_group_index: usize,
    pub(crate) renaming_group_idx: Option<usize>,
    pub(crate) rename_buffer: String,
    pub(crate) rename_error: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct CachedOverrides {
    pub(crate) active_border_color: Option<String>,
    pub(crate) inactive_border_color: Option<String>,
    pub(crate) active_border_size: Option<u16>,
    pub(crate) inactive_border_size: Option<u16>,
    pub(crate) text_color: Option<String>,
}

impl CharactersState {
    pub fn new() -> Self {
        Self {
            show_add_characters_popup: false,
            character_selections: std::collections::HashMap::new(),
            expanded_rows: std::collections::HashMap::new(),
            cached_overrides: std::collections::HashMap::new(),
            selected_cycle_group_index: 0,
            renaming_group_idx: None,
            rename_buffer: String::new(),
            rename_error: None,
        }
    }

    pub fn load_from_profile(&mut self, _profile: &Profile) {
        self.cached_overrides.clear();
        self.renaming_group_idx = None;
        self.rename_buffer.clear();
        self.rename_error = None;
    }
}

impl Default for CharactersState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn ui(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    state: &mut CharactersState,
    hotkey_state: &mut crate::manager::components::hotkey_settings::HotkeySettingsState,
    profile_reloaded: bool,
) -> bool {
    if profile_reloaded {
        state.load_from_profile(profile);
    }
    let mut changed = false;

    if state.selected_cycle_group_index >= profile.cycle_groups.len() {
        state.selected_cycle_group_index = 0;
    }

    render_two_column_layout(ui, profile, state, hotkey_state, &mut changed);

    if state.show_add_characters_popup {
        modals::render_add_characters_modal(ui.ctx(), profile, state, &mut changed);
    }

    if hotkey_state.is_dialog_open() {
        changed |= crate::manager::components::hotkey_settings::render_key_capture_modal(
            ui,
            profile,
            hotkey_state,
        );
    }

    changed
}

fn render_two_column_layout(
    ui: &mut egui::Ui,
    profile: &mut Profile,
    state: &mut CharactersState,
    hotkey_state: &mut crate::manager::components::hotkey_settings::HotkeySettingsState,
    changed: &mut bool,
) {
    let spacing = ui.spacing().item_spacing.x;
    let total_width = ui.available_width() - spacing;
    let left_width = total_width * 0.4;
    let right_width = total_width * 0.6;

    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(left_width, ui.available_height()),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                editor::render_character_editor_column(ui, profile, state, hotkey_state, changed);
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(right_width, ui.available_height()),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                list::render_cycle_group_column(ui, profile, state, hotkey_state, changed);
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::Config;
    use crate::manager::components::profile_selector::ProfileSelector;
    use crate::manager::state::SharedState;

    #[test]
    fn config_reload_cancels_stale_group_rename_only_after_success() {
        for readable in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.json");
            let mut saved = Config::default();
            saved.profiles[0].cycle_groups[0].name = "Restored group".into();
            saved.save_to(&path).unwrap();
            if !readable {
                std::fs::write(&path, b"{broken").unwrap();
            }
            let mut shared = SharedState::at_path(Config::default(), &path);
            let mut editor = CharactersState::new();
            editor.renaming_group_idx = Some(0);
            editor.rename_buffer = "Stale draft".into();
            editor.rename_error = Some("Old error".into());
            assert_eq!(
                ProfileSelector::new().reload_config(&mut shared).is_ok(),
                readable
            );
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                super::ui(
                    ui,
                    &mut shared.config.profiles[0],
                    &mut editor,
                    &mut crate::manager::components::hotkey_settings::HotkeySettingsState::new(),
                    std::mem::take(&mut shared.characters_reload_pending),
                );
            });
            output.textures_delta.clear();
            if readable {
                assert!(
                    editor.renaming_group_idx.is_none(),
                    "successful reload must cancel the old rename target"
                );
                assert!(editor.rename_buffer.is_empty() && editor.rename_error.is_none());
                assert_eq!(
                    shared.config.profiles[0].cycle_groups[0].name,
                    "Restored group"
                );
            } else {
                assert_eq!(editor.renaming_group_idx, Some(0));
                assert_eq!(editor.rename_buffer, "Stale draft");
                assert_eq!(editor.rename_error.as_deref(), Some("Old error"));
            }
        }
    }
}
