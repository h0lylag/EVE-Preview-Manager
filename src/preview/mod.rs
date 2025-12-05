//! Preview daemon - runs in background showing EVE window thumbnails

mod cycle_state;
mod event_handler;
pub mod font;
mod font_discovery;
mod session_state;
mod snapping;
mod thumbnail;

pub use font_discovery::{find_font_path, list_fonts, select_best_default_font};

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::damage::ConnectionExt as DamageExt;
use x11rb::protocol::xproto::*;

use crate::config::PersistentState;
use crate::constants::{self, eve, paths, wine};
use crate::hotkeys::{self, spawn_listener, CycleCommand};
use crate::types::Dimensions;
use crate::x11_utils::{activate_window, is_window_eve, is_window_minimized, minimize_window, AppContext, CachedAtoms};

use cycle_state::CycleState;
use event_handler::handle_event;
use session_state::SessionState;
use thumbnail::Thumbnail;

fn check_and_create_window<'a>(
    ctx: &AppContext<'a>,
    persistent_state: &PersistentState,
    window: Window,
    state: &mut SessionState,
) -> Result<Option<Thumbnail<'a>>> {
    let pid_atom = ctx.conn.intern_atom(false, b"_NET_WM_PID")
        .context("Failed to intern _NET_WM_PID atom")?
        .reply()
        .context("Failed to get reply for _NET_WM_PID atom")?
        .atom;
    if let Ok(prop) = ctx.conn
        .get_property(false, window, pid_atom, AtomEnum::CARDINAL, 0, 1)
        .context(format!("Failed to query _NET_WM_PID property for window {}", window))?
        .reply()
    {
        if !prop.value.is_empty() {
            let pid = u32::from_ne_bytes(prop.value[0..constants::x11::PID_PROPERTY_SIZE].try_into()
                .context("Invalid PID property format (expected 4 bytes)")?);
            
            // Skip our own thumbnail windows
            if pid == std::process::id() {
                return Ok(None);
            }
            
            if !std::fs::read_link(paths::PROC_EXE_FORMAT.replace("{}", &pid.to_string()))
                .map(|x| {
                    x.to_string_lossy().contains(wine::WINE64_PRELOADER)
                        || x.to_string_lossy().contains(wine::WINE_PRELOADER)
                })
                .inspect_err(|e| {
                    error!(
                        pid = pid,
                        error = ?e,
                        "Cannot read /proc/{pid}/exe, assuming wine process"
                    );
                })
                .unwrap_or(true)
            {
                return Ok(None); // Return if we can determine that the window is not running through wine.
            }
        } else {
            warn!(
                window = window,
                "_NET_WM_PID not set, assuming wine process"
            );
        }
    }

    ctx.conn.change_window_attributes(
        window,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .context(format!("Failed to set event mask for window {}", window))?;

    if let Some(eve_window) = is_window_eve(ctx.conn, window, ctx.atoms)
        .context(format!("Failed to check if window {} is EVE client", window))? {
        let character_name = eve_window.character_name().to_string();

        // Track last known character for this window (for logged-out cycling feature)
        state.update_last_character(window, &character_name);

        ctx.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::PROPERTY_CHANGE | EventMask::FOCUS_CHANGE),
        )
        .context(format!("Failed to set focus event mask for EVE window {} ('{}')", window, character_name))?;
        
        // Get saved position and dimensions for this character/window
        let position = state.get_position(
            &character_name, 
            window, 
            &persistent_state.character_positions,
            persistent_state.global.preserve_thumbnail_position_on_swap,
        );
        
        // Get dimensions from CharacterSettings or use auto-detected defaults
        let dimensions = if let Some(settings) = persistent_state.character_positions.get(&character_name) {
            // If dimensions are 0 (not yet saved), auto-detect
            if settings.dimensions.width == 0 || settings.dimensions.height == 0 {
                let (w, h) = persistent_state.default_thumbnail_size(
                    ctx.screen.width_in_pixels,
                    ctx.screen.height_in_pixels,
                );
                Dimensions::new(w, h)
            } else {
                settings.dimensions
            }
        } else {
            // Character not in settings yet - auto-detect
            let (w, h) = persistent_state.default_thumbnail_size(
                ctx.screen.width_in_pixels,
                ctx.screen.height_in_pixels,
            );
            Dimensions::new(w, h)
        };
        
        let mut thumbnail = Thumbnail::new(ctx, character_name.clone(), window, ctx.font_renderer, position, dimensions)
            .context(format!("Failed to create thumbnail for '{}' (window {})", character_name, window))?;
        if is_window_minimized(ctx.conn, window, ctx.atoms)
            .context(format!("Failed to query minimized state for window {}", window))?
        {
            debug!(window = window, character = %character_name, "Window minimized at startup");
            thumbnail
                .minimized()
                .context(format!("Failed to set minimized state for '{}'", character_name))?;
        }
        info!(
            window = window,
            character = %character_name,
            "Created thumbnail for EVE window"
        );
        Ok(Some(thumbnail))
    } else {
        Ok(None)
    }
}

fn get_eves<'a>(
    ctx: &AppContext<'a>,
    persistent_state: &mut PersistentState,
    state: &mut SessionState,
) -> Result<HashMap<Window, Thumbnail<'a>>> {
    let net_client_list = ctx.conn.intern_atom(false, b"_NET_CLIENT_LIST")
        .context("Failed to intern _NET_CLIENT_LIST atom")?
        .reply()
        .context("Failed to get reply for _NET_CLIENT_LIST atom")?
        .atom;
    let prop = ctx.conn
        .get_property(
            false,
            ctx.screen.root,
            net_client_list,
            AtomEnum::WINDOW,
            0,
            u32::MAX,
        )
        .context("Failed to query _NET_CLIENT_LIST property")?
        .reply()
        .context("Failed to get window list from X11 server")?;
    let windows: Vec<u32> = prop
        .value32()
        .ok_or_else(|| anyhow::anyhow!("Invalid return from _NET_CLIENT_LIST"))?
        .collect();

    let mut eves = HashMap::new();
    for w in windows {
        if let Some(eve) = check_and_create_window(ctx, persistent_state, w, state)
            .context(format!("Failed to process window {} during initial scan", w))? {
            
            // Save initial position and dimensions (important for first-time characters)
            // Query geometry to get actual position from X11
            let geom = ctx.conn.get_geometry(eve.window)
                .context("Failed to query geometry during initial scan")?
                .reply()
                .context("Failed to get geometry reply during initial scan")?;
            
            // ALWAYS update character_positions in memory (for manual saves)
            let settings = crate::types::CharacterSettings::new(
                geom.x,
                geom.y,
                eve.dimensions.width,
                eve.dimensions.height,
            );
            persistent_state.character_positions.insert(eve.character_name.clone(), settings);
            
            // Conditionally persist to disk based on auto-save setting
            if persistent_state.profile.auto_save_thumbnail_positions {
                persistent_state.save()
                    .context(format!("Failed to save initial position during scan for '{}'", eve.character_name))?;
            }
            
            eves.insert(w, eve);
        }
    }
    ctx.conn.flush()
        .context("Failed to flush X11 connection after creating thumbnails")?;
    Ok(eves)
}

pub fn run_preview_daemon() -> Result<()> {
    // Connect to X11 first to get screen dimensions for smart config defaults
    let (conn, screen_num) = x11rb::connect(None)
        .context("Failed to connect to X11 server. Is DISPLAY set correctly?")?;
    let screen = &conn.setup().roots[screen_num];
    info!(
        screen = screen_num,
        width = screen.width_in_pixels,
        height = screen.height_in_pixels,
        "Connected to X11 server"
    );

    // Set up signal handler for manual save trigger (SIGUSR1)
    let save_requested = Arc::new(AtomicBool::new(false));
    let save_flag = save_requested.clone();
    
    #[cfg(target_os = "linux")]
    {
        use signal_hook::consts::SIGUSR1;
        use signal_hook::flag as signal_flag;
        
        // Register SIGUSR1 to set the atomic flag
        signal_flag::register(SIGUSR1, save_flag)
            .context("Failed to register SIGUSR1 handler")?;
        info!("Registered SIGUSR1 handler for manual position save");
    }

    // Load config with screen-aware defaults
    let mut persistent_state = PersistentState::load_with_screen(
        screen.width_in_pixels,
        screen.height_in_pixels,
    );
    let config = persistent_state.build_display_config();
    info!(config = ?config, "Loaded display configuration");
    
    let mut session_state = SessionState::new();
    info!(
        count = persistent_state.character_positions.len(),
        "Loaded character positions from config"
    );
    
    // Initialize cycle state from config
    let mut cycle_state = CycleState::new(persistent_state.profile.cycle_group.clone());
    
    // Create channel for hotkey thread → main loop
    let (hotkey_tx, hotkey_rx) = mpsc::channel();
    
    // Spawn hotkey listener (optional - skip if permissions denied or not configured)
    let _hotkey_handle = if let (Some(forward_key), Some(backward_key)) =
        (&persistent_state.profile.cycle_forward_keys, &persistent_state.profile.cycle_backward_keys)
    {
        if hotkeys::check_permissions() {
            match spawn_listener(
                hotkey_tx,
                forward_key.clone(),
                backward_key.clone(),
                persistent_state.profile.selected_hotkey_device.clone(),
            ) {
                Ok(handle) => {
                    info!(
                        enabled = true,
                        forward = %forward_key.display_name(),
                        backward = %backward_key.display_name(),
                        "Hotkey support enabled with configured bindings"
                    );
                    Some(handle)
                }
                Err(e) => {
                    error!(error = %e, "Failed to start hotkey listener");
                    hotkeys::print_permission_error();
                    None
                }
            }
        } else {
            hotkeys::print_permission_error();
            None
        }
    } else {
        info!("Hotkey bindings not configured - hotkey support disabled");
        None
    };
    
    // Pre-cache atoms once at startup (eliminates roundtrip overhead)
    let atoms = CachedAtoms::new(&conn)
        .context("Failed to cache X11 atoms at startup")?;
    
    // Initialize font renderer with configured font (or fallback to system default)
    let font_renderer = if !persistent_state.profile.text_font_family.is_empty() {
        info!(
            configured_font = %persistent_state.profile.text_font_family,
            size = persistent_state.profile.text_size,
            "Attempting to load user-configured font"
        );
        // Try user-selected font first
        font::FontRenderer::from_font_name(
            &persistent_state.profile.text_font_family,
            persistent_state.profile.text_size as f32
        )
        .or_else(|e| {
            warn!(
                font = %persistent_state.profile.text_font_family,
                error = ?e,
                "Failed to load configured font, falling back to system default"
            );
            font::FontRenderer::from_system_font(&conn, persistent_state.profile.text_size as f32)
        })
    } else {
        info!(
            size = persistent_state.profile.text_size,
            "No font configured, using system default"
        );
        font::FontRenderer::from_system_font(&conn, persistent_state.profile.text_size as f32)
    }
    .context(format!("Failed to initialize font renderer with size {}", persistent_state.profile.text_size))?;
    
    info!(
        size = persistent_state.profile.text_size,
        font = %persistent_state.profile.text_font_family,
        "Font renderer initialized"
    );
    
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

    // Pre-cache picture formats (avoids expensive queries on every thumbnail)
    let formats = crate::x11_utils::CachedFormats::new(&conn, screen)
        .context("Failed to cache picture formats at startup")?;
    info!("Picture formats cached");

    let ctx = AppContext {
        conn: &conn,
        screen,
        config: &config,
        atoms: &atoms,
        formats: &formats,
        font_renderer: &font_renderer,
    };

    let mut eves = get_eves(&ctx, &mut persistent_state, &mut session_state)
        .context("Failed to get initial list of EVE windows")?;
    
    // Register initial windows with cycle state
    for (window, thumbnail) in eves.iter() {
        cycle_state.add_window(thumbnail.character_name.clone(), *window);
    }
    
    info!("Preview daemon running");
    
    loop {
        // Check if manual save was requested via SIGUSR1
        if save_requested.load(Ordering::Relaxed) {
            save_requested.store(false, Ordering::Relaxed);
            info!("Manual save requested via SIGUSR1");
            
            // Save current positions to disk
            if let Err(e) = persistent_state.save() {
                error!(error = ?e, "Failed to save positions after SIGUSR1");
            } else {
                info!("Positions saved successfully");
            }
        }
        
        // Check for hotkey commands (non-blocking)
        if let Ok(command) = hotkey_rx.try_recv() {
            // Check if we should only allow hotkeys when EVE window is focused
            let should_process = if persistent_state.global.hotkey_require_eve_focus {
                crate::x11_utils::is_eve_window_focused(&conn, screen, &atoms)
                    .inspect_err(|e| error!(error = %e, "Failed to check focused window"))
                    .unwrap_or(false)
            } else {
                true
            };
            
            if should_process {
                info!(command = ?command, "Received hotkey command");

                // Build logged-out map if feature is enabled in profile
                let logged_out_map = if persistent_state.profile.include_logged_out_in_cycle {
                    Some(&session_state.window_last_character)
                } else {
                    None
                };

                let result = match command {
                    CycleCommand::Forward => cycle_state.cycle_forward(logged_out_map),
                    CycleCommand::Backward => cycle_state.cycle_backward(logged_out_map),
                };

                if let Some((window, character_name)) = result {
                    let display_name = if character_name.is_empty() {
                        eve::LOGGED_OUT_DISPLAY_NAME
                    } else {
                        character_name
                    };
                    info!(
                        window = window,
                        character = %display_name,
                        "Activating window via hotkey"
                    );
                    if let Err(e) = activate_window(&conn, screen, &atoms, window) {
                        error!(window = window, error = %e, "Failed to activate window");
                    } else if persistent_state.global.minimize_clients_on_switch {
                        // Minimize all other EVE clients after successful activation
                        let other_windows: Vec<Window> = eves
                            .keys()
                            .copied()
                            .filter(|w| *w != window)
                            .collect();
                        for other_window in other_windows {
                            if let Err(e) = minimize_window(&conn, screen, &atoms, other_window) {
                                debug!(window = other_window, error = %e, "Failed to minimize window via hotkey");
                            }
                        }
                    }
                } else {
                    warn!(active_windows = cycle_state.config_order().len(), "No window to activate, cycle state is empty");
                }
            } else {
                info!(hotkey_require_eve_focus = persistent_state.global.hotkey_require_eve_focus, "Hotkey ignored, EVE window not focused (hotkey_require_eve_focus enabled)");
            }
        }

        let event = conn.wait_for_event()
            .context("Failed to wait for X11 event")?;
        let _ = handle_event(
            &ctx,
            &mut persistent_state,
            &mut eves,
            event,
            &mut session_state,
            &mut cycle_state,
            check_and_create_window
        ).inspect_err(|err| error!(error = ?err, "Event handling error"));
    }
}
