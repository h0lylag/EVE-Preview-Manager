use anyhow::{Context, Result};
use std::collections::HashMap;
use std::time::Instant;
use tracing::{debug, info, warn};
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::ErrorKind;
use x11rb::protocol::damage::ConnectionExt as DamageExt;
use x11rb::protocol::xproto::*;

use super::super::dispatcher::EventContext;
use super::super::thumbnail::Thumbnail;
use super::upsert_spatial_settings;
use crate::common::ipc::{DaemonMessage, ThumbnailSpatialUpdate};
use crate::common::types::{
    CharacterSettings, Dimensions, Position, SourceIdentity, SourceKind, ThumbnailState,
};

fn source_window_position(ctx: &crate::x11::AppContext, window: Window) -> Option<Position> {
    ctx.conn
        .get_geometry(window)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|geom| Position::new(geom.x, geom.y))
}

fn remove_from_group_drag(ctx: &mut EventContext<'_, '_>, source_window: Window) {
    if ctx.group_drag_state.anchor() == Some(source_window) {
        match super::input::cancel_group_drag(
            ctx.app_ctx.conn,
            ctx.eve_clients,
            ctx.group_drag_state,
            Some(source_window),
        ) {
            Ok(restored_count) => debug!(
                source_window,
                restored_count, "Cancelled group drag after anchor disappeared"
            ),
            Err(error) => warn!(
                source_window,
                error = %error,
                "Failed to restore group after anchor disappeared"
            ),
        }
    } else {
        ctx.group_drag_state.remove_member(source_window);
    }
}

/// The sole removal boundary for live event handling, including stale render failures.
fn forget_source(ctx: &mut EventContext<'_, '_>, source_window: Window) {
    remove_from_group_drag(ctx, source_window);
    ctx.cycle_state.remove_window(source_window);
    ctx.session_state.remove_window(source_window);
    ctx.eve_clients.remove(&source_window);
    super::state::reconcile_previews(ctx);
}

fn source_window_for_destroy_event(
    destroyed_window: Window,
    active_windows: &HashMap<Window, Option<SourceIdentity>>,
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
) -> Option<Window> {
    if active_windows.contains_key(&destroyed_window) || thumbnails.contains_key(&destroyed_window)
    {
        Some(destroyed_window)
    } else {
        thumbnails
            .iter()
            .find(|(_, thumbnail)| thumbnail.parent() == Some(destroyed_window))
            .map(|(source_window, _)| *source_window)
    }
}

/// Handle DamageNotify events - update damaged thumbnail
pub fn handle_damage_notify(
    ctx: &mut EventContext,
    event: x11rb::protocol::damage::NotifyEvent,
) -> Result<()> {
    // We cannot return early here based on global enabled check, because
    // some thumbnails might have per-source "Always Show" overrides.
    // Instead, we check the override status for the specific thumbnail below.

    if let Some((&source_window, thumbnail)) = ctx
        .eve_clients
        .iter_mut()
        .find(|(_, thumbnail)| thumbnail.damage() == event.damage)
    {
        // NON_EMPTY only notifies after the region becomes empty again. Re-arm before
        // updating, even if rendering is hidden or fails, and retain damage during capture.
        // Draws before the server processes this subtract do not notify again. Content-
        // dependent paths must capture afterwards on this same connection to include them.
        let started = thumbnail.damage_metrics.as_ref().map(|_| Instant::now());
        let update_result = (|| {
            ctx.app_ctx
                .conn
                .damage_subtract(event.damage, 0u32, 0u32)
                .with_context(|| {
                    format!("Failed to subtract damage region (damage={})", event.damage)
                })?;
            thumbnail.update_for_damage(ctx.display_config)
        })();
        if let (Some(started), Some(metrics)) = (started, thumbnail.damage_metrics.as_mut()) {
            metrics.record_damage(update_result.as_ref().ok().copied(), started.elapsed());
        }

        return finish_thumbnail_update(ctx, source_window, update_result.map(|_| ()))
            .with_context(|| {
                format!(
                    "Failed to update thumbnail for damage event (damage={})",
                    event.damage
                )
            });
    }
    Ok(())
}

/// Repaint once at the end of an exposure sequence, and only for our preview windows.
pub fn handle_expose(ctx: &mut EventContext, event: ExposeEvent) -> Result<()> {
    if event.count != 0 {
        return Ok(());
    }
    if let Some((&source_window, thumbnail)) = ctx
        .eve_clients
        .iter_mut()
        .find(|(_, thumbnail)| thumbnail.window() == event.window)
    {
        let result = thumbnail.update_for_expose(ctx.display_config, ctx.font_renderer);
        if let Some(metrics) = &mut thumbnail.damage_metrics {
            metrics.record_expose(result.is_err());
        }
        return finish_thumbnail_update(ctx, source_window, result).with_context(|| {
            format!(
                "Failed to repaint exposed thumbnail (window={})",
                event.window
            )
        });
    }
    Ok(())
}

fn finish_thumbnail_update(
    ctx: &mut EventContext,
    source_window: Window,
    result: Result<()>,
) -> Result<()> {
    if let Err(error) = result {
        if !is_stale_x11_window_error(&error, source_window) {
            return Err(error);
        }
        debug!(source_window, error = %error, "Removing preview for destroyed source window");
        forget_source(ctx, source_window);
    }
    Ok(())
}

/// Paint newly tracked previews and remove sources that disappeared during creation.
pub(in crate::daemon) fn draw_initial_border(
    ctx: &mut EventContext,
    source_window: Window,
) -> Result<()> {
    let Some(thumbnail) = ctx.eve_clients.get(&source_window) else {
        return Ok(());
    };
    let result = thumbnail.border(
        ctx.display_config,
        false,
        ctx.cycle_state
            .is_skipped(thumbnail.effective_source_identity().as_ref()),
        ctx.font_renderer,
    );
    finish_thumbnail_update(ctx, source_window, result)
}

fn is_stale_x11_window_error(error: &anyhow::Error, source_window: Window) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<ReplyError>(),
            Some(ReplyError::X11Error(x11_error))
                if x11_error.error_kind == ErrorKind::Window
                    // The source may disappear between attributes and geometry replies.
                    // Other drawable errors do not justify removing this source's tracking.
                    || (x11_error.error_kind == ErrorKind::Drawable
                        && x11_error.bad_value == source_window
                        && x11_error.major_opcode == GET_GEOMETRY_REQUEST)
        )
    })
}

/// Helper to process a window once it has been identified (used by Create, Map, and Property handlers)
pub fn process_detected_window(
    ctx: &mut EventContext,
    window: Window,
    identity: crate::daemon::window_detection::WindowIdentity,
) -> Result<()> {
    use crate::daemon::window_detection::check_and_create_window;

    debug!(
        window = window,
        character = %identity.name,
        is_custom = identity.is_custom(),
        "Identified window for preview"
    );
    debug!(?identity, "Identity details");

    ctx.cycle_state
        .add_window(identity.cycle_identity(), window);
    // Registration can change the EVE client count before any early return below.
    super::state::reconcile_previews(ctx);

    // MapNotify/PropertyNotify can re-detect a source window that already has a
    // thumbnail, especially around minimize/restore. Refresh in place so the
    // thumbnail keeps its current screen position instead of being recreated.
    if refresh_tracked_window(ctx, window, &identity)? {
        return Ok(());
    }

    match check_and_create_window(
        ctx.app_ctx,
        ctx.daemon_config,
        ctx.display_config,
        window,
        ctx.font_renderer,
        ctx.session_state,
        ctx.eve_clients,
        Some(identity.clone()),
        ctx.cycle_state.eve_client_count(),
    ) {
        Ok(Some(thumbnail)) => {
            let geom_result = ctx
                .app_ctx
                .conn
                .get_geometry(thumbnail.window())
                .map_err(anyhow::Error::from)
                .and_then(|cookie| cookie.reply().map_err(anyhow::Error::from));

            match geom_result {
                Ok(geom) => {
                    let effective_character_name = thumbnail.effective_character_name().to_string();
                    if !effective_character_name.is_empty() {
                        let settings = crate::common::types::CharacterSettings::new(
                            geom.x,
                            geom.y,
                            thumbnail.dimensions.width,
                            thumbnail.dimensions.height,
                        );

                        // Update geometry while preserving saved per-source settings such as
                        // preview mode and style overrides.
                        if identity.is_eve() {
                            upsert_spatial_settings(
                                &mut ctx.daemon_config.character_thumbnails,
                                &effective_character_name,
                                settings.clone(),
                            );
                        } else {
                            upsert_spatial_settings(
                                &mut ctx.daemon_config.custom_source_thumbnails,
                                &effective_character_name,
                                settings.clone(),
                            );
                        }

                        let update = ThumbnailSpatialUpdate::new(
                            SourceIdentity::new(identity.kind, effective_character_name.clone()),
                            Position::new(settings.x, settings.y),
                            settings.dimensions,
                        );
                        let _ = ctx.status_tx.send(DaemonMessage::PositionsChanged {
                            updates: vec![update],
                        });

                        // Only send CharacterDetected if this is a new window (avoid spam from Create+Map)
                        if !ctx.eve_clients.contains_key(&window) {
                            let _ = ctx.status_tx.send(DaemonMessage::CharacterDetected {
                                name: effective_character_name,
                                is_custom: identity.is_custom(),
                            });
                        }

                        // Ask custom apps to paint their first frame; the initial border
                        // below paints the preview even when the source remains idle.
                        if identity.is_custom() {
                            // This fixes issues where apps wait for focus or interaction to paint their first frame
                            let src_geom = ctx
                                .app_ctx
                                .conn
                                .get_geometry(window)
                                .context("Failed to get geometry for custom source expose")?
                                .reply()
                                .context("Failed to receive geometry reply")?;

                            let expose = ExposeEvent {
                                response_type: EXPOSE_EVENT,
                                sequence: 0,
                                window,
                                x: 0,
                                y: 0,
                                width: src_geom.width,
                                height: src_geom.height,
                                count: 0,
                            };

                            if let Err(e) = ctx.app_ctx.conn.send_event(
                                false,
                                window,
                                EventMask::EXPOSURE,
                                expose,
                            ) {
                                tracing::warn!(
                                    "Failed to send Expose event to {}: {}",
                                    thumbnail.character_name,
                                    e
                                );
                            }
                            let _ = ctx.app_ctx.conn.flush();
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to query geometry for new thumbnail window {}: {}",
                        thumbnail.window(),
                        e
                    );
                }
            }

            ctx.eve_clients.insert(window, thumbnail);

            if let Err(error) = draw_initial_border(ctx, window) {
                tracing::warn!(window, %error, "Failed to draw initial border");
            }
        }
        Ok(None) => {
            // NOTE: Even with rendering disabled, new EVE characters and custom sources
            // must reach the Manager via PositionsChanged so they appear for configuration.
            if !ctx.display_config.enabled && !identity.name.is_empty() {
                let is_new = if identity.is_eve() {
                    !ctx.daemon_config
                        .character_thumbnails
                        .contains_key(&identity.name)
                        && !ctx
                            .daemon_config
                            .profile
                            .character_thumbnails
                            .contains_key(&identity.name)
                } else {
                    !ctx.daemon_config
                        .custom_source_thumbnails
                        .contains_key(&identity.name)
                        && !ctx
                            .daemon_config
                            .profile
                            .custom_source_thumbnails
                            .contains_key(&identity.name)
                };

                if is_new {
                    let (w, h) = (
                        ctx.daemon_config.profile.thumbnail_default_width,
                        ctx.daemon_config.profile.thumbnail_default_height,
                    );
                    let spawn_position = ctx
                        .daemon_config
                        .fallback_new_thumbnail_position(source_window_position(
                            ctx.app_ctx,
                            window,
                        ))
                        .unwrap_or_default();

                    let settings = crate::common::types::CharacterSettings::new(
                        spawn_position.x,
                        spawn_position.y,
                        w,
                        h,
                    );
                    if identity.is_eve() {
                        ctx.daemon_config
                            .character_thumbnails
                            .insert(identity.name.clone(), settings);
                    } else {
                        ctx.daemon_config
                            .custom_source_thumbnails
                            .insert(identity.name.clone(), settings);
                    }
                    let update = ThumbnailSpatialUpdate::new(
                        identity.source_identity(),
                        spawn_position,
                        Dimensions::new(w, h),
                    );
                    let _ = ctx.status_tx.send(DaemonMessage::PositionsChanged {
                        updates: vec![update],
                    });
                    let _ = ctx.status_tx.send(DaemonMessage::CharacterDetected {
                        name: identity.name.clone(),
                        is_custom: identity.is_custom(),
                    });
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                window = window,
                error = %e,
                "Failed to create thumbnail"
            );
        }
    }
    super::super::activation::reconcile(ctx, std::time::Instant::now());
    Ok(())
}

/// Whether a source is actually viewable. An unanswered query keeps minimized rendering.
fn source_is_viewable(ctx: &EventContext, window: Window) -> bool {
    let attributes = ctx
        .app_ctx
        .conn
        .get_window_attributes(window)
        .map_err(anyhow::Error::from)
        .and_then(|cookie| cookie.reply().map_err(anyhow::Error::from));
    match attributes {
        Ok(attributes) => attributes.map_state == MapState::VIEWABLE,
        Err(error) => {
            tracing::warn!(window, %error, "Failed to verify source map state; keeping minimized rendering");
            false
        }
    }
}

fn refresh_tracked_window(
    ctx: &mut EventContext,
    window: Window,
    identity: &crate::daemon::window_detection::WindowIdentity,
) -> Result<bool> {
    use crate::x11::is_window_minimized;

    if !ctx.eve_clients.contains_key(&window) {
        return Ok(false);
    }

    // WM state properties can be absent or lag, and a stale MapNotify can arrive while a
    // restore is refused. Leave minimized rendering only once the source is actually viewable.
    let was_minimized = ctx.eve_clients[&window].state.is_minimized();
    let is_minimized = is_window_minimized(ctx.app_ctx.conn, window, ctx.app_ctx.atoms)
        .unwrap_or(false)
        || (was_minimized && !source_is_viewable(ctx, window));

    let mut position_changed = None;

    if let Some(thumbnail) = ctx.eve_clients.get_mut(&window) {
        if identity.is_eve() {
            thumbnail.sync_remembered_character_name(
                ctx.session_state
                    .window_last_character
                    .get(&window)
                    .cloned(),
            );
        }

        let effective_character_name = thumbnail.effective_character_name().to_string();
        if !effective_character_name.is_empty() {
            let mut settings = if identity.is_eve() {
                ctx.daemon_config
                    .character_thumbnails
                    .get(&effective_character_name)
                    .or_else(|| {
                        ctx.daemon_config
                            .profile
                            .character_thumbnails
                            .get(&effective_character_name)
                    })
                    .cloned()
            } else {
                ctx.daemon_config
                    .custom_source_thumbnails
                    .get(&effective_character_name)
                    .or_else(|| {
                        ctx.daemon_config
                            .profile
                            .custom_source_thumbnails
                            .get(&effective_character_name)
                    })
                    .cloned()
            }
            .or_else(|| {
                ctx.display_config
                    .settings_for(identity.kind, &effective_character_name)
                    .cloned()
            })
            .unwrap_or_else(|| {
                let mut settings = CharacterSettings::new(
                    thumbnail.current_position.x,
                    thumbnail.current_position.y,
                    thumbnail.dimensions.width,
                    thumbnail.dimensions.height,
                );
                settings.preview_mode = thumbnail.preview_mode.clone();
                settings
            });

            settings.x = thumbnail.current_position.x;
            settings.y = thumbnail.current_position.y;
            settings.dimensions = thumbnail.dimensions;

            let changed = if identity.is_eve() {
                upsert_spatial_settings(
                    &mut ctx.daemon_config.character_thumbnails,
                    &effective_character_name,
                    settings.clone(),
                )
            } else {
                upsert_spatial_settings(
                    &mut ctx.daemon_config.custom_source_thumbnails,
                    &effective_character_name,
                    settings.clone(),
                )
            };

            if changed {
                position_changed = Some(ThumbnailSpatialUpdate::new(
                    SourceIdentity::new(identity.kind, effective_character_name),
                    Position::new(settings.x, settings.y),
                    settings.dimensions,
                ));
            }
        }

        if is_minimized {
            thumbnail
                .minimized(ctx.display_config, ctx.font_renderer)
                .context(format!(
                    "Failed to refresh minimized state for '{}'",
                    thumbnail.character_name
                ))?;
        } else {
            thumbnail.state = ThumbnailState::Normal { focused: false };
            thumbnail
                .update(ctx.display_config, ctx.font_renderer)
                .context(format!(
                    "Failed to refresh restored thumbnail for '{}'",
                    thumbnail.character_name
                ))?;
        }
    }

    if let Some(update) = position_changed {
        let _ = ctx.status_tx.send(DaemonMessage::PositionsChanged {
            updates: vec![update],
        });
    }

    // Reconcile first so a focused or requested window is drawn once, with its final border;
    // only windows left unfocused need the inactive border.
    super::super::activation::reconcile(ctx, std::time::Instant::now());
    if !is_minimized
        && let Some(thumb) = ctx.eve_clients.get_mut(&window)
        && !thumb.state.is_focused()
        && let Err(error) = thumb.border(
            ctx.display_config,
            false,
            ctx.cycle_state
                .is_skipped(thumb.effective_source_identity().as_ref()),
            ctx.font_renderer,
        )
    {
        tracing::warn!(window, %error, "Failed to draw inactive border for refreshed window");
    }

    Ok(true)
}

/// Handle CreateNotify events - create thumbnail for a newly detected source window.
pub fn handle_create_notify(ctx: &mut EventContext, event: CreateNotifyEvent) -> Result<()> {
    debug!(window = event.window, "CreateNotify received");

    // Identification excludes EPM windows before subscribing to late title/class
    // updates, preserving the preview's existing mouse subscriptions.
    observe_source(ctx, event.window, true)
}

/// Handle MapNotify events - catch windows becoming visible
pub fn handle_map_notify(ctx: &mut EventContext, event: MapNotifyEvent) -> Result<()> {
    debug!(window = event.window, "MapNotify received");
    observe_source(ctx, event.window, true)
}

/// Handle DestroyNotify events - remove destroyed window
pub fn handle_destroy_notify(ctx: &mut EventContext, event: DestroyNotifyEvent) -> Result<()> {
    let window_to_remove = source_window_for_destroy_event(
        event.window,
        ctx.cycle_state.get_active_windows(),
        ctx.eve_clients,
    );
    // Evaluate before removal: removing the source is what makes its transaction end.
    let affects_focus = window_to_remove.is_some()
        || super::super::activation::structure_change_affects_focus(ctx, event.window);

    if let Some(win) = window_to_remove {
        info!(
            destroyed_window = event.window,
            client_window = win,
            "DestroyNotify matched tracked source (direct or parent)"
        );
        forget_source(ctx, win);
    } else {
        debug!(
            window = event.window,
            "Ignored DestroyNotify for unknown/untracked window"
        );
    }
    if affects_focus {
        super::super::activation::reconcile(ctx, std::time::Instant::now());
    }
    Ok(())
}

/// Handle PropertyNotify for identity changes (WM_NAME or WM_CLASS) to detect late-identifying windows
pub fn handle_identity_update(ctx: &mut EventContext, window: Window) -> Result<()> {
    observe_source(ctx, window, false)
}

/// Apply an identity observation from CreateNotify, MapNotify, or a WM_NAME/WM_CLASS change.
///
/// Unregistered windows go through full detection. A registered window keeps the kind, and a
/// custom source its alias, from admission until removal; only an EVE client's character name
/// changes. That rename reaches the preview before any refresh, so the registry and preview
/// agree whichever event observes it first. A broad custom rule that matches an EVE client
/// before it sets its EVE title therefore keeps it custom until the daemon rescans.
fn observe_source(ctx: &mut EventContext, window: Window, remapped: bool) -> Result<()> {
    use crate::daemon::window_detection::{WindowIdentity, identify_window, match_custom_rule};
    use crate::x11::is_window_eve;

    let Some(kind) = ctx.cycle_state.admitted_kind(window) else {
        if let Some(identity) = identify_window(
            ctx.app_ctx,
            window,
            ctx.session_state,
            &ctx.daemon_config.profile.custom_windows,
        )
        .context(format!("Failed to identify window {}", window))?
        {
            process_detected_window(ctx, window, identity)?;
        }
        return Ok(());
    };
    let rendered = ctx.eve_clients.contains_key(&window);

    let identity = match kind {
        SourceKind::Eve => {
            match is_window_eve(ctx.app_ctx.conn, window, ctx.app_ctx.atoms).context(format!(
                "Failed to check if window {} is EVE client during identity change",
                window
            ))? {
                Some(eve_window) => {
                    let name = eve_window.character_name().to_string();
                    if rendered {
                        apply_eve_rename(ctx, window, &name)?;
                    } else {
                        ctx.session_state.update_last_character(window, &name);
                    }
                    Some(WindowIdentity::new_eve(name))
                }
                // A title that stops matching never demotes an EVE client.
                None => ctx
                    .eve_clients
                    .get(&window)
                    .map(|thumbnail| WindowIdentity::new_eve(thumbnail.character_name.clone())),
            }
        }
        // Title changes cannot alter a rendered custom preview; skip the rule queries.
        SourceKind::Custom if rendered && !remapped => None,
        SourceKind::Custom => {
            let alias = ctx
                .cycle_state
                .get_active_windows()
                .get(&window)
                .cloned()
                .flatten()
                .map(|identity| identity.name)
                .unwrap_or_default();
            match match_custom_rule(
                ctx.app_ctx,
                window,
                &ctx.daemon_config.profile.custom_windows,
            )
            .context(format!(
                "Failed to match custom rules for window {}",
                window
            ))? {
                Some(identity) if identity.name == alias => Some(identity),
                // No title reclassifies a custom source, and its alias stays the admitted one.
                _ => rendered.then(|| WindowIdentity {
                    rule: ctx
                        .daemon_config
                        .profile
                        .custom_windows
                        .iter()
                        .find(|rule| rule.alias == alias)
                        .cloned(),
                    name: alias,
                    kind: SourceKind::Custom,
                }),
            }
        }
    };

    // A rendered preview refreshes only when its source maps again.
    match identity {
        Some(identity) if remapped || !rendered => process_detected_window(ctx, window, identity),
        _ => Ok(()),
    }
}

/// Rename a rendered EVE client's preview after login, logout, or a character swap.
fn apply_eve_rename(
    ctx: &mut EventContext,
    window: Window,
    new_character_name: &str,
) -> Result<()> {
    let old_name = ctx.eve_clients[&window].character_name.clone();

    // Repeated property notifications must not interrupt a click or drag.
    if old_name == new_character_name {
        return Ok(());
    }
    // Restore the old identity's layout before capturing geometry or applying the
    // new identity's settings. Those settings may hide the preview via an override.
    super::input::cancel_preview_input(ctx, window);
    let thumbnail = ctx
        .eve_clients
        .get_mut(&window)
        .expect("Checked contains_key");

    if !new_character_name.is_empty() {
        ctx.session_state
            .update_last_character(window, new_character_name);
        thumbnail.sync_remembered_character_name(
            ctx.session_state
                .window_last_character
                .get(&window)
                .cloned(),
        );
    } else if !old_name.is_empty() {
        ctx.session_state.update_last_character(window, &old_name);
        thumbnail.sync_remembered_character_name(
            ctx.session_state
                .window_last_character
                .get(&window)
                .cloned(),
        );
    }

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

    ctx.cycle_state
        .update_character(window, new_character_name.to_string());

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

    if !new_character_name.is_empty() {
        let final_settings = if let Some(settings) = new_settings {
            Some(settings)
        } else {
            let session_position = ctx
                .daemon_config
                .profile
                .thumbnail_preserve_position_on_swap
                .then_some(current_pos);
            let source_position = if session_position.is_none()
                && !ctx.daemon_config.profile.thumbnail_default_position_enabled
            {
                let src_geom = ctx
                    .app_ctx
                    .conn
                    .get_geometry(thumbnail.src())
                    .context("Failed to query source geometry for reset position")?
                    .reply()
                    .context("Failed to get source geometry reply for reset position")?;
                Some(Position::new(src_geom.x, src_geom.y))
            } else {
                source_window_position(ctx.app_ctx, thumbnail.src())
            };
            let default_position = ctx
                .daemon_config
                .resolve_initial_thumbnail_position(None, None, session_position, source_position)
                .expect("session/default/source position should be available");
            let settings = crate::common::types::CharacterSettings::new(
                default_position.x,
                default_position.y,
                thumbnail.dimensions.width,
                thumbnail.dimensions.height,
            );

            ctx.daemon_config
                .character_thumbnails
                .insert(new_character_name.to_string(), settings.clone());

            let _ = ctx.status_tx.send(DaemonMessage::CharacterDetected {
                name: new_character_name.to_string(),
                is_custom: false,
            });

            let update = ThumbnailSpatialUpdate::new(
                SourceIdentity::eve(new_character_name),
                Position::new(settings.x, settings.y),
                settings.dimensions,
            );
            let _ = ctx.status_tx.send(DaemonMessage::PositionsChanged {
                updates: vec![update],
            });

            Some(settings)
        };

        if let Some(ref settings) = final_settings {
            ctx.session_state
                .update_window_position(window, settings.x, settings.y);
        }

        thumbnail
            .set_character_name(
                new_character_name.to_string(),
                final_settings,
                ctx.cycle_state
                    .is_skipped(Some(&SourceIdentity::eve(new_character_name))),
                ctx.display_config,
                ctx.font_renderer,
            )
            .context(format!(
                "Failed to update thumbnail after character change from '{}'",
                old_name
            ))?;
    } else {
        thumbnail
            .set_character_name(
                String::new(),
                None,
                ctx.cycle_state
                    .is_skipped(thumbnail.effective_source_identity().as_ref()),
                ctx.display_config,
                ctx.font_renderer,
            )
            .context(format!(
                "Failed to clear thumbnail name after logout from '{}'",
                old_name
            ))?;
    }
    super::state::reconcile_previews(ctx);
    Ok(())
}

/// Handle ConfigureNotify events - update cached source dimensions
#[tracing::instrument(skip(ctx), fields(window = event.window))]
pub fn handle_configure_notify(ctx: &mut EventContext, event: ConfigureNotifyEvent) -> Result<()> {
    if let Some(thumbnail) = ctx.eve_clients.get_mut(&event.window) {
        // NOTE: This call is effectively a no-op.
        // We stopped caching source dimensions here to fix a race condition where
        // the event loop sees valid dimensions but the X server sees 1x1/unmapped.
        // Geometry is now queried freshly in `renderer::capture()`.
        thumbnail.update_source_dimensions(event.width, event.height);

        tracing::debug!(
            window = event.window,
            width = event.width,
            height = event.height,
            "Updated source dimensions from ConfigureNotify"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn stale_geometry_cleanup_preserves_other_tracking() {
        use crate::common::types::PreviewMode;
        use crate::config::{DaemonConfig, profile::Profile};
        use crate::daemon::{
            cycle_state::CycleState,
            font::FontRenderer,
            group_drag::{ChordButtons, GroupDragMember, GroupDragState},
            session_state::SessionState,
        };
        use crate::x11::{AppContext, CachedAtoms, CachedFormats};

        assert_eq!(std::env::var("EPM_X11_TESTS").as_deref(), Ok("1"));
        for lost_anchor in [false, true] {
            let (conn, screen_number) = x11rb::connect(None).unwrap();
            conn.damage_query_version(1, 1).unwrap().reply().unwrap();
            let screen = &conn.setup().roots[screen_number];
            let atoms = CachedAtoms::new(&conn).unwrap();
            let formats = CachedFormats::new(&conn, screen).unwrap();
            let app_ctx = AppContext {
                conn: &conn,
                screen,
                atoms: &atoms,
                formats: &formats,
            };
            let font = FontRenderer::resolve_from_config(&conn, "sans-serif", 12.0).unwrap();
            let mut config = DaemonConfig {
                profile: Profile::default(),
                character_thumbnails: HashMap::new(),
                custom_source_thumbnails: HashMap::new(),
                profile_hotkeys: HashMap::new(),
                runtime_hidden: false,
            };
            let display = config.build_display_config();
            let mut thumbnails = HashMap::new();
            let mut cycle = CycleState::new(Vec::new());
            let mut session = SessionState::new();
            let mut members = Vec::new();
            for (name, x) in [("Lost", 600), ("Survivor", 900)] {
                let src = conn.generate_id().unwrap();
                conn.create_window(
                    screen.root_depth,
                    src,
                    screen.root,
                    0,
                    0,
                    40,
                    40,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    screen.root_visual,
                    &CreateWindowAux::new(),
                )
                .unwrap()
                .check()
                .unwrap();
                conn.map_window(src).unwrap().check().unwrap();
                let identity = SourceIdentity::eve(name);
                let start_position = Position::new(x, 0);
                let mut thumbnail = Thumbnail::new(
                    &app_ctx,
                    identity.kind,
                    identity.name.clone(),
                    None,
                    src,
                    &display,
                    &font,
                    Some(start_position),
                    Dimensions::new(100, 60),
                    PreviewMode::Live,
                    false,
                )
                .unwrap();
                thumbnail.reposition(x, 50).unwrap();
                thumbnails.insert(src, thumbnail);
                cycle.add_window(Some(identity), src);
                session.update_window_position(src, x, 50);
                session.update_last_character(src, name);
                members.push(GroupDragMember {
                    source_window: src,
                    start_position,
                });
            }
            let source = members[0].source_window;
            let survivor = members[1].source_window;
            let survivor_start = members[1].start_position;
            let survivor_moved = thumbnails[&survivor].current_position;
            let anchor = if lost_anchor { source } else { survivor };
            let mut drag = GroupDragState::Active {
                anchor,
                pointer_start: Position::new(0, 0),
                members,
            };
            let (tx, _rx) = ipc_channel::ipc::channel().unwrap();
            let mut ctx = EventContext {
                app_ctx: &app_ctx,
                daemon_config: &mut config,
                eve_clients: &mut thumbnails,
                session_state: &mut session,
                cycle_state: &mut cycle,
                group_drag_state: &mut drag,
                status_tx: &tx,
                font_renderer: &font,
                display_config: &display,
            };

            // Reproduce the actual error shape, not the timing of the pipelined-query race.
            conn.get_window_attributes(source).unwrap().reply().unwrap();
            conn.destroy_window(source).unwrap().check().unwrap();
            let unrelated = conn.generate_id().unwrap();
            let unrelated_error = conn.get_geometry(unrelated).unwrap().reply().unwrap_err();
            let other_request_error = conn
                .get_image(ImageFormat::Z_PIXMAP, source, 0, 0, 1, 1, u32::MAX)
                .unwrap()
                .reply()
                .unwrap_err();
            for error in [unrelated_error, other_request_error] {
                assert!(
                    matches!(&error, ReplyError::X11Error(packet) if packet.error_kind == ErrorKind::Drawable)
                );
                assert!(finish_thumbnail_update(&mut ctx, source, Err(error.into())).is_err());
                assert_eq!(ctx.eve_clients.len(), 2);
                assert_eq!(ctx.cycle_state.get_active_windows().len(), 2);
                assert_eq!(ctx.session_state.window_positions.len(), 2);
                assert_eq!(ctx.session_state.window_last_character.len(), 2);
                assert_eq!(ctx.group_drag_state.anchor(), Some(anchor));
                assert!(
                    matches!(ctx.group_drag_state, GroupDragState::Active { members, .. } if members.len() == 2)
                );
            }
            let error = conn.get_geometry(source).unwrap().reply().unwrap_err();
            assert!(matches!(&error, ReplyError::X11Error(packet)
                if packet.error_kind == ErrorKind::Drawable
                    && packet.bad_value == source && packet.major_opcode == GET_GEOMETRY_REQUEST));
            finish_thumbnail_update(
                &mut ctx,
                source,
                Err(anyhow::Error::new(error).context("source capture")),
            )
            .unwrap();
            assert!(!ctx.eve_clients.contains_key(&source));
            assert!(!ctx.cycle_state.get_active_windows().contains_key(&source));
            assert!(!ctx.session_state.window_positions.contains_key(&source));
            assert!(
                !ctx.session_state
                    .window_last_character
                    .contains_key(&source)
            );
            assert!(ctx.eve_clients.contains_key(&survivor));
            assert!(ctx.cycle_state.get_active_windows().contains_key(&survivor));
            assert_eq!(
                ctx.session_state.window_positions[&survivor],
                survivor_moved
            );
            assert_eq!(
                ctx.session_state.window_last_character[&survivor],
                "Survivor"
            );
            if lost_anchor {
                assert!(matches!(
                    ctx.group_drag_state,
                    GroupDragState::SuppressingRelease(ChordButtons::Both)
                ));
                assert_eq!(ctx.eve_clients[&survivor].current_position, survivor_start);
                let geometry = conn
                    .get_geometry(ctx.eve_clients[&survivor].window())
                    .unwrap()
                    .reply()
                    .unwrap();
                assert_eq!(Position::new(geometry.x, geometry.y), survivor_start);
            } else {
                assert!(
                    matches!(ctx.group_drag_state, GroupDragState::Active { anchor, members, .. }
                    if *anchor == survivor && members.len() == 1 && members[0].source_window == survivor)
                );
                assert_eq!(ctx.eve_clients[&survivor].current_position, survivor_moved);
            }
        }
    }

    #[test]
    fn destroy_matcher_recognizes_tracked_source_without_thumbnail() {
        let active_windows = HashMap::from([(42, None)]);
        let thumbnails = HashMap::new();

        assert_eq!(
            source_window_for_destroy_event(42, &active_windows, &thumbnails),
            Some(42)
        );
        assert_eq!(
            source_window_for_destroy_event(99, &active_windows, &thumbnails),
            None
        );
    }
}
