//! Key capture functionality for interactive hotkey binding
//! Supports both keyboard keys and mouse buttons

use anyhow::{Context, Result};
use evdev::EventType;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::common::constants::{input, paths, permissions};
use crate::common::types::Position;
use crate::config::{HotkeyBackendType, HotkeyBinding};
use crate::input::device_detection;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ConfigureWindowAux, ConnectionExt, CreateWindowAux, Cursor, EventMask, Font,
    GrabMode, GrabStatus, InputFocus, KeyButMask, PropMode, StackMode, Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as WrapperExt;

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

/// Result of a global screen-position picker operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PositionPickResult {
    Picked(Position),
    Cancelled,
    Timeout,
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

const POSITION_PICK_TIMEOUT: Duration = Duration::from_secs(30);
const POSITION_PICK_POLL_INTERVAL: Duration = Duration::from_millis(10);
const POSITION_PICK_OVERLAY_OPACITY: u32 = 0x6600_0000;
const X11_CURSOR_CROSSHAIR: u16 = 34;
const X11_CURSOR_CROSSHAIR_MASK: u16 = 35;

/// Start picking a root-screen position in the background.
/// Returns a result receiver and a cancellation sender.
pub fn start_position_pick() -> (Receiver<PositionPickResult>, Sender<()>) {
    let (result_tx, result_rx) = mpsc::channel();
    let (cancel_tx, cancel_rx) = mpsc::channel();

    thread::spawn(move || {
        let result = capture_position_x11(cancel_rx).unwrap_or_else(|err| {
            warn!(error = %err, "Position picker error");
            PositionPickResult::Error(err.to_string())
        });

        let _ = result_tx.send(result);
    });

    (result_rx, cancel_tx)
}

fn capture_position_x11(cancel_rx: Receiver<()>) -> Result<PositionPickResult> {
    capture_position_overlay(&cancel_rx).or_else(|overlay_error| {
        warn!(
            error = %overlay_error,
            "Position picker overlay failed; falling back to root pointer grab"
        );
        capture_position_root_grab(&cancel_rx)
    })
}

fn capture_position_overlay(cancel_rx: &Receiver<()>) -> Result<PositionPickResult> {
    let (conn, screen_num) = x11rb::connect(None).context("Failed to connect to X11")?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    let cursor = create_crosshair_cursor(&conn).unwrap_or(0);
    let overlay = create_position_pick_overlay(&conn, screen, cursor)?;
    let keyboard_grabbed = conn
        .grab_keyboard(
            false,
            overlay,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|reply| reply.status == GrabStatus::SUCCESS);

    let _ = conn.set_input_focus(InputFocus::POINTER_ROOT, overlay, x11rb::CURRENT_TIME);
    conn.configure_window(
        overlay,
        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
    )
    .context("Failed to raise position picker overlay")?;
    conn.flush().context("Failed to flush X11 picker overlay")?;
    info!(
        keyboard_grabbed,
        width = screen.width_in_pixels,
        height = screen.height_in_pixels,
        "Position picker overlay mapped"
    );

    let start = std::time::Instant::now();
    let result = loop {
        if start.elapsed() > POSITION_PICK_TIMEOUT {
            break PositionPickResult::Timeout;
        }

        match cancel_rx.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
                break PositionPickResult::Cancelled;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        if let Some(event) = conn.poll_for_event()? {
            match event {
                Event::ButtonPress(event) => match event.detail {
                    1 => {
                        break PositionPickResult::Picked(Position::new(
                            event.root_x,
                            event.root_y,
                        ));
                    }
                    3 => break PositionPickResult::Cancelled,
                    _ => {}
                },
                Event::KeyPress(event) => {
                    let evdev_code = (event.detail as u16).saturating_sub(8);
                    if evdev_code == 1 {
                        break PositionPickResult::Cancelled;
                    }
                }
                _ => {}
            }
        }

        thread::sleep(POSITION_PICK_POLL_INTERVAL);
    };

    if keyboard_grabbed {
        let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    }
    let _ = conn.destroy_window(overlay);
    let _ = conn.flush();

    debug!(root, overlay, "Position picker overlay closed");
    Ok(result)
}

fn create_position_pick_overlay<C: Connection>(
    conn: &C,
    screen: &x11rb::protocol::xproto::Screen,
    cursor: Cursor,
) -> Result<Window> {
    let window = conn
        .generate_id()
        .context("Failed to generate position picker overlay ID")?;

    let mut aux = CreateWindowAux::new()
        .background_pixel(screen.black_pixel)
        .override_redirect(crate::common::constants::x11::OVERRIDE_REDIRECT)
        .event_mask(
            EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::KEY_PRESS
                | EventMask::EXPOSURE
                | EventMask::STRUCTURE_NOTIFY,
        );

    if cursor != x11rb::NONE {
        aux = aux.cursor(cursor);
    }

    conn.create_window(
        screen.root_depth,
        window,
        screen.root,
        0,
        0,
        screen.width_in_pixels,
        screen.height_in_pixels,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &aux,
    )
    .context("Failed to create position picker overlay")?;

    set_position_pick_overlay_properties(conn, window)?;
    conn.map_window(window)
        .context("Failed to map position picker overlay")?;

    Ok(window)
}

fn set_position_pick_overlay_properties<C: Connection>(conn: &C, window: Window) -> Result<()> {
    let net_wm_name = intern_atom(conn, b"_NET_WM_NAME")?;
    let utf8_string = intern_atom(conn, b"UTF8_STRING")?;
    let wm_class = intern_atom(conn, b"WM_CLASS")?;
    let net_wm_pid = intern_atom(conn, b"_NET_WM_PID")?;
    let net_wm_window_opacity = intern_atom(conn, b"_NET_WM_WINDOW_OPACITY")?;
    let net_wm_state = intern_atom(conn, b"_NET_WM_STATE")?;
    let net_wm_state_above = intern_atom(conn, b"_NET_WM_STATE_ABOVE")?;
    let net_wm_state_fullscreen = intern_atom(conn, b"_NET_WM_STATE_FULLSCREEN")?;
    let net_wm_window_type = intern_atom(conn, b"_NET_WM_WINDOW_TYPE")?;
    let net_wm_window_type_utility = intern_atom(conn, b"_NET_WM_WINDOW_TYPE_UTILITY")?;

    conn.change_property8(
        PropMode::REPLACE,
        window,
        net_wm_name,
        utf8_string,
        b"EPM Position Picker",
    )
    .context("Failed to set position picker title")?;
    conn.change_property8(
        PropMode::REPLACE,
        window,
        wm_class,
        AtomEnum::STRING,
        b"eve-preview-position-picker\0eve-preview-position-picker\0",
    )
    .context("Failed to set position picker WM_CLASS")?;
    conn.change_property32(
        PropMode::REPLACE,
        window,
        net_wm_pid,
        AtomEnum::CARDINAL,
        &[std::process::id()],
    )
    .context("Failed to set position picker PID")?;
    conn.change_property32(
        PropMode::REPLACE,
        window,
        net_wm_window_opacity,
        AtomEnum::CARDINAL,
        &[POSITION_PICK_OVERLAY_OPACITY],
    )
    .context("Failed to set position picker opacity")?;
    conn.change_property32(
        PropMode::REPLACE,
        window,
        net_wm_state,
        AtomEnum::ATOM,
        &[net_wm_state_above, net_wm_state_fullscreen],
    )
    .context("Failed to set position picker window state")?;
    conn.change_property32(
        PropMode::REPLACE,
        window,
        net_wm_window_type,
        AtomEnum::ATOM,
        &[net_wm_window_type_utility],
    )
    .context("Failed to set position picker window type")?;

    Ok(())
}

fn intern_atom<C: Connection>(conn: &C, name: &[u8]) -> Result<u32> {
    Ok(conn
        .intern_atom(false, name)
        .context("Failed to intern X11 atom")?
        .reply()
        .context("Failed to get X11 atom reply")?
        .atom)
}

fn capture_position_root_grab(cancel_rx: &Receiver<()>) -> Result<PositionPickResult> {
    let (conn, screen_num) = x11rb::connect(None).context("Failed to connect to X11")?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    let cursor = create_crosshair_cursor(&conn).unwrap_or(0);
    let pointer_reply = conn
        .grab_pointer(
            false,
            root,
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            x11rb::NONE,
            cursor,
            x11rb::CURRENT_TIME,
        )
        .context("Failed to grab pointer")?
        .reply()
        .context("Failed to get grab_pointer reply")?;

    if pointer_reply.status != GrabStatus::SUCCESS {
        return Err(anyhow::anyhow!(
            "GrabPointer failed with status: {:?}",
            pointer_reply.status
        ));
    }

    let keyboard_grabbed = conn
        .grab_keyboard(
            false,
            root,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some_and(|reply| reply.status == GrabStatus::SUCCESS);

    conn.flush().context("Failed to flush X11 picker grabs")?;
    info!(
        keyboard_grabbed,
        "Pointer grabbed for preview position picker"
    );

    let start = std::time::Instant::now();
    let result = loop {
        if start.elapsed() > POSITION_PICK_TIMEOUT {
            break PositionPickResult::Timeout;
        }

        match cancel_rx.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
                break PositionPickResult::Cancelled;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        if let Some(event) = conn.poll_for_event()? {
            match event {
                Event::ButtonPress(event) => match event.detail {
                    1 => {
                        break PositionPickResult::Picked(Position::new(
                            event.root_x,
                            event.root_y,
                        ));
                    }
                    3 => break PositionPickResult::Cancelled,
                    _ => {}
                },
                Event::KeyPress(event) => {
                    let evdev_code = (event.detail as u16).saturating_sub(8);
                    if evdev_code == 1 {
                        break PositionPickResult::Cancelled;
                    }
                }
                _ => {}
            }
        }

        thread::sleep(POSITION_PICK_POLL_INTERVAL);
    };

    let _ = conn.ungrab_pointer(x11rb::CURRENT_TIME);
    if keyboard_grabbed {
        let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    }
    let _ = conn.flush();

    Ok(result)
}

fn create_crosshair_cursor<C: Connection>(conn: &C) -> Option<Cursor> {
    let font: Font = conn.generate_id().ok()?;
    conn.open_font(font, b"cursor").ok()?;

    let cursor: Cursor = conn.generate_id().ok()?;
    conn.create_glyph_cursor(
        cursor,
        font,
        font,
        X11_CURSOR_CROSSHAIR,
        X11_CURSOR_CROSSHAIR_MASK,
        u16::MAX,
        u16::MAX,
        u16::MAX,
        0,
        0,
        0,
    )
    .ok()?;

    Some(cursor)
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
