//! X11 window operations

use anyhow::{Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::xproto::{
    ConnectionExt, KeyButMask, MOTION_NOTIFY_EVENT, Motion, MotionNotifyEvent,
};
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

    // Inject a synthetic motion event to wake up the client's input handling
    // This fixes "stuck mouse" issues on XWayland where hover states don't activate
    refresh_pointer_state(conn, window, timestamp).context("Failed to refresh pointer state")?;

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

/// Requests the window manager to restore/unminimize a window using EWMH protocols
pub fn unminimize_window(
    conn: &RustConnection,
    screen: &Screen,
    atoms: &CachedAtoms,
    window: Window,
) -> Result<()> {
    // Remove the _NET_WM_STATE_HIDDEN flag to unminimize
    let event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: atoms.net_wm_state,
        data: ClientMessageData::from([
            x11::NET_WM_STATE_REMOVE,
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
        "Failed to send _NET_WM_STATE unminimize event for window {}",
        window
    ))?;

    // Fallback for WMs that expect ICCCM-style deiconify (normal state) requests
    let change_state_event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: atoms.wm_change_state,
        data: ClientMessageData::from([x11::NORMAL_STATE, 0, 0, 0, 0]),
    };

    conn.send_event(
        false,
        screen.root,
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
        change_state_event,
    )
    .context(format!(
        "Failed to send WM_CHANGE_STATE normal state event for window {}",
        window
    ))?;

    conn.flush()
        .context("Failed to flush X11 connection after window unminimize")?;
    Ok(())
}

/// Injects a synthetic MotionNotify event to force the client to re-evaluate the cursor position.
///
/// This is necessary for XWayland compatibility (e.g., Wine/Proton games) where clients
/// often fail to detect that the mouse is hovering them after being programmatically activated.
///
/// Use this instead of WarpPointer on Wayland sessions.
fn refresh_pointer_state(conn: &RustConnection, window: Window, timestamp: u32) -> Result<()> {
    // Construct a synthetic MotionNotify event
    // The goal is to tell the client "the mouse is right here" without moving it physically.
    let motion_event = MotionNotifyEvent {
        response_type: MOTION_NOTIFY_EVENT,
        detail: Motion::NORMAL,
        sequence: 0,
        time: timestamp,
        root: 0, // Not needed for this hack
        event: window,
        child: window,
        root_x: 0,  // Not needed
        root_y: 0,  // Not needed
        event_x: 0, // Not needed, client usually re-polls or just seeing the event is enough
        event_y: 0, // Not needed
        state: KeyButMask::default(),
        same_screen: true,
    };

    // Send the event directly to the window
    conn.send_event(
        false,                     // propagate
        window,                    // destination
        EventMask::POINTER_MOTION, // event mask
        motion_event,              // event content
    )?;

    tracing::debug!(
        window = window,
        "Injected synthetic MotionNotify to refresh pointer state"
    );

    Ok(())
}
