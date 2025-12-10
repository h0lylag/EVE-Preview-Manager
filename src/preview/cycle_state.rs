//! Hotkey cycle state management
//!
//! Tracks active EVE windows and their cycle order for hotkey-based navigation.
//! Only characters listed in the profile's cycle_group are included in cycling.

use std::collections::HashMap;
use tracing::{debug, warn};
use x11rb::protocol::xproto::Window;

/// Maps character names to their window IDs and positions in cycle order
pub struct CycleState {
    /// Configured order from profile's cycle_group (persistent across sessions)
    config_order: Vec<String>,

    /// Current index in config_order (0-based)
    current_index: usize,

    /// Active windows: character_name → window_id
    /// Only includes characters that currently have windows
    active_windows: HashMap<String, Window>,
}

impl CycleState {
    pub fn new(config_order: Vec<String>) -> Self {
        Self {
            config_order,
            current_index: 0,
            active_windows: HashMap::new(),
        }
    }

    /// Register a new EVE window (called from CreateNotify)
    pub fn add_window(&mut self, character_name: String, window: Window) {
        debug!(character = %character_name, window = window, "Adding window for character");
        self.active_windows.insert(character_name.clone(), window);

        // Note: Only characters listed in the profile's `cycle_group` will be included in the cycle order.
        // We track all windows here, but `cycle_forward/backward` logic filters internally based on the config.
    }

    /// Remove window (called from DestroyNotify)
    pub fn remove_window(&mut self, window: Window) {
        // Find and remove from active_windows
        if let Some((name, _)) = self
            .active_windows
            .iter()
            .find(|&(_, &w)| w == window)
            .map(|(k, v)| (k.clone(), *v))
        {
            debug!(character = %name, window = window, "Removing window for character");
            self.active_windows.remove(&name);

            // If we removed the current character, clamp index
            self.clamp_index();
        }
    }

    /// Update character name (called on login/logout)
    pub fn update_character(&mut self, window: Window, new_name: String) {
        // Remove old entry
        if let Some((old_name, _)) = self
            .active_windows
            .iter()
            .find(|&(_, &w)| w == window)
            .map(|(k, v)| (k.clone(), *v))
        {
            self.active_windows.remove(&old_name);
        }

        // Add new entry
        self.add_window(new_name, window);
    }

    /// Move to next character in config order (forward cycle hotkey)
    /// Returns (window, character_name) to activate, or None if no active characters
    /// Only cycles through characters in the configured cycle_group list
    ///
    /// # Parameters
    /// - `logged_out_map`: Optional window→last_character mapping for including logged-out windows
    pub fn cycle_forward(
        &mut self,
        logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Option<(Window, &str)> {
        if self.active_windows.is_empty() && logged_out_map.is_none() {
            warn!(
                active_windows = self.active_windows.len(),
                "No active windows to cycle"
            );
            return None;
        }

        if self.config_order.is_empty() {
            warn!(
                config_order_len = self.config_order.len(),
                "Config order is empty - add character names to cycle_group in profile settings"
            );
            return None;
        }

        let start_index = self.current_index;
        loop {
            self.current_index = (self.current_index + 1) % self.config_order.len();

            let character_name = &self.config_order[self.current_index];

            // Check logged-in characters first
            if let Some(&window) = self.active_windows.get(character_name) {
                debug!(character = %character_name, index = self.current_index, "Cycling forward to logged-in character");
                return Some((window, character_name.as_str()));
            }

            // If enabled, check for logged-out windows with this character's last identity
            if let Some(map) = logged_out_map
                && let Some((&window, _)) = map
                    .iter()
                    .find(|(_, last_char)| *last_char == character_name)
            {
                debug!(character = %character_name, index = self.current_index, window = window, "Cycling forward to logged-out character");
                return Some((window, character_name.as_str()));
            }

            // Wrapped around without finding active or logged-out character
            if self.current_index == start_index {
                warn!(
                    config_order_len = self.config_order.len(),
                    active_windows = self.active_windows.len(),
                    "No active characters found in config order (configured characters may not be running)"
                );
                return None;
            }
        }
    }

    /// Move to previous character in config order (backward cycle hotkey)
    /// Returns (window, character_name) to activate, or None if no active characters
    /// Only cycles through characters in the configured cycle_group list
    ///
    /// # Parameters
    /// - `logged_out_map`: Optional window→last_character mapping for including logged-out windows
    pub fn cycle_backward(
        &mut self,
        logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Option<(Window, &str)> {
        if self.active_windows.is_empty() && logged_out_map.is_none() {
            warn!(
                active_windows = self.active_windows.len(),
                "No active windows to cycle"
            );
            return None;
        }

        if self.config_order.is_empty() {
            warn!(
                config_order_len = self.config_order.len(),
                "Config order is empty - add character names to cycle_group in profile settings"
            );
            return None;
        }

        let start_index = self.current_index;
        loop {
            self.current_index = if self.current_index == 0 {
                self.config_order.len() - 1
            } else {
                self.current_index - 1
            };

            let character_name = &self.config_order[self.current_index];

            // Check logged-in characters first
            if let Some(&window) = self.active_windows.get(character_name) {
                debug!(character = %character_name, index = self.current_index, "Cycling backward to logged-in character");
                return Some((window, character_name.as_str()));
            }

            // If enabled, check for logged-out windows with this character's last identity
            if let Some(map) = logged_out_map
                && let Some((&window, _)) = map
                    .iter()
                    .find(|(_, last_char)| *last_char == character_name)
            {
                debug!(character = %character_name, index = self.current_index, window = window, "Cycling backward to logged-out character");
                return Some((window, character_name.as_str()));
            }

            // Wrapped around without finding active or logged-out character
            if self.current_index == start_index {
                warn!(
                    config_order_len = self.config_order.len(),
                    active_windows = self.active_windows.len(),
                    "No active characters found in config order (configured characters may not be running)"
                );
                return None;
            }
        }
    }

    /// Activate specific character by name (per-character hotkey)
    /// Returns (window, character_name) to activate, or None if character not active
    /// Updates current_index to maintain consistency with cycle state
    ///
    /// # Parameters
    /// - `character_name`: Character to activate
    /// - `logged_out_map`: Optional window→last_character mapping for including logged-out windows
    pub fn activate_character<'a>(
        &mut self,
        character_name: &'a str,
        logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Option<(Window, &'a str)> {
        // Check logged-in characters first
        if let Some(&window) = self.active_windows.get(character_name) {
            debug!(character = %character_name, window = window, "Activating logged-in character via per-character hotkey");

            // Update current_index if this character is in config_order
            if let Some(index) = self.config_order.iter().position(|c| c == character_name) {
                self.current_index = index;
            }

            return Some((window, character_name));
        }

        // If enabled, check for logged-out windows with this character's last identity
        if let Some(map) = logged_out_map
            && let Some((&window, _)) = map
                .iter()
                .find(|(_, last_char)| *last_char == character_name)
        {
            debug!(character = %character_name, window = window, "Activating logged-out character via per-character hotkey");

            // Update current_index if this character is in config_order
            if let Some(index) = self.config_order.iter().position(|c| c == character_name) {
                self.current_index = index;
            }

            return Some((window, character_name));
        }

        // Character not found or not active
        debug!(character = %character_name, "Character not active, cannot activate");
        None
    }

    /// Set current character (called when clicking thumbnail)
    /// Returns true if character exists in config order
    pub fn set_current(&mut self, character_name: &str) -> bool {
        if let Some(index) = self.config_order.iter().position(|c| c == character_name) {
            debug!(character = %character_name, index = index, "Setting current character");
            self.current_index = index;
            true
        } else {
            warn!(character = %character_name, "Character not in config order");
            false
        }
    }

    /// Set current cycle position based on focused window
    /// Returns true if window was found and state updated
    pub fn set_current_by_window(&mut self, window: Window) -> bool {
        if let Some((character_name, _)) = self.active_windows.iter().find(|&(_, &w)| w == window) {
            let character_name = character_name.clone();
            return self.set_current(&character_name);
        }
        false
    }

    /// Clamp index to valid range after removing characters
    fn clamp_index(&mut self) {
        if !self.config_order.is_empty() && self.current_index >= self.config_order.len() {
            self.current_index = 0;
        }
    }

    /// Get current config order for saving
    pub fn config_order(&self) -> &[String] {
        &self.config_order
    }

    /// Cycles to the next available character within a specific subgroup of characters.
    /// Used for shared hotkeys (e.g. F1 bound to both CharA and CharB) to toggle between them.
    /// The cycle order respects the global configuration order to ensure consistent behavior.
    pub fn activate_next_in_group(
        &mut self,
        group: &[String],
        logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Option<(Window, String)> {
        // 1. Filter group to include only characters present in config_order
        //    and map them to their global indices
        let mut group_indices: Vec<(usize, &String)> = group
            .iter()
            .filter_map(|name| {
                self.config_order
                    .iter()
                    .position(|c| c == name)
                    .map(|idx| (idx, name))
            })
            .collect();

        if group_indices.is_empty() {
            debug!("No characters from hotkey group found in config order");
            return None;
        }

        // 2. Sort by global index to ensure we follow cycle order
        group_indices.sort_by_key(|(idx, _)| *idx);

        // 3. Find search start position
        let current_pos = self.current_index;

        // 4. Search forward: find the first available character in the group after the current position
        for (idx, name) in &group_indices {
            if *idx > current_pos
                && let Some((window, _)) = self.activate_character(name, logged_out_map)
            {
                debug!(character = %name, "Activated next in group (forward)");
                return Some((window, name.to_string()));
            }
        }

        // 5. Wrap around: Search from beginning of the list
        // Since we filtered internally and sorted, the first available character
        // in the group will be the correct wrap-around target.
        for (_, name) in &group_indices {
            if let Some((window, _)) = self.activate_character(name, logged_out_map) {
                debug!(character = %name, "Activated next in group (wrapped)");
                return Some((window, name.to_string()));
            }
        }

        debug!("No active characters found in hotkey group");
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cycle_forward_basic() {
        let mut state = CycleState::new(vec![
            "Char1".to_string(),
            "Char2".to_string(),
            "Char3".to_string(),
        ]);

        state.add_window("Char1".to_string(), 100);
        state.add_window("Char2".to_string(), 200);
        state.add_window("Char3".to_string(), 300);

        // Start at index 0 (Char1)
        assert_eq!(state.cycle_forward(None), Some((200, "Char2"))); // → Char2
        assert_eq!(state.cycle_forward(None), Some((300, "Char3"))); // → Char3
        assert_eq!(state.cycle_forward(None), Some((100, "Char1"))); // → Char1 (wrap)
    }

    #[test]
    fn test_cycle_backward_basic() {
        let mut state = CycleState::new(vec![
            "Char1".to_string(),
            "Char2".to_string(),
            "Char3".to_string(),
        ]);

        state.add_window("Char1".to_string(), 100);
        state.add_window("Char2".to_string(), 200);
        state.add_window("Char3".to_string(), 300);

        // Start at index 0 (Char1)
        assert_eq!(state.cycle_backward(None), Some((300, "Char3"))); // ← Char3 (wrap)
        assert_eq!(state.cycle_backward(None), Some((200, "Char2"))); // ← Char2
        assert_eq!(state.cycle_backward(None), Some((100, "Char1"))); // ← Char1
    }

    #[test]
    fn test_set_current() {
        let mut state = CycleState::new(vec!["Char1".to_string(), "Char2".to_string()]);

        state.add_window("Char1".to_string(), 100);
        state.add_window("Char2".to_string(), 200);

        assert!(state.set_current("Char2"));
        assert_eq!(state.cycle_forward(None), Some((100, "Char1"))); // Next after Char2 is Char1
    }

    #[test]
    fn test_skip_inactive_characters() {
        let mut state = CycleState::new(vec![
            "Active1".to_string(),
            "Inactive".to_string(),
            "Active2".to_string(),
        ]);

        state.add_window("Active1".to_string(), 100);
        state.add_window("Active2".to_string(), 300);
        // "Inactive" not added

        // Should skip "Inactive" in cycle
        assert_eq!(state.cycle_forward(None), Some((300, "Active2"))); // Active1 → Active2
        assert_eq!(state.cycle_forward(None), Some((100, "Active1"))); // Active2 → Active1 (wrap, skip Inactive)
    }
    #[test]
    fn test_activate_next_in_group() {
        let mut state = CycleState::new(vec![
            "A".to_string(), // 0
            "B".to_string(), // 1
            "C".to_string(), // 2
            "D".to_string(), // 3
        ]);

        state.add_window("A".to_string(), 100);
        state.add_window("C".to_string(), 300);

        let group = vec!["A".to_string(), "C".to_string()];

        // Start at 0 (A)
        // Next in group should be C (index 2)
        let res = state.activate_next_in_group(&group, None);
        assert_eq!(res, Some((300, "C".to_string())));
        assert_eq!(state.current_index, 2); // State should update to C

        // Next in group should be A (index 0) - wrapping
        let res = state.activate_next_in_group(&group, None);
        assert_eq!(res, Some((100, "A".to_string())));
        assert_eq!(state.current_index, 0);
    }

    #[test]
    fn test_remove_current_character() {
        let mut state = CycleState::new(vec!["Char1".to_string(), "Char2".to_string()]);

        state.add_window("Char1".to_string(), 100);
        state.add_window("Char2".to_string(), 200);

        state.set_current("Char2");
        state.remove_window(200); // Remove current character

        // Index should be clamped and cycle should still work
        assert_eq!(state.cycle_forward(None), Some((100, "Char1")));
    }

    #[test]
    fn test_empty_order() {
        let mut state = CycleState::new(vec![]);
        assert_eq!(state.cycle_forward(None), None);
        assert_eq!(state.cycle_backward(None), None);
    }

    #[test]
    fn test_auto_add_disabled() {
        // Characters NOT in config order should not be auto-added or cycled
        let mut state = CycleState::new(vec!["Char1".to_string()]);

        state.add_window("Char1".to_string(), 100);
        state.add_window("NewChar".to_string(), 200); // Not in config_order

        // NewChar should NOT be added to config order
        assert_eq!(state.config_order.len(), 1);
        assert!(!state.config_order.contains(&"NewChar".to_string()));

        // Cycling should skip NewChar
        assert_eq!(state.cycle_forward(None), Some((100, "Char1")));
        assert_eq!(state.cycle_forward(None), Some((100, "Char1"))); // Still Char1
    }

    #[test]
    fn test_update_character_name() {
        let mut state = CycleState::new(vec!["OldName".to_string()]);

        state.add_window("OldName".to_string(), 100);
        state.update_character(100, "NewName".to_string());

        // Old name should be removed, new name added
        assert!(!state.active_windows.contains_key("OldName"));
        assert_eq!(state.active_windows.get("NewName"), Some(&100));
    }
}
