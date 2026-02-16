//! Thumbnail window management
//!
//! Creates and manages X11 overlay windows that display scaled previews of EVE clients.
//! High-level logic that delegates rendering to `renderer::ThumbnailRenderer`.

use anyhow::{Context, Result};
use tracing::debug;
use x11rb::protocol::damage::Damage;
use x11rb::protocol::xproto::{ConnectionExt, Window};

use crate::common::constants::positioning;
use crate::common::types::{Dimensions, Position, ThumbnailState};
use crate::config::DisplayConfig;
use crate::x11::AppContext;

use super::font::FontRenderer;
use super::renderer::ThumbnailRenderer;
use super::snapping::Rect;

#[derive(Debug, Default)]
pub struct InputState {
    pub dragging: bool,
    pub drag_start: Position,
    pub win_start: Position,
    pub snap_targets: Vec<Rect>, // Cached snap targets computed when drag starts
}

#[derive(Debug)]
/// Top-level Thumbnail manager.
///
/// This struct holds the high-level state of a single thumbnail preview, including:
/// - Application state (name, visibility, dragging).
/// - Dimensions and positioning.
/// - Input handling state.
///
/// It delegates actual X11 operations (rendering, window management) to `ThumbnailRenderer`.
pub struct Thumbnail<'a> {
    // === Application State (public, frequently accessed) ===
    pub character_name: String,
    pub state: ThumbnailState,
    pub hidden: bool, // Tracks if hidden by "hide_when_no_focus"
    pub input_state: InputState,
    pub preview_mode: crate::common::types::PreviewMode,

    // === Geometry (public, immutable after creation) ===
    pub dimensions: Dimensions,

    pub current_position: Position, // Cached position for hit testing

    // === Backend ===
    renderer: ThumbnailRenderer<'a>,
}

impl<'a> Thumbnail<'a> {
    /// Creates a new `Thumbnail` instance.
    ///
    /// This initializes both the high-level state and the underlying X11 window/renderer.
    ///
    /// # Arguments
    /// * `ctx` - Application context.
    /// * `character_name` - Name of the character.
    /// * `src` - Source EVE window ID.
    /// * `font_renderer` - Renderer for shared font resources.
    /// * `position` - Optional initial position (if loaded from config).
    /// * `dimensions` - Initial size.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx: &AppContext<'a>,
        character_name: String,
        src: Window,
        display_config: &crate::config::DisplayConfig,
        font_renderer: &FontRenderer,
        position: Option<Position>,
        dimensions: Dimensions,
        preview_mode: crate::common::types::PreviewMode,
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
        debug!(
            character = %character_name,
            x = x,
            y = y,
            width = dimensions.width,
            height = dimensions.height,
            "Creating thumbnail"
        );

        let renderer = ThumbnailRenderer::new(
            ctx,
            &character_name,
            src,
            src_geom.depth,
            display_config,
            font_renderer,
            x,
            y,
            dimensions,
        )?;

        Ok(Self {
            character_name,
            state: ThumbnailState::default(),
            hidden: false,
            input_state: InputState::default(),
            preview_mode,
            dimensions,
            current_position: Position::new(x, y),
            renderer,
        })
    }

    // Accessors

    /// Returns the underlying X11 window ID of the thumbnail.
    pub fn window(&self) -> Window {
        self.renderer.window
    }

    /// Returns the source EVE window ID.
    pub fn src(&self) -> Window {
        self.renderer.src
    }

    /// Returns the DAMAGE extension object ID tracking the source window.
    pub fn damage(&self) -> Damage {
        self.renderer.damage
    }

    /// Returns the parent window ID, if known.
    pub fn parent(&self) -> Option<Window> {
        self.renderer.parent
    }

    /// Updates the parent window ID (e.g. after a ReparentNotify event).
    pub fn set_parent(&mut self, parent: Option<Window>) {
        self.renderer.set_parent(parent);
    }

    /// Checks if the thumbnail is currently visible (mapped and not hidden).
    pub fn is_visible(&self) -> bool {
        !self.hidden
    }

    /// Sets the visibility of the thumbnail.
    ///
    /// Manages X11 mapping/unmapping and upgrades internal `hidden` state.
    /// Does NOT modify the logical `state` (Normal/Minimized).
    pub fn visibility(&mut self, visible: bool) -> Result<()> {
        if self.is_visible() == visible {
            return Ok(());
        }

        if visible {
            self.hidden = false;
            self.renderer.map().context(format!(
                "Failed to map window for '{}'",
                self.character_name
            ))?;
        } else {
            self.hidden = true;
            self.renderer.unmap().context(format!(
                "Failed to unmap window for '{}'",
                self.character_name
            ))?;
        }
        Ok(())
    }

    /// Requests focus for the source EVE client.
    ///
    /// # Arguments
    /// * `timestamp` - X11 timestamp from the input event.
    pub fn focus(&self, timestamp: u32) -> Result<()> {
        self.renderer.focus(&self.character_name, timestamp)
    }

    /// Update the cached source dimensions (e.g. on ConfigureNotify)
    ///
    /// # NOTE
    /// This is currently a **no-op**. We intentionally do NOT cache dimensions here.
    /// Relying on `ConfigureNotify` for dimensions introduced race conditions with Steam/Xwayland
    /// windows, where the event loop would see valid dimensions but the server would see 1x1.
    ///
    /// Geometry is now queried freshly in `renderer::capture()`.
    pub fn update_source_dimensions(&mut self, _width: u16, _height: u16) {
        // No-op
    }

    /// Moves the thumbnail to a new position updates the cached state.
    pub fn reposition(&mut self, x: i16, y: i16) -> Result<()> {
        self.renderer.reposition(&self.character_name, x, y)?;
        // Update cached position
        self.current_position = Position::new(x, y);
        Ok(())
    }

    /// Resizes the thumbnail.
    ///
    /// Only performs X11 resize if the dimensions have actually changed.
    pub fn resize(&mut self, width: u16, height: u16) -> Result<()> {
        if self.dimensions.width == width && self.dimensions.height == height {
            return Ok(());
        }

        if width == 0 || height == 0 {
            return Err(anyhow::anyhow!(
                "Invalid resize dimensions for '{}': {}x{}",
                self.character_name,
                width,
                height
            ));
        }

        self.dimensions = crate::common::types::Dimensions::new(width, height);
        self.renderer.resize(&self.character_name, width, height)?;
        Ok(())
    }

    /// Updates the thumbnail border based on focus state.
    pub fn border(
        &self,
        display_config: &DisplayConfig,
        focused: bool,
        skipped: bool,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        self.renderer.border(
            display_config,
            &self.character_name,
            self.dimensions,
            focused,
            skipped,
            font_renderer,
        )
    }

    /// Sets the thumbnail to "Minimized" state and renders the localized overlay.
    pub fn minimized(
        &mut self,
        display_config: &DisplayConfig,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        self.state = ThumbnailState::Minimized;
        // Only render if allowed (might be hidden)
        // If hidden, the rendering will happen next time update() is called after reveal
        if self.is_visible() {
            self.renderer.minimized(
                display_config,
                &self.character_name,
                self.dimensions,
                font_renderer,
            )?;
        }
        Ok(())
    }

    /// Triggers a repaint of the thumbnail content and overlay.
    pub fn update(
        &mut self,
        display_config: &DisplayConfig,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        // Resolve per-character preview visibility override against the global setting.
        // override_render_preview: None = use global, Some(true) = force on, Some(false) = force off
        let should_render = display_config
            .character_settings
            .get(&self.character_name)
            .and_then(|s| s.override_render_preview)
            .unwrap_or(display_config.enabled);

        if !should_render {
            // Unmap the entire thumbnail window so it fully disappears
            self.visibility(false)?;
            return Ok(());
        }

        if !self.is_visible() {
            return Ok(());
        }

        match self.state {
            ThumbnailState::Minimized => {
                self.renderer.minimized(
                    display_config,
                    &self.character_name,
                    self.dimensions,
                    font_renderer,
                )?;
            }
            _ => match &self.preview_mode {
                crate::common::types::PreviewMode::Live => {
                    self.renderer
                        .update(&self.character_name, self.dimensions)?;
                }
                crate::common::types::PreviewMode::Static { color } => {
                    // ... color parsing ...
                    let color_u32 = crate::manager::utils::parse_hex_color(color)
                        .map_err(|_| anyhow::anyhow!("Invalid hex color: {}", color))?;

                    let x_color = x11rb::protocol::render::Color {
                        red: (color_u32.r() as u16) * 257,
                        green: (color_u32.g() as u16) * 257,
                        blue: (color_u32.b() as u16) * 257,
                        alpha: (color_u32.a() as u16) * 257,
                    };

                    self.renderer
                        .update_static(&self.character_name, self.dimensions, x_color)?;
                }
            },
        }
        Ok(())
    }

    // focus, reposition, resize unchanged

    /// Called when character name changes (e.g. login detection update).
    pub fn set_character_name(
        &mut self,
        new_name: String,
        new_settings: Option<crate::common::types::CharacterSettings>,
        display_config: &DisplayConfig,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        self.character_name = new_name;

        // NOTE: Resize must precede update_name because it regenerates the overlay pixmap.

        if let Some(settings) = new_settings {
            self.reposition(settings.x, settings.y).context(format!(
                "Failed to reposition after character change to '{}'",
                self.character_name
            ))?;

            self.resize(settings.dimensions.width, settings.dimensions.height)
                .context(format!(
                    "Failed to resize after character change to '{}'",
                    self.character_name
                ))?;

            self.preview_mode = settings.preview_mode;
        }

        // Force update of name (and implicit repaint if visible)
        self.renderer
            .update_name(
                display_config,
                &self.character_name,
                self.dimensions,
                font_renderer,
            )
            .context(format!(
                "Failed to update name overlay to '{}'",
                self.character_name
            ))?;

        self.update(display_config, font_renderer)
            .context("Failed to repaint after character change")?;

        Ok(())
    }

    /// Checks if a screen coordinate point is inside the thumbnail's bounds.
    ///
    /// Uses cached `current_position` to avoid synchronous X11 roundtrip.
    pub fn is_hovered(&self, x: i16, y: i16) -> bool {
        // Use cached position to avoid synchronous X11 roundtrip
        x >= self.current_position.x
            && x <= self.current_position.x + self.dimensions.width as i16
            && y >= self.current_position.y
            && y <= self.current_position.y + self.dimensions.height as i16
    }
}
