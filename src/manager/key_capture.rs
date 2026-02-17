//! Key capture functionality for interactive hotkey binding
//! Supports both keyboard keys and mouse buttons

use anyhow::{Context, Result};
use evdev::EventType;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::common::constants::{input, paths, permissions};
use crate::config::{HotkeyBackendType, HotkeyBinding};
use crate::input::device_detection;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, GrabMode, KeyButMask};

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

/// Key capture state for Manager display
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
            description: "Press any key or mouse button...".to_string(),
        }
    }

    /// Update description based on current state
    pub fn update_description(&mut self) {
        if let Some(key_code) = self.key_code {
            // Key captured, show full binding
            let binding =
                HotkeyBinding::new(key_code, self.ctrl, self.shift, self.alt, self.super_key);
            self.description = binding.display_name();
        } else {
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
                self.description = "Press any key or mouse button...".to_string();
            } else {
                self.description = format!("{}+?", parts.join("+"));
            }
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
pub fn start_capture(
    backend: HotkeyBackendType,
) -> Result<(Receiver<CaptureState>, Receiver<CaptureResult>, Sender<()>)> {
    // Check permissions first if using evdev
    if backend == HotkeyBackendType::Evdev && std::fs::read_dir(paths::DEV_INPUT).is_err() {
        return Err(anyhow::anyhow!(
            "Cannot access {}. Ensure you're in '{}' group:\n{}\nThen log out and back in.",
            paths::DEV_INPUT,
            permissions::INPUT_GROUP,
            permissions::ADD_TO_INPUT_GROUP
        ));
    }

    let (state_tx, state_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let (cancel_tx, cancel_rx) = mpsc::channel();

    thread::spawn(move || {
        let result = match backend {
            HotkeyBackendType::X11 => capture_key_x11(state_tx, cancel_rx),
            HotkeyBackendType::Evdev => capture_key_blocking(state_tx, cancel_rx),
        };

        match result {
            Ok(res) => {
                let _ = result_tx.send(res);
            }
            Err(e) => {
                warn!(error = %e, "Key capture error");
                let _ = result_tx.send(CaptureResult::Error(e.to_string()));
            }
        }
    });

    Ok((state_rx, result_rx, cancel_tx))
}

/// Blocking key capture using X11 GrabKeyboard
fn capture_key_x11(
    state_tx: Sender<CaptureState>,
    cancel_rx: Receiver<()>,
) -> Result<CaptureResult> {
    let (conn, screen_num) = x11rb::connect(None).context("Failed to connect to X11")?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    // Retry grabbing the keyboard.
    // This is necessary because another client (e.g., the window manager or a held button press)
    // might momentarily block the generic grab. We retry with a short timeout.
    let grab_timeout = Duration::from_millis(1000);
    let grab_start = std::time::Instant::now();
    let mut grabbed = false;

    while grab_start.elapsed() < grab_timeout {
        let reply = conn
            .grab_keyboard(
                false,
                root,
                x11rb::CURRENT_TIME,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )
            .context("Failed to grab keyboard")?
            .reply()
            .context("Failed to get grab_keyboard reply")?;

        if reply.status == x11rb::protocol::xproto::GrabStatus::SUCCESS {
            grabbed = true;
            break;
        } else if reply.status == x11rb::protocol::xproto::GrabStatus::ALREADY_GRABBED {
            // Wait and retry - using a very short sleep to minimize perceived latency
            // while still yielding to the scheduler.
            thread::sleep(Duration::from_millis(1));
            continue;
        } else {
            // Other error (InvalidTime, NotViewable, Frozen, etc.)
            return Err(anyhow::anyhow!(
                "GrabKeyboard failed with status: {:?}",
                reply.status
            ));
        }
    }

    if !grabbed {
        return Err(anyhow::anyhow!(
            "Failed to grab keyboard after retrying (AlreadyGrabbed)"
        ));
    }

    // Force a roundtrip to ensure the server has processed the grab and we are consistent.
    // This often fixes issues where events aren't delivered immediately after a grab.
    let _ = conn.get_input_focus()?.reply()?;

    info!("Keyboard grabbed for X11 key capture");

    let mut state = CaptureState::new();
    let _ = state_tx.send(state.clone());

    let timeout = Duration::from_secs(30);
    let start = std::time::Instant::now();

    // The X11 connection drop (RAII) will automatically release the keyboard grab.
    // We don't need an explicit ungrab at exit points.

    loop {
        if start.elapsed() > timeout {
            return Ok(CaptureResult::Timeout);
        }

        // Check for cancellation signal
        if cancel_rx.try_recv().is_ok() {
            info!("Key capture cancelled by signal");
            return Ok(CaptureResult::Cancelled);
        }

        // Ensure requests are sent
        let _ = conn.flush();

        // Non-blocking poll using x11rb
        if let Some(event) = conn.poll_for_event()? {
            match event {
                x11rb::protocol::Event::KeyPress(key_press) => {
                    let keycode = key_press.detail;
                    let state_mask = key_press.state;

                    // Convert X11 keycode to evdev (usually subtract 8).
                    // We need this conversion because `HotkeyBinding` internally stores keys
                    // using universally consistent evdev codes, regardless of the backend.
                    // X11 keycodes are offset by 8 from the kernel's evdev codes.
                    let evdev_code = (keycode as u16).saturating_sub(8);

                    // Check for Escape (evdev 1) first to allow cancelling
                    if evdev_code == 1 {
                        return Ok(CaptureResult::Cancelled);
                    }

                    debug!(x11_keycode=keycode, evdev_code=evdev_code, state=?state_mask, "X11 KeyPress");

                    // Map X11 modifier mask bits to our internal boolean flags
                    let modmask = state_mask;
                    state.shift = modmask.contains(KeyButMask::SHIFT);
                    state.ctrl = modmask.contains(KeyButMask::CONTROL);
                    state.alt = modmask.contains(KeyButMask::MOD1);
                    state.super_key = modmask.contains(KeyButMask::MOD4);

                    // Identify if the pressed key ITSELF is a modifier.
                    // We need to special-case this because the `state` mask in X11 reflects
                    // modifiers that were *already* down before this press.
                    // For visual feedback in the UI ("Ctrl + ?"), we want to show the modifier
                    // as active the moment it is pressed.
                    let is_modifier_key =
                        matches!(evdev_code, 42 | 54 | 29 | 97 | 56 | 100 | 125 | 126);

                    if is_modifier_key {
                        // Update the specific modifier flag for the key just pressed
                        match evdev_code {
                            42 | 54 => state.shift = true,
                            29 | 97 => state.ctrl = true,
                            56 | 100 => state.alt = true,
                            125 | 126 => state.super_key = true,
                            _ => {}
                        }

                        state.update_description();
                        let _ = state_tx.send(state.clone());
                    } else {
                        // Non-modifier key pressed - this is our hotkey trigger
                        state.key_code = Some(evdev_code);
                        state.update_description();

                        let binding = HotkeyBinding::new(
                            evdev_code,
                            state.ctrl,
                            state.shift,
                            state.alt,
                            state.super_key,
                        );

                        // X11 generic capture doesn't distinguish source devices
                        let _ = state_tx.send(state.clone());
                        return Ok(CaptureResult::Captured(binding));
                    }
                }
                x11rb::protocol::Event::KeyRelease(key_release) => {
                    // Update modifier visual state on release.
                    // This ensures that if a user releases 'Ctrl' without pressing another key,
                    // the UI feedback updates correctly ("Ctrl + ?" -> "Press any key...").
                    let evdev_code = (key_release.detail as u16).saturating_sub(8);
                    match evdev_code {
                        42 | 54 => state.shift = false,
                        29 | 97 => state.ctrl = false,
                        56 | 100 => state.alt = false,
                        125 | 126 => state.super_key = false,
                        _ => {}
                    }
                    if state.key_code.is_none() {
                        state.update_description();
                        let _ = state_tx.send(state.clone());
                    }
                }
                _ => {}
            }
        }

        thread::sleep(Duration::from_millis(10));
    }
}

/// Blocking key capture that sends state updates via channel
fn capture_key_blocking(
    state_tx: Sender<CaptureState>,
    cancel_rx: Receiver<()>,
) -> Result<CaptureResult> {
    // Find all input devices (keyboards and mice) with their paths
    let devices_with_paths = device_detection::find_all_input_devices_with_paths()
        .context("Failed to find input devices for key capture")?;

    // Convert to mutable devices and track their device IDs
    let mut devices_and_ids: Vec<_> = devices_with_paths
        .into_iter()
        .map(|(device, path)| {
            let dev = device;
            dev.set_nonblocking(true).ok();
            let device_id = device_detection::extract_device_id(&path);
            (dev, device_id)
        })
        .collect();

    info!(
        count = devices_and_ids.len(),
        "Starting key capture on all input devices (non-blocking mode)"
    );

    let mut state = CaptureState::new();
    let _ = state_tx.send(state.clone());

    // Track which devices have contributed to the current key combo
    let mut contributing_devices: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    let timeout = Duration::from_secs(30);
    let start = std::time::Instant::now();

    loop {
        // Check for timeout
        if start.elapsed() > timeout {
            info!("Key capture timed out");
            return Ok(CaptureResult::Timeout);
        }

        // Check for cancellation signal
        if cancel_rx.try_recv().is_ok() {
            info!("Key capture cancelled by signal");
            return Ok(CaptureResult::Cancelled);
        }

        // Poll all devices for events
        for (device, device_id) in &mut devices_and_ids {
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

                        debug!(key_code = key_code, value = event_value, device_id = %device_id, "Key event during capture");

                        // Update modifier state first
                        // For modifiers: set true on press/repeat, false on release
                        let is_modifier = match key_code {
                            29 | 97 => {
                                // Left Ctrl (29) or Right Ctrl (97)
                                state.ctrl = !is_release;
                                if !is_release {
                                    contributing_devices.insert(device_id.clone());
                                }
                                true
                            }
                            42 | 54 => {
                                // Left Shift (42) or Right Shift (54)
                                state.shift = !is_release;
                                if !is_release {
                                    contributing_devices.insert(device_id.clone());
                                }
                                true
                            }
                            56 | 100 => {
                                // Left Alt (56) or Right Alt (100)
                                state.alt = !is_release;
                                if !is_release {
                                    contributing_devices.insert(device_id.clone());
                                }
                                true
                            }
                            125 | 126 => {
                                // Left Super (125) or Right Super (126)
                                state.super_key = !is_release;
                                if !is_release {
                                    contributing_devices.insert(device_id.clone());
                                }
                                true
                            }
                            _ => false,
                        };

                        // If it's a non-modifier key press (not repeat!), process it
                        if !is_modifier && is_press {
                            // Check if it's Escape (cancel)
                            if key_code == 1 {
                                // KEY_ESC = 1
                                info!("Key capture cancelled by user (Escape)");
                                return Ok(CaptureResult::Cancelled);
                            }

                            // Block left and right mouse buttons (they interfere with UI interaction)
                            if key_code == input::BTN_LEFT || key_code == input::BTN_RIGHT {
                                debug!(
                                    "Ignoring mouse button {} (not allowed as hotkey)",
                                    key_code
                                );
                                continue;
                            }

                            // Add this device to contributors (main key source)
                            contributing_devices.insert(device_id.clone());

                            // Otherwise, capture the key
                            state.key_code = Some(key_code);
                            state.update_description();
                            let _ = state_tx.send(state.clone());

                            // Convert HashSet to sorted Vec for consistent ordering
                            let mut source_devices: Vec<String> =
                                contributing_devices.iter().cloned().collect();
                            source_devices.sort();

                            let binding = HotkeyBinding::with_devices(
                                key_code,
                                state.ctrl,
                                state.shift,
                                state.alt,
                                state.super_key,
                                source_devices,
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
