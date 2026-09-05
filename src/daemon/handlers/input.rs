use anyhow::{Context, Result};
use std::collections::HashMap;
use tracing::{debug, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use super::super::border_update::sync_focused_borders;
use super::super::dispatcher::EventContext;
use super::super::group_drag::{
    GroupDragMember, GroupDragState, is_group_chord_press, shared_delta, translated_position,
};
use super::super::snapping::{self, Rect};
use super::super::thumbnail::Thumbnail;
use super::upsert_spatial_settings;
use crate::common::constants::mouse;
use crate::common::ipc::{DaemonMessage, ThumbnailSpatialUpdate};
use crate::common::types::{Dimensions, Position, SourceIdentity};

fn source_window_for_pointer(
    ctx: &EventContext<'_, '_>,
    event_window: Window,
    root_x: i16,
    root_y: i16,
) -> Option<Window> {
    ctx.eve_clients
        .iter()
        .find(|(_, thumbnail)| thumbnail.window() == event_window && thumbnail.is_visible())
        .or_else(|| {
            ctx.eve_clients.iter().find(|(_, thumbnail)| {
                thumbnail.is_hovered(root_x, root_y) && thumbnail.is_visible()
            })
        })
        .map(|(source_window, _)| *source_window)
}

fn dragging_window(ctx: &EventContext<'_, '_>) -> Option<Window> {
    ctx.eve_clients
        .iter()
        .find(|(_, thumbnail)| thumbnail.input_state.dragging)
        .map(|(source_window, _)| *source_window)
}

fn start_group_drag(ctx: &mut EventContext<'_, '_>, anchor: Window, pointer_start: Position) {
    let members = ctx
        .eve_clients
        .iter_mut()
        .filter_map(|(&source_window, thumbnail)| {
            thumbnail.input_state.dragging = false;
            thumbnail.input_state.snap_targets.clear();
            thumbnail.is_visible().then_some(GroupDragMember {
                source_window,
                start_position: thumbnail.current_position,
            })
        })
        .collect::<Vec<_>>();
    debug_assert!(members.iter().any(|member| member.source_window == anchor));

    debug!(
        anchor,
        member_count = members.len(),
        x = pointer_start.x,
        y = pointer_start.y,
        "Started group thumbnail drag"
    );

    *ctx.group_drag_state = GroupDragState::Active {
        anchor,
        pointer_start,
        members,
    };
}

fn commit_thumbnail_positions(ctx: &mut EventContext<'_, '_>, source_windows: &[Window]) {
    struct Snapshot {
        source_window: Window,
        source: Option<SourceIdentity>,
        position: Position,
        dimensions: Dimensions,
    }

    let snapshots = source_windows
        .iter()
        .filter_map(|source_window| {
            ctx.eve_clients
                .get(source_window)
                .map(|thumbnail| Snapshot {
                    source_window: *source_window,
                    source: thumbnail.effective_source_identity(),
                    position: thumbnail.current_position,
                    dimensions: thumbnail.dimensions,
                })
        })
        .collect::<Vec<_>>();

    let mut updates = Vec::with_capacity(snapshots.len());
    for snapshot in snapshots {
        ctx.session_state.update_window_position(
            snapshot.source_window,
            snapshot.position.x,
            snapshot.position.y,
        );

        let Some(source) = snapshot.source else {
            continue;
        };

        let settings = crate::common::types::CharacterSettings::new(
            snapshot.position.x,
            snapshot.position.y,
            snapshot.dimensions.width,
            snapshot.dimensions.height,
        );
        let target = if source.kind.is_custom() {
            &mut ctx.daemon_config.custom_source_thumbnails
        } else {
            &mut ctx.daemon_config.character_thumbnails
        };
        upsert_spatial_settings(target, &source.name, settings);

        updates.push(ThumbnailSpatialUpdate::new(
            source,
            snapshot.position,
            snapshot.dimensions,
        ));
    }

    if !updates.is_empty() {
        let update_count = updates.len();
        if let Err(error) = ctx
            .status_tx
            .send(DaemonMessage::PositionsChanged { updates })
        {
            warn!(error = %error, update_count, "Failed to send thumbnail position batch");
        } else {
            debug!(update_count, "Sent batched thumbnail position update");
        }
    }
}

/// Restores the captured layout when an external event interrupts a group drag.
/// `excluded_window` identifies a destroyed anchor that can no longer be repositioned.
pub(in crate::daemon) fn cancel_group_drag(
    conn: &RustConnection,
    eve_clients: &mut HashMap<Window, Thumbnail<'_>>,
    group_drag_state: &mut GroupDragState,
    excluded_window: Option<Window>,
) -> Result<usize> {
    let Some(members) = group_drag_state.cancel_active() else {
        return Ok(0);
    };

    let mut queued_positions = Vec::with_capacity(members.len());
    let mut first_error = None;
    for member in members {
        if excluded_window == Some(member.source_window) {
            continue;
        }
        let Some(thumbnail) = eve_clients.get(&member.source_window) else {
            continue;
        };

        let restore_result = thumbnail
            .queue_reposition(member.start_position.x, member.start_position.y)
            .with_context(|| {
                format!(
                    "Failed to restore interrupted group drag for '{}'",
                    thumbnail.character_name
                )
            });
        match restore_result {
            Ok(()) => queued_positions.push((member.source_window, member.start_position)),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }

    let mut flush_succeeded = true;
    if !queued_positions.is_empty()
        && let Err(error) = conn
            .flush()
            .context("Failed to flush restored group thumbnail positions")
    {
        flush_succeeded = false;
        if first_error.is_none() {
            first_error = Some(error);
        }
    }

    if flush_succeeded {
        for (source_window, position) in &queued_positions {
            if let Some(thumbnail) = eve_clients.get_mut(source_window) {
                thumbnail.confirm_reposition(*position);
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(queued_positions.len())
}

fn finish_group_drag(ctx: &mut EventContext<'_, '_>, released_button: u8) {
    let Some(members) = ctx.group_drag_state.finish_active(released_button) else {
        return;
    };

    let moved_windows = members
        .iter()
        .filter_map(|member| {
            ctx.eve_clients
                .get(&member.source_window)
                .filter(|thumbnail| thumbnail.current_position != member.start_position)
                .map(|_| member.source_window)
        })
        .collect::<Vec<_>>();

    commit_thumbnail_positions(ctx, &moved_windows);

    debug!(
        moved_count = moved_windows.len(),
        released_button, "Finished group thumbnail drag"
    );
}

fn set_clicked_cycle_target(
    ctx: &mut EventContext<'_, '_>,
    window: Window,
    source_identity: Option<&SourceIdentity>,
) {
    let cycle_identity = source_identity.cloned().or_else(|| {
        ctx.session_state
            .window_last_character
            .get(&window)
            .map(|name| SourceIdentity::eve(name.clone()))
    });

    ctx.cycle_state
        .set_current_by_window_with_identity(window, cycle_identity.as_ref());

    debug!(
        window = window,
        source = %cycle_identity.as_ref().map(|id| id.name.as_str()).unwrap_or(""),
        "Set current window via thumbnail click"
    );
}

fn remembered_eve_identity(ctx: &EventContext<'_, '_>, window: Window) -> Option<SourceIdentity> {
    ctx.session_state
        .window_last_character
        .get(&window)
        .map(|name| SourceIdentity::eve(name.clone()))
}

/// Handle ButtonPress events - start a single or group drag.
#[tracing::instrument(skip(ctx), fields(window = event.event))]
pub fn handle_button_press(ctx: &mut EventContext, event: ButtonPressEvent) -> Result<()> {
    debug!(
        x = event.root_x,
        y = event.root_y,
        detail = event.detail,
        "ButtonPress received"
    );

    if ctx
        .group_drag_state
        .should_suppress_press(event.detail, event.state)
    {
        return Ok(());
    }

    let pointer_window = source_window_for_pointer(ctx, event.event, event.root_x, event.root_y);

    if is_group_chord_press(event.detail, event.state) {
        // If RMB started a normal drag, promote that exact preview even when X11 reports
        // the second button over a different window under the active pointer grab.
        let visible_drag_owner = dragging_window(ctx).filter(|source_window| {
            ctx.eve_clients
                .get(source_window)
                .is_some_and(Thumbnail::is_visible)
        });
        let Some(anchor) = visible_drag_owner.or(pointer_window) else {
            return Ok(());
        };
        start_group_drag(ctx, anchor, Position::new(event.root_x, event.root_y));
        return Ok(());
    }

    let Some(clicked_window) = pointer_window else {
        return Ok(()); // No thumbnail was clicked
    };

    if event.detail != mouse::BUTTON_RIGHT {
        return Ok(());
    }

    // Collect snap targets before mutably borrowing the dragged thumbnail.
    let snap_targets = ctx
        .eve_clients
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
        .collect();

    // Now get mutable reference to the clicked thumbnail
    if let Some(thumbnail) = ctx.eve_clients.get_mut(&clicked_window) {
        debug!(window = thumbnail.window(), source = %thumbnail.character_name, "ButtonPress on thumbnail");
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

        thumbnail.input_state.snap_targets = snap_targets;
        thumbnail.input_state.dragging = true;
        debug!(
            window = thumbnail.window(),
            snap_target_count = thumbnail.input_state.snap_targets.len(),
            "Started dragging thumbnail with cached snap targets"
        );
    }

    Ok(())
}

/// Handle button releases, completing group drags before normal click/drag behavior.
pub fn handle_button_release(ctx: &mut EventContext, event: ButtonReleaseEvent) -> Result<()> {
    use crate::x11::{activate_window, minimize_window, unminimize_window};

    debug!(
        x = event.root_x,
        y = event.root_y,
        detail = event.detail,
        "ButtonRelease received"
    );

    if ctx
        .group_drag_state
        .consume_suppressed_release(event.detail)
    {
        debug!(detail = event.detail, "Suppressed chord release");
        return Ok(());
    }

    if ctx.group_drag_state.is_active()
        && matches!(event.detail, mouse::BUTTON_LEFT | mouse::BUTTON_RIGHT)
    {
        finish_group_drag(ctx, event.detail);
        return Ok(());
    }

    let pointer_window = source_window_for_pointer(ctx, event.event, event.root_x, event.root_y);
    let clicked_key = if event.detail == mouse::BUTTON_RIGHT {
        // Complete the preview that owns the active RMB drag, even if the pointer was
        // released outside it or over another preview.
        dragging_window(ctx).or(pointer_window)
    } else {
        pointer_window
    };

    let Some(clicked_key) = clicked_key else {
        debug!("No thumbnail hovered at release position");
        return Ok(());
    };

    let mut clicked_src: Option<Window> = None;
    let mut clicked_identity = None;
    let mut finished_single_drag = false;
    let is_left_click = event.detail == mouse::BUTTON_LEFT;

    if let Some(thumbnail) = ctx.eve_clients.get_mut(&clicked_key) {
        debug!(window = thumbnail.window(), source = %thumbnail.character_name, "ButtonRelease on thumbnail");
        let src = thumbnail.src();
        clicked_src = Some(src);

        // Collect data we need for border updates before the mutable borrow
        let character_name = thumbnail.character_name.clone();
        clicked_identity = thumbnail.effective_source_identity();

        // Left-click focuses the window (dragging is right-click only)
        if is_left_click {
            if ctx.daemon_config.profile.client_minimize_on_switch
                && let Err(e) =
                    unminimize_window(ctx.app_ctx.conn, ctx.app_ctx.screen, ctx.app_ctx.atoms, src)
            {
                debug!(
                    window = src,
                    error = ?e,
                    "Failed to unminimize window before click activation"
                );
            }

            activate_window(
                ctx.app_ctx.conn,
                ctx.app_ctx.screen,
                ctx.app_ctx.atoms,
                src,
                event.time,
            )
            .context(format!(
                "Failed to activate window for '{}'",
                character_name
            ))?;
        }

        if event.detail == mouse::BUTTON_RIGHT {
            finished_single_drag = thumbnail.input_state.dragging;
            thumbnail.input_state.dragging = false;
            thumbnail.input_state.snap_targets.clear();
        }
    }

    if finished_single_drag {
        commit_thumbnail_positions(ctx, &[clicked_key]);
    }

    // After dropping the thumbnail borrow, update cycle state and borders for left-clicks.
    if is_left_click {
        if clicked_identity.is_none() {
            clicked_identity = remembered_eve_identity(ctx, clicked_key);
        }
        set_clicked_cycle_target(ctx, clicked_key, clicked_identity.as_ref());

        sync_focused_borders(
            ctx.eve_clients,
            ctx.cycle_state,
            ctx.display_config,
            ctx.font_renderer,
            clicked_key,
            "thumbnail click",
        );

        // Flush X11 connection to ensure border updates are rendered immediately
        let _ = ctx.app_ctx.conn.flush();
    }

    if is_left_click
        && ctx.daemon_config.profile.client_minimize_on_switch
        && let Some(clicked_src) = clicked_src
    {
        // Match the hotkey activation path: let focus settle before minimizing
        // other clients, otherwise some WMs can redirect focus during restore.
        std::thread::sleep(std::time::Duration::from_millis(25));

        // Select from every tracked source, including sources without a rendered preview.
        let windows_to_minimize = super::source_windows_to_minimize(
            ctx.cycle_state,
            ctx.session_state,
            ctx.display_config,
            clicked_src,
        );

        for window in windows_to_minimize {
            // Clear the border when a thumbnail exists; minimization does not require one.
            if let Some(thumb) = ctx.eve_clients.get_mut(&window) {
                // Don't change state here - let the minimize handler set it to Minimized
                // Just clear the border for now
                if let Err(e) = thumb.border(
                    ctx.display_config,
                    false,
                    ctx.cycle_state
                        .is_skipped(thumb.effective_source_identity().as_ref()),
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

/// Move a captured group without snapping, or process a single drag with snapping.
#[tracing::instrument(skip(ctx), fields(window = event.event))]
pub fn handle_motion_notify(ctx: &mut EventContext, event: MotionNotifyEvent) -> Result<()> {
    use tracing::trace;

    trace!(x = event.root_x, y = event.root_y, "MotionNotify received");

    let group_state = &*ctx.group_drag_state;
    let eve_clients = &mut *ctx.eve_clients;
    if let GroupDragState::Active {
        pointer_start,
        members,
        ..
    } = group_state
    {
        let pointer_start = *pointer_start;
        let pointer_now = Position::new(event.root_x, event.root_y);
        let delta = shared_delta(members, pointer_start, pointer_now);

        for member in members {
            let Some(thumbnail) = eve_clients.get(&member.source_window) else {
                continue;
            };
            let position = translated_position(member.start_position, delta);
            thumbnail
                .queue_reposition(position.x, position.y)
                .with_context(|| {
                    format!(
                        "Failed to queue group drag move for '{}'",
                        thumbnail.character_name
                    )
                })?;
        }

        ctx.app_ctx
            .conn
            .flush()
            .context("Failed to flush group thumbnail drag moves")?;
        for member in members {
            if let Some(thumbnail) = eve_clients.get_mut(&member.source_window) {
                let position = translated_position(member.start_position, delta);
                thumbnail.confirm_reposition(position);
            }
        }
        return Ok(());
    }

    // Find the dragging thumbnail
    let Some(dragging_window) = dragging_window(ctx) else {
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
    .with_context(|| {
        format!(
            "Failed to handle drag motion for '{}'",
            thumbnail.character_name
        )
    })?;

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
