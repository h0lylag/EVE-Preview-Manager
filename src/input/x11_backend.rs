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
use std::time::{Duration, Instant};
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::*;

use x11rb::rust_connection::RustConnection;

use crate::config::HotkeyBinding;
use crate::input::backend::{
    AllowedWindows, BackendCapabilities, HotkeyBackend, HotkeyConfiguration,
};
use crate::input::listener::{CycleCommand, TimestampedCommand};

pub struct X11Backend;

impl HotkeyBackend for X11Backend {
    fn spawn(
        sender: Sender<TimestampedCommand>,
        config: HotkeyConfiguration,
        _device_id: Option<String>, // Not used by X11 backend
        require_eve_focus: bool,
        allowed_windows: AllowedWindows,
    ) -> Result<Vec<JoinHandle<()>>> {
        // Check if we have any hotkeys to register
        let has_cycle = !config.cycle_hotkeys.is_empty();
        let has_character = !config.character_hotkeys.is_empty();
        let has_profile = !config.profile_hotkeys.is_empty();
        let has_skip = config.toggle_skip_key.is_some();
        let has_toggle_previews = config.toggle_previews_key.is_some();

        if !has_cycle && !has_character && !has_profile && !has_skip && !has_toggle_previews {
            info!("No hotkeys configured - X11 listener will not be started");
            return Ok(Vec::new());
        }

        debug!(
            has_cycle_keys = has_cycle,
            has_skip_key = has_skip,
            has_toggle_previews_key = has_toggle_previews,
            character_hotkey_count = config.character_hotkeys.len(),
            "Starting X11 hotkey listener"
        );

        let handle = thread::spawn(move || {
            if let Err(e) = run_x11_listener(sender, config, require_eve_focus, allowed_windows) {
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
    sender: Sender<TimestampedCommand>,
    config: HotkeyConfiguration,
    require_eve_focus: bool,
    allowed_windows: AllowedWindows,
) -> Result<()> {
    // Connect to X11
    let (conn, screen_num) =
        x11rb::connect(None).context("Failed to connect to X11 for hotkey listening")?;

    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    debug!("X11 hotkey listener connected to display");

    // Build a map of (keycode, modifiers) -> CycleCommand
    let mut hotkey_map: HashMap<(Keycode, ModMask), CycleCommand> = HashMap::new();

    // Register cycle hotkeys
    let cycle_hotkeys = Arc::new(config.cycle_hotkeys);
    for (command, cycle_hotkey) in cycle_hotkeys.iter() {
        if let Some((keycode, modmask)) = evdev_to_x11_key(cycle_hotkey) {
            register_hotkey(&conn, root, keycode, modmask)?;
            hotkey_map.insert((keycode, modmask), command.clone());
            debug!(
                binding = %cycle_hotkey.display_name(),
                x11_keycode = keycode,
                modmask = ?modmask,
                command = ?command,
                "Registered cycle hotkey"
            );
        } else {
            warn!(binding = %cycle_hotkey.display_name(), "Failed to map cycle hotkey to X11");
        }
    }

    // Register toggle skip hotkey
    if let Some(ref skip_key) = config.toggle_skip_key {
        if let Some((keycode, modmask)) = evdev_to_x11_key(skip_key) {
            register_hotkey(&conn, root, keycode, modmask)?;
            hotkey_map.insert((keycode, modmask), CycleCommand::ToggleSkip);
            debug!(
                binding = %skip_key.display_name(),
                x11_keycode = keycode,
                modmask = ?modmask,
                "Registered toggle skip hotkey"
            );
        } else {
            warn!(binding = %skip_key.display_name(), "Failed to map toggle skip key to X11");
        }
    }

    // Register toggle previews hotkey
    if let Some(ref toggle_previews_key) = config.toggle_previews_key {
        if let Some((keycode, modmask)) = evdev_to_x11_key(toggle_previews_key) {
            register_hotkey(&conn, root, keycode, modmask)?;
            hotkey_map.insert((keycode, modmask), CycleCommand::TogglePreviews);
            debug!(
                binding = %toggle_previews_key.display_name(),
                x11_keycode = keycode,
                modmask = ?modmask,
                "Registered toggle previews hotkey"
            );
        } else {
            warn!(binding = %toggle_previews_key.display_name(), "Failed to map toggle previews key to X11");
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
            debug!(
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
            debug!(
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

    debug!(
        registered_hotkeys = hotkey_map.len(),
        "X11 hotkeys registered, entering event loop"
    );

    listen_for_hotkeys(
        &conn,
        root,
        sender,
        hotkey_map,
        require_eve_focus,
        allowed_windows,
    )
}

fn listen_for_hotkeys(
    conn: &RustConnection,
    root: Window,
    sender: Sender<TimestampedCommand>,
    hotkey_map: HashMap<(Keycode, ModMask), CycleCommand>,
    require_eve_focus: bool,
    allowed_windows: AllowedWindows,
) -> Result<()> {
    // Track whether hotkeys are currently grabbed
    let mut hotkeys_grabbed = true;
    let mut last_focused_window: Option<Window> = None;

    loop {
        if sender.is_closed() {
            return Ok(());
        }
        // Bound event processing so continuous input cannot starve Manager focus checks.
        let focus_deadline = Instant::now() + Duration::from_millis(250);
        while let Some(event) = next_event_until(conn, focus_deadline)? {
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
                    // Check if we need EVE focus OR Custom Source focus
                    if require_eve_focus {
                        let focus_cookie = conn.get_input_focus()?;
                        match focus_cookie.reply() {
                            Ok(focus_reply) => {
                                let mut current = focus_reply.focus;
                                let mut is_allowed = false;

                                // Release the allowed-window lock before querying X11 ancestors.
                                let allowed_set = {
                                    if let Ok(guard) = allowed_windows.read() {
                                        guard.clone()
                                    } else {
                                        // Lock poisoned?
                                        std::collections::HashSet::new()
                                    }
                                };

                                // Walk up the tree up to 5 levels to find if any ancestor is allowed
                                // (e.g. FocusProxy -> ... -> RuneLite -> Root)
                                // 5 levels is arbitrary but should cover most cases (Proxy -> Window -> Frame -> WM -> Root)
                                for _ in 0..5 {
                                    if allowed_set.contains(&current) {
                                        is_allowed = true;
                                        break;
                                    }

                                    // Stop if we hit root or invalid
                                    if current == root || current == 0 {
                                        break;
                                    }

                                    // Get parent
                                    if let Ok(tree_cookie) = conn.query_tree(current) {
                                        if let Ok(tree_reply) = tree_cookie.reply() {
                                            current = tree_reply.parent;
                                        } else {
                                            break;
                                        }
                                    } else {
                                        break;
                                    }
                                }

                                if !is_allowed {
                                    // Try to get window class/title for debugging
                                    let window_class =
                                        get_window_class_sync(conn, focus_reply.focus)
                                            .unwrap_or_else(|_| "Unknown".to_string());
                                    debug!(
                                        window = focus_reply.focus,
                                        class = %window_class,
                                        "Focus required but window (and ancestors) not in allowed set, replaying"
                                    );
                                    conn.allow_events(Allow::REPLAY_KEYBOARD, key_event.time)?;
                                    conn.flush()?;
                                    continue;
                                }
                            }
                            Err(e) => {
                                error!(error = %e, "Failed to get input focus during hotkey check");
                                // If we fail to check focus, safe default is to Replay to avoid eating keys
                                conn.allow_events(Allow::REPLAY_KEYBOARD, key_event.time)?;
                                conn.flush()?;
                                continue;
                            }
                        }
                    }

                    // If we got here, we consume the event.
                    debug!(keycode = key_event.detail, "Consuming hotkey event");
                    conn.allow_events(Allow::ASYNC_KEYBOARD, key_event.time)?;
                    conn.flush()?;

                    // Normalize modifiers (remove NumLock, CapsLock, etc.)
                    let modmask = normalize_modmask(key_event.state);

                    // Look up the hotkey
                    if let Some(command) = hotkey_map.get(&(key_event.detail, modmask)) {
                        debug!(
                            keycode = key_event.detail,
                            modmask = ?modmask,
                            command = ?command,
                            "Hotkey pressed, sending command"
                        );

                        let timestamped_command = TimestampedCommand {
                            command: command.clone(),
                            timestamp: key_event.time,
                        };

                        // Input is already released. Never block handling the next grab
                        // while the daemon catches up; retain commands already queued.
                        match sender.try_send(timestamped_command) {
                            Ok(()) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                warn!("Hotkey command queue full; dropping new command");
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                return Ok(());
                            }
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

        // Recheck Manager focus after each bounded event-processing interval.
        let focus_cookie = conn.get_input_focus()?;
        let focused_window = focus_cookie.reply()?.focus;

        // Only check class if focus changed (optimization)
        if last_focused_window != Some(focused_window) {
            last_focused_window = Some(focused_window);
            let focused_class = get_window_class_sync(conn, focused_window).unwrap_or_default();
            let is_epm_focused = focused_class.eq_ignore_ascii_case("eve-preview-manager");

            // If Manager gained focus, ungrab hotkeys
            if is_epm_focused && hotkeys_grabbed {
                debug!("Manager gained focus, ungrabbing hotkeys to allow normal input");
                for (keycode, modmask) in hotkey_map.keys() {
                    ungrab_hotkey(conn, root, *keycode, *modmask)?;
                }
                hotkeys_grabbed = false;
                conn.flush()?;
            }
            // If Manager lost focus, regrab hotkeys
            else if !is_epm_focused && !hotkeys_grabbed {
                debug!("Manager lost focus, re-grabbing hotkeys");
                for (keycode, modmask) in hotkey_map.keys() {
                    register_hotkey(conn, root, *keycode, *modmask)?;
                }
                hotkeys_grabbed = true;
                conn.flush()?;
            }
        }
    }
}

/// Drain x11rb's queue before waiting on its socket, up to the next focus check.
#[allow(unsafe_code)] // Required for libc::poll().
fn next_event_until(conn: &RustConnection, deadline: Instant) -> Result<Option<Event>> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        if let Some(event) = conn.poll_for_event()? {
            return Ok(Some(event));
        }
        let mut fds = [libc::pollfd {
            fd: conn.stream().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        let timeout_ms = (deadline - now).as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: fds contains one valid pollfd, matching the supplied count.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout_ms) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("Failed to wait for X11 hotkey input");
        }
        if fds[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            anyhow::bail!("X11 hotkey connection closed while waiting for input");
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

/// Register a global hotkey with X11
fn register_hotkey(
    conn: &RustConnection,
    root: Window,
    keycode: Keycode,
    modmask: ModMask,
) -> Result<()> {
    // Grab each combination of the ignored NumLock (Mod2) and CapsLock modifiers.
    // X11 treats "Ctrl+C" and "Ctrl+C+NumLock" as completely different hotkeys.
    // These permutations cover both NumLock and CapsLock states.
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
            GrabMode::ASYNC, // pointer_mode: keep pointer processing normal
            GrabMode::SYNC,  // keyboard_mode: allow the ReplayKeyboard decision
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
    //! Run display regressions from `nix develop` on an isolated Xvfb server:
    //!
    //! ```sh
    //! cargo test --locked --all-features --no-run
    //! xvfb-run -a -s "-screen 0 1280x800x24 -nolisten tcp -noreset" \
    //!   env EPM_X11_TESTS=1 timeout 60s \
    //!   cargo test --locked --all-features input::x11_backend::tests -- --ignored --test-threads=1
    //! ```

    use super::*;
    use std::time::{Duration, Instant};
    use x11rb::protocol::xtest::ConnectionExt as _;

    const HOTKEY: Keycode = 67; // F1 on Xvfb's evdev mapping
    const OTHER_KEY: Keycode = 68;

    struct GrabTest {
        grabber: RustConnection,
        recipient: RustConnection,
        root: Window,
    }

    impl GrabTest {
        fn new() -> Self {
            assert_eq!(
                std::env::var("EPM_X11_TESTS").as_deref(),
                Ok("1"),
                "run display tests with EPM_X11_TESTS=1 under an isolated Xvfb server"
            );
            let (grabber, screen) = x11rb::connect(None).unwrap();
            let root = grabber.setup().roots[screen].root;
            let (recipient, _) = x11rb::connect(None).unwrap();
            let test = Self {
                grabber,
                recipient,
                root,
            };
            let window = test.recipient.generate_id().unwrap();
            test.recipient
                .create_window(
                    x11rb::COPY_DEPTH_FROM_PARENT,
                    window,
                    root,
                    0,
                    0,
                    200,
                    200,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new().event_mask(
                        EventMask::KEY_PRESS
                            | EventMask::KEY_RELEASE
                            | EventMask::POINTER_MOTION
                            | EventMask::BUTTON_PRESS
                            | EventMask::BUTTON_RELEASE,
                    ),
                )
                .unwrap()
                .check()
                .unwrap();
            test.recipient.map_window(window).unwrap().check().unwrap();
            test.recipient
                .set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
                .unwrap()
                .check()
                .unwrap();
            register_hotkey(&test.grabber, root, HOTKEY, ModMask::from(0u16)).unwrap();
            // Round trip ensures registration finishes before input from another connection.
            test.grabber.get_input_focus().unwrap().reply().unwrap();
            test
        }

        fn input(&self, event_type: u8, detail: u8, x: i16, y: i16) {
            self.recipient
                .xtest_fake_input(event_type, detail, x11rb::CURRENT_TIME, self.root, x, y, 0)
                .unwrap()
                .check()
                .unwrap();
        }

        fn press_hotkey(&self) -> Timestamp {
            self.input(KEY_PRESS_EVENT, HOTKEY, 0, 0);
            let event = next_input(&self.grabber);
            match event {
                Event::KeyPress(event) if event.detail == HOTKEY => event.time,
                event => panic!("expected grabbed hotkey press, got {event:?}"),
            }
        }

        fn allow(&self, mode: Allow, timestamp: Timestamp) {
            self.grabber.allow_events(mode, timestamp).unwrap();
            self.grabber.flush().unwrap();
            self.grabber.get_input_focus().unwrap().reply().unwrap();
        }

        fn assert_pointer_works(&self, coordinate: i16) {
            self.input(MOTION_NOTIFY_EVENT, 0, coordinate, coordinate);
            assert!(matches!(
                next_input(&self.recipient),
                Event::MotionNotify(event)
                    if event.root_x == coordinate && event.root_y == coordinate
            ));
            self.input(BUTTON_PRESS_EVENT, 1, 0, 0);
            assert!(matches!(
                next_input(&self.recipient),
                Event::ButtonPress(event) if event.detail == 1
            ));
            self.input(BUTTON_RELEASE_EVENT, 1, 0, 0);
            assert!(matches!(
                next_input(&self.recipient),
                Event::ButtonRelease(event) if event.detail == 1
            ));
        }
    }

    impl Drop for GrabTest {
        fn drop(&mut self) {
            // Release the grab and injected input even when an assertion panics.
            // Connection teardown also destroys the window and passive grabs.
            let _ = self.grabber.ungrab_keyboard(x11rb::CURRENT_TIME);
            let _ = self.grabber.flush();
            if let Ok(cookie) = self.grabber.get_input_focus() {
                let _ = cookie.reply();
            }
            for (event_type, detail) in [
                (KEY_RELEASE_EVENT, HOTKEY),
                (KEY_RELEASE_EVENT, OTHER_KEY),
                (BUTTON_RELEASE_EVENT, 1),
            ] {
                let _ = self.recipient.xtest_fake_input(
                    event_type,
                    detail,
                    x11rb::CURRENT_TIME,
                    self.root,
                    0,
                    0,
                    0,
                );
            }
            let _ = self.recipient.flush();
            if let Ok(cookie) = self.recipient.get_input_focus() {
                let _ = cookie.reply();
            }
        }
    }

    fn next_input(conn: &RustConnection) -> Event {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(event) = conn.poll_for_event().unwrap() {
                match event {
                    Event::KeyPress(_)
                    | Event::KeyRelease(_)
                    | Event::MotionNotify(_)
                    | Event::ButtonPress(_)
                    | Event::ButtonRelease(_) => return event,
                    Event::Error(error) => panic!("X11 error: {error:?}"),
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline, "timed out waiting for X11 input");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn listener_processes_buffered_hotkeys_and_exits_when_receiver_closes() {
        for allowed in [false, true] {
            let test = GrabTest::new();
            while test.grabber.poll_for_event().unwrap().is_some() {}
            test.input(KEY_PRESS_EVENT, HOTKEY, 0, 0);
            // The reply reads the preceding KeyPress into x11rb's queue, emptying the socket.
            let focus = test
                .grabber
                .get_input_focus()
                .unwrap()
                .reply()
                .unwrap()
                .focus;
            let allowed_windows = AllowedWindows::default();
            if allowed {
                allowed_windows.write().unwrap().insert(focus);
            }
            thread::scope(|scope| {
                // Drop the receiver before joining, including if an input assertion panics.
                let (tx, mut rx) = tokio::sync::mpsc::channel(1);
                let listener = scope.spawn(|| {
                    listen_for_hotkeys(
                        &test.grabber,
                        test.root,
                        tx,
                        HashMap::from([((HOTKEY, ModMask::from(0u16)), CycleCommand::ToggleSkip)]),
                        true,
                        allowed_windows,
                    )
                });
                if allowed {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    let command = loop {
                        if let Ok(command) = rx.try_recv() {
                            break command;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "buffered hotkey was not dispatched"
                        );
                        thread::sleep(Duration::from_millis(1));
                    };
                    assert_eq!(command.command, CycleCommand::ToggleSkip);
                    assert_ne!(command.timestamp, x11rb::CURRENT_TIME);
                } else {
                    assert!(
                        matches!(next_input(&test.recipient), Event::KeyPress(event) if event.detail == HOTKEY)
                    );
                    assert!(rx.try_recv().is_err());
                }
                test.input(KEY_RELEASE_EVENT, HOTKEY, 0, 0);
                if !allowed {
                    assert!(
                        matches!(next_input(&test.recipient), Event::KeyRelease(event) if event.detail == HOTKEY)
                    );
                }
                test.input(KEY_PRESS_EVENT, OTHER_KEY, 0, 0);
                assert!(
                    matches!(next_input(&test.recipient), Event::KeyPress(event) if event.detail == OTHER_KEY)
                );
                test.input(KEY_RELEASE_EVENT, OTHER_KEY, 0, 0);
                drop(rx);
                listener.join().unwrap().unwrap();
            });
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn listener_full_command_queue_does_not_freeze_later_grabs() {
        let test = GrabTest::new();
        thread::scope(|scope| {
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            tx.try_send(TimestampedCommand {
                command: CycleCommand::TogglePreviews,
                timestamp: 123,
            })
            .unwrap();
            let listener = scope.spawn(|| {
                listen_for_hotkeys(
                    &test.grabber,
                    test.root,
                    tx,
                    HashMap::from([((HOTKEY, ModMask::from(0u16)), CycleCommand::ToggleSkip)]),
                    false,
                    AllowedWindows::default(),
                )
            });
            // The first dispatch encounters a full queue. The second grab must still be released.
            for _ in 0..2 {
                test.input(KEY_PRESS_EVENT, HOTKEY, 0, 0);
                test.input(KEY_RELEASE_EVENT, HOTKEY, 0, 0);
            }
            test.input(KEY_PRESS_EVENT, OTHER_KEY, 0, 0);
            assert!(
                matches!(next_input(&test.recipient), Event::KeyPress(event) if event.detail == OTHER_KEY)
            );
            test.input(KEY_RELEASE_EVENT, OTHER_KEY, 0, 0);
            let queued = rx.try_recv().unwrap();
            assert_eq!(queued.command, CycleCommand::TogglePreviews);
            assert_eq!(queued.timestamp, 123);
            assert!(
                rx.try_recv().is_err(),
                "new commands must not displace queued work"
            );
            drop(rx);
            listener.join().unwrap().unwrap();
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn rejected_hotkey_is_replayed_to_focused_window() {
        let test = GrabTest::new();
        let timestamp = test.press_hotkey();
        test.allow(Allow::REPLAY_KEYBOARD, timestamp);
        assert!(matches!(
            next_input(&test.recipient),
            Event::KeyPress(event) if event.detail == HOTKEY && event.time == timestamp
        ));
        test.input(KEY_RELEASE_EVENT, HOTKEY, 0, 0);
        assert!(matches!(
            next_input(&test.recipient),
            Event::KeyRelease(event) if event.detail == HOTKEY
        ));
    }

    #[test]
    #[ignore = "requires isolated Xvfb; see test module for command"]
    fn held_hotkey_keeps_pointer_responsive_and_resumes_keyboard() {
        let test = GrabTest::new();
        let timestamp = test.press_hotkey();
        test.assert_pointer_works(30);
        test.allow(Allow::ASYNC_KEYBOARD, timestamp);
        test.assert_pointer_works(60);

        test.input(KEY_RELEASE_EVENT, HOTKEY, 0, 0);
        assert!(matches!(
            next_input(&test.grabber),
            Event::KeyRelease(event) if event.detail == HOTKEY
        ));
        test.input(KEY_PRESS_EVENT, OTHER_KEY, 0, 0);
        // The recipient must see only the new key, never the consumed hotkey.
        assert!(matches!(
            next_input(&test.recipient),
            Event::KeyPress(event) if event.detail == OTHER_KEY
        ));
        test.input(KEY_RELEASE_EVENT, OTHER_KEY, 0, 0);
        assert!(matches!(
            next_input(&test.recipient),
            Event::KeyRelease(event) if event.detail == OTHER_KEY
        ));
    }

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
