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
use crate::common::ipc::{BootstrapMessage, ConfigMessage, DaemonMessage, ThumbnailSpatialUpdate};
use crate::common::types::SourceIdentity;
use crate::config::DaemonConfig;
use crate::config::profile::LoggedOutUnidentifiedCycleMode;
use crate::input::listener::{self, CycleCommand, TimestampedCommand};
use crate::x11::{AppContext, CachedAtoms};
use ipc_channel::ipc::{self, IpcReceiver, IpcSender};

use super::activation::{self, ActivationOrigin};
use super::cycle_state::{CycleActivation, CycleState};
use super::dispatcher::{EventContext, handle_event};
use super::font;
use super::group_drag::GroupDragState;
use super::session_state::SessionState;
use super::source_registry::SourceRegistry;
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
    sources: SourceRegistry,
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

fn tracked_source_window_for_window(
    ctx: &AppContext<'_>,
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    sources: Option<&SourceRegistry>,
    window: Window,
) -> Option<Window> {
    super::focus::resolve_window(ctx, thumbnails, sources, window)
        .ok()
        .and_then(super::focus::FocusOwner::source)
}

fn active_tracked_source_window(
    ctx: &AppContext<'_>,
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    sources: Option<&SourceRegistry>,
) -> Option<Window> {
    let active_window = crate::x11::get_active_window(ctx.conn, ctx.screen, ctx.atoms)
        .ok()
        .flatten()?;

    tracked_source_window_for_window(ctx, thumbnails, sources, active_window)
}

enum DaemonControlMessage {
    Config(ConfigMessage),
    ManagerDisconnected,
}

fn apply_thumbnail_moves(
    resources: &mut DaemonResources<'_>,
    display_config: &crate::config::DisplayConfig,
    font_renderer: &font::FontRenderer,
    updates: Vec<ThumbnailSpatialUpdate>,
) {
    for update in updates {
        let thumbnail_opt = resources.eve_clients.values_mut().find(|thumbnail| {
            thumbnail.effective_source_identity().as_ref() == Some(&update.source)
        });

        if let Some(thumbnail) = thumbnail_opt {
            if thumbnail.current_position == update.position
                && thumbnail.dimensions == update.dimensions
            {
                debug!(name = %update.source.name, "Thumbnail move ignored: position/size unchanged");
                continue;
            }

            if let Err(error) = thumbnail.reposition(update.position.x, update.position.y) {
                error!(name = %update.source.name, %error, "Failed to reposition thumbnail");
            }
            let resized = thumbnail.dimensions != update.dimensions;
            if let Err(error) = thumbnail.resize(update.dimensions.width, update.dimensions.height)
            {
                error!(name = %update.source.name, %error, "Failed to resize thumbnail");
            } else if resized {
                // Resizing replaces the overlay pixmap and discards the window contents.
                // Rebuild both now: idle sources may never send another damage notification.
                let result = if thumbnail.state.is_minimized() {
                    thumbnail.update(display_config, font_renderer)
                } else {
                    thumbnail.border(
                        display_config,
                        thumbnail.state.is_focused(),
                        resources
                            .cycle
                            .is_skipped(thumbnail.effective_source_identity().as_ref()),
                        font_renderer,
                    )
                };
                if let Err(error) = result {
                    error!(name = %update.source.name, %error, "Failed to redraw thumbnail after resize");
                    // Preserve the content attempt even if rebuilding the overlay failed.
                    if let Err(error) = thumbnail.update(display_config, font_renderer) {
                        error!(name = %update.source.name, %error, "Failed to repaint thumbnail after resize");
                    }
                }
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

fn initialize_thumbnail_borders(
    eve_clients: &mut HashMap<Window, Thumbnail<'_>>,
    cycle_state: &CycleState,
    config: &crate::config::DisplayConfig,
    font_renderer: &font::FontRenderer,
) {
    for (window, thumbnail) in eve_clients.iter_mut() {
        // Check if this window currently has focus
        let is_focused = false; // Observed focus is reconciled on entry to the event loop.

        // Update state and draw appropriate border
        if !thumbnail.state.is_minimized() {
            thumbnail.state = crate::common::types::ThumbnailState::Normal {
                focused: is_focused,
            };
        }
        if let Err(e) = thumbnail.border(
            config,
            is_focused,
            cycle_state.is_skipped(thumbnail.effective_source_identity().as_ref()),
            font_renderer,
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
    // Root focus events replace idle polling at root/None, so this subscription is required.
    super::focus::select_root_focus_changes(&conn)?;

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
    daemon_config
        .profile
        .validate_cycle_group_names()
        .map_err(|err| anyhow::anyhow!(err))?;
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

fn cycle_hotkeys(
    profile: &crate::config::profile::Profile,
) -> Vec<(CycleCommand, crate::config::HotkeyBinding)> {
    let mut cycle_hotkeys: Vec<(CycleCommand, crate::config::HotkeyBinding)> = profile
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

    if let Some(fwd) = &profile.hotkey_logged_out_unidentified_cycle_forward {
        cycle_hotkeys.push((CycleCommand::LoggedOutUnidentifiedForward, fwd.clone()));
    }
    if let Some(bwd) = &profile.hotkey_logged_out_unidentified_cycle_backward {
        cycle_hotkeys.push((CycleCommand::LoggedOutUnidentifiedBackward, bwd.clone()));
    }

    cycle_hotkeys
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
    let cycle_hotkeys = cycle_hotkeys(&daemon_config.profile);

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
    display_config: crate::config::DisplayConfig,
    atoms: &CachedAtoms,
    formats: &crate::x11::CachedFormats,
    font_renderer: crate::daemon::font::FontRenderer,
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

    // One timer services activation probes, timeout, and visibility hysteresis.
    let focus_timer = tokio::time::sleep(tokio::time::Duration::from_secs(86400));
    tokio::pin!(focus_timer);
    let mut armed_deadline = None;
    let mut reconcile_startup = true;

    loop {
        let mut batch_full = true;
        // Scope ctx to allow mutable borrow of font_renderer later
        {
            // Construct AppContext for this iteration
            let ctx = AppContext {
                conn,
                screen,
                atoms,
                formats,
            };

            // Service due focus work before draining: its synchronous focus queries can move
            // newly arrived events into x11rb's queue, where socket readiness cannot see them.
            if reconcile_startup
                || activation::next_deadline(&resources.session)
                    .is_some_and(|deadline| std::time::Instant::now() >= deadline)
            {
                activation::reconcile(
                    &mut EventContext {
                        app_ctx: &ctx,
                        daemon_config: &mut resources.config,
                        eve_clients: &mut resources.eve_clients,
                        session_state: &mut resources.session,
                        cycle_state: &mut resources.cycle,
                        sources: &mut resources.sources,
                        group_drag_state: &mut resources.group_drag,
                        status_tx: &status_tx,
                        font_renderer: &font_renderer,
                        display_config: &display_config,
                    },
                    std::time::Instant::now(),
                );
                reconcile_startup = false;
            }

            // Bound each batch so continuous damage/input cannot starve focus deadlines.
            for _ in 0..256 {
                let Some(event) = ctx
                    .conn
                    .poll_for_event()
                    .context("Failed to poll for X11 event")?
                else {
                    batch_full = false;
                    break;
                };
                // Scope the mutable borrows for event handling
                {
                    let mut context = EventContext {
                        app_ctx: &ctx,
                        daemon_config: &mut resources.config,
                        eve_clients: &mut resources.eve_clients,
                        session_state: &mut resources.session,
                        cycle_state: &mut resources.cycle,
                        sources: &mut resources.sources,
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
            current_windows.extend(resources.sources.windows());

            // allow hotkeys for thumbnail overlay, source, and known parent/frame windows
            for thumbnail in resources.eve_clients.values() {
                current_windows.insert(thumbnail.window());
                current_windows.insert(thumbnail.src());
                if let Some(parent) = thumbnail.parent() {
                    current_windows.insert(parent);
                }
            }

            current_windows.retain(|window| {
                *window > 1
                    && !conn
                        .setup()
                        .roots
                        .iter()
                        .any(|screen| screen.root == *window)
            });

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

        let deadline = activation::next_deadline(&resources.session);
        if deadline != armed_deadline {
            if let Some(deadline) = deadline {
                focus_timer
                    .as_mut()
                    .reset(tokio::time::Instant::from_std(deadline));
            }
            armed_deadline = deadline;
        }

        tokio::select! {
            biased;

            // 1. Service due focus work (HIGHEST PRIORITY)
            // Precedes another hotkey, even under sustained input.
            () = &mut focus_timer, if armed_deadline.is_some() => { continue; }

            // 2. Handle Manager IPC commands
            // Control is rare and cheap; it precedes input so sustained hotkeys or X traffic cannot
            // starve shutdown.
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
                    DaemonControlMessage::Config(ConfigMessage::InitialConfig(_)) => {
                        return Err(anyhow::anyhow!(
                            "Received InitialConfig after daemon initialization"
                        ));
                    }

                    DaemonControlMessage::Config(ConfigMessage::ThumbnailMoves {
                        updates,
                    }) => {
                        debug!(update_count = updates.len(), "Received thumbnail move batch");

                        apply_thumbnail_moves(&mut resources, &display_config, &font_renderer, updates);
                    }
                }
            }

            // 3. Send Heartbeat (due once per interval; must not be starved by input)
            _ = heartbeat_interval.tick() => {
                for thumbnail in resources.eve_clients.values_mut() {
                    thumbnail.report_damage_metrics();
                }
                if let Err(e) = status_tx.send(DaemonMessage::Heartbeat) {
                    error!(error = %e, "Failed to send heartbeat to Manager");
                    // If we can't send heartbeat, manager might be dead.
                    // We'll let the IPC config channel failure handle termination.
                }
            }

            // 4. Handle legacy SIGUSR1 notifications
            _ = sigusr1.recv() => {
                info!("SIGUSR1 received; configuration changes require a Manager-driven daemon restart");
                let _ = status_tx.send(DaemonMessage::Status(
                    "SIGUSR1 ignored: use Save & Apply to reload configuration".to_string(),
                ));
            }

            // 5. Handle Hotkey Commands
            // Checked before X11 events to minimize latency and prevent XWayland grab conflicts
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
                                Some(&resources.sources),
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

                        activation::begin(&mut EventContext {
                            app_ctx: &ctx,
                            daemon_config: &mut resources.config,
                            eve_clients: &mut resources.eve_clients,
                            session_state: &mut resources.session,
                            cycle_state: &mut resources.cycle,
                            sources: &mut resources.sources,
                            group_drag_state: &mut resources.group_drag,
                            status_tx: &status_tx,
                            font_renderer: &font_renderer,
                            display_config: &display_config,
                        }, window, source_identity.as_ref(), timestamp, ActivationOrigin::Hotkey);
                    } else {
                        warn!("No window to activate via hotkey");
                    }
                } else {
                    info!(hotkey_require_eve_focus = resources.config.profile.hotkey_require_eve_focus, "Hotkey ignored, tracked source window not focused (hotkey_require_eve_focus enabled)");
                }


            }

            // 6. Handle X11 Events
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

            // x11rb may already hold events after the socket becomes empty.
            () = tokio::task::yield_now(), if batch_full => {}

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
        Ok(ConfigMessage::InitialConfig(config)) => *config,
        Ok(ConfigMessage::ThumbnailMoves { .. }) => {
            return Err(anyhow::anyhow!(
                "Expected InitialConfig on startup, got ThumbnailMoves"
            ));
        }
        Ok(ConfigMessage::Shutdown) => {
            return Err(anyhow::anyhow!(
                "Expected InitialConfig on startup, got Shutdown"
            ));
        }
        Err(e) => return Err(anyhow::anyhow!("Failed to receive initial config: {}", e)),
    };
    debug!("Received initial configuration");

    // 3. Initialize State from Config
    let (mut daemon_config, config, mut session_state, cycle_state) =
        initialize_state(screen, initial_config).context("Failed to initialize state")?;

    // 3. Setup Signal Handlers
    // We do this here as it requires async runtime context
    let sigusr1 = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
        .context("Failed to register SIGUSR1 handler")?;

    debug!("Registered legacy SIGUSR1 handler");

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
    let mut sources = SourceRegistry::default();
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
            &mut sources,
            &status_tx,
        )
        .context("Failed to get initial list of tracked source windows")?;
    }

    initialize_thumbnail_borders(&mut eve_clients, &cycle_state, &config, &font_renderer);

    // 8. Run Main Event Loop
    let resources = DaemonResources {
        config: daemon_config,
        session: session_state,
        cycle: cycle_state,
        sources,
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

fn handle_cycle_command<'a>(
    command: &CycleCommand,
    resources: &mut DaemonResources<'a>,
    ctx: &AppContext<'a>,
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
        .hotkey_logged_out_unidentified_cycle_mode
        == LoggedOutUnidentifiedCycleMode::AppendToGroups;

    match command {
        CycleCommand::Forward(group) => {
            if append_unidentified {
                resources.cycle.cycle_forward_with_unidentified(
                    &resources.sources,
                    group,
                    logged_out_map,
                    &resources.session.window_last_character,
                    resources.config.profile.hotkey_cycle_reset_index,
                )
            } else {
                resources.cycle.cycle_forward(
                    &resources.sources,
                    group,
                    logged_out_map,
                    resources.config.profile.hotkey_cycle_reset_index,
                )
            }
        }
        CycleCommand::Backward(group) => {
            if append_unidentified {
                resources.cycle.cycle_backward_with_unidentified(
                    &resources.sources,
                    group,
                    logged_out_map,
                    &resources.session.window_last_character,
                    resources.config.profile.hotkey_cycle_reset_index,
                )
            } else {
                resources.cycle.cycle_backward(
                    &resources.sources,
                    group,
                    logged_out_map,
                    resources.config.profile.hotkey_cycle_reset_index,
                )
            }
        }
        CycleCommand::LoggedOutUnidentifiedForward => {
            resources.cycle.cycle_unidentified_logged_out_forward(
                &resources.sources,
                &resources.session.window_last_character,
            )
        }
        CycleCommand::LoggedOutUnidentifiedBackward => {
            resources.cycle.cycle_unidentified_logged_out_backward(
                &resources.sources,
                &resources.session.window_last_character,
            )
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
                resources.cycle.activate_next_in_group(
                    &resources.sources,
                    source_group,
                    logged_out_map,
                )
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
            let Some(window) =
                active_tracked_source_window(ctx, &resources.eve_clients, Some(&resources.sources))
            else {
                warn!("Cannot toggle skip: No tracked window focused");
                return None;
            };
            // Remembered identity remains usable even when logged-out cycling is disabled.
            let Some(identity) = resources
                .sources
                .identity(window, Some(&resources.session.window_last_character))
                .filter(|identity| !identity.name.is_empty())
            else {
                warn!("Cannot toggle skip: Focused window has no source identity");
                return None;
            };
            let is_skipped = resources.cycle.toggle_skip(&identity);
            info!(identity = ?identity, skipped = is_skipped, "Toggled skip status");

            if let Some(thumbnail) = resources.eve_clients.get_mut(&window) {
                let focused = thumbnail.state.is_focused();
                let display_config = resources.config.build_display_config();
                if let Err(e) =
                    thumbnail.border(&display_config, focused, is_skipped, font_renderer)
                {
                    warn!(identity = ?identity, error = %e, "Failed to update border after toggle skip");
                }
            }
            None
        }
        CycleCommand::TogglePreviews => {
            restore_interrupted_group_drag(ctx.conn, resources, "preview visibility toggle");
            let display_config = resources.config.build_display_config();
            crate::daemon::handlers::state::toggle_previews(&mut EventContext {
                app_ctx: ctx,
                daemon_config: &mut resources.config,
                eve_clients: &mut resources.eve_clients,
                session_state: &mut resources.session,
                cycle_state: &mut resources.cycle,
                sources: &mut resources.sources,
                group_drag_state: &mut resources.group_drag,
                status_tx,
                font_renderer,
                display_config: &display_config,
            });
            None
        }
    }
}

#[cfg(test)]
mod tests {
    //! Run display tests serially on isolated Xvfb with EPM_X11_TESTS=1 and an outer timeout.
    use super::*;
    use crate::common::types::{Dimensions, PreviewMode};
    use crate::config::profile::{CycleSlot, Profile};
    use crate::daemon::font::FontRenderer;
    use crate::daemon::source_registry::TrackedSource;
    use crate::x11::CachedFormats;
    use x11rb::wrapper::ConnectionExt as _;

    #[test]
    fn unidentified_hotkeys_are_registered_in_both_modes() {
        use crate::config::HotkeyBinding;
        for mode in [
            LoggedOutUnidentifiedCycleMode::SeparateHotkeys,
            LoggedOutUnidentifiedCycleMode::AppendToGroups,
        ] {
            let mut profile = Profile {
                hotkey_logged_out_unidentified_cycle_mode: mode,
                hotkey_logged_out_unidentified_cycle_forward: Some(HotkeyBinding::new(
                    16, false, false, false, false,
                )),
                hotkey_logged_out_unidentified_cycle_backward: Some(HotkeyBinding::new(
                    17, false, false, false, false,
                )),
                ..Profile::default()
            };
            profile.cycle_groups[0].hotkey_forward =
                Some(HotkeyBinding::new(18, false, false, false, false));
            let keys = cycle_hotkeys(&profile);
            assert_eq!(keys.len(), 3);
            assert!(
                matches!(&keys[0].0, CycleCommand::Forward(name) if name == &profile.cycle_groups[0].name)
            );
            assert!(matches!(
                keys[1].0,
                CycleCommand::LoggedOutUnidentifiedForward
            ));
            assert_eq!(
                Some(&keys[1].1),
                profile
                    .hotkey_logged_out_unidentified_cycle_forward
                    .as_ref()
            );
            assert!(matches!(
                keys[2].0,
                CycleCommand::LoggedOutUnidentifiedBackward
            ));
            assert_eq!(
                Some(&keys[2].1),
                profile
                    .hotkey_logged_out_unidentified_cycle_backward
                    .as_ref()
            );
            profile.hotkey_logged_out_unidentified_cycle_forward = None;
            let keys = cycle_hotkeys(&profile);
            assert_eq!(keys.len(), 2);
            assert_eq!(keys[1].0, CycleCommand::LoggedOutUnidentifiedBackward);
            profile.hotkey_logged_out_unidentified_cycle_backward = None;
            assert_eq!(cycle_hotkeys(&profile).len(), 1);
            profile.cycle_groups[0].hotkey_forward = None;
            assert!(cycle_hotkeys(&profile).is_empty());
        }
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn unidentified_commands_work_in_both_modes() {
        with_x11(|ctx| {
            for mode in [
                LoggedOutUnidentifiedCycleMode::SeparateHotkeys,
                LoggedOutUnidentifiedCycleMode::AppendToGroups,
            ] {
                with_daemon(ctx, |resources, font, tx| {
                    resources
                        .config
                        .profile
                        .hotkey_logged_out_unidentified_cycle_mode = mode;
                    let alice = SourceIdentity::eve("Alice");
                    resources
                        .sources
                        .register(10, TrackedSource::from(alice.clone()));
                    resources.sources.register(20, TrackedSource::eve(""));
                    resources.sources.register(30, TrackedSource::eve(""));
                    resources.sources.register(40, TrackedSource::eve(""));
                    resources
                        .session
                        .window_last_character
                        .insert(40, "Bob".into());
                    let keys = HashMap::new();
                    let mut run = |command| {
                        let target =
                            handle_cycle_command(&command, resources, ctx, font, tx, &keys);
                        if let Some((window, identity)) = &target {
                            // The main loop records the current window after activation.
                            assert!(resources.cycle.set_current_by_window_with_identity(
                                &resources.sources,
                                *window,
                                identity.as_ref()
                            ));
                        }
                        target
                    };
                    assert_eq!(
                        run(CycleCommand::LoggedOutUnidentifiedForward),
                        Some((20, None))
                    );
                    assert_eq!(
                        run(CycleCommand::LoggedOutUnidentifiedForward),
                        Some((30, None))
                    );
                    assert_eq!(
                        run(CycleCommand::LoggedOutUnidentifiedForward),
                        Some((20, None))
                    );
                    assert_eq!(
                        run(CycleCommand::LoggedOutUnidentifiedBackward),
                        Some((30, None))
                    );
                    let group = resources.config.profile.cycle_groups[0].name.clone();
                    for (command, unidentified_window) in [
                        (CycleCommand::Forward(group.clone()), 20),
                        (CycleCommand::Backward(group), 30),
                    ] {
                        assert!(resources.cycle.set_current_by_window_with_identity(
                            &resources.sources,
                            10,
                            Some(&alice)
                        ));
                        let expected = if mode == LoggedOutUnidentifiedCycleMode::AppendToGroups {
                            Some((unidentified_window, None))
                        } else {
                            Some((10, Some(alice.clone())))
                        };
                        assert_eq!(
                            handle_cycle_command(&command, resources, ctx, font, tx, &keys),
                            expected
                        );
                    }
                });
            }
        });
    }

    #[test]
    fn cycle_group_startup_rejects_duplicates_and_preserves_distinct_orders() {
        use crate::config::profile::CycleGroup;
        let profile = Profile {
            cycle_groups: [("Fleet", "Alice"), ("Other", "Bob")]
                .into_iter()
                .map(|(name, character)| CycleGroup {
                    name: name.into(),
                    cycle_list: vec![CycleSlot::Eve(character.into())],
                    hotkey_forward: None,
                    hotkey_backward: None,
                })
                .collect(),
            ..Profile::default()
        };
        let mut config = DaemonConfig {
            profile,
            character_thumbnails: HashMap::new(),
            custom_source_thumbnails: HashMap::new(),
            profile_hotkeys: HashMap::new(),
            runtime_hidden: false,
        };
        let (_, _, _, mut cycle) = initialize_state(&Screen::default(), config.clone()).unwrap();
        let mut sources = SourceRegistry::default();
        sources.register(1, TrackedSource::eve("Alice"));
        sources.register(2, TrackedSource::eve("Bob"));
        assert_eq!(
            cycle.cycle_forward(&sources, "Fleet", None, false),
            Some((1, Some(SourceIdentity::eve("Alice"))))
        );
        assert_eq!(
            cycle.cycle_forward(&sources, "Other", None, false),
            Some((2, Some(SourceIdentity::eve("Bob"))))
        );
        for invalid in ["Fleet", "fleet", "", " Fleet "] {
            config.profile.cycle_groups[1].name = invalid.into();
            assert!(initialize_state(&Screen::default(), config.clone()).is_err());
        }
    }

    fn with_x11(test: impl FnOnce(&AppContext<'_>)) {
        assert_eq!(
            std::env::var("EPM_X11_TESTS").as_deref(),
            Ok("1"),
            "run display tests with EPM_X11_TESTS=1 under an isolated Xvfb server"
        );
        let (conn, screen_number) = x11rb::connect(None).unwrap();
        let screen = &conn.setup().roots[screen_number];
        let atoms = CachedAtoms::new(&conn).unwrap();
        let formats = CachedFormats::new(&conn, screen).unwrap();
        // Fixture windows belong to this connection and disappear when it closes.
        test(&AppContext {
            conn: &conn,
            screen,
            atoms: &atoms,
            formats: &formats,
        });
    }

    fn with_daemon<'a>(
        ctx: &AppContext<'a>,
        test: impl FnOnce(&mut DaemonResources<'a>, &FontRenderer, &IpcSender<DaemonMessage>),
    ) {
        let mut profile = Profile {
            thumbnail_enabled: false,
            ..Profile::default()
        };
        profile.cycle_groups[0].cycle_list = vec![
            CycleSlot::Eve("Alice".into()),
            CycleSlot::Source("Alice".into()),
        ];
        let cycle = CycleState::new(profile.cycle_groups.clone());
        let config = DaemonConfig {
            profile,
            character_thumbnails: HashMap::new(),
            custom_source_thumbnails: HashMap::new(),
            profile_hotkeys: HashMap::new(),
            runtime_hidden: false,
        };
        let mut resources = DaemonResources {
            config,
            cycle,
            sources: SourceRegistry::default(),
            session: SessionState::new(),
            eve_clients: HashMap::new(),
            group_drag: GroupDragState::default(),
        };
        let font = FontRenderer::resolve_from_config(ctx.conn, "sans-serif", 12.0).unwrap();
        let (tx, _rx) = ipc::channel().unwrap();
        test(&mut resources, &font, &tx);
    }

    fn window(ctx: &AppContext<'_>, parent: Window) -> Window {
        let id = ctx.conn.generate_id().unwrap();
        ctx.conn
            .create_window(
                ctx.screen.root_depth,
                id,
                parent,
                0,
                0,
                400,
                300,
                0,
                WindowClass::INPUT_OUTPUT,
                ctx.screen.root_visual,
                &CreateWindowAux::new(),
            )
            .unwrap()
            .check()
            .unwrap();
        ctx.conn.map_window(id).unwrap().check().unwrap();
        id
    }

    fn focus(ctx: &AppContext<'_>, window: Option<Window>) {
        ctx.conn
            .change_property32(
                PropMode::REPLACE,
                ctx.screen.root,
                ctx.atoms.net_active_window,
                AtomEnum::WINDOW,
                &window.into_iter().collect::<Vec<_>>(),
            )
            .unwrap()
            .check()
            .unwrap();
    }

    fn preview_pixels(ctx: &AppContext<'_>, thumbnail: &Thumbnail<'_>) -> Vec<u32> {
        let reply = ctx
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                thumbnail.window(),
                0,
                0,
                thumbnail.dimensions.width,
                thumbnail.dimensions.height,
                u32::MAX,
            )
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(
            reply.data.len(),
            usize::from(thumbnail.dimensions.width) * usize::from(thumbnail.dimensions.height) * 4
        );
        reply
            .data
            .chunks_exact(4)
            .map(|bytes| {
                let bytes = bytes.try_into().unwrap();
                let pixel = if ctx.conn.setup().image_byte_order == ImageOrder::LSB_FIRST {
                    u32::from_le_bytes(bytes)
                } else {
                    u32::from_be_bytes(bytes)
                };
                pixel & 0xFFFFFF
            })
            .collect()
    }

    fn check_manager_resize(mode: PreviewMode, minimized: bool, hidden: bool) {
        use crate::common::types::{Position, ThumbnailState};

        with_x11(|ctx| {
            // Real damage is subscribed, but no events are dispatched in this test.
            ctx.conn
                .damage_query_version(1, 1)
                .unwrap()
                .reply()
                .unwrap();
            with_daemon(ctx, |resources, font, _| {
                resources.config.profile.thumbnail_enabled = true;
                resources.config.profile.thumbnail_opacity = 100;
                resources.config.profile.thumbnail_active_border = true;
                resources.config.profile.thumbnail_active_border_size = 3;
                resources.config.profile.thumbnail_active_border_color = "#00FF00".into();
                resources.config.profile.thumbnail_inactive_border = false;
                resources.config.profile.thumbnail_text_color = "#FFFFFF".into();
                resources.config.profile.thumbnail_text_x = 10;
                resources.config.profile.thumbnail_text_y = 10;
                let display = resources.config.build_display_config();
                let identity = SourceIdentity::eve("Alice");
                let src = window(ctx, ctx.screen.root);
                let gc = ctx.conn.generate_id().unwrap();
                ctx.conn
                    .create_gc(gc, src, &CreateGCAux::new().foreground(0x0000FF))
                    .unwrap()
                    .check()
                    .unwrap();
                ctx.conn
                    .poly_fill_rectangle(
                        src,
                        gc,
                        &[Rectangle {
                            x: 0,
                            y: 0,
                            width: 400,
                            height: 300,
                        }],
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                let position = Position::new(600, 0);
                let mut thumbnail = Thumbnail::new(
                    ctx,
                    identity.kind,
                    identity.name.clone(),
                    None,
                    src,
                    &display,
                    font,
                    Some(position),
                    Dimensions::new(160, 100),
                    mode.clone(),
                    false,
                )
                .unwrap();
                thumbnail.state = if minimized {
                    ThumbnailState::Minimized
                } else {
                    ThumbnailState::Normal { focused: true }
                };
                if !minimized {
                    thumbnail.border(&display, true, true, font).unwrap();
                }
                thumbnail.update(&display, font).unwrap();
                resources
                    .sources
                    .register(src, TrackedSource::from(identity.clone()));
                resources.cycle.toggle_skip(&identity);
                resources.eve_clients.insert(src, thumbnail);

                for (dimensions, focused) in [
                    (Dimensions::new(240, 160), true),
                    (Dimensions::new(120, 80), false),
                ] {
                    let thumbnail = resources.eve_clients.get_mut(&src).unwrap();
                    if !minimized {
                        thumbnail.state = ThumbnailState::Normal { focused };
                    }
                    if !focused {
                        resources.cycle.toggle_skip(&identity);
                    }
                    if hidden {
                        thumbnail
                            .set_visibility_blocked(true, &display, font)
                            .unwrap();
                    }
                    apply_thumbnail_moves(
                        resources,
                        &display,
                        font,
                        vec![ThumbnailSpatialUpdate::new(
                            identity.clone(),
                            position,
                            dimensions,
                        )],
                    );
                    let thumbnail = resources.eve_clients.get_mut(&src).unwrap();
                    assert_eq!(thumbnail.dimensions, dimensions);
                    assert_eq!(resources.cycle.is_skipped(Some(&identity)), focused);
                    if hidden {
                        assert!(!thumbnail.is_visible());
                        thumbnail
                            .set_visibility_blocked(false, &display, font)
                            .unwrap();
                    }
                    let actual = preview_pixels(ctx, thumbnail);
                    let w = usize::from(dimensions.width);
                    let h = usize::from(dimensions.height);
                    let base = if minimized {
                        0
                    } else if matches!(mode, PreviewMode::Live) {
                        0x0000FF
                    } else {
                        0x00FFFF
                    };
                    assert_eq!(
                        actual[(h / 4) * w + w / 2],
                        base,
                        "resize must repaint interior: {mode:?}, minimized={minimized}, hidden={hidden}, {dimensions:?}"
                    );
                    assert_eq!(
                        actual[w + 1],
                        if focused && !minimized {
                            0x00FF00
                        } else {
                            base
                        },
                        "resize must restore current border"
                    );
                    assert_eq!(
                        actual[(3 * h / 4) * w + 3 * w / 4],
                        if focused && !minimized {
                            0xFF0000
                        } else {
                            base
                        },
                        "resize must restore current skip indicator"
                    );
                    assert!(
                        (10..30.min(h)).any(|y| {
                            actual[y * w + 10..y * w + 80.min(w)].iter().any(|pixel| {
                                pixel & 0xFF0000 != 0
                                    && pixel & 0x00FF00 != 0
                                    && pixel & 0x0000FF != 0
                            })
                        }),
                        "resize must restore name text"
                    );

                    // Compare the complete output against an explicit repaint at the new size.
                    if !minimized {
                        thumbnail.border(&display, focused, focused, font).unwrap();
                    }
                    thumbnail.update(&display, font).unwrap();
                    assert_eq!(actual, preview_pixels(ctx, thumbnail));
                }

                let before = preview_pixels(ctx, &resources.eve_clients[&src]);
                let dimensions = resources.eve_clients[&src].dimensions;
                apply_thumbnail_moves(
                    resources,
                    &display,
                    font,
                    vec![ThumbnailSpatialUpdate::new(
                        identity.clone(),
                        position,
                        Dimensions::new(0, 80),
                    )],
                );
                assert_eq!(resources.eve_clients[&src].dimensions, dimensions);
                assert_eq!(preview_pixels(ctx, &resources.eve_clients[&src]), before);

                // A move without resizing must not capture a newer source frame.
                ctx.conn
                    .change_gc(gc, &ChangeGCAux::new().foreground(0xFF0000))
                    .unwrap()
                    .check()
                    .unwrap();
                ctx.conn
                    .poly_fill_rectangle(
                        src,
                        gc,
                        &[Rectangle {
                            x: 0,
                            y: 0,
                            width: 400,
                            height: 300,
                        }],
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                apply_thumbnail_moves(
                    resources,
                    &display,
                    font,
                    vec![ThumbnailSpatialUpdate::new(
                        identity,
                        Position::new(650, 0),
                        dimensions,
                    )],
                );
                assert_eq!(preview_pixels(ctx, &resources.eve_clients[&src]), before);
                ctx.conn.free_gc(gc).unwrap().check().unwrap();
            });
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn startup_preserves_detected_minimized_preview() {
        with_x11(|ctx| {
            use x11rb::protocol::damage::ConnectionExt as _;
            ctx.conn
                .damage_query_version(1, 1)
                .unwrap()
                .reply()
                .unwrap();
            with_daemon(ctx, |resources, font, _| {
                resources.config.profile.thumbnail_enabled = true;
                let display = resources.config.build_display_config();
                let src = window(ctx, ctx.screen.root);
                ctx.conn
                    .change_property32(
                        PropMode::REPLACE,
                        src,
                        ctx.atoms.net_wm_state,
                        AtomEnum::ATOM,
                        &[ctx.atoms.net_wm_state_hidden],
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                let thumbnail = super::super::window_detection::check_and_create_window(
                    ctx,
                    &resources.config,
                    &display,
                    src,
                    font,
                    &mut resources.session,
                    &resources.eve_clients,
                    Some(super::super::window_detection::WindowIdentity::new_eve(
                        "Alice".into(),
                    )),
                    1,
                )
                .unwrap()
                .unwrap();
                assert!(thumbnail.state.is_minimized());
                let before = preview_pixels(ctx, &thumbnail);
                assert!(before.contains(&0));
                assert!(
                    before.iter().any(|&pixel| pixel != 0),
                    "minimized overlay must be visible"
                );
                resources.eve_clients.insert(src, thumbnail);
                initialize_thumbnail_borders(
                    &mut resources.eve_clients,
                    &resources.cycle,
                    &display,
                    font,
                );
                let thumbnail = &resources.eve_clients[&src];
                assert!(thumbnail.state.is_minimized());
                assert!(preview_pixels(ctx, thumbnail) == before);
            });
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn manager_resize_repaints_idle_preview() {
        check_manager_resize(PreviewMode::Live, false, false);
        check_manager_resize(
            PreviewMode::Static {
                color: "#00FFFF".into(),
            },
            false,
            false,
        );
        check_manager_resize(PreviewMode::Live, true, false);
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn manager_resize_hidden_preview_is_ready_on_reveal() {
        check_manager_resize(PreviewMode::Live, false, true);
        check_manager_resize(
            PreviewMode::Static {
                color: "#00FFFF".into(),
            },
            false,
            true,
        );
        check_manager_resize(PreviewMode::Live, true, true);
    }

    fn toggle<'a>(
        ctx: &AppContext<'a>,
        resources: &mut DaemonResources<'a>,
        font: &FontRenderer,
        tx: &IpcSender<DaemonMessage>,
    ) {
        assert_eq!(
            handle_cycle_command(
                &CycleCommand::ToggleSkip,
                resources,
                ctx,
                font,
                tx,
                &HashMap::new()
            ),
            None
        );
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn toggle_skip_without_thumbnails() {
        with_x11(|ctx| {
            with_daemon(ctx, |resources, font, tx| {
                let alice = SourceIdentity::eve("Alice");
                let custom_alice = SourceIdentity::custom("Alice");
                let eve = window(ctx, ctx.screen.root);
                let custom = window(ctx, ctx.screen.root);
                let child = window(ctx, eve);
                resources
                    .sources
                    .register(eve, TrackedSource::from(alice.clone()));
                resources
                    .sources
                    .register(custom, TrackedSource::from(custom_alice.clone()));
                assert!(!resources.config.profile.thumbnail_enabled);
                assert!(resources.eve_clients.is_empty());
                focus(ctx, Some(eve));
                toggle(ctx, resources, font, tx);
                assert!(resources.cycle.is_skipped(Some(&alice)));
                assert!(!resources.cycle.is_skipped(Some(&custom_alice)));
                let group = resources.config.profile.cycle_groups[0].name.clone();
                for _ in 0..3 {
                    assert_eq!(
                        resources
                            .cycle
                            .cycle_forward(&resources.sources, &group, None, false),
                        Some((custom, Some(custom_alice.clone())))
                    );
                }
                focus(ctx, Some(child));
                toggle(ctx, resources, font, tx);
                assert!(!resources.cycle.is_skipped(Some(&alice)));
                focus(ctx, Some(custom));
                toggle(ctx, resources, font, tx);
                assert!(resources.cycle.is_skipped(Some(&custom_alice)));
                assert_eq!(
                    resources
                        .cycle
                        .cycle_forward(&resources.sources, &group, None, false),
                    Some((eve, Some(alice.clone())))
                );
                toggle(ctx, resources, font, tx);
                assert!(!resources.cycle.is_skipped(Some(&custom_alice)));

                // Remembered identity works independently of the logged-out cycling option.
                resources.config.profile.hotkey_logged_out_cycle = false;
                resources.sources.register(eve, TrackedSource::eve(""));
                resources
                    .session
                    .window_last_character
                    .insert(eve, "Alice".into());
                focus(ctx, Some(eve));
                toggle(ctx, resources, font, tx);
                assert!(resources.cycle.is_skipped(Some(&alice)));
                toggle(ctx, resources, font, tx);
                let bob = SourceIdentity::eve("Bob");
                resources
                    .sources
                    .register(eve, TrackedSource::from(bob.clone()));
                toggle(ctx, resources, font, tx);
                assert!(resources.cycle.is_skipped(Some(&bob)));
                assert!(!resources.cycle.is_skipped(Some(&alice)));
                toggle(ctx, resources, font, tx);

                let unknown = window(ctx, ctx.screen.root);
                resources.sources.register(unknown, TrackedSource::eve(""));
                let untracked = window(ctx, ctx.screen.root);
                // Stale session data must not identify an untracked window.
                resources
                    .session
                    .window_last_character
                    .insert(untracked, "Alice".into());
                for active in [Some(unknown), Some(untracked), None, Some(0)] {
                    focus(ctx, active);
                    toggle(ctx, resources, font, tx);
                    assert!(!resources.cycle.is_skipped(Some(&alice)));
                    assert!(!resources.cycle.is_skipped(Some(&bob)));
                    assert!(!resources.cycle.is_skipped(Some(&custom_alice)));
                }
                resources
                    .session
                    .window_last_character
                    .insert(unknown, String::new());
                focus(ctx, Some(unknown));
                toggle(ctx, resources, font, tx);
                assert!(
                    !resources
                        .cycle
                        .is_skipped(Some(&SourceIdentity::eve(String::new())))
                );
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn toggle_skip_preserves_thumbnail_visibility_and_focus_matching() {
        with_x11(|ctx| {
            with_daemon(ctx, |resources, font, tx| {
                resources.config.profile.thumbnail_enabled = true;
                for identity in [
                    SourceIdentity::eve("Alice"),
                    SourceIdentity::custom("Alice"),
                ] {
                    let frame = window(ctx, ctx.screen.root);
                    let src = window(ctx, frame);
                    resources
                        .sources
                        .register(src, TrackedSource::from(identity.clone()));
                    let display = resources.config.build_display_config();
                    let thumbnail = Thumbnail::new(
                        ctx,
                        identity.kind,
                        identity.name.clone(),
                        None,
                        src,
                        &display,
                        font,
                        None,
                        Dimensions::new(160, 100),
                        PreviewMode::default(),
                        false,
                    )
                    .unwrap();
                    let preview = thumbnail.window();
                    assert_eq!(thumbnail.parent(), Some(frame));
                    resources.eve_clients.insert(src, thumbnail);
                    for blocked in [false, true] {
                        resources.config.runtime_hidden = blocked;
                        resources
                            .eve_clients
                            .get_mut(&src)
                            .unwrap()
                            .set_visibility_blocked(blocked, &display, font)
                            .unwrap();
                        for active in [src, preview, frame] {
                            focus(ctx, Some(active));
                            for expected_skip in [true, false] {
                                toggle(ctx, resources, font, tx);
                                assert_eq!(
                                    resources.cycle.is_skipped(Some(&identity)),
                                    expected_skip
                                );
                                assert_eq!(resources.eve_clients[&src].is_visible(), !blocked);
                                assert_eq!(
                                    ctx.conn
                                        .get_window_attributes(preview)
                                        .unwrap()
                                        .reply()
                                        .unwrap()
                                        .map_state,
                                    if blocked {
                                        MapState::UNMAPPED
                                    } else {
                                        MapState::VIEWABLE
                                    }
                                );
                                assert_eq!(
                                    crate::x11::get_active_window(ctx.conn, ctx.screen, ctx.atoms)
                                        .unwrap(),
                                    Some(active)
                                );
                                assert_eq!(resources.config.runtime_hidden, blocked);
                            }
                        }
                    }
                    resources.eve_clients.remove(&src);
                }
            })
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn toggle_skip_survives_border_failure() {
        with_x11(|ctx| {
            with_x11(|render_ctx| {
                with_daemon(render_ctx, |resources, font, tx| {
                    let identity = SourceIdentity::eve("Alice");
                    let src = window(ctx, ctx.screen.root);
                    resources
                        .sources
                        .register(src, TrackedSource::from(identity.clone()));
                    let display = resources.config.build_display_config();
                    let thumbnail = Thumbnail::new(
                        render_ctx,
                        identity.kind,
                        identity.name.clone(),
                        None,
                        src,
                        &display,
                        font,
                        None,
                        Dimensions::new(160, 100),
                        PreviewMode::default(),
                        false,
                    )
                    .unwrap();
                    // Disconnect only the fixture's rendering client; focus lookup uses the live connection.
                    ctx.conn
                        .kill_client(thumbnail.window())
                        .unwrap()
                        .check()
                        .unwrap();
                    assert!(render_ctx.conn.get_input_focus().unwrap().reply().is_err());
                    // X11 void requests can buffer after disconnect. Exhaust that buffer
                    // with bounded writes so the handler sees a synchronous render error.
                    assert!((0..8192).any(|_| render_ctx.conn.no_operation().is_err()));
                    assert!(thumbnail.border(&display, false, true, font).is_err());
                    resources.eve_clients.insert(src, thumbnail);
                    focus(ctx, Some(src));
                    toggle(ctx, resources, font, tx);
                    assert!(resources.cycle.is_skipped(Some(&identity)));
                    toggle(ctx, resources, font, tx);
                    assert!(!resources.cycle.is_skipped(Some(&identity)));
                })
            })
        });
    }
    // Observe the actual daemon with a real passive grab and a controlled WM.
    // The fixture delivers the command after KeyPress; it does not run the listener.
    // release_at_transfer: -1 just before focus, 1 just after, 0 uses release_delay_ms.
    fn confirmed_activation_case(
        minimize: bool,
        restore_delay_ms: u64,
        release_delay_ms: u64,
        pointer_inside: bool,
        release_at_transfer: i8,
        pointer_root: bool,
        flood_events: bool,
    ) {
        use std::time::{Duration, Instant};
        use x11rb::protocol::{Event, xtest::ConnectionExt as _};
        with_x11(|ctx| {
            let mut profile = Profile {
                thumbnail_enabled: true,
                thumbnail_hide_not_focused: true,
                client_minimize_on_switch: minimize,
                hotkey_require_eve_focus: true,
                ..Profile::default()
            };
            profile.cycle_groups[0].cycle_list =
                vec![CycleSlot::Eve("Alice".into()), CycleSlot::Eve("Bob".into())];
            let group = profile.cycle_groups[0].name.clone();
            let config = DaemonConfig {
                profile,
                character_thumbnails: HashMap::new(),
                custom_source_thumbnails: HashMap::new(),
                profile_hotkeys: HashMap::new(),
                runtime_hidden: false,
            };
            let display = config.build_display_config();
            let font = FontRenderer::resolve_from_config(ctx.conn, "sans-serif", 12.0).unwrap();
            let a = window(ctx, ctx.screen.root);
            let b = window(ctx, ctx.screen.root);
            let mut cycle = CycleState::new(config.profile.cycle_groups.clone());
            let mut sources = SourceRegistry::default();
            let mut thumbnails = HashMap::new();
            for (src, name) in [(a, "Alice"), (b, "Bob")] {
                sources.register(src, TrackedSource::eve(name));
                ctx.conn
                    .change_window_attributes(
                        src,
                        &ChangeWindowAttributesAux::new().event_mask(
                            EventMask::FOCUS_CHANGE
                                | EventMask::STRUCTURE_NOTIFY
                                | EventMask::PROPERTY_CHANGE,
                        ),
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                let thumbnail = Thumbnail::new(
                    ctx,
                    crate::common::types::SourceKind::Eve,
                    name.into(),
                    None,
                    src,
                    &display,
                    &font,
                    None,
                    Dimensions::new(160, 100),
                    PreviewMode::default(),
                    false,
                )
                .unwrap();
                thumbnails.insert(src, thumbnail);
            }
            let preview_b = thumbnails[&b].window();
            cycle.set_current_by_window_with_identity(
                &sources,
                a,
                Some(&SourceIdentity::eve("Alice")),
            );
            super::super::border_update::sync_focused_borders(
                &mut thumbnails,
                &cycle,
                &display,
                &font,
                a,
                super::super::border_update::BorderFocus::Observed,
                "audit initial focus",
            );
            ctx.conn
                .configure_window(a, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE))
                .unwrap()
                .check()
                .unwrap();
            ctx.conn
                .warp_pointer(
                    x11rb::NONE,
                    ctx.screen.root,
                    0,
                    0,
                    0,
                    0,
                    if pointer_inside { 300 } else { 900 },
                    if pointer_inside { 200 } else { 700 },
                )
                .unwrap()
                .check()
                .unwrap();
            ctx.conn
                .set_input_focus(
                    if pointer_root {
                        InputFocus::POINTER_ROOT
                    } else {
                        InputFocus::PARENT
                    },
                    a,
                    0u32,
                )
                .unwrap()
                .check()
                .unwrap();
            ctx.conn
                .change_property32(
                    PropMode::REPLACE,
                    ctx.screen.root,
                    ctx.atoms.net_active_window,
                    AtomEnum::WINDOW,
                    &[a],
                )
                .unwrap()
                .check()
                .unwrap();
            if minimize {
                ctx.conn.unmap_window(b).unwrap().check().unwrap();
            }
            let resources = DaemonResources {
                config,
                session: SessionState::new(),
                cycle,
                sources,
                eve_clients: thumbnails,
                group_drag: GroupDragState::default(),
            };
            let (wm, _) = x11rb::connect(None).unwrap();
            let root = ctx.screen.root;
            let active_atom = ctx.atoms.net_active_window;
            let change_atom = ctx.atoms.wm_change_state;
            wm.change_window_attributes(
                root,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_REDIRECT),
            )
            .unwrap()
            .check()
            .unwrap();
            let (hotkey_tx, hotkey_rx) = mpsc::channel(32);
            let (config_tx, config_rx) = ipc::channel().unwrap();
            let (status_tx, _status_rx) = ipc::channel().unwrap();
            let wm_task = std::thread::spawn(move || {
                for src in [a, b] {
                    wm.change_window_attributes(
                        src,
                        &ChangeWindowAttributesAux::new().event_mask(EventMask::FOCUS_CHANGE),
                    )
                    .unwrap()
                    .check()
                    .unwrap();
                }
                wm.grab_key(
                    false,
                    root,
                    ModMask::from(0u16),
                    112,
                    GrabMode::ASYNC,
                    GrabMode::SYNC,
                )
                .unwrap()
                .check()
                .unwrap();
                wm.xtest_fake_input(KEY_PRESS_EVENT, 112, 0, root, 0, 0, 0)
                    .unwrap()
                    .check()
                    .unwrap();
                let start = Instant::now();
                let mut activation_time = None;
                let mut restored_time = None;
                let mut minimized_early = false;
                let mut saw_minimize = false;
                let mut saw_restore_request = false;
                let mut released_time = None;
                let mut command_sent = false;
                while start.elapsed() < Duration::from_secs(3) {
                    while let Some(event) = wm.poll_for_event().unwrap() {
                        match event {
                            Event::KeyPress(e) if !command_sent => {
                                command_sent = true;
                                wm.allow_events(Allow::ASYNC_KEYBOARD, e.time)
                                    .unwrap()
                                    .check()
                                    .unwrap();
                                hotkey_tx
                                    .blocking_send(TimestampedCommand {
                                        command: CycleCommand::Forward(group.clone()),
                                        timestamp: e.time,
                                    })
                                    .unwrap();
                            }
                            Event::FocusIn(e) => {
                                eprintln!(
                                    "focus IN source={} mode={:?} detail={:?}",
                                    if e.event == a { "A" } else { "B" },
                                    e.mode,
                                    e.detail
                                );
                            }
                            Event::FocusOut(e) => {
                                eprintln!(
                                    "focus OUT source={} mode={:?} detail={:?}",
                                    if e.event == a { "A" } else { "B" },
                                    e.mode,
                                    e.detail
                                );
                            }
                            Event::MapRequest(e) if e.window == b => {
                                saw_restore_request = true;
                            }
                            Event::ConfigureRequest(e) => {
                                // Accept stacking requests without restoring a hidden client.
                                wm.configure_window(
                                    e.window,
                                    &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                                )
                                .unwrap()
                                .check()
                                .unwrap();
                            }
                            Event::ClientMessage(e) if e.type_ == active_atom && e.window == b => {
                                if activation_time.is_none() {
                                    activation_time = Some(Instant::now());
                                }
                            }
                            Event::ClientMessage(e)
                                if e.type_ == change_atom
                                    && e.window == a
                                    && e.data.as_data32()[0] == 3 =>
                            {
                                saw_minimize = true;
                                let focus = wm.get_input_focus().unwrap().reply().unwrap().focus;
                                let target_map = wm
                                    .get_window_attributes(b)
                                    .unwrap()
                                    .reply()
                                    .unwrap()
                                    .map_state;
                                minimized_early |= focus != b || target_map != MapState::VIEWABLE;
                                eprintln!(
                                    "minimize={minimize}, restore_delay={restore_delay_ms}ms: old-client minimize at {:?}; target map={target_map:?}, focus_is_target={}",
                                    activation_time.map(|t: Instant| t.elapsed()),
                                    focus == b
                                );
                                wm.unmap_window(a).unwrap().check().unwrap();
                            }
                            _ => {}
                        }
                    }
                    if restored_time.is_none()
                        && activation_time
                            .is_some_and(|t| t.elapsed() >= Duration::from_millis(restore_delay_ms))
                    {
                        wm.map_window(b).unwrap().check().unwrap();
                        if release_at_transfer == -1 {
                            wm.xtest_fake_input(KEY_RELEASE_EVENT, 112, 0, root, 0, 0, 0)
                                .unwrap()
                                .check()
                                .unwrap();
                            released_time = Some(Instant::now());
                            eprintln!("key release immediately BEFORE focus transfer");
                        }
                        wm.set_input_focus(
                            if pointer_root {
                                InputFocus::POINTER_ROOT
                            } else {
                                InputFocus::PARENT
                            },
                            b,
                            0u32,
                        )
                        .unwrap()
                        .check()
                        .unwrap();
                        if release_at_transfer == 1 {
                            wm.xtest_fake_input(KEY_RELEASE_EVENT, 112, 0, root, 0, 0, 0)
                                .unwrap()
                                .check()
                                .unwrap();
                            released_time = Some(Instant::now());
                            eprintln!("key release immediately AFTER focus transfer");
                        }
                        wm.change_property32(
                            PropMode::REPLACE,
                            root,
                            active_atom,
                            AtomEnum::WINDOW,
                            &[b],
                        )
                        .unwrap()
                        .check()
                        .unwrap();
                        restored_time = Some(Instant::now());
                    }
                    if release_at_transfer == 0
                        && released_time.is_none()
                        && activation_time
                            .is_some_and(|t| t.elapsed() >= Duration::from_millis(release_delay_ms))
                    {
                        eprintln!(
                            "key release: target_focus={} pointer_target={}",
                            wm.get_input_focus().unwrap().reply().unwrap().focus == b,
                            wm.query_pointer(root).unwrap().reply().unwrap().child == b
                        );
                        wm.xtest_fake_input(KEY_RELEASE_EVENT, 112, 0, root, 0, 0, 0)
                            .unwrap()
                            .check()
                            .unwrap();
                        released_time = Some(Instant::now());
                    }
                    if restored_time.is_some_and(|t| t.elapsed() >= Duration::from_millis(200))
                        && released_time.is_some_and(|t| t.elapsed() >= Duration::from_millis(200))
                    {
                        break;
                    }
                    if flood_events {
                        // More events than one daemon batch, throughout the pending interval.
                        for value in 0..512u32 {
                            wm.change_property32(
                                PropMode::REPLACE,
                                a,
                                AtomEnum::WM_COMMAND,
                                AtomEnum::CARDINAL,
                                &[value],
                            )
                            .unwrap();
                        }
                        wm.flush().unwrap();
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                let final_focus = wm.get_input_focus().unwrap().reply().unwrap().focus;
                let visible = wm
                    .get_window_attributes(preview_b)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .map_state
                    == MapState::VIEWABLE;
                eprintln!(
                    "RESULT transfer_release={release_at_transfer} minimize={minimize} restore_ms={restore_delay_ms} release_ms={release_delay_ms} pointer_inside={pointer_inside} early_minimize={minimized_early} restore_request={saw_restore_request} focused={} visible={visible}",
                    final_focus == b
                );
                assert!(released_time.is_some());
                config_tx.send(ConfigMessage::Shutdown).unwrap();
                (
                    minimized_early,
                    saw_minimize,
                    restored_time.is_some(),
                    final_focus == b,
                    visible,
                )
            });
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let signal =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
                        .unwrap();
                tokio::time::timeout(
                    Duration::from_secs(5),
                    run_event_loop(
                        ctx.conn,
                        ctx.screen,
                        display,
                        ctx.atoms,
                        ctx.formats,
                        font,
                        resources,
                        hotkey_rx,
                        HashMap::new(),
                        signal,
                        config_rx,
                        status_tx,
                        Arc::new(RwLock::new(HashSet::new())),
                    ),
                )
                .await
                .unwrap()
                .unwrap();
            });
            let (early, saw_minimize, restored, focused, visible) = wm_task.join().unwrap();
            assert!(restored && focused, "fixture must restore and focus target");
            assert!(!early, "must not minimize before target focus and mapping");
            assert!(visible, "successful switch must retain previews");
            assert_eq!(saw_minimize, minimize && restore_delay_ms < 1000);
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn confirmed_activation_grab_restore_matrix() {
        for pointer_root in [false, true] {
            for minimize in [false, true] {
                for inside in [false, true] {
                    for delay in [0, 150] {
                        for release_at_transfer in [-1, 1] {
                            confirmed_activation_case(
                                minimize,
                                delay,
                                0,
                                inside,
                                release_at_transfer,
                                pointer_root,
                                false,
                            );
                        }
                    }
                }
            }
        }
        // A released grab while the WM is still restoring the target.
        confirmed_activation_case(true, 150, 50, false, 0, false, false);
        confirmed_activation_case(true, 150, 50, false, 0, true, false);
    }
    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn activation_timeout_is_serviced_under_continuous_x_events() {
        confirmed_activation_case(true, 1200, 50, false, 0, false, true);
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn heartbeat_and_shutdown_are_not_starved_by_sustained_hotkeys() {
        use std::time::Duration;
        with_x11(|ctx| {
            let config = DaemonConfig {
                profile: Profile {
                    thumbnail_enabled: false,
                    hotkey_require_eve_focus: true,
                    ..Profile::default()
                },
                character_thumbnails: HashMap::new(),
                custom_source_thumbnails: HashMap::new(),
                profile_hotkeys: HashMap::new(),
                runtime_hidden: false,
            };
            let display = config.build_display_config();
            let font = FontRenderer::resolve_from_config(ctx.conn, "sans-serif", 12.0).unwrap();
            let mut resources = DaemonResources {
                cycle: CycleState::new(config.profile.cycle_groups.clone()),
                sources: SourceRegistry::default(),
                config,
                session: SessionState::default(),
                eve_clients: HashMap::new(),
                group_drag: GroupDragState::default(),
            };
            let source = window(ctx, ctx.screen.root);
            resources
                .sources
                .register(source, TrackedSource::eve("Alice"));
            // The real WM focus gate accepts every command, so each one is fully processed.
            focus(ctx, Some(source));
            ctx.conn
                .set_input_focus(InputFocus::PARENT, source, 0u32)
                .unwrap()
                .check()
                .unwrap();
            let command = || TimestampedCommand {
                command: CycleCommand::Forward("missing".into()),
                timestamp: 0,
            };
            let (hotkey_tx, hotkey_rx) = mpsc::channel(128);
            for _ in 0..128 {
                hotkey_tx.try_send(command()).unwrap();
            }
            // Input stays continuously ready until the loop exits and drops the receiver.
            let producer =
                std::thread::spawn(move || while hotkey_tx.blocking_send(command()).is_ok() {});
            let (config_tx, config_rx) = ipc::channel().unwrap();
            let (status_tx, status_rx) = ipc::channel().unwrap();
            // Two heartbeats span a full interval under sustained input; only then shut down.
            let control = std::thread::spawn(move || {
                let mut heartbeats = 0;
                while heartbeats < 2 {
                    match status_rx.try_recv_timeout(Duration::from_secs(5)) {
                        Ok(DaemonMessage::Heartbeat) => heartbeats += 1,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                config_tx.send(ConfigMessage::Shutdown).unwrap();
                heartbeats
            });
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let result = runtime.block_on(async {
                let signal =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
                        .unwrap();
                tokio::time::timeout(
                    Duration::from_secs(10),
                    run_event_loop(
                        ctx.conn,
                        ctx.screen,
                        display,
                        ctx.atoms,
                        ctx.formats,
                        font,
                        resources,
                        hotkey_rx,
                        HashMap::new(),
                        signal,
                        config_rx,
                        status_tx,
                        Arc::new(RwLock::new(HashSet::new())),
                    ),
                )
                .await
            });
            producer.join().unwrap();
            let heartbeats = control.join().unwrap();
            assert_eq!(
                heartbeats, 2,
                "heartbeats must continue under sustained input"
            );
            assert!(
                result.is_ok(),
                "a queued shutdown must beat sustained hotkey input"
            );
            result.unwrap().unwrap();
        });
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn preview_toggle_preserves_cancelled_drag_release() {
        with_x11(|ctx| {
            with_daemon(ctx, |resources, font, tx| {
                resources.group_drag = GroupDragState::SuppressingRelease(
                    crate::daemon::group_drag::ChordButtons::Right,
                );
                handle_cycle_command(
                    &CycleCommand::TogglePreviews,
                    resources,
                    ctx,
                    font,
                    tx,
                    &HashMap::new(),
                );
                assert!(
                    resources
                        .group_drag
                        .consume_suppressed_release(crate::common::constants::mouse::BUTTON_RIGHT)
                );
            })
        });
    }
}
