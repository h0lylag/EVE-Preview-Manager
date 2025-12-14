//! Thumbnail window management
//!
//! Creates and manages X11 overlay windows that display scaled previews of EVE clients.
//! High-level logic that delegates rendering to `renderer::ThumbnailRenderer`.

use anyhow::{Context, Result};
use tracing::info;
use x11rb::protocol::damage::Damage;
use x11rb::protocol::xproto::{ConnectionExt, Window};

use crate::constants::positioning;
use crate::types::{Dimensions, Position, ThumbnailState};
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

        let renderer =
            ThumbnailRenderer::new(ctx, &character_name, src, font_renderer, x, y, dimensions)?;

        Ok(Self {
            character_name,
            state: ThumbnailState::default(),
            hidden: false,
            input_state: InputState::default(),
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

    /// Updates the thumbnail border based on focus state.
    pub fn border(&self, focused: bool) -> Result<()> {
        self.renderer
            .border(&self.character_name, self.dimensions, focused)
    }

    /// Sets the thumbnail to "Minimized" state and renders the localized overlay.
    pub fn minimized(&mut self) -> Result<()> {
        self.state = ThumbnailState::Minimized;
        // Only render if allowed (might be hidden)
        // If hidden, the rendering will happen next time update() is called after reveal
        if self.is_visible() {
            self.renderer
                .minimized(&self.character_name, self.dimensions)?;
        }
        Ok(())
    }

    /// Triggers a repaint of the thumbnail content and overlay.
    ///
    /// Respects the current logical state (`Minimized` vs `Normal`).
    /// If hidden, does nothing.
    pub fn update(&self) -> Result<()> {
        if !self.is_visible() {
            return Ok(());
        }

        match self.state {
            ThumbnailState::Minimized => {
                self.renderer
                    .minimized(&self.character_name, self.dimensions)?;
            }
            _ => {
                self.renderer
                    .update(&self.character_name, self.dimensions)?;
            }
        }
        Ok(())
    }

    /// Requests focus for the source EVE client.
    pub fn focus(&self) -> Result<()> {
        self.renderer.focus(&self.character_name)
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

        self.dimensions = crate::types::Dimensions::new(width, height);
        self.renderer.resize(&self.character_name, width, height)?;
        Ok(())
    }

    /// Called when character name changes (e.g. login detection update).
    ///
    /// Updates the internal name, repellers the name overlay, and optionally applies new saved settings.
    pub fn set_character_name(
        &mut self,
        new_name: String,
        new_settings: Option<crate::types::CharacterSettings>,
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
        }

        // Force update of name (and implicit repaint if visible)
        self.renderer
            .update_name(&self.character_name, self.dimensions)
            .context(format!(
                "Failed to update name overlay to '{}'",
                self.character_name
            ))?;

        self.update()
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
