//! X11 window state queries

use anyhow::{Context, Result};
use tracing::debug;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use super::CachedAtoms;
use crate::common::constants::{eve, x11};
use crate::common::types::EveWindowType;

/// Identifies if a window belongs to EVE Online by inspecting its properties and title
pub fn is_window_eve(
    conn: &RustConnection,
    window: Window,
    atoms: &CachedAtoms,
) -> Result<Option<EveWindowType>> {
    let cookie = conn
        .get_property(false, window, atoms.wm_name, AtomEnum::STRING, 0, 1024)
        .context(format!(
            "Failed to query WM_NAME property for window {}",
            window
        ))?;
    let name_prop = match cookie.reply() {
        Ok(reply) => reply,
        Err(ReplyError::X11Error(err)) if err.error_kind == x11rb::protocol::ErrorKind::Window => {
            debug!(
                window = window,
                "Window destroyed before WM_NAME reply, skipping"
            );
            return Ok(None);
        }
        Err(err) => {
            return Err(err).context(format!("Failed to get WM_NAME reply for window {}", window));
        }
    };
    let title = String::from_utf8_lossy(&name_prop.value).into_owned();
    Ok(
        if let Some(name) = title.strip_prefix(eve::WINDOW_TITLE_PREFIX) {
            if name.to_lowercase().contains("steam_app_") {
                debug!(window=window, name=%name, "Ignored steam_app container title");
                None
            } else {
                Some(EveWindowType::LoggedIn(name.to_string()))
            }
        } else if title == eve::LOGGED_OUT_TITLE {
            Some(EveWindowType::LoggedOut)
        } else {
            None
        },
    )
}

/// Get the WM_CLASS property of a window (returns the second string, which is the class name)
pub fn get_window_class(
    conn: &RustConnection,
    window: Window,
    atoms: &CachedAtoms,
) -> Result<Option<String>> {
    let cookie = conn
        .get_property(false, window, atoms.wm_class, AtomEnum::STRING, 0, 1024)
        .context(format!(
            "Failed to query WM_CLASS property for window {}",
            window
        ))?;

    let prop = match cookie.reply() {
        Ok(reply) => reply,
        Err(ReplyError::X11Error(err)) if err.error_kind == x11rb::protocol::ErrorKind::Window => {
            debug!(
                window = window,
                "Window destroyed before WM_CLASS reply, skipping"
            );
            return Ok(None);
        }
        Err(err) => {
            return Err(err).context(format!(
                "Failed to get WM_CLASS reply for window {}",
                window
            ));
        }
    };

    if prop.value.is_empty() {
        return Ok(None);
    }

    let null_byte = 0;
    let parts: Vec<&[u8]> = prop.value.split(|&x| x == null_byte).collect();

    let class_bytes = if parts.len() >= 2 && !parts[1].is_empty() {
        parts[1]
    } else {
        parts[0]
    };

    Ok(Some(String::from_utf8_lossy(class_bytes).into_owned()))
}

/// Check whether the given EVE client window is currently minimized/iconified
pub fn is_window_minimized(
    conn: &RustConnection,
    window: Window,
    atoms: &CachedAtoms,
) -> Result<bool> {
    let net_state_cookie = conn
        .get_property(false, window, atoms.net_wm_state, AtomEnum::ATOM, 0, 1024)
        .context(format!(
            "Failed to query _NET_WM_STATE for window {}",
            window
        ))?;
    match net_state_cookie.reply() {
        Ok(reply) => {
            if let Some(mut values) = reply.value32()
                && values.any(|state| state == atoms.net_wm_state_hidden)
            {
                return Ok(true);
            }
        }
        Err(ReplyError::X11Error(err)) if err.error_kind == x11rb::protocol::ErrorKind::Window => {
            debug!(
                window = window,
                "Window destroyed before _NET_WM_STATE reply"
            );
            return Ok(false);
        }
        Err(err) => {
            return Err(err).context(format!(
                "Failed to get _NET_WM_STATE reply for window {}",
                window
            ));
        }
    }

    let wm_state_cookie = conn
        .get_property(false, window, atoms.wm_state, atoms.wm_state, 0, 2)
        .context(format!("Failed to query WM_STATE for window {}", window))?;
    match wm_state_cookie.reply() {
        Ok(reply) => {
            if let Some(mut values) = reply.value32()
                && let Some(state) = values.next()
                && state == x11::ICONIC_STATE
            {
                return Ok(true);
            }
        }
        Err(ReplyError::X11Error(err)) if err.error_kind == x11rb::protocol::ErrorKind::Window => {
            debug!(window = window, "Window destroyed before WM_STATE reply");
            return Ok(false);
        }
        Err(err) => {
            return Err(err).context(format!(
                "Failed to get WM_STATE reply for window {}",
                window
            ));
        }
    }

    Ok(false)
}

pub fn get_active_eve_window(
    conn: &RustConnection,
    screen: &Screen,
    atoms: &CachedAtoms,
) -> Result<Option<Window>> {
    let active_window = get_active_window(conn, screen, atoms)?;

    if let Some(active_window) = active_window {
        if is_window_eve(conn, active_window, atoms)
            .context(format!(
                "Failed to check if active window {} is EVE client",
                active_window
            ))?
            .is_some()
        {
            Ok(Some(active_window))
        } else {
            Ok(None)
        }
    } else {
        Ok(None)
    }
}

/// Get the currently focused window ID, if any
pub fn get_active_window(
    conn: &RustConnection,
    screen: &Screen,
    atoms: &CachedAtoms,
) -> Result<Option<Window>> {
    let active_window_prop = conn
        .get_property(
            false,
            screen.root,
            atoms.net_active_window,
            AtomEnum::WINDOW,
            0,
            1,
        )
        .context("Failed to query _NET_ACTIVE_WINDOW property")?
        .reply()
        .context("Failed to get reply for _NET_ACTIVE_WINDOW query")?;

    if active_window_prop.value.len() >= 4 {
        Ok(Some(u32::from_ne_bytes(
            active_window_prop.value[0..4]
                .try_into()
                .context("Invalid _NET_ACTIVE_WINDOW property format")?,
        )))
    } else {
        Ok(None)
    }
}

/// Check if a window is a "normal" top-level application window
/// (Filters out docks, popups, desktop, etc. based on _NET_WM_WINDOW_TYPE)
pub fn is_normal_window(
    conn: &RustConnection,
    window: Window,
    atoms: &CachedAtoms,
) -> Result<bool> {
    let type_cookie = conn
        .get_property(
            false,
            window,
            atoms.net_wm_window_type,
            AtomEnum::ATOM,
            0,
            32,
        )
        .context(format!(
            "Failed to query _NET_WM_WINDOW_TYPE for window {}",
            window
        ))?;

    match type_cookie.reply() {
        Ok(reply) => {
            if let Some(atoms_iter) = reply.value32() {
                // Check if any of the types match _NET_WM_WINDOW_TYPE_NORMAL
                // If the property is missing/empty, EWMH says to treat as Normal.
                // However, if it HAS types, we should check them.
                // Common types: NORMAL, DIALOG, UTILITY, TOOLBAR, SPLASH, DOCK, DESKTOP
                // We want NORMAL or DIALOG (maybe).
                // For now, let's just check if it contains NORMAL, or if the list implies it.
                // Actually, simpler logic: If it contains DOCK, DESKTOP, TOOLBAR, MENU, SPLASH -> False.
                // If unmatched or contains NORMAL -> True.

                // Collect to vec to iterate multiple times if needed, or just scan
                let types: Vec<u32> = atoms_iter.collect();

                if types.is_empty() {
                    return Ok(true); // Default to Normal
                }

                let ignore_types = [
                    atoms.net_wm_window_type_dock,
                    atoms.net_wm_window_type_desktop,
                    atoms.net_wm_window_type_toolbar,
                    atoms.net_wm_window_type_menu,
                    atoms.net_wm_window_type_utility,
                    atoms.net_wm_window_type_splash,
                    atoms.net_wm_window_type_dropdown_menu,
                    atoms.net_wm_window_type_popup_menu,
                    atoms.net_wm_window_type_tooltip,
                    atoms.net_wm_window_type_notification,
                    atoms.net_wm_window_type_combo,
                    atoms.net_wm_window_type_dnd,
                ];

                for t in types {
                    // If it is explicitly marked as one of the types we ignore, return false
                    if ignore_types.contains(&t) {
                        return Ok(false);
                    }
                }
                Ok(true)
            } else {
                // Property exists but empty value? Treat as normal.
                Ok(true)
            }
        }
        Err(ReplyError::X11Error(_)) => {
            // Window gone
            Ok(false)
        }
        Err(e) => Err(anyhow::anyhow!("Failed to get window type reply: {}", e)),
    }
}
