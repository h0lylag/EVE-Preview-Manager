//! Preview daemon main loop and initialization

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::os::fd::AsRawFd;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::damage::ConnectionExt as DamageExt;
use x11rb::protocol::xproto::*;

use crate::config::DaemonConfig;
use crate::constants::eve;
use crate::input::listener::{self, CycleCommand};
use crate::x11::{AppContext, CachedAtoms, activate_window, minimize_window};

use super::cycle_state::CycleState;
use super::event_handler::{EventContext, handle_event};
use super::font;
use super::session_state::SessionState;
use super::thumbnail::Thumbnail;
// use super::window_detection::check_and_create_window; // Moved to window_detection
use std::thread::JoinHandle;
use x11rb::rust_connection::RustConnection;

struct HotkeyResources {
    #[allow(dead_code)]
    handle: Option<Vec<JoinHandle<()>>>,
    rx: mpsc::Receiver<CycleCommand>,
    groups: HashMap<crate::config::HotkeyBinding, Vec<String>>,
}

struct DaemonResources<'a> {
    config: DaemonConfig,
    session: SessionState,
    cycle: CycleState,
    eve_clients: HashMap<Window, Thumbnail<'a>>,
}

fn initialize_x11() -> Result<(
    RustConnection,
    usize,
    CachedAtoms,
    crate::x11::CachedFormats,
)> {
    // Establish connection to the X server to query screen dimensions and root window ID
    // We need the screen dimensions early to set smart defaults for thumbnail sizes
    let (conn, screen_num) = x11rb::connect(None)
        .context("Failed to connect to X11 server. Is DISPLAY set correctly?")?;

    let screen = &conn.setup().roots[screen_num];
    info!(
        screen = screen_num,
        width = screen.width_in_pixels,
        height = screen.height_in_pixels,
        "Connected to X11 server"
    );

    // Pre-cache atoms once at startup
    let atoms = CachedAtoms::new(&conn).context("Failed to cache X11 atoms at startup")?;

    conn.damage_query_version(1, 1)
        .context("Failed to query DAMAGE extension version. Is DAMAGE extension available?")?;

    conn.change_window_attributes(
        screen.root,
        &ChangeWindowAttributesAux::new().event_mask(
            EventMask::SUBSTRUCTURE_NOTIFY
                | EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::POINTER_MOTION,
        ),
    )
    .context("Failed to set event mask on root window")?;

    // Pre-cache picture formats
    let formats = crate::x11::CachedFormats::new(&conn, screen)
        .context("Failed to cache picture formats at startup")?;
    info!("Picture formats cached");

    // Note: Font renderer initialization is deferred until after config load
    // as it depends on user-configured font settings.

    Ok((conn, screen_num, atoms, formats))
}

fn load_configuration(
    screen: &Screen,
) -> Result<(
    DaemonConfig,
    crate::config::DisplayConfig,
    SessionState,
    CycleState,
)> {
    // Load config with screen-aware defaults
    let daemon_config =
        DaemonConfig::load_with_screen(screen.width_in_pixels, screen.height_in_pixels);
    let config = daemon_config.build_display_config();
    info!(config = ?config, "Loaded display configuration");

    let session_state = SessionState::new();
    info!(
        count = daemon_config.character_thumbnails.len(),
        "Loaded character positions from config"
    );

    // Initialize cycle state from config
    let cycle_state = CycleState::new(daemon_config.profile.hotkey_cycle_group.clone());

    Ok((daemon_config, config, session_state, cycle_state))
}

fn setup_hotkeys(daemon_config: &DaemonConfig) -> HotkeyResources {
    // Create channel for hotkey thread → main loop
    let (hotkey_tx, hotkey_rx) = mpsc::channel(32);

    // Build character hotkey list from character_hotkeys HashMap, using cycle group order
    let character_hotkeys: Vec<_> = daemon_config
        .profile
        .hotkey_cycle_group
        .iter()
        .filter_map(|char_name| {
            if let Some(binding) = daemon_config.profile.character_hotkeys.get(char_name) {
                debug!(
                    character = %char_name,
                    binding = %binding.display_name(),
                    "Loaded per-character hotkey"
                );
                Some(binding.clone())
            } else {
                None
            }
        })
        .collect();

    // Group characters by hotkey binding to support cycling through multiple characters on the same key
    // This allows users to bind 'F1' to Cycle [Char1, Char2] effectively
    let mut hotkey_groups: HashMap<crate::config::HotkeyBinding, Vec<String>> = HashMap::new();

    for char_name in &daemon_config.profile.hotkey_cycle_group {
        if let Some(binding) = daemon_config.profile.character_hotkeys.get(char_name) {
            hotkey_groups
                .entry(binding.clone())
                .or_default()
                .push(char_name.clone());
        }
    }

    info!(
        unique_hotkeys = hotkey_groups.len(),
        total_characters = daemon_config.profile.hotkey_cycle_group.len(),
        "Built per-character hotkey groups"
    );

    // Debug: log each hotkey group
    for (binding, chars) in &hotkey_groups {
        debug!(
            binding = %binding.display_name(),
            characters = ?chars,
            "Hotkey group"
        );
    }

    // Spawn hotkey listener (start if any hotkeys configured: cycle or per-character)
    let has_cycle_keys = daemon_config.profile.hotkey_cycle_forward.is_some()
        && daemon_config.profile.hotkey_cycle_backward.is_some();
    let has_character_hotkeys = !character_hotkeys.is_empty();

    let hotkey_handle = if has_cycle_keys || has_character_hotkeys {
        // Select backend based on configuration
        use crate::config::HotkeyBackendType;
        use crate::input::backend::HotkeyBackend;
        
        match daemon_config.profile.hotkey_backend {
            HotkeyBackendType::X11 => {
                info!("Using X11 hotkey backend (secure, no permissions required)");
                match crate::input::x11_backend::X11Backend::spawn(
                    hotkey_tx,
                    daemon_config.profile.hotkey_cycle_forward.clone(),
                    daemon_config.profile.hotkey_cycle_backward.clone(),
                    character_hotkeys.clone(),
                    daemon_config.profile.hotkey_input_device.clone(),
                ) {
                    Ok(handle) => {
                        info!(
                            enabled = true,
                            backend = "x11",
                            has_cycle_keys = has_cycle_keys,
                            has_character_hotkeys = has_character_hotkeys,
                            "Hotkey support enabled"
                        );
                        Some(handle)
                    }
                    Err(e) => {
                        error!(error = %e, backend = "x11", "Failed to start hotkey listener");
                        None
                    }
                }
            }
            HotkeyBackendType::Evdev => {
                info!("Using evdev hotkey backend (requires input group membership)");
                if !crate::input::evdev_backend::EvdevBackend::is_available() {
                    listener::print_permission_error();
                    None
                } else {
                    match crate::input::evdev_backend::EvdevBackend::spawn(
                        hotkey_tx,
                        daemon_config.profile.hotkey_cycle_forward.clone(),
                        daemon_config.profile.hotkey_cycle_backward.clone(),
                        character_hotkeys.clone(),
                        daemon_config.profile.hotkey_input_device.clone(),
                    ) {
                        Ok(handle) => {
                            info!(
                                enabled = true,
                                backend = "evdev",
                                has_cycle_keys = has_cycle_keys,
                                has_character_hotkeys = has_character_hotkeys,
                                "Hotkey support enabled"
                            );
                            Some(handle)
                        }
                        Err(e) => {
                            error!(error = %e, backend = "evdev", "Failed to start hotkey listener");
                            listener::print_permission_error();
                            None
                        }
                    }
                }
            }
        }
    } else {
        info!("No hotkeys configured - hotkey support disabled");
        None
    };

    HotkeyResources {
        handle: hotkey_handle,
        rx: hotkey_rx,
        groups: hotkey_groups,
    }
}

async fn run_event_loop(
    ctx: AppContext<'_>,
    mut resources: DaemonResources<'_>,
    mut hotkey_rx: mpsc::Receiver<CycleCommand>,
    hotkey_groups: HashMap<crate::config::HotkeyBinding, Vec<String>>,
    mut sigusr1: tokio::signal::unix::Signal,
) -> Result<()> {
    info!("Preview daemon running (async)");

    // Wrap X11 connection in AsyncFd for async polling
    // This allows us to wake up exactly when X11 has data, without busy polling
    let x11_fd = AsyncFd::new(ctx.conn.stream().as_raw_fd())
        .context("Failed to create AsyncFd for X11 connection")?;

    loop {
        // Process all pending X11 events without blocking to ensure the queue is drained
        // This prevents the event channel from filling up during heavy activity
        while let Some(event) = ctx
            .conn
            .poll_for_event()
            .context("Failed to poll for X11 event")?
        {
            // Scope the mutable borrows for event handling
            {
                let mut context = EventContext {
                    app_ctx: &ctx,
                    daemon_config: &mut resources.config,
                    eve_clients: &mut resources.eve_clients,
                    session_state: &mut resources.session,
                    cycle_state: &mut resources.cycle,
                };

                let _ = handle_event(&mut context, event)
                    .inspect_err(|err| error!(error = ?err, "Event handling error"));
            }
        }

        // Flush any pending requests to X server
        let _ = ctx.conn.flush();

        tokio::select! {
             // 1. Handle SIGUSR1 (Manual Save)
            _ = sigusr1.recv() => {
                info!("Manual save requested via SIGUSR1");
                if let Err(e) = resources.config.save() {
                    error!(error = ?e, "Failed to save positions after SIGUSR1");
                } else {
                    info!("Positions saved successfully");
                }
            }

            // 2. Handle Hotkey Commands
            Some(command) = hotkey_rx.recv() => {
                 // Check if we should only allow hotkeys when EVE window is focused
                let should_process = if resources.config.profile.hotkey_require_eve_focus {
                    crate::x11::is_eve_window_focused(ctx.conn, ctx.screen, ctx.atoms)
                        .inspect_err(|e| error!(error = %e, "Failed to check focused window"))
                        .unwrap_or(false)
                } else {
                    true
                };

                if should_process {
                    info!(command = ?command, "Received hotkey command");

                    // Debug: log the actual binding details for per-character hotkeys
                    if let CycleCommand::CharacterHotkey(ref binding) = command {
                        debug!(
                            key_code = binding.key_code,
                            ctrl = binding.ctrl,
                            shift = binding.shift,
                            alt = binding.alt,
                            super_key = binding.super_key,
                            devices = ?binding.source_devices,
                            "Character hotkey binding details"
                        );
                    }

                    // Build logged-out map if feature is enabled in profile
                    let logged_out_map = if resources.config.profile.hotkey_logged_out_cycle {
                        Some(&resources.session.window_last_character)
                    } else {
                        None
                    };

                    let result = match command {
                        CycleCommand::Forward => resources.cycle.cycle_forward(logged_out_map)
                            .map(|(w, s)| (w, s.to_string())),
                        CycleCommand::Backward => resources.cycle.cycle_backward(logged_out_map)
                            .map(|(w, s)| (w, s.to_string())),
                        CycleCommand::CharacterHotkey(ref binding) => {
                            debug!(
                                binding = %binding.display_name(),
                                "Received per-character hotkey command"
                            );

                            // Find the group of characters sharing this hotkey
                            if let Some(char_group) = hotkey_groups.get(binding) {
                                debug!(
                                    binding = %binding.display_name(),
                                    group = ?char_group,
                                    "Found hotkey group"
                                );

                                // Delegate logic to CycleState
                                resources.cycle.activate_next_in_group(char_group, logged_out_map)
                            } else {
                                warn!(
                                    binding = %binding.display_name(),
                                    available_groups = hotkey_groups.len(),
                                    "Character hotkey binding not found in groups - this shouldn't happen!"
                                );
                                None
                            }
                        }
                    };

                    if let Some((window, character_name)) = result {
                        let display_name = if character_name.is_empty() {
                            eve::LOGGED_OUT_DISPLAY_NAME
                        } else {
                            &character_name
                        };
                        info!(
                            window = window,
                            character = %display_name,
                            "Activating window via hotkey"
                        );

                        if let Err(e) = activate_window(ctx.conn, ctx.screen, ctx.atoms, window) {
                            error!(window = window, error = %e, "Failed to activate window");
                        } else {
                            debug!(window = window, "activate_window completed successfully");

                            if resources.config.profile.client_minimize_on_switch {
                                // Minimize all other EVE clients after successful activation
                                let other_windows: Vec<Window> = resources.eve_clients
                                    .keys()
                                    .copied()
                                    .filter(|w| *w != window)
                                    .collect();
                                for other_window in other_windows {
                                    if let Err(e) = minimize_window(ctx.conn, ctx.screen, ctx.atoms, other_window) {
                                        debug!(window = other_window, error = %e, "Failed to minimize window via hotkey");
                                    }
                                }
                            }
                        }
                    } else {
                        warn!(active_windows = resources.cycle.config_order().len(), "No window to activate, cycle state is empty");
                    }
                } else {
                    info!(hotkey_require_eve_focus = resources.config.profile.hotkey_require_eve_focus, "Hotkey ignored, EVE window not focused (hotkey_require_eve_focus enabled)");
                }
            }

            // 3. Handle X11 Events Check
            // Wait for X11 connection to be readable (meaning an event is available)
            // This is level-triggered
            ready = x11_fd.readable() => {
                match ready {
                     Ok(mut guard) => {
                         // IMPORTANT: We must clear the readiness state, otherwise readable()
                         // will return immediately again in the next loop iteration, causing 100% CPU usage.
                         guard.clear_ready();
                     }
                     Err(e) => {
                         error!(error = ?e, "Failed to poll X11 fd readiness");
                     }
                }
                // Continue to top of loop to process events
                continue;
            }
        }
    }
}

pub async fn run_preview_daemon() -> Result<()> {
    // 1. Initialize X11 connection and resources
    let (conn, _screen_num, atoms, formats) =
        initialize_x11().context("Failed to initialize X11")?;

    // Re-acquire screen reference from connection (x11rb::connect returns screen index)
    let screen = &conn.setup().roots[_screen_num];

    // 2. Load Configuration and State
    let (mut daemon_config, config, mut session_state, mut cycle_state) =
        load_configuration(screen).context("Failed to load configuration")?;

    // 3. Setup Signal Handler
    // We do this here as it requires async runtime context
    let sigusr1 = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
        .context("Failed to register SIGUSR1 handler")?;
    info!("Registered SIGUSR1 handler for manual position save");

    // 4. Setup Hotkeys
    let hotkeys = setup_hotkeys(&daemon_config);

    // 5. Initialize Font Renderer
    // This depends on config so it runs after config load
    let font_renderer = font::FontRenderer::resolve_from_config(
        &conn,
        &daemon_config.profile.thumbnail_text_font,
        daemon_config.profile.thumbnail_text_size as f32,
    )
    .context("Failed to initialize font renderer")?;

    info!(
        size = daemon_config.profile.thumbnail_text_size,
        font = %daemon_config.profile.thumbnail_text_font,
        "Font renderer initialized"
    );

    // 6. Build AppContext
    let ctx = AppContext {
        conn: &conn,
        screen,
        config: &config,
        atoms: &atoms,
        formats: &formats,
        font_renderer: &font_renderer,
    };

    // 7. Initial Window Scan
    let eve_clients =
        super::window_detection::scan_eve_windows(&ctx, &mut daemon_config, &mut session_state)
            .context("Failed to get initial list of EVE windows")?;

    // Register initial windows with cycle state
    if config.enabled {
        for (window, thumbnail) in eve_clients.iter() {
            cycle_state.add_window(thumbnail.character_name.clone(), *window);
        }
    } else {
        for (window, character_name) in session_state.window_last_character.iter() {
            cycle_state.add_window(character_name.clone(), *window);
        }
    }

    // 8. Run Main Event Loop
    let resources = DaemonResources {
        config: daemon_config,
        session: session_state,
        cycle: cycle_state,
        eve_clients,
    };

    run_event_loop(ctx, resources, hotkeys.rx, hotkeys.groups, sigusr1).await
}
