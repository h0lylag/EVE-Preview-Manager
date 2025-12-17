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
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::*;

use x11rb::rust_connection::RustConnection;

use crate::config::HotkeyBinding;
use crate::input::backend::{BackendCapabilities, HotkeyBackend, HotkeyConfiguration};
use crate::input::listener::{CycleCommand, TimestampedCommand};

pub struct X11Backend;

impl HotkeyBackend for X11Backend {
    fn spawn(
        sender: Sender<TimestampedCommand>,
        config: HotkeyConfiguration,
        _device_id: Option<String>, // Not used by X11 backend
        require_eve_focus: bool,
    ) -> Result<Vec<JoinHandle<()>>> {
        // Check if we have any hotkeys to register
        let has_cycle = config.forward_key.is_some() && config.backward_key.is_some();
        let has_character = !config.character_hotkeys.is_empty();
        let has_profile = !config.profile_hotkeys.is_empty();
        let has_skip = config.toggle_skip_key.is_some();

        if !has_cycle && !has_character && !has_profile && !has_skip {
            info!("No hotkeys configured - X11 listener will not be started");
            return Ok(Vec::new());
        }

        info!(
            has_cycle_keys = has_cycle,
            has_skip_key = has_skip,
            character_hotkey_count = config.character_hotkeys.len(),
            "Starting X11 hotkey listener"
        );

        let handle = thread::spawn(move || {
            if let Err(e) = run_x11_listener(sender, config, require_eve_focus) {
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
#[allow(unsafe_code)] // Required for libc::poll() system call
fn run_x11_listener(
    sender: Sender<TimestampedCommand>,
    config: HotkeyConfiguration,
    require_eve_focus: bool,
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
    if let Some(ref fwd) = config.forward_key {
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

    if let Some(ref bwd) = config.backward_key {
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

    // Register toggle skip hotkey
    if let Some(ref skip_key) = config.toggle_skip_key {
        if let Some((keycode, modmask)) = evdev_to_x11_key(skip_key) {
            register_hotkey(&conn, root, keycode, modmask)?;
            hotkey_map.insert((keycode, modmask), CycleCommand::ToggleSkip);
            info!(
                binding = %skip_key.display_name(),
                x11_keycode = keycode,
                modmask = ?modmask,
                "Registered toggle skip hotkey"
            );
        } else {
            warn!(binding = %skip_key.display_name(), "Failed to map toggle skip key to X11");
        }
    }

    // Register character hotkeys
    let character_hotkeys = Arc::new(config.character_hotkeys);
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

    // Register profile hotkeys
    let profile_hotkeys = Arc::new(config.profile_hotkeys);
    for profile_hotkey in profile_hotkeys.iter() {
        if let Some((keycode, modmask)) = evdev_to_x11_key(profile_hotkey) {
            register_hotkey(&conn, root, keycode, modmask)?;
            hotkey_map.insert(
                (keycode, modmask),
                CycleCommand::ProfileHotkey(profile_hotkey.clone()),
            );
            info!(
                binding = %profile_hotkey.display_name(),
                x11_keycode = keycode,
                modmask = ?modmask,
                "Registered profile hotkey"
            );
        } else {
            warn!(binding = %profile_hotkey.display_name(), "Failed to map profile hotkey to X11");
        }
    }

    conn.flush().context("Failed to flush X11 connection")?;

    info!(
        registered_hotkeys = hotkey_map.len(),
        "X11 hotkeys registered, entering event loop"
    );

    // Track whether hotkeys are currently grabbed
    let mut hotkeys_grabbed = true;
    let mut last_focused_window: Option<Window> = None;

    // Get the raw file descriptor for poll()-based blocking
    let x11_fd = conn.stream().as_raw_fd();

    // Event loop - block on X11 fd with timeout for focus checking
    loop {
        // Use poll() to block with 250ms timeout
        // This gives us:
        // - Zero CPU usage when idle (thread sleeps in kernel)
        // - Instant event response (wakes immediately when event arrives)
        // - Periodic focus checking (every 250ms on timeout)
        let mut poll_fds = [libc::pollfd {
            fd: x11_fd,
            events: libc::POLLIN,
            revents: 0,
        }];

        // SAFETY: `poll_fds` is a valid pointer to a stack-allocated array of `pollfd`.
        // The array length is 1, which matches the second argument.
        // The timeout is 250ms, which is a safe integer value.
        let poll_result = unsafe { libc::poll(poll_fds.as_mut_ptr(), 1, 250) };

        if poll_result < 0 {
            return Err(anyhow::anyhow!("poll() failed"));
        }

        // Check if X11 events are available
        if poll_result > 0 && (poll_fds[0].revents & libc::POLLIN) != 0 {
            // Process all available events (may be multiple)
            while let Some(event) = conn.poll_for_event()? {
                match event {
                    Event::KeyPress(key_event) => {
                        // If hotkeys are not grabbed, this event shouldn't reach us
                        // But handle it anyway for robustness
                        if !hotkeys_grabbed {
                            conn.allow_events(Allow::REPLAY_KEYBOARD, key_event.time)?;
                            conn.flush()?;
                            continue;
                        }

                        // Hotkeys are grabbed, process normally
                        // Check if we need EVE focus
                        if require_eve_focus {
                            let focus_cookie = conn.get_input_focus()?;
                            let focused_window = focus_cookie.reply()?.focus;
                            let title =
                                get_window_title_sync(&conn, focused_window).unwrap_or_default();
                            let is_eve = title
                                .starts_with(crate::constants::eve::WINDOW_TITLE_PREFIX)
                                || title == crate::constants::eve::LOGGED_OUT_TITLE;

                            if !is_eve {
                                debug!(title = %title, "EVE focus required but not focused, replaying");
                                conn.allow_events(Allow::REPLAY_KEYBOARD, key_event.time)?;
                                conn.flush()?;
                                continue;
                            }
                        }

                        // If we got here, we consume the event.
                        info!(keycode = key_event.detail, "Consuming hotkey event");
                        conn.allow_events(Allow::ASYNC_KEYBOARD, key_event.time)?;
                        conn.flush()?;

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

                            let timestamped_command = TimestampedCommand {
                                command: command.clone(),
                                timestamp: key_event.time,
                            };

                            if let Err(e) = sender.blocking_send(timestamped_command) {
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
                        warn!(
                            "Keyboard mapping changed - hotkeys may not work correctly until restart"
                        );
                    }
                    _ => {
                        // Ignore other events
                    }
                }
            }
        }

        // Timeout expired or poll returned - check focus
        // This runs every 100ms (the poll timeout) when no events are arriving
        let focus_cookie = conn.get_input_focus()?;
        let focused_window = focus_cookie.reply()?.focus;

        // Only check class if focus changed (optimization)
        if last_focused_window != Some(focused_window) {
            last_focused_window = Some(focused_window);
            let focused_class = get_window_class_sync(&conn, focused_window).unwrap_or_default();
            let is_epm_focused = focused_class.eq_ignore_ascii_case("eve-preview-manager");

            // If EPM gained focus, ungrab hotkeys
            if is_epm_focused && hotkeys_grabbed {
                info!("EPM gained focus, ungrabbing hotkeys to allow normal input");
                for (keycode, modmask) in hotkey_map.keys() {
                    ungrab_hotkey(&conn, root, *keycode, *modmask)?;
                }
                hotkeys_grabbed = false;
                conn.flush()?;
            }
            // If EPM lost focus, regrab hotkeys
            else if !is_epm_focused && !hotkeys_grabbed {
                info!("EPM lost focus, re-grabbing hotkeys");
                for (keycode, modmask) in hotkey_map.keys() {
                    register_hotkey(&conn, root, *keycode, *modmask)?;
                }
                hotkeys_grabbed = true;
                conn.flush()?;
            }
        }
    }
}

/// Helper to synchronously get window class
fn get_window_class_sync(conn: &RustConnection, window: Window) -> Result<String> {
    let cookie = conn.get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)?;
    let reply = cookie.reply()?;

    if let Some(val) = reply.value8() {
        // WM_CLASS contains two null-terminated strings: <instance>\0<class>\0
        let bytes: Vec<u8> = val.collect();
        // Split by null byte
        let parts: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();

        // We usually care about the class (second string)
        if parts.len() >= 2 && !parts[1].is_empty() {
            Ok(String::from_utf8_lossy(parts[1]).into_owned())
        } else if !parts.is_empty() {
            Ok(String::from_utf8_lossy(parts[0]).into_owned())
        } else {
            Ok(String::new())
        }
    } else {
        Ok(String::new())
    }
}

/// Helper to ungrab a hotkey (reverse of register_hotkey)
fn ungrab_hotkey(
    conn: &RustConnection,
    root: Window,
    keycode: Keycode,
    modmask: ModMask,
) -> Result<()> {
    // Ungrab all the same permutations we grabbed in register_hotkey
    let ignore_masks = [
        ModMask::from(0u16),         // No lock keys
        ModMask::M2,                 // NumLock (Mod2)
        ModMask::LOCK,               // CapsLock
        ModMask::M2 | ModMask::LOCK, // NumLock + CapsLock
    ];

    for ignore_mask in &ignore_masks {
        let effective_modmask = modmask | *ignore_mask;
        conn.ungrab_key(keycode, root, effective_modmask)?;
    }

    Ok(())
}

fn get_window_title_sync(conn: &RustConnection, window: Window) -> Result<String> {
    let cookie = conn.get_property(false, window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 1024)?;
    let reply = cookie.reply()?;

    if let Some(val) = reply.value8() {
        // WM_NAME is typically a string, possibly Latin-1 but often ASCII/UTF-8 compatible for our prefix
        let bytes: Vec<u8> = val.collect();
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        Ok(String::new())
    }
}

/// Register a global hotkey with X11
fn register_hotkey(
    conn: &RustConnection,
    root: Window,
    keycode: Keycode,
    modmask: ModMask,
) -> Result<()> {
    // We must grab the key for every possible combination of "ignored" modifiers
    // (NumLock, CapsLock, ScrollLock).
    // X11 treats "Ctrl+C" and "Ctrl+C+NumLock" as completely different hotkeys.
    // By grabbing all permutations, we ensure the hotkey works regardless of Lock key state.
    let ignore_masks = [
        ModMask::from(0u16),         // No lock keys
        ModMask::M2,                 // NumLock (Mod2)
        ModMask::LOCK,               // CapsLock
        ModMask::M2 | ModMask::LOCK, // NumLock + CapsLock
    ];

    for ignore_mask in &ignore_masks {
        let effective_modmask = modmask | *ignore_mask;

        conn.grab_key(
            false, // owner_events: false = Send events to this client only, do not propagate to other windows
            root,
            effective_modmask,
            keycode,
            GrabMode::SYNC,  // SYNC to allow ReplayKeyboard decision
            GrabMode::ASYNC, // Keep mouse processing normal
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
    let normalized = state_u16
        & (ModMask::SHIFT.bits()
            | ModMask::CONTROL.bits()
            | ModMask::M1.bits()
            | ModMask::M4.bits());

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

    if (8..=255).contains(&x11_code) {
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
        assert_eq!(evdev_keycode_to_x11(1), Some(9)); // ESC: 1 -> 9
        assert_eq!(evdev_keycode_to_x11(15), Some(23)); // TAB: 15 -> 23
        assert_eq!(evdev_keycode_to_x11(59), Some(67)); // F1: 59 -> 67

        // Boundary cases
        assert_eq!(evdev_keycode_to_x11(0), Some(8)); // Minimum valid
        assert_eq!(evdev_keycode_to_x11(247), Some(255)); // Maximum valid
        assert_eq!(evdev_keycode_to_x11(248), None); // Beyond range
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
