use super::super::dispatcher::EventContext;
use crate::types::ThumbnailState;
use anyhow::{Context, Result};
use tracing::debug;
use x11rb::protocol::xproto::*;

/// Handle FocusIn events - update focused state and visibility
#[tracing::instrument(skip(ctx), fields(window = event.event))]
pub fn handle_focus_in(ctx: &mut EventContext, event: FocusInEvent) -> Result<()> {
    if event.mode == NotifyMode::UNGRAB {
        debug!(window = event.event, "Ignoring FocusIn with mode Ungrab");
        return Ok(());
    }

    debug!(window = event.event, "FocusIn received");

    if ctx.cycle_state.set_current_by_window(event.event) {
        debug!(window = event.event, "Synced cycle state to focused window");
    }

    if ctx.display_config.hide_when_no_focus && ctx.eve_clients.values().any(|x| !x.is_visible()) {
        for thumbnail in ctx.eve_clients.values_mut() {
            debug!(character = %thumbnail.character_name, "Revealing thumbnail due to focus change");
            thumbnail.visibility(true).context(format!(
                "Failed to show thumbnail '{}' on focus",
                thumbnail.character_name
            ))?;
            thumbnail
                .update(ctx.display_config, ctx.font_renderer)
                .context(format!(
                    "Failed to update thumbnail '{}' on focus reveal",
                    thumbnail.character_name
                ))?;
        }
    }

    for (window, thumbnail) in ctx.eve_clients.iter_mut() {
        if *window == event.event {
            if !thumbnail.state.is_focused() {
                thumbnail.state = ThumbnailState::Normal { focused: true };
                thumbnail
                    .border(
                        ctx.display_config,
                        true,
                        ctx.cycle_state.is_skipped(&thumbnail.character_name),
                        ctx.font_renderer,
                    )
                    .context(format!(
                        "Failed to update border on focus for '{}'",
                        thumbnail.character_name
                    ))?;
            }
        } else if thumbnail.state.is_focused() {
            thumbnail.state = ThumbnailState::Normal { focused: false };
            thumbnail
                .border(
                    ctx.display_config,
                    false,
                    ctx.cycle_state.is_skipped(&thumbnail.character_name),
                    ctx.font_renderer,
                )
                .context(format!(
                    "Failed to clear border for '{}' (focus moved to '{}')",
                    thumbnail.character_name, event.event
                ))?;
        }
    }
    Ok(())
}

/// Handle FocusOut events - update focused state and visibility  
#[tracing::instrument(skip(ctx), fields(window = event.event))]
pub fn handle_focus_out(ctx: &mut EventContext, event: FocusOutEvent) -> Result<()> {
    if event.mode == NotifyMode::GRAB {
        debug!(window = event.event, "Ignoring FocusOut with mode Grab");
        return Ok(());
    }

    debug!(window = event.event, "FocusOut received");

    if ctx.display_config.hide_when_no_focus {
        let was_active = ctx
            .eve_clients
            .get(&event.event)
            .map(|t| t.state.is_focused())
            .unwrap_or(false);

        if was_active {
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

pub fn handle_net_wm_state(ctx: &mut EventContext, window: Window, atom: Atom) -> Result<()> {
    if let Some(thumbnail) = ctx.eve_clients.get_mut(&window)
        && let Some(mut state) = ctx
            .app_ctx
            .conn
            .get_property(false, window, atom, AtomEnum::ATOM, 0, 1024)
            .context(format!(
                "Failed to query window state for window {}",
                window
            ))?
            .reply()
            .context(format!(
                "Failed to get window state reply for window {}",
                window
            ))?
            .value32()
        && state.any(|s| s == ctx.app_ctx.atoms.net_wm_state_hidden)
    {
        thumbnail
            .minimized(ctx.display_config, ctx.font_renderer)
            .context(format!(
                "Failed to set minimized state for '{}'",
                thumbnail.character_name
            ))?;
    }
    Ok(())
}
