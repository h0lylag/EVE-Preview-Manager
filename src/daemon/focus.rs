//! Resolve current keyboard focus without confusing grabs, frames, and previews.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt, EventMask, Window};
use x11rb::rust_connection::RustConnection;

use super::thumbnail::Thumbnail;
use crate::common::types::SourceIdentity;
use crate::x11::AppContext;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FocusOwner {
    Source(Window),
    Frame(Window),
    Preview(Window),
    Outside(Window),
    Root,
    None,
}

impl FocusOwner {
    /// Broad ownership is appropriate for interaction, never for activation confirmation.
    pub(super) fn source(self) -> Option<Window> {
        match self {
            Self::Source(source) | Self::Frame(source) | Self::Preview(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FocusObservation {
    pub raw_focus: Window,
    pub owner: FocusOwner,
    pub pointer_root: bool,
}

impl FocusObservation {
    pub(super) fn explicit_source(self) -> Option<Window> {
        match self.owner {
            FocusOwner::Source(source) if !self.pointer_root => Some(source),
            _ => None,
        }
    }

    /// Root/None transitions reach the root window as focus events (see
    /// `select_root_focus_changes`); pointer movement and indirect ownership do not.
    pub(super) fn needs_poll(self) -> bool {
        self.pointer_root || matches!(self.owner, FocusOwner::Frame(_) | FocusOwner::Preview(_))
    }

    pub(super) fn transient(self) -> bool {
        self.pointer_root || matches!(self.owner, FocusOwner::Root | FocusOwner::None)
    }
}

pub(super) fn real_window(ctx: &AppContext<'_>, window: Window) -> bool {
    window > 1
        && !ctx
            .conn
            .setup()
            .roots
            .iter()
            .any(|screen| screen.root == window)
}

/// Bounded traversal. Errors and exhaustion are not evidence of outside focus.
fn walk_owner(
    window: Window,
    is_root: impl Fn(Window) -> bool,
    direct: impl Fn(Window) -> Option<FocusOwner>,
    mut parent: impl FnMut(Window) -> Result<Window>,
) -> Result<FocusOwner> {
    if window <= 1 {
        return Ok(FocusOwner::None);
    }
    if is_root(window) {
        return Ok(FocusOwner::Root);
    }
    let mut current = window;
    for depth in 0..=10 {
        if let Some(owner) = direct(current) {
            return Ok(owner);
        }
        if depth == 10 {
            break;
        }
        let next = parent(current)?;
        // Never let a cached root parent match an unrelated window.
        if next <= 1 || is_root(next) {
            return Ok(FocusOwner::Outside(current));
        }
        current = next;
    }
    bail!("Focus ancestry exceeds ten parents for window {window}")
}

/// Ownership of `window` itself, without consulting its ancestors.
fn direct_owner(
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    active: Option<&HashMap<Window, Option<SourceIdentity>>>,
    window: Window,
) -> Option<FocusOwner> {
    if active.is_some_and(|windows| windows.contains_key(&window))
        || thumbnails.contains_key(&window)
    {
        return Some(FocusOwner::Source(window));
    }
    // Prefer a real source or preview over another source's cached frame.
    for (&source, thumbnail) in thumbnails {
        if thumbnail.src() == window {
            return Some(FocusOwner::Source(source));
        }
        if thumbnail.window() == window {
            return Some(FocusOwner::Preview(source));
        }
    }
    thumbnails.iter().find_map(|(&source, thumbnail)| {
        (thumbnail.parent() == Some(window)).then_some(FocusOwner::Frame(source))
    })
}

pub(super) fn resolve_window(
    ctx: &AppContext<'_>,
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    active: Option<&HashMap<Window, Option<SourceIdentity>>>,
    window: Window,
) -> Result<FocusOwner> {
    walk_owner(
        window,
        |w| ctx.conn.setup().roots.iter().any(|screen| screen.root == w),
        |w| direct_owner(thumbnails, active, w),
        |w| Ok(ctx.conn.query_tree(w)?.reply()?.parent),
    )
}

/// Bounded top-down descent along the pointer's child path.
///
/// Stops at the first directly owned window, so nested WM frames resolve to their source
/// and deeply nested source children never exhaust the bound. Reaching the leaf without an
/// owner is outside focus; exhausting the bound without one is unresolved.
fn descend_pointer(
    top_level: Window,
    direct: impl Fn(Window) -> Option<FocusOwner>,
    mut child_under_pointer: impl FnMut(Window) -> Result<Window>,
) -> Result<FocusOwner> {
    let mut current = top_level;
    for _ in 0..=POINTER_DESCENT_LIMIT {
        if let Some(owner) = direct(current) {
            return Ok(owner);
        }
        let child = child_under_pointer(current)?;
        if child == 0 {
            return Ok(FocusOwner::Outside(top_level));
        }
        current = child;
    }
    bail!("Pointer descent exceeds {POINTER_DESCENT_LIMIT} windows below {top_level}")
}

const POINTER_DESCENT_LIMIT: usize = 32;

/// Resolve PointerRoot focus: keyboard input goes to the window under the pointer.
fn pointer_owner(
    ctx: &AppContext<'_>,
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    active: &HashMap<Window, Option<SourceIdentity>>,
) -> Result<FocusOwner> {
    for screen in &ctx.conn.setup().roots {
        let reply = ctx.conn.query_pointer(screen.root)?.reply()?;
        if !reply.same_screen {
            continue;
        }
        if reply.child == 0 {
            return Ok(FocusOwner::Root);
        }
        return descend_pointer(
            reply.child,
            |w| direct_owner(thumbnails, Some(active), w),
            |w| Ok(ctx.conn.query_pointer(w)?.reply()?.child),
        );
    }
    bail!("Pointer is not on a known X11 screen")
}

pub(super) fn observe(
    ctx: &AppContext<'_>,
    thumbnails: &HashMap<Window, Thumbnail<'_>>,
    active: &HashMap<Window, Option<SourceIdentity>>,
) -> Result<FocusObservation> {
    let raw_focus = ctx.conn.get_input_focus()?.reply()?.focus;
    let pointer_root = raw_focus == 1;
    let owner = if pointer_root {
        pointer_owner(ctx, thumbnails, active)?
    } else {
        resolve_window(ctx, thumbnails, Some(active), raw_focus)?
    };
    Ok(FocusObservation {
        raw_focus,
        owner,
        pointer_root,
    })
}

/// Merge `FOCUS_CHANGE` into this client's existing event mask on every screen root.
///
/// X delivers focus events to the roots exactly when focus enters or leaves root, None,
/// or PointerRoot (not for moves between top-level windows), so those states need no
/// idle polling. Merging preserves every mask bit selected elsewhere.
pub(super) fn select_root_focus_changes(conn: &RustConnection) -> Result<()> {
    for screen in &conn.setup().roots {
        let current = conn
            .get_window_attributes(screen.root)?
            .reply()
            .context("Failed to read root event mask")?
            .your_event_mask;
        conn.change_window_attributes(
            screen.root,
            &ChangeWindowAttributesAux::new().event_mask(current | EventMask::FOCUS_CHANGE),
        )?
        .check()
        .context("Failed to select focus changes on root window")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(window: Window) -> Result<FocusOwner> {
        walk_owner(
            window,
            |w| w == 99,
            |w| match w {
                10 => Some(FocusOwner::Source(10)),
                20 => Some(FocusOwner::Preview(10)),
                30 => Some(FocusOwner::Frame(10)),
                // Deliberately poisoned cache: traversal must never consult this entry.
                99 | 0 | 1 => Some(FocusOwner::Frame(10)),
                _ => None,
            },
            |w| {
                Ok(match w {
                    11 => 10,
                    21 => 20,
                    31 => 30,
                    _ => 99,
                })
            },
        )
    }

    #[test]
    fn descendants_preserve_owner_kind_and_root_never_matches() {
        for (window, expected) in [
            (10, FocusOwner::Source(10)),
            (11, FocusOwner::Source(10)),
            (20, FocusOwner::Preview(10)),
            (21, FocusOwner::Preview(10)),
            (30, FocusOwner::Frame(10)),
            (31, FocusOwner::Frame(10)),
            (50, FocusOwner::Outside(50)),
            (99, FocusOwner::Root),
            (0, FocusOwner::None),
            (1, FocusOwner::None),
        ] {
            assert_eq!(resolve(window).unwrap(), expected);
        }
    }

    #[test]
    fn only_explicit_source_confirms_activation() {
        for owner in [
            FocusOwner::Source(10),
            FocusOwner::Frame(10),
            FocusOwner::Preview(10),
        ] {
            for pointer_root in [false, true] {
                let observation = FocusObservation {
                    raw_focus: 10,
                    owner,
                    pointer_root,
                };
                assert_eq!(
                    observation.explicit_source(),
                    (owner == FocusOwner::Source(10) && !pointer_root).then_some(10)
                );
            }
        }
    }

    #[test]
    fn failed_and_excessive_ancestry_are_unknown_not_outside() {
        assert!(walk_owner(10, |_| false, |_| None, |_| bail!("destroyed")).is_err());
        assert!(walk_owner(10, |_| false, |_| None, |w| Ok(w + 1)).is_err());
    }

    #[test]
    fn pointer_descent_stops_at_first_owner_and_never_exhausts_inside_it() {
        let owned = |w| match w {
            10 => Some(FocusOwner::Source(10)),
            30 => Some(FocusOwner::Frame(10)),
            _ => None,
        };
        // Top-level source with an arbitrarily deep child chain under the pointer.
        let queried = std::cell::Cell::new(0);
        let deep = descend_pointer(10, owned, |w| {
            queried.set(queried.get() + 1);
            Ok(w + 1)
        });
        assert_eq!(deep.unwrap(), FocusOwner::Source(10));
        assert_eq!(queried.get(), 0, "an owned top-level needs no descent");
        // Outer frame (unknown) -> wrapper (known frame) -> source.
        let nested = descend_pointer(29, owned, |w| Ok(if w == 29 { 30 } else { 10 }));
        assert_eq!(nested.unwrap(), FocusOwner::Frame(10));
    }

    #[test]
    fn pointer_descent_without_owner_is_outside_at_leaf_and_unresolved_when_bounded() {
        let none = |_| None;
        assert_eq!(
            descend_pointer(50, none, |w| Ok(if w < 53 { w + 1 } else { 0 })).unwrap(),
            FocusOwner::Outside(50)
        );
        assert!(descend_pointer(50, none, |w| Ok(w + 1)).is_err());
        assert!(descend_pointer(50, none, |_| bail!("destroyed")).is_err());
    }
}
