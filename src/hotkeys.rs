//! Global hotkey listener using evdev for raw keyboard input
//!
//! Monitors keyboard devices directly via /dev/input for low-latency hotkey detection.
//! Requires 'input' group membership to access raw input devices.

use anyhow::{Context, Result};
use evdev::{Device, EventType, KeyCode};
use std::sync::mpsc::Sender;
use std::thread;
use tracing::{debug, error, info, warn};

use crate::config::HotkeyBinding;
use crate::constants::{input, paths, permissions};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleCommand {
    Forward,
    Backward,
}

/// Find all keyboard devices (devices that have a Tab key)
/// Returns Vec of (Device, PathBuf) tuples to preserve path information
fn find_all_keyboard_devices() -> Result<Vec<(Device, std::path::PathBuf)>> {
    info!(path = %paths::DEV_INPUT, "Scanning for keyboard devices...");

    let mut devices = Vec::new();

    for entry in std::fs::read_dir(paths::DEV_INPUT)
        .context(format!("Failed to read {} - are you in the '{}' group?", paths::DEV_INPUT, permissions::INPUT_GROUP))?
    {
        let entry = entry?;
        let path = entry.path();

        // Try to open device
        if let Ok(device) = Device::open(&path) {
            // Check if it has Tab key (standard keyboard indicator)
            if let Some(keys) = device.supported_keys()
                    && keys.contains(KeyCode(input::KEY_TAB)) {
                    let key_count = keys.iter().count();
                    info!(device_path = %path.display(), name = ?device.name(), key_count = key_count, "Found keyboard device");
                    devices.push((device, path));
                }
        }
    }

    if devices.is_empty() {
        anyhow::bail!(
            "No keyboard device found. Ensure you're in '{}' group:\n\
             {}\n\
             Then log out and back in.",
            permissions::INPUT_GROUP,
            permissions::ADD_TO_INPUT_GROUP
        )
    }

    info!(count = devices.len(), "Listening on keyboard device(s)");

    Ok(devices)
}

/// Spawn background threads to listen for configured hotkeys on keyboard devices
/// If selected_device_id is Some, only listen on that specific device (by-id name)
/// If None, listen on all keyboard devices
pub fn spawn_listener(
    sender: Sender<CycleCommand>,
    forward_key: HotkeyBinding,
    backward_key: HotkeyBinding,
    selected_device_id: Option<String>,
) -> Result<Vec<thread::JoinHandle<()>>> {
    let mut devices = find_all_keyboard_devices()?;

    // Filter devices if specific device selected
    if let Some(device_id) = selected_device_id {
        info!(device_id = %device_id, "Filtering to specific input device");

        // Resolve by-id symlink to actual device path
        let by_id_path = format!("/dev/input/by-id/{}", device_id);
        let target_path = std::fs::read_link(&by_id_path)
            .with_context(|| format!("Failed to resolve device {}", by_id_path))?;

        let absolute_target = if target_path.is_absolute() {
            target_path
        } else {
            std::path::Path::new("/dev/input/by-id").join(&target_path).canonicalize()
                .with_context(|| format!("Failed to canonicalize {}", target_path.display()))?
        };

        info!(selected_device = %absolute_target.display(), "Resolved device path");

        // Filter devices to only include the one matching the resolved path
        devices.retain(|(_, device_path)| {
            // Canonicalize device path for proper comparison
            if let Ok(canonical_device_path) = device_path.canonicalize() {
                canonical_device_path == absolute_target
            } else {
                false
            }
        });

        if devices.is_empty() {
            anyhow::bail!("Selected device {} not found or not accessible", device_id);
        }
    }

    let mut handles = Vec::new();

    info!(
        forward = %forward_key.display_name(),
        backward = %backward_key.display_name(),
        device_count = devices.len(),
        "Starting hotkey listeners with configured bindings"
    );

    for (device, device_path) in devices {
        let sender = sender.clone();
        let forward_key = forward_key.clone();
        let backward_key = backward_key.clone();

        let handle = thread::spawn(move || {
            info!(device = ?device.name(), path = %device_path.display(), "Hotkey listener started");
            if let Err(e) = listen_for_hotkeys(device, sender, forward_key, backward_key) {
                error!(error = %e, "Hotkey listener error");
            }
        });
        handles.push(handle);
    }

    Ok(handles)
}

/// Listen for configured hotkey events on a single device
fn listen_for_hotkeys(
    mut device: Device,
    sender: Sender<CycleCommand>,
    forward_key: HotkeyBinding,
    backward_key: HotkeyBinding,
) -> Result<()> {
    loop {
        // Fetch events (blocks until available)
        let events = device.fetch_events()
            .context("Failed to fetch events")?;

        // Collect key press events that might match our bindings
        // We need to finish with the events iterator before querying key state
        let mut potential_hotkey_presses = Vec::new();

        for event in events {
            // Only care about key events
            if event.event_type() != EventType::KEY {
                continue;
            }

            // In evdev 0.13, use event.code() to get the raw key code
            let key_code = event.code();
            let pressed = event.value() == input::KEY_PRESS;

            // Log all key events for debugging
            debug!(key_code = key_code, value = event.value(), "Key event");

            // Check if this key code matches either of our bindings
            if pressed && (key_code == forward_key.key_code || key_code == backward_key.key_code) {
                potential_hotkey_presses.push(key_code);
            }
        }

        // Now process potential hotkey presses with current keyboard state
        for key_code in potential_hotkey_presses {
            // Check real-time state of modifier keys
            // This avoids race conditions from batched events
            let key_state = device.get_key_state()
                .context("Failed to get keyboard state")?;

            let ctrl_pressed = key_state.contains(KeyCode(29)) || key_state.contains(KeyCode(97));
            let shift_pressed = key_state.contains(KeyCode(input::KEY_LEFTSHIFT))
                || key_state.contains(KeyCode(input::KEY_RIGHTSHIFT));
            let alt_pressed = key_state.contains(KeyCode(56)) || key_state.contains(KeyCode(100));
            let super_pressed = key_state.contains(KeyCode(125)) || key_state.contains(KeyCode(126));

            // Check if this matches forward key binding
            if forward_key.matches(key_code, ctrl_pressed, shift_pressed, alt_pressed, super_pressed) {
                info!(
                    binding = %forward_key.display_name(),
                    "Forward hotkey pressed, sending command"
                );
                sender.send(CycleCommand::Forward)
                    .context("Failed to send cycle command")?;
            }
            // Check if this matches backward key binding
            else if backward_key.matches(key_code, ctrl_pressed, shift_pressed, alt_pressed, super_pressed) {
                info!(
                    binding = %backward_key.display_name(),
                    "Backward hotkey pressed, sending command"
                );
                sender.send(CycleCommand::Backward)
                    .context("Failed to send cycle command")?;
            }
        }
    }
}

/// Check if hotkeys are available (user has input group permissions)
pub fn check_permissions() -> bool {
    std::fs::read_dir(paths::DEV_INPUT).is_ok()
}

/// Print helpful error message if permissions missing
pub fn print_permission_error() {
    error!(path = %paths::DEV_INPUT, "Cannot access input devices");
    error!(group = %permissions::INPUT_GROUP, "Hotkeys require group membership");
    error!(command = %permissions::ADD_TO_INPUT_GROUP, "Add user to input group");
    error!("  Then log out and back in");
    warn!(continuing = true, "Continuing without hotkey support...");
}

/// List available input devices from /dev/input/by-id/
/// Returns a list of (device_id, friendly_name) tuples
pub fn list_input_devices() -> Result<Vec<(String, String)>> {
    let by_id_path = "/dev/input/by-id";
    let mut devices = Vec::new();

    // Check if by-id directory exists
    if !std::path::Path::new(by_id_path).exists() {
        return Ok(devices);
    }

    for entry in std::fs::read_dir(by_id_path)
        .context(format!("Failed to read {} directory", by_id_path))?
    {
        let entry = entry?;
        let path = entry.path();

        // Only include event devices (skip mouse/js devices)
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.contains("-event-") {
                // Try to open device to verify it's a keyboard
                if let Ok(target) = std::fs::read_link(&path) {
                    let absolute_path = if target.is_absolute() {
                        target
                    } else {
                        std::path::Path::new(by_id_path).join(&target).canonicalize()?
                    };

                    // Check if it's a keyboard by trying to open it
                    if let Ok(device) = Device::open(&absolute_path) {
                        if let Some(keys) = device.supported_keys() {
                            if keys.contains(KeyCode(input::KEY_TAB)) {
                                // Create friendly name from device id
                                let friendly_name = name
                                    .replace("-event-kbd", "")
                                    .replace("-event-mouse", "")
                                    .replace("_", " ")
                                    .replace("-", " ");

                                devices.push((name.to_string(), friendly_name));
                            }
                        }
                    }
                }
            }
        }
    }

    // Sort by friendly name
    devices.sort_by(|a, b| a.1.cmp(&b.1));

    Ok(devices)
}
