//! Key capture functionality for interactive hotkey binding

use anyhow::{Context, Result};
use evdev::{Device, EventType, KeyCode};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::config::HotkeyBinding;
use crate::constants::{input, paths, permissions};

/// Result of a key capture operation
#[derive(Debug, Clone)]
pub enum CaptureResult {
    /// Key was successfully captured
    Captured(HotkeyBinding),
    /// User pressed Escape to cancel
    Cancelled,
    /// Capture timed out (no key pressed within timeout period)
    Timeout,
    /// Error occurred during capture
    Error(String),
}

/// Key capture state for GUI display
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureState {
    /// Currently detected modifiers (live feedback)
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_key: bool,
    /// The main key that was pressed (None until a non-modifier key is pressed)
    pub key_code: Option<u16>,
    /// Human-readable description of what's being detected
    pub description: String,
}

impl CaptureState {
    pub fn new() -> Self {
        Self {
            ctrl: false,
            shift: false,
            alt: false,
            super_key: false,
            key_code: None,
            description: "Press any key combination...".to_string(),
        }
    }

    /// Update description based on current state
    pub fn update_description(&mut self) {
        if self.key_code.is_none() {
            // Still waiting for main key
            let mut parts = Vec::new();
            if self.ctrl {
                parts.push("Ctrl");
            }
            if self.shift {
                parts.push("Shift");
            }
            if self.alt {
                parts.push("Alt");
            }
            if self.super_key {
                parts.push("Super");
            }

            if parts.is_empty() {
                self.description = "Press any key combination...".to_string();
            } else {
                self.description = format!("{}+?", parts.join("+"));
            }
        } else {
            // Key captured, show full binding
            let binding = HotkeyBinding::new(
                self.key_code.unwrap(),
                self.ctrl,
                self.shift,
                self.alt,
                self.super_key,
            );
            self.description = binding.display_name();
        }
    }
}

impl Default for CaptureState {
    fn default() -> Self {
        Self::new()
    }
}

/// Start capturing a key press in the background
/// Returns a receiver that will receive updates about capture state and final result
pub fn start_capture() -> Result<(Receiver<CaptureState>, Receiver<CaptureResult>)> {
    // Check permissions first
    if !std::fs::read_dir(paths::DEV_INPUT).is_ok() {
        return Err(anyhow::anyhow!(
            "Cannot access {}. Ensure you're in '{}' group:\n{}\nThen log out and back in.",
            paths::DEV_INPUT,
            permissions::INPUT_GROUP,
            permissions::ADD_TO_INPUT_GROUP
        ));
    }

    let (state_tx, state_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();

    thread::spawn(move || {
        match capture_key_blocking(state_tx) {
            Ok(result) => {
                let _ = result_tx.send(result);
            }
            Err(e) => {
                warn!(error = %e, "Key capture error");
                let _ = result_tx.send(CaptureResult::Error(e.to_string()));
            }
        }
    });

    Ok((state_rx, result_rx))
}

/// Blocking key capture that sends state updates via channel
fn capture_key_blocking(state_tx: Sender<CaptureState>) -> Result<CaptureResult> {
    // Find all keyboard devices
    let mut devices = find_all_keyboard_devices()
        .context("Failed to find keyboard devices for key capture")?;

    // Set all devices to non-blocking mode so we can poll them all
    for device in &mut devices {
        device.set_nonblocking(true)
            .context("Failed to set device to non-blocking mode")?;
    }

    info!(count = devices.len(), "Starting key capture on all keyboards (non-blocking mode)");

    let mut state = CaptureState::new();
    let _ = state_tx.send(state.clone());

    let timeout = Duration::from_secs(30);
    let start = std::time::Instant::now();

    loop {
        // Check for timeout
        if start.elapsed() > timeout {
            info!("Key capture timed out");
            return Ok(CaptureResult::Timeout);
        }

        // Poll all devices for events
        for device in &mut devices {
            // Try to fetch events with timeout
            match device.fetch_events() {
                Ok(events) => {
                    for event in events {
                        // Only care about key events
                        if event.event_type() != EventType::KEY {
                            continue;
                        }

                        let key_code = event.code();
                        let event_value = event.value();
                        let is_press = event_value == input::KEY_PRESS;
                        let is_release = event_value == input::KEY_RELEASE;

                        debug!(key_code = key_code, value = event_value, "Key event during capture");

                        // Check if it's Escape (cancel) - only on initial press
                        if key_code == 1 && is_press {
                            // KEY_ESC = 1
                            info!("Key capture cancelled by user (Escape)");
                            return Ok(CaptureResult::Cancelled);
                        }

                        // Update modifier state
                        // For modifiers: set true on press/repeat, false on release
                        let is_modifier = match key_code {
                            29 | 97 => {
                                // Left Ctrl (29) or Right Ctrl (97)
                                if is_release {
                                    state.ctrl = false;
                                } else {
                                    state.ctrl = true;
                                }
                                true
                            }
                            42 | 54 => {
                                // Left Shift (42) or Right Shift (54)
                                if is_release {
                                    state.shift = false;
                                } else {
                                    state.shift = true;
                                }
                                true
                            }
                            56 | 100 => {
                                // Left Alt (56) or Right Alt (100)
                                if is_release {
                                    state.alt = false;
                                } else {
                                    state.alt = true;
                                }
                                true
                            }
                            125 | 126 => {
                                // Left Super (125) or Right Super (126)
                                if is_release {
                                    state.super_key = false;
                                } else {
                                    state.super_key = true;
                                }
                                true
                            }
                            _ => false,
                        };

                        // If it's a non-modifier key press (not repeat!), capture it
                        if !is_modifier && is_press {
                            state.key_code = Some(key_code);
                            state.update_description();
                            let _ = state_tx.send(state.clone());

                            let binding = HotkeyBinding::new(
                                key_code,
                                state.ctrl,
                                state.shift,
                                state.alt,
                                state.super_key,
                            );

                            info!(binding = ?binding, "Key captured successfully");
                            return Ok(CaptureResult::Captured(binding));
                        }

                        // Update description for modifier changes
                        state.update_description();
                        let _ = state_tx.send(state.clone());
                    }
                }
                Err(e) => {
                    // Check if it's a timeout error (no events available)
                    // This is normal - just means this device has no events right now
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        continue; // Try next device
                    }
                    // For other errors, log but don't fail - one device error shouldn't stop capture
                    debug!(error = %e, "Error fetching events from device");
                }
            }
        }

        // Small sleep to avoid busy-waiting when polling multiple devices
        thread::sleep(Duration::from_millis(10));
    }
}

/// Find all keyboard devices (devices that have a Tab key)
fn find_all_keyboard_devices() -> Result<Vec<Device>> {
    info!(path = %paths::DEV_INPUT, "Scanning for keyboard devices for key capture...");

    let mut devices = Vec::new();

    for entry in std::fs::read_dir(paths::DEV_INPUT)
        .context(format!("Failed to read {} directory", paths::DEV_INPUT))?
    {
        let entry = entry?;
        let path = entry.path();

        if let Ok(device) = Device::open(&path) {
            // Check if it has Tab key (standard keyboard indicator)
            if let Some(keys) = device.supported_keys() {
                if keys.contains(KeyCode(input::KEY_TAB)) {
                    let key_count = keys.iter().count();
                    info!(device_path = %path.display(), name = ?device.name(), key_count = key_count, "Found keyboard device for capture");
                    devices.push(device);
                }
            }
        }
    }

    if devices.is_empty() {
        return Err(anyhow::anyhow!(
            "No keyboard device found for key capture. Ensure you're in '{}' group:\n{}\nThen log out and back in.",
            permissions::INPUT_GROUP,
            permissions::ADD_TO_INPUT_GROUP
        ));
    }

    info!(count = devices.len(), "Found keyboard devices for key capture");

    Ok(devices)
}
