//! Font rendering and discovery
//!
//! Provides TrueType rendering via fontdue with X11 core font fallback,
//! and font discovery via fontconfig.

use anyhow::{Context, Result};
use fontconfig::{Fontconfig, Pattern};
use fontdue::{Font, FontSettings};
use std::collections::BTreeSet;
use std::ffi::CString;
use std::fs;
use std::path::PathBuf;
use tracing::{debug, info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as XprotoExt, Font as X11Font};

// ============================================================================
// Font Discovery
// ============================================================================

/// Common font style names for parsing family+style strings
const KNOWN_STYLES: &[&str] = &[
    // Condensed variants (longest first)
    "Condensed Bold Oblique",
    "Condensed Bold Italic",
    "Condensed Bold",
    "Condensed Oblique",
    "Condensed Italic",
    "Condensed",
    // Weight + style combinations
    "ExtraBold Italic",
    "ExtraLight Italic",
    "SemiBold Italic",
    "Black Italic",
    "Medium Italic",
    "Light Italic",
    "Thin Italic",
    "Bold Oblique",
    "Bold Italic",
    // Weights (heavy to light)
    "ExtraBold",
    "Black",
    "Bold",
    "SemiBold",
    "Medium",
    "Book",
    "Regular",
    "Light",
    "ExtraLight",
    "Thin",
    // Styles
    "Oblique",
    "Italic",
    // Width variants
    "Expanded",
];

/// Get list of all individual fonts with their full names
pub fn list_fonts() -> Result<Vec<String>> {
    info!("Loading available fonts from fontconfig...");
    let fc = Fontconfig::new().context("Failed to initialize fontconfig")?;
    let pattern = Pattern::new(&fc);
    let font_set = fontconfig::list_fonts(&pattern, None);

    let mut fonts = BTreeSet::new();
    for font_pattern in font_set.iter() {
        let family = font_pattern
            .get_string(fontconfig::FC_FAMILY)
            .unwrap_or("Unknown");

        let font_name = if let Some(style_str) = font_pattern.get_string(fontconfig::FC_STYLE) {
            if style_str == "Regular" {
                family.to_string()
            } else {
                format!("{} {}", family, style_str)
            }
        } else {
            family.to_string()
        };

        fonts.insert(font_name);
    }

    info!(
        count = fonts.len(),
        "Discovered individual fonts via fontconfig"
    );
    Ok(fonts.into_iter().collect())
}

/// Find best matching font file path for a given family name or full font name
pub fn find_font_path(font_name: &str) -> Result<PathBuf> {
    let fc = Fontconfig::new().context("Failed to initialize fontconfig")?;

    let mut family_name = font_name;
    let mut style_name: Option<&str> = None;

    for style in KNOWN_STYLES {
        if let Some(style_pos) = font_name.rfind(style)
            && style_pos + style.len() == font_name.len()
        {
            let prefix = &font_name[..style_pos];
            if prefix.is_empty() || prefix.ends_with(' ') {
                family_name = prefix.trim();
                style_name = Some(style);
                debug!(
                    font = font_name,
                    family = family_name,
                    style = style,
                    "Parsed font into family and style"
                );
                break;
            }
        }
    }

    let mut pattern = Pattern::new(&fc);
    let family_cstr = CString::new(family_name)
        .with_context(|| format!("Invalid family name: {}", family_name))?;
    pattern.add_string(fontconfig::FC_FAMILY, &family_cstr);

    if let Some(style) = style_name {
        let style_cstr =
            CString::new(style).with_context(|| format!("Invalid style name: {}", style))?;
        pattern.add_string(fontconfig::FC_STYLE, &style_cstr);
    }

    let matched = pattern.font_match();

    if let Some(matched_family) = matched.get_string(fontconfig::FC_FAMILY)
        && !matched_family.eq_ignore_ascii_case(family_name)
    {
        warn!(
            requested = font_name,
            requested_family = family_name,
            matched_family = matched_family,
            "Fontconfig returned different font family - requested font may not be installed"
        );
        return Err(anyhow::anyhow!(
            "Font '{}' not found - fontconfig returned family '{}' instead",
            font_name,
            matched_family
        ));
    }

    let file_path = matched
        .filename()
        .with_context(|| format!("No font file found for '{}'", font_name))?;

    let path = PathBuf::from(file_path);

    if !path.exists() {
        warn!(
            font = font_name,
            path = %path.display(),
            "Font file path from fontconfig does not exist"
        );
        return Err(anyhow::anyhow!(
            "Font file path '{}' does not exist",
            path.display()
        ));
    }

    debug!(
        font = font_name,
        family = family_name,
        style = ?style_name,
        path = %path.display(),
        "Resolved font path via family + style"
    );

    Ok(path)
}

/// Scans for a suitable default TrueType font from a hardcoded list of preferred fonts.
/// Returns the first match found on the system.
pub fn select_best_default_font() -> Result<(String, PathBuf)> {
    let candidates = crate::constants::defaults::text::FONT_CANDIDATES;

    for candidate in candidates {
        if let Ok(path) = find_font_path(candidate)
            && path.exists()
        {
            info!(font = candidate, path = %path.display(), "Selected default font via fontconfig");
            return Ok((candidate.to_string(), path));
        }
    }

    debug!("Specific fonts not found, querying for any monospace font");
    let fc = Fontconfig::new().context("Failed to initialize fontconfig")?;
    let mut pattern = Pattern::new(&fc);
    pattern.add_integer(fontconfig::FC_SPACING, 100);

    let font_set = fontconfig::list_fonts(&pattern, None);

    for font_pattern in font_set.iter() {
        let family = font_pattern
            .get_string(fontconfig::FC_FAMILY)
            .unwrap_or("Unknown");

        if let Some(style) = font_pattern.get_string(fontconfig::FC_STYLE) {
            let style_lower = style.to_lowercase();
            if style_lower.contains("bold")
                || style_lower.contains("italic")
                || style_lower.contains("oblique")
            {
                continue;
            }
        }

        if let Some(file_path) = font_pattern.filename() {
            let path = PathBuf::from(file_path);
            if path.exists() {
                info!(font = family, path = %path.display(), "Selected first available monospace font");
                return Ok((family.to_string(), path));
            }
        }
    }

    Err(anyhow::anyhow!(
        "No TrueType fonts found. Tried:\n\
         - Specific fonts: {:?}\n\
         - Any monospace Regular/Normal font via fontconfig\n\
         \n\
         Will fall back to X11 core fonts.",
        candidates
    ))
}

// ============================================================================
// Font Rendering
// ============================================================================

/// Rendered text as ARGB bitmap
pub struct RenderedText {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u32>,
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
        info!(path = %path.display(), size = size, "Attempting to load font from path");

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

        info!(path = %path.display(), "Successfully loaded font from path");
        Ok(Self::Fontdue { font, size })
    }

    /// Load font from a font name via fontconfig
    pub fn from_font_name(font_name: &str, size: f32) -> Result<Self> {
        info!(font_name = %font_name, size = size, "Resolving font via fontconfig");

        let font_path = find_font_path(font_name).with_context(|| {
            format!(
                "Failed to resolve font '{}'. Font not found or not installed. \
                 Use 'fc-list' to see available fonts.",
                font_name
            )
        })?;

        info!(font_name = %font_name, resolved_path = %font_path.display(), "Resolved font name to path via fontconfig");

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
        info!(size = size, "Loading default system font");

        match select_best_default_font() {
            Ok((name, path)) => {
                info!(font = %name, "Using TrueType font via fontdue");
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
            info!(
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
            info!(size = font_size, "No font configured, using system default");
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

    /// Render text to an ARGB bitmap
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

                let mut data = vec![0x00000000; width * height];

                let fg_a = ((fg_color >> 24) & 0xFF) as f32 / 255.0;
                let fg_r = ((fg_color >> 16) & 0xFF) as f32 / 255.0;
                let fg_g = ((fg_color >> 8) & 0xFF) as f32 / 255.0;
                let fg_b = (fg_color & 0xFF) as f32 / 255.0;

                for (x_offset, metrics, bitmap) in glyphs {
                    let baseline_y = max_ascent - (metrics.height as i32 + metrics.ymin);

                    for gy in 0..metrics.height {
                        for gx in 0..metrics.width {
                            let px = x_offset + gx as i32;
                            let py = baseline_y + gy as i32;

                            if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                                continue;
                            }

                            let coverage = bitmap[gy * metrics.width + gx] as f32 / 255.0;

                            if coverage > 0.0 {
                                let alpha = (fg_a * coverage * 255.0) as u32;
                                let r = (fg_r * coverage * 255.0) as u32;
                                let g = (fg_g * coverage * 255.0) as u32;
                                let b = (fg_b * coverage * 255.0) as u32;

                                let pixel = (alpha << 24) | (r << 16) | (g << 8) | b;
                                data[(py as usize) * width + (px as usize)] = pixel;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_common_fonts() {
        let test_families = vec!["DejaVu Sans", "Liberation Sans", "Monospace"];

        for family in test_families {
            if let Ok(path) = find_font_path(family) {
                println!("{} -> {}", family, path.display());
                assert!(path.is_absolute(), "Font path should be absolute");
            }
        }
    }
}
