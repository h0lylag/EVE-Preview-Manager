//! Daemon main loop and runtime initialization

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::os::fd::AsRawFd;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::damage::ConnectionExt as DamageExt;
use x11rb::protocol::xproto::*;

use crate::common::constants::eve;
use crate::common::ipc::{BootstrapMessage, ConfigMessage, DaemonMessage};
use crate::config::DaemonConfig;
use crate::input::listener::{self, CycleCommand, TimestampedCommand};
use crate::x11::{AppContext, CachedAtoms, activate_window, minimize_window, unminimize_window};
use ipc_channel::ipc::{self, IpcReceiver, IpcSender};

use super::cycle_state::CycleState;
use super::dispatcher::{EventContext, handle_event};
use super::font;
use super::session_state::SessionState;
use super::thumbnail::Thumbnail;

use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use x11rb::rust_connection::RustConnection;

use crate::input::backend::AllowedWindows;

struct HotkeyResources {
    #[allow(dead_code)]
    handle: Option<Vec<JoinHandle<()>>>,
    rx: mpsc::Receiver<TimestampedCommand>,
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
    // Initial screen metrics are required for auto-scaling thumbnails.
    let (conn, screen_num) = x11rb::connect(None)
        .context("Failed to connect to X11 server. Is DISPLAY set correctly?")?;

    let screen = &conn.setup().roots[screen_num];
    debug!(
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
    debug!("Picture formats cached");

    // Note: Font renderer initialization is deferred until after config load
    // as it depends on user-configured font settings.

    Ok((conn, screen_num, atoms, formats))
}

fn initialize_state(
    _screen: &Screen,
    daemon_config: DaemonConfig,
) -> Result<(
    DaemonConfig,
    crate::config::DisplayConfig,
    SessionState,
    CycleState,
)> {
    let config = daemon_config.build_display_config();
    debug!("Loaded display configuration");

    let session_state = SessionState::new();
    debug!(
        count = daemon_config.character_thumbnails.len(),
        "Loaded character positions from config"
    );

    // Initialize cycle state from config
    let cycle_state = CycleState::new(daemon_config.profile.cycle_groups.clone());

    Ok((daemon_config, config, session_state, cycle_state))
}

fn setup_hotkeys(daemon_config: &DaemonConfig, allowed_windows: AllowedWindows) -> HotkeyResources {
    // Create channel for hotkey thread → main loop
    let (hotkey_tx, hotkey_rx) = mpsc::channel(32);

    // Build character hotkey list from ALL defined character hotkeys
    // This ensures detached characters still have their hotkeys registered
    let mut character_hotkeys: Vec<_> = daemon_config
        .profile
        .character_hotkeys
        .values()
        .cloned()
        .collect();

    let profile_hotkeys: Vec<_> = daemon_config.profile_hotkeys.keys().cloned().collect();

    // Group characters by hotkey binding to support cycling through multiple characters on the same key
    // This allows users to bind 'F1' to Cycle [Char1, Char2] effectively
    let mut hotkey_groups: HashMap<crate::config::HotkeyBinding, Vec<String>> = HashMap::new();

    // Iterate over ALL defined character hotkeys, not just those in the cycle group.
    // This allows characters outside the cycle group to still be activated via hotkey.
    for (char_name, binding) in &daemon_config.profile.character_hotkeys {
        hotkey_groups
            .entry(binding.clone())
            .or_default()
            .push(char_name.clone());
    }

    // Include Custom Source hotkeys in the groups
    for rule in &daemon_config.profile.custom_windows {
        if let Some(binding) = &rule.hotkey {
            hotkey_groups
                .entry(binding.clone())
                .or_default()
                .push(rule.alias.clone());

            character_hotkeys.push(binding.clone());
        }
    }

    debug!(
        unique_hotkeys = hotkey_groups.len(),
        cycle_groups = daemon_config.profile.cycle_groups.len(),
        "Built per-character hotkey groups"
    );

    // Debug: log each hotkey group
    for (binding, chars) in &hotkey_groups {
        debug!(
            binding = %binding.display_name(),
            characters = ?chars,
            "Hotkey group registered"
        );
    }

    // Spawn hotkey listener (start if any hotkeys configured: cycle or per-character)
    let cycle_hotkeys: Vec<(CycleCommand, crate::config::HotkeyBinding)> = daemon_config
        .profile
        .cycle_groups
        .iter()
        .flat_map(|g| {
            let mut hotkeys = Vec::new();
            if let Some(fwd) = &g.hotkey_forward {
                hotkeys.push((CycleCommand::Forward(g.name.clone()), fwd.clone()));
            }
            if let Some(bwd) = &g.hotkey_backward {
                hotkeys.push((CycleCommand::Backward(g.name.clone()), bwd.clone()));
            }
            hotkeys
        })
        .collect();

    let has_cycle_keys = !cycle_hotkeys.is_empty();
    let has_character_hotkeys = !character_hotkeys.is_empty();
    let _has_profile_hotkeys = !profile_hotkeys.is_empty();
    let has_profile_hotkeys = !profile_hotkeys.is_empty();
    let has_skip_key = daemon_config.profile.hotkey_toggle_skip.is_some();
    let has_toggle_previews_key = daemon_config.profile.hotkey_toggle_previews.is_some();

    let hotkey_handle = if has_cycle_keys
        || has_character_hotkeys
        || has_profile_hotkeys
        || has_skip_key
        || has_toggle_previews_key
    {
        // Select backend based on functionality
        use crate::config::HotkeyBackendType;
        use crate::input::backend::{HotkeyBackend, HotkeyConfiguration};

        let hotkey_config = HotkeyConfiguration {
            cycle_hotkeys,
            character_hotkeys: character_hotkeys.clone(),
            profile_hotkeys: profile_hotkeys.clone(),
            toggle_skip_key: daemon_config.profile.hotkey_toggle_skip.clone(),
            toggle_previews_key: daemon_config.profile.hotkey_toggle_previews.clone(),
        };

        match daemon_config.profile.hotkey_backend {
            HotkeyBackendType::X11 => {
                debug!("Using X11 hotkey backend");
                match crate::input::x11_backend::X11Backend::spawn(
                    hotkey_tx,
                    hotkey_config,
                    daemon_config.profile.hotkey_input_device.clone(),
                    daemon_config.profile.hotkey_require_eve_focus,
                    allowed_windows.clone(),
                ) {
                    Ok(handle) => {
                        debug!(
                            enabled = true,
                            backend = "x11",
                            has_cycle_keys = has_cycle_keys,
                            has_character_hotkeys = has_character_hotkeys,
                            has_profile_hotkeys = has_profile_hotkeys,
                            has_skip_key = has_skip_key,
                            has_toggle_previews_key = has_toggle_previews_key,
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
                        hotkey_config,
                        daemon_config.profile.hotkey_input_device.clone(),
                        daemon_config.profile.hotkey_require_eve_focus,
                        allowed_windows.clone(),
                    ) {
                        Ok(handle) => {
                            debug!(
                                enabled = true,
                                backend = "evdev",
                                has_cycle_keys = has_cycle_keys,
                                has_character_hotkeys = has_character_hotkeys,
                                has_profile_hotkeys = has_profile_hotkeys,
                                has_skip_key = has_skip_key,
                                has_toggle_previews_key = has_toggle_previews_key,
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

#[allow(clippy::too_many_arguments)]
async fn run_event_loop(
    conn: &RustConnection,
    screen: &Screen,
    mut display_config: crate::config::DisplayConfig,
    atoms: &CachedAtoms,
    formats: &crate::x11::CachedFormats,
    mut font_renderer: crate::daemon::font::FontRenderer,
    mut resources: DaemonResources<'_>,
    mut hotkey_rx: mpsc::Receiver<TimestampedCommand>,
    hotkey_groups: HashMap<crate::config::HotkeyBinding, Vec<String>>,
    mut sigusr1: tokio::signal::unix::Signal,
    config_rx: IpcReceiver<ConfigMessage>,
    status_tx: IpcSender<DaemonMessage>,
    allowed_windows: AllowedWindows,
) -> Result<()> {
    debug!("Daemon running (async)");

    // Wrap IPC receiver in something async-friendly?
    // IpcReceiver is blocking. IPC-channel doesn't support async recv out of the box in a way that integrates with tokio::select! easily without a bridge.
    // We should spawn a thread to bridge IPC messages to a tokio channel.
    let (ipc_config_tx, mut ipc_config_rx_tokio) = mpsc::channel(1);

    std::thread::spawn(move || {
        while let Ok(msg) = config_rx.recv() {
            if ipc_config_tx.blocking_send(msg).is_err() {
                break; // Manager connection lost
            }
        }
        // If config_rx fails (Manager side closed), this thread ends.
        // We should probably explicitly terminate the daemon here if we want absolute safety.
        error!("IPC Config channel closed - Manager process likely terminated. Exiting daemon.");
        std::process::exit(1);
    });

    // Wrap X11 connection in AsyncFd for async polling
    // This allows us to wake up exactly when X11 has data, without busy polling
    let x11_fd = AsyncFd::new(conn.stream().as_raw_fd())
        .context("Failed to create AsyncFd for X11 connection")?;

    // Heartbeat timer (3s interval) - skip missed ticks to prevent backlog
    let mut heartbeat_interval = tokio::time::interval(std::time::Duration::from_secs(3));
    heartbeat_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Timer for delayed thumbnail hiding (hysteresis)
    let hide_timer = tokio::time::sleep(tokio::time::Duration::from_secs(86400));
    tokio::pin!(hide_timer);

    loop {
        // Scope ctx to allow mutable borrow of font_renderer later
        {
            // Construct AppContext for this iteration
            let ctx = AppContext {
                conn,
                screen,
                atoms,
                formats,
            };

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

                        status_tx: &status_tx,
                        font_renderer: &font_renderer,
                        display_config: &display_config,
                    };

                    let _ = handle_event(&mut context, event)
                        .inspect_err(|err| error!(error = ?err, "Event handling error"));
                }
            }

            // Flush any pending requests to X server
            let _ = ctx.conn.flush();
        }

        // Sync allowed windows with backend
        // Include both EVE client source windows AND thumbnail windows to ensure hotkeys
        // work when focus is on either the clients themselves or their thumbnails.
        // This is critical when thumbnails are hidden/shown or clients are minimized.
        {
            let mut current_windows: HashSet<u32> = HashSet::new();

            // allow hotkeys for all EVE client source windows known to the cycle state
            // (including those without thumbnails/previews)
            for src_window in resources.cycle.get_active_windows().values() {
                current_windows.insert(*src_window);
            }

            // allow hotkeys for thumbnail overlay windows
            for thumbnail in resources.eve_clients.values() {
                current_windows.insert(thumbnail.window());
            }

            let need_update = {
                if let Ok(guard) = allowed_windows.read() {
                    *guard != current_windows
                } else {
                    true
                }
            };

            #[allow(clippy::collapsible_if)]
            if need_update {
                if let Ok(mut guard) = allowed_windows.write() {
                    *guard = current_windows;
                    debug!("Allowed windows set updated");
                }
            }
        }

        // Update hide timer if deadline was set or changed
        if let Some(deadline) = resources.session.focus_loss_deadline {
            // Calculate duration until deadline
            // If deadline is in past, use 0 duration to fire immediately
            let duration = deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(std::time::Duration::ZERO);

            hide_timer
                .as_mut()
                .reset(tokio::time::Instant::now() + duration);

            debug!(
                delay_ms = duration.as_millis(),
                "Updated hide timer deadline"
            );
        }

        tokio::select! {
            biased;  // Process branches in order - prioritize hotkeys over heartbeat/IPC

            // 1. Handle Hotkey Commands (HIGHEST PRIORITY)
            // Checked first to minimize latency and prevent XWayland grab conflicts
            Some(msg) = hotkey_rx.recv() => {
                 let TimestampedCommand { command, timestamp } = msg;

                 // Reconstruct AppContext for hotkey handling (read-only borrow)
                let ctx = AppContext {
                    conn,
                    screen,

                    atoms,
                    formats,
                };

                // NOTE: Logic gates hotkeys to only function when a tracked window has focus.
                // This prevents hotkeys from firing while typing in other applications (e.g. Discord).
                let should_process = if resources.config.profile.hotkey_require_eve_focus {
                    match crate::x11::get_active_window(ctx.conn, ctx.screen, ctx.atoms) {
                        Ok(Some(active_window)) => {
                            // Check if active window is a known EVE window (thumbnail OR just identified)
                            let is_known = resources.eve_clients.contains_key(&active_window) ||
                                         resources.cycle.get_active_windows().values().any(|&w| w == active_window);

                            if is_known {
                                true
                            } else {
                                // NOTE: The active window might be a child (e.g. in Wine/Proton apps like Mod Organizer).
                                // We must walk the window hierarchy to check if any ancestor is a tracked client.
                                let mut current = active_window;
                                let mut found_ancestor = false;

                                // NOTE: Limit traversal depth to prevent infinite loops (X11 cycles) or stalls.
                                for _ in 0..10 {
                                    match ctx.conn.query_tree(current) {
                                        Ok(cookie) => {
                                            if let Ok(reply) = cookie.reply() {
                                                if resources.eve_clients.contains_key(&reply.parent) ||
                                                   resources.cycle.get_active_windows().values().any(|&w| w == reply.parent) {
                                                    found_ancestor = true;
                                                    debug!(
                                                        child = active_window,
                                                        parent = reply.parent,
                                                        "Hotkey allowed: Found tracked ancestor"
                                                    );
                                                    break;
                                                }
                                                // Stop if we hit root or invalid window
                                                if reply.parent == ctx.screen.root || reply.parent == 0 {
                                                    break;
                                                }
                                                current = reply.parent;
                                            } else {
                                                break;
                                            }
                                        }
                                        Err(_) => break,
                                    }
                                }

                                if !found_ancestor {
                                    debug!(
                                        active_window = active_window,
                                        "Hotkey ignored: Focused window is not a tracked client or descendant"
                                    );
                                }
                                found_ancestor
                            }
                        }
                        Ok(None) => false,
                        Err(e) => {
                            error!(error = %e, "Failed to check focused window");
                            false
                        }
                    }
                } else {
                    true
                };

                if should_process {
                    debug!(command = ?command, "Received hotkey command");

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

                    if let Some((window, character_name)) = handle_cycle_command(&command, &mut resources, &ctx, &font_renderer, &status_tx, &hotkey_groups) {
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

                        // NOTE: When minimize mode is enabled, unminimize the target window FIRST
                        // before calling activate_window. This ensures the window is restored from
                        // minimized state so it can properly receive keyboard focus.
                        if resources.config.profile.client_minimize_on_switch
                            && let Err(e) = unminimize_window(ctx.conn, ctx.screen, ctx.atoms, window)
                        {
                            error!(window = window, error = %e, "Failed to unminimize window before activation");
                        }

                        if let Err(e) = activate_window(ctx.conn, ctx.screen, ctx.atoms, window, timestamp) {
                            error!(window = window, error = %e, "Failed to activate window");
                        } else {
                            debug!(window = window, "activate_window completed successfully");

                            // Set current window immediately after successful activation.
                            // This ensures the border shows correctly during the 25ms delay before
                            // FocusIn arrives. The FocusIn handler will confirm this later.
                            resources.cycle.set_current_by_window(window);

                            // Draw active border immediately to prevent flash during delay
                            if let Some(thumb) = resources.eve_clients.get(&window) {
                                let display_config = resources.config.build_display_config();
                                if let Err(e) = thumb.border(
                                    &display_config,
                                    true,
                                    resources.cycle.is_skipped(&thumb.character_name),
                                    &font_renderer,
                                ) {
                                    warn!(window = window, error = %e, "Failed to draw initial active border");
                                }
                            }

                            // Clear borders from ALL other windows immediately (including minimized ones)
                            // This ensures we don't leave stale active borders on minimized windows
                            for (w, thumb) in resources.eve_clients.iter_mut() {
                                if *w != window {
                                    let display_config = resources.config.build_display_config();
                                    // Only change state for non-minimized windows
                                    // Minimized windows should stay Minimized - calling border() on them causes
                                    // double-rendering. Instead, re-call minimized() to properly clear and re-render.
                                    if thumb.state.is_minimized() {
                                        if let Err(e) = thumb.minimized(&display_config, &font_renderer) {
                                            warn!(window = *w, error = %e, "Failed to re-render minimized window");
                                        }
                                    } else {
                                        thumb.state = crate::common::types::ThumbnailState::Normal { focused: false };
                                        if let Err(e) = thumb.border(
                                            &display_config,
                                            false,
                                            resources.cycle.is_skipped(&thumb.character_name),
                                            &font_renderer,
                                        ) {
                                            warn!(window = *w, error = %e, "Failed to clear border during switch");
                                        }
                                    }
                                }
                            }

                            // CRITICAL: Flush X11 connection to ensure border updates are rendered
                            // before the 25ms delay. Without this, borders may flash to wrong clients.
                            let _ = ctx.conn.flush();

                            if resources.config.profile.client_minimize_on_switch {
                                // NOTE: Critical delay to prevent KWin focus thrashing. Without this,
                                // KWin repeatedly redirects focus to window 2097152 (internal KWin window)
                                // during the minimize operations, causing continuous FocusOut/FocusIn loops.
                                // The 25ms allows KWin to fully commit to the focus transfer before we
                                // start changing other window states.
                                tokio::time::sleep(std::time::Duration::from_millis(25)).await;

                                // Minimize all other EVE clients after successful activation
                                let other_windows: Vec<Window> = resources.eve_clients
                                    .iter()
                                    .filter(|(w, _)| **w != window)
                                    .filter(|(_, t)| {
                                        let is_exempt = resources.config
                                            .custom_source_thumbnails
                                            .get(&t.character_name)
                                            .or_else(|| resources.config.character_thumbnails.get(&t.character_name))
                                            .map(|settings| settings.exempt_from_minimize)
                                            .unwrap_or(false);

                                        !is_exempt
                                    })
                                    .map(|(w, _)| *w)
                                    .collect();
                                for other_window in other_windows {
                                    // Clear border on the window BEFORE minimizing it
                                    // This prevents leaving stale active borders on minimized windows
                                    if let Some(thumb) = resources.eve_clients.get_mut(&other_window) {
                                        // Don't change state here - let the minimize handler set it to Minimized
                                        // Just clear the border for now
                                        let display_config = resources.config.build_display_config();
                                        if let Err(e) = thumb.border(
                                            &display_config,
                                            false,
                                            resources.cycle.is_skipped(&thumb.character_name),
                                            &font_renderer,
                                        ) {
                                            warn!(window = other_window, error = %e, "Failed to clear border before minimize");
                                        }
                                    }
                                    if let Err(e) = minimize_window(ctx.conn, ctx.screen, ctx.atoms, other_window) {
                                        debug!(window = other_window, error = %e, "Failed to minimize window via hotkey");
                                    }
                                }

                                // Minimize Manager GUI as well (to prevent focus stealing/clutter)
                                // We search for "eve-preview-manager" class.
                                // NOTE: Thumbnails are now "eve-preview-thumbnail", so this is safe/unique.
                                let manager_window = crate::x11::get_client_list(ctx.conn, ctx.atoms)
                                    .ok()
                                    .and_then(|windows| {
                                        windows.into_iter().find(|&w| {
                                            crate::x11::get_window_class(ctx.conn, w, ctx.atoms)
                                                .ok()
                                                .flatten()
                                                .map(|class| class == "eve-preview-manager")
                                                .unwrap_or(false)
                                        })
                                    });

                                if let Some(mgr_win) = manager_window {
                                    if let Err(e) = minimize_window(ctx.conn, ctx.screen, ctx.atoms, mgr_win) {
                                        debug!(window = mgr_win, error = %e, "Failed to minimize Manager GUI");
                                    } else {
                                        debug!("Minimized Manager GUI");
                                    }
                                }
                            }
                        }
                    } else {
                        warn!("No window to activate via hotkey");
                    }
                } else {
                    info!(hotkey_require_eve_focus = resources.config.profile.hotkey_require_eve_focus, "Hotkey ignored, EVE window not focused (hotkey_require_eve_focus enabled)");
                }


            }

            // 2. Handle X11 Events (SECOND PRIORITY)
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

            // 3. Handle Delayed Hide (Hysteresis)
            // Only process this branch if there's an active deadline
            () = &mut hide_timer, if resources.session.focus_loss_deadline.is_some() => {
                debug!("Executing delayed thumbnail hide");
                for thumbnail in resources.eve_clients.values_mut() {
                    if let Err(e) = thumbnail.visibility(false) {
                        error!(error = %e, character = %thumbnail.character_name, "Failed to hide thumbnail on focus timeout");
                    }
                }
                // Clear deadline - this will disable the branch until next FocusOut
                resources.session.focus_loss_deadline = None;
            }

            // 4. Send Heartbeat (Lower priority - can wait)
            _ = heartbeat_interval.tick() => {
                if let Err(e) = status_tx.send(DaemonMessage::Heartbeat) {
                    error!(error = %e, "Failed to send heartbeat to Manager");
                    // If we can't send heartbeat, manager might be dead.
                    // We'll let the IPC config channel failure handle termination.
                }
            }

            // 4. Handle SIGUSR1 (Lower priority)
            _ = sigusr1.recv() => {
                info!("SIGUSR1 received - config is now managed by Manager via IPC");
                let _ = status_tx.send(DaemonMessage::Status("SIGUSR1 received: Syncing config...".to_string()));
            }

            // 5. Handle IPC Config Updates (Lower priority - expensive operation)
            Some(msg) = ipc_config_rx_tokio.recv() => {
                match msg {
                    ConfigMessage::Full(new_config) => {
                        let new_config = *new_config; // Unbox
                        info!("Received full config update via IPC");

                        // Update DaemonConfig
                        resources.config = new_config;

                        // Only rebuild font renderer if font settings actually changed
                        let font_name = &resources.config.profile.thumbnail_text_font;
                        let font_size = resources.config.profile.thumbnail_text_size as f32;

                        if !font_renderer.matches_config(font_name, font_size) {
                            debug!("Font settings changed, rebuilding renderer");
                            let new_renderer = crate::daemon::font::FontRenderer::resolve_from_config(
                                conn,
                                font_name,
                                font_size,
                            );

                            match new_renderer {
                                Ok(renderer) => {
                                    font_renderer = renderer;
                                    info!("Font renderer updated");
                                }
                                Err(e) => {
                                    error!(error = %e, "Failed to update font renderer");
                                }
                            }
                        } else {
                            debug!("Font settings unchanged, skipping rebuild");
                        }

                        // Update CycleState (hotkeys)
                        // NOTE: Do NOT recreate CycleState here! It would wipe out active_windows tracking.
                        // CycleState is only created once at startup and maintains window state across config reloads.

                        // Force redraw of all thumbnails with new settings
                        display_config = resources.config.build_display_config();
                        for thumbnail in resources.eve_clients.values_mut() {
                             let _ = thumbnail.update(&display_config, &font_renderer);
                        }

                        info!("Full config updated");
                    },

                    ConfigMessage::ThumbnailMove { name, is_custom, x, y, width, height } => {
                        debug!(
                            name = %name,
                            is_custom = is_custom,
                            x = x,
                            y = y,
                            width = width,
                            height = height,
                            "Received ThumbnailMove delta"
                        );

                        // Find the specific thumbnail by character name AND type (EVE vs custom source)
                        let thumbnail_opt = resources.eve_clients.values_mut().find(|t| {
                            if t.character_name != name {
                                return false;
                            }

                            // Verify this thumbnail matches the expected type
                            // Custom sources and EVE characters can have name collisions
                            if is_custom {
                                resources.config.custom_source_thumbnails.contains_key(&name)
                            } else {
                                resources.config.character_thumbnails.contains_key(&name)
                            }
                        });

                        if let Some(thumb) = thumbnail_opt {
                            // IDEMPOTENCY CHECK (Critical for performance)
                            // If the position/size matches what we already have, skip processing
                            // This prevents redundant X11 operations when the Daemon initiated the change
                            if thumb.current_position.x == x
                                && thumb.current_position.y == y
                                && thumb.dimensions.width == width
                                && thumb.dimensions.height == height
                            {
                                debug!(
                                    name = %name,
                                    "ThumbnailMove ignored: position/size unchanged (idempotent)"
                                );
                                continue;  // Skip to next iteration of select! loop
                            }

                            // Position differs - Manager corrected it (e.g., snapping, clamping)
                            // Apply the Manager's authoritative coordinates
                            if let Err(e) = thumb.reposition(x, y) {
                                error!(name = %name, error = %e, "Failed to reposition thumbnail");
                            }
                            if let Err(e) = thumb.resize(width, height) {
                                error!(name = %name, error = %e, "Failed to resize thumbnail");
                            }
                            info!(
                                name = %name,
                                x = x,
                                y = y,
                                width = width,
                                height = height,
                                "Position updated by Manager (ThumbnailMove delta)"
                            );
                        } else {
                            debug!(name = %name, is_custom = is_custom, "ThumbnailMove ignored: character not tracked");
                        }
                    }
                }
            }
        }
    }
}

pub async fn run_daemon(ipc_server_name: String) -> Result<()> {
    // 1. Initialize X11 connection and resources
    let (conn, _screen_num, atoms, formats) =
        initialize_x11().context("Failed to initialize X11")?;

    // Re-acquire screen reference from connection (x11rb::connect returns screen index)
    let screen = &conn.setup().roots[_screen_num];

    // 2. Setup IPC and get initial config
    debug!("Connecting to IPC server: {}", ipc_server_name);
    let bootstrap_sender: IpcSender<BootstrapMessage> =
        IpcSender::connect(ipc_server_name).context("Failed to connect to IPC server")?;

    let (config_tx, config_rx) =
        ipc::channel::<ConfigMessage>().context("Failed to create config IPC channel")?;
    let (status_tx, status_rx) =
        ipc::channel::<DaemonMessage>().context("Failed to create status IPC channel")?;

    // Send the channels to the Manager
    bootstrap_sender
        .send((config_tx, status_rx))
        .context("Failed to send bootstrap message")?;

    debug!("Waiting for initial configuration...");
    let initial_config = match config_rx.recv() {
        Ok(ConfigMessage::Full(config)) => *config,
        Ok(ConfigMessage::ThumbnailMove { .. }) => {
            return Err(anyhow::anyhow!(
                "Expected Full config on startup, got ThumbnailMove"
            ));
        }
        Err(e) => return Err(anyhow::anyhow!("Failed to receive initial config: {}", e)),
    };
    debug!("Received initial configuration");

    // 3. Initialize State from Config
    let (mut daemon_config, config, mut session_state, mut cycle_state) =
        initialize_state(screen, initial_config).context("Failed to initialize state")?;

    // 3. Setup Signal Handlers
    // We do this here as it requires async runtime context
    let sigusr1 = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
        .context("Failed to register SIGUSR1 handler")?;

    debug!("Registered SIGUSR1 handler for manual position save");

    // 4. Setup Hotkeys
    let allowed_windows = Arc::new(RwLock::new(HashSet::new()));
    let hotkeys = setup_hotkeys(&daemon_config, allowed_windows.clone());

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

    // 6. Build AppContext & 7. Initial Window Scan
    // We scope this so ctx (borrowing font_renderer) is dropped before we move font_renderer
    let mut eve_clients;
    {
        let ctx = AppContext {
            conn: &conn,
            screen,
            atoms: &atoms,
            formats: &formats,
        };

        // Initial scan for existing EVE windows
        // Now populates cycle_state directly during scan
        eve_clients = super::window_detection::scan_eve_windows(
            &ctx,
            &config,
            &font_renderer,
            &mut daemon_config,
            &mut session_state,
            &mut cycle_state,
        )
        .context("Failed to get initial list of EVE windows")?;
    }

    // Initialize border state for all windows (defaults to inactive/cleared)
    // This ensures inactive borders are drawn immediately on startup if enabled
    let active_eve_window = crate::x11::get_active_eve_window(&conn, screen, &atoms)
        .ok()
        .flatten();

    for (window, thumbnail) in eve_clients.iter_mut() {
        // Check if this window currently has focus
        let is_focused = active_eve_window.map(|w| w == *window).unwrap_or(false);

        // Update state and draw appropriate border
        thumbnail.state = crate::common::types::ThumbnailState::Normal {
            focused: is_focused,
        };
        if let Err(e) = thumbnail.border(
            &config,
            is_focused,
            cycle_state.is_skipped(&thumbnail.character_name),
            &font_renderer,
        ) {
            // Log warning but continue
            tracing::warn!(
                window = window,
                character = %thumbnail.character_name,
                error = %e,
                "Failed to draw initial border"
            );
        }
    }

    // 8. Run Main Event Loop
    let resources = DaemonResources {
        config: daemon_config,
        session: session_state,
        cycle: cycle_state,
        eve_clients,
    };

    run_event_loop(
        &conn,
        screen,
        config.clone(),
        &atoms,
        &formats,
        font_renderer,
        resources,
        hotkeys.rx,
        hotkeys.groups,
        sigusr1,
        config_rx,
        status_tx,
        allowed_windows,
    )
    .await
}

fn handle_cycle_command(
    command: &CycleCommand,
    resources: &mut DaemonResources<'_>,
    ctx: &AppContext<'_>,
    font_renderer: &crate::daemon::font::FontRenderer,
    status_tx: &IpcSender<DaemonMessage>,
    hotkey_groups: &HashMap<crate::config::HotkeyBinding, Vec<String>>,
) -> Option<(Window, String)> {
    // Build logged-out map if feature is enabled in profile
    let logged_out_map = if resources.config.profile.hotkey_logged_out_cycle {
        Some(&resources.session.window_last_character)
    } else {
        None
    };

    match command {
        CycleCommand::Forward(group) => resources
            .cycle
            .cycle_forward(
                group,
                logged_out_map,
                resources.config.profile.hotkey_cycle_reset_index,
            )
            .map(|(w, s)| (w, s.to_string())),
        CycleCommand::Backward(group) => resources
            .cycle
            .cycle_backward(
                group,
                logged_out_map,
                resources.config.profile.hotkey_cycle_reset_index,
            )
            .map(|(w, s)| (w, s.to_string())),
        CycleCommand::CharacterHotkey(binding) => {
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
                resources
                    .cycle
                    .activate_next_in_group(char_group, logged_out_map)
            } else {
                warn!(
                    binding = %binding.display_name(),
                    available_groups = hotkey_groups.len(),
                    "Character hotkey binding not found in groups - this shouldn't happen!"
                );
                None
            }
        }
        CycleCommand::ProfileHotkey(binding) => {
            info!(binding = %binding.display_name(), "Received profile switch hotkey");

            if let Some(profile_name) = resources.config.profile_hotkeys.get(binding) {
                info!(target_profile = %profile_name, "Requesting profile switch via IPC");
                if let Err(e) =
                    status_tx.send(DaemonMessage::RequestProfileSwitch(profile_name.clone()))
                {
                    error!(error = %e, "Failed to send profile switch request to Manager");
                }
            }
            None
        }
        CycleCommand::ToggleSkip => {
            // Identify focused window to determine which character to skip
            let active_window = crate::x11::get_active_eve_window(ctx.conn, ctx.screen, ctx.atoms)
                .ok()
                .flatten();

            if let Some(window) = active_window {
                if let Some(thumbnail) = resources.eve_clients.get_mut(&window) {
                    let char_name = thumbnail.character_name.clone();
                    let is_skipped = resources.cycle.toggle_skip(&char_name);
                    info!(character = %char_name, skipped = is_skipped, "Toggled skip status");

                    // Force redraw of border to show/hide indicator
                    let focused = thumbnail.state.is_focused();
                    let display_config = resources.config.build_display_config();
                    if let Err(e) =
                        thumbnail.border(&display_config, focused, is_skipped, font_renderer)
                    {
                        warn!(character = %char_name, error = %e, "Failed to update border after toggle skip");
                    }
                } else {
                    warn!("Focused EVE window not found in client list");
                }
            } else {
                warn!("Cannot toggle skip: No EVE window focused");
            }
            None
        }
        CycleCommand::TogglePreviews => {
            resources.config.runtime_hidden = !resources.config.runtime_hidden;
            info!(
                hidden = resources.config.runtime_hidden,
                "Toggled previews visibility"
            );

            // Force visibility update for all known thumbnails
            let display_config = resources.config.build_display_config();
            for thumbnail in resources.eve_clients.values_mut() {
                // When revealing, respect per-character overrides: force-hidden thumbnails stay hidden
                let should_render = display_config
                    .character_settings
                    .get(&thumbnail.character_name)
                    .and_then(|s| s.override_render_preview)
                    .unwrap_or(display_config.enabled);

                let target_visible = !resources.config.runtime_hidden && should_render;

                if let Err(e) = thumbnail.visibility(target_visible) {
                    warn!(character = %thumbnail.character_name, error = %e, "Failed to update visibility after toggle");
                } else if target_visible {
                    // Force update to ensure content is drawn if revealed
                    let _ = thumbnail.update(&display_config, font_renderer);
                }
            }
            None
        }
    }
}
