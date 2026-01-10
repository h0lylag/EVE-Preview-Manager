use anyhow::{Context, Result, anyhow};
use std::io::Cursor;
use std::process::{Child, Command};

#[cfg(target_os = "linux")]
pub fn load_tray_icon_pixmap() -> Result<ksni::Icon> {
    let icon_bytes = include_bytes!("../../assets/com.evepreview.manager.png");
    let decoder = png::Decoder::new(Cursor::new(icon_bytes));
    let mut reader = decoder.read_info()?;
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .context("PNG has no output buffer size")?
    ];
    let info = reader.next_frame(&mut buf)?;
    let rgba = &buf[..info.buffer_size()];

    // Convert RGBA to ARGB for ksni
    let argb: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => {
            rgba.chunks_exact(4)
                .flat_map(|chunk| [chunk[3], chunk[0], chunk[1], chunk[2]]) // RGBA → ARGB
                .collect()
        }
        png::ColorType::Rgb => {
            rgba.chunks_exact(3)
                .flat_map(|chunk| [0xFF, chunk[0], chunk[1], chunk[2]]) // RGB → ARGB (full alpha)
                .collect()
        }
        other => {
            return Err(anyhow!(
                "Unsupported icon color type {:?} (expected RGB or RGBA)",
                other
            ));
        }
    };

    Ok(ksni::Icon {
        width: info.width as i32,
        height: info.height as i32,
        data: argb,
    })
}

/// Load window icon from embedded PNG (same as tray icon)
#[cfg(target_os = "linux")]
pub fn load_window_icon() -> Result<egui::IconData> {
    let icon_bytes = include_bytes!("../../assets/com.evepreview.manager.png");
    let decoder = png::Decoder::new(Cursor::new(icon_bytes));
    let mut reader = decoder.read_info()?;
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .context("PNG has no output buffer size")?
    ];
    let info = reader.next_frame(&mut buf)?;
    let rgba = &buf[..info.buffer_size()];

    // egui IconData expects RGBA format
    let rgba_vec = match info.color_type {
        png::ColorType::Rgba => rgba.to_vec(),
        png::ColorType::Rgb => {
            // Convert RGB to RGBA
            let mut rgba_data = Vec::with_capacity(rgba.len() / 3 * 4);
            for chunk in rgba.chunks_exact(3) {
                rgba_data.extend_from_slice(chunk);
                rgba_data.push(0xFF); // Add full alpha
            }
            rgba_data
        }
        other => {
            return Err(anyhow!(
                "Unsupported window icon color type {:?} (expected RGB or RGBA)",
                other
            ));
        }
    };

    Ok(egui::IconData {
        rgba: rgba_vec,
        width: info.width,
        height: info.height,
    })
}

pub fn spawn_daemon(ipc_server_name: &str) -> Result<Child> {
    let exe_path = std::env::current_exe().context("Failed to resolve executable path")?;
    Command::new(exe_path)
        .arg("daemon")
        .arg("--ipc-server")
        .arg(ipc_server_name)
        .spawn()
        .context("Failed to spawn daemon process")
}

/// Parse hex color string - supports both #RRGGBB and #AARRGGBB formats.
/// Returns a Color32 if parsing succeeds, treating 6-digit hex as full-opacity RGB.
pub fn parse_hex_color(hex: &str) -> Result<egui::Color32, ()> {
    let hex = hex.trim_start_matches('#');

    match hex.len() {
        6 => {
            // RGB format - assume full opacity
            let rr = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ())?;
            let gg = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ())?;
            let bb = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ())?;
            Ok(egui::Color32::from_rgba_unmultiplied(rr, gg, bb, 255))
        }
        8 => {
            // ARGB format
            let aa = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ())?;
            let rr = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ())?;
            let gg = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ())?;
            let bb = u8::from_str_radix(&hex[6..8], 16).map_err(|_| ())?;
            Ok(egui::Color32::from_rgba_unmultiplied(rr, gg, bb, aa))
        }
        _ => Err(()),
    }
}

/// Format egui Color32 to hex string (#AARRGGBB or #RRGGBB)
pub fn format_hex_color(color: egui::Color32) -> String {
    if color.a() == 255 {
        // Full opacity - use shorter RGB format
        format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
    } else {
        // Has transparency - use ARGB format
        format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            color.a(),
            color.r(),
            color.g(),
            color.b()
        )
    }
}
