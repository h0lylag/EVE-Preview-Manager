//! Runtime configuration for the preview daemon
//!
//! Loads the selected profile and global settings at startup,
//! then maintains character positions synchronized with the config file.

use anyhow::Result;
use std::collections::HashMap;

use tracing::{error, info};
use x11rb::protocol::render::Color;

use crate::common::color::{HexColor, Opacity};
use crate::common::types::{CharacterSettings, Position, TextOffset};

/// Snapshot of display settings for the renderer.
#[derive(Debug, Clone)]
pub struct DisplayConfig {
    pub enabled: bool,
    pub opacity: u32, // 0-255 mapped to 0-0xFFFFFFFF
    pub active_border_size: u16,
    pub active_border_color: Color,
    pub text_offset: TextOffset,
    pub text_color: u32,
    pub hide_when_no_focus: bool,
    pub inactive_border_enabled: bool,

    /// Map of character name -> settings (overrides, aliases, etc)
    pub character_settings:
        std::collections::HashMap<String, crate::common::types::CharacterSettings>,
    pub inactive_border_color: Color,
    pub inactive_border_size: u16,
    pub minimized_overlay_enabled: bool,
}
use serde::{Deserialize, Serialize};

/// Daemon runtime configuration - holds selected profile settings
/// Built from the JSON config at runtime, not serialized directly
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct DaemonConfig {
    pub profile: crate::config::profile::Profile,
    /// Runtime overrides (position/dimensions) distinct from persisting profile.
    pub character_thumbnails: HashMap<String, CharacterSettings>,
    /// Active custom source settings
    pub custom_source_thumbnails: HashMap<String, CharacterSettings>,
    /// Flattened map of hotkey bindings to profile names
    pub profile_hotkeys: HashMap<crate::config::HotkeyBinding, String>,
    // Ephemeral state: used to temporarily hide previews via hotkey
    pub runtime_hidden: bool,
}

impl DaemonConfig {
    /// Get default thumbnail dimensions from profile settings
    pub fn default_thumbnail_size(&self, _screen_width: u16, _screen_height: u16) -> (u16, u16) {
        (
            self.profile.thumbnail_default_width,
            self.profile.thumbnail_default_height,
        )
    }

    /// Build DisplayConfig from current settings
    pub fn build_display_config(&self) -> DisplayConfig {
        let active_border_color = HexColor::parse(&self.profile.thumbnail_active_border_color)
            .map(|c| c.to_x11_color())
            .unwrap_or_else(|| {
                error!(active_border_color = %self.profile.thumbnail_active_border_color, "Invalid active_border_color hex, using default");
                HexColor::from_argb32(0xFFFF0000).to_x11_color()
            });

        let text_color = HexColor::parse(&self.profile.thumbnail_text_color)
            .map(|c| c.argb32())
            .unwrap_or_else(|| {
                error!(text_color = %self.profile.thumbnail_text_color, "Invalid text_color hex, using default");
                HexColor::from_argb32(0xFF_FF_FF_FF).argb32()
            });

        let inactive_border_color = HexColor::parse(&self.profile.thumbnail_inactive_border_color)
            .map(|c| c.to_x11_color())
            .unwrap_or_else(|| {
                // If invalid, default to transparent
                HexColor::from_argb32(0x00000000).to_x11_color()
            });

        let opacity = Opacity::from_percent(self.profile.thumbnail_opacity).to_argb32();

        let mut character_settings = self.profile.character_thumbnails.clone();

        // 1. Merge saved custom source thumbnails (positions/modes)
        character_settings.extend(self.profile.custom_source_thumbnails.clone());

        // 2. Apply Custom Window Rules as default overrides
        // If a custom source has a rule, we ensure its overrides are applied to the settings map.
        // This handles cases where a custom source hasn't been "saved" (moved) yet but has config rule overrides.
        for rule in &self.profile.custom_windows {
            character_settings
                .entry(rule.alias.clone())
                .and_modify(|settings| {
                    // Update existing settings with rule overrides if present (Rule takes precedence or fills gaps?)
                    // Usually saved settings (user edits via context menu) should win,
                    // BUT for custom sources, the "Rule" IS the user edit for these overrides effectively.
                    // The UI writes to the Rule. So the Rule is authoritative for overrides.
                    if rule.active_border_color.is_some() {
                        settings.override_active_border_color = rule.active_border_color.clone();
                    }
                    if rule.inactive_border_color.is_some() {
                        settings.override_inactive_border_color =
                            rule.inactive_border_color.clone();
                    }
                    if rule.active_border_size.is_some() {
                        settings.override_active_border_size = rule.active_border_size;
                    }
                    if rule.inactive_border_size.is_some() {
                        settings.override_inactive_border_size = rule.inactive_border_size;
                    }
                    if rule.text_color.is_some() {
                        settings.override_text_color = rule.text_color.clone();
                    }
                    if rule.preview_mode.is_some() {
                        settings.preview_mode = rule.preview_mode.clone().unwrap_or_default();
                    }
                })
                .or_insert_with(|| {
                    // Create minimal settings from rule
                    crate::common::types::CharacterSettings {
                        x: 0,
                        y: 0, // Will be positioned by spawn logic if 0
                        dimensions: crate::common::types::Dimensions::new(
                            rule.default_width,
                            rule.default_height,
                        ),
                        alias: None,
                        notes: None,
                        override_active_border_color: rule.active_border_color.clone(),
                        override_inactive_border_color: rule.inactive_border_color.clone(),
                        override_active_border_size: rule.active_border_size,
                        override_inactive_border_size: rule.inactive_border_size,
                        override_text_color: rule.text_color.clone(),
                        preview_mode: rule.preview_mode.clone().unwrap_or_default(),
                        exempt_from_minimize: false,
                    }
                });
        }

        DisplayConfig {
            enabled: self.profile.thumbnail_enabled,
            opacity,
            active_border_size: if self.profile.thumbnail_active_border {
                self.profile.thumbnail_active_border_size
            } else {
                0
            },
            active_border_color,
            text_offset: TextOffset::from_border_edge(
                self.profile.thumbnail_text_x,
                self.profile.thumbnail_text_y,
            ),
            text_color,
            hide_when_no_focus: self.profile.thumbnail_hide_not_focused,
            inactive_border_enabled: self.profile.thumbnail_inactive_border,
            inactive_border_color,
            inactive_border_size: if self.profile.thumbnail_inactive_border {
                self.profile.thumbnail_inactive_border_size
            } else {
                0
            },
            minimized_overlay_enabled: self.profile.client_minimize_show_overlay,
            character_settings,
        }
    }

    /// Handle character name change (login/logout)
    /// Returns new position if the new character has a saved position
    pub fn handle_character_change(
        &mut self,
        old_name: &str,
        new_name: &str,
        current_position: Position,
        current_width: u16,
        current_height: u16,
    ) -> Result<Option<CharacterSettings>> {
        info!(old = %old_name, new = %new_name, "Character change");

        if !old_name.is_empty() {
            let mut settings = self
                .character_thumbnails
                .get(old_name)
                .cloned()
                .unwrap_or_else(|| {
                    CharacterSettings::new(
                        current_position.x,
                        current_position.y,
                        current_width,
                        current_height,
                    )
                });

            // Update session state (position) while preserving user customization (style/mode).
            settings.x = current_position.x;
            settings.y = current_position.y;
            settings.dimensions =
                crate::common::types::Dimensions::new(current_width, current_height);

            self.character_thumbnails
                .insert(old_name.to_string(), settings);
        }

        // NOTE: Refresh overrides from disk to respect external Manager changes (e.g. static mode).
        // Memory holds the authoritative window position, but disk holds the authoritative user config.
        if !new_name.is_empty()
            && let Ok(disk_config) = crate::config::profile::Config::load()
        {
            let pd_name = &self.profile.profile_name;
            if let Some(disk_profile) = disk_config
                .profiles
                .iter()
                .find(|p| &p.profile_name == pd_name)
                && let Some(disk_settings) = disk_profile.character_thumbnails.get(new_name)
            {
                self.character_thumbnails
                    .entry(new_name.to_string())
                    .and_modify(|mem_settings| {
                        mem_settings.preview_mode = disk_settings.preview_mode.clone();
                        mem_settings.alias = disk_settings.alias.clone();
                        mem_settings.notes = disk_settings.notes.clone();
                        mem_settings.override_active_border_color =
                            disk_settings.override_active_border_color.clone();
                        mem_settings.override_inactive_border_color =
                            disk_settings.override_inactive_border_color.clone();
                        mem_settings.override_active_border_size =
                            disk_settings.override_active_border_size;
                        mem_settings.override_inactive_border_size =
                            disk_settings.override_inactive_border_size;
                        mem_settings.override_text_color =
                            disk_settings.override_text_color.clone();
                    })
                    .or_insert_with(|| disk_settings.clone());
            }
        }

        if !new_name.is_empty()
            && let Some(settings) = self.character_thumbnails.get(new_name)
        {
            info!(
                character = %new_name,
                x = settings.x,
                y = settings.y,
                width = settings.dimensions.width,
                height = settings.dimensions.height,
                "Moving and resizing to saved settings for character"
            );
            return Ok(Some(settings.clone()));
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn test_config(
        opacity_percent: u8,
        border_size: u16,
        border_color: &str,
        text_x: i16,
        text_y: i16,
        text_color: &str,
        hide_when_no_focus: bool,
        snap_threshold: u16,
    ) -> DaemonConfig {
        use crate::config::profile::Profile;

        DaemonConfig {
            profile: Profile {
                profile_name: "Test Profile".to_string(),
                profile_description: String::new(),
                thumbnail_default_width: 480,
                thumbnail_default_height: 270,
                thumbnail_opacity: opacity_percent,
                thumbnail_active_border: border_size > 0, // In tests, valid size > 0 implies enabled
                thumbnail_active_border_size: border_size,
                thumbnail_active_border_color: border_color.to_string(),
                thumbnail_inactive_border: false,
                thumbnail_inactive_border_size: 0,
                thumbnail_inactive_border_color: "#00000000".to_string(),
                thumbnail_text_size: 18,
                thumbnail_text_x: text_x,
                thumbnail_text_y: text_y,
                thumbnail_text_color: text_color.to_string(),
                thumbnail_text_font: String::new(),
                thumbnail_auto_save_position: false,
                thumbnail_snap_threshold: snap_threshold,
                thumbnail_hide_not_focused: hide_when_no_focus,
                thumbnail_preserve_position_on_swap: false,
                client_minimize_on_switch: false,
                hotkey_input_device: None,
                hotkey_logged_out_cycle: false,
                hotkey_require_eve_focus: true,
                hotkey_cycle_reset_index: false,
                cycle_groups: vec![crate::config::profile::CycleGroup::default_group()],
                custom_windows: Vec::new(),
                character_hotkeys: HashMap::new(),
                hotkey_backend: crate::config::HotkeyBackendType::X11,
                thumbnail_enabled: true,
                character_thumbnails: HashMap::new(),
                custom_source_thumbnails: HashMap::new(),
                hotkey_profile_switch: None,
                hotkey_toggle_skip: None,
                hotkey_toggle_previews: None,
                client_minimize_show_overlay: false,
            },
            character_thumbnails: HashMap::new(),
            custom_source_thumbnails: HashMap::new(),
            profile_hotkeys: HashMap::new(),
            runtime_hidden: false,
        }
    }

    #[test]
    fn test_build_display_config_valid_colors() {
        let state = test_config(75, 3, "#FF00FF00", 15, 25, "#FFFFFFFF", true, 20);

        let config = state.build_display_config();
        assert_eq!(config.active_border_size, 3);
        assert_eq!(config.text_offset.x, 15);
        assert_eq!(config.text_offset.y, 25);
        assert!(config.hide_when_no_focus);
        assert_eq!(config.opacity, 0xBF000000);
        assert_eq!(config.active_border_color.red, 0);
        assert_eq!(config.active_border_color.green, 65535);
        assert_eq!(config.active_border_color.blue, 0);
        assert_eq!(config.active_border_color.alpha, 65535);
        assert!(!config.minimized_overlay_enabled);
    }

    #[test]
    fn test_build_display_config_border_disabled_override() {
        let mut state = test_config(100, 5, "invalid", 10, 20, "also_invalid", false, 15);
        // Explicitly disable border, even though size is 5
        state.profile.thumbnail_active_border = false;

        let config = state.build_display_config();

        // Should enforce size 0
        assert_eq!(config.active_border_size, 0);

        // Other defaults should still apply
        assert_eq!(config.opacity, 0xFF000000);
    }

    #[test]
    fn test_build_display_config_invalid_colors_fallback() {
        let state = test_config(100, 5, "invalid", 10, 20, "also_invalid", false, 15);

        let config = state.build_display_config();
        assert_eq!(config.opacity, 0xFF000000);
        assert_eq!(config.active_border_size, 5); // Enabled in test helper
        assert_eq!(config.active_border_color.red, 65535);
        assert_eq!(config.active_border_color.blue, 0);
        assert_eq!(config.active_border_color.alpha, 65535);
    }

    #[test]
    fn test_handle_character_change_both_names() {
        let mut state = test_config(75, 3, "#FF00FF00", 10, 20, "#FFFFFFFF", false, 15);

        state.character_thumbnails.insert(
            "NewChar".to_string(),
            CharacterSettings::new(500, 600, 240, 135),
        );

        let current_pos = Position::new(100, 200);
        let result = state.handle_character_change("OldChar", "NewChar", current_pos, 480, 270);

        let old_settings = state.character_thumbnails.get("OldChar").unwrap();
        assert_eq!(old_settings.x, 100);
        assert_eq!(old_settings.y, 200);
        assert_eq!(old_settings.dimensions.width, 480);
        assert_eq!(old_settings.dimensions.height, 270);

        if let Ok(Some(new_pos)) = result {
            assert_eq!(new_pos.x, 500);
            assert_eq!(new_pos.y, 600);
        }

        let new_settings = state.character_thumbnails.get("NewChar").unwrap();
        assert_eq!(new_settings.x, 500);
        assert_eq!(new_settings.y, 600);
    }

    #[test]
    fn test_handle_character_change_logout() {
        let mut state = test_config(75, 3, "#FF00FF00", 10, 20, "#FFFFFFFF", false, 15);

        let current_pos = Position::new(300, 400);
        let result = state.handle_character_change("LoggingOut", "", current_pos, 480, 270);

        let settings = state.character_thumbnails.get("LoggingOut").unwrap();
        assert_eq!(settings.x, 300);
        assert_eq!(settings.y, 400);
        assert_eq!(settings.dimensions.width, 480);
        assert_eq!(settings.dimensions.height, 270);

        if let Ok(new_pos) = result {
            assert_eq!(new_pos, None);
        }
    }

    #[test]
    fn test_handle_character_change_new_character_no_saved_position() {
        let mut state = test_config(75, 3, "#FF00FF00", 10, 20, "#FFFFFFFF", false, 15);

        let current_pos = Position::new(700, 800);
        let result = state.handle_character_change("", "BrandNewChar", current_pos, 480, 270);

        if let Ok(new_pos) = result {
            assert_eq!(new_pos, None);
        }
    }

    #[test]
    fn test_save_filters_empty_keys_logic() {
        let mut state = test_config(75, 3, "#FF00FF00", 10, 20, "#FFFFFFFF", false, 15);

        // 1. Verify handle_character_change doesn't insert empty old_name
        let _ = state.handle_character_change("", "NewChar", Position::new(0, 0), 100, 100);
        assert!(!state.character_thumbnails.contains_key(""));

        // 2. Verify it doesn't try to look up empty new_name
        let _ = state.handle_character_change("OldChar", "", Position::new(0, 0), 100, 100);
        assert!(!state.character_thumbnails.contains_key(""));
    }
}
