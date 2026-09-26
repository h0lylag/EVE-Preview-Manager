//! evdev hotkey backend
//!
//! Monitors input devices directly via /dev/input for low-latency hotkey detection.
//! Supports both keyboard keys and mouse buttons (including Mouse 4/5 side buttons).
//! Requires 'input' group membership to access raw input devices.
//!
//! This backend provides advanced features like:
//! - Cross-device modifier detection (Shift on keyboard + Mouse4 on mouse)
//! - Device-specific filtering
//! - Guaranteed global capture
//!
//! Security warning: Requires 'input' group membership, which allows ALL applications
//! to read keyboard and mouse input. Use only if you need the advanced features.

use anyhow::{Context, Result};
use evdev::{Device, EventType, KeyCode};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::{Event, xproto::*};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::common::constants::{input, paths, permissions};
use crate::input::backend::{
    AllowedWindows, BackendCapabilities, HotkeyBackend, HotkeyConfiguration,
};
use crate::input::device_detection;
use crate::input::listener::{CycleCommand, TimestampedCommand};

pub struct EvdevBackend;

/// Samples server time without consuming the daemon's X11 events.
/// Each device listener owns one connection and samples serially. Its unmapped
/// InputOnly window is not drawable and is destroyed on connection close under
/// X11's default DestroyAll close-down mode.
/// See X11 CreateWindow and Connection Close:
/// <https://www.x.org/releases/X11R7.7/doc/xproto/x11protocol.html>.
struct XTimestampSource {
    conn: RustConnection,
    window: Window,
    atom: Atom,
}

impl XTimestampSource {
    fn new() -> Result<Self> {
        let (conn, screen_number) =
            x11rb::connect(None).context("Failed to connect to X11 for evdev timestamps")?;
        let root = conn.setup().roots[screen_number].root;
        let window = conn.generate_id()?;
        conn.create_window(
            0,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?
        .check()
        .context("Failed to create evdev timestamp window")?;
        let atom = conn
            .intern_atom(false, b"_EPM_INPUT_TIMESTAMP")?
            .reply()
            .context("Failed to intern evdev timestamp atom")?
            .atom;
        Ok(Self { conn, window, atom })
    }

    fn timestamp(&self) -> Result<u32> {
        // ICCCM 2.1 documents this timestamp acquisition technique:
        // https://www.x.org/releases/X11R7.7/doc/xorg-docs/icccm/icccm.html
        // X11 PropertyNotify guarantees NewValue even for a zero-length append.
        // Checking the request also flushes it and prevents waiting for a notification
        // that will never arrive after a protocol error (e.g. a destroyed window).
        // Verified in pinned x11rb 0.14, src/rust_connection/mod.rs:
        // RustConnection::check_for_raw_error flushes and queues unrelated events.
        self.conn
            .change_property8(
                PropMode::APPEND,
                self.window,
                self.atom,
                AtomEnum::STRING,
                &[],
            )?
            .check()
            .context("Failed to request evdev X11 timestamp")?;
        loop {
            if let Event::PropertyNotify(event) = self
                .conn
                .wait_for_event()
                .context("Failed to read evdev X11 timestamp")?
                && event.window == self.window
                && event.atom == self.atom
                && event.state == Property::NEW_VALUE
            {
                return Ok(event.time);
            }
        }
    }
}

impl HotkeyBackend for EvdevBackend {
    fn spawn(
        sender: Sender<TimestampedCommand>,
        config: HotkeyConfiguration,
        selected_device_id: Option<String>,
        require_eve_focus: bool,
        _allowed_windows: AllowedWindows,
    ) -> Result<Vec<JoinHandle<()>>> {
        spawn_listener_impl(sender, config, selected_device_id, require_eve_focus)
    }

    fn is_available() -> bool {
        check_permissions()
    }

    fn name() -> &'static str {
        "evdev"
    }

    fn capabilities() -> BackendCapabilities {
        BackendCapabilities {
            supports_cross_device_modifiers: true,
            supports_device_filtering: true,
            requires_permissions: true,
            permission_description: Some(format!(
                "Requires '{}' group membership. Run: {}",
                permissions::INPUT_GROUP,
                permissions::ADD_TO_INPUT_GROUP
            )),
        }
    }
}

/// Initializes and manages background threads for low-latency input event monitoring across multiple devices
fn spawn_listener_impl(
    sender: tokio::sync::mpsc::Sender<crate::input::listener::TimestampedCommand>,
    config: HotkeyConfiguration,
    selected_device_id: Option<String>,
    _require_eve_focus: bool, // Not currently implemented for evdev backend
) -> Result<Vec<thread::JoinHandle<()>>> {
    // We need to detect all devices upfront to support "cross-device" modifiers.
    // For example, a user might hold 'Shift' on their keyboard while pressing a 'Mouse Button'
    // on their mouse. To support this, every listener thread needs access to the current state
    // of ALL other input devices.
    let devices = device_detection::find_all_input_devices_with_paths()?;

    // Create shared list of paths. This Arc<Vec> will be shared with every thread
    // so they can query the global state of the system's input devices at any time.
    let all_device_paths: Vec<_> = devices.iter().map(|(_dev, path)| path.clone()).collect();

    let mut devices = devices;

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
            // Use devices associated with the configured hotkey bindings
            info!("Auto-detect mode: using devices from hotkey bindings");

            let mut required_devices = std::collections::HashSet::new();
            for (_, binding) in &config.cycle_hotkeys {
                required_devices.extend(binding.source_devices.iter().cloned());
            }
            for binding in &config.character_hotkeys {
                required_devices.extend(binding.source_devices.iter().cloned());
            }
            for binding in &config.profile_hotkeys {
                required_devices.extend(binding.source_devices.iter().cloned());
            }
            if let Some(ref skip) = config.toggle_skip_key {
                required_devices.extend(skip.source_devices.iter().cloned());
            }
            if let Some(ref toggle_previews) = config.toggle_previews_key {
                required_devices.extend(toggle_previews.source_devices.iter().cloned());
            }

            if required_devices.is_empty() {
                warn!(
                    "Auto-detect mode but no source devices found in bindings, listening on all devices"
                );
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
            // Legacy: specific device ID (compatibility for old configs)
            info!(device_id = %device_id, "Filtering to specific input device (legacy)");

            let by_id_path = format!("/dev/input/by-id/{}", device_id);
            let target_path = std::fs::read_link(&by_id_path)
                .with_context(|| format!("Failed to resolve device {}", by_id_path))?;

            let absolute_target = if target_path.is_absolute() {
                target_path
            } else {
                std::path::Path::new("/dev/input/by-id")
                    .join(&target_path)
                    .canonicalize()
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

    let cycle_configured = !config.cycle_hotkeys.is_empty();
    let has_character_hotkeys = !config.character_hotkeys.is_empty();
    let has_profile_hotkeys = !config.profile_hotkeys.is_empty();
    let has_skip_key = config.toggle_skip_key.is_some();
    let has_toggle_previews_key = config.toggle_previews_key.is_some();

    if cycle_configured
        || has_character_hotkeys
        || has_profile_hotkeys
        || has_skip_key
        || has_toggle_previews_key
    {
        info!(
            cycle_hotkey_count = config.cycle_hotkeys.len(),
            character_hotkey_count = config.character_hotkeys.len(),
            profile_hotkey_count = config.profile_hotkeys.len(),
            has_skip_key = has_skip_key,
            has_toggle_previews_key = has_toggle_previews_key,
            device_count = devices.len(),
            "Starting hotkey listeners"
        );
    } else {
        warn!("No hotkeys configured - hotkey listener will not be started");
        return Ok(Vec::new());
    }

    for (device, device_path) in devices {
        let sender = sender.clone();
        let config = config.clone();
        let all_device_paths = Arc::clone(&all_device_paths);

        let handle = thread::spawn(move || {
            info!(device = ?device.name(), path = %device_path.display(), "Hotkey listener started");
            if let Err(e) = listen_for_hotkeys(device, sender, config, all_device_paths) {
                error!(error = %e, "Hotkey listener error");
            }
        });
        handles.push(handle);
    }

    Ok(handles)
}

/// Event loop processing raw input events from a single device, handling key presses and state tracking
fn listen_for_hotkeys(
    mut device: Device,
    sender: Sender<TimestampedCommand>,
    config: HotkeyConfiguration,
    all_device_paths: Arc<Vec<std::path::PathBuf>>,
) -> Result<()> {
    let timestamp_source = XTimestampSource::new()?;
    loop {
        let events = device.fetch_events().context("Failed to fetch events")?;

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
                let is_cycle_key = config
                    .cycle_hotkeys
                    .iter()
                    .any(|(_, hk)| hk.key_code == key_code);
                let is_character_key = config
                    .character_hotkeys
                    .iter()
                    .any(|hk| hk.key_code == key_code);
                let is_profile_key = config
                    .profile_hotkeys
                    .iter()
                    .any(|hk| hk.key_code == key_code);
                let is_skip_key = config
                    .toggle_skip_key
                    .as_ref()
                    .is_some_and(|k| k.key_code == key_code);
                let is_toggle_previews_key = config
                    .toggle_previews_key
                    .as_ref()
                    .is_some_and(|k| k.key_code == key_code);

                if is_cycle_key
                    || is_character_key
                    || is_profile_key
                    || is_skip_key
                    || is_toggle_previews_key
                {
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
                    && let Ok(key_state) = dev.get_key_state()
                {
                    ctrl_pressed |=
                        key_state.contains(KeyCode(29)) || key_state.contains(KeyCode(97));
                    shift_pressed |= key_state.contains(KeyCode(input::KEY_LEFTSHIFT))
                        || key_state.contains(KeyCode(input::KEY_RIGHTSHIFT));
                    alt_pressed |=
                        key_state.contains(KeyCode(56)) || key_state.contains(KeyCode(100));
                    super_pressed |=
                        key_state.contains(KeyCode(125)) || key_state.contains(KeyCode(126));
                }
            }

            // Check cycle hotkeys first
            let mut handled = false;
            let mut command_to_send = None;

            for (cmd, binding) in &config.cycle_hotkeys {
                if binding.matches(
                    key_code,
                    ctrl_pressed,
                    shift_pressed,
                    alt_pressed,
                    super_pressed,
                ) {
                    info!(
                        binding = %binding.display_name(),
                        command = ?cmd,
                        "Cycle hotkey pressed, sending command"
                    );
                    command_to_send = Some(cmd.clone());
                    handled = true;
                    break;
                }
            }

            if !handled
                && let Some(ref skip_key) = config.toggle_skip_key
                && skip_key.matches(
                    key_code,
                    ctrl_pressed,
                    shift_pressed,
                    alt_pressed,
                    super_pressed,
                )
            {
                info!(
                    binding = %skip_key.display_name(),
                    "Toggle skip hotkey pressed, sending command"
                );
                command_to_send = Some(CycleCommand::ToggleSkip);
                handled = true;
            }

            if !handled
                && let Some(ref toggle_previews_key) = config.toggle_previews_key
                && toggle_previews_key.matches(
                    key_code,
                    ctrl_pressed,
                    shift_pressed,
                    alt_pressed,
                    super_pressed,
                )
            {
                info!(
                    binding = %toggle_previews_key.display_name(),
                    "Toggle previews hotkey pressed, sending command"
                );
                command_to_send = Some(CycleCommand::TogglePreviews);
                handled = true;
            }

            if !handled {
                // Check per-character hotkeys
                for char_hotkey in &config.character_hotkeys {
                    if char_hotkey.matches(
                        key_code,
                        ctrl_pressed,
                        shift_pressed,
                        alt_pressed,
                        super_pressed,
                    ) {
                        info!(
                            binding = %char_hotkey.display_name(),
                            "Per-character hotkey pressed, sending command"
                        );
                        command_to_send = Some(CycleCommand::CharacterHotkey(char_hotkey.clone()));
                        break; // Only send one command per keypress
                    }
                }
            }

            if !handled && command_to_send.is_none() {
                // Check profile hotkeys
                for profile_hotkey in &config.profile_hotkeys {
                    if profile_hotkey.matches(
                        key_code,
                        ctrl_pressed,
                        shift_pressed,
                        alt_pressed,
                        super_pressed,
                    ) {
                        info!(
                            binding = %profile_hotkey.display_name(),
                            "Profile hotkey pressed, sending command"
                        );
                        command_to_send = Some(CycleCommand::ProfileHotkey(profile_hotkey.clone()));
                        break; // Only send one command per keypress
                    }
                }
            }

            if let Some(command) = command_to_send {
                // Raw evdev timestamps are not in the X server's clock domain.
                // Sample when handling the hotkey; this is not the original time
                // of an input event that was delayed before reaching this listener.
                let timestamp = timestamp_source.timestamp()?;
                debug!(command = ?command, timestamp, "Sending hotkey with X server timestamp");
                let timestamped_command = TimestampedCommand { command, timestamp };
                sender
                    .blocking_send(timestamped_command)
                    .context("Failed to send hotkey command")?;
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

    for entry in
        std::fs::read_dir(by_id_path).context(format!("Failed to read {} directory", by_id_path))?
    {
        let entry = entry?;
        let path = entry.path();

        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.contains("-event-")
            && let Ok(target) = std::fs::read_link(&path)
        {
            let absolute_path = if target.is_absolute() {
                target
            } else {
                std::path::Path::new(by_id_path)
                    .join(&target)
                    .canonicalize()?
            };

            if let Ok(device) = Device::open(&absolute_path)
                && let Some(keys) = device.supported_keys()
            {
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

#[cfg(test)]
mod tests {
    //! Run on an isolated display, never the user's desktop:
    //! ```sh
    //! xvfb-run -a -s "-screen 0 1280x800x24 -nolisten tcp -noreset" \
    //!   env EPM_X11_TESTS=1 timeout 60s \
    //!   cargo test --locked --all-features input::evdev_backend::tests -- --ignored --test-threads=1
    //! ```
    use super::*;
    use crate::common::constants::x11;
    use crate::x11::{CachedAtoms, activate_window, refresh_pointer_state};

    fn timestamp_source() -> XTimestampSource {
        assert_eq!(
            std::env::var("EPM_X11_TESTS").as_deref(),
            Ok("1"),
            "run display tests with EPM_X11_TESTS=1 under isolated Xvfb"
        );
        XTimestampSource::new().unwrap()
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn server_timestamps_transfer_focus_and_survive_activation_and_motion() {
        let source = timestamp_source();
        let (observer, screen_number) = x11rb::connect(None).unwrap();
        let screen = &observer.setup().roots[screen_number];
        let atoms = CachedAtoms::new(&observer).unwrap();
        let target = observer.generate_id().unwrap();
        observer
            .create_window(
                0,
                target,
                screen.root,
                0,
                0,
                100,
                100,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new().event_mask(EventMask::POINTER_MOTION),
            )
            .unwrap()
            .check()
            .unwrap();
        observer.map_window(target).unwrap().check().unwrap();
        observer
            .change_window_attributes(
                screen.root,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
            )
            .unwrap()
            .check()
            .unwrap();

        let mut timestamp = 0;
        for _ in 0..3 {
            // Let server time advance so reusing a previous sample fails the
            // last-focus-change check, rather than passing within one millisecond.
            std::thread::sleep(std::time::Duration::from_millis(2));
            observer
                .set_input_focus(InputFocus::PARENT, screen.root, x11rb::CURRENT_TIME)
                .unwrap()
                .check()
                .unwrap();
            timestamp = source.timestamp().unwrap();
            assert_ne!(timestamp, x11rb::CURRENT_TIME);
            assert_eq!(
                observer.get_input_focus().unwrap().reply().unwrap().focus,
                screen.root
            );
            observer
                .set_input_focus(InputFocus::PARENT, target, timestamp)
                .unwrap()
                .check()
                .unwrap();
            assert_eq!(
                observer.get_input_focus().unwrap().reply().unwrap().focus,
                target
            );
        }
        assert_eq!(
            source
                .conn
                .get_window_attributes(source.window)
                .unwrap()
                .reply()
                .unwrap()
                .map_state,
            MapState::UNMAPPED
        );

        activate_window(&source.conn, screen, &atoms, target, timestamp).unwrap();
        refresh_pointer_state(&source.conn, target, timestamp).unwrap();
        // Round trips fence both connections before draining the observer's events.
        source.conn.get_input_focus().unwrap().reply().unwrap();
        observer.get_input_focus().unwrap().reply().unwrap();
        let mut saw_activation = false;
        let mut saw_motion = false;
        while let Some(event) = observer.poll_for_event().unwrap() {
            match event {
                Event::ClientMessage(event) if event.type_ == atoms.net_active_window => {
                    assert_eq!(event.window, target);
                    assert_eq!(event.format, 32);
                    assert_eq!(
                        event.data.as_data32(),
                        [x11::ACTIVE_WINDOW_SOURCE_PAGER, timestamp, 0, 0, 0]
                    );
                    saw_activation = true;
                }
                Event::MotionNotify(event) if event.response_type & 0x80 != 0 => {
                    assert_eq!(event.event, target);
                    assert_eq!(event.time, timestamp);
                    saw_motion = true;
                }
                _ => {}
            }
        }
        assert!(saw_activation, "activation message was not delivered");
        assert!(saw_motion, "synthetic pointer refresh was not delivered");
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn timestamp_filters_unrelated_events_and_cleans_up_its_window() {
        let source = timestamp_source();
        let (observer, _) = x11rb::connect(None).unwrap();
        observer
            .change_window_attributes(
                source.window,
                &ChangeWindowAttributesAux::new()
                    .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY),
            )
            .unwrap()
            .check()
            .unwrap();
        let noise = source
            .conn
            .intern_atom(false, b"_EPM_TEST_TIMESTAMP_NOISE")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        // Queue a wrong-atom NewValue and a correct-atom Deleted event. Neither
        // may supply the timestamp for the next request.
        source
            .conn
            .change_property8(
                PropMode::REPLACE,
                source.window,
                noise,
                AtomEnum::STRING,
                b"noise",
            )
            .unwrap()
            .check()
            .unwrap();
        source.timestamp().unwrap();
        source
            .conn
            .delete_property(source.window, source.atom)
            .unwrap()
            .check()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let timestamp = source.timestamp().unwrap();
        let property = observer
            .get_property(false, source.window, source.atom, AtomEnum::STRING, 0, 1)
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(property.value_len, 0);
        let mut last_timestamp = None;
        let mut saw_noise = false;
        while let Some(event) = observer.poll_for_event().unwrap() {
            if let Event::PropertyNotify(event) = event {
                saw_noise |= event.atom == noise;
                if event.atom == source.atom && event.state == Property::NEW_VALUE {
                    last_timestamp = Some(event.time);
                }
            }
        }
        assert!(
            saw_noise,
            "sampling must not drain another connection's events"
        );
        assert_eq!(last_timestamp, Some(timestamp));
        let helper = source.window;
        drop(source);
        let Event::DestroyNotify(event) = observer.wait_for_event().unwrap() else {
            panic!("closing timestamp connection must destroy its helper window");
        };
        assert_eq!(event.window, helper);
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn destroyed_timestamp_window_returns_error_without_waiting_for_an_event() {
        let source = timestamp_source();
        source
            .conn
            .destroy_window(source.window)
            .unwrap()
            .check()
            .unwrap();
        assert!(source.timestamp().is_err());
    }
}
