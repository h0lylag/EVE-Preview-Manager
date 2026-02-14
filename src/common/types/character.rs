//! Character-specific types and settings for EVE Online windows

use super::geometry::{Dimensions, Position};
use serde::{Deserialize, Serialize};

/// EVE Online window type classification
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EveWindowType {
    /// Logged-in EVE client with character name
    LoggedIn(String),
    /// Logged-out EVE client (character select screen)
    LoggedOut,
}

impl EveWindowType {
    /// Get the character name, or empty string if logged out
    pub fn character_name(&self) -> &str {
        match self {
            EveWindowType::LoggedIn(name) => name,
            EveWindowType::LoggedOut => "",
        }
    }
}

/// Thumbnail lifecycle state
/// Using an enum makes invalid states (e.g., focused + minimized) impossible to represent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailState {
    /// Visible and available for interaction (may or may not have focus)
    Normal { focused: bool },
    /// Window is minimized by the window manager
    Minimized,
}

impl ThumbnailState {
    /// Check if the thumbnail currently has input focus
    pub fn is_focused(&self) -> bool {
        matches!(self, Self::Normal { focused: true })
    }

    /// Check if the thumbnail is minimized by the window manager
    #[allow(dead_code)]
    pub fn is_minimized(&self) -> bool {
        matches!(self, Self::Minimized)
    }
}

impl Default for ThumbnailState {
    fn default() -> Self {
        // New thumbnails start in unfocused normal state
        Self::Normal { focused: false }
    }
}

/// Preview rendering mode for the thumbnail
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMode {
    /// Live preview from the source window (default)
    #[default]
    Live,
    /// Static solid color fill
    Static { color: String },
}

/// Per-character settings: position and thumbnail dimensions
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "CharacterSettingsProxy", into = "CharacterSettingsProxy")]
pub struct CharacterSettings {
    pub x: i16,
    pub y: i16,
    /// Thumbnail dimensions (0 = use auto-detect)
    pub dimensions: Dimensions,

    // -- Advanced Character Settings --
    pub alias: Option<String>,
    pub notes: Option<String>,
    pub override_active_border_color: Option<String>,
    pub override_inactive_border_color: Option<String>,
    pub override_active_border_size: Option<u16>,
    pub override_inactive_border_size: Option<u16>,
    pub override_text_color: Option<String>,
    pub preview_mode: PreviewMode,
    /// If true, this character is exempt from minimize-on-switch behavior
    pub exempt_from_minimize: bool,
}

#[derive(Serialize, Deserialize)]
struct CharacterSettingsProxy {
    x: i16,
    y: i16,
    #[serde(default)]
    width: u16,
    #[serde(default)]
    height: u16,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    override_active_border_color: Option<String>,
    #[serde(default)]
    override_inactive_border_color: Option<String>,
    #[serde(default)]
    override_active_border_size: Option<u16>,
    #[serde(default)]
    override_inactive_border_size: Option<u16>,
    #[serde(default)]
    override_text_color: Option<String>,
    #[serde(default)]
    preview_mode: PreviewMode,
    #[serde(default)]
    exempt_from_minimize: bool,
}

impl From<CharacterSettings> for CharacterSettingsProxy {
    fn from(settings: CharacterSettings) -> Self {
        Self {
            x: settings.x,
            y: settings.y,
            width: settings.dimensions.width,
            height: settings.dimensions.height,
            alias: settings.alias,
            notes: settings.notes,
            override_active_border_color: settings.override_active_border_color,
            override_inactive_border_color: settings.override_inactive_border_color,
            override_active_border_size: settings.override_active_border_size,
            override_inactive_border_size: settings.override_inactive_border_size,
            override_text_color: settings.override_text_color,
            preview_mode: settings.preview_mode,
            exempt_from_minimize: settings.exempt_from_minimize,
        }
    }
}

impl From<CharacterSettingsProxy> for CharacterSettings {
    fn from(proxy: CharacterSettingsProxy) -> Self {
        Self {
            x: proxy.x,
            y: proxy.y,
            dimensions: Dimensions {
                width: proxy.width,
                height: proxy.height,
            },
            alias: proxy.alias,
            notes: proxy.notes,
            override_active_border_color: proxy.override_active_border_color,
            override_inactive_border_color: proxy.override_inactive_border_color,
            override_active_border_size: proxy.override_active_border_size,
            override_inactive_border_size: proxy.override_inactive_border_size,
            override_text_color: proxy.override_text_color,
            preview_mode: proxy.preview_mode,
            exempt_from_minimize: proxy.exempt_from_minimize,
        }
    }
}

impl CharacterSettings {
    pub fn new(x: i16, y: i16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            dimensions: Dimensions::new(width, height),
            alias: None,
            notes: None,
            override_active_border_color: None,
            override_inactive_border_color: None,
            override_active_border_size: None,
            override_inactive_border_size: None,
            override_text_color: None,
            preview_mode: PreviewMode::default(),
            exempt_from_minimize: false,
        }
    }

    pub fn position(&self) -> Position {
        Position::new(self.x, self.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thumbnail_state_normal_unfocused() {
        let state = ThumbnailState::Normal { focused: false };

        assert!(!state.is_focused());
        assert!(!state.is_minimized());
    }

    #[test]
    fn test_thumbnail_state_normal_focused() {
        let state = ThumbnailState::Normal { focused: true };

        assert!(state.is_focused());
        assert!(!state.is_minimized());
    }

    #[test]
    fn test_thumbnail_state_minimized() {
        let state = ThumbnailState::Minimized;

        assert!(!state.is_focused());
        assert!(state.is_minimized());
    }

    #[test]
    fn test_thumbnail_state_default() {
        let state = ThumbnailState::default();
        assert_eq!(state, ThumbnailState::Normal { focused: false });
        assert!(!state.is_focused());
    }

    #[test]
    fn test_eve_window_type_logged_in() {
        let window = EveWindowType::LoggedIn("TestCharacter".to_string());
        assert_eq!(window.character_name(), "TestCharacter");

        let window2 = EveWindowType::LoggedIn("AnotherChar".to_string());
        assert_ne!(window, window2);
    }

    #[test]
    fn test_eve_window_type_logged_out() {
        let window = EveWindowType::LoggedOut;
        assert_eq!(window.character_name(), "");

        let window2 = EveWindowType::LoggedOut;
        assert_eq!(window, window2);
    }

    #[test]
    fn test_eve_window_type_equality() {
        let logged_in1 = EveWindowType::LoggedIn("Char1".to_string());
        let logged_in2 = EveWindowType::LoggedIn("Char1".to_string());
        let logged_in3 = EveWindowType::LoggedIn("Char2".to_string());
        let logged_out = EveWindowType::LoggedOut;

        assert_eq!(logged_in1, logged_in2);
        assert_ne!(logged_in1, logged_in3);
        assert_ne!(logged_in1, logged_out);
    }

    #[test]
    fn test_character_settings_new() {
        let settings = CharacterSettings::new(100, 200, 640, 480);
        assert_eq!(settings.x, 100);
        assert_eq!(settings.y, 200);
        assert_eq!(settings.dimensions.width, 640);
        assert_eq!(settings.dimensions.height, 480);
    }

    #[test]
    fn test_character_settings_position() {
        let settings = CharacterSettings::new(150, 250, 800, 600);
        let pos = settings.position();
        assert_eq!(pos.x, 150);
        assert_eq!(pos.y, 250);
    }

    #[test]
    fn test_character_settings_serialization() {
        let settings = CharacterSettings::new(50, 75, 1920, 1080);
        let json = serde_json::to_string(&settings).unwrap();
        let deserialized: CharacterSettings = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.x, settings.x);
        assert_eq!(deserialized.y, settings.y);
        assert_eq!(deserialized.dimensions.width, settings.dimensions.width);
        assert_eq!(deserialized.dimensions.height, settings.dimensions.height);
    }

    #[test]
    fn test_character_settings_zero_dimensions() {
        // Zero dimensions mean "use auto-detect"
        let settings = CharacterSettings::new(100, 100, 0, 0);
        assert_eq!(settings.dimensions.width, 0);
        assert_eq!(settings.dimensions.height, 0);
    }

    #[test]
    fn test_preview_mode_serialization() {
        let mode = PreviewMode::Static {
            color: "#FF0000".to_string(),
        };
        let json = serde_json::to_string(&mode).unwrap();
        // Check for correct format: {"static":{"color":"#FF0000"}}
        let expected = "{\"static\":{\"color\":\"#FF0000\"}}";
        assert_eq!(json, expected);

        let deserialized: PreviewMode = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, mode);

        let live = PreviewMode::Live;
        let json_live = serde_json::to_string(&live).unwrap();
        assert_eq!(json_live, "\"live\"");
    }
}
