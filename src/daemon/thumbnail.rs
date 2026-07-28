//! Thumbnail window management
//!
//! Creates and manages X11 overlay windows that display scaled previews of tracked sources.
//! High-level logic that delegates rendering to `renderer::ThumbnailRenderer`.

use anyhow::{Context, Result};
use tracing::debug;
use x11rb::protocol::damage::Damage;
use x11rb::protocol::xproto::{ConnectionExt, Window};

use crate::common::constants::positioning;
use crate::common::types::{Dimensions, Position, SourceIdentity, SourceKind, ThumbnailState};
use crate::config::DisplayConfig;
use crate::x11::AppContext;

use super::font::FontRenderer;
use super::overlay::OverlayIdentity;
use super::renderer::ThumbnailRenderer;
use super::snapping::Rect;

fn effective_character_name_from<'a>(
    live_name: &'a str,
    remembered_name: Option<&'a str>,
) -> &'a str {
    if !live_name.is_empty() {
        live_name
    } else {
        remembered_name.unwrap_or("")
    }
}

fn display_character_name_from<'a>(
    live_name: &'a str,
    remembered_name: Option<&'a str>,
    show_logged_out_character_name: bool,
) -> &'a str {
    if !live_name.is_empty() {
        live_name
    } else if show_logged_out_character_name {
        remembered_name.unwrap_or("")
    } else {
        ""
    }
}

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
    source_kind: SourceKind,
    pub character_name: String,
    remembered_character_name: Option<String>,
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
    /// * `character_name` - Live name reported by the source window.
    /// * `remembered_character_name` - Last known EVE character for a logged-out client.
    /// * `src` - Source window ID.
    /// * `font_renderer` - Renderer for shared font resources.
    /// * `position` - Optional initial position (if loaded from config).
    /// * `dimensions` - Initial size.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx: &AppContext<'a>,
        source_kind: SourceKind,
        character_name: String,
        remembered_character_name: Option<String>,
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
            .context("Failed to send geometry query for source window")?
            .reply()
            .context(format!(
                "Failed to get geometry for source window {} (character: '{}')",
                src, character_name
            ))?;

        // Use saved position OR top-left of the source window with the standard padding.
        let Position { x, y } = position.unwrap_or_else(|| {
            Position::new(
                src_geom.x + positioning::DEFAULT_SPAWN_OFFSET,
                src_geom.y + positioning::DEFAULT_SPAWN_OFFSET,
            )
        });
        let remembered_character_name = remembered_character_name.filter(|name| !name.is_empty());
        let style_character_name =
            effective_character_name_from(&character_name, remembered_character_name.as_deref());
        let display_character_name = display_character_name_from(
            &character_name,
            remembered_character_name.as_deref(),
            display_config.show_logged_out_character_name,
        );

        debug!(
            character = %character_name,
            remembered_character = %remembered_character_name.as_deref().unwrap_or(""),
            x = x,
            y = y,
            width = dimensions.width,
            height = dimensions.height,
            "Creating thumbnail"
        );

        let renderer = ThumbnailRenderer::new(
            ctx,
            source_kind,
            style_character_name,
            display_character_name,
            src,
            src_geom.depth,
            display_config,
            font_renderer,
            x,
            y,
            dimensions,
        )?;

        Ok(Self {
            source_kind,
            character_name,
            remembered_character_name,
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

    /// Returns the live display name reported by the source window.
    pub fn live_character_name(&self) -> &str {
        &self.character_name
    }

    pub fn source_kind(&self) -> SourceKind {
        self.source_kind
    }

    /// Returns the remembered session identity synchronized from SessionState.
    pub fn remembered_character_name(&self) -> Option<&str> {
        self.remembered_character_name.as_deref()
    }

    /// Update the cached remembered identity from authoritative SessionState.
    pub fn sync_remembered_character_name(&mut self, remembered_character_name: Option<String>) {
        self.remembered_character_name = remembered_character_name.filter(|name| !name.is_empty());
    }

    /// Returns the name used for behavior and per-source settings.
    pub fn effective_character_name(&self) -> &str {
        effective_character_name_from(self.live_character_name(), self.remembered_character_name())
    }

    pub fn effective_source_identity(&self) -> Option<SourceIdentity> {
        let name = self.effective_character_name();
        (!name.is_empty()).then(|| SourceIdentity::new(self.source_kind, name.to_string()))
    }

    /// Returns the label that should be shown on the thumbnail.
    pub fn display_character_name<'b>(&'b self, display_config: &DisplayConfig) -> &'b str {
        display_character_name_from(
            self.live_character_name(),
            self.remembered_character_name(),
            display_config.show_logged_out_character_name,
        )
    }

    fn overlay_identity<'b>(&'b self, display_config: &DisplayConfig) -> OverlayIdentity<'b> {
        OverlayIdentity {
            kind: self.source_kind,
            style: self.effective_character_name(),
            display: self.display_character_name(display_config),
        }
    }

    /// Returns the underlying X11 window ID of the thumbnail.
    pub fn window(&self) -> Window {
        self.renderer.window
    }

    /// Returns the source application window ID.
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

    /// Moves the thumbnail to a new position and updates the cached state.
    pub fn reposition(&mut self, x: i16, y: i16) -> Result<()> {
        self.renderer.reposition(x, y)?;
        // Update cached position
        self.current_position = Position::new(x, y);
        Ok(())
    }

    /// Queues a move without flushing or changing the cached position.
    pub(super) fn queue_reposition(&self, x: i16, y: i16) -> Result<()> {
        self.renderer.queue_reposition(x, y)?;
        Ok(())
    }

    /// Records a queued position after its shared X11 flush succeeds.
    pub(super) fn confirm_reposition(&mut self, position: Position) {
        self.current_position = position;
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
        let effective_character_name = self.effective_character_name().to_string();
        self.renderer
            .resize(&effective_character_name, width, height)?;
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
            self.overlay_identity(display_config),
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
                self.overlay_identity(display_config),
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
        // Resolve per-source preview visibility override against the global setting.
        // override_render_preview: None = use global, Some(true) = force on, Some(false) = force off
        let should_render = display_config
            .settings_for(self.source_kind, self.effective_character_name())
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

        let preview_mode = display_config
            .settings_for(self.source_kind, self.effective_character_name())
            .map(|settings| &settings.preview_mode)
            .unwrap_or(&self.preview_mode);

        match self.state {
            ThumbnailState::Minimized => {
                self.renderer.minimized(
                    display_config,
                    self.overlay_identity(display_config),
                    self.dimensions,
                    font_renderer,
                )?;
            }
            _ => match preview_mode {
                crate::common::types::PreviewMode::Live => {
                    self.renderer
                        .update(self.effective_character_name(), self.dimensions)?;
                }
                crate::common::types::PreviewMode::Static { color } => {
                    let color_u32 = crate::manager::utils::parse_hex_color(color)
                        .map_err(|_| anyhow::anyhow!("Invalid hex color: {}", color))?;

                    let x_color = x11rb::protocol::render::Color {
                        red: (color_u32.r() as u16) * 257,
                        green: (color_u32.g() as u16) * 257,
                        blue: (color_u32.b() as u16) * 257,
                        alpha: (color_u32.a() as u16) * 257,
                    };

                    self.renderer.update_static(
                        self.effective_character_name(),
                        self.dimensions,
                        x_color,
                    )?;
                }
            },
        }
        Ok(())
    }

    /// Redraw the name overlay after display-related config changes.
    pub fn refresh_name_overlay(
        &self,
        display_config: &DisplayConfig,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        self.renderer.update_name(
            display_config,
            self.overlay_identity(display_config),
            self.dimensions,
            font_renderer,
        )
    }

    /// Called when character name changes (e.g. login detection update).
    pub fn set_character_name(
        &mut self,
        new_name: String,
        new_settings: Option<crate::common::types::CharacterSettings>,
        display_config: &DisplayConfig,
        font_renderer: &FontRenderer,
    ) -> Result<()> {
        self.character_name = new_name;
        if !self.character_name.is_empty() {
            self.remembered_character_name = Some(self.character_name.clone());
        }

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
                self.overlay_identity(display_config),
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

#[cfg(test)]
mod tests {
    use super::{display_character_name_from, effective_character_name_from};

    #[test]
    fn effective_name_prefers_live_character() {
        assert_eq!(
            effective_character_name_from("Live", Some("Remembered")),
            "Live"
        );
    }

    #[test]
    fn effective_name_uses_remembered_logged_out_identity() {
        assert_eq!(
            effective_character_name_from("", Some("Remembered")),
            "Remembered"
        );
    }

    #[test]
    fn effective_name_is_empty_for_unidentified_logged_out_identity() {
        assert_eq!(effective_character_name_from("", None), "");
    }

    #[test]
    fn display_name_keeps_live_character_even_when_logged_out_display_is_disabled() {
        assert_eq!(
            display_character_name_from("Live", Some("Remembered"), false),
            "Live"
        );
    }

    #[test]
    fn display_name_hides_remembered_logged_out_identity_when_disabled() {
        assert_eq!(
            display_character_name_from("", Some("Remembered"), false),
            ""
        );
    }

    #[test]
    fn display_name_shows_remembered_logged_out_identity_when_enabled() {
        assert_eq!(
            display_character_name_from("", Some("Remembered"), true),
            "Remembered"
        );
    }
}
