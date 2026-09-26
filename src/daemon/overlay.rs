//! Overlay management for thumbnails (text and borders)

use anyhow::{Context, Result};
use tracing::error;
use x11rb::connection::Connection;
use x11rb::protocol::render::{ConnectionExt as RenderExt, CreatePictureAux, PictOp, Picture};
use x11rb::protocol::xproto::{
    Char2b, ConnectionExt as XprotoExt, CreateGCAux, Gcontext, ImageFormat, Pixmap,
};
use x11rb::rust_connection::RustConnection;

use crate::common::constants::x11;
use crate::common::types::{Dimensions, SourceKind, TextOffset};
use crate::config::DisplayConfig;

use super::font::FontRenderer;

#[derive(Clone, Copy)]
pub struct OverlayIdentity<'a> {
    pub kind: SourceKind,
    pub style: &'a str,
    pub display: &'a str,
}

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
    overlay_gc: Gcontext,           // Graphics context for text rendering
    active_border_fill: Picture,    // Solid color fill for active border
    inactive_border_fill: Picture,  // Solid color fill for inactive border
    skipped_indicator_gc: Gcontext, // GC for drawing skipped indicator (Red X)

    // === Borrowed Dependencies ===
    conn: &'a RustConnection,
    formats: &'a crate::x11::CachedFormats,
}

impl<'a> OverlayRenderer<'a> {
    /// Creates a new `OverlayRenderer`.
    ///
    /// # Arguments
    /// * `conn` - X11 connection.
    /// * `config` - Display configuration (colors, sizes).
    /// * `formats` - X11 Render formats.
    /// * `font_renderer` - System for rendering text glyphs (used for initial render).
    /// * `root` - Root window ID (for pixmap creation).
    /// * `dimensions` - Initial size of the overlay.
    /// * `identity` - Effective styling key and label text for the thumbnail.
    pub fn new<'b>(
        conn: &'a RustConnection,
        config: &'b DisplayConfig,
        formats: &'a crate::x11::CachedFormats,
        font_renderer: &FontRenderer,
        root: u32,
        dimensions: Dimensions,
        identity: OverlayIdentity<'_>,
    ) -> Result<Self> {
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
            identity.style
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
            identity.style
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
            identity.style
        ))?;

        // Create skipped indicator GC (Red)
        let skipped_indicator_gc = conn
            .generate_id()
            .context("Failed to generate ID for skipped indicator GC")?;
        conn.create_gc(
            skipped_indicator_gc,
            overlay_pixmap,
            &CreateGCAux::new()
                .foreground(0xFFFF0000) // Opaque Red
                .line_width(3), // Thicker lines for visibility
        )
        .context(format!(
            "Failed to create skipped indicator GC for '{}'",
            identity.style
        ))?;

        // Create active border fill
        let active_border_fill = conn
            .generate_id()
            .context("Failed to generate ID for active border fill picture")?;
        conn.render_create_solid_fill(active_border_fill, config.active_border_color)
            .context(format!(
                "Failed to create active border fill for '{}'",
                identity.style
            ))?;

        // Create inactive border fill
        let inactive_border_fill = conn
            .generate_id()
            .context("Failed to generate ID for inactive border fill picture")?;
        conn.render_create_solid_fill(inactive_border_fill, config.inactive_border_color)
            .context(format!(
                "Failed to create inactive border fill for '{}'",
                identity.style
            ))?;

        let renderer = Self {
            overlay_pixmap,
            overlay_picture,
            overlay_gc,
            active_border_fill,
            inactive_border_fill,
            skipped_indicator_gc,
            conn,
            formats,
        };

        // Render initial name
        let initial_border_size = renderer.calculate_border_size(config, identity, false);
        renderer
            .clear_content_area(dimensions, initial_border_size)
            .context(format!(
                "Failed to clear content area for initial render of '{}'",
                identity.style
            ))?;

        renderer
            .update_name(
                config,
                identity,
                dimensions,
                initial_border_size,
                font_renderer,
            )
            .context(format!(
                "Failed to render initial name for '{}'",
                identity.style
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

    /// Draws the skipped indicator (diagonal red lines)
    pub fn draw_skipped_indicator(&self, dimensions: Dimensions) -> Result<()> {
        let w = dimensions.width as i16;
        let h = dimensions.height as i16;

        let segments = [
            x11rb::protocol::xproto::Segment {
                x1: 0,
                y1: 0,
                x2: w,
                y2: h,
            },
            x11rb::protocol::xproto::Segment {
                x1: w,
                y1: 0,
                x2: 0,
                y2: h,
            },
        ];

        self.conn
            .poly_segment(self.overlay_pixmap, self.skipped_indicator_gc, &segments)
            .context("Failed to draw skipped indicator segments")?;

        Ok(())
    }

    /// Calculates the effective border size implementation
    pub fn calculate_border_size(
        &self,
        config: &DisplayConfig,
        identity: OverlayIdentity<'_>,
        focused: bool,
    ) -> u16 {
        if let Some(settings) = config.settings_for(identity.kind, identity.style) {
            if focused {
                settings
                    .override_active_border_size
                    .unwrap_or(config.active_border_size)
            } else {
                settings
                    .override_inactive_border_size
                    .unwrap_or(config.inactive_border_size)
            }
        } else if focused {
            config.active_border_size
        } else {
            config.inactive_border_size
        }
    }

    /// Clears the center content area (inside the border).
    pub fn clear_content_area(&self, dimensions: Dimensions, border_size: u16) -> Result<()> {
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
                border_size as i16,
                border_size as i16,
                dimensions.width.saturating_sub(border_size * 2),
                dimensions.height.saturating_sub(border_size * 2),
            )
            .context("Failed to clear content area")?;
        Ok(())
    }

    /// Renders the character name onto the overlay.
    ///
    /// Handles both server-side X11 text rendering (if core fonts are used) and
    /// client-side rendering (if TrueType fonts are used via `fontdue`).
    /// The caller must clear the previous label first. Text is composited over any
    /// remaining overlay content, including the skip indicator.
    pub fn update_name(
        &self,
        config: &DisplayConfig,
        identity: OverlayIdentity<'_>,
        dimensions: Dimensions,
        _border_size: u16,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        // Resolve settings overrides
        let (display_name, text_color) = if identity.display.is_empty() {
            ("", config.text_color)
        } else if let Some(settings) = config.settings_for(identity.kind, identity.style) {
            let display_name = settings.alias.as_deref().unwrap_or(identity.display);
            let text_color = settings
                .override_text_color
                .as_deref()
                .and_then(crate::common::color::HexColor::parse)
                .map(|c| c.argb32())
                .unwrap_or(config.text_color);
            (display_name, text_color)
        } else {
            (identity.display, config.text_color)
        };

        if display_name.is_empty() || text_color >> 24 == 0 {
            return Ok(());
        }

        if font_renderer.requires_direct_rendering() {
            if let Some(font_id) = font_renderer.x11_font_id() {
                // ImageText8 copies its background rectangle as well as glyph pixels.
                // Draw onto a separate transparent layer so it cannot erase the skip indicator.
                self.composite_text_layer(
                    dimensions,
                    TextOffset::from_border_edge(0, 0),
                    |pixmap, picture| {
                        self.conn
                            .render_composite(
                                PictOp::CLEAR,
                                picture,
                                0u32,
                                picture,
                                0,
                                0,
                                0,
                                0,
                                0,
                                0,
                                dimensions.width,
                                dimensions.height,
                            )
                            .context("Failed to clear X11 text layer")?;

                        let gc = self
                            .conn
                            .generate_id()
                            .context("Failed to generate X11 text GC ID")?;
                        let pixel = crate::common::color::HexColor::from_argb32(text_color)
                            .to_premultiplied_argb32();
                        self.conn
                            .create_gc(
                                gc,
                                pixmap,
                                &CreateGCAux::new()
                                    .font(font_id)
                                    .foreground(pixel)
                                    .background(0),
                            )
                            .context("Failed to create X11 text GC")?;

                        let draw = self
                            .conn
                            .image_text8(
                                pixmap,
                                gc,
                                config.text_offset.x,
                                config.text_offset.y + font_renderer.size() as i16,
                                display_name.as_bytes(),
                            )
                            .context("Failed to draw X11 text");
                        // Release the GC even if text serialization or drawing failed.
                        let cleanup = self.conn.free_gc(gc);
                        draw?;
                        cleanup.context("Failed to free X11 text GC")?;
                        Ok(())
                    },
                )?;
            }
        } else {
            let rendered = font_renderer
                .render_text(display_name, text_color)
                .context("Failed to rasterize text")?;
            if rendered.width > 0 && rendered.height > 0 {
                self.composite_text_layer(
                    Dimensions::new(rendered.width as u16, rendered.height as u16),
                    config.text_offset,
                    |pixmap, _| {
                        // Fontdue supplies premultiplied BGRA pixels for the whole layer.
                        self.conn
                            .put_image(
                                ImageFormat::Z_PIXMAP,
                                pixmap,
                                self.overlay_gc,
                                rendered.width as u16,
                                rendered.height as u16,
                                0,
                                0,
                                0,
                                x11::ARGB_DEPTH,
                                &rendered.data,
                            )
                            .context("Failed to upload text bitmap")?;
                        Ok(())
                    },
                )?;
            }
        }
        Ok(())
    }

    /// Paint a temporary ARGB layer and composite it over the existing overlay.
    /// Release both X11 resources on success and on failed painting/compositing.
    fn composite_text_layer(
        &self,
        dimensions: Dimensions,
        offset: TextOffset,
        paint: impl FnOnce(Pixmap, Picture) -> Result<()>,
    ) -> Result<()> {
        let pixmap = self
            .conn
            .generate_id()
            .context("Failed to generate text pixmap ID")?;
        let picture = self
            .conn
            .generate_id()
            .context("Failed to generate text picture ID")?;
        self.conn
            .create_pixmap(
                x11::ARGB_DEPTH,
                pixmap,
                self.overlay_pixmap,
                dimensions.width,
                dimensions.height,
            )
            .context("Failed to create text pixmap")?;

        let result: Result<()> = (|| {
            self.conn
                .render_create_picture(picture, pixmap, self.formats.argb, &CreatePictureAux::new())
                .context("Failed to create text picture")?;
            let render: Result<()> = (|| {
                paint(pixmap, picture)?;
                self.conn
                    .render_composite(
                        PictOp::OVER,
                        picture,
                        0u32,
                        self.overlay_picture,
                        0,
                        0,
                        0,
                        0,
                        offset.x,
                        offset.y,
                        dimensions.width,
                        dimensions.height,
                    )
                    .context("Failed to composite text layer")?;
                Ok(())
            })();
            let cleanup = self.conn.render_free_picture(picture);
            render?;
            cleanup.context("Failed to free text picture")?;
            Ok(())
        })();
        let cleanup = self.conn.free_pixmap(pixmap);
        result?;
        cleanup.context("Failed to free text pixmap")?;
        Ok(())
    }

    /// Draws the overlay content with strict Z-order:
    /// 1. Skipped Indicator (Red X) - Bottom
    /// 2. Text (Name) - Middle
    /// 3. Border - Top (covers everything at edges)
    pub fn draw_border(
        &self,
        config: &DisplayConfig,
        identity: OverlayIdentity<'_>,
        dimensions: Dimensions,
        focused: bool,
        skipped: bool,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        // 1. Clear the entire overlay first (transparent background)
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
            .context("Failed to clear overlay")?;

        // 2. Draw skipped indicator (Red X)
        // Drawn first so text appears on top of it
        if skipped {
            self.draw_skipped_indicator(dimensions)?;
        }

        // Determine effective border size and color source
        let effective_size = self.calculate_border_size(config, identity, focused);

        // 3. Draw Text
        // We pass effective_size mainly if text positioning depended on it,
        // but currently text is positioned by config offset.
        self.update_name(config, identity, dimensions, effective_size, font_renderer)
            .context(format!(
                "Failed to update name overlay for '{}'",
                identity.style
            ))?;

        // 4. Draw Border (Top Layer)
        // Only if size > 0 and enabled
        let should_draw_border = if focused {
            effective_size > 0
        } else {
            config.inactive_border_enabled && effective_size > 0
        };

        if should_draw_border {
            let (fill_picture, temp_fill_id) =
                if let Some(settings) = config.settings_for(identity.kind, identity.style) {
                    let override_color_hex = if focused {
                        settings.override_active_border_color.as_ref()
                    } else {
                        settings.override_inactive_border_color.as_ref()
                    };

                    if let Some(hex) = override_color_hex {
                        if let Some(color) =
                            crate::common::color::HexColor::parse(hex).map(|c| c.to_x11_color())
                        {
                            let pid = self.conn.generate_id()?;
                            self.conn.render_create_solid_fill(pid, color)?;
                            (pid, Some(pid))
                        } else if focused {
                            (self.active_border_fill, None)
                        } else {
                            (self.inactive_border_fill, None)
                        }
                    } else if focused {
                        (self.active_border_fill, None)
                    } else {
                        (self.inactive_border_fill, None)
                    }
                } else if focused {
                    (self.active_border_fill, None)
                } else {
                    (self.inactive_border_fill, None)
                };

            // Draw 4 strips for the border
            let w = dimensions.width as i16;
            let h = dimensions.height as i16;
            let b = effective_size as i16;

            // Top
            self.conn.render_composite(
                PictOp::SRC,
                fill_picture,
                0u32,
                self.overlay_picture,
                0,
                0,
                0,
                0,
                0,
                0,
                dimensions.width,
                effective_size,
            )?;
            // Bottom
            self.conn.render_composite(
                PictOp::SRC,
                fill_picture,
                0u32,
                self.overlay_picture,
                0,
                0,
                0,
                0,
                0,
                h - b,
                dimensions.width,
                effective_size,
            )?;
            // Left
            self.conn.render_composite(
                PictOp::SRC,
                fill_picture,
                0u32,
                self.overlay_picture,
                0,
                0,
                0,
                0,
                0,
                b,
                effective_size,
                (h - 2 * b).max(0) as u16,
            )?;
            // Right
            self.conn.render_composite(
                PictOp::SRC,
                fill_picture,
                0u32,
                self.overlay_picture,
                0,
                0,
                0,
                0,
                w - b,
                b,
                effective_size,
                (h - 2 * b).max(0) as u16,
            )?;

            // Clean up temp fill
            if let Some(pid) = temp_fill_id {
                self.conn.render_free_picture(pid)?;
            }
        }

        Ok(())
    }

    /// Draws the "MINIMIZED" state overlay.
    pub fn draw_minimized(
        &self,
        config: &DisplayConfig,
        identity: OverlayIdentity<'_>,
        dimensions: Dimensions,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        self.draw_border(config, identity, dimensions, false, false, font_renderer)
            .context(format!(
                "Failed to clear border for minimized window '{}'",
                identity.style
            ))?;

        if !config.minimized_overlay_enabled {
            return Ok(());
        }

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
                identity.style
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

        if let Err(e) = self.conn.free_gc(self.skipped_indicator_gc) {
            error!(gc = self.skipped_indicator_gc, error = %e, "Failed to free skipped indicator GC");
        }

        if let Err(e) = self.conn.render_free_picture(self.active_border_fill) {
            error!(picture = self.active_border_fill, error = %e, "Failed to free active border fill picture");
        }

        if let Err(e) = self.conn.render_free_picture(self.inactive_border_fill) {
            error!(
                picture = self.inactive_border_fill,
                error = %e,
                "Failed to free inactive border fill picture"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DaemonConfig, profile::Profile};

    #[test]
    #[ignore = "requires isolated Xvfb and EPM_X11_TESTS=1"]
    fn text_layer_releases_resources_after_success_and_paint_failure() {
        assert_eq!(std::env::var("EPM_X11_TESTS").as_deref(), Ok("1"));
        let (conn, screen_number) = x11rb::connect(None).unwrap();
        let screen = &conn.setup().roots[screen_number];
        let formats = crate::x11::CachedFormats::new(&conn, screen).unwrap();
        let config = DaemonConfig {
            profile: Profile::default(),
            character_thumbnails: Default::default(),
            custom_source_thumbnails: Default::default(),
            profile_hotkeys: Default::default(),
            runtime_hidden: false,
        }
        .build_display_config();
        let font_id = conn.generate_id().unwrap();
        conn.open_font(font_id, b"fixed").unwrap().check().unwrap();
        let font = FontRenderer::X11Fallback {
            font_id,
            size: 12.0,
        };
        let overlay = OverlayRenderer::new(
            &conn,
            &config,
            &formats,
            &font,
            screen.root,
            Dimensions::new(160, 100),
            OverlayIdentity {
                kind: SourceKind::Eve,
                style: "",
                display: "",
            },
        )
        .unwrap();

        for fail in [false, true] {
            let mut allocated = (0, 0);
            let result = overlay.composite_text_layer(
                Dimensions::new(8, 8),
                TextOffset::from_border_edge(0, 0),
                |pixmap, picture| {
                    allocated = (pixmap, picture);
                    conn.render_composite(
                        PictOp::CLEAR,
                        picture,
                        0u32,
                        picture,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        8,
                        8,
                    )?
                    .check()?;
                    anyhow::ensure!(!fail, "injected paint failure");
                    Ok(())
                },
            );
            if fail {
                assert_eq!(result.unwrap_err().to_string(), "injected paint failure");
            } else {
                result.unwrap();
            }
            // Server replies prove both IDs were freed, rather than merely queued for cleanup.
            assert!(conn.get_geometry(allocated.0).unwrap().reply().is_err());
            assert!(
                conn.render_free_picture(allocated.1)
                    .unwrap()
                    .check()
                    .is_err()
            );
        }
        conn.close_font(font_id).unwrap().check().unwrap();
    }
}
