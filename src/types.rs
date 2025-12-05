//! Domain types for type safety and clarity

use serde::{Deserialize, Serialize};

/// EVE Online window type classification
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EveWindowType {
    /// Logged-in EVE client with character name
    LoggedIn(String),
    /// Logged-out EVE client (character select screen)
    LoggedOut,
}

impl EveWindowType {
    /// Get the character name, or empty string if logged out
    pub fn character_name(&self) -> &str {
        match self {
            EveWindowType::LoggedIn(name) => name,
            EveWindowType::LoggedOut => "",
        }
    }
    
}

/// A position in 2D space (X11 coordinates)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Position {
    pub x: i16,
    pub y: i16,
}

impl Position {
    /// Create a new position
    pub fn new(x: i16, y: i16) -> Self {
        Self { x, y }
    }

    /// Convert to tuple for compatibility
    pub fn as_tuple(self) -> (i16, i16) {
        (self.x, self.y)
    }

    /// Create from tuple
    pub fn from_tuple(tuple: (i16, i16)) -> Self {
        Self { x: tuple.0, y: tuple.1 }
    }
}

impl From<(i16, i16)> for Position {
    fn from(tuple: (i16, i16)) -> Self {
        Self::from_tuple(tuple)
    }
}

impl From<Position> for (i16, i16) {
    fn from(pos: Position) -> Self {
        pos.as_tuple()
    }
}

/// Thumbnail dimensions (width × height)
/// Using a newtype prevents accidentally swapping width and height
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Dimensions {
    pub width: u16,
    pub height: u16,
}

impl Dimensions {
    /// Create new dimensions
    pub fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }

    /// Calculate aspect ratio (width / height)
    pub fn aspect_ratio(&self) -> f32 {
        if self.height == 0 {
            0.0
        } else {
            self.width as f32 / self.height as f32
        }
    }

    /// Calculate total area in pixels
    pub fn area(&self) -> u32 {
        self.width as u32 * self.height as u32
    }

    /// Convert to tuple for compatibility
    pub fn as_tuple(self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// Create from tuple
    pub fn from_tuple(tuple: (u16, u16)) -> Self {
        Self { width: tuple.0, height: tuple.1 }
    }
}

impl From<(u16, u16)> for Dimensions {
    fn from(tuple: (u16, u16)) -> Self {
        Self::from_tuple(tuple)
    }
}

impl From<Dimensions> for (u16, u16) {
    fn from(dims: Dimensions) -> Self {
        dims.as_tuple()
    }
}

/// Text offset from border edge
/// Using a newtype makes the coordinate context clear (not absolute window coordinates)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct TextOffset {
    pub x: i16,
    pub y: i16,
}

impl TextOffset {
    /// Create text offset from border edge
    pub fn from_border_edge(x: i16, y: i16) -> Self {
        Self { x, y }
    }

}

/// Thumbnail lifecycle state
/// Using an enum makes invalid states (e.g., focused + minimized) impossible to represent
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailState {
    /// Visible and available for interaction (may or may not have focus)
    Normal { focused: bool },
    /// Window is minimized by the window manager
    Minimized,
    /// Hidden due to hide_when_no_focus feature (all EVE windows unfocused)
    Hidden,
}

impl ThumbnailState {
    /// Check if the thumbnail should be visible on screen
    pub fn is_visible(&self) -> bool {
        matches!(self, Self::Normal { .. } | Self::Minimized)
    }
    
    /// Check if the thumbnail currently has input focus
    pub fn is_focused(&self) -> bool {
        matches!(self, Self::Normal { focused: true })
    }
    
    /// Check if the thumbnail is minimized by the window manager
    pub fn is_minimized(&self) -> bool {
        matches!(self, Self::Minimized)
    }
}

impl Default for ThumbnailState {
    fn default() -> Self {
        // New thumbnails start in unfocused normal state
        Self::Normal { focused: false }
    }
}

/// Per-character settings: position and thumbnail dimensions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterSettings {
    pub x: i16,
    pub y: i16,
    /// Thumbnail dimensions (0 = use auto-detect)
    #[serde(flatten)]
    pub dimensions: Dimensions,
}

impl CharacterSettings {
    pub fn new(x: i16, y: i16, width: u16, height: u16) -> Self {
        Self { 
            x, 
            y, 
            dimensions: Dimensions::new(width, height),
        }
    }
    
    pub fn position(&self) -> Position {
        Position::new(self.x, self.y)
    }
    
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_creation() {
        let pos = Position::new(100, 200);
        assert_eq!(pos.x, 100);
        assert_eq!(pos.y, 200);
    }

    #[test]
    fn test_position_tuple_conversion() {
        let pos = Position::new(150, 250);
        let tuple = pos.as_tuple();
        assert_eq!(tuple, (150, 250));
        
        let pos2 = Position::from_tuple(tuple);
        assert_eq!(pos, pos2);
    }

    #[test]
    fn test_position_from_trait() {
        let pos: Position = (100, 200).into();
        assert_eq!(pos.x, 100);
        assert_eq!(pos.y, 200);
        
        let tuple: (i16, i16) = pos.into();
        assert_eq!(tuple, (100, 200));
    }

    #[test]
    fn test_dimensions_creation() {
        let dims = Dimensions::new(640, 480);
        assert_eq!(dims.width, 640);
        assert_eq!(dims.height, 480);
    }

    #[test]
    fn test_dimensions_aspect_ratio() {
        let dims = Dimensions::new(1920, 1080);
        assert!((dims.aspect_ratio() - 1.777).abs() < 0.001);
        
        let square = Dimensions::new(100, 100);
        assert_eq!(square.aspect_ratio(), 1.0);
        
        // Zero height edge case
        let zero_height = Dimensions::new(100, 0);
        assert_eq!(zero_height.aspect_ratio(), 0.0);
    }

    #[test]
    fn test_dimensions_area() {
        let dims = Dimensions::new(1920, 1080);
        assert_eq!(dims.area(), 2_073_600);
        
        let small = Dimensions::new(10, 20);
        assert_eq!(small.area(), 200);
    }

    #[test]
    fn test_dimensions_tuple_conversion() {
        let dims = Dimensions::new(800, 600);
        let tuple = dims.as_tuple();
        assert_eq!(tuple, (800, 600));
        
        let dims2 = Dimensions::from_tuple(tuple);
        assert_eq!(dims, dims2);
    }

    #[test]
    fn test_dimensions_from_trait() {
        let dims: Dimensions = (1024, 768).into();
        assert_eq!(dims.width, 1024);
        assert_eq!(dims.height, 768);
        
        let tuple: (u16, u16) = dims.into();
        assert_eq!(tuple, (1024, 768));
    }

    #[test]
    fn test_text_offset_creation() {
    let offset = TextOffset::from_border_edge(10, 20);
    assert_eq!(offset.x, 10);
    assert_eq!(offset.y, 20);
        
    let offset2 = TextOffset::from_border_edge(15, 25);
    assert_eq!(offset2.x, 15);
    assert_eq!(offset2.y, 25);
    }

    #[test]
    fn test_thumbnail_state_normal_unfocused() {
        let state = ThumbnailState::Normal { focused: false };
        assert!(state.is_visible());
        assert!(!state.is_focused());
        assert!(!state.is_minimized());
    }

    #[test]
    fn test_thumbnail_state_normal_focused() {
        let state = ThumbnailState::Normal { focused: true };
        assert!(state.is_visible());
        assert!(state.is_focused());
        assert!(!state.is_minimized());
    }

    #[test]
    fn test_thumbnail_state_minimized() {
        let state = ThumbnailState::Minimized;
        assert!(state.is_visible());
        assert!(!state.is_focused());
        assert!(state.is_minimized());
    }

    #[test]
    fn test_thumbnail_state_hidden() {
        let state = ThumbnailState::Hidden;
        assert!(!state.is_visible());
        assert!(!state.is_focused());
        assert!(!state.is_minimized());
    }

    #[test]
    fn test_thumbnail_state_default() {
        let state = ThumbnailState::default();
        assert_eq!(state, ThumbnailState::Normal { focused: false });
        assert!(state.is_visible());
        assert!(!state.is_focused());
    }
}
