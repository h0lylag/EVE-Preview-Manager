use anyhow::{Context, Result};
use tracing::{debug, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

use super::super::dispatcher::EventContext;
use super::super::snapping::{self, Rect};
use super::super::thumbnail::Thumbnail;
use crate::common::constants::mouse;
use crate::common::types::Position;

/// Handle ButtonPress events - start dragging or set current character
#[tracing::instrument(skip(ctx), fields(window = event.event))]
pub fn handle_button_press(ctx: &mut EventContext, event: ButtonPressEvent) -> Result<()> {
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
pub fn handle_button_release(ctx: &mut EventContext, event: ButtonReleaseEvent) -> Result<()> {
    use crate::common::ipc::DaemonMessage;
    use crate::x11::minimize_window;

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

        // Collect data we need for border updates before the mutable borrow
        let character_name = thumbnail.character_name.clone();

        // Left-click focuses the window (dragging is right-click only)
        if is_left_click {
            thumbnail
                .focus(event.time)
                .context(format!("Failed to focus window for '{}'", character_name))?;

            // Update cycle state and borders immediately to prevent flash
            ctx.cycle_state.set_current(&character_name);
        }

        // Save position after drag ends (right-click release)
        if thumbnail.input_state.dragging {
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

            ctx.session_state
                .update_window_position(thumbnail.window(), geom.x, geom.y);

            if !thumbnail.character_name.is_empty() {
                let settings = crate::common::types::CharacterSettings::new(
                    geom.x,
                    geom.y,
                    thumbnail.dimensions.width,
                    thumbnail.dimensions.height,
                );

                // Check if this is a Custom Source
                let is_custom_source = ctx
                    .daemon_config
                    .profile
                    .custom_windows
                    .iter()
                    .any(|rule| rule.alias == thumbnail.character_name);

                if is_custom_source {
                    ctx.daemon_config
                        .custom_source_thumbnails
                        .insert(thumbnail.character_name.clone(), settings);
                } else {
                    ctx.daemon_config
                        .character_thumbnails
                        .insert(thumbnail.character_name.clone(), settings);
                }

                let _ = ctx.status_tx.send(DaemonMessage::PositionChanged {
                    name: thumbnail.character_name.clone(),
                    x: geom.x,
                    y: geom.y,
                    width: thumbnail.dimensions.width,
                    height: thumbnail.dimensions.height,
                    is_custom: is_custom_source,
                });
            }

            debug!(
                window = thumbnail.window(),
                x = geom.x,
                y = geom.y,
                "Sent PositionChanged IPC message after drag"
            );
        }

        thumbnail.input_state.dragging = false;
        thumbnail.input_state.snap_targets.clear();
    }

    // After dropping the thumbnail borrow, update borders for left-clicks
    if is_left_click {
        if let Some(thumb) = ctx.eve_clients.get_mut(&clicked_key) {
            // Set active border on clicked window
            thumb.state = crate::common::types::ThumbnailState::Normal { focused: true };
            if let Err(e) = thumb.border(
                ctx.display_config,
                true,
                ctx.cycle_state.is_skipped(&thumb.character_name),
                ctx.font_renderer,
            ) {
                warn!(window = clicked_key, error = %e, "Failed to draw active border after click");
            }
        }

        // Clear borders from ALL other windows (including minimized ones)
        // This ensures we don't leave stale active borders on minimized windows
        for (w, thumb) in ctx.eve_clients.iter_mut() {
            if *w != clicked_key {
                // Only change state for non-minimized windows
                // Minimized windows should stay Minimized - calling border() on them causes
                // double-rendering. Instead, re-call minimized() to properly clear and re-render.
                if thumb.state.is_minimized() {
                    if let Err(e) = thumb.minimized(ctx.display_config, ctx.font_renderer) {
                        warn!(window = *w, error = %e, "Failed to re-render minimized window");
                    }
                } else {
                    thumb.state = crate::common::types::ThumbnailState::Normal { focused: false };
                    if let Err(e) = thumb.border(
                        ctx.display_config,
                        false,
                        ctx.cycle_state.is_skipped(&thumb.character_name),
                        ctx.font_renderer,
                    ) {
                        warn!(window = *w, error = %e, "Failed to clear border during click switch");
                    }
                }
            }
        }

        // Flush X11 connection to ensure border updates are rendered immediately
        let _ = ctx.app_ctx.conn.flush();
    }

    if is_left_click
        && ctx.daemon_config.profile.client_minimize_on_switch
        && let Some(clicked_src) = clicked_src
    {
        // Collect windows to minimize and clear their borders first
        let windows_to_minimize: Vec<Window> = ctx
            .eve_clients
            .iter()
            .filter(|(_, t)| t.src() != clicked_src)
            .filter(|(_, t)| {
                let is_exempt = ctx.daemon_config
                    .custom_source_thumbnails
                    .get(&t.character_name)
                    .or_else(|| ctx.daemon_config.character_thumbnails.get(&t.character_name))
                    .map(|settings| settings.exempt_from_minimize)
                    .unwrap_or(false);

                !is_exempt
            })
            .map(|(w, _)| *w)
            .collect();

        for window in windows_to_minimize {
            // Clear border BEFORE minimizing to prevent stale active borders
            if let Some(thumb) = ctx.eve_clients.get_mut(&window) {
                // Don't change state here - let the minimize handler set it to Minimized
                // Just clear the border for now
                if let Err(e) = thumb.border(
                    ctx.display_config,
                    false,
                    ctx.cycle_state.is_skipped(&thumb.character_name),
                    ctx.font_renderer,
                ) {
                    warn!(window = window, error = %e, "Failed to clear border before minimize");
                }
            }

            if let Err(e) = minimize_window(
                ctx.app_ctx.conn,
                ctx.app_ctx.screen,
                ctx.app_ctx.atoms,
                window,
            ) {
                debug!(error = ?e, window = window, "Failed to minimize window");
            }
        }
    }

    Ok(())
}

/// Handle MotionNotify events - process drag motion with snapping
#[tracing::instrument(skip(ctx), fields(window = event.event))]
pub fn handle_motion_notify(ctx: &mut EventContext, event: MotionNotifyEvent) -> Result<()> {
    use tracing::trace;

    trace!(x = event.root_x, y = event.root_y, "MotionNotify received");

    // Find the dragging thumbnail
    let dragging_window = ctx
        .eve_clients
        .iter()
        .find(|(_, t)| t.input_state.dragging)
        .map(|(win, _)| *win);

    let Some(dragging_window) = dragging_window else {
        return Ok(());
    };

    let snap_threshold = ctx.daemon_config.profile.thumbnail_snap_threshold;

    let thumbnail = ctx
        .eve_clients
        .get_mut(&dragging_window)
        .context("Dragging window not found in clients map")?;
    let snap_targets = thumbnail.input_state.snap_targets.clone();

    handle_drag_motion(
        thumbnail,
        &event,
        &snap_targets,
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
    _config_width: u16,
    _config_height: u16,
    snap_threshold: u16,
) -> Result<()> {
    use tracing::trace;

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
        width: thumbnail.dimensions.width,
        height: thumbnail.dimensions.height,
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
