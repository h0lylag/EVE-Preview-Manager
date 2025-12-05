//! Font rendering with two-tier fallback: TrueType (fontdue) or X11 core fonts

use anyhow::{Context, Result};
use fontdue::{Font, FontSettings};
use std::fs;
use std::path::PathBuf;
use tracing::{info, warn};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as XprotoExt, Font as X11Font}; // X11 Font is just u32

/// Rendered text as ARGB bitmap
pub struct RenderedText {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u32>, // ARGB pixels (premultiplied alpha)
}

/// Font renderer with two-tier fallback: TrueType (fontdue) or X11 core fonts
#[derive(Debug)]
pub enum FontRenderer {
    /// High-quality TrueType rendering via fontdue (preferred)
    Fontdue { font: Font, size: f32 },
    /// Fallback to X11 core fonts (guaranteed available, basic rendering)
    X11Fallback { font_id: X11Font, size: f32 },
}

impl FontRenderer {
    /// Load a TrueType font from a file path
    pub fn from_path(path: PathBuf, size: f32) -> Result<Self> {
        info!(path = %path.display(), size = size, "Attempting to load font from path");
        
        let font_data = fs::read(&path)
            .with_context(|| format!(
                "Failed to read font file: {}. Check that the file exists and is readable.",
                path.display()
            ))?;
        
        let font = Font::from_bytes(font_data, FontSettings::default())
            .map_err(|e| anyhow::anyhow!(
                "Failed to parse font file '{}': {}. Font may be corrupt or in an unsupported format.",
                path.display(),
                e
            ))?;
        
        info!(path = %path.display(), "Successfully loaded font from path");
        Ok(Self::Fontdue { font, size })
    }
    
    /// Load font from a font name (family or fullname) via fontconfig
    pub fn from_font_name(font_name: &str, size: f32) -> Result<Self> {
        info!(font_name = %font_name, size = size, "Resolving font via fontconfig");
        
        let font_path = crate::preview::find_font_path(font_name)
            .with_context(|| format!(
                "Failed to resolve font '{}'. Font not found or not installed. \
                 Use 'fc-list' to see available fonts.",
                font_name
            ))?;
        
        info!(font_name = %font_name, resolved_path = %font_path.display(), "Resolved font name to path via fontconfig");
        
        // Capture path string before move for error context
        let path_display = font_path.display().to_string();
        Self::from_path(font_path, size)
            .with_context(|| format!(
                "Failed to load font '{}' from path '{}'. \
                 Font file may be corrupt or in an unsupported format.",
                font_name,
                path_display
            ))
    }
    
    /// Try to load best available system font with automatic X11 fallback
    pub fn from_system_font<C: Connection>(conn: &C, size: f32) -> Result<Self> {
        info!(size = size, "Loading default system font");
        
        // Try TrueType fonts first (preferred)
        match crate::preview::select_best_default_font() {
            Ok((name, path)) => {
                info!(font = %name, "Using TrueType font via fontdue");
                Self::from_path(path, size)
            }
            Err(e) => {
                warn!(error = %e, "No TrueType fonts available, falling back to X11 core fonts");
                
                // Generate font ID and open the font
                let font_id = conn.generate_id()
                    .context("Failed to generate X11 font ID")?;
                conn.open_font(font_id, b"fixed")
                    .context("Failed to open X11 'fixed' font")?;
                
                info!("Using X11 core font 'fixed' (basic rendering)");
                Ok(Self::X11Fallback { font_id, size })
            }
        }
    }
    
    /// Check if this renderer requires direct X11 rendering (cannot pre-render to bitmap)
    pub fn requires_direct_rendering(&self) -> bool {
        matches!(self, Self::X11Fallback { .. })
    }
    
    /// Get the X11 font ID (only valid for X11Fallback variant)
    pub fn x11_font_id(&self) -> Option<X11Font> {
        match self {
            Self::X11Fallback { font_id, .. } => Some(*font_id),
            _ => None,
        }
    }
    
    /// Get the font size
    pub fn size(&self) -> f32 {
        match self {
            Self::Fontdue { size, .. } => *size,
            Self::X11Fallback { size, .. } => *size,
        }
    }
    
    /// Render text to an ARGB bitmap with the given foreground color (transparent background)
    /// For X11 fallback variant, returns empty (rendering happens directly via ImageText8)
    pub fn render_text(
        &self,
        text: &str,
        fg_color: u32,  // ARGB format
    ) -> Result<RenderedText> {
        match self {
            Self::Fontdue { font, size } => {
                // TrueType rendering via fontdue
                if text.is_empty() {
                    return Ok(RenderedText {
                        width: 0,
                        height: 0,
                        data: Vec::new(),
                    });
                }
                
                // Layout glyphs
                let mut glyphs = Vec::new();
                let mut x = 0.0f32;
                let mut max_ascent = 0i32;
                let mut max_descent = 0i32;
                
                for ch in text.chars() {
                    let (metrics, bitmap) = font.rasterize(ch, *size);
                    
                    // Track the maximum ascent and descent
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
                
                // Create ARGB bitmap filled with fully transparent pixels
                let mut data = vec![0x00000000; width * height];
                
                // Extract color components (foreground is NOT premultiplied - raw ARGB)
                let fg_a = ((fg_color >> 24) & 0xFF) as f32 / 255.0;
                let fg_r = ((fg_color >> 16) & 0xFF) as f32 / 255.0;
                let fg_g = ((fg_color >> 8) & 0xFF) as f32 / 255.0;
                let fg_b = (fg_color & 0xFF) as f32 / 255.0;
                
                // Render each glyph
                for (x_offset, metrics, bitmap) in glyphs {
                    // Position glyph relative to baseline (which is at max_ascent from top)
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
                                // Premultiply: alpha = fg_alpha * coverage, RGB = fg_RGB * coverage
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
            Self::X11Fallback { .. } => {
                // X11 fonts use immediate-mode rendering (ImageText8)
                // Cannot pre-render to bitmap - return empty
                // Actual rendering happens in thumbnail.rs overlay method
                Ok(RenderedText {
                    width: 0,
                    height: 0,
                    data: Vec::new(),
                })
            }
        }
    }
}
