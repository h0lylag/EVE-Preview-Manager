//! Daemon Thumbnail Renderer
//!
//! Handles low-level X11 window creation, rendering, and resource management.

use anyhow::{Context, Result};
use tracing::{debug, error, info};
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

use crate::common::constants::x11;
use crate::common::types::Dimensions;
use crate::x11::{AppContext, to_fixed};

use super::font::FontRenderer;
use super::overlay::OverlayRenderer;
use crate::config::DisplayConfig;

#[derive(Debug)]
/// Handles low-level X11 window creation, rendering, and resource management.
///
/// This struct is responsible for:
/// - Creating and managing the X11 thumbnail window.
/// - Setting window properties (opacity, input masks, PID).
/// - Compositing the source window and overlay onto the thumbnail window.
/// - Handling X11 resource cleanup via `Drop`.
pub struct ThumbnailRenderer<'a> {
    // === X11 Window Handles ===
    /// The X11 window ID for the clickable thumbnail.
    pub window: Window,
    /// The source X11 window ID (the EVE client).
    pub src: Window,
    /// The parent window ID, if the source window has been reparented (e.g. by a window manager).
    pub parent: Option<Window>,
    /// The DAMAGE extension handle used to track updates to the source window.
    pub damage: Damage,
    root: Window,

    // === X11 Render Resources (private, owned resources) ===
    src_picture: Picture,
    dst_picture: Picture,

    // === Overlay Renderer (handles text, border, pixmap) ===
    overlay: OverlayRenderer<'a>,

    // === Borrowed Dependencies (private, references to app context) ===
    pub conn: &'a RustConnection,
    pub atoms: &'a crate::x11::CachedAtoms,
}

impl<'a> ThumbnailRenderer<'a> {
    pub fn set_parent(&mut self, parent: Option<Window>) {
        self.parent = parent;
    }

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
        opacity: u32,
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
                &[opacity],
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
            .inspect_err(|e| {
                error!(
                    window = window,
                    error = ?e,
                    "Failed to map thumbnail window"
                )
            })
            .context(format!(
                "Failed to map thumbnail window for '{}'",
                character_name
            ))?;
        debug!(
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
        src_depth: u8,
        character_name: &str,
    ) -> Result<(Picture, Picture)> {
        // Determine source format based on window depth
        let src_format = if src_depth == 32 {
            info!(character = %character_name, depth = src_depth, format = "ARGB32", "Using ARGB format for source window");
            ctx.formats.argb
        } else {
            // Default to RGB (usually 24-bit)
            // If it's not 32 or root depth, this might still be wrong, but it covers standard cases.
            debug!(character = %character_name, depth = src_depth, format = "RGB24", "Using RGB format for source window");
            ctx.formats.rgb
        };

        // Source picture
        let src_picture = ctx
            .conn
            .generate_id()
            .context("Failed to generate ID for source picture")?;

        ctx.conn
            .render_create_picture(src_picture, src, src_format, &CreatePictureAux::new())
            .context(format!(
                "Failed to create source picture for '{}'",
                character_name
            ))?;

        // Apply bilinear filter for smoother downscaling (better text readability)
        ctx.conn
            .render_set_picture_filter(src_picture, "bilinear".as_bytes(), &[])
            .context(format!(
                "Failed to set bilinear filter for '{}'",
                character_name
            ))?;

        // Destination picture
        let dst_picture = ctx
            .conn
            .generate_id()
            .context("Failed to generate ID for destination picture")?;
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

        Ok((src_picture, dst_picture))
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

    /// Creates a new `ThumbnailRenderer`.
    ///
    /// # Arguments
    /// * `ctx` - The application context containing X11 connection and config.
    /// * `character_name` - Name of the character (for logging and window titles).
    /// * `src` - The source window ID to render as a thumbnail.
    /// * `src_depth` - The depth of the source window (to select correct Render format).
    /// * `font_renderer` - Renderer for text overlays.
    /// * `x`, `y` - Initial screen coordinates.
    /// * `dimensions` - Initial size of the thumbnail.
    ///
    /// # Errors
    /// Returns an error if any X11 resource creation fails (window, pictures, pixmaps).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx: &AppContext<'a>,
        character_name: &str,
        src: Window,
        src_depth: u8,
        display_config: &crate::config::DisplayConfig,
        font_renderer: &FontRenderer,
        x: i16,
        y: i16,
        dimensions: Dimensions,
    ) -> Result<Self> {
        // Create window and setup properties
        let window = Self::create_window(ctx, character_name, x, y, dimensions)?;

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
            character_name: character_name.to_string(),
            should_cleanup: true,
        };

        Self::setup_window_properties(ctx, window, display_config.opacity, character_name)?;

        // Create rendering resources
        let (src_picture, dst_picture) =
            Self::create_render_resources(ctx, window, src, src_depth, character_name)?;

        // Create overlay renderer
        let overlay = OverlayRenderer::new(
            ctx.conn,
            display_config,
            ctx.formats,
            font_renderer,
            ctx.screen.root,
            dimensions,
            character_name,
        )?;

        // Setup damage tracking
        let damage = Self::create_damage_tracking(ctx, src, character_name)?;

        let renderer = Self {
            // X11 Window Handles
            window,
            src,
            parent: {
                // Proactively check for existing parent (handle already-running windowed clients)
                match ctx.conn.query_tree(src) {
                    Ok(cookie) => match cookie.reply() {
                        Ok(reply) => {
                            if reply.parent != ctx.screen.root {
                                info!(
                                    window = src,
                                    parent = reply.parent,
                                    "Detected existing parent for window"
                                );
                                Some(reply.parent)
                            } else {
                                None
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to get query_tree reply: {:?}", e);
                            None
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Failed to send query_tree: {:?}", e);
                        None
                    }
                }
            },
            damage,
            root: ctx.screen.root,

            // X11 Render Resources
            src_picture,
            dst_picture,

            // Overlay
            overlay,

            // Borrowed Dependencies
            conn: ctx.conn,
            atoms: ctx.atoms,
        };

        // Success! Disable cleanup guard since Thumbnail's Drop will handle it now
        window_guard.should_cleanup = false;

        Ok(renderer)
    }

    /// Maps the thumbnail window, making it visible on screen.
    pub fn map(&self) -> Result<()> {
        self.conn.map_window(self.window)?;
        Ok(())
    }

    /// Unmaps the thumbnail window, hiding it from screen.
    pub fn unmap(&self) -> Result<()> {
        self.conn.unmap_window(self.window)?;
        Ok(())
    }

    /// Captures the current content of the source window and composites it into the thumbnail.
    ///
    /// This applies the necessary scaling transform to fit the source content into the thumbnail dimensions.
    ///
    /// # Errors
    /// Returns an error if X11 composite operations fail.
    /// Captures the current content of the source window and composites it into the thumbnail.
    ///
    /// This applies the necessary scaling transform to fit the source content into the thumbnail dimensions.
    ///
    /// # Errors
    /// Returns an error if X11 composite operations fail.
    pub fn capture(&self, character_name: &str, dimensions: Dimensions) -> Result<()> {
        // Query attributes to check map state
        let attr_cookie = self.conn.get_window_attributes(self.src)?;
        let attrs = attr_cookie.reply()?;

        // SAFETY: Check map state to prevent crashing KWin/Xwayland.
        // Attempting to composite from an unmapped window (even with valid size)
        // triggers a "Double free or corruption" crash in glamor/Xwayland.
        if attrs.map_state != MapState::VIEWABLE {
            // Debug logging for capture issues
            tracing::trace!(
                character = character_name,
                src_window = self.src,
                "Skipping capture of unmapped window"
            );
            return Ok(()); // Skip capture to prevent crash
        }

        // NOTE: Query geometry fresh every frame.
        // Do NOT rely on cached dimensions (e.g., from ConfigureNotify).
        // Steam/Proton windows race between events and actual state, leading to
        // invalid reads if we trust the cache. Always ask the server.
        // Query source window geometry fresh
        let geom_cookie = self.conn.get_geometry(self.src)?;
        let geom = geom_cookie.reply()?;
        let src_width = geom.width;
        let src_height = geom.height;

        // Debug logging for capture issues
        tracing::trace!(
            character = character_name,
            src_window = self.src,
            width = src_width,
            height = src_height,
            "Capturing source window"
        );

        // Safety Check: Skip capture if window is effectively empty/unmapped to avoid X server crashes
        // A 1x1 window (like seen with Firefox initially) can crash X11 drivers when used in Render operations
        if src_width <= 1 || src_height <= 1 {
            tracing::warn!(
                character = character_name,
                width = src_width,
                height = src_height,
                "Skipping capture of 1x1/empty window (likely not mapped yet)"
            );
            return Ok(());
        }

        let transform = Transform {
            matrix11: to_fixed(src_width as f32 / dimensions.width as f32),
            matrix22: to_fixed(src_height as f32 / dimensions.height as f32),
            matrix33: to_fixed(1.0),
            ..Default::default()
        };
        self.conn
            .render_set_picture_transform(self.src_picture, transform)
            .context(format!("Failed to set transform for '{}'", character_name))?;
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
                dimensions.width,
                dimensions.height,
            )
            .context(format!(
                "Failed to composite source window for '{}'",
                character_name
            ))?;
        Ok(())
    }

    /// Fills the thumbnail with a static solid color.
    pub fn fill_static(
        &self,
        character_name: &str,
        dimensions: Dimensions,
        color: x11rb::protocol::render::Color,
    ) -> Result<()> {
        let rect = x11rb::protocol::xproto::Rectangle {
            x: 0,
            y: 0,
            width: dimensions.width,
            height: dimensions.height,
        };

        self.conn
            .render_fill_rectangles(PictOp::SRC, self.dst_picture, color, &[rect])
            .context(format!(
                "Failed to fill static color for '{}'",
                character_name
            ))?;
        Ok(())
    }

    /// Draws the border and updates the name overlay.
    ///
    /// # Arguments
    /// * `focused` - If true, draws the border. If false, clears the border area.
    /// * `skipped` - If true, draws the skipped indicator (diagonal red lines).
    pub fn border(
        &self,
        display_config: &DisplayConfig,
        character_name: &str,
        dimensions: Dimensions,
        focused: bool,
        skipped: bool,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        self.overlay.draw_border(
            display_config,
            character_name,
            dimensions,
            focused,
            skipped,
            font_renderer,
        )?;

        self.overlay(character_name, dimensions)
            .context(format!("Failed to apply overlay for '{}'", character_name))
    }

    /// Renders the "MINIMIZED" state overlay.
    ///
    /// Clears any existing border and draws the localized logic for minimized windows.
    pub fn minimized(
        &self,
        display_config: &DisplayConfig,
        character_name: &str,
        dimensions: Dimensions,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        self.overlay
            .draw_minimized(display_config, character_name, dimensions, font_renderer)?;
        self.update(character_name, dimensions).context(format!(
            "Failed to update minimized display for '{}'",
            character_name
        ))?;
        Ok(())
    }

    /// Updates the text overlay with the character name.
    pub fn update_name(
        &self,
        display_config: &DisplayConfig,
        character_name: &str,
        dimensions: Dimensions,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        // Calculate appropriate border size to preserve the hole
        // We default to focused=false since this is usually called during initialization or generic updates
        // However, if we are focused, the next border() call will correct it.
        let border_size = self
            .overlay
            .calculate_border_size(display_config, character_name, false);

        // Must clear content area explicitly now
        self.overlay
            .clear_content_area(dimensions, border_size)
            .context(format!(
                "Failed to clear content area for '{}'",
                character_name
            ))?;

        self.overlay.update_name(
            display_config,
            character_name,
            dimensions,
            border_size,
            font_renderer,
        )
    }

    /// Composites the text/border overlay on top of the thumbnail content.
    pub fn overlay(&self, character_name: &str, dimensions: Dimensions) -> Result<()> {
        self.conn
            .render_composite(
                PictOp::OVER,
                self.overlay.overlay_picture,
                0u32,
                self.dst_picture,
                0,
                0,
                0,
                0,
                0,
                0,
                dimensions.width,
                dimensions.height,
            )
            .context(format!(
                "Failed to composite overlay onto destination for '{}'",
                character_name
            ))?;
        Ok(())
    }

    /// Logic for full update cycle: capture source -> apply overlay.
    pub fn update(&self, character_name: &str, dimensions: Dimensions) -> Result<()> {
        self.capture(character_name, dimensions).context(format!(
            "Failed to capture source window for '{}'",
            character_name
        ))?;
        self.overlay(character_name, dimensions)
            .context(format!("Failed to apply overlay for '{}'", character_name))?;
        Ok(())
    }

    /// Logic for static update cycle: fill static color -> apply overlay.
    pub fn update_static(
        &self,
        character_name: &str,
        dimensions: Dimensions,
        color: x11rb::protocol::render::Color,
    ) -> Result<()> {
        self.fill_static(character_name, dimensions, color)?;
        self.overlay(character_name, dimensions)
            .context(format!("Failed to apply overlay for '{}'", character_name))?;
        Ok(())
    }

    /// Sends a request to the Window Manager to focus the source window.
    ///
    /// # Arguments
    /// * `timestamp` - X11 timestamp from the input event that triggered this action.
    pub fn focus(&self, character_name: &str, timestamp: u32) -> Result<()> {
        // Explicitly raise the window to the front.
        // Some clients (like RuneLite/Java) or Window Managers (especially under Xwayland)
        // require an explicit StackMode::ABOVE request to actually bring the window
        // to the foreground, even when _NET_ACTIVE_WINDOW is sent.
        self.conn
            .configure_window(
                self.src,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )
            .context(format!(
                "Failed to raise window for '{}' to top of stack",
                character_name
            ))?;

        let ev = ClientMessageEvent {
            response_type: CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: self.src,
            type_: self.atoms.net_active_window,
            data: ClientMessageData::from([
                x11::ACTIVE_WINDOW_SOURCE_PAGER,
                timestamp,
                0,
                0,
                0,
            ]),
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
                character_name
            ))?;
        self.conn
            .flush()
            .context("Failed to flush X11 connection after focus event")?;
        info!(
            window = self.window,
            character = %character_name,
            timestamp = timestamp,
            "Activating window via click"
        );
        Ok(())
    }

    /// Moves the thumbnail window to a new position.
    pub fn reposition(&mut self, character_name: &str, x: i16, y: i16) -> Result<()> {
        self.conn
            .configure_window(
                self.window,
                &ConfigureWindowAux::new().x(x as i32).y(y as i32),
            )
            .context(format!(
                "Failed to reposition window for '{}' to ({}, {})",
                character_name, x, y
            ))?;

        self.conn
            .flush()
            .context("Failed to flush X11 connection after reposition")?;
        Ok(())
    }

    /// Resizes the thumbnail window and recreates necessary render resources.
    pub fn resize(&mut self, character_name: &str, width: u16, height: u16) -> Result<()> {
        self.conn
            .configure_window(
                self.window,
                &ConfigureWindowAux::new()
                    .width(width as u32)
                    .height(height as u32),
            )
            .context(format!("Failed to resize window for '{}'", character_name))?;

        // Recreate overlay resources via helper
        self.overlay
            .resize(self.root, width, height)
            .context(format!(
                "Failed to resize overlay resources for '{}'",
                character_name
            ))?;

        self.conn
            .flush()
            .context("Failed to flush X11 connection after resize")?;
        Ok(())
    }
}

impl Drop for ThumbnailRenderer<'_> {
    fn drop(&mut self) {
        // Clean up each resource independently to prevent cascade failures
        // If one cleanup fails, we still attempt to clean up the rest

        if let Err(e) = self.conn.damage_destroy(self.damage) {
            error!(damage = self.damage, error = %e, "Failed to destroy damage");
        }

        // OverlayRenderer Drop will handle overlay resources

        if let Err(e) = self.conn.render_free_picture(self.src_picture) {
            error!(
                picture = self.src_picture,
                error = %e,
                "Failed to free source picture"
            );
        }

        if let Err(e) = self.conn.render_free_picture(self.dst_picture) {
            error!(
                picture = self.dst_picture,
                error = %e,
                "Failed to free destination picture"
            );
        }

        if let Err(e) = self.conn.destroy_window(self.window) {
            error!(
                window = self.window,
                error = %e,
                "Failed to destroy window"
            );
        }

        if let Err(e) = self.conn.flush() {
            error!(error = %e, "Failed to flush X11 connection during cleanup");
        }
    }
}
