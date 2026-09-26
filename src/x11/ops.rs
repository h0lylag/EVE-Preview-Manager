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
/// A successful send does not confirm WM acceptance or input focus.
///
/// # Arguments
/// * `timestamp` - Last user-activity time in the X server's clock domain.
///   X11 callers retain their input-event time; evdev samples when handling the hotkey.
///   EWMH 3.8 defines the payload and permits the WM to refuse activation:
///   <https://specifications.freedesktop.org/wm/latest/ar01s03.html>.
pub fn activate_window(
    conn: &RustConnection,
    screen: &Screen,
    atoms: &CachedAtoms,
    window: Window,
    timestamp: u32,
) -> Result<()> {
    // Retain the existing stacking request for previously reported client/WM
    // compatibility issues. X11 ConfigureWindow allows the WM to intercept it;
    // submitting StackMode::ABOVE does not confirm that the window was raised.
    // https://www.x.org/releases/X11R7.7/doc/xproto/x11protocol.html
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

/// Requests ICCCM iconification, preceded by the existing EWMH HIDDEN hint.
/// HIDDEN is derived state; WM_CHANGE_STATE(IconicState) below is the ICCCM
/// minimization request. See EWMH 5.7 and ICCCM 4.1.4:
/// <https://specifications.freedesktop.org/wm/latest/ar01s05.html> and
/// <https://www.x.org/releases/X11R7.7/doc/xorg-docs/icccm/icccm.html>.
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

    // ICCCM 4.1.4 defines WM_CHANGE_STATE only for requesting IconicState.
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

/// Requests the ICCCM Iconic-to-Normal transition by mapping the source window.
/// A successful request does not confirm that the window manager restored or focused it.
/// Map the client window, not its WM frame; a managing client receives MapRequest
/// when it selects SubstructureRedirect on the parent. See ICCCM 4.1.4 and X11 MapWindow:
/// <https://www.x.org/releases/X11R7.7/doc/xorg-docs/icccm/icccm.html> and
/// <https://www.x.org/releases/X11R7.7/doc/xproto/x11protocol.html>.
pub fn unminimize_window(conn: &RustConnection, window: Window) -> Result<()> {
    conn.map_window(window)
        .context(format!("Failed to request mapping window {}", window))?
        .check()
        .context(format!("Failed to map window {} for restoration", window))?;
    conn.flush()
        .context("Failed to flush X11 connection after window restore request")?;
    Ok(())
}

/// Sends a synthetic MotionNotify nudge for the existing Wine/XWayland cursor workaround.
///
/// Whether the client reacts is application-specific. X11 SendEvent delivers the
/// supplied payload; it does not replace its time with the server's current time.
/// <https://www.x.org/releases/X11R7.7/doc/xproto/x11protocol.html>.
///
/// Use this instead of WarpPointer on Wayland sessions.
pub(crate) fn refresh_pointer_state(
    conn: &RustConnection,
    window: Window,
    timestamp: u32,
) -> Result<()> {
    let pointer = conn
        .query_pointer(window)
        .context("Failed to query pointer for refresh_pointer_state")?
        .reply()
        .context("Failed to get QueryPointer reply for refresh_pointer_state")?;

    if !pointer.same_screen {
        tracing::debug!(
            window = window,
            "Skipping pointer refresh because pointer is not on the same screen"
        );
        return Ok(());
    }

    // Use a tiny synthetic nudge so Wine/EVE observes a motion transition without
    // poisoning cursor state with the legacy (0,0) coordinates.
    let jitter_x = if pointer.win_x > 0 { -1 } else { 1 };
    let jitter_y = if pointer.win_y > 0 { -1 } else { 1 };

    let motion_event = MotionNotifyEvent {
        response_type: MOTION_NOTIFY_EVENT,
        detail: Motion::NORMAL,
        sequence: 0,
        time: timestamp,
        root: pointer.root,
        event: window,
        child: window,
        root_x: pointer.root_x.saturating_add(jitter_x),
        root_y: pointer.root_y.saturating_add(jitter_y),
        event_x: pointer.win_x.saturating_add(jitter_x),
        event_y: pointer.win_y.saturating_add(jitter_y),
        // Use default to ensure we don't accidentally trigger "Drag" logic
        // if a mouse button was physically held during this call.
        state: KeyButMask::default(),
        same_screen: true, // Force the client to treat it as a local event
    };

    conn.send_event(false, window, EventMask::POINTER_MOTION, motion_event)?;

    tracing::debug!(
        window = window,
        event_x = motion_event.event_x,
        event_y = motion_event.event_y,
        "Injected synthetic MotionNotify nudge to refresh pointer state"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Run on an isolated display:
    //! ```sh
    //! xvfb-run -a -s "-screen 0 1280x800x24 -nolisten tcp -noreset" \
    //!   env EPM_X11_TESTS=1 timeout 60s \
    //!   cargo test --locked --all-features x11::ops::tests -- --ignored --test-threads=1
    //! ```
    use super::*;
    use x11rb::protocol::Event;

    fn test_connection() -> (RustConnection, usize) {
        assert_eq!(
            std::env::var("EPM_X11_TESTS").as_deref(),
            Ok("1"),
            "run display tests with EPM_X11_TESTS=1 under isolated Xvfb"
        );
        x11rb::connect(None).unwrap()
    }

    fn window(conn: &RustConnection, parent: Window) -> Window {
        let window = conn.generate_id().unwrap();
        conn.create_window(
            0,
            window,
            parent,
            0,
            0,
            100,
            100,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )
        .unwrap()
        .check()
        .unwrap();
        window
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn restore_maps_windows_and_reports_destroyed_targets() {
        let (conn, screen_number) = test_connection();
        let window = window(&conn, conn.setup().roots[screen_number].root);
        assert_eq!(
            conn.get_window_attributes(window)
                .unwrap()
                .reply()
                .unwrap()
                .map_state,
            MapState::UNMAPPED
        );
        for _ in 0..2 {
            unminimize_window(&conn, window).unwrap();
            assert_eq!(
                conn.get_window_attributes(window)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .map_state,
                MapState::VIEWABLE
            );
        }
        conn.destroy_window(window).unwrap().check().unwrap();
        assert!(unminimize_window(&conn, window).is_err());
    }

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn restore_defers_to_wm_for_root_and_reparented_clients() {
        let (client, screen_number) = test_connection();
        let (wm, _) = test_connection();
        let root = client.setup().roots[screen_number].root;
        let frame = window(&wm, root);
        wm.map_window(frame).unwrap().check().unwrap();
        client
            .set_input_focus(InputFocus::PARENT, root, x11rb::CURRENT_TIME)
            .unwrap()
            .check()
            .unwrap();

        for parent in [root, frame] {
            wm.change_window_attributes(
                parent,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_REDIRECT),
            )
            .unwrap()
            .check()
            .unwrap();
            let target = window(&client, root);
            if parent == frame {
                wm.reparent_window(target, frame, 0, 0)
                    .unwrap()
                    .check()
                    .unwrap();
                wm.unmap_window(frame).unwrap().check().unwrap();
            }
            unminimize_window(&client, target).unwrap();
            // check() acknowledges server processing, not WM action. Deliberately
            // leave the request unhandled first: the target must remain unmapped.
            assert_eq!(
                client
                    .get_window_attributes(target)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .map_state,
                MapState::UNMAPPED
            );
            assert_eq!(
                client.get_input_focus().unwrap().reply().unwrap().focus,
                root
            );
            let Event::MapRequest(request) = wm.wait_for_event().unwrap() else {
                panic!("restore must emit MapRequest, not a state ClientMessage");
            };
            assert_eq!((request.parent, request.window), (parent, target));
            wm.map_window(target).unwrap().check().unwrap();
            if parent == frame {
                // A minimized WM frame is also unmapped. Only the WM restores
                // that frame; mapping the client alone cannot make it viewable.
                assert_eq!(
                    client
                        .get_window_attributes(target)
                        .unwrap()
                        .reply()
                        .unwrap()
                        .map_state,
                    MapState::UNVIEWABLE
                );
                wm.map_window(frame).unwrap().check().unwrap();
            }
            assert_eq!(
                client
                    .get_window_attributes(target)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .map_state,
                MapState::VIEWABLE
            );
            // Mapping itself does not force keyboard focus.
            assert_eq!(
                client.get_input_focus().unwrap().reply().unwrap().focus,
                root
            );
            client.destroy_window(target).unwrap().check().unwrap();
        }
    }
}
