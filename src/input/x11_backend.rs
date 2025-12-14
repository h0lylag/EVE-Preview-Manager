//! X11 XGrabKey hotkey backend
//!
//! Uses X11's native global hotkey registration via XGrabKey.
//! This is the default backend as it requires no special permissions.
//!
//! Limitations:
//! - Cannot distinguish between different physical keyboards/mice
//! - May conflict with other applications using the same hotkeys
//! - Some exotic key combinations may not work under XWayland

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::config::HotkeyBinding;
use crate::input::backend::{BackendCapabilities, HotkeyBackend};
use crate::input::listener::CycleCommand;

pub struct X11Backend;

impl HotkeyBackend for X11Backend {
    fn spawn(
        sender: Sender<CycleCommand>,
        forward_key: Option<HotkeyBinding>,
        backward_key: Option<HotkeyBinding>,
        character_hotkeys: Vec<HotkeyBinding>,
        _device_id: Option<String>, // Not used by X11 backend
    ) -> Result<Vec<JoinHandle<()>>> {
        // Check if we have any hotkeys to register
        let has_cycle = forward_key.is_some() && backward_key.is_some();
        let has_character = !character_hotkeys.is_empty();

        if !has_cycle && !has_character {
            info!("No hotkeys configured - X11 listener will not be started");
            return Ok(Vec::new());
        }

        info!(
            has_cycle_keys = has_cycle,
            character_hotkey_count = character_hotkeys.len(),
            "Starting X11 hotkey listener"
        );

        let handle = thread::spawn(move || {
            if let Err(e) = run_x11_listener(sender, forward_key, backward_key, character_hotkeys)
            {
                error!(error = %e, "X11 hotkey listener error");
            }
        });

        Ok(vec![handle])
    }

    fn is_available() -> bool {
        // Check if we can connect to X11
        x11rb::connect(None).is_ok()
    }

    fn name() -> &'static str {
        "X11"
    }

    fn capabilities() -> BackendCapabilities {
        BackendCapabilities {
            supports_cross_device_modifiers: false,
            supports_device_filtering: false,
            requires_permissions: false,
            permission_description: None,
        }
    }
}

/// Main X11 listener loop
fn run_x11_listener(
    sender: Sender<CycleCommand>,
    forward_key: Option<HotkeyBinding>,
    backward_key: Option<HotkeyBinding>,
    character_hotkeys: Vec<HotkeyBinding>,
) -> Result<()> {
    // Connect to X11
    let (conn, screen_num) =
        x11rb::connect(None).context("Failed to connect to X11 for hotkey listening")?;

    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    info!("X11 hotkey listener connected to display");

    // Build a map of (keycode, modifiers) -> CycleCommand
    let mut hotkey_map: HashMap<(Keycode, ModMask), CycleCommand> = HashMap::new();

    // Register cycle hotkeys
    if let Some(ref fwd) = forward_key {
        if let Some((keycode, modmask)) = evdev_to_x11_key(fwd) {
            register_hotkey(&conn, root, keycode, modmask)?;
            hotkey_map.insert((keycode, modmask), CycleCommand::Forward);
            info!(
                binding = %fwd.display_name(),
                x11_keycode = keycode,
                modmask = ?modmask,
                "Registered forward cycle hotkey"
            );
        } else {
            warn!(binding = %fwd.display_name(), "Failed to map forward key to X11");
        }
    }

    if let Some(ref bwd) = backward_key {
        if let Some((keycode, modmask)) = evdev_to_x11_key(bwd) {
            register_hotkey(&conn, root, keycode, modmask)?;
            hotkey_map.insert((keycode, modmask), CycleCommand::Backward);
            info!(
                binding = %bwd.display_name(),
                x11_keycode = keycode,
                modmask = ?modmask,
                "Registered backward cycle hotkey"
            );
        } else {
            warn!(binding = %bwd.display_name(), "Failed to map backward key to X11");
        }
    }

    // Register character hotkeys
    let character_hotkeys = Arc::new(character_hotkeys);
    for char_hotkey in character_hotkeys.iter() {
        if let Some((keycode, modmask)) = evdev_to_x11_key(char_hotkey) {
            register_hotkey(&conn, root, keycode, modmask)?;
            hotkey_map.insert(
                (keycode, modmask),
                CycleCommand::CharacterHotkey(char_hotkey.clone()),
            );
            info!(
                binding = %char_hotkey.display_name(),
                x11_keycode = keycode,
                modmask = ?modmask,
                "Registered character hotkey"
            );
        } else {
            warn!(binding = %char_hotkey.display_name(), "Failed to map character hotkey to X11");
        }
    }

    conn.flush().context("Failed to flush X11 connection")?;

    info!(registered_hotkeys = hotkey_map.len(), "X11 hotkeys registered, entering event loop");

    // Event loop
    loop {
        let event = conn
            .wait_for_event()
            .context("Failed to wait for X11 event")?;

        match event {
            Event::KeyPress(key_event) => {
                debug!(
                    keycode = key_event.detail,
                    state = u16::from(key_event.state),
                    "KeyPress event"
                );

                // Normalize modifiers (remove NumLock, CapsLock, etc.)
                let modmask = normalize_modmask(key_event.state);

                // Look up the hotkey
                if let Some(command) = hotkey_map.get(&(key_event.detail, modmask)) {
                    info!(
                        keycode = key_event.detail,
                        modmask = ?modmask,
                        command = ?command,
                        "Hotkey pressed, sending command"
                    );

                    if let Err(e) = sender.blocking_send(command.clone()) {
                        error!(error = %e, "Failed to send hotkey command");
                    }
                } else {
                    debug!(
                        keycode = key_event.detail,
                        modmask = ?modmask,
                        "KeyPress event didn't match any registered hotkey"
                    );
                }
            }
            Event::MappingNotify(_) => {
                // Keyboard mapping changed, we should re-register hotkeys
                // For now, just log it - full implementation would rebuild the map
                warn!("Keyboard mapping changed - hotkeys may not work correctly until restart");
            }
            _ => {
                // Ignore other events
            }
        }
    }
}

/// Register a global hotkey with X11
fn register_hotkey(conn: &RustConnection, root: Window, keycode: Keycode, modmask: ModMask) -> Result<()> {
    // We need to grab the key with and without NumLock/CapsLock/ScrollLock
    // because X11 treats these as different modifier combinations
    let ignore_masks = [
        ModMask::from(0u16),           // No lock keys
        ModMask::M2,                   // NumLock
        ModMask::LOCK,                 // CapsLock
        ModMask::M2 | ModMask::LOCK,   // Both
    ];

    for ignore_mask in &ignore_masks {
        let effective_modmask = modmask | *ignore_mask;
        
        conn.grab_key(
            false, // Don't prevent other clients from receiving events (owner_events)
            root,
            effective_modmask,
            keycode,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .with_context(|| {
            format!(
                "Failed to grab key: keycode={}, modmask={:?}",
                keycode, effective_modmask
            )
        })?;
    }

    Ok(())
}

/// Normalize modifier mask by removing lock keys
fn normalize_modmask(state: KeyButMask) -> ModMask {
    // Convert KeyButMask to u16 and back to ModMask, filtering out lock keys
    let state_u16: u16 = state.into();
    
    // Keep only Shift, Control, Mod1 (Alt), Mod4 (Super)
    // Remove Mod2 (NumLock), Lock (CapsLock), Mod5 (ScrollLock)
    let normalized = state_u16 & (
        ModMask::SHIFT.bits() |
        ModMask::CONTROL.bits() |
        ModMask::M1.bits() |
        ModMask::M4.bits()
    );
    
    ModMask::from(normalized)
}

/// Convert evdev key binding to X11 keycode and modifier mask
fn evdev_to_x11_key(binding: &HotkeyBinding) -> Option<(Keycode, ModMask)> {
    // Convert evdev keycode to X11 keycode
    let x11_keycode = evdev_keycode_to_x11(binding.key_code)?;

    // Build modifier mask
    let mut modmask = ModMask::from(0u16);

    if binding.ctrl {
        modmask |= ModMask::CONTROL;
    }
    if binding.shift {
        modmask |= ModMask::SHIFT;
    }
    if binding.alt {
        modmask |= ModMask::M1; // Alt is typically Mod1
    }
    if binding.super_key {
        modmask |= ModMask::M4; // Super is typically Mod4
    }

    Some((x11_keycode, modmask))
}

/// Convert evdev keycode to X11 keycode
///
/// X11 keycodes are typically evdev keycode + 8
/// This is the standard mapping on modern Linux systems
fn evdev_keycode_to_x11(evdev_code: u16) -> Option<Keycode> {
    // Most X11 servers use evdev + 8 mapping
    // Valid X11 keycodes are 8-255
    let x11_code = evdev_code.checked_add(8)?;
    
    if x11_code >= 8 && x11_code <= 255 {
        Some(x11_code as Keycode)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evdev_to_x11_keycode() {
        // Common keys
        assert_eq!(evdev_keycode_to_x11(1), Some(9));   // ESC: 1 -> 9
        assert_eq!(evdev_keycode_to_x11(15), Some(23)); // TAB: 15 -> 23
        assert_eq!(evdev_keycode_to_x11(59), Some(67)); // F1: 59 -> 67
        
        // Boundary cases
        assert_eq!(evdev_keycode_to_x11(0), Some(8));   // Minimum valid
        assert_eq!(evdev_keycode_to_x11(247), Some(255)); // Maximum valid
        assert_eq!(evdev_keycode_to_x11(248), None);    // Beyond range
    }

    #[test]
    fn test_evdev_to_x11_binding() {
        // Simple key (Tab)
        let binding = HotkeyBinding::new(15, false, false, false, false);
        let result = evdev_to_x11_key(&binding);
        assert_eq!(result, Some((23, ModMask::from(0u16))));

        // With Shift
        let binding = HotkeyBinding::new(15, false, true, false, false);
        let result = evdev_to_x11_key(&binding);
        assert_eq!(result, Some((23, ModMask::SHIFT)));

        // With Ctrl+Alt
        let binding = HotkeyBinding::new(59, true, false, true, false);
        let result = evdev_to_x11_key(&binding);
        assert_eq!(result, Some((67, ModMask::CONTROL | ModMask::M1)));
    }

    #[test]
    fn test_normalize_modmask() {
        // Just Shift (should be preserved)
        let state = KeyButMask::from(ModMask::SHIFT.bits());
        assert_eq!(normalize_modmask(state), ModMask::SHIFT);

        // Shift + NumLock (should remove NumLock)
        let state = KeyButMask::from(ModMask::SHIFT.bits() | ModMask::M2.bits());
        assert_eq!(normalize_modmask(state), ModMask::SHIFT);

        // Control + CapsLock (should remove CapsLock)
        let state = KeyButMask::from(ModMask::CONTROL.bits() | ModMask::LOCK.bits());
        assert_eq!(normalize_modmask(state), ModMask::CONTROL);
    }
}
