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

/// A named group of characters for cycling
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CycleGroup {
    pub name: String,
    #[serde(default)]
    pub characters: Vec<String>,
    pub hotkey_forward: Option<crate::config::HotkeyBinding>,
    pub hotkey_backward: Option<crate::config::HotkeyBinding>,
}

impl CycleGroup {
    pub fn default_group() -> Self {
        Self {
            name: "Default".to_string(),
            characters: Vec::new(),
            hotkey_forward: None,
            hotkey_backward: None,
        }
    }
}

/// Rule for identifying and naming arbitrary application windows
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomWindowRule {
    /// Pattern to match window title (optional)
    pub title_pattern: Option<String>,
    /// Pattern to match window class/process (optional)
    pub class_pattern: Option<String>,
    /// Display name used as the identifier ("Character Name")
    pub alias: String,
    /// Default width for this source type
    #[serde(default = "default_thumbnail_width")]
    pub default_width: u16,
    /// Default height for this source type
    #[serde(default = "default_thumbnail_height")]
    pub default_height: u16,
    /// If true, only preview the first matching window found
    #[serde(default)]
    pub limit: bool,
}

/// Hotkey backend type selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HotkeyBackendType {
    /// X11 XGrabKey backend (default, secure, no permissions required)
    X11,
    /// evdev raw input backend (optional, requires input group membership)
    Evdev,
}

/// Strategy for saving configuration files
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStrategy {
    /// Load existing config from disk and preserve its character positions/dimensions
    /// Used by GUI when saving general settings to avoid overwriting daemon's position updates
    Preserve,
    /// Overwrite disk config cleanly with current state
    /// Used when we know we have the full authoritative state
    Overwrite,
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
/// Profile - A complete set of visual and behavioral settings
#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub profile_name: String,
    pub profile_description: String,

    // Thumbnail default dimensions
    /// Default thumbnail width for new characters
    pub thumbnail_default_width: u16,
    /// Default thumbnail height for new characters
    pub thumbnail_default_height: u16,

    // Thumbnail visual settings
    /// Enable/disable thumbnail rendering entirely (daemon still runs for hotkeys)
    pub thumbnail_enabled: bool,
    pub thumbnail_opacity: u8,
    pub thumbnail_active_border: bool,
    pub thumbnail_active_border_size: u16,
    pub thumbnail_active_border_color: String,
    pub thumbnail_inactive_border: bool,
    pub thumbnail_inactive_border_size: u16,
    pub thumbnail_inactive_border_color: String,
    pub thumbnail_text_size: u16,
    pub thumbnail_text_x: i16,
    pub thumbnail_text_y: i16,
    pub thumbnail_text_font: String,
    pub thumbnail_text_color: String,

    // Thumbnail behavior settings
    /// Automatically save thumbnail positions when dragged
    /// If disabled, positions can be manually saved via system tray menu
    pub thumbnail_auto_save_position: bool,
    pub thumbnail_snap_threshold: u16,
    pub thumbnail_hide_not_focused: bool,
    /// When a new character logs in without saved coordinates, inherit the previous character's thumbnail position
    /// This keeps thumbnails in place when swapping characters on the same EVE client
    pub thumbnail_preserve_position_on_swap: bool,

    // Client behavior settings
    pub client_minimize_on_switch: bool,
    /// When minimized, show "MINIMIZED" text overlay
    pub client_minimize_show_overlay: bool,

    // Hotkey settings (per-profile)
    /// Hotkey backend selection (X11 or evdev)
    pub hotkey_backend: HotkeyBackendType,

    /// Selected input device for hotkey monitoring (by-id name, None = all devices)
    /// Only used by evdev backend
    pub hotkey_input_device: Option<String>,

    // REMOVED LEGACY FIELDS in favor of cycle_groups
    // hotkey_cycle_forward, hotkey_cycle_backward, hotkey_cycle_group are now inside CycleGroup
    /// Multiple cycle groups, each with its own character list and hotkeys
    /// Multiple cycle groups, each with its own character list and hotkeys
    pub cycle_groups: Vec<CycleGroup>,

    /// Include logged-out characters in hotkey cycle if they were previously logged in during this session
    pub hotkey_logged_out_cycle: bool,

    /// Require EVE window focused for hotkeys to work
    pub hotkey_require_eve_focus: bool,

    /// Hotkey to switch to this profile (global)
    pub hotkey_profile_switch: Option<crate::config::HotkeyBinding>,

    /// Hotkey to temporarily skip the current character in the cycle
    pub hotkey_toggle_skip: Option<crate::config::HotkeyBinding>,

    /// Hotkey to toggle visibility of all thumbnails (ephemeral)
    pub hotkey_toggle_previews: Option<crate::config::HotkeyBinding>,

    /// Per-character hotkey assignments (character_name -> optional binding)
    /// Allows direct switching to specific characters with dedicated hotkeys
    /// Display order follows hotkey_cycle_group
    pub character_hotkeys: HashMap<String, crate::config::HotkeyBinding>,

    // Per-profile character positions and dimensions
    pub character_thumbnails: HashMap<String, CharacterSettings>,

    /// Per-profile custom source positions and dimensions (separate from characters)
    pub custom_source_thumbnails: HashMap<String, CharacterSettings>,

    /// Custom window matching rules for external applications
    pub custom_windows: Vec<CustomWindowRule>,
}

// Default value functions
// Default value functions
pub(crate) fn default_border_size() -> u16 {
    crate::constants::defaults::border::SIZE
}

pub(crate) fn default_profile_name() -> String {
    crate::constants::defaults::behavior::PROFILE_NAME.to_string()
}

pub(crate) fn default_hotkey_backend() -> HotkeyBackendType {
    HotkeyBackendType::X11
}

pub(crate) fn default_window_width() -> u16 {
    crate::constants::defaults::manager::WINDOW_WIDTH
}

pub(crate) fn default_window_height() -> u16 {
    crate::constants::defaults::manager::WINDOW_HEIGHT
}

pub(crate) fn default_snap_threshold() -> u16 {
    crate::constants::defaults::behavior::SNAP_THRESHOLD
}

pub(crate) fn default_preserve_thumbnail_position_on_swap() -> bool {
    crate::constants::defaults::behavior::PRESERVE_POSITION_ON_SWAP
}

pub(crate) fn default_thumbnail_width() -> u16 {
    crate::constants::defaults::thumbnail::WIDTH
}

pub(crate) fn default_thumbnail_height() -> u16 {
    crate::constants::defaults::thumbnail::HEIGHT
}

pub(crate) fn default_thumbnail_enabled() -> bool {
    true // Default: thumbnails enabled
}

pub(crate) fn default_border_enabled() -> bool {
    crate::constants::defaults::border::ENABLED
}

pub(crate) fn default_inactive_border_enabled() -> bool {
    false // Default: inactive borders disabled
}

pub(crate) fn default_inactive_border_color() -> String {
    crate::constants::defaults::border::INACTIVE_COLOR.to_string()
}

pub(crate) fn default_text_font_family() -> String {
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

pub(crate) fn default_auto_save_thumbnail_positions() -> bool {
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
        thumbnail_active_border: crate::constants::defaults::border::ENABLED,
        thumbnail_active_border_size: crate::constants::defaults::border::SIZE,
        thumbnail_active_border_color: crate::constants::defaults::border::ACTIVE_COLOR.to_string(),
        thumbnail_inactive_border: default_inactive_border_enabled(),
        thumbnail_inactive_border_size: crate::constants::defaults::border::SIZE,
        thumbnail_inactive_border_color: default_inactive_border_color(),
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
        client_minimize_show_overlay: false, // Default: off (clean minimized look)
        hotkey_backend: default_hotkey_backend(), // Default: X11 (secure, no permissions)
        hotkey_input_device: None, // Default: no device selected (only used by evdev backend)
        hotkey_logged_out_cycle: false, // Default: off
        hotkey_require_eve_focus: crate::constants::defaults::behavior::HOTKEY_REQUIRE_EVE_FOCUS,
        hotkey_profile_switch: None,
        hotkey_toggle_skip: None,     // User must configure
        hotkey_toggle_previews: None, // User must configure
        cycle_groups: vec![CycleGroup::default_group()],
        character_hotkeys: HashMap::new(),
        character_thumbnails: HashMap::new(),
        custom_source_thumbnails: HashMap::new(),
        custom_windows: Vec::new(),
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

impl Default for Profile {
    fn default() -> Self {
        default_profiles().into_iter().next().unwrap()
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
            info!(
                "Config file not found, creating default config at {:?}",
                config_path
            );
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

    pub fn get_active_profile(&self) -> Option<&Profile> {
        self.profiles
            .iter()
            .find(|p| p.profile_name == self.global.selected_profile)
    }

    pub fn get_active_profile_mut(&mut self) -> Option<&mut Profile> {
        self.profiles
            .iter_mut()
            .find(|p| p.profile_name == self.global.selected_profile)
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
            SaveStrategy::Preserve => {
                let mut clone = self.clone();
                if config_path.exists()
                    && let Ok(contents) = fs::read_to_string(&config_path)
                    && let Ok(existing_config) = serde_json::from_str::<Config>(&contents)
                {
                    for profile_to_save in clone.profiles.iter_mut() {
                        if let Some(existing_profile) = existing_config
                            .profiles
                            .iter()
                            .find(|p| p.profile_name == profile_to_save.profile_name)
                        {
                            // Profile exists on disk
                            // Merge strategy:
                            // 1. For characters in BOTH: keep GUI settings (overrides), update positions from Disk
                            // 2. For characters ONLY in Disk: add to GUI (newly discovered by daemon)

                            for (char_name, disk_settings) in &existing_profile.character_thumbnails
                            {
                                if let Some(gui_settings) =
                                    profile_to_save.character_thumbnails.get_mut(char_name)
                                {
                                    // Found in both: update position/dim from disk, keep GUI overrides
                                    gui_settings.x = disk_settings.x;
                                    gui_settings.y = disk_settings.y;
                                    // Don't overwrite dimensions - GUI state is authoritative (it updates from disk via polling, but allows user overrides)
                                    // gui_settings.dimensions = disk_settings.dimensions;
                                }
                                // If they are valid new windows found by the daemon, the daemon will re-add them
                                // to the config on its next pass/save cycle. This fixes the "zombie character" bug.
                            }

                            // Same merge logic for custom sources
                            for (source_name, disk_settings) in
                                &existing_profile.custom_source_thumbnails
                            {
                                if let Some(gui_settings) = profile_to_save
                                    .custom_source_thumbnails
                                    .get_mut(source_name)
                                {
                                    gui_settings.x = disk_settings.x;
                                    gui_settings.y = disk_settings.y;
                                    // Dimension updates logic mirrors characters
                                }
                            }
                        }
                    }
                }
                clone
            }
            SaveStrategy::Overwrite => self.clone(),
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
        self.save_with_strategy(SaveStrategy::Preserve)
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
        let profile =
            Profile::default_with_name("Test Profile".to_string(), "A test profile".to_string());

        assert_eq!(profile.profile_name, "Test Profile");
        assert_eq!(profile.profile_description, "A test profile");
        assert_eq!(
            profile.thumbnail_opacity,
            crate::constants::defaults::thumbnail::OPACITY_PERCENT
        );
        assert_eq!(
            profile.thumbnail_active_border_size,
            crate::constants::defaults::border::SIZE
        );
        assert!(profile.character_thumbnails.is_empty());
        assert!(profile.custom_source_thumbnails.is_empty());
    }

    #[test]
    fn test_config_default() {
        let config = Config::default();

        assert_eq!(config.profiles.len(), 1);
        assert_eq!(
            config.global.selected_profile,
            crate::constants::defaults::behavior::PROFILE_NAME
        );
        assert_eq!(
            config.global.window_width,
            crate::constants::defaults::manager::WINDOW_WIDTH
        );
        assert_eq!(
            config.global.window_height,
            crate::constants::defaults::manager::WINDOW_HEIGHT
        );
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
        assert_eq!(
            settings.window_width,
            crate::constants::defaults::manager::WINDOW_WIDTH
        );
        assert_eq!(
            settings.window_height,
            crate::constants::defaults::manager::WINDOW_HEIGHT
        );
    }

    #[test]
    fn test_profile_behavior_defaults() {
        let profile = Profile::default_with_name("Test".to_string(), String::new());

        // Test migrated behavior settings are properly defaulted
        assert_eq!(
            profile.thumbnail_snap_threshold,
            crate::constants::defaults::behavior::SNAP_THRESHOLD
        );
        assert_eq!(
            profile.thumbnail_preserve_position_on_swap,
            crate::constants::defaults::behavior::PRESERVE_POSITION_ON_SWAP
        );
        assert_eq!(
            profile.thumbnail_default_width,
            crate::constants::defaults::thumbnail::WIDTH
        );
        assert_eq!(
            profile.thumbnail_default_height,
            crate::constants::defaults::thumbnail::HEIGHT
        );
        assert_eq!(
            profile.client_minimize_on_switch,
            crate::constants::defaults::behavior::MINIMIZE_CLIENTS_ON_SWITCH
        );
        assert!(!profile.client_minimize_show_overlay);
        assert_eq!(
            profile.thumbnail_hide_not_focused,
            crate::constants::defaults::behavior::HIDE_WHEN_NO_FOCUS
        );
    }

    #[test]
    fn test_save_strategy_variants() {
        let strategy = SaveStrategy::Preserve;
        assert_eq!(strategy, SaveStrategy::Preserve);
        assert_ne!(strategy, SaveStrategy::Overwrite);
    }

    #[test]
    fn test_profile_with_hotkeys() {
        let mut profile = Profile::default_with_name("Hotkey Test".to_string(), String::new());
        profile.cycle_groups[0].hotkey_forward = Some(crate::config::HotkeyBinding::new(
            15, false, false, false, false,
        ));
        profile.cycle_groups[0].hotkey_backward = Some(crate::config::HotkeyBinding::new(
            15, false, true, false, false,
        ));

        assert!(profile.cycle_groups[0].hotkey_forward.is_some());
        assert!(profile.cycle_groups[0].hotkey_backward.is_some());

        let json = serde_json::to_string(&profile).unwrap();
        let deserialized: Profile = serde_json::from_str(&json).unwrap();

        assert_eq!(
            deserialized.cycle_groups[0].hotkey_forward,
            profile.cycle_groups[0].hotkey_forward
        );
        assert_eq!(
            deserialized.cycle_groups[0].hotkey_backward,
            profile.cycle_groups[0].hotkey_backward
        );
    }

    #[test]
    fn test_profile_cycle_group() {
        let mut profile = Profile::default_with_name("Cycle Test".to_string(), String::new());
        // Populate the default group
        profile.cycle_groups[0].characters = vec![
            "Character1".to_string(),
            "Character2".to_string(),
            "Character3".to_string(),
        ];

        assert_eq!(profile.cycle_groups[0].characters.len(), 3);
        assert_eq!(profile.cycle_groups[0].characters[0], "Character1");
    }

    #[test]
    fn test_migration_legacy_hotkeys() {
        // Start with a valid default profile to ensure all required fields are present
        let default_profile = Profile::default_with_name("Legacy Test".to_string(), String::new());
        let mut json_value = serde_json::to_value(&default_profile).unwrap();

        // 1. Remove the new `cycle_groups` field to simulate an old config
        if let Some(obj) = json_value.as_object_mut() {
            obj.remove("cycle_groups");

            // 2. Inject legacy fields
            obj.insert(
                "hotkey_cycle_group".to_string(),
                serde_json::json!(["A", "B"]),
            );
            // We need to match the actual serialization format of HotkeyBinding, or mostly likely just "keys" if that's how it's defined
            // Based on HotkeyBinding usage elsewhere, it likely serializes to a struct.
            // Let's create a binding object.
            // Assuming HotkeyBinding deserialization is robust or standard.
            // If HotkeyBinding is complex, we can use serde_json::to_value on a real binding.
            let dummy_binding = crate::config::HotkeyBinding::new(15, false, false, false, false); // Tab key?

            obj.insert(
                "hotkey_cycle_forward".to_string(),
                serde_json::to_value(&dummy_binding).unwrap(),
            );
            obj.insert(
                "hotkey_cycle_backward".to_string(),
                serde_json::to_value(&dummy_binding).unwrap(),
            );
        }

        let legacy_json = serde_json::to_string(&json_value).unwrap();

        let profile: Profile =
            serde_json::from_str(&legacy_json).expect("Failed to deserialize legacy profile");

        // Verify migration
        assert_eq!(profile.cycle_groups.len(), 1);
        let group = &profile.cycle_groups[0];
        assert_eq!(group.name, "Default");
        assert_eq!(group.characters, vec!["A", "B"]);
        assert!(group.hotkey_forward.is_some());
        assert!(group.hotkey_backward.is_some());
    }
}
