//! Source matching and confirmed window-manager focus for preview visibility.
use super::{cycle_state::CycleState, session_state::SessionState, thumbnail::Thumbnail};
use crate::common::types::SourceIdentity;
use crate::config::DisplayConfig;
use crate::x11::AppContext;
use anyhow::Result;
use std::collections::HashMap;
use tracing::debug;
use x11rb::errors::ReplyError;
use x11rb::protocol::{ErrorKind, xproto::*};

fn direct_tracked_source_window(
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    active_windows: Option<&HashMap<Window, Option<SourceIdentity>>>,
    window: Window,
) -> Option<Window> {
    if active_windows.is_some_and(|windows| windows.contains_key(&window)) {
        return Some(window);
    }

    if thumbnails.contains_key(&window) {
        return Some(window);
    }

    thumbnails.iter().find_map(|(&source_window, thumbnail)| {
        (thumbnail.window() == window
            || thumbnail.src() == window
            || thumbnail.parent() == Some(window))
        .then_some(source_window)
    })
}

pub(super) fn tracked_source_window_for_window(
    ctx: &AppContext<'_>,
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    active_windows: Option<&HashMap<Window, Option<SourceIdentity>>>,
    window: Window,
) -> Option<Window> {
    if let Some(source_window) = direct_tracked_source_window(thumbnails, active_windows, window) {
        return Some(source_window);
    }

    let mut current = window;
    for _ in 0..10 {
        let parent = ctx
            .conn
            .query_tree(current)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.parent)?;

        if let Some(source_window) =
            direct_tracked_source_window(thumbnails, active_windows, parent)
        {
            debug!(
                child = window,
                parent = parent,
                source_window = source_window,
                "Matched focused window to tracked source ancestor"
            );
            return Some(source_window);
        }

        if parent == ctx.screen.root || parent == 0 {
            break;
        }
        current = parent;
    }

    None
}

pub(super) fn active_tracked_source_window(
    ctx: &AppContext<'_>,
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    active_windows: Option<&HashMap<Window, Option<SourceIdentity>>>,
) -> Option<Window> {
    let active_window = crate::x11::get_active_window(ctx.conn, ctx.screen, ctx.atoms)
        .ok()
        .flatten()?;

    tracked_source_window_for_window(ctx, thumbnails, active_windows, active_window)
}

/// Unlike activation helpers, preview overlays are not active application sources.
fn observed_active_source(
    ctx: &AppContext<'_>,
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    cycle: &CycleState,
) -> Result<Option<Window>> {
    let Some(active) = crate::x11::get_active_window(ctx.conn, ctx.screen, ctx.atoms)? else {
        return Ok(None);
    };
    let root = ctx.screen.root;
    let mut current = active;
    for _ in 0..10 {
        if current == 0
            || current == root
            || thumbnails
                .values()
                .any(|thumbnail| thumbnail.window() == current)
        {
            return Ok(None);
        }
        if cycle.get_active_windows().contains_key(&current) {
            return Ok(Some(current));
        }
        if let Some((&source, _)) = thumbnails.iter().find(|(source, thumbnail)| {
            cycle.get_active_windows().contains_key(source) && thumbnail.parent() == Some(current)
        }) {
            return Ok(Some(source));
        }
        // Startup and render-disabled sources have no thumbnail caching their frame.
        for &source in cycle
            .get_active_windows()
            .keys()
            .filter(|source| !thumbnails.contains_key(source))
        {
            match ctx.conn.query_tree(source)?.reply() {
                Ok(tree) if tree.parent == current => return Ok(Some(source)),
                Ok(_) => {}
                Err(ReplyError::X11Error(error)) if error.error_kind == ErrorKind::Window => {}
                Err(error) => return Err(error.into()),
            }
        }
        current = ctx.conn.query_tree(current)?.reply()?.parent;
    }
    Ok(None)
}

/// Observe focus without using optimistic border or cycle selection.
/// Returns whether observation succeeded; errors preserve the previous state and deadline.
pub(super) fn refresh_active_source(
    ctx: &AppContext<'_>,
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    cycle: &CycleState,
    session: &mut SessionState,
    display: &DisplayConfig,
) -> bool {
    if !display.hide_active {
        return false;
    }
    match observed_active_source(ctx, thumbnails, cycle) {
        Ok(active) => {
            session.active_source_window = active;
            if active.is_some() {
                session.focus_loss_deadline = None;
                session.focus_hidden = false;
            } else if display.hide_when_no_focus
                && !session.focus_hidden
                && session.focus_loss_deadline.is_none()
            {
                session.focus_loss_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(100));
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, "Failed to observe active source");
            return false;
        }
    }
    true
}
