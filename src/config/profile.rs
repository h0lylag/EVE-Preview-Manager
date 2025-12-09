//! Profile-based configuration for the GUI manager
//!
//! Supports multiple profiles, each containing visual settings (opacity, border, text),
//! hotkey bindings, and per-character thumbnail positions.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::info;

use crate::types::CharacterSettings;

/// Strategy for saving configuration files
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStrategy {
    /// Preserve character_thumbnails entries already on disk (GUI edits)
    PreserveCharacterPositions,
    /// Overwrite character_thumbnails with in-memory data (daemon updates)
    OverwriteCharacterPositions,
}

/// Top-level configuration with profile support
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub global: GlobalSettings,
    #[serde(default = "default_profiles")]
    pub profiles: Vec<Profile>,
}

/// Global application settings (applies to all profiles)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSettings {
    #[serde(default = "default_profile_name")]
    pub selected_profile: String,
    #[serde(default = "default_window_width")]
    pub window_width: u16,
    #[serde(default = "default_window_height")]
    pub window_height: u16,
}

/// Profile - A complete set of visual and behavioral settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub profile_name: String,
    #[serde(default)]
    pub profile_description: String,
    
    // Thumbnail default dimensions
    /// Default thumbnail width for new characters
    #[serde(default = "default_thumbnail_width")]
    pub thumbnail_default_width: u16,
    /// Default thumbnail height for new characters
    #[serde(default = "default_thumbnail_height")]
    pub thumbnail_default_height: u16,

    // Thumbnail visual settings
    /// Enable/disable thumbnail rendering entirely (daemon still runs for hotkeys)
    #[serde(default = "default_thumbnail_enabled")]
    pub thumbnail_enabled: bool,
    pub thumbnail_opacity: u8,
    #[serde(default = "default_border_enabled")]
    pub thumbnail_border: bool,
    pub thumbnail_border_size: u16,
    pub thumbnail_border_color: String,
    pub thumbnail_text_size: u16,
    pub thumbnail_text_x: i16,
    pub thumbnail_text_y: i16,
    #[serde(default = "default_text_font_family")]
    pub thumbnail_text_font: String,
    pub thumbnail_text_color: String,
    
    // Thumbnail behavior settings
    /// Automatically save thumbnail positions when dragged
    /// If disabled, positions can be manually saved via system tray menu
    #[serde(default = "default_auto_save_thumbnail_positions")]
    pub thumbnail_auto_save_position: bool,
    #[serde(default = "default_snap_threshold")]
    pub thumbnail_snap_threshold: u16,
    #[serde(default)]
    pub thumbnail_hide_not_focused: bool,
    /// When a new character logs in without saved coordinates, inherit the previous character's thumbnail position
    /// This keeps thumbnails in place when swapping characters on the same EVE client
    #[serde(default = "default_preserve_thumbnail_position_on_swap")]
    pub thumbnail_preserve_position_on_swap: bool,
    
    // Client behavior settings
    #[serde(default)]
    pub client_minimize_on_switch: bool,
    
    // Hotkey settings (per-profile)
    /// Selected input device for hotkey monitoring (by-id name, None = all devices)
    #[serde(default)]
    pub hotkey_input_device: Option<String>,

    /// Forward cycle hotkey binding (user must configure)
    pub hotkey_cycle_forward: Option<crate::config::HotkeyBinding>,

    /// Backward cycle hotkey binding (user must configure)
    pub hotkey_cycle_backward: Option<crate::config::HotkeyBinding>,

    /// Include logged-out characters in hotkey cycle if they were previously logged in during this session
    #[serde(default)]
    pub hotkey_logged_out_cycle: bool,

    /// Require EVE window focused for hotkeys to work
    #[serde(default)]
    pub hotkey_require_eve_focus: bool,

    /// Character cycle order (list of character names)
    #[serde(default)]
    pub hotkey_cycle_group: Vec<String>,

    /// Per-character hotkey assignments (character_name -> optional binding)
    /// Allows direct switching to specific characters with dedicated hotkeys
    /// Display order follows hotkey_cycle_group
    #[serde(default)]
    pub character_hotkeys: HashMap<String, crate::config::HotkeyBinding>,

    // Per-profile character positions and dimensions
    #[serde(default)]
    pub character_thumbnails: HashMap<String, CharacterSettings>,
}

// Default value functions
fn default_profile_name() -> String {
    crate::constants::defaults::behavior::PROFILE_NAME.to_string()
}

fn default_window_width() -> u16 {
    crate::constants::defaults::manager::WINDOW_WIDTH
}

fn default_window_height() -> u16 {
    crate::constants::defaults::manager::WINDOW_HEIGHT
}

fn default_snap_threshold() -> u16 {
    crate::constants::defaults::behavior::SNAP_THRESHOLD
}

fn default_preserve_thumbnail_position_on_swap() -> bool {
    crate::constants::defaults::behavior::PRESERVE_POSITION_ON_SWAP
}

fn default_thumbnail_width() -> u16 {
    crate::constants::defaults::thumbnail::WIDTH
}

fn default_thumbnail_height() -> u16 {
    crate::constants::defaults::thumbnail::HEIGHT
}

fn default_thumbnail_enabled() -> bool {
    true // Default: thumbnails enabled
}

fn default_border_enabled() -> bool {
    crate::constants::defaults::border::ENABLED
}

fn default_text_font_family() -> String {
    // Try to detect best default TrueType font, but don't fail config creation
    match crate::preview::select_best_default_font() {
        Ok((name, _path)) => {
            tracing::info!(font = %name, "Using detected default font for new config");
            name
        }
        Err(_e) => {
            // Empty string = daemon will use from_system_font() which has X11 fallback
            tracing::warn!("Could not detect TrueType font, config will use X11 fallback");
            String::new()
        }
    }
}

fn default_auto_save_thumbnail_positions() -> bool {
    true
}

fn default_profiles() -> Vec<Profile> {
    vec![Profile {
        profile_name: crate::constants::defaults::behavior::PROFILE_NAME.to_string(),
        profile_description: crate::constants::defaults::behavior::PROFILE_DESCRIPTION.to_string(),
        thumbnail_default_width: default_thumbnail_width(),
        thumbnail_default_height: default_thumbnail_height(),
        thumbnail_enabled: default_thumbnail_enabled(),
        thumbnail_opacity: crate::constants::defaults::thumbnail::OPACITY_PERCENT,
        thumbnail_border: crate::constants::defaults::border::ENABLED,
        thumbnail_border_size: crate::constants::defaults::border::SIZE,
        thumbnail_border_color: crate::constants::defaults::border::COLOR.to_string(),
        thumbnail_text_size: crate::constants::defaults::text::SIZE,
        thumbnail_text_x: crate::constants::defaults::text::OFFSET_X,
        thumbnail_text_y: crate::constants::defaults::text::OFFSET_Y,
        thumbnail_text_font: default_text_font_family(),
        thumbnail_text_color: crate::constants::defaults::text::COLOR.to_string(),
        thumbnail_auto_save_position: default_auto_save_thumbnail_positions(),
        thumbnail_snap_threshold: default_snap_threshold(),
        thumbnail_hide_not_focused: crate::constants::defaults::behavior::HIDE_WHEN_NO_FOCUS,
        thumbnail_preserve_position_on_swap: default_preserve_thumbnail_position_on_swap(),
        client_minimize_on_switch: crate::constants::defaults::behavior::MINIMIZE_CLIENTS_ON_SWITCH,
        hotkey_input_device: None, // Default: no device selected (hotkeys disabled)
        hotkey_cycle_forward: None, // User must configure
        hotkey_cycle_backward: None, // User must configure
        hotkey_logged_out_cycle: false, // Default: off
        hotkey_require_eve_focus: crate::constants::defaults::behavior::HOTKEY_REQUIRE_EVE_FOCUS,
        hotkey_cycle_group: Vec::new(),
        character_hotkeys: HashMap::new(),
        character_thumbnails: HashMap::new(),
    }]
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            selected_profile: default_profile_name(),
            window_width: default_window_width(),
            window_height: default_window_height(),
        }
    }
}

impl Profile {
    /// Create a new profile with default values and the given name
    pub fn default_with_name(name: String, description: String) -> Self {
        let mut profile = default_profiles().into_iter().next().unwrap();
        profile.profile_name = name;
        profile.profile_description = description;
        profile
    }
}

impl Config {
    pub fn path() -> PathBuf {
        #[cfg(not(test))]
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        #[cfg(test)]
        let mut path = std::env::temp_dir().join("eve-preview-manager-test");
        
        path.push(crate::constants::config::APP_DIR);
        path.push(crate::constants::config::FILENAME);
        path
    }
    
    /// Load configuration from JSON file or create default
    pub fn load() -> Result<Self> {
        let config_path = Self::path();
        
        if !config_path.exists() {
            info!("Config file not found, creating default config at {:?}", config_path);
            let config = Config::default();
            config.save()?;
            return Ok(config);
        }
        
        let contents = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config from {:?}", config_path))?;
        
        let config: Config = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse JSON from {:?}", config_path))?;
        
        info!("Loaded config with {} profile(s)", config.profiles.len());
        Ok(config)
    }
    
    /// Save configuration to JSON file using chosen strategy
    pub fn save_with_strategy(&self, strategy: SaveStrategy) -> Result<()> {
        let config_path = Self::path();
        
        // Ensure config directory exists
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory {:?}", parent))?;
        }
        
        let config_to_save = match strategy {
            SaveStrategy::PreserveCharacterPositions => {
                let mut clone = self.clone();
                if config_path.exists()
                    && let Ok(contents) = fs::read_to_string(&config_path)
                        && let Ok(existing_config) = serde_json::from_str::<Config>(&contents) {
                            for profile_to_save in clone.profiles.iter_mut() {
                                if let Some(existing_profile) = existing_config.profiles.iter()
                                    .find(|p| p.profile_name == profile_to_save.profile_name)
                                {
                                    // Profile exists on disk - preserve its character positions
                                    profile_to_save.character_thumbnails = existing_profile.character_thumbnails.clone();
                                }
                                // If profile doesn't exist on disk (new/duplicated profile),
                                // keep the character_thumbnails from the in-memory profile (from clone/duplication)
                            }
                        }
                clone
            }
            SaveStrategy::OverwriteCharacterPositions => self.clone(),
        };
        
        let json_string = serde_json::to_string_pretty(&config_to_save)
            .context("Failed to serialize config to JSON")?;
        
        fs::write(&config_path, json_string)
            .with_context(|| format!("Failed to write config to {:?}", config_path))?;
        
        info!("Saved config to {:?}", config_path);
        Ok(())
    }

    /// Convenience helper: save preserving character positions (GUI default)
    pub fn save(&self) -> Result<()> {
        self.save_with_strategy(SaveStrategy::PreserveCharacterPositions)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            global: GlobalSettings::default(),
            profiles: default_profiles(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_default_with_name() {
        let profile = Profile::default_with_name(
            "Test Profile".to_string(),
            "A test profile".to_string(),
        );
        
        assert_eq!(profile.profile_name, "Test Profile");
        assert_eq!(profile.profile_description, "A test profile");
        assert_eq!(profile.thumbnail_opacity, crate::constants::defaults::thumbnail::OPACITY_PERCENT);
        assert_eq!(profile.thumbnail_border_size, crate::constants::defaults::border::SIZE);
        assert!(profile.character_thumbnails.is_empty());
    }

    #[test]
    fn test_config_default() {
        let config = Config::default();
        
        assert_eq!(config.profiles.len(), 1);
        assert_eq!(config.global.selected_profile, crate::constants::defaults::behavior::PROFILE_NAME);
        assert_eq!(config.global.window_width, crate::constants::defaults::manager::WINDOW_WIDTH);
        assert_eq!(config.global.window_height, crate::constants::defaults::manager::WINDOW_HEIGHT);
    }

    #[test]
    fn test_profile_serialization() {
        let mut profile = Profile::default_with_name("Test".to_string(), String::new());
        profile.character_thumbnails.insert(
            "TestChar".to_string(),
            CharacterSettings::new(100, 200, 480, 270),
        );
        
        let json = serde_json::to_string(&profile).unwrap();
        let deserialized: Profile = serde_json::from_str(&json).unwrap();
        
        assert_eq!(deserialized.profile_name, "Test");
        assert_eq!(deserialized.character_thumbnails.len(), 1);
        assert!(deserialized.character_thumbnails.contains_key("TestChar"));
    }

    #[test]
    fn test_config_serialization_roundtrip() {
        let mut config = Config::default();
        config.profiles[0].character_thumbnails.insert(
            "Character1".to_string(),
            CharacterSettings::new(50, 100, 640, 360),
        );
        
        let json = serde_json::to_string_pretty(&config).unwrap();
        let deserialized: Config = serde_json::from_str(&json).unwrap();
        
        assert_eq!(deserialized.profiles.len(), config.profiles.len());
        assert_eq!(
            deserialized.profiles[0].character_thumbnails.len(),
            config.profiles[0].character_thumbnails.len()
        );
    }

    #[test]
    fn test_global_settings_defaults() {
        let settings = GlobalSettings::default();
        
        assert_eq!(settings.selected_profile, "default");
        assert_eq!(settings.window_width, crate::constants::defaults::manager::WINDOW_WIDTH);
        assert_eq!(settings.window_height, crate::constants::defaults::manager::WINDOW_HEIGHT);
    }

    #[test]
    fn test_profile_behavior_defaults() {
        let profile = Profile::default_with_name("Test".to_string(), String::new());
        
        // Test migrated behavior settings are properly defaulted
        assert_eq!(profile.thumbnail_snap_threshold, crate::constants::defaults::behavior::SNAP_THRESHOLD);
        assert_eq!(
            profile.thumbnail_preserve_position_on_swap,
            crate::constants::defaults::behavior::PRESERVE_POSITION_ON_SWAP
        );
        assert_eq!(profile.thumbnail_default_width, crate::constants::defaults::thumbnail::WIDTH);
        assert_eq!(profile.thumbnail_default_height, crate::constants::defaults::thumbnail::HEIGHT);
        assert_eq!(profile.client_minimize_on_switch, crate::constants::defaults::behavior::MINIMIZE_CLIENTS_ON_SWITCH);
        assert_eq!(profile.thumbnail_hide_not_focused, crate::constants::defaults::behavior::HIDE_WHEN_NO_FOCUS);
    }

    #[test]
    fn test_save_strategy_preserve_character_thumbnails() {
        // This tests the strategy concept - actual file I/O is integration test territory
        let strategy = SaveStrategy::PreserveCharacterPositions;
        assert_eq!(strategy, SaveStrategy::PreserveCharacterPositions);
        assert_ne!(strategy, SaveStrategy::OverwriteCharacterPositions);
    }

    #[test]
    fn test_profile_with_hotkeys() {
        let mut profile = Profile::default_with_name("Hotkey Test".to_string(), String::new());
        profile.hotkey_cycle_forward = Some(crate::config::HotkeyBinding::new(15, false, false, false, false));
        profile.hotkey_cycle_backward = Some(crate::config::HotkeyBinding::new(15, false, true, false, false));
        
        assert!(profile.hotkey_cycle_forward.is_some());
        assert!(profile.hotkey_cycle_backward.is_some());
        
        let json = serde_json::to_string(&profile).unwrap();
        let deserialized: Profile = serde_json::from_str(&json).unwrap();
        
        assert_eq!(deserialized.hotkey_cycle_forward, profile.hotkey_cycle_forward);
        assert_eq!(deserialized.hotkey_cycle_backward, profile.hotkey_cycle_backward);
    }

    #[test]
    fn test_profile_cycle_group() {
        let mut profile = Profile::default_with_name("Cycle Test".to_string(), String::new());
        profile.hotkey_cycle_group = vec![
            "Character1".to_string(),
            "Character2".to_string(),
            "Character3".to_string(),
        ];
        
        assert_eq!(profile.hotkey_cycle_group.len(), 3);
        assert_eq!(profile.hotkey_cycle_group[0], "Character1");
    }
}
