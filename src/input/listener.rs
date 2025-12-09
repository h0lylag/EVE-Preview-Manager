//! Global hotkey listener using evdev for raw keyboard and mouse input
//!
//! Monitors input devices directly via /dev/input for low-latency hotkey detection.
//! Supports both keyboard keys and mouse buttons (including Mouse 4/5 side buttons).
//! Requires 'input' group membership to access raw input devices.

use anyhow::{Context, Result};
use evdev::{Device, EventType, KeyCode};
use tokio::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use tracing::{debug, error, info, warn};

use crate::config::HotkeyBinding;
use crate::constants::{input, paths, permissions};
use crate::input::device_detection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleCommand {
    Forward,
    Backward,
    /// Per-character hotkey pressed - includes the binding for cycle group lookup
    CharacterHotkey(HotkeyBinding),
}

/// Spawn background threads to listen for configured hotkeys on input devices (keyboards and mice)
pub fn spawn_listener(
    sender: tokio::sync::mpsc::Sender<CycleCommand>,
    forward_key: Option<HotkeyBinding>,
    backward_key: Option<HotkeyBinding>,
    character_hotkeys: Vec<HotkeyBinding>,
    selected_device_id: Option<String>,
) -> Result<Vec<thread::JoinHandle<()>>> {
    // Get all device paths for cross-device modifier state queries
    let all_device_paths: Vec<_> = device_detection::find_all_input_devices_with_paths()?
        .into_iter()
        .map(|(_dev, path)| path)
        .collect();
    
    let mut devices = device_detection::find_all_input_devices_with_paths()?;

    // Handle device selection
    match selected_device_id.as_deref() {
        None => {
            // No device selected - hotkeys disabled
            info!("No input device selected, hotkey listener disabled");
            return Ok(Vec::new());
        }
        Some("all") => {
            // Listen on all devices - no filtering needed
            info!("Listening on all input devices");
        }
        Some("auto") => {
            // Auto-detect mode: use devices from the hotkey bindings
            info!("Auto-detect mode: using devices from hotkey bindings");

            // Collect all unique device IDs from all bindings (cycle + per-character)
            let mut required_devices = std::collections::HashSet::new();
            if let Some(ref fwd) = forward_key {
                required_devices.extend(fwd.source_devices.iter().cloned());
            }
            if let Some(ref bwd) = backward_key {
                required_devices.extend(bwd.source_devices.iter().cloned());
            }
            for binding in &character_hotkeys {
                required_devices.extend(binding.source_devices.iter().cloned());
            }

            if required_devices.is_empty() {
                warn!("Auto-detect mode but no source devices found in bindings, listening on all devices");
            } else {
                // Filter to only the required devices
                info!(devices = ?required_devices, "Filtering to auto-detected devices");
                
                devices.retain(|(_, device_path)| {
                    let device_id = device_detection::extract_device_id(device_path);
                    required_devices.contains(&device_id)
                });

                if devices.is_empty() {
                    warn!("None of the auto-detected devices found, falling back to all devices");
                    devices = device_detection::find_all_input_devices_with_paths()?;
                }
            }
        }
        Some(device_id) => {
            // Legacy: specific device ID (from old configs)
            info!(device_id = %device_id, "Filtering to specific input device (legacy)");

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

            devices.retain(|(_, device_path)| {
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
    }

    let mut handles = Vec::new();

    // Share all device paths so each listener can query modifier state from all devices
    let all_device_paths = Arc::new(all_device_paths);

    let cycle_configured = forward_key.is_some() && backward_key.is_some();
    let has_character_hotkeys = !character_hotkeys.is_empty();

    if cycle_configured {
        info!(
            forward = %forward_key.as_ref().unwrap().display_name(),
            backward = %backward_key.as_ref().unwrap().display_name(),
            character_hotkey_count = character_hotkeys.len(),
            device_count = devices.len(),
            "Starting hotkey listeners with cycle and per-character bindings"
        );
    } else if has_character_hotkeys {
        info!(
            character_hotkey_count = character_hotkeys.len(),
            device_count = devices.len(),
            "Starting hotkey listeners with per-character bindings only"
        );
    } else {
        warn!("No hotkeys configured - hotkey listener will not be started");
        return Ok(Vec::new());
    }

    for (device, device_path) in devices {
        let sender = sender.clone();
        let forward_key = forward_key.clone();
        let backward_key = backward_key.clone();
        let character_hotkeys = character_hotkeys.clone();
        let all_device_paths = Arc::clone(&all_device_paths);

        let handle = thread::spawn(move || {
            info!(device = ?device.name(), path = %device_path.display(), "Hotkey listener started");
            if let Err(e) = listen_for_hotkeys(device, sender, forward_key, backward_key, character_hotkeys, all_device_paths) {
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
    forward_key: Option<HotkeyBinding>,
    backward_key: Option<HotkeyBinding>,
    character_hotkeys: Vec<HotkeyBinding>,
    all_device_paths: Arc<Vec<std::path::PathBuf>>,
) -> Result<()> {
    loop {
        let events = device.fetch_events()
            .context("Failed to fetch events")?;

        let mut potential_hotkey_presses = Vec::new();

        // Collect potential hotkey presses (non-modifier keys)
        for event in events {
            if event.event_type() != EventType::KEY {
                continue;
            }

            let key_code = event.code();
            let pressed = event.value() == input::KEY_PRESS;

            debug!(key_code = key_code, value = event.value(), "Key event");

            // Collect non-modifier key presses that might be hotkeys
            if pressed {
                let is_cycle_key = forward_key.as_ref().is_some_and(|fwd| fwd.key_code == key_code)
                    || backward_key.as_ref().is_some_and(|bwd| bwd.key_code == key_code);
                let is_character_key = character_hotkeys.iter().any(|hk| hk.key_code == key_code);

                if is_cycle_key || is_character_key {
                    potential_hotkey_presses.push(key_code);
                }
            }
        }

        // For each potential hotkey, query current modifier state from ALL devices
        for key_code in potential_hotkey_presses {
            // Query modifier state across all devices to handle cross-device hotkeys
            // (e.g., Shift held on keyboard + Mouse Button pressed on mouse)
            let mut ctrl_pressed = false;
            let mut shift_pressed = false;
            let mut alt_pressed = false;
            let mut super_pressed = false;

            for device_path in all_device_paths.iter() {
                if let Ok(dev) = Device::open(device_path)
                    && let Ok(key_state) = dev.get_key_state() {
                    ctrl_pressed |= key_state.contains(KeyCode(29)) || key_state.contains(KeyCode(97));
                    shift_pressed |= key_state.contains(KeyCode(input::KEY_LEFTSHIFT))
                        || key_state.contains(KeyCode(input::KEY_RIGHTSHIFT));
                    alt_pressed |= key_state.contains(KeyCode(56)) || key_state.contains(KeyCode(100));
                    super_pressed |= key_state.contains(KeyCode(125)) || key_state.contains(KeyCode(126));
                }
            }

            // Check cycle hotkeys first (Forward/Backward take priority)
            let mut handled = false;

            if let Some(ref fwd) = forward_key
                && fwd.matches(key_code, ctrl_pressed, shift_pressed, alt_pressed, super_pressed) {
                info!(
                    binding = %fwd.display_name(),
                    "Forward hotkey pressed, sending command"
                );
                sender.blocking_send(CycleCommand::Forward)
                    .context("Failed to send cycle command")?;
                handled = true;
            }

            if !handled
                && let Some(ref bwd) = backward_key
                && bwd.matches(key_code, ctrl_pressed, shift_pressed, alt_pressed, super_pressed) {
                info!(
                    binding = %bwd.display_name(),
                    "Backward hotkey pressed, sending command"
                );
                sender.blocking_send(CycleCommand::Backward)
                    .context("Failed to send cycle command")?;
                handled = true;
            }

            if !handled {
                // Check per-character hotkeys
                for char_hotkey in &character_hotkeys {
                    if char_hotkey.matches(key_code, ctrl_pressed, shift_pressed, alt_pressed, super_pressed) {
                        info!(
                            binding = %char_hotkey.display_name(),
                            "Per-character hotkey pressed, sending command"
                        );
                        sender.blocking_send(CycleCommand::CharacterHotkey(char_hotkey.clone()))
                            .context("Failed to send character hotkey command")?;
                        break; // Only send one command per keypress
                    }
                }
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
pub fn list_input_devices() -> Result<Vec<(String, String)>> {
    let by_id_path = "/dev/input/by-id";
    let mut devices = Vec::new();

    if !std::path::Path::new(by_id_path).exists() {
        return Ok(devices);
    }

    for entry in std::fs::read_dir(by_id_path)
        .context(format!("Failed to read {} directory", by_id_path))?
    {
        let entry = entry?;
        let path = entry.path();

        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.contains("-event-")
                && let Ok(target) = std::fs::read_link(&path) {
                    let absolute_path = if target.is_absolute() {
                        target
                    } else {
                        std::path::Path::new(by_id_path).join(&target).canonicalize()?
                    };

                    if let Ok(device) = Device::open(&absolute_path)
                        && let Some(keys) = device.supported_keys() {
                            // Accept both keyboards (Tab key) and mice (left button)
                            let is_keyboard = keys.contains(KeyCode(input::KEY_TAB));
                            let is_mouse = keys.contains(KeyCode(input::BTN_LEFT));

                            if is_keyboard || is_mouse {
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

    devices.sort_by(|a, b| a.1.cmp(&b.1));

    Ok(devices)
}
