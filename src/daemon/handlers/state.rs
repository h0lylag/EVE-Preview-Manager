use super::super::border_update::sync_focused_borders;
use super::super::dispatcher::EventContext;
use crate::common::types::SourceIdentity;
use anyhow::{Context, Result};
use tracing::debug;
use x11rb::protocol::xproto::*;

/// Handle FocusIn events - update focused state and visibility
#[tracing::instrument(skip(ctx), fields(window = event.event))]
pub fn handle_focus_in(ctx: &mut EventContext, event: FocusInEvent) -> Result<()> {
    if ctx.display_config.hide_active && matches!(event.mode, NotifyMode::GRAB | NotifyMode::UNGRAB)
    {
        return Ok(());
    }
    if event.mode == NotifyMode::UNGRAB {
        debug!(window = event.event, "Ignoring FocusIn with mode Ungrab");
        return Ok(());
    }

    debug!(window = event.event, "FocusIn received");
    if ctx.display_config.hide_active {
        refresh_preview_focus(ctx);
        reconcile_previews(ctx);
        return Ok(());
    }

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

    let remembered_character = ctx
        .session_state
        .window_last_character
        .get(&event.event)
        .map(|name| SourceIdentity::eve(name.clone()));

    if ctx
        .cycle_state
        .set_current_by_window_with_identity(event.event, remembered_character.as_ref())
    {
        debug!(window = event.event, "Synced cycle state to focused window");
    }

    restore_focus_visibility(ctx);

    sync_focused_borders(
        ctx.eve_clients,
        ctx.cycle_state,
        ctx.display_config,
        ctx.font_renderer,
        Some(event.event),
        "focus in",
    );

    Ok(())
}

/// Handle FocusOut events - update focused state and visibility  
#[tracing::instrument(skip(ctx), fields(window = event.event))]
pub fn handle_focus_out(ctx: &mut EventContext, event: FocusOutEvent) -> Result<()> {
    if ctx.display_config.hide_active && matches!(event.mode, NotifyMode::GRAB | NotifyMode::UNGRAB)
    {
        return Ok(());
    }
    if event.mode == NotifyMode::GRAB {
        debug!(window = event.event, "Ignoring FocusOut with mode Grab");
        return Ok(());
    }

    debug!(window = event.event, "FocusOut received");
    if ctx.display_config.hide_active {
        refresh_preview_focus(ctx);
        reconcile_previews(ctx);
        return Ok(());
    }

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

/// Accept confirmed source focus, whether observed through FocusIn or late detection.
/// The preview-toggle block remains authoritative.
pub(super) fn restore_focus_visibility(ctx: &mut EventContext) {
    if ctx.display_config.hide_active {
        refresh_preview_focus(ctx);
        reconcile_previews(ctx);
        return;
    }
    if ctx.session_state.focus_loss_deadline.take().is_some() {
        debug!("Cancelled pending focus loss hide");
    }
    ctx.session_state.focus_hidden = false;
    reconcile_previews(ctx);
}

/// Reconcile focus selection and borders from the WM property, never event/request ordering.
pub(in crate::daemon) fn refresh_preview_focus(ctx: &mut EventContext) {
    if !super::super::focus::refresh_active_source(
        ctx.app_ctx,
        ctx.eve_clients,
        ctx.cycle_state,
        ctx.session_state,
        ctx.display_config,
    ) {
        return;
    }
    let active = ctx.session_state.active_source_window;
    if let Some(window) = active {
        let remembered = ctx
            .session_state
            .window_last_character
            .get(&window)
            .map(|name| SourceIdentity::eve(name.clone()));
        ctx.cycle_state
            .set_current_by_window_with_identity(window, remembered.as_ref());
    }
    sync_focused_borders(
        ctx.eve_clients,
        ctx.cycle_state,
        ctx.display_config,
        ctx.font_renderer,
        active,
        "confirmed active source",
    );
}

/// Apply every active hiding reason, hiding first when overlapping previews swap.
pub(in crate::daemon) fn reconcile_previews(ctx: &mut EventContext) {
    let count = ctx.cycle_state.eve_client_count();
    let decisions: Vec<_> = ctx
        .eve_clients
        .iter()
        .map(|(&source, thumbnail)| {
            let blocked = ctx.display_config.preview_visibility_blocked(
                thumbnail.source_kind(),
                ctx.daemon_config.runtime_hidden,
                ctx.session_state.focus_hidden,
                count,
                ctx.session_state.active_source_window == Some(source),
            );
            (source, blocked)
        })
        .collect();
    for &(source, blocked) in &decisions {
        if blocked {
            super::input::cancel_hidden_preview_input(ctx, source);
        }
    }
    for phase in [true, false] {
        for &(source, blocked) in &decisions {
            if blocked != phase {
                continue;
            }
            if let Some(thumbnail) = ctx.eve_clients.get_mut(&source)
                && let Err(error) =
                    thumbnail.set_visibility_blocked(blocked, ctx.display_config, ctx.font_renderer)
            {
                tracing::warn!(source = %thumbnail.character_name, error = %error, "Failed to reconcile preview visibility");
            }
        }
    }
}

pub fn toggle_previews(ctx: &mut EventContext) {
    ctx.daemon_config.runtime_hidden = !ctx.daemon_config.runtime_hidden;
    tracing::info!(
        hidden = ctx.daemon_config.runtime_hidden,
        "Toggled previews visibility"
    );
    reconcile_previews(ctx);
}

pub fn hide_after_focus_loss(ctx: &mut EventContext) {
    refresh_preview_focus(ctx);
    if ctx.display_config.hide_active && ctx.session_state.active_source_window.is_some() {
        ctx.session_state.focus_loss_deadline = None;
        reconcile_previews(ctx);
        return;
    }
    // Preserve cancellation even if a drag has no currently visible members.
    if ctx.group_drag_state.is_active()
        && let Err(error) = super::input::cancel_group_drag(
            ctx.app_ctx.conn,
            ctx.eve_clients,
            ctx.group_drag_state,
            None,
        )
    {
        tracing::warn!(error = %error, "Failed to restore group after focus loss");
    }
    ctx.session_state.focus_hidden = true;
    ctx.session_state.focus_loss_deadline = None;
    reconcile_previews(ctx);
}
