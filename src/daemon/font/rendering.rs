//! Font rendering logic (Fontdue + X11 fallback)

use anyhow::{Context, Result};
use fontdue::{Font, FontSettings};
use std::fs;
use std::path::PathBuf;
use tracing::{debug, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as XprotoExt, Font as X11Font};

use super::discovery::{find_font_path, select_best_default_font};

/// Rendered text as BGRA bitmap (optimized for X11)
pub struct RenderedText {
    pub width: usize,
    pub height: usize,
    /// Little-endian ARGB (BGRA in memory): Blue, Green, Red, Alpha
    pub data: Vec<u8>,
}

/// Font renderer with TrueType (fontdue) or X11 core font fallback
#[derive(Debug)]
pub enum FontRenderer {
    Fontdue { font: Font, size: f32 },
    X11Fallback { font_id: X11Font, size: f32 },
}

impl FontRenderer {
    /// Load a TrueType font from a file path
    pub fn from_path(path: PathBuf, size: f32) -> Result<Self> {
        debug!(path = %path.display(), size = size, "Attempting to load font from path");

        let font_data = fs::read(&path).with_context(|| {
            format!(
                "Failed to read font file: {}. Check that the file exists and is readable.",
                path.display()
            )
        })?;

        let font = Font::from_bytes(font_data, FontSettings::default())
            .map_err(|e| anyhow::anyhow!(
                "Failed to parse font file '{}': {}. Font may be corrupt or in an unsupported format.",
                path.display(),
                e
            ))?;

        debug!(path = %path.display(), "Successfully loaded font from path");
        Ok(Self::Fontdue { font, size })
    }

    /// Load font from a font name via fontconfig
    pub fn from_font_name(font_name: &str, size: f32) -> Result<Self> {
        debug!(font_name = %font_name, size = size, "Resolving font via fontconfig");

        let font_path = find_font_path(font_name).with_context(|| {
            format!(
                "Failed to resolve font '{}'. Font not found or not installed. \
                 Use 'fc-list' to see available fonts.",
                font_name
            )
        })?;

        debug!(font_name = %font_name, resolved_path = %font_path.display(), "Resolved font name to path via fontconfig");

        let path_display = font_path.display().to_string();
        Self::from_path(font_path, size).with_context(|| {
            format!(
                "Failed to load font '{}' from path '{}'. \
                 Font file may be corrupt or in an unsupported format.",
                font_name, path_display
            )
        })
    }

    /// Try to load best available system font with automatic X11 fallback
    pub fn from_system_font<C: Connection>(conn: &C, size: f32) -> Result<Self> {
        debug!(size = size, "Loading default system font");

        match select_best_default_font() {
            Ok((name, path)) => {
                debug!(font = %name, "Using TrueType font via fontdue");
                Self::from_path(path, size)
            }
            Err(e) => {
                warn!(error = %e, "No TrueType fonts available, falling back to X11 core fonts");

                let font_id = conn
                    .generate_id()
                    .context("Failed to generate X11 font ID")?;
                conn.open_font(font_id, b"fixed")
                    .context("Failed to open X11 'fixed' font")?;

                info!("Using X11 core font 'fixed' (basic rendering)");
                Ok(Self::X11Fallback { font_id, size })
            }
        }
    }

    /// Resolve font from configuration or fallback to system default
    pub fn resolve_from_config<C: Connection>(
        conn: &C,
        font_name: &str,
        font_size: f32,
    ) -> Result<Self> {
        if !font_name.is_empty() {
            debug!(
                configured_font = %font_name,
                size = font_size,
                "Attempting to load user-configured font"
            );
            Self::from_font_name(font_name, font_size).or_else(|e| {
                warn!(
                    font = %font_name,
                    error = ?e,
                    "Failed to load configured font, falling back to system default"
                );
                Self::from_system_font(conn, font_size)
            })
        } else {
            debug!(size = font_size, "No font configured, using system default");
            Self::from_system_font(conn, font_size)
        }
        .context(format!(
            "Failed to initialize font renderer with size {}",
            font_size
        ))
    }

    pub fn requires_direct_rendering(&self) -> bool {
        matches!(self, Self::X11Fallback { .. })
    }

    pub fn x11_font_id(&self) -> Option<X11Font> {
        match self {
            Self::X11Fallback { font_id, .. } => Some(*font_id),
            _ => None,
        }
    }

    pub fn size(&self) -> f32 {
        match self {
            Self::Fontdue { size, .. } => *size,
            Self::X11Fallback { size, .. } => *size,
        }
    }

    /// Render text to a BGRA bitmap (X11 optimized)
    pub fn render_text(&self, text: &str, fg_color: u32) -> Result<RenderedText> {
        match self {
            Self::Fontdue { font, size } => {
                if text.is_empty() {
                    return Ok(RenderedText {
                        width: 0,
                        height: 0,
                        data: Vec::new(),
                    });
                }

                let mut glyphs = Vec::new();
                let mut x = 0.0f32;
                let mut max_ascent = 0i32;
                let mut max_descent = 0i32;

                for ch in text.chars() {
                    let (metrics, bitmap) = font.rasterize(ch, *size);
                    let ascent = metrics.height as i32 + metrics.ymin;
                    let descent = -metrics.ymin;
                    max_ascent = max_ascent.max(ascent);
                    max_descent = max_descent.max(descent);
                    glyphs.push((x as i32, metrics, bitmap));
                    x += metrics.advance_width;
                }

                let width = x.ceil() as usize;
                let height = (max_ascent + max_descent) as usize;

                if width == 0 || height == 0 {
                    return Ok(RenderedText {
                        width: 0,
                        height: 0,
                        data: Vec::new(),
                    });
                }

                // Allocate buffer for BGRA data (4 bytes per pixel)
                let mut data = vec![0u8; width * height * 4];

                // Pre-calculate color components
                let fg_a = (fg_color >> 24) & 0xFF;
                let fg_r = (fg_color >> 16) & 0xFF;
                let fg_g = (fg_color >> 8) & 0xFF;
                let fg_b = fg_color & 0xFF;

                for (x_offset, metrics, bitmap) in glyphs {
                    let baseline_y = max_ascent - (metrics.height as i32 + metrics.ymin);

                    for gy in 0..metrics.height {
                        for gx in 0..metrics.width {
                            let px = x_offset + gx as i32;
                            let py = baseline_y + gy as i32;

                            if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                                continue;
                            }

                            let coverage = bitmap[gy * metrics.width + gx] as u32;

                            if coverage > 0 {
                                // Integer math for performance
                                // pixel = color * coverage / 255
                                let alpha = (fg_a * coverage) / 255;
                                let r = (fg_r * coverage) / 255;
                                let g = (fg_g * coverage) / 255;
                                let b = (fg_b * coverage) / 255;

                                let idx = ((py as usize) * width + (px as usize)) * 4;

                                // Write BGRA directly (Little Endian)
                                data[idx] = b as u8;
                                data[idx + 1] = g as u8;
                                data[idx + 2] = r as u8;
                                data[idx + 3] = alpha as u8;
                            }
                        }
                    }
                }

                Ok(RenderedText {
                    width,
                    height,
                    data,
                })
            }
            Self::X11Fallback { .. } => Ok(RenderedText {
                width: 0,
                height: 0,
                data: Vec::new(),
            }),
        }
    }
}
