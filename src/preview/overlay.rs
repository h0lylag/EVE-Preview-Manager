//! Overlay management for thumbnails (text and borders)

use anyhow::{Context, Result};
use tracing::error;
use x11rb::connection::Connection;
use x11rb::protocol::render::{ConnectionExt as RenderExt, CreatePictureAux, PictOp, Picture};
use x11rb::protocol::xproto::{
    Char2b, ConnectionExt as XprotoExt, CreateGCAux, Gcontext, ImageFormat, Pixmap,
};
use x11rb::rust_connection::RustConnection;

use crate::config::DisplayConfig;
use crate::constants::x11;
use crate::types::Dimensions;

use super::font::FontRenderer;

#[derive(Debug)]
/// Handles text and border overlay rendering for thumbnails.
///
/// This struct manages:
/// - The overlay pixmap where text and borders are drawn.
/// - The graphics context (GC) for X11 text rendering.
/// - Integration with `FontRenderer` for text glyph generation.
pub struct OverlayRenderer<'a> {
    // === X11 Resources (private, owned) ===
    /// Backing pixmap for the overlay layer.
    pub overlay_pixmap: Pixmap,
    /// X Render Picture wrapping the overlay pixmap.
    pub overlay_picture: Picture,
    overlay_gc: Gcontext, // Graphics context for text rendering
    border_fill: Picture, // Solid color fill for border

    // === Borrowed Dependencies ===
    conn: &'a RustConnection,
    config: &'a DisplayConfig,
    formats: &'a crate::x11::CachedFormats,
    font_renderer: &'a FontRenderer,
}

impl<'a> OverlayRenderer<'a> {
    /// Creates a new `OverlayRenderer`.
    ///
    /// # Arguments
    /// * `conn` - X11 connection.
    /// * `config` - Display configuration (colors, sizes).
    /// * `formats` - X11 Render formats.
    /// * `font_renderer` - System for rendering text glyphs.
    /// * `root` - Root window ID (for pixmap creation).
    /// * `dimensions` - Initial size of the overlay.
    /// * `character_name` - Debug name for error logging.
    pub fn new(
        conn: &'a RustConnection,
        config: &'a DisplayConfig,
        formats: &'a crate::x11::CachedFormats,
        font_renderer: &'a FontRenderer,
        root: u32,
        dimensions: Dimensions,
        character_name: &str,
    ) -> Result<Self> {
        // ... (implementation of new)
        // Create overlay pixmap
        let overlay_pixmap = conn
            .generate_id()
            .context("Failed to generate ID for overlay pixmap")?;
        conn.create_pixmap(
            x11::ARGB_DEPTH,
            overlay_pixmap,
            root,
            dimensions.width,
            dimensions.height,
        )
        .context(format!(
            "Failed to create overlay pixmap for '{}'",
            character_name
        ))?;

        // Create overlay picture
        let overlay_picture = conn
            .generate_id()
            .context("Failed to generate ID for overlay picture")?;
        conn.render_create_picture(
            overlay_picture,
            overlay_pixmap,
            formats.argb,
            &CreatePictureAux::new(),
        )
        .context(format!(
            "Failed to create overlay picture for '{}'",
            character_name
        ))?;

        // Create overlay GC
        let overlay_gc = conn
            .generate_id()
            .context("Failed to generate ID for overlay graphics context")?;
        conn.create_gc(
            overlay_gc,
            overlay_pixmap,
            &CreateGCAux::new().foreground(config.text_color),
        )
        .context(format!(
            "Failed to create graphics context for '{}'",
            character_name
        ))?;

        // Create border fill
        let border_fill = conn
            .generate_id()
            .context("Failed to generate ID for border fill picture")?;
        conn.render_create_solid_fill(border_fill, config.border_color)
            .context(format!(
                "Failed to create border fill for '{}'",
                character_name
            ))?;

        let renderer = Self {
            overlay_pixmap,
            overlay_picture,
            overlay_gc,
            border_fill,
            conn,
            config,
            formats,
            font_renderer,
        };

        // Render initial name
        renderer
            .update_name(character_name, dimensions)
            .context(format!(
                "Failed to render initial name for '{}'",
                character_name
            ))?;

        Ok(renderer)
    }

    /// Resizes the overlay resources.
    ///
    /// This destroys the old pixmap/picture and creates new ones with the given dimensions.
    pub fn resize(&mut self, root: u32, width: u16, height: u16) -> Result<()> {
        // Free old resources
        self.cleanup_overlay_resources();

        // Recreate resources with new dimensions
        let overlay_pixmap = self.conn.generate_id()?;
        self.conn
            .create_pixmap(x11::ARGB_DEPTH, overlay_pixmap, root, width, height)?;
        self.overlay_pixmap = overlay_pixmap;

        let overlay_picture = self.conn.generate_id()?;
        self.conn.render_create_picture(
            overlay_picture,
            overlay_pixmap,
            self.formats.argb,
            &CreatePictureAux::new(),
        )?;
        self.overlay_picture = overlay_picture;

        Ok(())
    }

    /// Renders the character name onto the overlay.
    ///
    /// Handles both direct X11 text rendering (if core fonts are used) and
    /// client-side rendering (if TrueType fonts are used via `fontdue`).
    pub fn update_name(&self, character_name: &str, dimensions: Dimensions) -> Result<()> {
        // ... (rest of implementation) ...
        // Clear the overlay area (inside border)
        self.conn
            .render_composite(
                PictOp::CLEAR,
                self.overlay_picture,
                0u32,
                self.overlay_picture,
                0,
                0,
                0,
                0,
                self.config.border_size as i16,
                self.config.border_size as i16,
                dimensions.width - self.config.border_size * 2,
                dimensions.height - self.config.border_size * 2,
            )
            .context(format!(
                "Failed to clear overlay area for '{}'",
                character_name
            ))?;

        // Render text based on font renderer type
        if self.font_renderer.requires_direct_rendering() {
            // X11 fallback: direct rendering using ImageText8
            if let Some(font_id) = self.font_renderer.x11_font_id() {
                // Create GC with font
                let gc = self
                    .conn
                    .generate_id()
                    .context("Failed to generate GC ID for X11 text")?;

                // Convert ARGB color to X11 pixel value (strip alpha)
                let fg_pixel = self.config.text_color & 0x00FFFFFF;

                self.conn
                    .create_gc(
                        gc,
                        self.overlay_pixmap,
                        &CreateGCAux::new().font(font_id).foreground(fg_pixel),
                    )
                    .context(format!(
                        "Failed to create GC for X11 text rendering for '{}'",
                        character_name
                    ))?;

                // ImageText8 renders directly to drawable
                self.conn
                    .image_text8(
                        self.overlay_pixmap,
                        gc,
                        self.config.text_offset.x,
                        self.config.text_offset.y + self.font_renderer.size() as i16, // Baseline adjustment
                        character_name.as_bytes(),
                    )
                    .context(format!(
                        "Failed to render X11 text for '{}'",
                        character_name
                    ))?;

                self.conn.free_gc(gc).context("Failed to free text GC")?;
            }
        } else {
            // Fontdue: pre-rendered bitmap
            let rendered = self
                .font_renderer
                .render_text(character_name, self.config.text_color)
                .context(format!(
                    "Failed to render text '{}' with font renderer",
                    character_name
                ))?;

            if rendered.width > 0 && rendered.height > 0 {
                // Upload rendered text bitmap to X11
                // rendered.data is already in BGRA format (Little Endian ARGB)
                let text_pixmap = self
                    .conn
                    .generate_id()
                    .context("Failed to generate ID for text pixmap")?;
                self.conn
                    .create_pixmap(
                        x11::ARGB_DEPTH,
                        text_pixmap,
                        self.overlay_pixmap,
                        rendered.width as u16,
                        rendered.height as u16,
                    )
                    .context(format!(
                        "Failed to create text pixmap for '{}'",
                        character_name
                    ))?;

                self.conn
                    .put_image(
                        ImageFormat::Z_PIXMAP,
                        text_pixmap,
                        self.overlay_gc,
                        rendered.width as u16,
                        rendered.height as u16,
                        0,
                        0,
                        0,
                        x11::ARGB_DEPTH,
                        &rendered.data,
                    )
                    .context(format!(
                        "Failed to upload text image for '{}'",
                        character_name
                    ))?;

                // Create picture for the text pixmap
                let text_picture = self
                    .conn
                    .generate_id()
                    .context("Failed to generate ID for text picture")?;
                self.conn
                    .render_create_picture(
                        text_picture,
                        text_pixmap,
                        self.formats.argb,
                        &CreatePictureAux::new(),
                    )
                    .context(format!(
                        "Failed to create text picture for '{}'",
                        character_name
                    ))?;

                // Composite text onto overlay
                self.conn
                    .render_composite(
                        PictOp::OVER,
                        text_picture,
                        0u32,
                        self.overlay_picture,
                        0,
                        0,
                        0,
                        0,
                        self.config.text_offset.x,
                        self.config.text_offset.y,
                        rendered.width as u16,
                        rendered.height as u16,
                    )
                    .context(format!(
                        "Failed to composite text onto overlay for '{}'",
                        character_name
                    ))?;

                // Cleanup
                self.conn
                    .render_free_picture(text_picture)
                    .context("Failed to free text picture")?;
                self.conn
                    .free_pixmap(text_pixmap)
                    .context("Failed to free text pixmap")?;
            }
        }

        Ok(())
    }

    /// Draws (or clears) the border around the thumbnail overlay.
    pub fn draw_border(
        &self,
        character_name: &str,
        dimensions: Dimensions,
        focused: bool,
    ) -> Result<()> {
        if focused {
            // Only render border fill if we actually have a border size
            if self.config.border_size > 0 {
                self.conn
                    .render_composite(
                        PictOp::SRC,
                        self.border_fill,
                        0u32,
                        self.overlay_picture,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        dimensions.width,
                        dimensions.height,
                    )
                    .context(format!("Failed to render border for '{}'", character_name))?;
            }
        } else {
            self.conn
                .render_composite(
                    PictOp::CLEAR,
                    self.overlay_picture,
                    0u32,
                    self.overlay_picture,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    dimensions.width,
                    dimensions.height,
                )
                .context(format!("Failed to clear border for '{}'", character_name))?;
        }
        self.update_name(character_name, dimensions)
            .context(format!(
                "Failed to update name overlay after border change for '{}'",
                character_name
            ))?;
        Ok(())
    }

    /// Draws the "MINIMIZED" state overlay.
    pub fn draw_minimized(&self, character_name: &str, dimensions: Dimensions) -> Result<()> {
        self.draw_border(character_name, dimensions, false)
            .context(format!(
                "Failed to clear border for minimized window '{}'",
                character_name
            ))?;
        let extents = self
            .conn
            .query_text_extents(
                self.overlay_gc,
                b"MINIMIZED"
                    .iter()
                    .map(|&c| Char2b { byte1: 0, byte2: c })
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
            .context("Failed to send text extents query for MINIMIZED text")?
            .reply()
            .context("Failed to get text extents for MINIMIZED text")?;
        self.conn
            .image_text8(
                self.overlay_pixmap,
                self.overlay_gc,
                (dimensions.width as i16 - extents.overall_width as i16) / 2,
                (dimensions.height as i16 + extents.font_ascent + extents.font_descent) / 2,
                b"MINIMIZED",
            )
            .context(format!(
                "Failed to render MINIMIZED text for '{}'",
                character_name
            ))?;
        Ok(())
    }

    fn cleanup_overlay_resources(&self) {
        if let Err(e) = self.conn.free_pixmap(self.overlay_pixmap) {
            error!(pixmap = self.overlay_pixmap, error = %e, "Failed to free overlay pixmap");
        }

        if let Err(e) = self.conn.render_free_picture(self.overlay_picture) {
            error!(picture = self.overlay_picture, error = %e, "Failed to free overlay picture");
        }
    }
}

impl Drop for OverlayRenderer<'_> {
    fn drop(&mut self) {
        self.cleanup_overlay_resources();

        if let Err(e) = self.conn.free_gc(self.overlay_gc) {
            error!(gc = self.overlay_gc, error = %e, "Failed to free GC");
        }

        if let Err(e) = self.conn.render_free_picture(self.border_fill) {
            error!(picture = self.border_fill, error = %e, "Failed to free border fill picture");
        }
    }
}
