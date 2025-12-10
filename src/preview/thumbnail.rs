//! Thumbnail window management
//!
//! Creates and manages X11 overlay windows that display scaled previews of EVE clients.
//! Handles rendering via X11 RENDER extension, drag interactions, and border highlighting.

use anyhow::{Context, Result};
use tracing::{error, info};
use x11rb::connection::Connection;
use x11rb::protocol::damage::{
    ConnectionExt as DamageExt, Damage, ReportLevel as DamageReportLevel,
};
use x11rb::protocol::render::{
    ConnectionExt as RenderExt, CreatePictureAux, PictOp, Picture, Transform,
};
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as WrapperExt;

use crate::config::DisplayConfig;
use crate::constants::{positioning, x11};
use crate::types::{Dimensions, Position, ThumbnailState};
use crate::x11::{AppContext, to_fixed};

use super::font::FontRenderer;
use super::snapping::Rect;

#[derive(Debug, Default)]
pub struct InputState {
    pub dragging: bool,
    pub drag_start: Position,
    pub win_start: Position,
    pub snap_targets: Vec<Rect>, // Cached snap targets computed when drag starts
}

#[derive(Debug)]
pub struct Thumbnail<'a> {
    // === Application State (public, frequently accessed) ===
    pub character_name: String,
    pub state: ThumbnailState,
    pub input_state: InputState,

    // === Geometry (public, immutable after creation) ===
    pub dimensions: Dimensions,
    pub current_position: Position, // Cached position for hit testing

    // === X11 Window Handles (private/public owned resources) ===
    pub window: Window, // Our thumbnail window (public for event handling)
    pub src: Window,    // Source EVE window (public for event handling)
    pub damage: Damage, // DAMAGE extension handle (public for event matching)
    root: Window,       // Root window (private, cached from screen)

    // === X11 Render Resources (private, owned resources) ===
    border_fill: Picture,     // Solid color fill for border
    src_picture: Picture,     // Picture wrapping source window
    dst_picture: Picture,     // Picture wrapping our thumbnail window
    overlay_gc: Gcontext,     // Graphics context for text rendering
    overlay_pixmap: Pixmap,   // Backing pixmap for overlay compositing
    overlay_picture: Picture, // Picture wrapping overlay pixmap

    // === Borrowed Dependencies (private, references to app context) ===
    conn: &'a RustConnection,
    config: &'a DisplayConfig,
    formats: &'a crate::x11::CachedFormats,
    font_renderer: &'a FontRenderer,
    atoms: &'a crate::x11::CachedAtoms,
}

impl<'a> Thumbnail<'a> {
    /// Create and configure the X11 window
    fn create_window(
        ctx: &AppContext,
        character_name: &str,
        x: i16,
        y: i16,
        dimensions: Dimensions,
    ) -> Result<Window> {
        let window = ctx
            .conn
            .generate_id()
            .context("Failed to generate X11 window ID")?;
        ctx.conn
            .create_window(
                ctx.screen.root_depth,
                window,
                ctx.screen.root,
                x,
                y,
                dimensions.width,
                dimensions.height,
                0,
                WindowClass::INPUT_OUTPUT,
                ctx.screen.root_visual,
                &CreateWindowAux::new()
                    .override_redirect(x11::OVERRIDE_REDIRECT)
                    .event_mask(
                        EventMask::SUBSTRUCTURE_NOTIFY
                            | EventMask::BUTTON_PRESS
                            | EventMask::BUTTON_RELEASE
                            | EventMask::POINTER_MOTION,
                    ),
            )
            .context(format!(
                "Failed to create thumbnail window for '{}'",
                character_name
            ))?;

        Ok(window)
    }

    /// Setup window properties (opacity, WM_CLASS, always-on-top, PID)
    fn setup_window_properties(
        ctx: &AppContext,
        window: Window,
        character_name: &str,
    ) -> Result<()> {
        // Set PID so we can identify our own thumbnail windows
        let pid = std::process::id();
        ctx.conn
            .change_property32(
                PropMode::REPLACE,
                window,
                ctx.atoms.net_wm_pid,
                AtomEnum::CARDINAL,
                &[pid],
            )
            .context(format!(
                "Failed to set _NET_WM_PID for '{}'",
                character_name
            ))?;

        // Set opacity
        ctx.conn
            .change_property32(
                PropMode::REPLACE,
                window,
                ctx.atoms.net_wm_window_opacity,
                AtomEnum::CARDINAL,
                &[ctx.config.opacity],
            )
            .context(format!(
                "Failed to set window opacity for '{}'",
                character_name
            ))?;

        // Set WM_CLASS
        ctx.conn
            .change_property8(
                PropMode::REPLACE,
                window,
                ctx.atoms.wm_class,
                AtomEnum::STRING,
                b"eve-preview-manager\0eve-preview-manager\0",
            )
            .context(format!("Failed to set WM_CLASS for '{}'", character_name))?;

        // Set always-on-top
        ctx.conn
            .change_property32(
                PropMode::REPLACE,
                window,
                ctx.atoms.net_wm_state,
                AtomEnum::ATOM,
                &[ctx.atoms.net_wm_state_above],
            )
            .context(format!(
                "Failed to set window always-on-top for '{}'",
                character_name
            ))?;

        // Map window to make it visible
        ctx.conn
            .map_window(window)
            .inspect_err(|e| error!(window = window, error = ?e, "Failed to map thumbnail window"))
            .context(format!(
                "Failed to map thumbnail window for '{}'",
                character_name
            ))?;
        info!(
            window = window,
            character = %character_name,
            "Mapped thumbnail window"
        );

        Ok(())
    }

    /// Create render pictures and resources
    fn create_render_resources(
        ctx: &AppContext,
        window: Window,
        src: Window,
        dimensions: Dimensions,
        character_name: &str,
    ) -> Result<(Picture, Picture, Picture, Pixmap, Picture, Gcontext)> {
        // Border fill
        let border_fill = ctx
            .conn
            .generate_id()
            .context("Failed to generate ID for border fill picture")?;
        ctx.conn
            .render_create_solid_fill(border_fill, ctx.config.border_color)
            .context(format!(
                "Failed to create border fill for '{}'",
                character_name
            ))?;

        // Source and destination pictures
        let src_picture = ctx
            .conn
            .generate_id()
            .context("Failed to generate ID for source picture")?;
        let dst_picture = ctx
            .conn
            .generate_id()
            .context("Failed to generate ID for destination picture")?;
        ctx.conn
            .render_create_picture(src_picture, src, ctx.formats.rgb, &CreatePictureAux::new())
            .context(format!(
                "Failed to create source picture for '{}'",
                character_name
            ))?;
        ctx.conn
            .render_create_picture(
                dst_picture,
                window,
                ctx.formats.rgb,
                &CreatePictureAux::new(),
            )
            .context(format!(
                "Failed to create destination picture for '{}'",
                character_name
            ))?;

        // Overlay resources
        let overlay_pixmap = ctx
            .conn
            .generate_id()
            .context("Failed to generate ID for overlay pixmap")?;
        let overlay_picture = ctx
            .conn
            .generate_id()
            .context("Failed to generate ID for overlay picture")?;
        ctx.conn
            .create_pixmap(
                x11::ARGB_DEPTH,
                overlay_pixmap,
                ctx.screen.root,
                dimensions.width,
                dimensions.height,
            )
            .context(format!(
                "Failed to create overlay pixmap for '{}'",
                character_name
            ))?;
        ctx.conn
            .render_create_picture(
                overlay_picture,
                overlay_pixmap,
                ctx.formats.argb,
                &CreatePictureAux::new(),
            )
            .context(format!(
                "Failed to create overlay picture for '{}'",
                character_name
            ))?;

        let overlay_gc = ctx
            .conn
            .generate_id()
            .context("Failed to generate ID for overlay graphics context")?;
        ctx.conn
            .create_gc(
                overlay_gc,
                overlay_pixmap,
                &CreateGCAux::new().foreground(ctx.config.text_color),
            )
            .context(format!(
                "Failed to create graphics context for '{}'",
                character_name
            ))?;

        Ok((
            border_fill,
            src_picture,
            dst_picture,
            overlay_pixmap,
            overlay_picture,
            overlay_gc,
        ))
    }

    /// Create damage tracking for source window
    fn create_damage_tracking(
        ctx: &AppContext,
        src: Window,
        character_name: &str,
    ) -> Result<Damage> {
        let damage = ctx
            .conn
            .generate_id()
            .context("Failed to generate ID for damage tracking")?;
        ctx.conn
            .damage_create(damage, src, DamageReportLevel::RAW_RECTANGLES)
            .context(format!(
                "Failed to create damage tracking for '{}' (check DAMAGE extension)",
                character_name
            ))?;
        Ok(damage)
    }

    pub fn new(
        ctx: &AppContext<'a>,
        character_name: String,
        src: Window,
        font_renderer: &'a FontRenderer,
        position: Option<Position>,
        dimensions: Dimensions,
    ) -> Result<Self> {
        // Validate dimensions are non-zero
        if dimensions.width == 0 || dimensions.height == 0 {
            return Err(anyhow::anyhow!(
                "Invalid thumbnail dimensions for '{}': {}x{} (must be non-zero)",
                character_name,
                dimensions.width,
                dimensions.height
            ));
        }

        // Query source window geometry
        let src_geom = ctx
            .conn
            .get_geometry(src)
            .context("Failed to send geometry query for source EVE window")?
            .reply()
            .context(format!(
                "Failed to get geometry for source window {} (character: '{}')",
                src, character_name
            ))?;

        // Use saved position OR top-left of EVE window with 20px padding
        let Position { x, y } = position.unwrap_or_else(|| {
            Position::new(
                src_geom.x + positioning::DEFAULT_SPAWN_OFFSET,
                src_geom.y + positioning::DEFAULT_SPAWN_OFFSET,
            )
        });
        info!(
            character = %character_name,
            x = x,
            y = y,
            width = dimensions.width,
            height = dimensions.height,
            "Creating thumbnail"
        );

        // Create window and setup properties
        let window = Self::create_window(ctx, &character_name, x, y, dimensions)?;

        // RAII guard to automatically destroy the window if initialization fails partially
        // This ensures we don't leak orphaned windows if we error out before returning the valid Thumbnail struct
        struct WindowGuard<'a> {
            conn: &'a RustConnection,
            window: Window,
            character_name: String,
            should_cleanup: bool,
        }

        impl Drop for WindowGuard<'_> {
            fn drop(&mut self) {
                if self.should_cleanup {
                    if let Err(e) = self.conn.destroy_window(self.window) {
                        error!(
                            window = self.window,
                            character = %self.character_name,
                            error = %e,
                            "Failed to cleanup window after initialization failure"
                        );
                    }
                    // Flush to ensure cleanup is sent to server
                    let _ = self.conn.flush();
                }
            }
        }

        let mut window_guard = WindowGuard {
            conn: ctx.conn,
            window,
            character_name: character_name.clone(),
            should_cleanup: true,
        };

        Self::setup_window_properties(ctx, window, &character_name)?;

        // Create rendering resources
        let (border_fill, src_picture, dst_picture, overlay_pixmap, overlay_picture, overlay_gc) =
            Self::create_render_resources(ctx, window, src, dimensions, &character_name)?;

        // Setup damage tracking
        let damage = Self::create_damage_tracking(ctx, src, &character_name)?;

        let thumbnail = Self {
            // Application State
            character_name,
            state: ThumbnailState::default(), // Start in unfocused normal state
            input_state: InputState::default(),

            // Geometry
            dimensions,
            current_position: Position::new(x, y),

            // X11 Window Handles
            window,
            src,
            damage,
            root: ctx.screen.root,

            // X11 Render Resources
            border_fill,
            src_picture,
            dst_picture,
            overlay_gc,
            overlay_pixmap,
            overlay_picture,

            // Borrowed Dependencies
            conn: ctx.conn,
            config: ctx.config,
            formats: ctx.formats,
            font_renderer,
            atoms: ctx.atoms,
        };

        // Render initial name overlay
        thumbnail.update_name().context(format!(
            "Failed to render initial name overlay for '{}'",
            thumbnail.character_name
        ))?;

        // Success! Disable cleanup guard since Thumbnail's Drop will handle it now
        window_guard.should_cleanup = false;

        Ok(thumbnail)
    }

    pub fn visibility(&mut self, visible: bool) -> Result<()> {
        let currently_visible = self.state.is_visible();
        if visible == currently_visible {
            return Ok(());
        }

        if visible {
            // Restore from Hidden state to Normal (unfocused)
            self.state = ThumbnailState::Normal { focused: false };
            self.conn.map_window(self.window).context(format!(
                "Failed to map window for '{}'",
                self.character_name
            ))?;
        } else {
            // Hide the window
            self.state = ThumbnailState::Hidden;
            self.conn.unmap_window(self.window).context(format!(
                "Failed to unmap window for '{}'",
                self.character_name
            ))?;
        }
        Ok(())
    }

    fn capture(&self) -> Result<()> {
        let geom = self
            .conn
            .get_geometry(self.src)
            .context("Failed to send geometry query for source window")?
            .reply()
            .context(format!(
                "Failed to get geometry for source window (character: '{}')",
                self.character_name
            ))?;
        let transform = Transform {
            matrix11: to_fixed(geom.width as f32 / self.dimensions.width as f32),
            matrix22: to_fixed(geom.height as f32 / self.dimensions.height as f32),
            matrix33: to_fixed(1.0),
            ..Default::default()
        };
        self.conn
            .render_set_picture_transform(self.src_picture, transform)
            .context(format!(
                "Failed to set transform for '{}'",
                self.character_name
            ))?;
        self.conn
            .render_composite(
                PictOp::SRC,
                self.src_picture,
                0u32,
                self.dst_picture,
                0,
                0,
                0,
                0,
                0,
                0,
                self.dimensions.width,
                self.dimensions.height,
            )
            .context(format!(
                "Failed to composite source window for '{}'",
                self.character_name
            ))?;
        Ok(())
    }

    pub fn border(&self, focused: bool) -> Result<()> {
        if focused {
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
                    self.dimensions.width,
                    self.dimensions.height,
                )
                .context(format!(
                    "Failed to render border for '{}'",
                    self.character_name
                ))?;
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
                    self.dimensions.width,
                    self.dimensions.height,
                )
                .context(format!(
                    "Failed to clear border for '{}'",
                    self.character_name
                ))?;
        }
        self.update_name().context(format!(
            "Failed to update name overlay after border change for '{}'",
            self.character_name
        ))?;
        Ok(())
    }

    pub fn minimized(&mut self) -> Result<()> {
        self.state = ThumbnailState::Minimized;
        self.border(false).context(format!(
            "Failed to clear border for minimized window '{}'",
            self.character_name
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
                (self.dimensions.width as i16 - extents.overall_width as i16) / 2,
                (self.dimensions.height as i16 + extents.font_ascent + extents.font_descent) / 2,
                b"MINIMIZED",
            )
            .context(format!(
                "Failed to render MINIMIZED text for '{}'",
                self.character_name
            ))?;
        self.update().context(format!(
            "Failed to update minimized display for '{}'",
            self.character_name
        ))?;

        Ok(())
    }

    pub fn update_name(&self) -> Result<()> {
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
                self.dimensions.width - self.config.border_size * 2,
                self.dimensions.height - self.config.border_size * 2,
            )
            .context(format!(
                "Failed to clear overlay area for '{}'",
                self.character_name
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
                        self.character_name
                    ))?;

                // ImageText8 renders directly to drawable
                self.conn
                    .image_text8(
                        self.overlay_pixmap,
                        gc,
                        self.config.text_offset.x,
                        self.config.text_offset.y + self.font_renderer.size() as i16, // Baseline adjustment
                        self.character_name.as_bytes(),
                    )
                    .context(format!(
                        "Failed to render X11 text for '{}'",
                        self.character_name
                    ))?;

                self.conn.free_gc(gc).context("Failed to free text GC")?;
            }
        } else {
            // Fontdue: pre-rendered bitmap
            let rendered = self
                .font_renderer
                .render_text(&self.character_name, self.config.text_color)
                .context(format!(
                    "Failed to render text '{}' with font renderer",
                    self.character_name
                ))?;

            if rendered.width > 0 && rendered.height > 0 {
                // Upload rendered text bitmap to X11
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
                        self.character_name
                    ))?;

                // Convert Vec<u32> ARGB to bytes in X11 native format (little-endian BGRA)
                let mut image_data = Vec::with_capacity(rendered.data.len() * 4);
                for pixel in &rendered.data {
                    image_data.push(*pixel as u8); // B
                    image_data.push((pixel >> 8) as u8); // G
                    image_data.push((pixel >> 16) as u8); // R
                    image_data.push((pixel >> 24) as u8); // A
                }

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
                        &image_data,
                    )
                    .context(format!(
                        "Failed to upload text image for '{}'",
                        self.character_name
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
                        self.character_name
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
                        self.character_name
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

    fn overlay(&self) -> Result<()> {
        self.conn
            .render_composite(
                PictOp::OVER,
                self.overlay_picture,
                0u32,
                self.dst_picture,
                0,
                0,
                0,
                0,
                0,
                0,
                self.dimensions.width,
                self.dimensions.height,
            )
            .context(format!(
                "Failed to composite overlay onto destination for '{}'",
                self.character_name
            ))?;
        Ok(())
    }

    pub fn update(&self) -> Result<()> {
        self.capture().context(format!(
            "Failed to capture source window for '{}'",
            self.character_name
        ))?;
        self.overlay().context(format!(
            "Failed to apply overlay for '{}'",
            self.character_name
        ))?;
        Ok(())
    }

    pub fn focus(&self) -> Result<()> {
        let ev = ClientMessageEvent {
            response_type: CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: self.src,
            type_: self.atoms.net_active_window,
            data: [2, 0, 0, 0, 0].into(),
        };

        self.conn
            .send_event(
                false,
                self.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                ev,
            )
            .context(format!(
                "Failed to send focus event for '{}'",
                self.character_name
            ))?;
        self.conn
            .flush()
            .context("Failed to flush X11 connection after focus event")?;
        info!(window = self.window, character = %self.character_name, "Focused window");
        Ok(())
    }

    pub fn reposition(&mut self, x: i16, y: i16) -> Result<()> {
        self.conn
            .configure_window(
                self.window,
                &ConfigureWindowAux::new().x(x as i32).y(y as i32),
            )
            .context(format!(
                "Failed to reposition window for '{}' to ({}, {})",
                self.character_name, x, y
            ))?;

        // Update cached position
        self.current_position = Position::new(x, y);

        self.conn
            .flush()
            .context("Failed to flush X11 connection after reposition")?;
        Ok(())
    }

    /// Called when character name changes (login/logout)
    /// Updates name and optionally moves to new position
    pub fn set_character_name(
        &mut self,
        new_name: String,
        new_position: Option<Position>,
    ) -> Result<()> {
        self.character_name = new_name;
        self.update_name().context(format!(
            "Failed to update name overlay to '{}'",
            self.character_name
        ))?;

        if let Some(Position { x, y }) = new_position {
            self.reposition(x, y).context(format!(
                "Failed to reposition after character change to '{}'",
                self.character_name
            ))?;
        }
        Ok(())
    }

    pub fn is_hovered(&self, x: i16, y: i16) -> bool {
        // Use cached position to avoid synchronous X11 roundtrip
        x >= self.current_position.x
            && x <= self.current_position.x + self.dimensions.width as i16
            && y >= self.current_position.y
            && y <= self.current_position.y + self.dimensions.height as i16
    }
}

impl Drop for Thumbnail<'_> {
    fn drop(&mut self) {
        // Clean up each resource independently to prevent cascade failures
        // If one cleanup fails, we still attempt to clean up the rest

        if let Err(e) = self.conn.damage_destroy(self.damage) {
            error!(damage = self.damage, error = %e, "Failed to destroy damage");
        }

        if let Err(e) = self.conn.free_gc(self.overlay_gc) {
            error!(gc = self.overlay_gc, error = %e, "Failed to free GC");
        }

        if let Err(e) = self.conn.render_free_picture(self.overlay_picture) {
            error!(picture = self.overlay_picture, error = %e, "Failed to free overlay picture");
        }

        if let Err(e) = self.conn.render_free_picture(self.src_picture) {
            error!(picture = self.src_picture, error = %e, "Failed to free source picture");
        }

        if let Err(e) = self.conn.render_free_picture(self.dst_picture) {
            error!(picture = self.dst_picture, error = %e, "Failed to free destination picture");
        }

        if let Err(e) = self.conn.render_free_picture(self.border_fill) {
            error!(picture = self.border_fill, error = %e, "Failed to free border fill picture");
        }

        if let Err(e) = self.conn.free_pixmap(self.overlay_pixmap) {
            error!(pixmap = self.overlay_pixmap, error = %e, "Failed to free pixmap");
        }

        if let Err(e) = self.conn.destroy_window(self.window) {
            error!(
                window = self.window,
                character = %self.character_name,
                error = %e,
                "Failed to destroy window"
            );
        }

        if let Err(e) = self.conn.flush() {
            error!(error = %e, "Failed to flush X11 connection during cleanup");
        }
    }
}
