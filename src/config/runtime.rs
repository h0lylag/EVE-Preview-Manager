//! Runtime configuration for the preview daemon
//!
//! Loads the selected profile and global settings at startup,
//! then maintains character positions synchronized with the config file.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use x11rb::protocol::render::Color;

use crate::color::{HexColor, Opacity};
use crate::config::profile::SaveStrategy;
use crate::types::{CharacterSettings, Position, TextOffset};

/// Shared display configuration for all thumbnails
/// Immutable after creation - can be borrowed without RefCell
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
    pub character_settings: std::collections::HashMap<String, crate::types::CharacterSettings>,
    pub inactive_border_color: Color,
    pub inactive_border_size: u16,
    pub minimized_overlay_enabled: bool,
}
/// Daemon runtime configuration - holds selected profile settings
/// Built from the JSON config at runtime, not serialized directly
#[derive(Clone)]
pub struct DaemonConfig {
    pub profile: crate::config::profile::Profile,
    pub character_thumbnails: HashMap<String, CharacterSettings>,
    // Map of HotkeyBinding -> Profile Name
    pub profile_hotkeys: HashMap<crate::config::HotkeyBinding, String>,
}

impl DaemonConfig {
    fn config_path() -> PathBuf {
        #[cfg(not(test))]
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        #[cfg(test)]
        let mut path = std::env::temp_dir().join("eve-preview-manager-test");

        path.push(crate::constants::config::APP_DIR);
        path.push(crate::constants::config::FILENAME);
        path
    }

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
            character_settings: self.profile.character_thumbnails.clone(),
        }
    }

    pub fn load() -> Self {
        let config_path = Self::config_path();
        if let Ok(contents) = fs::read_to_string(&config_path) {
            match serde_json::from_str::<crate::config::profile::Config>(&contents) {
                Ok(profile_config) => {
                    info!("Loading daemon config from profile-based format");
                    return Self::from_profile_config(profile_config);
                }
                Err(e) => {
                    error!(path = %config_path.display(), error = %e, "Failed to parse config file");
                    error!(path = %config_path.display(), "Please fix the syntax errors in your config file.");
                    std::process::exit(1);
                }
            }
        }

        error!(path = %config_path.display(), "No config file found. Please run the GUI manager first to create a profile.");
        error!("Run: eve-preview-manager");
        std::process::exit(1);
    }

    fn from_profile_config(config: crate::config::profile::Config) -> Self {
        let profile = config
            .profiles
            .iter()
            .find(|p| p.profile_name == config.global.selected_profile)
            .or_else(|| config.profiles.first())
            .expect("Config must have at least one profile")
            .clone();

        info!(profile = %profile.profile_name, "Using profile for daemon settings");

        // aggregated map of hotkeys -> profile names
        let mut profile_hotkeys = HashMap::new();
        for p in &config.profiles {
            if let Some(binding) = &p.hotkey_profile_switch {
                profile_hotkeys.insert(binding.clone(), p.profile_name.clone());
            }
        }

        DaemonConfig {
            profile: profile.clone(),
            character_thumbnails: profile.character_thumbnails.clone(),
            profile_hotkeys,
        }
    }

    /// Load config with screen size available (for future smart defaults)
    pub fn load_with_screen(_screen_width: u16, _screen_height: u16) -> Self {
        Self::load()
    }

    /// Save character positions to the profile config
    pub fn save(&self) -> Result<()> {
        let profile_config = self.prepare_config_to_save()?;
        profile_config.save_with_strategy(SaveStrategy::Overwrite)
    }

    /// Save character positions using a specific strategy
    pub fn save_with_strategy(&self, strategy: SaveStrategy) -> Result<()> {
        let profile_config = self.prepare_config_to_save()?;
        profile_config.save_with_strategy(strategy)
    }

    /// Prepare config for saving by merging current daemon state into a fresh disk config
    fn prepare_config_to_save(&self) -> Result<crate::config::profile::Config> {
        let config_path = Self::config_path();
        let mut profile_config = if let Ok(contents) = fs::read_to_string(&config_path) {
            serde_json::from_str::<crate::config::profile::Config>(&contents)
                .context("Failed to parse profile config for save")?
        } else {
            crate::config::profile::Config::default()
        };

        let selected_name = profile_config.global.selected_profile.clone();
        let profile_idx = profile_config
            .profiles
            .iter()
            .position(|p| p.profile_name == selected_name)
            .unwrap_or(0);

        let profile_positions = &mut profile_config.profiles[profile_idx].character_thumbnails;
        for (char_name, char_settings) in &self.character_thumbnails {
            if !char_name.is_empty() {
                profile_positions
                    .entry(char_name.clone())
                    .and_modify(|e| {
                        // Update ONLY the position/dimensions from the daemon's state.
                        // Essential to PRESERVE any override settings (colors, sizes) that were loaded from disk.
                        e.x = char_settings.x;
                        e.y = char_settings.y;
                        e.dimensions = char_settings.dimensions;
                    })
                    .or_insert_with(|| char_settings.clone());
            }
        }

        // Explicitly remove any empty key that might have been loaded from a corrupt config
        // Sanitize ALL profiles to ensure the config file is clean globally
        for profile in &mut profile_config.profiles {
            profile.character_thumbnails.remove("");
        }

        Ok(profile_config)
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

        if !old_name.is_empty() && self.profile.thumbnail_auto_save_position {
            let settings = CharacterSettings::new(
                current_position.x,
                current_position.y,
                current_width,
                current_height,
            );
            self.character_thumbnails
                .insert(old_name.to_string(), settings);

            self.save().context(format!(
                "Failed to save config after character change from '{}' to '{}'",
                old_name, new_name
            ))?;
        } else if !old_name.is_empty() {
            let settings = CharacterSettings::new(
                current_position.x,
                current_position.y,
                current_width,
                current_height,
            );
            self.character_thumbnails
                .insert(old_name.to_string(), settings);
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

    /// Save a newly detected character to the config file while preserving all other state on disk.
    ///
    /// This method is used when "Auto-save thumbnail positions" is DISABLED. It ensures that
    /// the new character is added to the config (so the GUI can see it), but any other
    /// pending in-memory position changes for other characters are NOT written to disk.
    ///
    /// # Logic
    /// 1. Loads the current configuration from disk (source of truth).
    /// 2. Inserts ONLY the new character's settings.
    /// 3. Saves back to disk using the `PreserveCharacterPositions` strategy (redundant but safe).
    pub fn save_new_character(&self, char_name: &str, settings: CharacterSettings) -> Result<()> {
        info!(character = %char_name, "Safe-saving new character to config (preserving disk state)");

        let config_path = Self::config_path();

        // 1. Load current on-disk config to ensure we don't clobber unrelated changes
        let mut disk_config = if let Ok(contents) = fs::read_to_string(&config_path) {
            serde_json::from_str::<crate::config::profile::Config>(&contents)
                .context("Failed to parse existing config for safe update")?
        } else {
            // Should theoretically not happen if app is running, but handle gracefully
            crate::config::profile::Config::default()
        };

        // 2. Find the active profile in the disk config
        let selected_name = disk_config.global.selected_profile.clone();
        if let Some(profile) = disk_config
            .profiles
            .iter_mut()
            .find(|p| p.profile_name == selected_name)
        {
            // 3. Insert ONLY the new character
            profile
                .character_thumbnails
                .insert(char_name.to_string(), settings);
        } else {
            // Fallback: modify the first profile if selected not found (unlikely)
            if let Some(profile) = disk_config.profiles.first_mut() {
                profile
                    .character_thumbnails
                    .insert(char_name.to_string(), settings);
            }
        }

        // 4. Save back to disk
        // We can just save the modified disk_config directly
        disk_config
            .save_with_strategy(SaveStrategy::Overwrite)
            .context("Failed to save config with new character")
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
                cycle_groups: vec![crate::config::profile::CycleGroup::default_group()],
                custom_windows: Vec::new(),
                character_hotkeys: HashMap::new(),
                hotkey_backend: crate::config::HotkeyBackendType::X11,
                thumbnail_enabled: true,
                character_thumbnails: HashMap::new(),
                hotkey_profile_switch: None,
                hotkey_toggle_skip: None,
                client_minimize_show_overlay: false,
            },
            character_thumbnails: HashMap::new(),
            profile_hotkeys: HashMap::new(),
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
