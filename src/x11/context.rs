//! Application context and cached X11 state

use anyhow::{Context, Result};
use x11rb::protocol::render::{ConnectionExt as RenderExt, Fixed, Pictformat};
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

use crate::common::constants::{fixed_point, x11};

/// Application context holding immutable shared state
pub struct AppContext<'a> {
    pub conn: &'a RustConnection,
    pub screen: &'a Screen,
    pub atoms: &'a CachedAtoms,
    pub formats: &'a CachedFormats,
}

/// Pre-cached X11 atoms to avoid repeated roundtrips
#[derive(Debug)]
pub struct CachedAtoms {
    pub wm_name: Atom,
    pub net_wm_pid: Atom,
    pub net_wm_state: Atom,
    pub net_wm_state_hidden: Atom,
    pub net_wm_state_above: Atom,
    pub net_wm_window_opacity: Atom,
    pub wm_class: Atom,
    pub net_active_window: Atom,
    pub wm_change_state: Atom,
    pub wm_state: Atom,
    pub net_client_list: Atom,
    pub net_wm_window_type: Atom,
    pub net_wm_window_type_dock: Atom,
    pub net_wm_window_type_desktop: Atom,
    pub net_wm_window_type_toolbar: Atom,
    pub net_wm_window_type_menu: Atom,
    pub net_wm_window_type_utility: Atom,
    pub net_wm_window_type_splash: Atom,
    pub net_wm_window_type_dropdown_menu: Atom,
    pub net_wm_window_type_popup_menu: Atom,
    pub net_wm_window_type_tooltip: Atom,
    pub net_wm_window_type_notification: Atom,
    pub net_wm_window_type_combo: Atom,
    pub net_wm_window_type_dnd: Atom,
}

impl CachedAtoms {
    pub fn new(conn: &RustConnection) -> Result<Self> {
        Ok(Self {
            wm_name: conn
                .intern_atom(false, b"WM_NAME")
                .context("Failed to intern WM_NAME atom")?
                .reply()
                .context("Failed to get reply for WM_NAME atom")?
                .atom,
            net_wm_pid: conn
                .intern_atom(false, b"_NET_WM_PID")
                .context("Failed to intern _NET_WM_PID atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_PID atom")?
                .atom,
            net_wm_state: conn
                .intern_atom(false, b"_NET_WM_STATE")
                .context("Failed to intern _NET_WM_STATE atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_STATE atom")?
                .atom,
            net_wm_state_hidden: conn
                .intern_atom(false, b"_NET_WM_STATE_HIDDEN")
                .context("Failed to intern _NET_WM_STATE_HIDDEN atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_STATE_HIDDEN atom")?
                .atom,
            net_wm_state_above: conn
                .intern_atom(false, b"_NET_WM_STATE_ABOVE")
                .context("Failed to intern _NET_WM_STATE_ABOVE atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_STATE_ABOVE atom")?
                .atom,
            net_wm_window_opacity: conn
                .intern_atom(false, b"_NET_WM_WINDOW_OPACITY")
                .context("Failed to intern _NET_WM_WINDOW_OPACITY atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_OPACITY atom")?
                .atom,
            wm_class: conn
                .intern_atom(false, b"WM_CLASS")
                .context("Failed to intern WM_CLASS atom")?
                .reply()
                .context("Failed to get reply for WM_CLASS atom")?
                .atom,
            net_active_window: conn
                .intern_atom(false, b"_NET_ACTIVE_WINDOW")
                .context("Failed to intern _NET_ACTIVE_WINDOW atom")?
                .reply()
                .context("Failed to get reply for _NET_ACTIVE_WINDOW atom")?
                .atom,
            wm_change_state: conn
                .intern_atom(false, b"WM_CHANGE_STATE")
                .context("Failed to intern WM_CHANGE_STATE atom")?
                .reply()
                .context("Failed to get reply for WM_CHANGE_STATE atom")?
                .atom,
            wm_state: conn
                .intern_atom(false, b"WM_STATE")
                .context("Failed to intern WM_STATE atom")?
                .reply()
                .context("Failed to get reply for WM_STATE atom")?
                .atom,
            net_client_list: conn
                .intern_atom(false, b"_NET_CLIENT_LIST")
                .context("Failed to intern _NET_CLIENT_LIST atom")?
                .reply()
                .context("Failed to get reply for _NET_CLIENT_LIST atom")?
                .atom,
            net_wm_window_type: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE")
                .context("Failed to intern _NET_WM_WINDOW_TYPE atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE atom")?
                .atom,
            net_wm_window_type_dock: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_DOCK")
                .context("Failed to intern _NET_WM_WINDOW_TYPE_DOCK atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE_DOCK atom")?
                .atom,
            net_wm_window_type_desktop: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_DESKTOP")
                .context("Failed to intern _NET_WM_WINDOW_TYPE_DESKTOP atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE_DESKTOP atom")?
                .atom,
            net_wm_window_type_toolbar: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_TOOLBAR")
                .context("Failed to intern _NET_WM_WINDOW_TYPE_TOOLBAR atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE_TOOLBAR atom")?
                .atom,
            net_wm_window_type_menu: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_MENU")
                .context("Failed to intern _NET_WM_WINDOW_TYPE_MENU atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE_MENU atom")?
                .atom,
            net_wm_window_type_utility: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_UTILITY")
                .context("Failed to intern _NET_WM_WINDOW_TYPE_UTILITY atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE_UTILITY atom")?
                .atom,
            net_wm_window_type_splash: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_SPLASH")
                .context("Failed to intern _NET_WM_WINDOW_TYPE_SPLASH atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE_SPLASH atom")?
                .atom,
            net_wm_window_type_dropdown_menu: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_DROPDOWN_MENU")
                .context("Failed to intern _NET_WM_WINDOW_TYPE_DROPDOWN_MENU atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE_DROPDOWN_MENU atom")?
                .atom,
            net_wm_window_type_popup_menu: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_POPUP_MENU")
                .context("Failed to intern _NET_WM_WINDOW_TYPE_POPUP_MENU atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE_POPUP_MENU atom")?
                .atom,
            net_wm_window_type_tooltip: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_TOOLTIP")
                .context("Failed to intern _NET_WM_WINDOW_TYPE_TOOLTIP atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE_TOOLTIP atom")?
                .atom,
            net_wm_window_type_notification: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_NOTIFICATION")
                .context("Failed to intern _NET_WM_WINDOW_TYPE_NOTIFICATION atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE_NOTIFICATION atom")?
                .atom,
            net_wm_window_type_combo: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_COMBO")
                .context("Failed to intern _NET_WM_WINDOW_TYPE_COMBO atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE_COMBO atom")?
                .atom,
            net_wm_window_type_dnd: conn
                .intern_atom(false, b"_NET_WM_WINDOW_TYPE_DND")
                .context("Failed to intern _NET_WM_WINDOW_TYPE_DND atom")?
                .reply()
                .context("Failed to get reply for _NET_WM_WINDOW_TYPE_DND atom")?
                .atom,
        })
    }
}

/// Pre-cached picture formats to avoid repeated expensive queries
#[derive(Debug)]
pub struct CachedFormats {
    pub rgb: Pictformat,
    pub argb: Pictformat,
}

impl CachedFormats {
    pub fn new(conn: &RustConnection, screen: &Screen) -> Result<Self> {
        let formats_reply = conn
            .render_query_pict_formats()
            .context("Failed to query RENDER picture formats")?
            .reply()
            .context("Failed to get RENDER formats reply")?;

        let rgb = formats_reply
            .formats
            .iter()
            .find(|f| f.depth == screen.root_depth && f.direct.alpha_mask == 0)
            .ok_or_else(|| anyhow::anyhow!("No RGB format found for depth {}", screen.root_depth))?
            .id;

        let argb = formats_reply
            .formats
            .iter()
            .find(|f| f.depth == x11::ARGB_DEPTH && f.direct.alpha_mask != 0)
            .ok_or_else(|| anyhow::anyhow!("No ARGB format found for depth {}", x11::ARGB_DEPTH))?
            .id;

        Ok(Self { rgb, argb })
    }
}

/// Converts standard float values to the 16.16 fixed-point format required by the X11 Render extension
pub fn to_fixed(v: f32) -> Fixed {
    (v * fixed_point::MULTIPLIER).round() as Fixed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_fixed_whole_numbers() {
        assert_eq!(to_fixed(1.0), 65536);
        assert_eq!(to_fixed(2.0), 131072);
        assert_eq!(to_fixed(0.0), 0);
    }

    #[test]
    fn test_to_fixed_fractional() {
        // 0.5 * 65536 = 32768
        assert_eq!(to_fixed(0.5), 32768);

        // 1.5 * 65536 = 98304
        assert_eq!(to_fixed(1.5), 98304);

        // 0.25 * 65536 = 16384
        assert_eq!(to_fixed(0.25), 16384);
    }

    #[test]
    fn test_to_fixed_negative() {
        assert_eq!(to_fixed(-1.0), -65536);
        assert_eq!(to_fixed(-0.5), -32768);
    }

    #[test]
    fn test_to_fixed_rounding() {
        // Test that rounding works correctly
        let result = to_fixed(1.0 / 3.0);
        // 1/3 * 65536 ≈ 21845.33, should round to 21845
        assert_eq!(result, 21845);
    }

    #[test]
    fn test_to_fixed_large_values() {
        // Test with screen coordinate scale values
        assert_eq!(to_fixed(1920.0), 1920 * 65536);
        assert_eq!(to_fixed(1080.0), 1080 * 65536);
    }
}
