//! Profile-based configuration for GUI manager
//!
//! New config architecture with support for multiple profiles,
//! each containing a complete set of visual and behavioral settings.

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
    /// Preserve character_positions entries already on disk (GUI edits)
    PreserveCharacterPositions,
    /// Overwrite character_positions with in-memory data (daemon updates)
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

/// Global daemon behavior (applies to all profiles)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalSettings {
    #[serde(default = "default_profile_name")]
    pub selected_profile: String,
    #[serde(default = "default_window_width")]
    pub window_width: u16,
    #[serde(default = "default_window_height")]
    pub window_height: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_x: Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_y: Option<i16>,
    #[serde(default)]
    pub minimize_clients_on_switch: bool,
    #[serde(default)]
    pub hotkey_require_eve_focus: bool,
    #[serde(default)]
    pub hide_when_no_focus: bool,
    #[serde(default = "default_snap_threshold")]
    pub snap_threshold: u16,
    /// When a new character logs in without saved coordinates, inherit the previous character's thumbnail position
    /// This keeps thumbnails in place when swapping characters on the same EVE client
    #[serde(default = "default_preserve_thumbnail_position_on_swap")]
    pub preserve_thumbnail_position_on_swap: bool,
    /// Default thumbnail width for new characters
    #[serde(default = "default_thumbnail_width")]
    pub default_thumbnail_width: u16,
    /// Default thumbnail height for new characters
    #[serde(default = "default_thumbnail_height")]
    pub default_thumbnail_height: u16,
}

/// Profile - A complete set of visual and behavioral settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    
    // Visual settings
    #[serde(rename = "opacity_percent")]
    pub opacity_percent: u8,
    #[serde(default = "default_border_enabled")]
    pub border_enabled: bool,
    pub border_size: u16,
    #[serde(rename = "border_color")]
    pub border_color: String,
    pub text_size: u16,
    pub text_x: i16,
    pub text_y: i16,
    #[serde(rename = "text_color")]
    pub text_color: String,
    #[serde(default = "default_text_font_family")]
    pub text_font_family: String,
    
    // Hotkey settings (per-profile)
    /// Forward cycle hotkey binding (user must configure)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle_forward_keys: Option<crate::config::HotkeyBinding>,

    /// Backward cycle hotkey binding (user must configure)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle_backward_keys: Option<crate::config::HotkeyBinding>,

    /// Selected input device for hotkey monitoring (by-id name, None = all devices)
    #[serde(default)]
    pub selected_hotkey_device: Option<String>,

    /// Character cycle order (list of character names)
    #[serde(default)]
    pub cycle_group: Vec<String>,

    /// Include logged-out characters in Tab/Shift+Tab cycle if they were previously logged in during this session
    #[serde(default)]
    pub include_logged_out_in_cycle: bool,

    /// Automatically save thumbnail positions when dragged
    /// If disabled, positions can be manually saved via system tray menu
    #[serde(default = "default_auto_save_thumbnail_positions")]
    pub auto_save_thumbnail_positions: bool,

    // Per-profile character positions and dimensions
    #[serde(rename = "characters", default)]
    pub character_positions: HashMap<String, CharacterSettings>,
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
        name: crate::constants::defaults::behavior::PROFILE_NAME.to_string(),
        description: crate::constants::defaults::behavior::PROFILE_DESCRIPTION.to_string(),
        opacity_percent: crate::constants::defaults::thumbnail::OPACITY_PERCENT,
        border_enabled: crate::constants::defaults::border::ENABLED,
        border_size: crate::constants::defaults::border::SIZE,
        border_color: crate::constants::defaults::border::COLOR.to_string(),
        text_size: crate::constants::defaults::text::SIZE,
        text_x: crate::constants::defaults::text::OFFSET_X,
        text_y: crate::constants::defaults::text::OFFSET_Y,
        text_color: crate::constants::defaults::text::COLOR.to_string(),
        text_font_family: default_text_font_family(),
        cycle_forward_keys: None, // User must configure
        cycle_backward_keys: None, // User must configure
        selected_hotkey_device: None, // Default: monitor all devices
        cycle_group: Vec::new(),
        include_logged_out_in_cycle: false, // Default: off
        auto_save_thumbnail_positions: default_auto_save_thumbnail_positions(),
        character_positions: HashMap::new(),
    }]
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            selected_profile: default_profile_name(),
            window_width: default_window_width(),
            window_height: default_window_height(),
            window_x: None,
            window_y: None,
            minimize_clients_on_switch: crate::constants::defaults::behavior::MINIMIZE_CLIENTS_ON_SWITCH,
            hotkey_require_eve_focus: crate::constants::defaults::behavior::HOTKEY_REQUIRE_EVE_FOCUS,
            hide_when_no_focus: crate::constants::defaults::behavior::HIDE_WHEN_NO_FOCUS,
            snap_threshold: default_snap_threshold(),
            preserve_thumbnail_position_on_swap: default_preserve_thumbnail_position_on_swap(),
            default_thumbnail_width: default_thumbnail_width(),
            default_thumbnail_height: default_thumbnail_height(),
        }
    }
}

impl Profile {
    /// Create a new profile with default values and the given name
    pub fn default_with_name(name: String, description: String) -> Self {
        let mut profile = default_profiles().into_iter().next().unwrap();
        profile.name = name;
        profile.description = description;
        profile
    }
}

impl Config {
    pub fn path() -> PathBuf {
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
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
                                    .find(|p| p.name == profile_to_save.name)
                                {
                                    // Profile exists on disk - preserve its character positions
                                    profile_to_save.character_positions = existing_profile.character_positions.clone();
                                }
                                // If profile doesn't exist on disk (new/duplicated profile),
                                // keep the character_positions from the in-memory profile (from clone/duplication)
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
