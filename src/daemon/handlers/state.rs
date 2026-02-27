use super::super::dispatcher::EventContext;
use crate::common::types::ThumbnailState;
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

    // Get the window we expect to be focused on (set by hotkey/click handlers)
    let expected_window = ctx.cycle_state.get_current_window();

    // If we have an expected window and this FocusIn is for a different window,
    // it's likely an intermediate focus event during a transition (e.g., window manager
    // focusing intermediate windows during tabbing). Skip processing entirely to avoid
    // corrupting the cycle state.
    //
    // NOTE: Only filter UNTRACKED windows (WM internals, transient overlays, etc.).
    // If this FocusIn is for a window we actually track, always allow it through.
    // This prevents a stuck-filter scenario where a custom source redirects focus to
    // an internal subwindow after activation — the tracked window's FocusIn never
    // arrives, leaving current_window permanently set and blocking all future events.
    if let Some(expected) = expected_window
        && event.event != expected
        && !ctx.eve_clients.contains_key(&event.event)
    {
        debug!(
            focusin_window = event.event,
            expected_window = expected,
            "Ignoring FocusIn for untracked intermediate window during transition"
        );
        // Don't update cycle state or draw borders - wait for the correct window's FocusIn
        return Ok(());
    }

    if ctx.cycle_state.set_current_by_window(event.event) {
        debug!(window = event.event, "Synced cycle state to focused window");
    }

    // Cancel any pending hide operation since we regained focus
    if ctx.session_state.focus_loss_deadline.is_some() {
        ctx.session_state.focus_loss_deadline = None;
        debug!("Cancelled pending focus loss hide");
    }

    if ctx.display_config.hide_when_no_focus && ctx.eve_clients.values().any(|x| !x.is_visible()) {
        for thumbnail in ctx.eve_clients.values_mut() {
            // Respect per-character override: don't reveal force-hidden thumbnails
            let should_render = ctx
                .display_config
                .character_settings
                .get(&thumbnail.character_name)
                .and_then(|s| s.override_render_preview)
                .unwrap_or(ctx.display_config.enabled);

            if !should_render {
                continue;
            }

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
        } else {
            // Update ALL other clients to unfocused state
            // This ensures borders stay in sync even when minimize-on-switch is active
            // Only change state for non-minimized windows - minimized windows stay Minimized
            // For minimized windows, calling border() causes double-rendering, so re-call minimized() instead
            if thumbnail.state.is_minimized() {
                thumbnail
                    .minimized(ctx.display_config, ctx.font_renderer)
                    .context(format!(
                        "Failed to re-render minimized window '{}' (focus moved to '{}')",
                        thumbnail.character_name, event.event
                    ))?;
            } else {
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
            // Schedule the hide operation with a short delay (hysteresis) to allow for
            // quick focus cycling without flickering.
            ctx.session_state.focus_loss_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(100));
            debug!(
                window = event.event,
                "Scheduled delayed thumbnail hide due to focus loss"
            );
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
