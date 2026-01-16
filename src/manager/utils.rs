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

pub fn spawn_daemon(ipc_server_name: &str, debug: bool) -> Result<Child> {
    let exe_path = std::env::current_exe().context("Failed to resolve executable path")?;
    let mut command = Command::new(exe_path);
    command
        .arg("daemon")
        .arg("--ipc-server")
        .arg(ipc_server_name);

    if debug {
        command.arg("--debug");
    }

    command.spawn().context("Failed to spawn daemon process")
}
