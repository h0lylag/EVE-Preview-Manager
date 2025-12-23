use anyhow::{Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, Window};

#[derive(Clone, Debug)]
pub struct WindowInfo {
    #[allow(dead_code)]
    pub id: Window,
    pub title: String,
    pub class: String,
}

pub fn get_running_applications() -> Result<Vec<WindowInfo>> {
    let (conn, screen_num) = x11rb::connect(None).context("Failed to connect to X11")?;
    let screen = &conn.setup().roots[screen_num];

    // Get _NET_CLIENT_LIST atom
    let net_client_list = conn
        .intern_atom(false, b"_NET_CLIENT_LIST")?
        .reply()
        .context("Failed to intern _NET_CLIENT_LIST")?
        .atom;

    let utf8_string = conn
        .intern_atom(false, b"UTF8_STRING")?
        .reply()
        .context("Failed to intern UTF8_STRING")?
        .atom;

    let wm_name = conn
        .intern_atom(false, b"_NET_WM_NAME")?
        .reply()
        .context("Failed to intern _NET_WM_NAME")?
        .atom;

    // Get list of windows
    let reply = conn
        .get_property(
            false,
            screen.root,
            net_client_list,
            AtomEnum::WINDOW,
            0,
            1024, // Expect reasonable number of windows
        )?
        .reply()
        .context("Failed to get _NET_CLIENT_LIST")?;

    let mut windows = Vec::new();

    if let Some(values) = reply.value32() {
        for window in values {
            // Get WM_CLASS
            let class_reply = conn
                .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)?
                .reply();

            // Get Title (_NET_WM_NAME or WM_NAME)
            let title_reply = conn
                .get_property(false, window, wm_name, utf8_string, 0, 1024)?
                .reply();

            let class = if let Ok(reply) = class_reply {
                // WM_CLASS contains two null-terminated strings: instance and class. We usually want the second (class).
                // But sometimes they are same. Let's parse.
                // "firefox\0Firefox\0"
                let val = reply.value;
                let s = String::from_utf8_lossy(&val);
                let parts: Vec<&str> = s.split('\0').collect();
                if parts.len() >= 2 && !parts[1].is_empty() {
                    parts[1].to_string() // Class name (capitalized usually)
                } else if !parts.is_empty() && !parts[0].is_empty() {
                    parts[0].to_string()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            let title = if let Ok(reply) = title_reply {
                String::from_utf8_lossy(&reply.value).to_string()
            } else {
                // Fallback to WM_NAME
                if let Ok(reply) = conn
                    .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 1024)?
                    .reply()
                {
                    String::from_utf8_lossy(&reply.value).to_string()
                } else {
                    String::new()
                }
            };

            // Basic filtering
            if !class.is_empty() && !title.is_empty() {
                // Determine if we should show it
                // Skip EVE Preview Manager itself?
                if class != "eve-preview-manager" && class != "com.evepreview.manager" {
                    windows.push(WindowInfo {
                        id: window,
                        title,
                        class,
                    });
                }
            }
        }
    }

    // Sort by class for easier reading
    windows.sort_by(|a, b| a.class.cmp(&b.class).then(a.title.cmp(&b.title)));

    Ok(windows)
}
