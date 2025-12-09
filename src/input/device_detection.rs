//! Input device detection for keyboards and mice
//!
//! Provides unified device detection logic used by both the hotkey listener
//! and the GUI key capture functionality.

use anyhow::{Context, Result};
use evdev::{Device, KeyCode};
use std::path::{Path, PathBuf};
use tracing::info;

use crate::constants::{input, paths, permissions};

/// Scans the system for compatible input devices, returning them with their paths for direct access
pub fn find_all_input_devices_with_paths() -> Result<Vec<(Device, PathBuf)>> {
    info!(path = %paths::DEV_INPUT, "Scanning for input devices...");

    let mut devices = Vec::new();

    for entry in std::fs::read_dir(paths::DEV_INPUT)
        .context(format!("Failed to read {} - are you in the '{}' group?", paths::DEV_INPUT, permissions::INPUT_GROUP))?
    {
        let entry = entry?;
        let path = entry.path();

        if let Ok(device) = Device::open(&path)
            && let Some(device_type) = classify_input_device(&device) {
                let key_count = device.supported_keys().map(|k| k.iter().count()).unwrap_or(0);
                info!(
                    device_path = %path.display(),
                    name = ?device.name(),
                    device_type = device_type,
                    key_count = key_count,
                    "Found input device"
                );
                devices.push((device, path));
            }
    }

    if devices.is_empty() {
        anyhow::bail!(
            "No input device found. Ensure you're in '{}' group:\n\
             {}\n\
             Then log out and back in.",
            permissions::INPUT_GROUP,
            permissions::ADD_TO_INPUT_GROUP
        )
    }

    info!(count = devices.len(), "Listening on input device(s)");

    Ok(devices)
}

/// Classify an input device as keyboard, mouse, or both
/// Returns None if the device is neither
fn classify_input_device(device: &Device) -> Option<&'static str> {
    if let Some(keys) = device.supported_keys() {
        // Accept both keyboards (Tab key) and mice (left button)
        let is_keyboard = keys.contains(KeyCode(input::KEY_TAB));
        let is_mouse = keys.contains(KeyCode(input::BTN_LEFT));

        match (is_keyboard, is_mouse) {
            (true, true) => Some("keyboard+mouse"),
            (true, false) => Some("keyboard"),
            (false, true) => Some("mouse"),
            (false, false) => None,
        }
    } else {
        None
    }
}

/// Resolves a stable device identifier from an event path by checking symlinks in /dev/input/by-id
/// Returns a human-readable device ID (e.g., "usb-Logitech_G502-event-kbd")
pub fn extract_device_id(event_path: &Path) -> String {
    let by_id_path = "/dev/input/by-id";
    
    // Try to find this device in /dev/input/by-id/
    if let Ok(entries) = std::fs::read_dir(by_id_path) {
        for entry in entries.flatten() {
            if let Ok(target) = std::fs::read_link(entry.path()) {
                let resolved = if target.is_absolute() {
                    target
                } else {
                    std::path::Path::new(by_id_path).join(&target)
                };

                // Canonicalize both paths for comparison
                if let (Ok(resolved_canonical), Ok(event_canonical)) =
                    (resolved.canonicalize(), event_path.canonicalize())
                    && resolved_canonical == event_canonical {
                    // Found the matching by-id symlink
                    if let Some(name) = entry.file_name().to_str() {
                        return name.to_string();
                    }
                }
            }
        }
    }
    
    // Fallback to event path filename if no by-id link found
    event_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}
