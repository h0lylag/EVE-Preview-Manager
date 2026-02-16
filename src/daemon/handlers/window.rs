use anyhow::{Context, Result};
use tracing::{debug, info};
use x11rb::connection::Connection;
use x11rb::protocol::damage::ConnectionExt as DamageExt;
use x11rb::protocol::xproto::*;

use super::super::dispatcher::EventContext;
use crate::common::types::Position;

/// Handle DamageNotify events - update damaged thumbnail
pub fn handle_damage_notify(
    ctx: &mut EventContext,
    event: x11rb::protocol::damage::NotifyEvent,
) -> Result<()> {
    // We cannot return early here based on global enabled check, because
    // some thumbnails might have per-character "Always Show" overrides.
    // Instead, we check the override status for the specific thumbnail below.

    if let Some(thumbnail) = ctx
        .eve_clients
        .values_mut()
        .find(|thumbnail| thumbnail.damage() == event.damage)
    {
        thumbnail
            .update(ctx.display_config, ctx.font_renderer)
            .context(format!(
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

/// Helper to process a window once it has been identified (used by Create, Map, and Property handlers)
pub fn process_detected_window(
    ctx: &mut EventContext,
    window: Window,
    identity: crate::daemon::window_detection::WindowIdentity,
) -> Result<()> {
    use crate::common::ipc::DaemonMessage;
    use crate::daemon::window_detection::check_and_create_window;

    debug!(
        window = window,
        character = %identity.name,
        is_custom = !identity.is_eve,
        "Identified window for preview"
    );
    debug!(?identity, "Identity details");

    ctx.cycle_state.add_window(identity.name.clone(), window);

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
                    if !thumbnail.character_name.is_empty() {
                        let settings = crate::common::types::CharacterSettings::new(
                            geom.x,
                            geom.y,
                            thumbnail.dimensions.width,
                            thumbnail.dimensions.height,
                        );

                        if identity.is_eve {
                            // Check if we already have settings for this character.
                            // If so, update the geometry but PRESERVE the user's overrides (like preview_mode).
                            // This fixes the issue where unminimizing a client (MapNotify) would reset it to Live mode.
                            if let Some(existing) = ctx
                                .daemon_config
                                .character_thumbnails
                                .get_mut(&thumbnail.character_name)
                            {
                                existing.x = settings.x;
                                existing.y = settings.y;
                                existing.dimensions = settings.dimensions;
                            } else {
                                ctx.daemon_config
                                    .character_thumbnails
                                    .insert(thumbnail.character_name.clone(), settings.clone());
                            }
                        } else {
                            ctx.daemon_config
                                .custom_source_thumbnails
                                .insert(thumbnail.character_name.clone(), settings.clone());
                        }

                        let _ = ctx.status_tx.send(DaemonMessage::PositionChanged {
                            name: thumbnail.character_name.clone(),
                            x: settings.x,
                            y: settings.y,
                            width: settings.dimensions.width,
                            height: settings.dimensions.height,
                            is_custom: !identity.is_eve,
                        });

                        // Only send CharacterDetected if this is a new window (avoid spam from Create+Map)
                        if !ctx.eve_clients.contains_key(&window) {
                            let _ = ctx.status_tx.send(DaemonMessage::CharacterDetected {
                                name: thumbnail.character_name.clone(),
                                is_custom: !identity.is_eve,
                            });
                        }

                        // Force initial update for custom sources as they might not emit Damage events immediately
                        if !identity.is_eve {
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

            // Check if this newly detected/mapped window is actually the focused window
            // This handles cases like unminimizing where MapNotify might race with FocusIn,
            // or where we overwrote the focused state by re-inserting the thumbnail.
            let is_actually_focused = crate::x11::get_active_window(
                ctx.app_ctx.conn,
                ctx.app_ctx.screen,
                ctx.app_ctx.atoms,
            )
            .unwrap_or(None)
            .map(|active| active == window)
            .unwrap_or(false);

            if is_actually_focused {
                // Update this window to focused
                if let Some(thumb) = ctx.eve_clients.get_mut(&window) {
                    thumb.state = crate::common::types::ThumbnailState::Normal { focused: true };
                    if let Err(e) = thumb.border(
                        ctx.display_config,
                        true,
                        ctx.cycle_state.is_skipped(&thumb.character_name),
                        ctx.font_renderer,
                    ) {
                        tracing::warn!(window = window, error = %e, "Failed to draw active border for restored window");
                    }
                }

                // Clear borders from ALL other windows (including minimized ones)
                // This prevents stale active borders from appearing on wrong clients
                for (w, thumb) in ctx.eve_clients.iter_mut() {
                    if *w != window {
                        // Only change state for non-minimized windows
                        // Minimized windows should stay Minimized - calling border() on them causes
                        // double-rendering. Instead, re-call minimized() to properly clear and re-render.
                        if thumb.state.is_minimized() {
                            if let Err(e) = thumb.minimized(ctx.display_config, ctx.font_renderer) {
                                tracing::warn!(window = *w, error = %e, "Failed to re-render minimized window");
                            }
                        } else {
                            thumb.state =
                                crate::common::types::ThumbnailState::Normal { focused: false };
                            if let Err(e) = thumb.border(
                                ctx.display_config,
                                false,
                                ctx.cycle_state.is_skipped(&thumb.character_name),
                                ctx.font_renderer,
                            ) {
                                tracing::warn!(window = *w, error = %e, "Failed to clear border for previous window");
                            }
                        }
                    }
                }
            } else {
                // Not focused, just draw inactive border
                if let Some(thumb) = ctx.eve_clients.get_mut(&window)
                    && let Err(e) = thumb.border(
                        ctx.display_config,
                        false,
                        ctx.cycle_state.is_skipped(&thumb.character_name),
                        ctx.font_renderer,
                    )
                {
                    tracing::warn!(window = window, error = %e, "Failed to draw initial border for new window");
                }
            }
        }
        Ok(None) => {}
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

/// Handle CreateNotify events - create thumbnail for new EVE window
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
            "DestroyNotify matched EVE client (direct or parent)"
        );
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
    use crate::common::ipc::DaemonMessage;
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
                    let settings = if ctx
                        .daemon_config
                        .profile
                        .thumbnail_preserve_position_on_swap
                    {
                        crate::common::types::CharacterSettings::new(
                            current_pos.x,
                            current_pos.y,
                            thumbnail.dimensions.width,
                            thumbnail.dimensions.height,
                        )
                    } else {
                        let src_geom = ctx
                            .app_ctx
                            .conn
                            .get_geometry(thumbnail.src())
                            .context("Failed to query source geometry for reset position")?
                            .reply()
                            .context("Failed to get source geometry reply for reset position")?;

                        let default_x = src_geom.x
                            + crate::common::constants::positioning::DEFAULT_SPAWN_OFFSET;
                        let default_y = src_geom.y
                            + crate::common::constants::positioning::DEFAULT_SPAWN_OFFSET;

                        crate::common::types::CharacterSettings::new(
                            default_x,
                            default_y,
                            thumbnail.dimensions.width,
                            thumbnail.dimensions.height,
                        )
                    };

                    ctx.daemon_config
                        .character_thumbnails
                        .insert(new_character_name.to_string(), settings.clone());

                    let _ = ctx.status_tx.send(DaemonMessage::CharacterDetected {
                        name: new_character_name.to_string(),
                        is_custom: false,
                    });

                    let _ = ctx.status_tx.send(DaemonMessage::PositionChanged {
                        name: new_character_name.to_string(),
                        x: settings.x,
                        y: settings.y,
                        width: settings.dimensions.width,
                        height: settings.dimensions.height,
                        is_custom: false, // EVE chars are never custom sources
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
                            ctx.cycle_state.is_skipped(&thumbnail.character_name),
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
