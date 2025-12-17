//! Hotkey listener public API
//!
//! This module provides the public API for hotkey detection.
//! Re-exports shared types and functions from backend-specific modules.

use anyhow::Result;

use crate::config::HotkeyBinding;
use crate::input::evdev_backend;

/// Hotkey command sent from input listeners to the main daemon loop
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleCommand {
    Forward,
    Backward,
    /// Triggered when a character-specific hotkey is pressed, carrying its binding configuration for context
    CharacterHotkey(HotkeyBinding),
    /// Triggered when a profile switch hotkey is pressed
    ProfileHotkey(HotkeyBinding),
    /// Triggered when the toggle skip hotkey is pressed
    ToggleSkip,
}

/// A wrapper around CycleCommand that includes the timestamp of the input event
#[derive(Debug, Clone)]
pub struct TimestampedCommand {
    pub command: CycleCommand,
    /// X11-compatible timestamp (milliseconds)
    pub timestamp: u32,
}

/// Print helpful error message if evdev permissions are missing
pub fn print_permission_error() {
    evdev_backend::print_permission_error()
}

/// List available input devices from /dev/input/by-id/
/// Used for the evdev backend's device selector
pub fn list_input_devices() -> Result<Vec<(String, String)>> {
    evdev_backend::list_input_devices()
}
