use anyhow::{Context, Result};
use tracing::{debug, info, warn};
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::ErrorKind;
use x11rb::protocol::damage::ConnectionExt as DamageExt;
use x11rb::protocol::xproto::*;

use super::super::border_update::sync_focused_borders;
use super::super::dispatcher::EventContext;
use super::upsert_spatial_settings;
use crate::common::ipc::{DaemonMessage, ThumbnailSpatialUpdate};
use crate::common::types::{
    CharacterSettings, Dimensions, Position, SourceIdentity, ThumbnailState,
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

/// Handle DamageNotify events - update damaged thumbnail
pub fn handle_damage_notify(
    ctx: &mut EventContext,
    event: x11rb::protocol::damage::NotifyEvent,
) -> Result<()> {
    // We cannot return early here based on global enabled check, because
    // some thumbnails might have per-source "Always Show" overrides.
    // Instead, we check the override status for the specific thumbnail below.

    if let Some(source_window) = ctx
        .eve_clients
        .iter()
        .find(|(_, thumbnail)| thumbnail.damage() == event.damage)
        .map(|(source_window, _)| *source_window)
    {
        let update_result = ctx
            .eve_clients
            .get_mut(&source_window)
            .expect("damage lookup returned an existing thumbnail")
            .update(ctx.display_config, ctx.font_renderer);

        if let Err(error) = update_result {
            if is_stale_x11_window_error(&error) {
                let character = ctx
                    .eve_clients
                    .get(&source_window)
                    .map(|thumbnail| thumbnail.effective_character_name().to_string())
                    .unwrap_or_default();

                debug!(
                    damage = event.damage,
                    source_window = source_window,
                    character = %character,
                    error = %error,
                    "Ignoring damage event for destroyed source window"
                );

                remove_from_group_drag(ctx, source_window);
                ctx.cycle_state.remove_window(source_window);
                ctx.session_state.remove_window(source_window);
                ctx.eve_clients.remove(&source_window);
                return Ok(());
            }

            return Err(error).context(format!(
                "Failed to update thumbnail for damage event (damage={})",
                event.damage
            ));
        }

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

fn is_stale_x11_window_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<ReplyError>(),
            Some(ReplyError::X11Error(x11_error)) if x11_error.error_kind == ErrorKind::Window
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

    let cycle_identity = (!identity.name.is_empty()).then(|| identity.source_identity());
    ctx.cycle_state.add_window(cycle_identity, window);

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
    ) {
        Ok(Some(mut thumbnail)) => {
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

                        // Force initial update for custom sources as they might not emit Damage events immediately
                        if identity.is_custom() {
                            // 1. Attempt immediate capture
                            if let Err(e) = thumbnail.update(ctx.display_config, ctx.font_renderer)
                            {
                                tracing::warn!(
                                    "Failed to perform initial update for custom source {}: {}",
                                    thumbnail.character_name,
                                    e
                                );
                            }

                            // 2. Send synthetic Expose event to force the application to repaint
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

            // Check whether this newly created thumbnail belongs to the active source
            // window. Detection can arrive after activation but before FocusIn.
            let is_actually_focused = crate::x11::get_active_window(
                ctx.app_ctx.conn,
                ctx.app_ctx.screen,
                ctx.app_ctx.atoms,
            )
            .unwrap_or(None)
            .map(|active| active == window)
            .unwrap_or(false);

            if is_actually_focused {
                sync_focused_borders(
                    ctx.eve_clients,
                    ctx.cycle_state,
                    ctx.display_config,
                    ctx.font_renderer,
                    window,
                    "restored focused window",
                );
            } else {
                // Not focused, just draw inactive border
                if let Some(thumb) = ctx.eve_clients.get_mut(&window)
                    && let Err(e) = thumb.border(
                        ctx.display_config,
                        false,
                        ctx.cycle_state
                            .is_skipped(thumb.effective_source_identity().as_ref()),
                        ctx.font_renderer,
                    )
                {
                    tracing::warn!(window = window, error = %e, "Failed to draw initial border for new window");
                }
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
    Ok(())
}

fn refresh_tracked_window(
    ctx: &mut EventContext,
    window: Window,
    identity: &crate::daemon::window_detection::WindowIdentity,
) -> Result<bool> {
    use crate::x11::{get_active_window, is_window_minimized};

    if !ctx.eve_clients.contains_key(&window) {
        return Ok(false);
    }

    let is_actually_focused =
        get_active_window(ctx.app_ctx.conn, ctx.app_ctx.screen, ctx.app_ctx.atoms)
            .unwrap_or(None)
            .map(|active| active == window)
            .unwrap_or(false);
    let is_minimized =
        is_window_minimized(ctx.app_ctx.conn, window, ctx.app_ctx.atoms).unwrap_or(false);

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

        if thumbnail.character_name != identity.name {
            debug!(
                window = window,
                current = %thumbnail.character_name,
                detected = %identity.name,
                "Tracked window identity changed during detection; waiting for property handler"
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

    if is_actually_focused {
        sync_focused_borders(
            ctx.eve_clients,
            ctx.cycle_state,
            ctx.display_config,
            ctx.font_renderer,
            window,
            "tracked window refreshed",
        );
    } else if !is_minimized
        && let Some(thumb) = ctx.eve_clients.get_mut(&window)
        && let Err(e) = thumb.border(
            ctx.display_config,
            false,
            ctx.cycle_state
                .is_skipped(thumb.effective_source_identity().as_ref()),
            ctx.font_renderer,
        )
    {
        tracing::warn!(window = window, error = %e, "Failed to draw inactive border for refreshed window");
    }

    Ok(true)
}

/// Handle CreateNotify events - create thumbnail for a newly detected source window.
pub fn handle_create_notify(ctx: &mut EventContext, event: CreateNotifyEvent) -> Result<()> {
    use crate::daemon::window_detection::identify_window;

    debug!(window = event.window, "CreateNotify received");

    // Subscribe to property changes so we can detect late-identifying windows (e.g. WM_CLASS set after creation)
    let _ = ctx.app_ctx.conn.change_window_attributes(
        event.window,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    );

    if let Some(identity) = identify_window(
        ctx.app_ctx,
        event.window,
        ctx.session_state,
        &ctx.daemon_config.profile.custom_windows,
    )
    .context(format!("Failed to identify window {}", event.window))?
    {
        process_detected_window(ctx, event.window, identity)?;
    }
    Ok(())
}

/// Handle MapNotify events - catch windows becoming visible
pub fn handle_map_notify(ctx: &mut EventContext, event: MapNotifyEvent) -> Result<()> {
    use crate::daemon::window_detection::identify_window;

    debug!(window = event.window, "MapNotify received");

    if let Some(identity) = identify_window(
        ctx.app_ctx,
        event.window,
        ctx.session_state,
        &ctx.daemon_config.profile.custom_windows,
    )
    .context(format!("Failed to identify window {}", event.window))?
    {
        process_detected_window(ctx, event.window, identity)?;
    }
    Ok(())
}

/// Handle DestroyNotify events - remove destroyed window
pub fn handle_destroy_notify(ctx: &mut EventContext, event: DestroyNotifyEvent) -> Result<()> {
    let window_to_remove = if ctx.eve_clients.contains_key(&event.window) {
        Some(event.window)
    } else {
        ctx.eve_clients
            .iter()
            .find(|(_, thumb)| thumb.parent() == Some(event.window))
            .map(|(win, _)| *win)
    };

    if let Some(win) = window_to_remove {
        info!(
            destroyed_window = event.window,
            client_window = win,
            "DestroyNotify matched tracked source (direct or parent)"
        );
        remove_from_group_drag(ctx, win);
        ctx.cycle_state.remove_window(win);
        ctx.session_state.remove_window(win);
        ctx.eve_clients.remove(&win);
    } else {
        debug!(
            window = event.window,
            "Ignored DestroyNotify for unknown/untracked window"
        );
    }
    Ok(())
}

/// Handle PropertyNotify for identity changes (WM_NAME or WM_CLASS) to detect late-identifying windows
pub fn handle_identity_update(ctx: &mut EventContext, window: Window) -> Result<()> {
    use crate::daemon::window_detection::identify_window;
    use crate::x11::is_window_eve;

    // Check if the window is already tracked
    if ctx.eve_clients.contains_key(&window) {
        // Window is tracked. Check if it's an EVE window to handle character swaps/renames.
        if let Some(eve_window) = is_window_eve(ctx.app_ctx.conn, window, ctx.app_ctx.atoms)
            .context(format!(
                "Failed to check if window {} is EVE client during property change",
                window
            ))?
        {
            // It IS an EVE window.
            // Re-borrow thumbnail mutably
            let thumbnail = ctx
                .eve_clients
                .get_mut(&window)
                .expect("Checked contains_key");
            let old_name = thumbnail.character_name.clone();
            let new_character_name = eve_window.character_name();

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

            // Optimization: If name hasn't changed, we can exit early.
            if old_name == new_character_name {
                return Ok(());
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
                        .resolve_initial_thumbnail_position(
                            None,
                            None,
                            session_position,
                            source_position,
                        )
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
                        ctx.display_config,
                        ctx.font_renderer,
                    )
                    .context(format!(
                        "Failed to update thumbnail after character change from '{}'",
                        old_name
                    ))?;

                if !thumbnail.state.is_minimized() {
                    thumbnail
                        .border(
                            ctx.display_config,
                            thumbnail.state.is_focused(),
                            ctx.cycle_state
                                .is_skipped(thumbnail.effective_source_identity().as_ref()),
                            ctx.font_renderer,
                        )
                        .context("Failed to restore border after character change")?;
                }
            } else {
                thumbnail
                    .set_character_name(String::new(), None, ctx.display_config, ctx.font_renderer)
                    .context(format!(
                        "Failed to clear thumbnail name after logout from '{}'",
                        old_name
                    ))?;
            }
        } else {
            // Tracked, but not valid EVE window (likely Custom Source)
            // Implicitly ignore property updates for custom sources to prevent re-detection loops
        }
    } else {
        // Window is NOT tracked. Verify and identify.
        if let Some(identity) = identify_window(
            ctx.app_ctx,
            window,
            ctx.session_state,
            &ctx.daemon_config.profile.custom_windows,
        )
        .context(format!(
            "Failed to identify window {} during property change",
            window
        ))? {
            process_detected_window(ctx, window, identity)?;
        }
    }
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
