use super::super::dispatcher::EventContext;
use super::super::preview_visibility::VisibilityContext;
use anyhow::{Context, Result};
use tracing::debug;
use x11rb::protocol::xproto::*;

/// Every focus event is a hint to query current ownership, including grab and pointer events.
#[tracing::instrument(skip(ctx), fields(window = event.event))]
pub fn handle_focus_in(ctx: &mut EventContext, event: FocusInEvent) -> Result<()> {
    debug!(window = event.event, mode = ?event.mode, detail = ?event.detail, "FocusIn received");
    super::super::activation::reconcile(ctx, std::time::Instant::now());
    Ok(())
}

#[tracing::instrument(skip(ctx), fields(window = event.event))]
pub fn handle_focus_out(ctx: &mut EventContext, event: FocusOutEvent) -> Result<()> {
    debug!(window = event.event, mode = ?event.mode, detail = ?event.detail, "FocusOut received");
    super::super::activation::reconcile(ctx, std::time::Instant::now());
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
pub(in crate::daemon) fn restore_focus_visibility(ctx: &mut EventContext) {
    if ctx.session_state.focus_loss_deadline.take().is_some() {
        debug!("Cancelled pending focus loss hide");
    }
    ctx.session_state.focus_hidden = false;
    reconcile_previews(ctx);
}

/// Apply every active hiding reason, including when a previous reason has just cleared.
/// Hides are applied before reveals so previews that swap places never overlap.
pub(in crate::daemon) fn reconcile_previews(ctx: &mut EventContext) {
    let eve_client_count = ctx.cycle_state.eve_client_count();
    let session = &*ctx.session_state;
    let visibility = VisibilityContext::new(
        ctx.display_config,
        ctx.daemon_config,
        session,
        eve_client_count,
    );
    let decisions: Vec<(Window, bool)> = ctx
        .eve_clients
        .iter()
        .map(|(&source, thumbnail)| {
            let blocked =
                session
                    .preview_visibility
                    .blocked(source, thumbnail.source_kind(), &visibility);
            (source, blocked)
        })
        .collect();
    // Hiding unmaps the grab window, so end any click or drag it owns first. This is
    // idempotent, so a preview already hidden by a failed unmap is still cleaned up.
    for &(source, blocked) in &decisions {
        if blocked {
            super::input::cancel_preview_input(ctx, source);
        }
    }
    for reveal in [false, true] {
        for &(source, blocked) in &decisions {
            if blocked == reveal {
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
    ctx.session_state.focus_hidden = true;
    ctx.session_state.focus_loss_deadline = None;
    reconcile_previews(ctx);
}
