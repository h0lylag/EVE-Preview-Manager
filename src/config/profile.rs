//! Profile-based configuration for the Manager
//!
//! Supports multiple profiles, each containing visual settings (opacity, border, text),
//! hotkey bindings, and per-source thumbnail positions.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::info;

use crate::common::types::{CharacterSettings, Dimensions, Position, SourceIdentity};

/// A named group of typed sources for cycling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CycleGroup {
    pub name: String,
    // Rename to "cycle_list" for JSON, but accept "characters" (legacy) and "slots" (intermediate) for compat
    #[serde(
        default,
        rename = "cycle_list",
        alias = "characters",
        alias = "slots",
        deserialize_with = "deserialize_slots"
    )]
    pub cycle_list: Vec<CycleSlot>,
    pub hotkey_forward: Option<crate::config::HotkeyBinding>,
    pub hotkey_backward: Option<crate::config::HotkeyBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum CycleSlot {
    #[serde(rename = "eve")]
    Eve(String),
    #[serde(rename = "source")]
    Source(String),
}

impl CycleGroup {
    pub fn default_group() -> Self {
        Self {
            name: "Default".to_string(),
            cycle_list: Vec::new(),
            hotkey_forward: None,
            hotkey_backward: None,
        }
    }
}

// Helper for migrating legacy string list to CycleSlot::Eve
fn deserialize_slots<'de, D>(deserializer: D) -> Result<Vec<CycleSlot>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // For binary formats (IPC), strict typing is enforced and we don't need migration logic.
    // Migration is only relevant for JSON config files.
    if !deserializer.is_human_readable() {
        return Vec::<CycleSlot>::deserialize(deserializer);
    }

    use serde::de::{self, Visitor};
    use std::fmt;

    struct SlotsVisitor;

    impl<'de> Visitor<'de> for SlotsVisitor {
        type Value = Vec<CycleSlot>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a list of strings or CycleSlot objects")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut slots = Vec::new();

            #[derive(Deserialize)]
            #[serde(untagged)]
            enum Helper {
                Legacy(String),
                Modern(CycleSlot),
            }

            while let Some(elem) = seq.next_element::<Helper>()? {
                match elem {
                    Helper::Legacy(s) => slots.push(CycleSlot::Eve(s)),
                    Helper::Modern(slot) => slots.push(slot),
                }
            }

            Ok(slots)
        }
    }

    deserializer.deserialize_seq(SlotsVisitor)
}

/// Rule for identifying and naming arbitrary application windows
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomWindowRule {
    /// Pattern to match window title (optional)
    pub title_pattern: Option<String>,
    /// Pattern to match window class/process (optional)
    pub class_pattern: Option<String>,
    /// Display name used as the custom-source identity key.
    pub alias: String,

    // --- Layout Overrides ---
    /// Default width for this source type
    #[serde(default = "default_thumbnail_width")]
    pub default_width: u16,
    /// Default height for this source type
    #[serde(default = "default_thumbnail_height")]
    pub default_height: u16,
    /// If true, only preview the first matching window found
    #[serde(default)]
    pub limit: bool,

    // --- Visual Overrides (Optional) ---
    // Border Overrides
    pub active_border_color: Option<String>,
    pub inactive_border_color: Option<String>,
    pub active_border_size: Option<u16>,
    pub inactive_border_size: Option<u16>,

    // Text Overrides
    pub text_color: Option<String>,
    pub text_size: Option<u16>,
    pub text_x: Option<i16>,
    pub text_y: Option<i16>,

    // Behavior Overrides
    #[serde(default)]
    pub preview_mode: Option<crate::common::types::PreviewMode>,
    /// If true, this source is exempt from minimize-on-switch behavior
    #[serde(default)]
    pub exempt_from_minimize: bool,
    /// Per-source override for preview rendering.
    /// None = use global setting, Some(true) = always show, Some(false) = always hide
    #[serde(default)]
    pub override_render_preview: Option<bool>,
    /// Specific hotkey to activate this source directly
    pub hotkey: Option<crate::config::HotkeyBinding>,
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

/// How unidentified logged-out clients participate in hotkey cycling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoggedOutUnidentifiedCycleMode {
    /// Cycle unidentified logged-out clients with their own dedicated hotkeys.
    SeparateHotkeys,
    /// Append unidentified logged-out clients after configured cycle group entries.
    AppendToGroups,
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
    #[serde(default = "default_backup_enabled")]
    pub backup_enabled: bool,
    #[serde(default = "default_backup_interval_days")]
    pub backup_interval_days: u32,
    #[serde(default = "default_backup_retention_count")]
    pub backup_retention_count: u32,
    #[serde(default)]
    pub minimize_to_tray: bool,
    #[serde(default)]
    pub start_minimized_to_tray: bool,
}

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
    /// Whether new previews with no saved coordinates should use a fixed top-left screen position
    pub thumbnail_default_position_enabled: bool,
    /// Stored fixed top-left screen position for new previews with no saved coordinates
    pub thumbnail_default_position: Position,

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
    /// When an EVE client logs out, keep showing the last known character name in its thumbnail label
    pub thumbnail_show_logged_out_character_name: bool,

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
    /// Multiple cycle groups, each with its own source list and hotkeys.
    pub cycle_groups: Vec<CycleGroup>,

    /// Include logged-out characters in hotkey cycle if they were previously logged in during this session
    pub hotkey_logged_out_cycle: bool,
    /// Include logged-out clients that have not been associated with a character yet
    pub hotkey_logged_out_unidentified_cycle: bool,
    /// How unidentified logged-out clients participate in cycle hotkeys
    pub hotkey_logged_out_unidentified_cycle_mode: LoggedOutUnidentifiedCycleMode,
    /// Dedicated forward hotkey for unidentified logged-out clients
    pub hotkey_logged_out_unidentified_cycle_forward: Option<crate::config::HotkeyBinding>,
    /// Dedicated backward hotkey for unidentified logged-out clients
    pub hotkey_logged_out_unidentified_cycle_backward: Option<crate::config::HotkeyBinding>,

    /// Require a tracked source window to be focused for hotkeys to work.
    pub hotkey_require_eve_focus: bool,

    /// Reset cycle index to the beginning when switching between cycle groups
    pub hotkey_cycle_reset_index: bool,

    /// Hotkey to switch to this profile (global)
    pub hotkey_profile_switch: Option<crate::config::HotkeyBinding>,

    /// Hotkey to temporarily skip the current source in the cycle.
    pub hotkey_toggle_skip: Option<crate::config::HotkeyBinding>,

    /// Hotkey to toggle visibility of all thumbnails (ephemeral)
    pub hotkey_toggle_previews: Option<crate::config::HotkeyBinding>,

    /// EVE character hotkey assignments (character_name -> binding).
    /// Custom source hotkeys live on their CustomWindowRule entries.
    pub character_hotkeys: HashMap<String, crate::config::HotkeyBinding>,

    // Per-profile character positions and dimensions
    pub character_thumbnails: HashMap<String, CharacterSettings>,

    /// Per-profile custom source positions and dimensions (separate from characters)
    pub custom_source_thumbnails: HashMap<String, CharacterSettings>,

    /// Custom window matching rules for external applications
    pub custom_windows: Vec<CustomWindowRule>,
}

// Default value functions
pub(crate) fn default_border_size() -> u16 {
    crate::common::constants::defaults::border::SIZE
}

pub(crate) fn default_profile_name() -> String {
    crate::common::constants::defaults::behavior::PROFILE_NAME.to_string()
}

pub(crate) fn default_hotkey_backend() -> HotkeyBackendType {
    HotkeyBackendType::X11
}

pub(crate) fn default_logged_out_unidentified_cycle_mode() -> LoggedOutUnidentifiedCycleMode {
    LoggedOutUnidentifiedCycleMode::SeparateHotkeys
}

pub(crate) fn default_backup_enabled() -> bool {
    crate::common::constants::config::backup::ENABLED
}

pub(crate) fn default_backup_interval_days() -> u32 {
    crate::common::constants::config::backup::INTERVAL_DAYS
}

pub(crate) fn default_backup_retention_count() -> u32 {
    crate::common::constants::config::backup::RETENTION_COUNT
}

pub(crate) fn default_window_width() -> u16 {
    crate::common::constants::defaults::manager::WINDOW_WIDTH
}

pub(crate) fn default_window_height() -> u16 {
    crate::common::constants::defaults::manager::WINDOW_HEIGHT
}

pub(crate) fn default_snap_threshold() -> u16 {
    crate::common::constants::defaults::behavior::SNAP_THRESHOLD
}

pub(crate) fn default_preserve_thumbnail_position_on_swap() -> bool {
    crate::common::constants::defaults::behavior::PRESERVE_POSITION_ON_SWAP
}

pub(crate) fn default_show_logged_out_character_name() -> bool {
    false
}

pub(crate) fn default_thumbnail_width() -> u16 {
    crate::common::constants::defaults::thumbnail::WIDTH
}

pub(crate) fn default_thumbnail_height() -> u16 {
    crate::common::constants::defaults::thumbnail::HEIGHT
}

pub(crate) fn default_thumbnail_enabled() -> bool {
    true // Default: thumbnails enabled
}

pub(crate) fn default_border_enabled() -> bool {
    crate::common::constants::defaults::border::ENABLED
}

pub(crate) fn default_inactive_border_enabled() -> bool {
    false // Default: inactive borders disabled
}

pub(crate) fn default_inactive_border_color() -> String {
    crate::common::constants::defaults::border::INACTIVE_COLOR.to_string()
}

pub(crate) fn default_text_font_family() -> String {
    // Try to detect best default TrueType font, but don't fail config creation
    match crate::daemon::select_best_default_font() {
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
        profile_name: crate::common::constants::defaults::behavior::PROFILE_NAME.to_string(),
        profile_description: crate::common::constants::defaults::behavior::PROFILE_DESCRIPTION
            .to_string(),
        thumbnail_default_width: default_thumbnail_width(),
        thumbnail_default_height: default_thumbnail_height(),
        thumbnail_default_position_enabled: false,
        thumbnail_default_position: Position::default(),
        thumbnail_enabled: default_thumbnail_enabled(),
        thumbnail_opacity: crate::common::constants::defaults::thumbnail::OPACITY_PERCENT,
        thumbnail_active_border: crate::common::constants::defaults::border::ENABLED,
        thumbnail_active_border_size: crate::common::constants::defaults::border::SIZE,
        thumbnail_active_border_color: crate::common::constants::defaults::border::ACTIVE_COLOR
            .to_string(),
        thumbnail_inactive_border: default_inactive_border_enabled(),
        thumbnail_inactive_border_size: crate::common::constants::defaults::border::SIZE,
        thumbnail_inactive_border_color: default_inactive_border_color(),
        thumbnail_text_size: crate::common::constants::defaults::text::SIZE,
        thumbnail_text_x: crate::common::constants::defaults::text::OFFSET_X,
        thumbnail_text_y: crate::common::constants::defaults::text::OFFSET_Y,
        thumbnail_text_font: default_text_font_family(),
        thumbnail_text_color: crate::common::constants::defaults::text::COLOR.to_string(),
        thumbnail_auto_save_position: default_auto_save_thumbnail_positions(),
        thumbnail_snap_threshold: default_snap_threshold(),
        thumbnail_hide_not_focused:
            crate::common::constants::defaults::behavior::HIDE_WHEN_NO_FOCUS,
        thumbnail_preserve_position_on_swap: default_preserve_thumbnail_position_on_swap(),
        thumbnail_show_logged_out_character_name: default_show_logged_out_character_name(),
        client_minimize_on_switch:
            crate::common::constants::defaults::behavior::MINIMIZE_CLIENTS_ON_SWITCH,
        client_minimize_show_overlay: false, // Default: off (clean minimized look)
        hotkey_backend: default_hotkey_backend(), // Default: X11 (secure, no permissions)
        hotkey_input_device: None, // Default: no device selected (only used by evdev backend)
        hotkey_logged_out_cycle: false, // Default: off
        hotkey_logged_out_unidentified_cycle: false,
        hotkey_logged_out_unidentified_cycle_mode: default_logged_out_unidentified_cycle_mode(),
        hotkey_logged_out_unidentified_cycle_forward: None,
        hotkey_logged_out_unidentified_cycle_backward: None,
        hotkey_require_eve_focus:
            crate::common::constants::defaults::behavior::HOTKEY_REQUIRE_EVE_FOCUS,
        hotkey_cycle_reset_index: false,
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
            backup_enabled: default_backup_enabled(),
            backup_interval_days: default_backup_interval_days(),
            backup_retention_count: default_backup_retention_count(),
            minimize_to_tray: false,
            start_minimized_to_tray: false,
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

    /// Update thumbnail position/dimensions if changed.
    /// Returns true if the configuration was modified, false otherwise.
    pub fn update_thumbnail_spatial(
        &mut self,
        source: &SourceIdentity,
        position: Position,
        dimensions: Dimensions,
    ) -> bool {
        let map = if source.kind.is_custom() {
            &mut self.custom_source_thumbnails
        } else {
            &mut self.character_thumbnails
        };

        if let Some(existing) = map.get_mut(&source.name) {
            // Check if anything actually changed
            if existing.x == position.x
                && existing.y == position.y
                && existing.dimensions == dimensions
            {
                // No change
                return false;
            }

            // Update existing entry
            existing.x = position.x;
            existing.y = position.y;
            existing.dimensions = dimensions;
            true
        } else {
            // New entry - always a change
            map.insert(
                source.name.clone(),
                CharacterSettings::new(position.x, position.y, dimensions.width, dimensions.height),
            );
            true
        }
    }

    pub fn validate_custom_source_aliases(&self) -> std::result::Result<(), String> {
        let mut seen: HashMap<String, String> = HashMap::new();

        for rule in &self.custom_windows {
            let trimmed = rule.alias.trim();
            if trimmed.is_empty() {
                return Err("Custom source display names cannot be empty".to_string());
            }

            let normalized = trimmed.to_lowercase();
            if let Some(existing) = seen.get(&normalized) {
                return Err(format!(
                    "Duplicate custom source display name '{}'",
                    existing
                ));
            }
            seen.insert(normalized, trimmed.to_string());
        }

        Ok(())
    }

    pub fn validate_custom_source_alias_for_rule(
        &self,
        rule_idx: usize,
        alias: &str,
    ) -> std::result::Result<String, String> {
        let trimmed = alias.trim();
        if trimmed.is_empty() {
            return Err("Custom source display name cannot be empty".to_string());
        }

        let normalized = trimmed.to_lowercase();
        if self
            .custom_windows
            .iter()
            .enumerate()
            .any(|(idx, rule)| idx != rule_idx && rule.alias.trim().to_lowercase() == normalized)
        {
            return Err(format!("Another custom source already uses '{}'", trimmed));
        }

        Ok(trimmed.to_string())
    }

    pub fn rename_custom_source_alias(
        &mut self,
        rule_idx: usize,
        new_alias: &str,
    ) -> std::result::Result<bool, String> {
        let new_alias = self.validate_custom_source_alias_for_rule(rule_idx, new_alias)?;
        let Some(rule) = self.custom_windows.get(rule_idx) else {
            return Err("Custom source rule no longer exists".to_string());
        };

        let old_alias = rule.alias.clone();
        if old_alias == new_alias {
            return Ok(false);
        }

        let old_alias_normalized = old_alias.trim().to_lowercase();
        let old_alias_count = self
            .custom_windows
            .iter()
            .filter(|rule| rule.alias.trim().to_lowercase() == old_alias_normalized)
            .count();

        self.custom_windows[rule_idx].alias = new_alias.clone();

        let old_settings_key = self
            .custom_source_thumbnails
            .keys()
            .find(|key| key.trim().to_lowercase() == old_alias_normalized)
            .cloned();

        if let Some(settings_key) = old_settings_key
            && let Some(settings) = self.custom_source_thumbnails.get(&settings_key).cloned()
        {
            if old_alias_count > 1 {
                self.custom_source_thumbnails
                    .entry(new_alias.clone())
                    .or_insert(settings);
            } else if let Some(settings) = self.custom_source_thumbnails.remove(&settings_key) {
                self.custom_source_thumbnails
                    .insert(new_alias.clone(), settings);
            }
        }

        if old_alias_count == 1 {
            for group in &mut self.cycle_groups {
                for slot in &mut group.cycle_list {
                    if let CycleSlot::Source(name) = slot
                        && name.trim().to_lowercase() == old_alias_normalized
                    {
                        *name = new_alias.clone();
                    }
                }
            }
        }

        Ok(true)
    }

    pub fn remove_custom_source_rule(&mut self, rule_idx: usize) -> bool {
        if rule_idx >= self.custom_windows.len() {
            return false;
        }

        let alias = self.custom_windows.remove(rule_idx).alias;
        let normalized = alias.trim().to_lowercase();
        if !self
            .custom_windows
            .iter()
            .any(|rule| rule.alias.trim().to_lowercase() == normalized)
        {
            if let Some(settings_key) = self
                .custom_source_thumbnails
                .keys()
                .find(|key| key.trim().to_lowercase() == normalized)
                .cloned()
            {
                self.custom_source_thumbnails.remove(&settings_key);
            }
            for group in &mut self.cycle_groups {
                group.cycle_list.retain(|slot| match slot {
                    CycleSlot::Eve(_) => true,
                    CycleSlot::Source(name) => name.trim().to_lowercase() != normalized,
                });
            }
        }

        true
    }
}

impl Default for Profile {
    fn default() -> Self {
        default_profiles().into_iter().next().unwrap()
    }
}

impl Config {
    pub fn validate_profile_name(
        &self,
        excluded_idx: Option<usize>,
        candidate: &str,
    ) -> std::result::Result<String, String> {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            return Err("Profile name cannot be empty".to_string());
        }

        let normalized = trimmed.to_lowercase();
        if self.profiles.iter().enumerate().any(|(idx, profile)| {
            Some(idx) != excluded_idx && profile.profile_name.trim().to_lowercase() == normalized
        }) {
            return Err(format!("Another profile already uses '{}'", trimmed));
        }

        Ok(trimmed.to_string())
    }

    pub fn validate_profile_names(&self) -> std::result::Result<(), String> {
        let mut seen: HashMap<String, String> = HashMap::new();

        for profile in &self.profiles {
            let trimmed = profile.profile_name.trim();
            if trimmed.is_empty() {
                return Err("Profile name cannot be empty".to_string());
            }
            if profile.profile_name != trimmed {
                return Err(format!(
                    "Profile name '{}' has leading or trailing whitespace",
                    trimmed
                ));
            }

            let normalized = trimmed.to_lowercase();
            if let Some(existing) = seen.get(&normalized) {
                return Err(format!("Duplicate profile name '{}'", existing));
            }
            seen.insert(normalized, trimmed.to_string());
        }

        Ok(())
    }

    pub fn path() -> PathBuf {
        // Allow overriding config directory via env var (for testing isolation)
        if let Ok(dir) = std::env::var("EVE_PREVIEW_MANAGER_CONFIG_DIR") {
            let mut path = PathBuf::from(dir);
            path.push(crate::common::constants::config::FILENAME);
            return path;
        }

        #[cfg(not(test))]
        let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        #[cfg(test)]
        let mut path = std::env::temp_dir().join("eve-preview-manager-test");

        path.push(crate::common::constants::config::APP_DIR);
        path.push(crate::common::constants::config::FILENAME);
        path
    }

    /// Load configuration from JSON file or create default
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path())
    }

    /// Load configuration from a specific path
    pub fn load_from(config_path: &std::path::Path) -> Result<Self> {
        if !config_path.exists() {
            info!(
                "Config file not found, creating default config at {:?}",
                config_path
            );
            let config = Config::default();
            config.save_to(config_path)?;
            return Ok(config);
        }

        let contents = fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read config from {:?}", config_path))?;

        let config: Config = serde_json::from_str(&contents)
            .with_context(|| format!("Failed to parse JSON from {:?}", config_path))?;

        info!(path = ?config_path, profile_count = config.profiles.len(), "Loaded config");
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

    /// Save configuration to JSON file.
    ///
    /// Atomically replaces config.json with the current in-memory state.
    /// The Manager maintains authoritative state via IPC synchronization.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    /// Save configuration to a specific path
    pub fn save_to(&self, config_path: &std::path::Path) -> Result<()> {
        self.validate_profile_names()
            .map_err(|err| anyhow::anyhow!(err))
            .context("Configuration has invalid profile names")?;

        let json = serde_json::to_vec_pretty(self).context("Failed to serialize config to JSON")?;

        crate::config::write_atomically(config_path, &json)
            .with_context(|| format!("Failed to write config to {:?}", config_path))?;

        info!(path = ?config_path, "Saved config");
        Ok(())
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
    use crate::common::types::CharacterSettings;

    fn test_custom_rule(alias: &str) -> CustomWindowRule {
        CustomWindowRule {
            title_pattern: Some(alias.to_string()),
            class_pattern: None,
            alias: alias.to_string(),
            default_width: default_thumbnail_width(),
            default_height: default_thumbnail_height(),
            limit: false,
            active_border_color: None,
            inactive_border_color: None,
            active_border_size: None,
            inactive_border_size: None,
            text_color: None,
            text_size: None,
            text_x: None,
            text_y: None,
            preview_mode: None,
            exempt_from_minimize: false,
            override_render_preview: None,
            hotkey: None,
        }
    }

    fn config_with_profile_names(names: &[&str]) -> Config {
        let mut config = Config::default();
        let template = config.profiles[0].clone();
        config.profiles = names
            .iter()
            .map(|name| {
                let mut profile = template.clone();
                profile.profile_name = (*name).to_string();
                profile
            })
            .collect();
        if let Some(name) = names.first() {
            config.global.selected_profile = (*name).to_string();
        }
        config
    }

    #[test]
    fn test_profile_default_with_name() {
        let profile =
            Profile::default_with_name("Test Profile".to_string(), "A test profile".to_string());

        assert_eq!(profile.profile_name, "Test Profile");
        assert_eq!(profile.profile_description, "A test profile");
        assert_eq!(
            profile.thumbnail_opacity,
            crate::common::constants::defaults::thumbnail::OPACITY_PERCENT
        );
        assert_eq!(
            profile.thumbnail_active_border_size,
            crate::common::constants::defaults::border::SIZE
        );
        assert!(profile.character_thumbnails.is_empty());
        assert!(profile.custom_source_thumbnails.is_empty());
    }

    #[test]
    fn profile_name_validation_returns_trimmed_name() {
        let config = config_with_profile_names(&["Mining"]);

        assert_eq!(
            config.validate_profile_name(None, "  PvP  "),
            Ok("PvP".to_string())
        );
    }

    #[test]
    fn profile_name_validation_rejects_empty_names() {
        let config = Config::default();

        for candidate in ["", " \t\n"] {
            assert_eq!(
                config.validate_profile_name(None, candidate),
                Err("Profile name cannot be empty".to_string())
            );
        }
    }

    #[test]
    fn profile_name_validation_rejects_case_insensitive_trimmed_duplicates() {
        let config = config_with_profile_names(&["Mining"]);

        for candidate in [" mining ", "MINING"] {
            assert_eq!(
                config.validate_profile_name(None, candidate),
                Err(format!(
                    "Another profile already uses '{}'",
                    candidate.trim()
                ))
            );
        }
    }

    #[test]
    fn profile_name_validation_excludes_profile_being_edited() {
        let config = config_with_profile_names(&["Mining", "PvP"]);

        assert_eq!(
            config.validate_profile_name(Some(0), " MINING "),
            Ok("MINING".to_string())
        );
        assert_eq!(
            config.validate_profile_name(Some(0), "pvp"),
            Err("Another profile already uses 'pvp'".to_string())
        );
    }

    #[test]
    fn profile_names_validation_rejects_noncanonical_names() {
        let blank = config_with_profile_names(&[" \t"]);
        assert_eq!(
            blank.validate_profile_names(),
            Err("Profile name cannot be empty".to_string())
        );

        let padded = config_with_profile_names(&[" Mining "]);
        assert_eq!(
            padded.validate_profile_names(),
            Err("Profile name 'Mining' has leading or trailing whitespace".to_string())
        );
    }

    #[test]
    fn profile_names_validation_rejects_case_insensitive_duplicates() {
        let config = config_with_profile_names(&["Mining", "MINING"]);

        assert_eq!(
            config.validate_profile_names(),
            Err("Duplicate profile name 'Mining'".to_string())
        );
    }

    #[test]
    fn invalid_profile_names_do_not_replace_saved_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.json");
        let valid = config_with_profile_names(&["Mining"]);
        valid.save_to(&config_path).unwrap();
        let saved = fs::read(&config_path).unwrap();

        let invalid = config_with_profile_names(&["Mining", "MINING"]);
        assert!(invalid.save_to(&config_path).is_err());
        assert_eq!(fs::read(&config_path).unwrap(), saved);
    }

    #[test]
    fn invalid_profile_names_load_for_repair() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.json");
        let invalid = config_with_profile_names(&["Mining", "MINING"]);
        fs::write(&config_path, serde_json::to_vec_pretty(&invalid).unwrap()).unwrap();

        let loaded = Config::load_from(&config_path).unwrap();
        assert_eq!(loaded.profiles.len(), 2);
        assert_eq!(
            loaded.validate_profile_names(),
            Err("Duplicate profile name 'Mining'".to_string())
        );
    }

    #[test]
    fn custom_source_alias_validation_allows_eve_name_collision() {
        let mut profile = Profile::default();
        profile.character_thumbnails.insert(
            "h0ly lag".to_string(),
            CharacterSettings::new(10, 20, 300, 200),
        );
        profile.custom_windows.push(test_custom_rule("h0ly lag"));

        assert!(profile.validate_custom_source_aliases().is_ok());
    }

    #[test]
    fn custom_source_alias_validation_rejects_duplicate_sources_case_insensitively() {
        let mut profile = Profile::default();
        profile.custom_windows.push(test_custom_rule("Browser"));
        profile.custom_windows.push(test_custom_rule(" browser "));

        assert!(profile.validate_custom_source_aliases().is_err());
    }

    #[test]
    fn rename_custom_source_alias_migrates_source_state_only() {
        let mut profile = Profile::default();
        profile.custom_windows.push(test_custom_rule("Old"));
        profile
            .custom_source_thumbnails
            .insert("Old".to_string(), CharacterSettings::new(10, 20, 300, 200));
        profile.cycle_groups[0].cycle_list = vec![
            CycleSlot::Source("Old".to_string()),
            CycleSlot::Eve("Old".to_string()),
        ];

        assert_eq!(profile.rename_custom_source_alias(0, "New"), Ok(true));

        assert!(!profile.custom_source_thumbnails.contains_key("Old"));
        assert!(profile.custom_source_thumbnails.contains_key("New"));
        assert_eq!(
            profile.cycle_groups[0].cycle_list,
            vec![
                CycleSlot::Source("New".to_string()),
                CycleSlot::Eve("Old".to_string())
            ]
        );
    }

    #[test]
    fn rename_custom_source_alias_migrates_normalized_source_slots() {
        let mut profile = Profile::default();
        profile.custom_windows.push(test_custom_rule(" Old "));
        profile.cycle_groups[0].cycle_list = vec![
            CycleSlot::Source("old".to_string()),
            CycleSlot::Eve("old".to_string()),
        ];

        assert_eq!(profile.rename_custom_source_alias(0, "New"), Ok(true));

        assert_eq!(
            profile.cycle_groups[0].cycle_list,
            vec![
                CycleSlot::Source("New".to_string()),
                CycleSlot::Eve("old".to_string())
            ]
        );
    }

    #[test]
    fn rename_legacy_duplicate_custom_source_copies_shared_settings() {
        let mut profile = Profile::default();
        profile.custom_windows.push(test_custom_rule("Old"));
        profile.custom_windows.push(test_custom_rule("Old"));
        profile
            .custom_source_thumbnails
            .insert("Old".to_string(), CharacterSettings::new(10, 20, 300, 200));
        profile.cycle_groups[0]
            .cycle_list
            .push(CycleSlot::Source("Old".to_string()));

        assert_eq!(profile.rename_custom_source_alias(1, "New"), Ok(true));

        assert!(profile.custom_source_thumbnails.contains_key("Old"));
        assert_eq!(
            profile.custom_source_thumbnails.get("New"),
            profile.custom_source_thumbnails.get("Old")
        );
        assert_eq!(
            profile.cycle_groups[0].cycle_list,
            vec![CycleSlot::Source("Old".to_string())]
        );
    }

    #[test]
    fn rename_legacy_duplicate_custom_source_finds_shared_settings_case_insensitively() {
        let mut profile = Profile::default();
        profile.custom_windows.push(test_custom_rule("Old"));
        profile.custom_windows.push(test_custom_rule(" old "));
        profile
            .custom_source_thumbnails
            .insert("Old".to_string(), CharacterSettings::new(10, 20, 300, 200));

        assert_eq!(profile.rename_custom_source_alias(1, "New"), Ok(true));

        assert!(profile.custom_source_thumbnails.contains_key("Old"));
        assert_eq!(
            profile.custom_source_thumbnails.get("New"),
            profile.custom_source_thumbnails.get("Old")
        );
    }

    #[test]
    fn remove_custom_source_rule_cleans_source_state_only_when_alias_is_unused() {
        let mut profile = Profile::default();
        profile.custom_windows.push(test_custom_rule("Shared"));
        profile.custom_source_thumbnails.insert(
            "Shared".to_string(),
            CharacterSettings::new(10, 20, 300, 200),
        );
        profile.cycle_groups[0].cycle_list = vec![
            CycleSlot::Source("Shared".to_string()),
            CycleSlot::Eve("Shared".to_string()),
        ];

        assert!(profile.remove_custom_source_rule(0));

        assert!(!profile.custom_source_thumbnails.contains_key("Shared"));
        assert_eq!(
            profile.cycle_groups[0].cycle_list,
            vec![CycleSlot::Eve("Shared".to_string())]
        );
    }

    #[test]
    fn remove_custom_source_rule_preserves_state_for_remaining_normalized_duplicate() {
        let mut profile = Profile::default();
        profile.custom_windows.push(test_custom_rule("Shared"));
        profile.custom_windows.push(test_custom_rule(" shared "));
        profile.custom_source_thumbnails.insert(
            "Shared".to_string(),
            CharacterSettings::new(10, 20, 300, 200),
        );
        profile.cycle_groups[0]
            .cycle_list
            .push(CycleSlot::Source("Shared".to_string()));

        assert!(profile.remove_custom_source_rule(0));

        assert!(profile.custom_source_thumbnails.contains_key("Shared"));
        assert_eq!(
            profile.cycle_groups[0].cycle_list,
            vec![CycleSlot::Source("Shared".to_string())]
        );
    }

    #[test]
    fn remove_custom_source_rule_cleans_normalized_settings_when_alias_is_unused() {
        let mut profile = Profile::default();
        profile.custom_windows.push(test_custom_rule(" Shared "));
        profile.custom_source_thumbnails.insert(
            "shared".to_string(),
            CharacterSettings::new(10, 20, 300, 200),
        );
        profile.cycle_groups[0].cycle_list = vec![
            CycleSlot::Source("shared".to_string()),
            CycleSlot::Eve("shared".to_string()),
        ];

        assert!(profile.remove_custom_source_rule(0));

        assert!(profile.custom_source_thumbnails.is_empty());
        assert_eq!(
            profile.cycle_groups[0].cycle_list,
            vec![CycleSlot::Eve("shared".to_string())]
        );
    }

    #[test]
    fn json_deserialization_preserves_same_name_eve_entries() {
        let mut config = Config::default();
        let profile = &mut config.profiles[0];
        profile.character_thumbnails.insert(
            "h0ly lag".to_string(),
            CharacterSettings::new(10, 20, 300, 200),
        );
        profile.custom_windows.push(test_custom_rule("h0ly lag"));
        profile.cycle_groups[0]
            .cycle_list
            .push(CycleSlot::Eve("h0ly lag".to_string()));

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: Config = serde_json::from_str(&json).unwrap();
        let profile = &deserialized.profiles[0];

        assert!(profile.character_thumbnails.contains_key("h0ly lag"));
        assert!(profile.custom_source_thumbnails.contains_key("h0ly lag"));
        assert_eq!(
            profile.cycle_groups[0].cycle_list,
            vec![CycleSlot::Eve("h0ly lag".to_string())]
        );
    }

    #[test]
    fn test_config_default() {
        let config = Config::default();

        assert_eq!(config.profiles.len(), 1);
        assert_eq!(
            config.global.selected_profile,
            crate::common::constants::defaults::behavior::PROFILE_NAME
        );
        assert_eq!(
            config.global.window_width,
            crate::common::constants::defaults::manager::WINDOW_WIDTH
        );
        assert_eq!(
            config.global.window_height,
            crate::common::constants::defaults::manager::WINDOW_HEIGHT
        );
    }

    #[test]
    fn test_profile_serialization() {
        let mut profile = Profile::default_with_name("Test".to_string(), String::new());
        profile.thumbnail_default_position_enabled = true;
        profile.thumbnail_default_position = Position::new(321, 654);
        profile.character_thumbnails.insert(
            "TestChar".to_string(),
            CharacterSettings::new(100, 200, 480, 270),
        );

        let json = serde_json::to_string(&profile).unwrap();
        let deserialized: Profile = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.profile_name, "Test");
        assert!(deserialized.thumbnail_default_position_enabled);
        assert_eq!(
            deserialized.thumbnail_default_position,
            Position::new(321, 654)
        );
        assert_eq!(deserialized.character_thumbnails.len(), 1);
        assert!(deserialized.character_thumbnails.contains_key("TestChar"));
    }

    #[test]
    fn test_disabled_default_position_retains_coordinates() {
        let mut profile = Profile::default_with_name("Test".to_string(), String::new());
        profile.thumbnail_default_position_enabled = false;
        profile.thumbnail_default_position = Position::new(321, 654);

        let json = serde_json::to_string(&profile).unwrap();
        let deserialized: Profile = serde_json::from_str(&json).unwrap();

        assert!(!deserialized.thumbnail_default_position_enabled);
        assert_eq!(
            deserialized.thumbnail_default_position,
            Position::new(321, 654)
        );
    }

    #[test]
    fn test_legacy_default_position_option_enables_setting() {
        let profile = Profile::default_with_name("Legacy Position".to_string(), String::new());
        let mut json_value = serde_json::to_value(&profile).unwrap();

        if let Some(obj) = json_value.as_object_mut() {
            obj.remove("thumbnail_default_position_enabled");
            obj.insert(
                "thumbnail_default_position".to_string(),
                serde_json::json!({ "x": 321, "y": 654 }),
            );
        }

        let deserialized: Profile = serde_json::from_value(json_value).unwrap();
        assert!(deserialized.thumbnail_default_position_enabled);
        assert_eq!(
            deserialized.thumbnail_default_position,
            Position::new(321, 654)
        );
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
            crate::common::constants::defaults::manager::WINDOW_WIDTH
        );
        assert_eq!(
            settings.window_height,
            crate::common::constants::defaults::manager::WINDOW_HEIGHT
        );
        assert_eq!(
            settings.backup_enabled,
            crate::common::constants::config::backup::ENABLED
        );
        assert_eq!(
            settings.backup_interval_days,
            crate::common::constants::config::backup::INTERVAL_DAYS
        );
        assert_eq!(
            settings.backup_retention_count,
            crate::common::constants::config::backup::RETENTION_COUNT
        );
        assert!(!settings.minimize_to_tray);
        assert!(!settings.start_minimized_to_tray);
    }

    #[test]
    fn test_profile_behavior_defaults() {
        let profile = Profile::default_with_name("Test".to_string(), String::new());

        // Test migrated behavior settings are properly defaulted
        assert_eq!(
            profile.thumbnail_snap_threshold,
            crate::common::constants::defaults::behavior::SNAP_THRESHOLD
        );
        assert_eq!(
            profile.thumbnail_preserve_position_on_swap,
            crate::common::constants::defaults::behavior::PRESERVE_POSITION_ON_SWAP
        );
        assert!(!profile.thumbnail_show_logged_out_character_name);
        assert!(!profile.hotkey_logged_out_unidentified_cycle);
        assert_eq!(
            profile.hotkey_logged_out_unidentified_cycle_mode,
            LoggedOutUnidentifiedCycleMode::SeparateHotkeys
        );
        assert!(
            profile
                .hotkey_logged_out_unidentified_cycle_forward
                .is_none()
        );
        assert!(
            profile
                .hotkey_logged_out_unidentified_cycle_backward
                .is_none()
        );
        assert_eq!(
            profile.thumbnail_default_width,
            crate::common::constants::defaults::thumbnail::WIDTH
        );
        assert_eq!(
            profile.thumbnail_default_height,
            crate::common::constants::defaults::thumbnail::HEIGHT
        );
        assert!(!profile.thumbnail_default_position_enabled);
        assert_eq!(profile.thumbnail_default_position, Position::default());
        assert_eq!(
            profile.client_minimize_on_switch,
            crate::common::constants::defaults::behavior::MINIMIZE_CLIENTS_ON_SWITCH
        );
        assert_eq!(
            profile.thumbnail_hide_not_focused,
            crate::common::constants::defaults::behavior::HIDE_WHEN_NO_FOCUS
        );
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
    fn test_logged_out_unidentified_cycle_mode_serialization() {
        let mut profile =
            Profile::default_with_name("Unidentified Test".to_string(), String::new());
        profile.hotkey_logged_out_unidentified_cycle = true;
        profile.hotkey_logged_out_unidentified_cycle_mode =
            LoggedOutUnidentifiedCycleMode::AppendToGroups;
        profile.hotkey_logged_out_unidentified_cycle_forward = Some(
            crate::config::HotkeyBinding::new(16, false, false, false, false),
        );
        profile.hotkey_logged_out_unidentified_cycle_backward = Some(
            crate::config::HotkeyBinding::new(17, false, false, false, false),
        );

        let json = serde_json::to_string(&profile).unwrap();
        assert!(json.contains("append_to_groups"));

        let deserialized: Profile = serde_json::from_str(&json).unwrap();
        assert!(deserialized.hotkey_logged_out_unidentified_cycle);
        assert_eq!(
            deserialized.hotkey_logged_out_unidentified_cycle_mode,
            LoggedOutUnidentifiedCycleMode::AppendToGroups
        );
        assert_eq!(
            deserialized.hotkey_logged_out_unidentified_cycle_forward,
            profile.hotkey_logged_out_unidentified_cycle_forward
        );
        assert_eq!(
            deserialized.hotkey_logged_out_unidentified_cycle_backward,
            profile.hotkey_logged_out_unidentified_cycle_backward
        );
    }

    #[test]
    fn test_logged_out_unidentified_cycle_missing_fields_default() {
        let profile = Profile::default_with_name("Missing Fields".to_string(), String::new());
        let mut json_value = serde_json::to_value(&profile).unwrap();

        if let Some(obj) = json_value.as_object_mut() {
            obj.remove("thumbnail_default_position_enabled");
            obj.remove("thumbnail_default_position");
            obj.remove("hotkey_logged_out_unidentified_cycle");
            obj.remove("hotkey_logged_out_unidentified_cycle_mode");
            obj.remove("hotkey_logged_out_unidentified_cycle_forward");
            obj.remove("hotkey_logged_out_unidentified_cycle_backward");
        }

        let deserialized: Profile = serde_json::from_value(json_value).unwrap();
        assert!(!deserialized.thumbnail_default_position_enabled);
        assert_eq!(deserialized.thumbnail_default_position, Position::default());
        assert!(!deserialized.hotkey_logged_out_unidentified_cycle);
        assert_eq!(
            deserialized.hotkey_logged_out_unidentified_cycle_mode,
            LoggedOutUnidentifiedCycleMode::SeparateHotkeys
        );
        assert!(
            deserialized
                .hotkey_logged_out_unidentified_cycle_forward
                .is_none()
        );
        assert!(
            deserialized
                .hotkey_logged_out_unidentified_cycle_backward
                .is_none()
        );
    }

    #[test]
    fn test_profile_cycle_group() {
        let mut profile = Profile::default_with_name("Cycle Test".to_string(), String::new());
        // Populate the default group
        profile.cycle_groups[0].cycle_list = vec![
            CycleSlot::Eve("Character1".to_string()),
            CycleSlot::Eve("Character2".to_string()),
            CycleSlot::Eve("Character3".to_string()),
        ];

        assert_eq!(profile.cycle_groups[0].cycle_list.len(), 3);
        assert_eq!(
            profile.cycle_groups[0].cycle_list[0],
            CycleSlot::Eve("Character1".to_string())
        );
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
        assert_eq!(group.cycle_list.len(), 2);
        assert_eq!(group.cycle_list[0], CycleSlot::Eve("A".to_string()));
        assert_eq!(group.cycle_list[1], CycleSlot::Eve("B".to_string()));
        assert!(group.hotkey_forward.is_some());
        assert!(group.hotkey_backward.is_some());
    }

    #[test]
    fn test_filesystem_roundtrip() {
        let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
        let config_path = temp_dir.path().join("config.json");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::write(&config_path, b"{}").expect("Failed to create existing config");
            fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640))
                .expect("Failed to set config permissions");
        }

        let mut config = Config::default();
        config.global.selected_profile = "filesystem_test".to_string();

        // Save to isolated path
        config
            .save_to(&config_path)
            .expect("Failed to save config to temp path");
        assert!(config_path.exists());

        let saved = fs::read(&config_path).expect("Failed to read saved config");
        let saved_config: Config =
            serde_json::from_slice(&saved).expect("Saved config was not complete JSON");
        assert_eq!(saved_config.global.selected_profile, "filesystem_test");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }

        assert_eq!(fs::read_dir(temp_dir.path()).unwrap().count(), 1);

        // Load from isolated path
        let loaded = Config::load_from(&config_path).expect("Failed to load config from temp path");
        assert_eq!(loaded.global.selected_profile, "filesystem_test");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn save_atomically_replaces_existing_file() {
        use std::io::{Read, Seek};

        let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
        let config_path = temp_dir.path().join("config.json");

        let mut old_config = Config::default();
        old_config.global.selected_profile = "old".to_string();
        old_config.save_to(&config_path).unwrap();
        let old_contents = fs::read(&config_path).unwrap();
        let mut old_handle = fs::File::open(&config_path).unwrap();

        let mut new_config = old_config;
        new_config.global.selected_profile = "new".to_string();
        new_config.save_to(&config_path).unwrap();

        old_handle.rewind().unwrap();
        let mut contents_from_old_handle = Vec::new();
        old_handle
            .read_to_end(&mut contents_from_old_handle)
            .unwrap();
        assert_eq!(contents_from_old_handle, old_contents);

        let loaded = Config::load_from(&config_path).unwrap();
        assert_eq!(loaded.global.selected_profile, "new");
        assert_eq!(fs::read_dir(temp_dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn save_cleans_temporary_file_after_failed_replacement() {
        let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
        let config_path = temp_dir.path().join("config.json");
        fs::create_dir(&config_path).unwrap();
        fs::write(config_path.join("marker"), b"unchanged").unwrap();

        Config::default()
            .save_to(&config_path)
            .expect_err("Replacing a non-empty directory must fail");

        assert_eq!(fs::read(config_path.join("marker")).unwrap(), b"unchanged");
        assert_eq!(fs::read_dir(temp_dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn test_default_config_creation() {
        let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
        let config_path = temp_dir
            .path()
            .join("nested")
            .join("non_existent_config.json");

        assert!(!config_path.exists());

        // Should create default file
        let loaded = Config::load_from(&config_path).expect("Failed to load/create default config");

        assert!(config_path.exists());
        assert_eq!(
            loaded.global.selected_profile,
            crate::common::constants::defaults::behavior::PROFILE_NAME
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
