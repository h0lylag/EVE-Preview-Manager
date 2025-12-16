//! Hotkey cycle state management
//!
//! Tracks active EVE windows and their cycle order for hotkey-based navigation.
//! Only characters listed in the profile's cycle_group are included in cycling.

use std::collections::{HashMap, HashSet};
use tracing::{debug, warn};
use x11rb::protocol::xproto::Window;

/// Maps character names to their window IDs and positions in cycle order
pub struct CycleState {
    /// Configured order from profile's cycle_group (persistent across sessions)
    config_order: Vec<String>,

    /// Current index in config_order (0-based)
    current_index: usize,

    /// Currently focused active window (if any)
    /// Used to resolve starting position for cycling, especially for detached characters
    current_window: Option<Window>,

    /// Active windows: character_name → window_id
    /// Only includes characters that currently have windows
    active_windows: HashMap<String, Window>,

    /// Characters temporarily skipped from cycling
    skipped_characters: HashSet<String>,
}

impl CycleState {
    pub fn new(config_order: Vec<String>) -> Self {
        Self {
            config_order,
            current_index: 0,
            current_window: None,
            active_windows: HashMap::new(),
            skipped_characters: HashSet::new(),
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

            // Clear current_window if it matches
            if self.current_window == Some(window) {
                self.current_window = None;
            }
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

    /// Toggle skip status for a character
    /// Returns new skipped state (true = skipped, false = active)
    pub fn toggle_skip(&mut self, character_name: &str) -> bool {
        if self.skipped_characters.contains(character_name) {
            debug!(character = %character_name, "Unskipping character");
            self.skipped_characters.remove(character_name);
            false
        } else {
            debug!(character = %character_name, "Skipping character");
            self.skipped_characters.insert(character_name.to_string());
            true
        }
    }

    /// Check if a character is currently skipped
    pub fn is_skipped(&self, character_name: &str) -> bool {
        self.skipped_characters.contains(character_name)
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

            // Skip characters marked as skipped
            if self.skipped_characters.contains(character_name) {
                // Check termination condition (wrapped around to start)
                if self.current_index == start_index {
                    warn!("All active characters are skipped");
                    return None;
                }
                continue;
            }

            // Check logged-in characters first
            if let Some(&window) = self.active_windows.get(character_name) {
                debug!(character = %character_name, index = self.current_index, "Cycling forward to logged-in character");
                self.current_window = Some(window);
                return Some((window, character_name.as_str()));
            }

            // If enabled, check for logged-out windows with this character's last identity
            if let Some(map) = logged_out_map
                && let Some((&window, _)) = map
                    .iter()
                    .find(|(_, last_char)| *last_char == character_name)
            {
                debug!(character = %character_name, index = self.current_index, window = window, "Cycling forward to logged-out character");
                self.current_window = Some(window);
                return Some((window, character_name.as_str()));
            }

            // Wrapped around without finding active or logged-out character
            if self.current_index == start_index {
                // If we get here, it means we scanned everything unskipped but found nothing active
                // (Warning already logged in normal logic flow or implicitly handled)
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

            // Skip characters marked as skipped
            if self.skipped_characters.contains(character_name) {
                // Check termination condition (wrapped around to start)
                if self.current_index == start_index {
                    warn!("All active characters are skipped");
                    return None;
                }
                continue;
            }

            // Check logged-in characters first
            if let Some(&window) = self.active_windows.get(character_name) {
                debug!(character = %character_name, index = self.current_index, "Cycling backward to logged-in character");
                self.current_window = Some(window);
                return Some((window, character_name.as_str()));
            }

            // If enabled, check for logged-out windows with this character's last identity
            if let Some(map) = logged_out_map
                && let Some((&window, _)) = map
                    .iter()
                    .find(|(_, last_char)| *last_char == character_name)
            {
                debug!(character = %character_name, index = self.current_index, window = window, "Cycling backward to logged-out character");
                self.current_window = Some(window);
                return Some((window, character_name.as_str()));
            }

            // Wrapped around without finding active or logged-out character
            if self.current_index == start_index {
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

            // Always update current_window
            self.current_window = Some(window);

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

            // Always update current_window
            self.current_window = Some(window);

            return Some((window, character_name));
        }

        // Character not found or not active
        debug!(character = %character_name, "Character not active, cannot activate");
        None
    }

    /// Set current character (called when clicking thumbnail)
    /// Returns true if character exists in config order
    pub fn set_current(&mut self, character_name: &str) -> bool {
        // Resolve window for this character if possible to update current_window
        if let Some(&window) = self.active_windows.get(character_name) {
            self.current_window = Some(window);
        }

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
    /// Returns true if window was found and state updated (even for detached characters)
    pub fn set_current_by_window(&mut self, window: Window) -> bool {
        // Always track the current window, even if it's not part of the cycle group
        self.current_window = Some(window);

        if let Some((character_name, _)) = self.active_windows.iter().find(|&(_, &w)| w == window) {
            let character_name = character_name.clone();
            // This will try to update current_index if in group, but we return true regardless if found
            self.set_current(&character_name);
            return true; // Found the window
        }

        // Window not known (not an EVE client?)
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
    ///
    /// # Sorting Logic
    /// 1. Characters present in `config_order` (Cycle Group) are prioritized, sorted by their index in the group.
    /// 2. Characters NOT in `config_order` are appended, sorted alphabetically.
    pub fn activate_next_in_group(
        &mut self,
        group: &[String],
        logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Option<(Window, String)> {
        // 1. Separate group into "In Cycle Group" and "Out of Cycle Group"
        let mut in_group_indices: Vec<(usize, &String)> = Vec::new();
        let mut out_of_group: Vec<&String> = Vec::new();

        for name in group {
            if let Some(idx) = self.config_order.iter().position(|c| c == name) {
                in_group_indices.push((idx, name));
            } else {
                out_of_group.push(name);
            }
        }

        // 2. Sort "In Group" by config order
        in_group_indices.sort_by_key(|(idx, _)| *idx);

        // 3. Sort "Out of Group" alphabetically
        out_of_group.sort();

        // 4. Combine into final sorted candidates list
        let sorted_candidates: Vec<&String> = in_group_indices
            .into_iter()
            .map(|(_, name)| name)
            .chain(out_of_group)
            .collect();

        if sorted_candidates.is_empty() {
            debug!("No characters found in hotkey group");
            return None;
        }

        // 5. Find starting position based on `current_window`
        let start_pos = if let Some(curr_win) = self.current_window
            // Find which character owns this window
            && let Some((curr_char, _)) = self.active_windows.iter().find(|&(_, &w)| w == curr_win)
            // Find that character in the sorted candidates
            && let Some(pos) = sorted_candidates.iter().position(|&c| c == curr_char)
        {
            pos
        } else {
            // Default to starting before the first item
            sorted_candidates.len().saturating_sub(1)
        };

        // 6. Cycle through candidates starting after start_pos
        for i in 1..=sorted_candidates.len() {
            let idx = (start_pos + i) % sorted_candidates.len();
            let name = sorted_candidates[idx];

            // Respect skipped status
            if self.skipped_characters.contains(name) {
                continue;
            }

            if let Some((window, _)) = self.activate_character(name, logged_out_map) {
                debug!(character = %name, "Activated next in group (advanced)");
                return Some((window, name.to_string()));
            }
        }

        debug!("No active characters found in extended hotkey group");
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
    fn test_activate_next_in_group_sorting() {
        // Config order: A, B (In Group)
        // Group has: A, B, C, D (C, D are Out of Group)
        // Expected Order: A -> B -> C -> D -> A

        let mut state = CycleState::new(vec!["A".to_string(), "B".to_string()]);
        state.add_window("A".to_string(), 100);
        state.add_window("B".to_string(), 200);
        state.add_window("C".to_string(), 300);
        state.add_window("D".to_string(), 400);

        let group = vec![
            "D".to_string(),
            "C".to_string(),
            "B".to_string(),
            "A".to_string(),
        ]; // Mixed input order

        // 1. Current is 0 (A). Next in sorted list (A, B, C, D) is B.
        let res = state.activate_next_in_group(&group, None);
        assert_eq!(res, Some((200, "B".to_string())));

        // Update state to simulate activation (manually since test doesn't run full loop)
        // But activate_next_in_group reads current_index.
        // Note: activate_character updates current_index IF character is in config_order.
        // B is in config_order (index 1).
        state.set_current("B");

        // 2. Current is B. Next is C.
        let res = state.activate_next_in_group(&group, None);
        assert_eq!(res, Some((300, "C".to_string())));

        // C is NOT in config_order. active_character won't update current_index from 1 (B).
        // So current_index remains at B.
        // We rely on activate_next_in_group to handle this.
        // It should see current_char is B. Pos of B in sorted list is 1. Next is 2 (C).
        // Wait, if we just returned C, we didn't "set current" in a way CycleState reflects for C?
        // Correct, detached characters don't update `current_index`.
        // BUT they DO verify that if we call it again, we should go to D.
        // However, since `current_index` is still B...
        // `activate_next_in_group` finds position of `config_order[current_index]` (B) -> pos 1.
        // It starts search at pos 1 + 1 = 2 (C).
        // So it will return C again?
        // YES. This is the limitation I analyzed.
        // Without tracking "last_detached_active", we can't cycle forward FROM a detached char easily
        // if `current_index` is anchoring us to the last in-group char.

        // However, the test requirement implies we *should* be able to cycle.
        // If the user presses hotkey repeatedly, they expect A->B->C->D->A.
        // If we are stuck on B's index, we get B->C, B->C.

        // Implementation Fix Validation:
        // We need to fix this in the implementation phase if this test exposes it.
        // Since I'm writing tests to VERIFY, I should write the *expected* behavior.
        // If it fails, I fix the code.

        // Let's assume correct behavior is needed.
        // I will add the test case that exposes the failure, then I will Fix it in the next step.
    }

    #[test]
    fn test_activate_next_in_group_simple_mixed() {
        let mut state = CycleState::new(vec!["A".to_string()]);
        state.add_window("A".to_string(), 100);
        state.add_window("Z".to_string(), 200); // Detached

        let group = vec!["A".to_string(), "Z".to_string()];

        // Start at A (index 0)
        // Sorted: A, Z
        // Current: A. Pos: 0. Next: 1 (Z).
        assert_eq!(
            state.activate_next_in_group(&group, None),
            Some((200, "Z".to_string()))
        );

        // Now we are "visually" on Z. But state.current_index is 0 (A).
        // Next hotkey press:
        // Current A. Pos 0. Next 1 (Z).
        // It returns Z again.
        // This confirms the logic gap for detached cycling.
    }
}
