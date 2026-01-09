use anyhow::{Context, Result};
use tracing::{debug, info};
use x11rb::connection::Connection;
use x11rb::protocol::damage::ConnectionExt as DamageExt;
use x11rb::protocol::xproto::*;

use super::super::dispatcher::EventContext;
use crate::types::Position;

/// Handle DamageNotify events - update damaged thumbnail
pub fn handle_damage_notify(
    ctx: &mut EventContext,
    event: x11rb::protocol::damage::NotifyEvent,
) -> Result<()> {
    if !ctx.display_config.enabled {
        return Ok(());
    }

    if let Some(thumbnail) = ctx
        .eve_clients
        .values()
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

/// Handle CreateNotify events - create thumbnail for new EVE window
pub fn handle_create_notify(ctx: &mut EventContext, event: CreateNotifyEvent) -> Result<()> {
    use crate::ipc::DaemonMessage;
    use crate::preview::window_detection::{check_and_create_window, identify_window};

    debug!(window = event.window, "CreateNotify received");

    if let Some(identity) = identify_window(
        ctx.app_ctx,
        event.window,
        ctx.session_state,
        &ctx.daemon_config.profile.custom_windows,
    )
    .context(format!("Failed to identify window {}", event.window))?
    {
        info!(window = event.window, character = %identity.name, is_custom = !identity.is_eve, "Detected relevant window");

        ctx.cycle_state
            .add_window(identity.name.clone(), event.window);

        if ctx.display_config.enabled {
            match check_and_create_window(
                ctx.app_ctx,
                ctx.daemon_config,
                ctx.display_config,
                event.window,
                ctx.font_renderer,
                ctx.session_state,
                ctx.eve_clients,
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
                            if !thumbnail.character_name.is_empty() {
                                let settings = crate::types::CharacterSettings::new(
                                    geom.x,
                                    geom.y,
                                    thumbnail.dimensions.width,
                                    thumbnail.dimensions.height,
                                );

                                if identity.is_eve {
                                    ctx.daemon_config
                                        .character_thumbnails
                                        .insert(thumbnail.character_name.clone(), settings.clone());
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
                                });
                                let _ = ctx.status_tx.send(DaemonMessage::CharacterDetected(
                                    thumbnail.character_name.clone(),
                                ));
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

                    ctx.eve_clients.insert(event.window, thumbnail);

                    if let Some(thumb) = ctx.eve_clients.get_mut(&event.window)
                        && let Err(e) = thumb.border(
                            ctx.display_config,
                            false,
                            ctx.cycle_state.is_skipped(&thumb.character_name),
                            ctx.font_renderer,
                        )
                    {
                        tracing::warn!(window = event.window, error = %e, "Failed to draw initial border for new window");
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        "Failed to create thumbnail for window {}: {}",
                        event.window,
                        e
                    );
                }
            }
        }
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

/// Handle PropertyNotify specifically for WM_NAME (Character detection)
pub fn handle_wm_name_change(ctx: &mut EventContext, window: Window) -> Result<()> {
    use crate::ipc::DaemonMessage;
    use crate::preview::window_detection::{check_and_create_window, identify_window};
    use crate::x11::is_window_eve;

    if let Some(thumbnail) = ctx.eve_clients.get_mut(&window)
        && let Some(eve_window) = is_window_eve(ctx.app_ctx.conn, window, ctx.app_ctx.atoms)
            .context(format!(
                "Failed to check if window {} is EVE client during property change",
                window
            ))?
    {
        let old_name = thumbnail.character_name.clone();
        let new_character_name = eve_window.character_name();

        ctx.session_state
            .update_last_character(window, new_character_name);

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
                    crate::types::CharacterSettings::new(
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

                ctx.daemon_config
                    .character_thumbnails
                    .insert(new_character_name.to_string(), settings.clone());

                let _ = ctx.status_tx.send(DaemonMessage::CharacterDetected(
                    new_character_name.to_string(),
                ));

                let _ = ctx.status_tx.send(DaemonMessage::PositionChanged {
                    name: new_character_name.to_string(),
                    x: settings.x,
                    y: settings.y,
                    width: settings.dimensions.width,
                    height: settings.dimensions.height,
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
        // Potential new EVE window detection
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
            ctx.cycle_state.add_window(identity.name.clone(), window);

            if ctx.display_config.enabled
                && let Some(thumbnail) = check_and_create_window(
                    ctx.app_ctx,
                    ctx.daemon_config,
                    ctx.display_config,
                    window,
                    ctx.font_renderer,
                    ctx.session_state,
                    ctx.eve_clients,
                )
                .context(format!("Failed to create thumbnail for window {}", window))?
            {
                let geom = ctx
                    .app_ctx
                    .conn
                    .get_geometry(thumbnail.window())
                    .context("Failed to query geometry for newly detected thumbnail")?
                    .reply()
                    .context("Failed to get geometry reply for newly detected thumbnail")?;

                if !thumbnail.character_name.is_empty() {
                    let settings = crate::types::CharacterSettings::new(
                        geom.x,
                        geom.y,
                        thumbnail.dimensions.width,
                        thumbnail.dimensions.height,
                    );

                    ctx.daemon_config
                        .character_thumbnails
                        .insert(thumbnail.character_name.clone(), settings.clone());

                    let _ = ctx.status_tx.send(DaemonMessage::PositionChanged {
                        name: thumbnail.character_name.clone(),
                        x: settings.x,
                        y: settings.y,
                        width: settings.dimensions.width,
                        height: settings.dimensions.height,
                    });
                    let _ = ctx.status_tx.send(DaemonMessage::CharacterDetected(
                        thumbnail.character_name.clone(),
                    ));
                }

                ctx.eve_clients.insert(window, thumbnail);

                if let Some(thumb) = ctx.eve_clients.get_mut(&window)
                    && let Err(e) = thumb.border(
                        ctx.display_config,
                        false,
                        ctx.cycle_state.is_skipped(&thumb.character_name),
                        ctx.font_renderer,
                    )
                {
                    tracing::warn!(window = window, error = %e, "Failed to draw initial border for newly detected window");
                }
            }
        }
    }
    Ok(())
}
