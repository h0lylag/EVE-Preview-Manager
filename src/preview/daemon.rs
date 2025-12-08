//! Preview daemon main loop and initialization

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::damage::ConnectionExt as DamageExt;
use x11rb::protocol::xproto::*;

use crate::config::DaemonConfig;
use crate::constants::eve;
use super::hotkeys::{self, spawn_listener, CycleCommand};
use crate::x11::{activate_window, minimize_window, AppContext, CachedAtoms};

use super::cycle_state::CycleState;
use super::event_handler::handle_event;
use super::font;
use super::session_state::SessionState;
use super::thumbnail::Thumbnail;
use super::window_detection::check_and_create_window;

fn get_eves<'a>(
    ctx: &AppContext<'a>,
    daemon_config: &mut DaemonConfig,
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
        if let Some(eve) = check_and_create_window(ctx, daemon_config, w, state)
            .context(format!("Failed to process window {} during initial scan", w))? {

            // Save initial position and dimensions (important for first-time characters)
            // Query geometry to get actual position from X11
            let geom = ctx.conn.get_geometry(eve.window)
                .context("Failed to query geometry during initial scan")?
                .reply()
                .context("Failed to get geometry reply during initial scan")?;

            // Update character_thumbnails in memory (skip logged-out clients with empty name)
            if !eve.character_name.is_empty() {
                let settings = crate::types::CharacterSettings::new(
                    geom.x,
                    geom.y,
                    eve.dimensions.width,
                    eve.dimensions.height,
                );
                daemon_config.character_thumbnails.insert(eve.character_name.clone(), settings);
            }

            eves.insert(w, eve);
        }
    }

    // Save once after processing all windows (avoids repeated disk writes)
    if daemon_config.profile.thumbnail_auto_save_position && !eves.is_empty() {
        daemon_config.save()
            .context("Failed to save initial positions after startup scan")?;
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
    let mut daemon_config = DaemonConfig::load_with_screen(
        screen.width_in_pixels,
        screen.height_in_pixels,
    );
    let config = daemon_config.build_display_config();
    info!(config = ?config, "Loaded display configuration");
    
    let mut session_state = SessionState::new();
    info!(
        count = daemon_config.character_thumbnails.len(),
        "Loaded character positions from config"
    );
    
    // Initialize cycle state from config
    let mut cycle_state = CycleState::new(daemon_config.profile.hotkey_cycle_group.clone());
    
    // Create channel for hotkey thread → main loop
    let (hotkey_tx, hotkey_rx) = mpsc::channel();

    // Build character hotkey list from character_hotkeys HashMap, using cycle group order
    let character_hotkeys: Vec<_> = daemon_config.profile.hotkey_cycle_group
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

    // Build hotkey groups: map each unique hotkey binding to ordered list of characters
    // When multiple characters share a hotkey, pressing it cycles through them based on cycle order
    let mut hotkey_groups: HashMap<crate::config::HotkeyBinding, Vec<String>> = HashMap::new();

    for char_name in &daemon_config.profile.hotkey_cycle_group {
        if let Some(binding) = daemon_config.profile.character_hotkeys.get(char_name) {
            hotkey_groups.entry(binding.clone())
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

    let _hotkey_handle = if has_cycle_keys || has_character_hotkeys {
        if hotkeys::check_permissions() {
            match spawn_listener(
                hotkey_tx,
                daemon_config.profile.hotkey_cycle_forward.clone(),
                daemon_config.profile.hotkey_cycle_backward.clone(),
                character_hotkeys.clone(),
                daemon_config.profile.hotkey_input_device.clone(),
            ) {
                Ok(handle) => {
                    info!(
                        enabled = true,
                        has_cycle_keys = has_cycle_keys,
                        has_character_hotkeys = has_character_hotkeys,
                        "Hotkey support enabled"
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
        info!("No hotkeys configured - hotkey support disabled");
        None
    };
    
    // Pre-cache atoms once at startup (eliminates roundtrip overhead)
    let atoms = CachedAtoms::new(&conn)
        .context("Failed to cache X11 atoms at startup")?;
    
    // Initialize font renderer with configured font (or fallback to system default)
    let font_renderer = if !daemon_config.profile.thumbnail_text_font.is_empty() {
        info!(
            configured_font = %daemon_config.profile.thumbnail_text_font,
            size = daemon_config.profile.thumbnail_text_size,
            "Attempting to load user-configured font"
        );
        // Try user-selected font first
        font::FontRenderer::from_font_name(
            &daemon_config.profile.thumbnail_text_font,
            daemon_config.profile.thumbnail_text_size as f32
        )
        .or_else(|e| {
            warn!(
                font = %daemon_config.profile.thumbnail_text_font,
                error = ?e,
                "Failed to load configured font, falling back to system default"
            );
            font::FontRenderer::from_system_font(&conn, daemon_config.profile.thumbnail_text_size as f32)
        })
    } else {
        info!(
            size = daemon_config.profile.thumbnail_text_size,
            "No font configured, using system default"
        );
        font::FontRenderer::from_system_font(&conn, daemon_config.profile.thumbnail_text_size as f32)
    }
    .context(format!("Failed to initialize font renderer with size {}", daemon_config.profile.thumbnail_text_size))?;
    
    info!(
        size = daemon_config.profile.thumbnail_text_size,
        font = %daemon_config.profile.thumbnail_text_font,
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
    let formats = crate::x11::CachedFormats::new(&conn, screen)
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

    let mut eves = get_eves(&ctx, &mut daemon_config, &mut session_state)
        .context("Failed to get initial list of EVE windows")?;
    
    // Register initial windows with cycle state
    // When thumbnails are enabled, use the eves HashMap
    // When thumbnails are disabled, use session_state.window_last_character which was
    // populated by check_eve_window() during get_eves() scan
    if config.enabled {
        for (window, thumbnail) in eves.iter() {
            cycle_state.add_window(thumbnail.character_name.clone(), *window);
        }
    } else {
        // Thumbnails disabled - register windows from session_state tracking
        for (window, character_name) in session_state.window_last_character.iter() {
            cycle_state.add_window(character_name.clone(), *window);
        }
    }
    
    info!("Preview daemon running");
    
    loop {
        // Check if manual save was requested via SIGUSR1
        if save_requested.load(Ordering::Relaxed) {
            save_requested.store(false, Ordering::Relaxed);
            info!("Manual save requested via SIGUSR1");
            
            // Save current positions to disk
            if let Err(e) = daemon_config.save() {
                error!(error = ?e, "Failed to save positions after SIGUSR1");
            } else {
                info!("Positions saved successfully");
            }
        }
        
        // Check for hotkey commands (with small timeout to avoid busy-waiting)
        // Use recv_timeout instead of try_recv to efficiently wait for hotkeys
        // while still processing X11 events when they arrive
        match hotkey_rx.recv_timeout(std::time::Duration::from_millis(10)) {
            Ok(command) => {
                // Check if we should only allow hotkeys when EVE window is focused
                let should_process = if daemon_config.profile.hotkey_require_eve_focus {
                    crate::x11::is_eve_window_focused(&conn, screen, &atoms)
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
                let logged_out_map = if daemon_config.profile.hotkey_logged_out_cycle {
                    Some(&session_state.window_last_character)
                } else {
                    None
                };

                let result = match command {
                    CycleCommand::Forward => cycle_state.cycle_forward(logged_out_map),
                    CycleCommand::Backward => cycle_state.cycle_backward(logged_out_map),
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
                            if char_group.is_empty() {
                                warn!(binding = %binding.display_name(), "Character hotkey group is empty");
                                None
                            } else if char_group.len() == 1 {
                                // Single character - direct activation
                                let char_name = &char_group[0];
                                cycle_state.activate_character(char_name, logged_out_map)
                            } else {
                                // Multiple characters share this hotkey - find next one in cycle order
                                // Start from the character AFTER the current cycle position
                                let current_cycle_pos = cycle_state.current_position();
                                let current_char = daemon_config.profile.hotkey_cycle_group.get(current_cycle_pos);
                                
                                debug!(
                                    binding = %binding.display_name(),
                                    current_index = current_cycle_pos,
                                    current_character = ?current_char,
                                    "Starting multi-character hotkey search"
                                );
                                
                                // Find all characters in this hotkey group with their positions in the cycle order
                                let mut group_with_positions: Vec<(usize, &String)> = char_group
                                    .iter()
                                    .filter_map(|char_name| {
                                        daemon_config.profile.hotkey_cycle_group
                                            .iter()
                                            .position(|c| c == char_name)
                                            .map(|pos| (pos, char_name))
                                    })
                                    .collect();
                                
                                // Sort by position in cycle order
                                group_with_positions.sort_by_key(|(pos, _)| *pos);
                                
                                debug!(
                                    binding = %binding.display_name(),
                                    group_positions = ?group_with_positions,
                                    "Hotkey group characters with positions"
                                );
                                
                                // Find the first character after current position (wrapping around)
                                let start_search_pos = (current_cycle_pos + 1) % daemon_config.profile.hotkey_cycle_group.len();
                                
                                let mut result = None;
                                
                                // Try characters starting from after current position
                                for (pos, char_name) in &group_with_positions {
                                    if *pos >= start_search_pos
                                        && let Some(activation_result) = cycle_state.activate_character(char_name, logged_out_map) {
                                            info!(
                                                binding = %binding.display_name(),
                                                character = %char_name,
                                                group_size = char_group.len(),
                                                "Per-character hotkey activation (forward from current position)"
                                            );
                                            result = Some(activation_result);
                                            break;
                                        }
                                }
                                
                                // If nothing found after current position, wrap around and check from beginning
                                if result.is_none() {
                                    for (pos, char_name) in &group_with_positions {
                                        if *pos < start_search_pos
                                            && let Some(activation_result) = cycle_state.activate_character(char_name, logged_out_map) {
                                                info!(
                                                    binding = %binding.display_name(),
                                                    character = %char_name,
                                                    group_size = char_group.len(),
                                                    "Per-character hotkey activation (wrapped around)"
                                                );
                                                result = Some(activation_result);
                                                break;
                                            }
                                    }
                                }

                                if result.is_none() {
                                    warn!(
                                        binding = %binding.display_name(),
                                        group_size = char_group.len(),
                                        "No active windows in character hotkey group"
                                    );
                                }

                                result
                            }
                        } else {
                            warn!(
                                binding = %binding.display_name(),
                                available_groups = hotkey_groups.len(),
                                "Character hotkey binding not found in groups - this shouldn't happen!"
                            );
                            // Debug: log all available groups for comparison
                            for available_binding in hotkey_groups.keys() {
                                debug!(
                                    available = %available_binding.display_name(),
                                    matches = (available_binding == binding),
                                    "Available hotkey group"
                                );
                            }
                            None
                        }
                    }
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
                    
                    // Debug: log window ID before activation attempt
                    debug!(
                        window_id = window,
                        window_hex = format!("0x{:x}", window),
                        "About to call activate_window"
                    );
                    
                    if let Err(e) = activate_window(&conn, screen, &atoms, window) {
                        error!(window = window, error = %e, "Failed to activate window");
                    } else {
                        debug!(window = window, "activate_window completed successfully");
                        
                        if daemon_config.profile.client_minimize_on_switch {
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
                    }
                } else {
                    warn!(active_windows = cycle_state.config_order().len(), "No window to activate, cycle state is empty");
                }
            } else {
                info!(hotkey_require_eve_focus = daemon_config.profile.hotkey_require_eve_focus, "Hotkey ignored, EVE window not focused (hotkey_require_eve_focus enabled)");
            }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // No hotkey received within timeout - this is normal, continue to X11 events
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                warn!("Hotkey channel disconnected, hotkeys will no longer work");
            }
        }

        // Process any pending X11 events (non-blocking)
        // With recv_timeout above, we wake up every 10ms to check for X11 events
        // This ensures responsive event handling even when thumbnails are disabled
        while let Some(event) = conn.poll_for_event()
            .context("Failed to poll for X11 event")? {
            let _ = handle_event(
                &ctx,
                &mut daemon_config,
                &mut eves,
                event,
                &mut session_state,
                &mut cycle_state,
            ).inspect_err(|err| error!(error = ?err, "Event handling error"));
        }
    }
}
