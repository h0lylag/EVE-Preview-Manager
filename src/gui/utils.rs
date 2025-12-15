use anyhow::{Context, Result, anyhow};
use std::io::Cursor;
use std::process::{Child, Command};

#[cfg(target_os = "linux")]
pub fn load_tray_icon_pixmap() -> Result<ksni::Icon> {
    let icon_bytes = include_bytes!("../../assets/icon.png");
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
    let icon_bytes = include_bytes!("../../assets/icon.png");
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

pub fn spawn_preview_daemon() -> Result<Child> {
    let exe_path = std::env::current_exe().context("Failed to resolve executable path")?;
    Command::new(exe_path)
        .arg("--preview")
        .spawn()
        .context("Failed to spawn preview daemon")
}
