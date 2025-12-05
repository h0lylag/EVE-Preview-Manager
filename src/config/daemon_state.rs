//! Persistent state configuration for preview daemon
//!
//! Runtime state extracted from JSON profile config.
//! The daemon loads the selected profile and global settings at startup,
//! then maintains runtime character positions synchronized with the JSON file.

use anyhow::{Context, Result};
// serde derives aren't needed in this module (profile config is parsed elsewhere)
// keep serde usages local to the config/profile module
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};
use x11rb::protocol::render::Color;

use crate::color::{HexColor, Opacity};
use crate::config::profile::SaveStrategy;
use crate::types::{CharacterSettings, Position, TextOffset};


// ==============================================================================
// Daemon Runtime State
// ==============================================================================

/// Immutable display settings (loaded once at startup)
/// Can be borrowed by Thumbnails without RefCell
#[derive(Debug, Clone)]
/// Shared display configuration for all thumbnails
/// Per-character dimensions are stored in CharacterSettings, not here
pub struct DisplayConfig {
    pub opacity: u32,
    pub border_size: u16,
    pub border_color: Color,
    pub text_offset: TextOffset,
    pub text_color: u32,
    pub hide_when_no_focus: bool,
}

/// Daemon runtime state - holds selected profile + global settings
/// This is NOT serialized - it's built from the JSON config at runtime
#[derive(Debug)]
pub struct PersistentState {
    // Visual settings from selected profile
    pub profile: crate::config::profile::Profile,
    
    // Behavior settings that apply globally
    pub global: crate::config::profile::GlobalSettings,
    
    // Runtime character positions (synced with profile)
    pub character_positions: HashMap<String, CharacterSettings>,
}

impl PersistentState {
    fn config_path() -> PathBuf {
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        path.push(crate::constants::config::APP_DIR);
        path.push(crate::constants::config::FILENAME);
        path
    }

    /// Get default thumbnail dimensions for screen size
    pub fn default_thumbnail_size(&self, _screen_width: u16, _screen_height: u16) -> (u16, u16) {
        // Use configured default dimensions from global settings
        (self.global.default_thumbnail_width, self.global.default_thumbnail_height)
    }

    /// Build DisplayConfig from current settings
    /// Returns a new DisplayConfig that can be used independently
    /// Note: Per-character dimensions are not included here - they're in CharacterSettings
    pub fn build_display_config(&self) -> DisplayConfig {
        // Parse colors from hex strings using color module
        // Supports both 6-digit (RRGGBB) and 8-digit (AARRGGBB) formats
        // 6-digit format automatically gets full opacity (FF) prepended
        // Optional '#' prefix is supported but not required
        let border_color = HexColor::parse(&self.profile.border_color)
            .map(|c| c.to_x11_color())
            .unwrap_or_else(|| {
                error!(border_color = %self.profile.border_color, "Invalid border_color hex, using default");
                HexColor::from_argb32(0xFFFF0000).to_x11_color()
            });
        
        let text_color = HexColor::parse(&self.profile.text_color)
            .map(|c| c.argb32())  // Use raw ARGB, not premultiplied
            .unwrap_or_else(|| {
                error!(text_color = %self.profile.text_color, "Invalid text_color hex, using default");
                HexColor::from_argb32(0xFF_FF_FF_FF).argb32()
            });
        
        let opacity = Opacity::from_percent(self.profile.opacity_percent).to_argb32();
        
        DisplayConfig {
            opacity,
            border_size: self.profile.border_size,
            border_color,
            text_offset: TextOffset::from_border_edge(self.profile.text_x, self.profile.text_y),
            text_color,
            hide_when_no_focus: self.global.hide_when_no_focus,
        }
    }
    pub fn load() -> Self {
        // Load new profile-based config format
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

        // No config file - create default and write it
        error!(path = %config_path.display(), "No config file found. Please run the GUI manager first to create a profile.");
    error!("Run: eve-project-manager");
        std::process::exit(1);
    }

    /// Convert from profile-based Config to daemon PersistentState
    /// Simply extracts the selected profile and global settings - no conversion needed
    fn from_profile_config(config: crate::config::profile::Config) -> Self {
        // Find the selected profile
        let profile = config.profiles
            .iter()
            .find(|p| p.name == config.global.selected_profile)
            .or_else(|| config.profiles.first())
            .expect("Config must have at least one profile")
            .clone();
        
        info!(profile = %profile.name, "Using profile for daemon settings");
        
        // Just clone the structs - no conversion!
        PersistentState {
            profile: profile.clone(),
            global: config.global.clone(),
            character_positions: profile.character_positions.clone(),
        }
    }

    /// Load config with screen size for smart defaults
    /// Used when X11 connection is available before config load
    /// Note: Dimensions are now per-character, auto-detected at runtime, not during config load
    pub fn load_with_screen(_screen_width: u16, _screen_height: u16) -> Self {
        // Just load normally - dimensions are handled per-character at runtime
        Self::load()
    }

    /// Save character positions to the profile config
    /// This only updates character_positions, preserving all other profile settings
    pub fn save(&self) -> Result<()> {
        // Load the profile-based config
        let config_path = Self::config_path();
        let mut profile_config = if let Ok(contents) = fs::read_to_string(&config_path) {
            serde_json::from_str::<crate::config::profile::Config>(&contents)
                .context("Failed to parse profile config for save")?
        } else {
            // No config exists, create default
            crate::config::profile::Config::default()
        };
        
        // Update ONLY character positions in the selected profile
        // Preserve all other settings (they come from GUI)
        let selected_name = profile_config.global.selected_profile.clone();
        let profile_idx = profile_config.profiles
            .iter()
            .position(|p| p.name == selected_name)
            .unwrap_or(0);
        
        // Merge character positions: keep existing positions, add/update only those we have
        let profile_positions = &mut profile_config.profiles[profile_idx].character_positions;
        for (char_name, char_settings) in &self.character_positions {
            profile_positions.insert(char_name.clone(), *char_settings);
        }
        
        // Save the updated profile config (daemon owns character positions)
        profile_config.save_with_strategy(SaveStrategy::OverwriteCharacterPositions)
    }

    /// Handle character name change (login/logout)
    /// Returns new position if the new character has a saved position
    /// Accepts current thumbnail dimensions to ensure they're saved correctly
    pub fn handle_character_change(
        &mut self,
        old_name: &str,
        new_name: &str,
        current_position: Position,
        current_width: u16,
        current_height: u16,
    ) -> Result<Option<Position>> {
    info!(old = %old_name, new = %new_name, "Character change");
        
        // Save old character's position and current dimensions (if auto-save enabled)
        if !old_name.is_empty() && self.profile.auto_save_thumbnail_positions {
            let settings = CharacterSettings::new(
                current_position.x, 
                current_position.y, 
                current_width, 
                current_height
            );
            self.character_positions.insert(old_name.to_string(), settings);
            
            // Save to disk
            self.save()
                .context(format!("Failed to save config after character change from '{}' to '{}'", old_name, new_name))?;
        } else if !old_name.is_empty() {
            // Auto-save disabled - still update in-memory state for potential manual save
            let settings = CharacterSettings::new(
                current_position.x, 
                current_position.y, 
                current_width, 
                current_height
            );
            self.character_positions.insert(old_name.to_string(), settings);
        }
        
        // Return new position if we have one saved for the new character
        if !new_name.is_empty()
            && let Some(settings) = self.character_positions.get(new_name) {
                info!(character = %new_name, x = settings.x, y = settings.y, "Moving to saved position for character");
                return Ok(Some(settings.position()));
            }
        
        // Character logged out OR new character with no saved position → keep current position
        Ok(None)
    }

    // helper removed: parse_num was an env-var parsing helper (hex/decimal) but is
    // not used by the daemon runtime. If we need this behavior later, reintroduce
    // a small helper in a shared util module.
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create test PersistentState with visual + behavior settings
    fn test_state(
        opacity_percent: u8,
        border_size: u16,
        border_color: &str,
        text_x: i16,
        text_y: i16,
        text_color: &str,
        hide_when_no_focus: bool,
        snap_threshold: u16,
    ) -> PersistentState {
        use crate::config::profile::{GlobalSettings, Profile};

        PersistentState {
            profile: Profile {
                name: "Test Profile".to_string(),
                description: String::new(),
                opacity_percent,
                border_enabled: true,
                border_size,
                border_color: border_color.to_string(),
                text_size: 18,
                text_x,
                text_y,
                text_color: text_color.to_string(),
                text_font_family: String::new(),
                cycle_forward_keys: None,
                cycle_backward_keys: None,
                selected_hotkey_device: None,
                cycle_group: vec![],
                include_logged_out_in_cycle: false,
                character_positions: HashMap::new(),
            },
            global: GlobalSettings {
                selected_profile: "Test Profile".to_string(),
                window_width: 600,
                window_height: 800,
                window_x: None,
                window_y: None,
                minimize_clients_on_switch: false,
                hotkey_require_eve_focus: true,
                hide_when_no_focus,
                snap_threshold,
                preserve_thumbnail_position_on_swap: false,
                default_thumbnail_width: 480,
                default_thumbnail_height: 270,
            },
            character_positions: HashMap::new(),
        }
    }

    #[test]
    fn test_build_display_config_valid_colors() {
        let state = test_state(
            75,  // opacity_percent
            3,   // border_size
            "#FF00FF00",  // Green border
            15,  // text_x
            25,  // text_y
            "#FFFFFFFF",  // White text color
            true,  // hide_when_no_focus
            20,  // snap_threshold
        );

        let config = state.build_display_config();
        assert_eq!(config.border_size, 3);
        assert_eq!(config.text_offset.x, 15);
        assert_eq!(config.text_offset.y, 25);
        assert_eq!(config.hide_when_no_focus, true);
        
        // Opacity: 75% → 0xBF
        assert_eq!(config.opacity, 0xBF000000);
        
        // Border color: #FF00FF00 → Color { red: 0, green: 65535, blue: 0, alpha: 65535 }
        assert_eq!(config.border_color.red, 0);
        assert_eq!(config.border_color.green, 65535);
        assert_eq!(config.border_color.blue, 0);
        assert_eq!(config.border_color.alpha, 65535);
    }

    #[test]
    fn test_build_display_config_invalid_colors_fallback() {
        let state = test_state(
            100,  // opacity_percent
            5,    // border_size
            "invalid",  // invalid border color
            10,   // text_x
            20,   // text_y
            "also_invalid",  // invalid text color
            false,  // hide_when_no_focus
            15,  // snap_threshold
        );

        let config = state.build_display_config();
        
        // Opacity: 100% → 0xFF
        assert_eq!(config.opacity, 0xFF000000);
        
        // Default border_color: 0xFFFF0000 (opaque red)
        // Alpha conversion: 0xFF (255) * 257 = 65535 in 16-bit
        assert_eq!(config.border_color.red, 65535);
        assert_eq!(config.border_color.blue, 0);
        assert_eq!(config.border_color.alpha, 65535);
    }

    #[test]
    fn test_update_position_with_character_name() {
        let mut state = test_state(
            75, 3, "#FF00FF00", 10, 20, "#FFFFFFFF", false, 15,
        );

        // Update position with dimensions
        let _ = state.update_position("TestChar", 100, 200, 480, 270);
        
        let settings = state.character_positions.get("TestChar").unwrap();
        assert_eq!(settings.x, 100);
        assert_eq!(settings.y, 200);
        assert_eq!(settings.dimensions.width, 480);
        assert_eq!(settings.dimensions.height, 270);
    }

    #[test]
    fn test_update_position_empty_name_ignored() {
        let mut state = test_state(
            75, 3, "#FF00FF00", 10, 20, "#FFFFFFFF", false, 15,
        );

        let _ = state.update_position("", 300, 400, 480, 270);
        
        // Empty name should not be inserted
        assert!(state.character_positions.is_empty());
    }

    #[test]
    fn test_handle_character_change_both_names() {
        let mut state = test_state(
            75, 3, "#FF00FF00", 10, 20, "#FFFFFFFF", false, 15,
        );

        state.character_positions.insert("NewChar".to_string(), CharacterSettings::new(500, 600, 240, 135));

        let current_pos = Position::new(100, 200);
        let result = state.handle_character_change("OldChar", "NewChar", current_pos, 480, 270);

        // Should save old position AND dimensions
        let old_settings = state.character_positions.get("OldChar").unwrap();
        assert_eq!(old_settings.x, 100);
        assert_eq!(old_settings.y, 200);
        assert_eq!(old_settings.dimensions.width, 480);
        assert_eq!(old_settings.dimensions.height, 270);

        // Should return the new character's saved position
        if let Ok(Some(new_pos)) = result {
            assert_eq!(new_pos.x, 500);
            assert_eq!(new_pos.y, 600);
        }

        // Verify the new position exists in the map
        let new_settings = state.character_positions.get("NewChar").unwrap();
        assert_eq!(new_settings.x, 500);
        assert_eq!(new_settings.y, 600);
    }

    #[test]
    fn test_handle_character_change_logout() {
        let mut state = test_state(
            75, 3, "#FF00FF00", 10, 20, "#FFFFFFFF", false, 15,
        );

        let current_pos = Position::new(300, 400);
        let result = state.handle_character_change("LoggingOut", "", current_pos, 480, 270);

        // Should save old position AND dimensions
        let settings = state.character_positions.get("LoggingOut").unwrap();
        assert_eq!(settings.x, 300);
        assert_eq!(settings.y, 400);
        assert_eq!(settings.dimensions.width, 480);
        assert_eq!(settings.dimensions.height, 270);

        // Logging out: should return None (keep current position)
        if let Ok(new_pos) = result {
            assert_eq!(new_pos, None);
        }
    }

    #[test]
    fn test_handle_character_change_new_character_no_saved_position() {
        let mut state = test_state(
            75, 3, "#FF00FF00", 10, 20, "#FFFFFFFF", false, 15,
        );

        let current_pos = Position::new(700, 800);
        let result = state.handle_character_change("", "BrandNewChar", current_pos, 480, 270);

        // Empty old name not saved
        assert!(state.character_positions.is_empty());

        // New character with no saved position: should return None (keep current position)
        if let Ok(new_pos) = result {
            assert_eq!(new_pos, None);
        }
    }
}
