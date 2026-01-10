//! X11 window operations

use anyhow::{Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use super::CachedAtoms;
use crate::common::constants::x11;

/// Requests the window manager to grant focus to the specified window using standard EWMH protocols
///
/// # Arguments
/// * `timestamp` - The timestamp of the input event that triggered this request (required by EWMH)
pub fn activate_window(
    conn: &RustConnection,
    screen: &Screen,
    atoms: &CachedAtoms,
    window: Window,
    timestamp: u32,
) -> Result<()> {
    conn.configure_window(
        window,
        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
    )
    .context(format!("Failed to raise window {} to top of stack", window))?;

    let event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: atoms.net_active_window,
        data: ClientMessageData::from([x11::ACTIVE_WINDOW_SOURCE_PAGER, timestamp, 0, 0, 0]),
    };

    conn.send_event(
        false,
        screen.root,
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
        event,
    )
    .context(format!(
        "Failed to send _NET_ACTIVE_WINDOW event for window {}",
        window
    ))?;

    conn.flush()
        .context("Failed to flush X11 connection after window activation")?;
    Ok(())
}

/// Requests the window manager to hide/minimize the window using EWMH status flags
pub fn minimize_window(
    conn: &RustConnection,
    screen: &Screen,
    atoms: &CachedAtoms,
    window: Window,
) -> Result<()> {
    let event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: atoms.net_wm_state,
        data: ClientMessageData::from([
            x11::NET_WM_STATE_ADD,
            atoms.net_wm_state_hidden,
            0,
            x11::ACTIVE_WINDOW_SOURCE_PAGER,
            0,
        ]),
    };

    conn.send_event(
        false,
        screen.root,
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
        event,
    )
    .context(format!(
        "Failed to send _NET_WM_STATE minimize event for window {}",
        window
    ))?;

    // Fallback for WMs that expect ICCCM-style iconify requests
    let change_state_event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: atoms.wm_change_state,
        data: ClientMessageData::from([x11::ICONIC_STATE, 0, 0, 0, 0]),
    };

    conn.send_event(
        false,
        screen.root,
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
        change_state_event,
    )
    .context(format!(
        "Failed to send WM_CHANGE_STATE iconify event for window {}",
        window
    ))?;

    conn.flush()
        .context("Failed to flush X11 connection after window minimize")?;
    Ok(())
}
