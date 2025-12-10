//! Hotkey binding configuration and key code mapping

use evdev::KeyCode;
use serde::de;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::str::FromStr;

/// A keyboard hotkey binding with modifiers
/// Serializes to/from object format: {"keys": [...], "source_devices": [...]}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HotkeyBinding {
    /// evdev key code (e.g., KEY_TAB = 15, KEY_F1 = 59)
    pub key_code: u16,

    /// Control key pressed
    pub ctrl: bool,

    /// Shift key pressed
    pub shift: bool,

    /// Alt key pressed
    pub alt: bool,

    /// Super/Windows key pressed
    pub super_key: bool,

    /// Input devices that contributed to this binding (e.g., keyboard, mouse)
    /// Used for auto-detection of which devices to listen to at runtime
    pub source_devices: Vec<String>,
}

impl HotkeyBinding {
    /// Create a new hotkey binding
    pub fn new(key_code: u16, ctrl: bool, shift: bool, alt: bool, super_key: bool) -> Self {
        Self {
            key_code,
            ctrl,
            shift,
            alt,
            super_key,
            source_devices: Vec::new(),
        }
    }

    /// Create a new hotkey binding with source devices
    pub fn with_devices(
        key_code: u16,
        ctrl: bool,
        shift: bool,
        alt: bool,
        super_key: bool,
        source_devices: Vec<String>,
    ) -> Self {
        Self {
            key_code,
            ctrl,
            shift,
            alt,
            super_key,
            source_devices,
        }
    }

    /// Get human-readable display name for this binding (for UI)
    pub fn display_name(&self) -> String {
        let mut parts = Vec::new();

        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.super_key {
            parts.push("Super".to_string());
        }

        parts.push(key_code_to_name(self.key_code));

        parts.join("+")
    }

    /// Check if this binding matches a key press with current modifier state
    pub fn matches(
        &self,
        key_code: u16,
        ctrl: bool,
        shift: bool,
        alt: bool,
        super_key: bool,
    ) -> bool {
        self.key_code == key_code
            && self.ctrl == ctrl
            && self.shift == shift
            && self.alt == alt
            && self.super_key == super_key
    }

    /// Convert to array format for JSON serialization
    /// Format: [modifier_keys..., main_key]
    /// Example: ["KEY_LEFTSHIFT", "KEY_TAB"]
    fn to_key_array(&self) -> Vec<String> {
        let mut keys = Vec::new();

        // Add modifiers in consistent order
        if self.ctrl {
            keys.push("KEY_LEFTCTRL".to_string());
        }
        if self.shift {
            keys.push("KEY_LEFTSHIFT".to_string());
        }
        if self.alt {
            keys.push("KEY_LEFTALT".to_string());
        }
        if self.super_key {
            keys.push("KEY_LEFTMETA".to_string());
        }

        // Add main key using evdev's Debug format
        keys.push(format!("{:?}", KeyCode(self.key_code)));

        keys
    }

    /// Parse from array format
    /// Format: [modifier_keys..., main_key]
    fn from_key_array(keys: &[String]) -> Result<Self, String> {
        if keys.is_empty() {
            return Err("Empty key array".to_string());
        }

        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut super_key = false;
        let mut main_key_code: Option<u16> = None;

        for (i, key_name) in keys.iter().enumerate() {
            // Check if it's a modifier
            match key_name.as_str() {
                "KEY_LEFTCTRL" | "KEY_RIGHTCTRL" => {
                    ctrl = true;
                }
                "KEY_LEFTSHIFT" | "KEY_RIGHTSHIFT" => {
                    shift = true;
                }
                "KEY_LEFTALT" | "KEY_RIGHTALT" => {
                    alt = true;
                }
                "KEY_LEFTMETA" | "KEY_RIGHTMETA" => {
                    super_key = true;
                }
                _ => {
                    // Last key should be the main key
                    if i == keys.len() - 1 {
                        main_key_code = linux_name_to_key_code(key_name);
                        if main_key_code.is_none() {
                            return Err(format!("Unknown key name: {}", key_name));
                        }
                    } else {
                        // Non-modifier, non-last key is invalid
                        return Err(format!(
                            "Non-modifier key '{}' must be last in array",
                            key_name
                        ));
                    }
                }
            }
        }

        match main_key_code {
            Some(code) => Ok(Self::new(code, ctrl, shift, alt, super_key)),
            None => Err("No main key found in array".to_string()),
        }
    }
}

impl Default for HotkeyBinding {
    fn default() -> Self {
        // Default to Tab key with no modifiers
        Self::new(crate::constants::input::KEY_TAB, false, false, false, false)
    }
}

// Custom serialization to object format with keys and source_devices
impl Serialize for HotkeyBinding {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("HotkeyBinding", 2)?;
        state.serialize_field("keys", &self.to_key_array())?;
        state.serialize_field("source_devices", &self.source_devices)?;
        state.end()
    }
}

// Custom deserialization from object format (with backward compatibility for array format)
impl<'de> Deserialize<'de> for HotkeyBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum HotkeyFormat {
            Object {
                keys: Vec<String>,
                #[serde(default)]
                source_devices: Vec<String>,
            },
            Array(Vec<String>),
        }

        match HotkeyFormat::deserialize(deserializer)? {
            HotkeyFormat::Object {
                keys,
                source_devices,
            } => {
                let mut binding =
                    HotkeyBinding::from_key_array(&keys).map_err(de::Error::custom)?;
                binding.source_devices = source_devices;
                Ok(binding)
            }
            HotkeyFormat::Array(keys) => {
                // Legacy format - no source devices
                HotkeyBinding::from_key_array(&keys).map_err(de::Error::custom)
            }
        }
    }
}

/// Convert evdev key code to human-readable name (for UI display)
/// Uses evdev's KeyCode conversion and formats it nicely
pub fn key_code_to_name(code: u16) -> String {
    // Get the Linux key name from evdev (e.g., "KEY_TAB", "KEY_F1")
    let linux_name = format!("{:?}", KeyCode(code));

    // Strip "KEY_" prefix if present
    let name = linux_name.strip_prefix("KEY_").unwrap_or(&linux_name);

    // Format special cases for better readability
    match name {
        // Modifiers - add space between LEFT/RIGHT and modifier name
        "LEFTCTRL" => "Left Ctrl".to_string(),
        "RIGHTCTRL" => "Right Ctrl".to_string(),
        "LEFTSHIFT" => "Left Shift".to_string(),
        "RIGHTSHIFT" => "Right Shift".to_string(),
        "LEFTALT" => "Left Alt".to_string(),
        "RIGHTALT" => "Right Alt".to_string(),
        "LEFTMETA" => "Left Super".to_string(),
        "RIGHTMETA" => "Right Super".to_string(),

        // Common keys with nice formatting
        "ESC" => "Esc".to_string(),
        "BACKSPACE" => "Backspace".to_string(),
        "ENTER" => "Enter".to_string(),
        "SPACE" => "Space".to_string(),
        "CAPSLOCK" => "Caps Lock".to_string(),
        "NUMLOCK" => "Num Lock".to_string(),
        "SCROLLLOCK" => "Scroll Lock".to_string(),
        "SYSRQ" => "Print Screen".to_string(),

        // Navigation
        "PAGEUP" => "Page Up".to_string(),
        "PAGEDOWN" => "Page Down".to_string(),
        "INSERT" => "Insert".to_string(),
        "DELETE" => "Delete".to_string(),
        "HOME" => "Home".to_string(),
        "END" => "End".to_string(),

        // Media keys
        "VOLUMEUP" => "Volume Up".to_string(),
        "VOLUMEDOWN" => "Volume Down".to_string(),
        "MUTE" => "Mute".to_string(),
        "PLAYPAUSE" => "Play/Pause".to_string(),
        "STOPCD" => "Stop".to_string(),
        "NEXTSONG" => "Next".to_string(),
        "PREVIOUSSONG" => "Previous".to_string(),

        // Numpad keys - add space
        s if s.starts_with("KP") => {
            let rest = s.strip_prefix("KP").unwrap();
            match rest {
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => {
                    format!("Numpad {}", rest)
                }
                "ENTER" => "Numpad Enter".to_string(),
                "PLUS" => "Numpad Plus".to_string(),
                "MINUS" => "Numpad Minus".to_string(),
                "ASTERISK" => "Numpad Asterisk".to_string(),
                "SLASH" => "Numpad Slash".to_string(),
                "DOT" => "Numpad Period".to_string(),
                _ => format!("Numpad {}", rest),
            }
        }

        // Single letters/numbers - already clean
        s if s.len() == 1 => s.to_string(),

        // Function keys - already clean (F1, F2, etc.)
        s if s.starts_with('F') && s.len() <= 3 => s.to_string(),

        // Everything else - convert underscores to spaces and title case
        s => s
            .replace('_', " ")
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first
                        .to_uppercase()
                        .chain(chars.as_str().to_lowercase().chars())
                        .collect(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Convert Linux input event code name (KEY_*) to evdev key code
/// Uses evdev crate's KeyCode::from_str for robust parsing
fn linux_name_to_key_code(name: &str) -> Option<u16> {
    // Try to parse using evdev's built-in FromStr implementation
    if let Ok(key_code) = KeyCode::from_str(name) {
        return Some(key_code.code());
    }

    // Fallback: None if not recognized
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_name() {
        let binding = HotkeyBinding::new(15, false, false, false, false);
        assert_eq!(binding.display_name(), "Tab");

        let binding = HotkeyBinding::new(15, false, true, false, false);
        assert_eq!(binding.display_name(), "Shift+Tab");

        let binding = HotkeyBinding::new(59, true, false, false, false);
        assert_eq!(binding.display_name(), "Ctrl+F1");

        let binding = HotkeyBinding::new(59, true, true, true, false);
        assert_eq!(binding.display_name(), "Ctrl+Shift+Alt+F1");
    }

    #[test]
    fn test_matches() {
        let binding = HotkeyBinding::new(15, false, true, false, false);

        assert!(binding.matches(15, false, true, false, false));
        assert!(!binding.matches(15, false, false, false, false));
        assert!(!binding.matches(16, false, true, false, false));
    }

    #[test]
    fn test_key_code_names() {
        assert_eq!(key_code_to_name(15), "Tab");
        assert_eq!(key_code_to_name(59), "F1");
        assert_eq!(key_code_to_name(57), "Space");
        assert_eq!(key_code_to_name(30), "A");
    }

    #[test]
    fn test_to_key_array() {
        let binding = HotkeyBinding::new(15, false, false, false, false);
        assert_eq!(binding.to_key_array(), vec!["KEY_TAB"]);

        let binding = HotkeyBinding::new(15, false, true, false, false);
        assert_eq!(binding.to_key_array(), vec!["KEY_LEFTSHIFT", "KEY_TAB"]);

        let binding = HotkeyBinding::new(59, true, true, false, false);
        assert_eq!(
            binding.to_key_array(),
            vec!["KEY_LEFTCTRL", "KEY_LEFTSHIFT", "KEY_F1"]
        );
    }

    #[test]
    fn test_from_key_array() {
        let keys = vec!["KEY_TAB".to_string()];
        let binding = HotkeyBinding::from_key_array(&keys).unwrap();
        assert_eq!(binding.key_code, 15);
        assert!(!binding.shift);

        let keys = vec!["KEY_LEFTSHIFT".to_string(), "KEY_TAB".to_string()];
        let binding = HotkeyBinding::from_key_array(&keys).unwrap();
        assert_eq!(binding.key_code, 15);
        assert!(binding.shift);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let binding = HotkeyBinding::new(15, false, true, false, false);
        let json = serde_json::to_string(&binding).unwrap();
        // New object format includes keys and source_devices
        assert_eq!(
            json,
            r#"{"keys":["KEY_LEFTSHIFT","KEY_TAB"],"source_devices":[]}"#
        );

        let deserialized: HotkeyBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, binding);
    }

    #[test]
    fn test_legacy_array_format_deserialization() {
        // Test that we can still deserialize old array format configs
        let legacy_json = r#"["KEY_LEFTSHIFT","KEY_TAB"]"#;
        let binding: HotkeyBinding = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(binding.key_code, 15);
        assert!(binding.shift);
        assert!(!binding.ctrl);
        assert!(binding.source_devices.is_empty());
    }

    #[test]
    fn test_object_format_with_devices() {
        let binding = HotkeyBinding::with_devices(
            15,
            false,
            true,
            false,
            false,
            vec!["device1".to_string(), "device2".to_string()],
        );
        let json = serde_json::to_string(&binding).unwrap();
        assert_eq!(
            json,
            r#"{"keys":["KEY_LEFTSHIFT","KEY_TAB"],"source_devices":["device1","device2"]}"#
        );

        let deserialized: HotkeyBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, binding);
        assert_eq!(deserialized.source_devices, vec!["device1", "device2"]);
    }

    #[test]
    fn test_evdev_keycode_conversion() {
        // Test that we can convert to/from KEY_* names
        assert_eq!(format!("{:?}", KeyCode(15)), "KEY_TAB");
        assert_eq!(format!("{:?}", KeyCode(59)), "KEY_F1");

        // Test linux_name_to_key_code using evdev's parser
        assert_eq!(linux_name_to_key_code("KEY_TAB"), Some(15));
        assert_eq!(linux_name_to_key_code("KEY_F1"), Some(59));
        assert_eq!(linux_name_to_key_code("KEY_LEFTSHIFT"), Some(42));
        assert_eq!(linux_name_to_key_code("INVALID_KEY"), None);
    }

    #[test]
    fn test_key_code_to_name_comprehensive() {
        // Test that we now support ALL evdev keys, not just our hardcoded subset

        // Common keys
        assert_eq!(key_code_to_name(15), "Tab");
        assert_eq!(key_code_to_name(59), "F1");
        assert_eq!(key_code_to_name(57), "Space");
        assert_eq!(key_code_to_name(30), "A");

        // Modifiers
        assert_eq!(key_code_to_name(29), "Left Ctrl");
        assert_eq!(key_code_to_name(42), "Left Shift");
        assert_eq!(key_code_to_name(125), "Left Super");

        // Numpad (using evdev's KP_ prefix)
        assert_eq!(key_code_to_name(79), "Numpad 1"); // KEY_KP1
        assert_eq!(key_code_to_name(96), "Numpad Enter"); // KEY_KPENTER

        // Navigation
        assert_eq!(key_code_to_name(104), "Page Up"); // KEY_PAGEUP
        assert_eq!(key_code_to_name(102), "Home");

        // Less common keys that weren't in our original mapping
        // These would have been "Unknown" before, now they work automatically
        assert_eq!(key_code_to_name(113), "Mute"); // KEY_MUTE
        assert_eq!(key_code_to_name(114), "Volume Down"); // KEY_VOLUMEDOWN
        assert_eq!(key_code_to_name(115), "Volume Up"); // KEY_VOLUMEUP
    }
}
