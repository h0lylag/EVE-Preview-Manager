//! X11 event processing for the preview daemon
//!
//! Handles window creation/destruction, damage notifications, focus changes,
//! and mouse interactions (click-to-focus, drag-to-move).

use anyhow::{Context, Result};
use std::collections::HashMap;
use tracing::{debug, info, trace, warn};
use x11rb::connection::Connection;
use x11rb::protocol::Event::{self, CreateNotify, DamageNotify, DestroyNotify, PropertyNotify};
use x11rb::protocol::damage::ConnectionExt as DamageExt;
use x11rb::protocol::xproto::*;

use crate::config::DaemonConfig;
use crate::constants::mouse;
use crate::types::{Position, ThumbnailState};
use crate::x11::{AppContext, is_window_eve, minimize_window};

use super::cycle_state::CycleState;
use super::session_state::SessionState;
use super::snapping::{self, Rect};
use super::thumbnail::Thumbnail;

/// Context bundle for event handlers to reduce argument count
pub struct EventContext<'a, 'b> {
    pub app_ctx: &'a AppContext<'a>,
    pub daemon_config: &'b mut DaemonConfig,
    pub eve_clients: &'b mut HashMap<Window, Thumbnail<'a>>,
    pub session_state: &'b mut SessionState,
    pub cycle_state: &'b mut CycleState,
}

/// Handle DamageNotify events - update damaged thumbnail
#[tracing::instrument(skip(ctx, event), fields(window = event.drawable))]
fn handle_damage_notify(
    ctx: &mut EventContext,
    event: x11rb::protocol::damage::NotifyEvent,
) -> Result<()> {
    // Skip rendering updates if thumbnails are disabled (daemon still runs for hotkeys)
    if !ctx.app_ctx.config.enabled {
        return Ok(());
    }

    // NOTE: Logging skipped for high-frequency path
    if let Some(thumbnail) = ctx
        .eve_clients
        .values()
        .find(|thumbnail| thumbnail.damage() == event.damage)
    {
        thumbnail.update().context(format!(
            "Failed to update thumbnail for damage event (damage={})",
            event.damage
        ))?;
        ctx.app_ctx
            .conn
            .damage_subtract(event.damage, 0u32, 0u32)
            .context(format!(
                "Failed to subtract damage region (damage={})",
                event.damage
            ))?;
        ctx.app_ctx
            .conn
            .flush()
            .context("Failed to flush X11 connection after damage update")?;
    }
    Ok(())
}

/// Handle CreateNotify events - create thumbnail for new EVE window
#[tracing::instrument(skip(ctx, event))]
fn handle_create_notify(ctx: &mut EventContext, event: CreateNotifyEvent) -> Result<()> {
    use crate::preview::window_detection::{check_and_create_window, check_eve_window};

    debug!(window = event.window, "CreateNotify received");

    // First, check if this is an EVE window and register it with cycle state
    // This happens regardless of whether thumbnails are enabled
    if let Some(character_name) = check_eve_window(ctx.app_ctx, event.window, ctx.session_state)
        .context(format!(
            "Failed to check if window {} is EVE client",
            event.window
        ))?
    {
        info!(window = event.window, character = %character_name, "Detected new EVE window");

        // Always register with cycle state for hotkey support
        ctx.cycle_state
            .add_window(character_name.clone(), event.window);

        // Only create thumbnail if thumbnails are enabled
        if ctx.app_ctx.config.enabled
            && let Some(thumbnail) = check_and_create_window(
                ctx.app_ctx,
                ctx.daemon_config,
                event.window,
                ctx.session_state,
            )
            .context(format!(
                "Failed to create thumbnail for window {}",
                event.window
            ))?
        {
            // Save initial position and dimensions for new character
            let geom = ctx
                .app_ctx
                .conn
                .get_geometry(thumbnail.window())
                .context("Failed to query geometry for new thumbnail")?
                .reply()
                .context("Failed to get geometry reply for new thumbnail")?;

            // NOTE: Update character_thumbnails in memory (for manual saves)
            // Skip logged-out clients with empty character name
            if !thumbnail.character_name.is_empty() {
                let settings = crate::types::CharacterSettings::new(
                    geom.x,
                    geom.y,
                    thumbnail.dimensions.width,
                    thumbnail.dimensions.height,
                );
                ctx.daemon_config
                    .character_thumbnails
                    .insert(thumbnail.character_name.clone(), settings);
            }

            // Conditionally persist to disk based on auto-save setting
            // NOTE: If auto-save is DISABLED, we still perform a "safe save" of ONLY the new character's entry
            // This ensures the GUI can discover the new character via file monitoring w/o overwriting other pending
            // position changes that the user chose NOT to save.
            if ctx.daemon_config.profile.thumbnail_auto_save_position {
                ctx.daemon_config.save().context(format!(
                    "Failed to save initial position for new character '{}'",
                    thumbnail.character_name
                ))?;
            } else {
                let settings = ctx
                    .daemon_config
                    .character_thumbnails
                    .get(&thumbnail.character_name)
                    .copied()
                    .unwrap_or_else(|| {
                        crate::types::CharacterSettings::new(
                            geom.x,
                            geom.y,
                            thumbnail.dimensions.width,
                            thumbnail.dimensions.height,
                        )
                    });

                ctx.daemon_config
                    .save_new_character(&thumbnail.character_name, settings)
                    .context(format!(
                        "Failed to safe-save initial position for new character '{}'",
                        thumbnail.character_name
                    ))?;
            }

            ctx.eve_clients.insert(event.window, thumbnail);
        }
    }
    Ok(())
}

/// Handle DestroyNotify events - remove destroyed window
#[tracing::instrument(skip(ctx), fields(window = event.window))]
fn handle_destroy_notify(ctx: &mut EventContext, event: DestroyNotifyEvent) -> Result<()> {
    // Check if the destroyed window is a known client OR the parent of a known client (if reparented)
    let window_to_remove = if ctx.eve_clients.contains_key(&event.window) {
        Some(event.window)
    } else {
        // Linear search is fine here as we have very few clients (usually < 10)
        ctx.eve_clients
            .iter()
            .find(|(_, thumb)| thumb.parent() == Some(event.window))
            .map(|(win, _)| *win)
    };

    if let Some(win) = window_to_remove {
        info!(
            destroyed_window = event.window,
            client_window = win,
            "DestroyNotify matched EVE client (direct or parent)"
        );
        ctx.cycle_state.remove_window(win);
        ctx.session_state.remove_window(win);
        ctx.eve_clients.remove(&win);
    } else {
        // Log at debug now since we expect many unrelated destroys
        debug!(
            window = event.window,
            "Ignored DestroyNotify for unknown/untracked window"
        );
    }
    Ok(())
}

/// Handle FocusIn events - update focused state and visibility
#[tracing::instrument(skip(ctx), fields(window = event.event))]
fn handle_focus_in(ctx: &mut EventContext, event: FocusInEvent) -> Result<()> {
    debug!(window = event.event, "FocusIn received");

    // Sync cycle state with the focused window
    if ctx.cycle_state.set_current_by_window(event.event) {
        debug!(window = event.event, "Synced cycle state to focused window");
    }

    // Handle "Hide when no focus" logic - Reveal and refresh if needed
    // We check this BEFORE updating the specific focused window to ensure clean state transitions
    if ctx.app_ctx.config.hide_when_no_focus && ctx.eve_clients.values().any(|x| !x.is_visible()) {
        for thumbnail in ctx.eve_clients.values_mut() {
            debug!(character = %thumbnail.character_name, "Revealing thumbnail due to focus change");
            thumbnail.visibility(true).context(format!(
                "Failed to show thumbnail '{}' on focus",
                thumbnail.character_name
            ))?;
            // Force immediate repaint to ensure content is visible (MapWindow doesn't guarantee content)
            thumbnail.update().context(format!(
                "Failed to update thumbnail '{}' on focus reveal",
                thumbnail.character_name
            ))?;
        }
    }

    // Now update the specific window that received focus
    if let Some(thumbnail) = ctx.eve_clients.get_mut(&event.event) {
        // Transition to focused normal state (from minimized or unfocused)
        thumbnail.state = ThumbnailState::Normal { focused: true };
        thumbnail.border(true).context(format!(
            "Failed to update border on focus for '{}'",
            thumbnail.character_name
        ))?;
    }
    Ok(())
}

/// Handle FocusOut events - update focused state and visibility  
#[tracing::instrument(skip(ctx), fields(window = event.event))]
fn handle_focus_out(ctx: &mut EventContext, event: FocusOutEvent) -> Result<()> {
    debug!(window = event.event, "FocusOut received");
    if let Some(thumbnail) = ctx.eve_clients.get_mut(&event.event) {
        // Transition to unfocused normal state
        thumbnail.state = ThumbnailState::Normal { focused: false };
        thumbnail.border(false).context(format!(
            "Failed to clear border on focus loss for '{}'",
            thumbnail.character_name
        ))?;
        if ctx.app_ctx.config.hide_when_no_focus
            && ctx.eve_clients.values().all(|x| !x.state.is_focused())
        {
            for thumbnail in ctx.eve_clients.values_mut() {
                debug!(character = %thumbnail.character_name, "Hiding thumbnail due to focus loss");
                thumbnail.visibility(false).context(format!(
                    "Failed to hide thumbnail '{}' on focus loss",
                    thumbnail.character_name
                ))?;
            }
        }
    }
    Ok(())
}

/// Handle ButtonPress events - start dragging or set current character
#[tracing::instrument(skip(ctx), fields(window = event.event))]
fn handle_button_press(ctx: &mut EventContext, event: ButtonPressEvent) -> Result<()> {
    debug!(
        x = event.root_x,
        y = event.root_y,
        detail = event.detail,
        "ButtonPress received"
    );

    // First, find which window was clicked (if any)
    let clicked_window = ctx
        .eve_clients
        .iter()
        .find(|(_, thumb)| thumb.is_hovered(event.root_x, event.root_y) && thumb.is_visible())
        .map(|(win, _)| *win);

    let Some(clicked_window) = clicked_window else {
        return Ok(()); // No thumbnail was clicked
    };

    // For right-click drags, collect snap targets BEFORE getting mutable reference
    let snap_targets = if event.detail == mouse::BUTTON_RIGHT {
        ctx.eve_clients
            .iter()
            .filter(|(win, t)| **win != clicked_window && t.is_visible())
            .filter_map(|(_, t)| {
                ctx.app_ctx
                    .conn
                    .get_geometry(t.window())
                    .ok()
                    .and_then(|req| req.reply().ok())
                    .map(|geom| Rect {
                        x: geom.x,
                        y: geom.y,
                        width: t.dimensions.width,
                        height: t.dimensions.height,
                    })
            })
            .collect()
    } else {
        Vec::new() // No snap targets needed for left-click
    };

    // Now get mutable reference to the clicked thumbnail
    if let Some(thumbnail) = ctx.eve_clients.get_mut(&clicked_window) {
        debug!(window = thumbnail.window(), character = %thumbnail.character_name, "ButtonPress on thumbnail");
        let geom = ctx
            .app_ctx
            .conn
            .get_geometry(thumbnail.window())
            .context("Failed to send geometry query on button press")?
            .reply()
            .context(format!(
                "Failed to get geometry on button press for '{}'",
                thumbnail.character_name
            ))?;
        thumbnail.input_state.drag_start = Position::new(event.root_x, event.root_y);
        thumbnail.input_state.win_start = Position::new(geom.x, geom.y);

        // Only allow dragging with right-click
        if event.detail == mouse::BUTTON_RIGHT {
            // Store the pre-computed snap targets
            thumbnail.input_state.snap_targets = snap_targets;
            thumbnail.input_state.dragging = true;
            debug!(
                window = thumbnail.window(),
                snap_target_count = thumbnail.input_state.snap_targets.len(),
                "Started dragging thumbnail with cached snap targets"
            );
        }
        // Left-click sets current character for cycling
        if event.detail == mouse::BUTTON_LEFT {
            ctx.cycle_state.set_current(&thumbnail.character_name);
            debug!(character = %thumbnail.character_name, "Set current character via click");
        }
    }
    Ok(())
}

/// Handle ButtonRelease events - focus window and save position after drag
#[tracing::instrument(skip(ctx), fields(window = event.event))]
fn handle_button_release(ctx: &mut EventContext, event: ButtonReleaseEvent) -> Result<()> {
    debug!(
        x = event.root_x,
        y = event.root_y,
        detail = event.detail,
        "ButtonRelease received"
    );

    // First pass: identify the hovered thumbnail by the EVE window key
    let clicked_key = ctx.eve_clients
        .iter()
        .find(|(_, thumb)| {
            let hovered = thumb.is_hovered(event.root_x, event.root_y);
            if hovered {
                debug!(window = thumb.window(), character = %thumb.character_name, "Found hovered thumbnail");
            }
            hovered
        })
        .map(|(eve_window, _)| *eve_window);

    let Some(clicked_key) = clicked_key else {
        debug!("No thumbnail hovered at release position");
        return Ok(());
    };

    let mut clicked_src: Option<Window> = None;
    let is_left_click = event.detail == mouse::BUTTON_LEFT;

    if let Some(thumbnail) = ctx.eve_clients.get_mut(&clicked_key) {
        debug!(window = thumbnail.window(), character = %thumbnail.character_name, "ButtonRelease on thumbnail");
        clicked_src = Some(thumbnail.src());

        // Left-click focuses the window (dragging is right-click only)
        if is_left_click {
            thumbnail.focus().context(format!(
                "Failed to focus window for '{}'",
                thumbnail.character_name
            ))?;
        }

        // Save position after drag ends (right-click release)
        // This saves to disk ONCE per drag operation, not during motion events
        if thumbnail.input_state.dragging {
            // Query actual position from X11
            let geom = ctx
                .app_ctx
                .conn
                .get_geometry(thumbnail.window())
                .context("Failed to send geometry query after drag")?
                .reply()
                .context(format!(
                    "Failed to get geometry after drag for '{}'",
                    thumbnail.character_name
                ))?;

            // Update session state (in-memory only)
            ctx.session_state
                .update_window_position(thumbnail.window(), geom.x, geom.y);

            // NOTE: Update character_thumbnails in memory (for manual saves)
            // Skip logged-out clients with empty character name
            if !thumbnail.character_name.is_empty() {
                let settings = crate::types::CharacterSettings::new(
                    geom.x,
                    geom.y,
                    thumbnail.dimensions.width,
                    thumbnail.dimensions.height,
                );
                ctx.daemon_config
                    .character_thumbnails
                    .insert(thumbnail.character_name.clone(), settings);
            }

            // Conditionally persist to disk based on auto-save setting
            if ctx.daemon_config.profile.thumbnail_auto_save_position {
                debug!(
                    window = thumbnail.window(),
                    x = geom.x,
                    y = geom.y,
                    "Saved position after drag (auto-save enabled)"
                );
                ctx.daemon_config.save().context(format!(
                    "Failed to save position for '{}' after drag",
                    thumbnail.character_name
                ))?;
            } else {
                debug!(
                    window = thumbnail.window(),
                    x = geom.x,
                    y = geom.y,
                    "Updated position in memory (auto-save disabled)"
                );
            }
        }

        // Clear dragging state and free cached snap targets
        thumbnail.input_state.dragging = false;
        thumbnail.input_state.snap_targets.clear();
    }

    // After releasing mutable borrow, optionally minimize other EVE clients
    // This implements the "minimize on switch" feature to keep the workspace clean
    if is_left_click
        && ctx.daemon_config.profile.client_minimize_on_switch
        && let Some(clicked_src) = clicked_src
    {
        for other_window in ctx
            .eve_clients
            .values()
            .filter(|t| t.src() != clicked_src)
            .map(|t| t.src())
        {
            if let Err(e) = minimize_window(
                ctx.app_ctx.conn,
                ctx.app_ctx.screen,
                ctx.app_ctx.atoms,
                other_window,
            ) {
                debug!(error = ?e, window = other_window, "Failed to minimize window");
            }
        }
    }

    Ok(())
}

/// Handle MotionNotify events - process drag motion with snapping
#[tracing::instrument(skip(ctx), fields(window = event.event))]
fn handle_motion_notify(ctx: &mut EventContext, event: MotionNotifyEvent) -> Result<()> {
    trace!(x = event.root_x, y = event.root_y, "MotionNotify received");

    // Find the dragging thumbnail (typically only one at a time)
    let dragging_window = ctx
        .eve_clients
        .iter()
        .find(|(_, t)| t.input_state.dragging)
        .map(|(win, _)| *win);

    let Some(dragging_window) = dragging_window else {
        return Ok(()); // No thumbnail is being dragged
    };

    let snap_threshold = ctx.daemon_config.profile.thumbnail_snap_threshold;

    // Get the dragging thumbnail and clone snap targets to avoid borrow conflict
    // Snap targets were computed once in ButtonPress, avoiding repeated X11 queries
    // Vec<Rect> clone is cheap since Rect is Copy (just copying some i16/u16 values)
    let thumbnail = ctx.eve_clients.get_mut(&dragging_window).unwrap();
    let snap_targets = thumbnail.input_state.snap_targets.clone();

    handle_drag_motion(
        thumbnail,
        &event,
        &snap_targets, // Use cached data (cloned to avoid borrow conflict)
        thumbnail.dimensions.width,
        thumbnail.dimensions.height,
        snap_threshold,
    )
    .context(format!(
        "Failed to handle drag motion for '{}'",
        thumbnail.character_name
    ))?;

    Ok(())
}

/// Handle drag motion for a single thumbnail with snapping
fn handle_drag_motion(
    thumbnail: &mut Thumbnail,
    event: &MotionNotifyEvent,
    snap_targets: &[Rect],
    config_width: u16,
    config_height: u16,
    snap_threshold: u16,
) -> Result<()> {
    if !thumbnail.input_state.dragging {
        return Ok(());
    }

    let dx = event.root_x - thumbnail.input_state.drag_start.x;
    let dy = event.root_y - thumbnail.input_state.drag_start.y;
    let new_x = thumbnail.input_state.win_start.x + dx;
    let new_y = thumbnail.input_state.win_start.y + dy;

    let dragged_rect = Rect {
        x: new_x,
        y: new_y,
        width: config_width,
        height: config_height,
    };

    let Position {
        x: final_x,
        y: final_y,
    } = snapping::find_snap_position(dragged_rect, snap_targets, snap_threshold)
        .unwrap_or_else(|| Position::new(new_x, new_y));

    trace!(
        window = thumbnail.window(),
        from_x = thumbnail.input_state.win_start.x,
        from_y = thumbnail.input_state.win_start.y,
        to_x = final_x,
        to_y = final_y,
        "Dragging thumbnail to new position"
    );

    // Always reposition (let X11 handle no-op if position unchanged)
    thumbnail.reposition(final_x, final_y)?;

    Ok(())
}

pub fn handle_event(ctx: &mut EventContext, event: Event) -> Result<()> {
    match event {
        DamageNotify(event) => handle_damage_notify(ctx, event),
        CreateNotify(event) => handle_create_notify(ctx, event),
        DestroyNotify(event) => handle_destroy_notify(ctx, event),
        Event::FocusIn(event) => handle_focus_in(ctx, event),
        Event::FocusOut(event) => handle_focus_out(ctx, event),
        Event::ButtonPress(event) => handle_button_press(ctx, event),
        Event::ButtonRelease(event) => handle_button_release(ctx, event),
        Event::MotionNotify(event) => handle_motion_notify(ctx, event),
        PropertyNotify(event) => {
            if event.atom == ctx.app_ctx.atoms.wm_name
                && let Some(thumbnail) = ctx.eve_clients.get_mut(&event.window)
                && let Some(eve_window) =
                    is_window_eve(ctx.app_ctx.conn, event.window, ctx.app_ctx.atoms).context(
                        format!(
                            "Failed to check if window {} is EVE client during property change",
                            event.window
                        ),
                    )?
            {
                // Character name changed (login/logout/character switch)
                let old_name = thumbnail.character_name.clone();
                let new_character_name = eve_window.character_name();

                // Track last known character for this window (for logged-out cycling feature)
                ctx.session_state
                    .update_last_character(event.window, new_character_name);

                // Query actual position from X11
                let geom = ctx
                    .app_ctx
                    .conn
                    .get_geometry(thumbnail.window())
                    .context("Failed to send geometry query during character change")?
                    .reply()
                    .context(format!(
                        "Failed to get geometry during character change for window {}",
                        thumbnail.window()
                    ))?;
                let current_pos = Position::new(geom.x, geom.y);

                // Update cycle state with new character name
                ctx.cycle_state
                    .update_character(event.window, new_character_name.to_string());

                // Handle character swap: updates position mapping in config and saves to disk
                // Returns either preserved position (if configured) or current position
                let new_settings = ctx
                    .daemon_config
                    .handle_character_change(
                        &old_name,
                        new_character_name,
                        current_pos,
                        thumbnail.dimensions.width,
                        thumbnail.dimensions.height,
                    )
                    .context(format!(
                        "Failed to handle character change from '{}' to '{}'",
                        old_name, new_character_name
                    ))?;

                // NOTE: We must ensure the NEW character is written to disk immediately.
                // handle_character_change only saves the OLD character (if auto-save is on).
                // If we don't save here, the GUI won't see the new character until the next auto-save.
                if !new_character_name.is_empty() {
                    let final_settings = if let Some(settings) = new_settings {
                        // Character already exists in config; use its saved settings
                        Some(settings)
                    } else {
                        // New/Unseen character found. Determine initial position based on profile settings.
                        let settings = if ctx
                            .daemon_config
                            .profile
                            .thumbnail_preserve_position_on_swap
                        {
                            // Inherit position: User wants new characters to appear where the previous one was
                            crate::types::CharacterSettings::new(
                                current_pos.x,
                                current_pos.y,
                                thumbnail.dimensions.width,
                                thumbnail.dimensions.height,
                            )
                        } else {
                            // Reset position: User disabled inheritance, so snap to default offset from the EVE client window
                            let src_geom = ctx
                                .app_ctx
                                .conn
                                .get_geometry(thumbnail.src())
                                .context("Failed to query source geometry for reset position")?
                                .reply()
                                .context(
                                    "Failed to get source geometry reply for reset position",
                                )?;

                            let default_x =
                                src_geom.x + crate::constants::positioning::DEFAULT_SPAWN_OFFSET;
                            let default_y =
                                src_geom.y + crate::constants::positioning::DEFAULT_SPAWN_OFFSET;

                            crate::types::CharacterSettings::new(
                                default_x,
                                default_y,
                                thumbnail.dimensions.width,
                                thumbnail.dimensions.height,
                            )
                        };

                        // Update memory
                        ctx.daemon_config
                            .character_thumbnails
                            .insert(new_character_name.to_string(), settings);

                        // Persist to disk
                        if ctx.daemon_config.profile.thumbnail_auto_save_position {
                            ctx.daemon_config.save().context(format!(
                                "Failed to save config after character login '{}'",
                                new_character_name
                            ))?;
                        } else {
                            ctx.daemon_config
                                .save_new_character(new_character_name, settings)
                                .context(format!(
                                    "Failed to safe-save config after character login '{}'",
                                    new_character_name
                                ))?;
                        }

                        Some(settings)
                    };

                    // Update session state
                    if let Some(settings) = final_settings {
                        ctx.session_state.update_window_position(
                            event.window,
                            settings.x,
                            settings.y,
                        );
                    }

                    // Update thumbnail (moves/resizes to new settings)
                    thumbnail
                        .set_character_name(new_character_name.to_string(), final_settings)
                        .context(format!(
                            "Failed to update thumbnail after character change from '{}'",
                            old_name
                        ))?;
                } else {
                    // Handle logout: Clear the name, no position/size change
                    thumbnail
                        .set_character_name(String::new(), None)
                        .context(format!(
                            "Failed to clear thumbnail name after logout from '{}'",
                            old_name
                        ))?;
                }
            } else if event.atom == ctx.app_ctx.atoms.wm_name {
                // Check if this is a new EVE window being detected (title change from generic to character name)
                use crate::preview::window_detection::{check_and_create_window, check_eve_window};

                if let Some(character_name) =
                    check_eve_window(ctx.app_ctx, event.window, ctx.session_state).context(
                        format!(
                            "Failed to check if window {} became EVE client",
                            event.window
                        ),
                    )?
                {
                    // Register with cycle state (always, regardless of thumbnail setting)
                    ctx.cycle_state
                        .add_window(character_name.clone(), event.window);

                    // Only create thumbnail if thumbnails are enabled
                    if ctx.app_ctx.config.enabled
                        && let Some(thumbnail) = check_and_create_window(
                            ctx.app_ctx,
                            ctx.daemon_config,
                            event.window,
                            ctx.session_state,
                        )
                        .context(format!(
                            "Failed to create thumbnail for newly detected EVE window {}",
                            event.window
                        ))?
                    {
                        // Save initial position and dimensions for newly detected character
                        let geom = ctx
                            .app_ctx
                            .conn
                            .get_geometry(thumbnail.window())
                            .context("Failed to query geometry for newly detected thumbnail")?
                            .reply()
                            .context("Failed to get geometry reply for newly detected thumbnail")?;

                        // NOTE: Update character_thumbnails in memory (for manual saves)
                        // Skip logged-out clients with empty character name
                        if !thumbnail.character_name.is_empty() {
                            let settings = crate::types::CharacterSettings::new(
                                geom.x,
                                geom.y,
                                thumbnail.dimensions.width,
                                thumbnail.dimensions.height,
                            );
                            ctx.daemon_config
                                .character_thumbnails
                                .insert(thumbnail.character_name.clone(), settings);
                        }

                        // Conditionally persist to disk based on auto-save setting
                        // NOTE: If auto-save is DISABLED, we still perform a "safe save" of ONLY the new character's entry.
                        if ctx.daemon_config.profile.thumbnail_auto_save_position {
                            ctx.daemon_config.save().context(format!(
                                "Failed to save initial position for newly detected character '{}'",
                                thumbnail.character_name
                            ))?;
                        } else {
                            let settings = ctx
                                .daemon_config
                                .character_thumbnails
                                .get(&thumbnail.character_name)
                                .copied()
                                .unwrap_or_else(|| {
                                    crate::types::CharacterSettings::new(
                                        geom.x,
                                        geom.y,
                                        thumbnail.dimensions.width,
                                        thumbnail.dimensions.height,
                                    )
                                });

                            ctx.daemon_config.save_new_character(&thumbnail.character_name, settings).context(format!(
                                "Failed to safe-save initial position for newly detected character '{}'",
                                thumbnail.character_name
                             ))?;
                        }

                        ctx.eve_clients.insert(event.window, thumbnail);
                    }
                }
            } else if event.atom == ctx.app_ctx.atoms.net_wm_state
                && let Some(thumbnail) = ctx.eve_clients.get_mut(&event.window)
                && let Some(mut state) = ctx
                    .app_ctx
                    .conn
                    .get_property(false, event.window, event.atom, AtomEnum::ATOM, 0, 1024)
                    .context(format!(
                        "Failed to query window state for window {}",
                        event.window
                    ))?
                    .reply()
                    .context(format!(
                        "Failed to get window state reply for window {}",
                        event.window
                    ))?
                    .value32()
                && state.any(|s| s == ctx.app_ctx.atoms.net_wm_state_hidden)
            {
                thumbnail.minimized().context(format!(
                    "Failed to set minimized state for '{}'",
                    thumbnail.character_name
                ))?;
            }
            Ok(())
        }
        Event::ConfigureNotify(event) => {
            // Update cached position if window manager moves the thumbnail
            if let Some(_thumbnail) = ctx.eve_clients.get_mut(&event.window) {
                // WARNING: We ignore ConfigureNotify position updates to prevent feedback loops.
                // We are the authority on thumbnail position; the Window Manager might send us weird
                // coordinates (e.g. relative to parent frame) that would corrupt our state if trusted blindly.
                // We only update position via explicit user drag actions.
                /*
                // Only update if position actually changed
                if thumbnail.current_position.x != event.x || thumbnail.current_position.y != event.y {
                    trace!(
                        old_x = thumbnail.current_position.x,
                        old_y = thumbnail.current_position.y,
                        new_x = event.x,
                        new_y = event.y,
                        "Updating cached position from ConfigureNotify"
                    );
                    thumbnail.current_position = Position::new(event.x, event.y);
                }
                */
            }
            Ok(())
        }
        Event::UnmapNotify(event) => {
            if ctx.eve_clients.contains_key(&event.window) {
                info!(
                    window = event.window,
                    "UnmapNotify received for tracked client"
                );
                // TODO: Consider handling Unmap as "hidden" state if not minimized?
            }
            Ok(())
        }
        Event::ReparentNotify(event) => {
            if let Some(thumbnail) = ctx.eve_clients.get_mut(&event.window) {
                info!(
                    window = event.window,
                    parent = event.parent,
                    "ReparentNotify received - updating tracked parent"
                );
                thumbnail.set_parent(Some(event.parent));
            }
            Ok(())
        }
        Event::MapNotify(event) => {
            if ctx.eve_clients.contains_key(&event.window) {
                debug!(
                    window = event.window,
                    "MapNotify received for tracked client"
                );
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
