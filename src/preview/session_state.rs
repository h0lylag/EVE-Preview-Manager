//! Session-only state for window position tracking
//!
//! Tracks window positions within the current X11 session. Used for preserving
//! thumbnail positions when characters log out and for position inheritance.

use std::collections::HashMap;
use tracing::info;
use x11rb::protocol::xproto::Window;

use crate::types::{CharacterSettings, Position};

/// Runtime state for position tracking
/// Window positions are session-only (not persisted to disk)
#[derive(Default)]
pub struct SessionState {
    /// Window ID → position (session-only, not persisted)
    /// Used for logged-out windows that show "EVE" without character name
    /// Window IDs are ephemeral and don't survive X11 server restarts
    pub window_positions: HashMap<Window, Position>,

    /// Window ID → last known character name (session-only)
    /// Tracks which character was last logged in on each window
    /// Used for including logged-out windows in cycle (if enabled in profile)
    pub window_last_character: HashMap<Window, String>,
}

impl SessionState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get initial position for a thumbnail
    /// Priority: character position (from persistent state) > window position (if enabled) > None (use EVE window + offset)
    /// Window position only used for logged-out windows or if preserve_thumbnail_position_on_swap is enabled
    pub fn get_position(
        &self,
        character_name: &str,
        window: Window,
        character_thumbnails: &HashMap<String, CharacterSettings>,
        preserve_position_on_swap: bool,
    ) -> Option<Position> {
        // If character has a name (not just "EVE"), check character position from config
        if !character_name.is_empty() {
            if let Some(settings) = character_thumbnails.get(character_name) {
                info!(character = %character_name, x = settings.x, y = settings.y, "Using saved position for character");
                return Some(settings.position());
            }

            // New character with no saved position → check if we should inherit window position
            if preserve_position_on_swap && let Some(&pos) = self.window_positions.get(&window) {
                info!(character = %character_name, position = ?pos, "Inheriting window position for new character");
                return Some(pos);
            }

            // New character with no saved position and no inheritance → return None (use EVE window + offset)
            return None;
        }

        // Logged-out window ("EVE" title) → use window position from this session
        if let Some(&pos) = self.window_positions.get(&window) {
            info!(window = window, position = ?pos, "Using session position for logged-out window");
            Some(pos)
        } else {
            None
        }
    }

    /// Update session position (window tracking)
    pub fn update_window_position(&mut self, window: Window, x: i16, y: i16) {
        self.window_positions.insert(window, Position::new(x, y));
        info!(
            window = window,
            x = x,
            y = y,
            "Saved session position for window"
        );
    }

    /// Remove window from session tracking (called on DestroyNotify)
    pub fn remove_window(&mut self, window: Window) {
        self.window_positions.remove(&window);
        self.window_last_character.remove(&window);
    }

    /// Update last known character for a window (called on character name change)
    /// Only tracks non-empty character names (ignores logged-out state)
    pub fn update_last_character(&mut self, window: Window, character_name: &str) {
        if !character_name.is_empty() {
            self.window_last_character
                .insert(window, character_name.to_string());
            info!(window = window, character = %character_name, "Tracked last known character for window");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_position_character_from_config() {
        let state = SessionState::new();
        let mut char_positions = HashMap::new();
        char_positions.insert(
            "Alice".to_string(),
            CharacterSettings::new(100, 200, 240, 135),
        );

        let pos = state.get_position("Alice", 123, &char_positions, true);
        assert_eq!(pos, Some(Position::new(100, 200)));
    }

    #[test]
    fn test_get_position_new_character_no_inherit() {
        let state = SessionState {
            window_positions: HashMap::from([(456, Position::new(300, 400))]),
            window_last_character: HashMap::new(),
        };
        let char_positions = HashMap::new();

        // New character "Bob" with window 456 that has position but preserve disabled → should return None (EVE window + offset)
        let pos = state.get_position("Bob", 456, &char_positions, false);
        assert_eq!(pos, None);
    }

    #[test]
    fn test_get_position_new_character_with_inherit() {
        let state = SessionState {
            window_positions: HashMap::from([(789, Position::new(500, 600))]),
            window_last_character: HashMap::new(),
        };
        let char_positions = HashMap::new();

        // New character "Charlie" with preserve enabled → should use window position
        let pos = state.get_position("Charlie", 789, &char_positions, true);
        assert_eq!(pos, Some(Position::new(500, 600)));
    }

    #[test]
    fn test_get_position_new_character_inherit_but_no_window_position() {
        let state = SessionState {
            window_positions: HashMap::new(),
            window_last_character: HashMap::new(),
        };
        let char_positions = HashMap::new();

        // preserve enabled but window 999 has no saved position → None (EVE window + offset)
        let pos = state.get_position("Diana", 999, &char_positions, true);
        assert_eq!(pos, None);
    }

    #[test]
    fn test_get_position_logged_out_window() {
        let state = SessionState {
            window_positions: HashMap::from([(111, Position::new(700, 800))]),
            window_last_character: HashMap::new(),
        };
        let char_positions = HashMap::new();

        // Empty character name (logged-out "EVE" window) → use window position (preserve flag doesn't matter for logged-out)
        let pos = state.get_position("", 111, &char_positions, false);
        assert_eq!(pos, Some(Position::new(700, 800)));
    }

    #[test]
    fn test_get_position_logged_out_window_no_saved_position() {
        let state = SessionState::new();
        let char_positions = HashMap::new();

        // Logged-out window with no saved position → None (EVE window + offset)
        let pos = state.get_position("", 222, &char_positions, true);
        assert_eq!(pos, None);
    }

    #[test]
    fn test_get_position_character_priority_over_window() {
        let mut state = SessionState::new();
        state.window_positions.insert(333, Position::new(900, 1000));

        let mut char_positions = HashMap::new();
        char_positions.insert(
            "Eve".to_string(),
            CharacterSettings::new(1100, 1200, 240, 135),
        );

        // Character position should take priority even with preserve enabled
        let pos = state.get_position("Eve", 333, &char_positions, true);
        assert_eq!(pos, Some(Position::new(1100, 1200)));
    }

    #[test]
    fn test_update_window_position() {
        let mut state = SessionState::new();

        state.update_window_position(444, 1300, 1400);
        assert_eq!(
            state.window_positions.get(&444),
            Some(&Position::new(1300, 1400))
        );

        // Update existing position
        state.update_window_position(444, 1500, 1600);
        assert_eq!(
            state.window_positions.get(&444),
            Some(&Position::new(1500, 1600))
        );
    }

    #[test]
    fn test_update_window_position_multiple_windows() {
        let mut state = SessionState::new();

        state.update_window_position(555, 100, 200);
        state.update_window_position(666, 300, 400);

        assert_eq!(
            state.window_positions.get(&555),
            Some(&Position::new(100, 200))
        );
        assert_eq!(
            state.window_positions.get(&666),
            Some(&Position::new(300, 400))
        );
    }

    #[test]
    fn test_remove_window() {
        let mut state = SessionState::new();

        state.update_window_position(777, 100, 200);
        state.update_window_position(888, 300, 400);
        assert_eq!(state.window_positions.len(), 2);

        // Remove first window
        state.remove_window(777);
        assert_eq!(state.window_positions.get(&777), None);
        assert_eq!(
            state.window_positions.get(&888),
            Some(&Position::new(300, 400))
        );
        assert_eq!(state.window_positions.len(), 1);

        // Remove second window
        state.remove_window(888);
        assert_eq!(state.window_positions.get(&888), None);
        assert_eq!(state.window_positions.len(), 0);
    }

    #[test]
    fn test_remove_nonexistent_window() {
        let mut state = SessionState::new();
        state.update_window_position(999, 100, 200);

        // Removing non-existent window should be safe (no-op)
        state.remove_window(123);
        assert_eq!(state.window_positions.len(), 1);
        assert_eq!(
            state.window_positions.get(&999),
            Some(&Position::new(100, 200))
        );
    }
}
