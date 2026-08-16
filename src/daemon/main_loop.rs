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
use crate::common::types::SourceIdentity;
use crate::config::DaemonConfig;
use crate::config::profile::LoggedOutUnidentifiedCycleMode;
use crate::input::listener::{self, CycleCommand, TimestampedCommand};
use crate::x11::{
    AppContext, CachedAtoms, activate_window, minimize_window, refresh_pointer_state,
    unminimize_window,
};
use ipc_channel::ipc::{self, IpcReceiver, IpcSender};

use super::border_update::sync_focused_borders;
use super::cycle_state::{CycleActivation, CycleState};
use super::dispatcher::{EventContext, handle_event};
use super::font;
use super::group_drag::GroupDragState;
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
    groups: HashMap<crate::config::HotkeyBinding, Vec<SourceIdentity>>,
}

struct DaemonResources<'a> {
    config: DaemonConfig,
    session: SessionState,
    cycle: CycleState,
    eve_clients: HashMap<Window, Thumbnail<'a>>,
    group_drag: GroupDragState,
}

fn restore_interrupted_group_drag(
    conn: &RustConnection,
    resources: &mut DaemonResources<'_>,
    reason: &'static str,
) {
    match super::handlers::input::cancel_group_drag(
        conn,
        &mut resources.eve_clients,
        &mut resources.group_drag,
        None,
    ) {
        Ok(0) => {}
        Ok(restored_count) => {
            debug!(restored_count, reason, "Restored interrupted group drag")
        }
        Err(error) => {
            warn!(error = %error, reason, "Failed to restore interrupted group drag")
        }
    }
}

fn direct_tracked_source_window(
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    active_windows: Option<&HashMap<Window, Option<SourceIdentity>>>,
    window: Window,
) -> Option<Window> {
    if active_windows.is_some_and(|windows| windows.contains_key(&window)) {
        return Some(window);
    }

    if thumbnails.contains_key(&window) {
        return Some(window);
    }

    thumbnails.iter().find_map(|(&source_window, thumbnail)| {
        (thumbnail.window() == window
            || thumbnail.src() == window
            || thumbnail.parent() == Some(window))
        .then_some(source_window)
    })
}

fn tracked_source_window_for_window(
    ctx: &AppContext<'_>,
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    active_windows: Option<&HashMap<Window, Option<SourceIdentity>>>,
    window: Window,
) -> Option<Window> {
    if let Some(source_window) = direct_tracked_source_window(thumbnails, active_windows, window) {
        return Some(source_window);
    }

    let mut current = window;
    for _ in 0..10 {
        let parent = ctx
            .conn
            .query_tree(current)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.parent)?;

        if let Some(source_window) =
            direct_tracked_source_window(thumbnails, active_windows, parent)
        {
            debug!(
                child = window,
                parent = parent,
                source_window = source_window,
                "Matched focused window to tracked source ancestor"
            );
            return Some(source_window);
        }

        if parent == ctx.screen.root || parent == 0 {
            break;
        }
        current = parent;
    }

    None
}

fn active_tracked_source_window(
    ctx: &AppContext<'_>,
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
) -> Option<Window> {
    let active_window = crate::x11::get_active_window(ctx.conn, ctx.screen, ctx.atoms)
        .ok()
        .flatten()?;

    tracked_source_window_for_window(ctx, thumbnails, None, active_window)
}

enum DaemonControlMessage {
    Config(ConfigMessage),
    ManagerDisconnected,
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
        "Loaded EVE character positions from config"
    );

    // Initialize cycle state from config
    let cycle_state = CycleState::new(daemon_config.profile.cycle_groups.clone());

    Ok((daemon_config, config, session_state, cycle_state))
}

fn setup_hotkeys(daemon_config: &DaemonConfig, allowed_windows: AllowedWindows) -> HotkeyResources {
    // Create channel for hotkey thread → main loop
    let (hotkey_tx, hotkey_rx) = mpsc::channel(32);

    // Build direct-source hotkey listener list from all EVE character hotkeys.
    // This ensures detached characters still have their hotkeys registered.
    let mut source_hotkeys: Vec<_> = daemon_config
        .profile
        .character_hotkeys
        .values()
        .cloned()
        .collect();

    let profile_hotkeys: Vec<_> = daemon_config.profile_hotkeys.keys().cloned().collect();

    // Group typed sources by hotkey binding so one key can rotate through every
    // EVE character or custom source assigned to it.
    let mut hotkey_groups: HashMap<crate::config::HotkeyBinding, Vec<SourceIdentity>> =
        HashMap::new();

    // Iterate over ALL defined character hotkeys, not just those in the cycle group.
    // This allows characters outside the cycle group to still be activated via hotkey.
    for (char_name, binding) in &daemon_config.profile.character_hotkeys {
        hotkey_groups
            .entry(binding.clone())
            .or_default()
            .push(SourceIdentity::eve(char_name.clone()));
    }

    // Include Custom Source hotkeys in the groups
    for rule in &daemon_config.profile.custom_windows {
        if let Some(binding) = &rule.hotkey {
            hotkey_groups
                .entry(binding.clone())
                .or_default()
                .push(SourceIdentity::custom(rule.alias.clone()));

            source_hotkeys.push(binding.clone());
        }
    }

    debug!(
        unique_hotkeys = hotkey_groups.len(),
        cycle_groups = daemon_config.profile.cycle_groups.len(),
        "Built direct-source hotkey groups"
    );

    // Debug: log each hotkey group
    for (binding, sources) in &hotkey_groups {
        debug!(
            binding = %binding.display_name(),
            sources = ?sources,
            "Hotkey group registered"
        );
    }

    // Spawn hotkey listener (start if any hotkeys configured: cycle or direct-source)
    let mut cycle_hotkeys: Vec<(CycleCommand, crate::config::HotkeyBinding)> = daemon_config
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

    if daemon_config.profile.hotkey_logged_out_unidentified_cycle
        && daemon_config
            .profile
            .hotkey_logged_out_unidentified_cycle_mode
            == LoggedOutUnidentifiedCycleMode::SeparateHotkeys
    {
        if let Some(fwd) = &daemon_config
            .profile
            .hotkey_logged_out_unidentified_cycle_forward
        {
            cycle_hotkeys.push((CycleCommand::LoggedOutUnidentifiedForward, fwd.clone()));
        }
        if let Some(bwd) = &daemon_config
            .profile
            .hotkey_logged_out_unidentified_cycle_backward
        {
            cycle_hotkeys.push((CycleCommand::LoggedOutUnidentifiedBackward, bwd.clone()));
        }
    }

    let has_cycle_keys = !cycle_hotkeys.is_empty();
    let has_direct_source_hotkeys = !source_hotkeys.is_empty();
    let _has_profile_hotkeys = !profile_hotkeys.is_empty();
    let has_profile_hotkeys = !profile_hotkeys.is_empty();
    let has_skip_key = daemon_config.profile.hotkey_toggle_skip.is_some();
    let has_toggle_previews_key = daemon_config.profile.hotkey_toggle_previews.is_some();

    let hotkey_handle = if has_cycle_keys
        || has_direct_source_hotkeys
        || has_profile_hotkeys
        || has_skip_key
        || has_toggle_previews_key
    {
        // Select backend based on functionality
        use crate::config::HotkeyBackendType;
        use crate::input::backend::{HotkeyBackend, HotkeyConfiguration};

        let hotkey_config = HotkeyConfiguration {
            cycle_hotkeys,
            character_hotkeys: source_hotkeys.clone(),
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
                            has_direct_source_hotkeys = has_direct_source_hotkeys,
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
                                has_direct_source_hotkeys = has_direct_source_hotkeys,
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
    hotkey_groups: HashMap<crate::config::HotkeyBinding, Vec<SourceIdentity>>,
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
            let is_shutdown = matches!(msg, ConfigMessage::Shutdown);
            if ipc_config_tx
                .blocking_send(DaemonControlMessage::Config(msg))
                .is_err()
            {
                return; // Main loop already ended
            }
            if is_shutdown {
                return; // Intentional shutdown; the main loop will exit cleanly.
            }
        }

        warn!(
            "IPC Config channel closed - Manager process likely terminated. Shutting down daemon."
        );
        let _ = ipc_config_tx.blocking_send(DaemonControlMessage::ManagerDisconnected);
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
                        group_drag_state: &mut resources.group_drag,

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
        // Include tracked source, parent/frame, and thumbnail windows so hotkeys
        // work when focus is on a source, its WM frame, or its preview overlay.
        // This is critical when thumbnails are hidden/shown or clients are minimized.
        {
            let mut current_windows: HashSet<u32> = HashSet::new();

            // allow hotkeys for all tracked source windows known to the cycle state
            // (including those without thumbnails/previews)
            for src_window in resources.cycle.get_active_windows().keys() {
                current_windows.insert(*src_window);
            }

            // allow hotkeys for thumbnail overlay, source, and known parent/frame windows
            for thumbnail in resources.eve_clients.values() {
                current_windows.insert(thumbnail.window());
                current_windows.insert(thumbnail.src());
                if let Some(parent) = thumbnail.parent() {
                    current_windows.insert(parent);
                }
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
                            if tracked_source_window_for_window(
                                &ctx,
                                &resources.eve_clients,
                                Some(resources.cycle.get_active_windows()),
                                active_window,
                            )
                            .is_some()
                            {
                                true
                            } else {
                                debug!(
                                    active_window = active_window,
                                    "Hotkey ignored: Focused window is not a tracked source or descendant"
                                );
                                false
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

                    // Debug: log the actual binding details for direct-source hotkeys.
                    if let CycleCommand::CharacterHotkey(ref binding) = command {
                        debug!(
                            key_code = binding.key_code,
                            ctrl = binding.ctrl,
                            shift = binding.shift,
                            alt = binding.alt,
                            super_key = binding.super_key,
                            devices = ?binding.source_devices,
                            "Direct-source hotkey binding details"
                        );
                    }

                    if let Some((window, source_identity)) = handle_cycle_command(&command, &mut resources, &ctx, &font_renderer, &status_tx, &hotkey_groups) {
                        let display_name = source_identity
                            .as_ref()
                            .map(|identity| identity.name.as_str())
                            .filter(|name| !name.is_empty())
                            .unwrap_or(eve::LOGGED_OUT_DISPLAY_NAME);
                        info!(
                            window = window,
                            source = %display_name,
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
                            resources
                                .cycle
                                .set_current_by_window_with_identity(window, source_identity.as_ref());

                            let display_config = resources.config.build_display_config();
                            sync_focused_borders(
                                &mut resources.eve_clients,
                                &resources.cycle,
                                &display_config,
                                &font_renderer,
                                window,
                                "hotkey activation",
                            );

                            // Refresh pointer state after the immediate border redraw work. This
                            // keeps the final synthetic mouse event near the real cursor instead
                            // of the legacy activation-time (0,0) coordinate.
                            if let Err(e) = refresh_pointer_state(ctx.conn, window, timestamp) {
                                debug!(window = window, error = %e, "Failed to refresh pointer state after border redraw");
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

                                // Minimize all other tracked source windows after successful activation.
                                // NOTE: Custom source rule overrides are resolved by
                                // build_display_config() into custom source settings.
                                let other_windows: Vec<Window> = resources.eve_clients
                                    .iter()
                                    .filter(|(w, _)| **w != window)
                                    .filter(|(_, t)| {
                                        !display_config
                                            .settings_for(t.source_kind(), t.effective_character_name())
                                            .map(|s| s.exempt_from_minimize)
                                            .unwrap_or(false)
                                    })
                                    .map(|(w, _)| *w)
                                    .collect();
                                for other_window in other_windows {
                                    // Clear border on the window BEFORE minimizing it
                                    // This prevents leaving stale active borders on minimized windows
                                    if let Some(thumb) = resources.eve_clients.get_mut(&other_window) {
                                        // Don't change state here - let the minimize handler set it to Minimized
                                        // Just clear the border for now
                                        if let Err(e) = thumb.border(
                                            &display_config,
                                            false,
                                            resources.cycle.is_skipped(thumb.effective_source_identity().as_ref()),
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
                                let manager_window = crate::x11::get_client_list(ctx.conn, ctx.screen, ctx.atoms)
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
                    info!(hotkey_require_eve_focus = resources.config.profile.hotkey_require_eve_focus, "Hotkey ignored, tracked source window not focused (hotkey_require_eve_focus enabled)");
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
                restore_interrupted_group_drag(conn, &mut resources, "focus-loss hide");
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
            msg = ipc_config_rx_tokio.recv() => {
                let Some(msg) = msg else {
                    info!("IPC bridge closed - shutting down daemon");
                    return Ok(());
                };

                match msg {
                    DaemonControlMessage::ManagerDisconnected => {
                        info!("Manager IPC disconnected - shutting down daemon");
                        return Ok(());
                    }
                    DaemonControlMessage::Config(ConfigMessage::Shutdown) => {
                        info!("Graceful shutdown requested by Manager");
                        return Ok(());
                    }
                    DaemonControlMessage::Config(ConfigMessage::Full(new_config)) => {
                        let new_config = *new_config; // Unbox
                        info!("Received full config update via IPC");
                        restore_interrupted_group_drag(conn, &mut resources, "configuration update");

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
                             let _ = thumbnail.refresh_name_overlay(&display_config, &font_renderer);
                             let _ = thumbnail.update(&display_config, &font_renderer);
                        }

                        info!("Full config updated");
                    },

                    DaemonControlMessage::Config(ConfigMessage::ThumbnailMoves {
                        updates,
                    }) => {
                        debug!(update_count = updates.len(), "Received thumbnail move batch");

                        for update in updates {
                            let thumbnail_opt = resources.eve_clients.values_mut().find(|t| {
                                t.effective_source_identity().as_ref() == Some(&update.source)
                            });

                            if let Some(thumb) = thumbnail_opt {
                                if thumb.current_position == update.position
                                    && thumb.dimensions == update.dimensions
                                {
                                    debug!(
                                        name = %update.source.name,
                                        "Thumbnail move ignored: position/size unchanged"
                                    );
                                    continue;
                                }

                                if let Err(e) = thumb.reposition(update.position.x, update.position.y) {
                                    error!(name = %update.source.name, error = %e, "Failed to reposition thumbnail");
                                }
                                if let Err(e) = thumb.resize(update.dimensions.width, update.dimensions.height) {
                                    error!(name = %update.source.name, error = %e, "Failed to resize thumbnail");
                                }
                                info!(
                                    name = %update.source.name,
                                    x = update.position.x,
                                    y = update.position.y,
                                    width = update.dimensions.width,
                                    height = update.dimensions.height,
                                    "Position updated by Manager"
                                );
                            } else {
                                debug!(name = %update.source.name, kind = ?update.source.kind, "Thumbnail move ignored: source not tracked");
                            }
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
        Ok(ConfigMessage::ThumbnailMoves { .. }) => {
            return Err(anyhow::anyhow!(
                "Expected Full config on startup, got ThumbnailMoves"
            ));
        }
        Ok(ConfigMessage::Shutdown) => {
            return Err(anyhow::anyhow!(
                "Expected Full config on startup, got Shutdown"
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

        // Initial scan for existing tracked source windows
        // Now populates cycle_state directly during scan
        eve_clients = super::window_detection::scan_eve_windows(
            &ctx,
            &config,
            &font_renderer,
            &mut daemon_config,
            &mut session_state,
            &mut cycle_state,
            &status_tx,
        )
        .context("Failed to get initial list of tracked source windows")?;
    }

    // Initialize border state for all windows (defaults to inactive/cleared)
    // This ensures inactive borders are drawn immediately on startup if enabled
    let init_ctx = AppContext {
        conn: &conn,
        screen,
        atoms: &atoms,
        formats: &formats,
    };
    let active_source_window = active_tracked_source_window(&init_ctx, &eve_clients);

    for (window, thumbnail) in eve_clients.iter_mut() {
        // Check if this window currently has focus
        let is_focused = active_source_window.map(|w| w == *window).unwrap_or(false);

        // Update state and draw appropriate border
        thumbnail.state = crate::common::types::ThumbnailState::Normal {
            focused: is_focused,
        };
        if let Err(e) = thumbnail.border(
            &config,
            is_focused,
            cycle_state.is_skipped(thumbnail.effective_source_identity().as_ref()),
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
        group_drag: GroupDragState::default(),
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
    hotkey_groups: &HashMap<crate::config::HotkeyBinding, Vec<SourceIdentity>>,
) -> Option<CycleActivation> {
    // Build logged-out map if feature is enabled in profile
    let logged_out_map = if resources.config.profile.hotkey_logged_out_cycle {
        Some(&resources.session.window_last_character)
    } else {
        None
    };
    let append_unidentified = resources
        .config
        .profile
        .hotkey_logged_out_unidentified_cycle
        && resources
            .config
            .profile
            .hotkey_logged_out_unidentified_cycle_mode
            == LoggedOutUnidentifiedCycleMode::AppendToGroups;

    match command {
        CycleCommand::Forward(group) => {
            if append_unidentified {
                resources.cycle.cycle_forward_with_unidentified(
                    group,
                    logged_out_map,
                    &resources.session.window_last_character,
                    resources.config.profile.hotkey_cycle_reset_index,
                )
            } else {
                resources.cycle.cycle_forward(
                    group,
                    logged_out_map,
                    resources.config.profile.hotkey_cycle_reset_index,
                )
            }
        }
        CycleCommand::Backward(group) => {
            if append_unidentified {
                resources.cycle.cycle_backward_with_unidentified(
                    group,
                    logged_out_map,
                    &resources.session.window_last_character,
                    resources.config.profile.hotkey_cycle_reset_index,
                )
            } else {
                resources.cycle.cycle_backward(
                    group,
                    logged_out_map,
                    resources.config.profile.hotkey_cycle_reset_index,
                )
            }
        }
        CycleCommand::LoggedOutUnidentifiedForward => {
            if resources
                .config
                .profile
                .hotkey_logged_out_unidentified_cycle
                && resources
                    .config
                    .profile
                    .hotkey_logged_out_unidentified_cycle_mode
                    == LoggedOutUnidentifiedCycleMode::SeparateHotkeys
            {
                resources
                    .cycle
                    .cycle_unidentified_logged_out_forward(&resources.session.window_last_character)
            } else {
                None
            }
        }
        CycleCommand::LoggedOutUnidentifiedBackward => {
            if resources
                .config
                .profile
                .hotkey_logged_out_unidentified_cycle
                && resources
                    .config
                    .profile
                    .hotkey_logged_out_unidentified_cycle_mode
                    == LoggedOutUnidentifiedCycleMode::SeparateHotkeys
            {
                resources.cycle.cycle_unidentified_logged_out_backward(
                    &resources.session.window_last_character,
                )
            } else {
                None
            }
        }
        CycleCommand::CharacterHotkey(binding) => {
            debug!(binding = %binding.display_name(), "Received direct-source hotkey command");

            // Find the group of typed sources sharing this hotkey.
            if let Some(source_group) = hotkey_groups.get(binding) {
                debug!(
                    binding = %binding.display_name(),
                    group = ?source_group,
                    "Found hotkey group"
                );

                // Delegate logic to CycleState
                resources
                    .cycle
                    .activate_next_in_group(source_group, logged_out_map)
            } else {
                warn!(
                    binding = %binding.display_name(),
                    available_groups = hotkey_groups.len(),
                    "Direct-source hotkey binding not found in groups - this shouldn't happen!"
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
            // Identify focused window to determine which source to skip.
            let active_window = active_tracked_source_window(ctx, &resources.eve_clients);

            if let Some(window) = active_window {
                if let Some(thumbnail) = resources.eve_clients.get_mut(&window) {
                    let Some(identity) = thumbnail.effective_source_identity() else {
                        warn!("Cannot toggle skip: Focused window has no source identity");
                        return None;
                    };
                    let is_skipped = resources.cycle.toggle_skip(&identity);
                    info!(identity = ?identity, skipped = is_skipped, "Toggled skip status");

                    // Force redraw of border to show/hide indicator
                    let focused = thumbnail.state.is_focused();
                    let display_config = resources.config.build_display_config();
                    if let Err(e) =
                        thumbnail.border(&display_config, focused, is_skipped, font_renderer)
                    {
                        warn!(identity = ?identity, error = %e, "Failed to update border after toggle skip");
                    }
                } else {
                    warn!("Focused window not found in client list");
                }
            } else {
                warn!("Cannot toggle skip: No tracked window focused");
            }
            None
        }
        CycleCommand::TogglePreviews => {
            restore_interrupted_group_drag(ctx.conn, resources, "preview visibility toggle");
            resources.config.runtime_hidden = !resources.config.runtime_hidden;
            info!(
                hidden = resources.config.runtime_hidden,
                "Toggled previews visibility"
            );

            // Force visibility update for all known thumbnails
            let display_config = resources.config.build_display_config();
            for thumbnail in resources.eve_clients.values_mut() {
                // When revealing, respect per-source overrides: force-hidden thumbnails stay hidden.
                let should_render = display_config
                    .settings_for(
                        thumbnail.source_kind(),
                        thumbnail.effective_character_name(),
                    )
                    .and_then(|s| s.override_render_preview)
                    .unwrap_or(display_config.enabled);

                let target_visible = !resources.config.runtime_hidden && should_render;

                if let Err(e) = thumbnail.visibility(target_visible) {
                    warn!(source = %thumbnail.character_name, error = %e, "Failed to update visibility after toggle");
                } else if target_visible {
                    // Force update to ensure content is drawn if revealed
                    let _ = thumbnail.update(&display_config, font_renderer);
                }
            }
            None
        }
    }
}
