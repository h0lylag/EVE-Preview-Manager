//! Hotkey cycle state management
//!
//! Tracks active EVE windows and their cycle order for hotkey-based navigation.
//! Only characters listed in configured cycle groups are included in cycling.

use std::collections::{HashMap, HashSet};
use tracing::{debug, warn};
use x11rb::protocol::xproto::Window;

/// State for a single cycle group
#[derive(Debug, Clone)]
struct GroupState {
    order: Vec<String>,
    current_index: usize,
}

/// Tracks source windows and their live character names for cycle order
pub struct CycleState {
    /// Active cycle groups: group_name -> GroupState
    groups: HashMap<String, GroupState>,

    /// Currently focused active window (if any)
    /// Used to resolve starting position for cycling, especially for detached characters
    current_window: Option<Window>,

    /// Active windows: source_window_id -> live_character_name.
    /// Logged-out windows keep an empty live name, while remembered identity is
    /// resolved from SessionState by source window id when needed.
    active_windows: HashMap<Window, String>,

    /// Characters temporarily skipped from cycling
    skipped_characters: HashSet<String>,

    /// The name of the cycle group that was last active (used for reset logic)
    last_active_group: Option<String>,
}

impl CycleState {
    pub fn new(cycle_groups: Vec<crate::config::profile::CycleGroup>) -> Self {
        let mut groups = HashMap::new();
        for group in cycle_groups {
            groups.insert(
                group.name,
                GroupState {
                    order: group
                        .cycle_list
                        .iter()
                        .map(|slot| match slot {
                            crate::config::profile::CycleSlot::Eve(name) => name.clone(),
                            crate::config::profile::CycleSlot::Source(name) => name.clone(),
                        })
                        .collect(),
                    current_index: 0,
                },
            );
        }

        Self {
            groups,
            current_window: None,
            active_windows: HashMap::new(),
            skipped_characters: HashSet::new(),
            last_active_group: None,
        }
    }

    /// Register a new EVE window (called from CreateNotify)
    pub fn add_window(&mut self, character_name: String, window: Window) {
        debug!(character = %character_name, window = window, "Adding window for character");
        self.active_windows.insert(window, character_name);

        // Cycle commands filter this superset against the configured group order.
    }

    /// Remove window (called from DestroyNotify)
    pub fn remove_window(&mut self, window: Window) {
        if let Some(name) = self.active_windows.remove(&window) {
            debug!(character = %name, window = window, "Removing window for character");

            // If a tracked window disappeared, keep group indices in range.
            self.clamp_indices();

            // Clear current_window if it matches
            if self.current_window == Some(window) {
                self.current_window = None;
            }
        }
    }

    /// Update character name (called on login/logout)
    pub fn update_character(&mut self, window: Window, new_name: String) {
        self.add_window(new_name, window);
    }

    fn active_window_for_character(
        active_windows: &HashMap<Window, String>,
        character_name: &str,
    ) -> Option<Window> {
        active_windows
            .iter()
            .find_map(|(&window, live_name)| (live_name == character_name).then_some(window))
    }

    fn logged_out_window_for_character(
        active_windows: &HashMap<Window, String>,
        character_name: &str,
        logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Option<Window> {
        let map = logged_out_map?;

        active_windows
            .iter()
            .filter(|(_, live_name)| live_name.is_empty())
            .find_map(|(&window, _)| {
                map.get(&window)
                    .is_some_and(|last_char| last_char == character_name)
                    .then_some(window)
            })
    }

    fn character_for_window<'a>(
        &'a self,
        window: Window,
        logged_out_map: Option<&'a HashMap<Window, String>>,
    ) -> Option<&'a str> {
        let live_name = self.active_windows.get(&window)?;
        if !live_name.is_empty() {
            return Some(live_name.as_str());
        }

        logged_out_map.and_then(|map| map.get(&window).map(String::as_str))
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

    /// Move to next character in specified group (forward cycle hotkey)
    /// Returns (window, character_name) to activate, or None if no active characters
    ///
    /// # Parameters
    /// - `group_name`: Name of the cycle group to use
    /// - `logged_out_map`: Optional window→last_character mapping for including logged-out windows
    pub fn cycle_forward(
        &mut self,
        group_name: &str,
        logged_out_map: Option<&HashMap<Window, String>>,
        reset_on_switch: bool,
    ) -> Option<(Window, String)> {
        match self.groups.get_mut(group_name) {
            Some(group_state) => {
                // Reset index logic
                if reset_on_switch {
                    let group_changed = self.last_active_group.as_deref() != Some(group_name);
                    if group_changed {
                        debug!(
                            group = group_name,
                            "Switched to new cycle group with reset enabled - resetting index to prev"
                        );
                        group_state.current_index = group_state.order.len().saturating_sub(1); // Set to end so next increment moves to 0
                    }
                }
                self.last_active_group = Some(group_name.to_string());
                if self.active_windows.is_empty() && logged_out_map.is_none() {
                    warn!(
                        active_windows = self.active_windows.len(),
                        "No active windows to cycle"
                    );
                    return None;
                }

                if group_state.order.is_empty() {
                    warn!(
                        group = group_name,
                        "Cycle group order is empty - add characters to this group in settings"
                    );
                    return None;
                }

                let start_index = group_state.current_index;
                loop {
                    group_state.current_index =
                        (group_state.current_index + 1) % group_state.order.len();

                    let character_name = &group_state.order[group_state.current_index];

                    // Skip characters marked as skipped
                    if self.skipped_characters.contains(character_name) {
                        // Check termination condition (wrapped around to start)
                        if group_state.current_index == start_index {
                            warn!("All active characters in group are skipped");
                            return None;
                        }
                        continue;
                    }

                    // Check active windows first
                    if let Some(window) =
                        Self::active_window_for_character(&self.active_windows, character_name)
                    {
                        debug!(group = group_name, character = %character_name, index = group_state.current_index, "Cycling forward to logged-in character");
                        return Some((window, character_name.clone()));
                    }

                    // Check logged-out windows
                    if let Some(window) = Self::logged_out_window_for_character(
                        &self.active_windows,
                        character_name,
                        logged_out_map,
                    ) {
                        debug!(group = group_name, character = %character_name, index = group_state.current_index, window = window, "Cycling forward to logged-out character");
                        return Some((window, character_name.clone()));
                    }

                    // Wrapped around?
                    if group_state.current_index == start_index {
                        return None;
                    }
                }
            }
            None => {
                warn!(group = group_name, "Cycle group not found");
                None
            }
        }
    }

    /// Move to previous character in specified group (backward cycle hotkey)
    pub fn cycle_backward(
        &mut self,
        group_name: &str,
        logged_out_map: Option<&HashMap<Window, String>>,
        reset_on_switch: bool,
    ) -> Option<(Window, String)> {
        match self.groups.get_mut(group_name) {
            Some(group_state) => {
                // Reset index logic
                if reset_on_switch {
                    let group_changed = self.last_active_group.as_deref() != Some(group_name);
                    if group_changed {
                        debug!(
                            group = group_name,
                            "Switched to new cycle group with reset enabled - resetting index to 0"
                        );
                        group_state.current_index = 0; // Set to 0 so next decrement moves to len-1 (conceptually "backward from 0" means "last item")
                    }
                }
                self.last_active_group = Some(group_name.to_string());
                if self.active_windows.is_empty() && logged_out_map.is_none() {
                    return None;
                }

                if group_state.order.is_empty() {
                    return None;
                }

                let start_index = group_state.current_index;
                loop {
                    group_state.current_index = if group_state.current_index == 0 {
                        group_state.order.len() - 1
                    } else {
                        group_state.current_index - 1
                    };

                    let character_name = &group_state.order[group_state.current_index];

                    // Skip characters marked as skipped
                    if self.skipped_characters.contains(character_name) {
                        if group_state.current_index == start_index {
                            return None;
                        }
                        continue;
                    }

                    if let Some(window) =
                        Self::active_window_for_character(&self.active_windows, character_name)
                    {
                        debug!(group = group_name, character = %character_name, index = group_state.current_index, "Cycling backward to logged-in character");
                        return Some((window, character_name.clone()));
                    }

                    if let Some(window) = Self::logged_out_window_for_character(
                        &self.active_windows,
                        character_name,
                        logged_out_map,
                    ) {
                        debug!(group = group_name, character = %character_name, index = group_state.current_index, window = window, "Cycling backward to logged-out character");
                        return Some((window, character_name.clone()));
                    }

                    if group_state.current_index == start_index {
                        return None;
                    }
                }
            }
            None => None,
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
        if let Some(window) =
            Self::active_window_for_character(&self.active_windows, character_name)
        {
            debug!(character = %character_name, window = window, "Activating logged-in character via per-character hotkey");

            // Update current_index in ALL groups that contain this character
            // This keeps the cycle position "active" on the character we just jumped to
            for group in self.groups.values_mut() {
                if let Some(index) = group.order.iter().position(|c| c == character_name) {
                    group.current_index = index;
                }
            }

            return Some((window, character_name));
        }

        // If enabled, check for logged-out windows with this character's last identity
        if let Some(window) = Self::logged_out_window_for_character(
            &self.active_windows,
            character_name,
            logged_out_map,
        ) {
            debug!(character = %character_name, window = window, "Activating logged-out character via per-character hotkey");

            // Update current_index in ALL groups
            for group in self.groups.values_mut() {
                if let Some(index) = group.order.iter().position(|c| c == character_name) {
                    group.current_index = index;
                }
            }

            return Some((window, character_name));
        }

        // Character not found or not active
        debug!(character = %character_name, "Character not active, cannot activate");
        None
    }

    fn set_current_group_index(&mut self, character_name: &str) -> bool {
        if character_name.is_empty() {
            return false;
        }

        let mut found_in_any_group = false;

        for group in self.groups.values_mut() {
            if let Some(index) = group.order.iter().position(|c| c == character_name) {
                group.current_index = index;
                found_in_any_group = true;
            }
        }

        if found_in_any_group {
            debug!(character = %character_name, "Updated current cycle group index");
            true
        } else {
            false
        }
    }

    /// Set current cycle position by exact window ID, with an optional character name for
    /// logged-out windows whose live thumbnail name is empty.
    pub fn set_current_by_window_with_character(
        &mut self,
        window: Window,
        character_name: Option<&str>,
    ) -> bool {
        self.current_window = Some(window);

        if let Some(active_name) = self.active_windows.get(&window).cloned() {
            if !active_name.is_empty() {
                self.set_current_group_index(&active_name);
                return true;
            }

            if let Some(name) = character_name {
                self.set_current_group_index(name);
            }
            return true;
        }

        character_name
            .map(|name| self.set_current_group_index(name))
            .unwrap_or(false)
    }

    /// Clamp index to valid range in all groups after removing characters
    fn clamp_indices(&mut self) {
        for group in self.groups.values_mut() {
            if !group.order.is_empty() && group.current_index >= group.order.len() {
                group.current_index = 0;
            }
        }
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
        // Prefer Default-group order when available, then append remaining shared-hotkey
        // candidates alphabetically for stable cycling.
        let mut in_group_indices: Vec<(usize, &String)> = Vec::new();
        let mut out_of_group: Vec<&String> = Vec::new();

        for name in group {
            let in_default = self
                .groups
                .get("Default")
                .and_then(|g| g.order.iter().position(|c| c == name));

            if let Some(idx) = in_default {
                in_group_indices.push((idx, name));
            } else {
                out_of_group.push(name);
            }
        }

        in_group_indices.sort_by_key(|(idx, _)| *idx);
        out_of_group.sort();

        let sorted_candidates: Vec<&String> = in_group_indices
            .into_iter()
            .map(|(_, name)| name)
            .chain(out_of_group)
            .collect();

        if sorted_candidates.is_empty() {
            debug!("No characters found in hotkey group");
            return None;
        }

        // Start from the exact current window when possible, so logged-out clients
        // with remembered identities cycle from the clicked or focused source.
        let start_pos = if let Some(curr_win) = self.current_window
            && let Some(curr_char) = self.character_for_window(curr_win, logged_out_map)
            && let Some(pos) = sorted_candidates.iter().position(|&c| c == curr_char)
        {
            pos
        } else if let Some(default_group) = self.groups.get("Default")
            && !default_group.order.is_empty()
        {
            // Fallback to "Default" group index logic if available
            let current_char_name = &default_group.order[default_group.current_index];
            if let Some(pos) = sorted_candidates
                .iter()
                .position(|&c| c == current_char_name)
            {
                pos
            } else {
                sorted_candidates.len().saturating_sub(1)
            }
        } else {
            sorted_candidates.len().saturating_sub(1)
        };

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

    /// Get the window ID of the currently focused window (if known)
    pub fn get_current_window(&self) -> Option<Window> {
        self.current_window
    }

    /// Get all active source windows known to cycle state with their live names.
    pub fn get_active_windows(&self) -> &HashMap<Window, String> {
        &self.active_windows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_group(name: &str, characters: &[&str]) -> crate::config::profile::CycleGroup {
        use crate::config::profile::{CycleGroup, CycleSlot};

        CycleGroup {
            name: name.to_string(),
            cycle_list: characters
                .iter()
                .map(|character| CycleSlot::Eve((*character).to_string()))
                .collect(),
            hotkey_forward: None,
            hotkey_backward: None,
        }
    }

    #[test]
    fn test_cycle_forward_multi_group() {
        let group1 = test_group("G1", &["A", "B"]);
        let mut state = CycleState::new(vec![group1]);
        state.add_window("A".to_string(), 100);
        state.add_window("B".to_string(), 200);

        assert_eq!(
            state.cycle_forward("G1", None, false),
            Some((200, "B".to_string()))
        );
    }

    #[test]
    fn test_cycle_reset_on_group_switch() {
        use crate::config::profile::CycleGroup;
        let group1 = CycleGroup {
            name: "G1".to_string(),
            cycle_list: vec![
                crate::config::profile::CycleSlot::Eve("A".to_string()),
                crate::config::profile::CycleSlot::Eve("B".to_string()),
                crate::config::profile::CycleSlot::Eve("C".to_string()),
            ],
            hotkey_forward: None,
            hotkey_backward: None,
        };
        let group2 = CycleGroup {
            name: "G2".to_string(),
            cycle_list: vec![
                crate::config::profile::CycleSlot::Eve("D".to_string()),
                crate::config::profile::CycleSlot::Eve("E".to_string()),
            ],
            hotkey_forward: None,
            hotkey_backward: None,
        };

        let mut state = CycleState::new(vec![group1, group2]);
        state.add_window("A".to_string(), 100);
        state.add_window("B".to_string(), 200);
        state.add_window("C".to_string(), 300);
        state.add_window("D".to_string(), 400);
        state.add_window("E".to_string(), 500);

        // Cycle G1: Start (0->A), Forward (1->B)
        // Note: New state index is 0. cycle_forward increments -> 1 (B)
        // Wait, cycle_forward logic: current_index = (current_index + 1) % len.
        // Initial current_index is 0.
        // 1. cycle_forward -> index 1 ("B"). Returns B.
        assert_eq!(
            state.cycle_forward("G1", None, false),
            Some((200, "B".to_string()))
        );
        // Current index is 1.

        // Cycle G2: Start (0->D), Forward (1->E).
        // Switch to G2.
        assert_eq!(
            state.cycle_forward("G2", None, false),
            Some((500, "E".to_string()))
        );

        // Switch back to G1 with reset=false. Should resume at next index (2->C).
        assert_eq!(
            state.cycle_forward("G1", None, false),
            Some((300, "C".to_string()))
        );

        // Switch to G2 again.
        assert_eq!(
            state.cycle_forward("G2", None, false),
            Some((400, "D".to_string()))
        );

        // Switch back to G1 with reset=true. Should reset to 0 ("A")?
        // Logic: if reset, set current_index = len - 1.
        // Then cycle_forward increments -> 0.
        // So it should return index 0 ("A").
        assert_eq!(
            state.cycle_forward("G1", None, true),
            Some((100, "A".to_string()))
        );
    }

    #[test]
    fn test_logged_out_click_preserves_clicked_window() {
        use std::collections::HashMap;

        let group = test_group("G1", &["A", "B"]);
        let mut state = CycleState::new(vec![group]);

        // Multiple logged-out thumbnails all have an empty live character name,
        // so clicks must anchor current_window to the exact source window.
        state.add_window("".to_string(), 111);
        state.add_window("".to_string(), 222);

        assert!(state.set_current_by_window_with_character(111, Some("A")));
        assert_eq!(state.get_current_window(), Some(111));
        assert_eq!(state.groups.get("G1").unwrap().current_index, 0);

        let logged_out = HashMap::from([(111, "A".to_string()), (222, "B".to_string())]);
        assert_eq!(
            state.cycle_forward("G1", Some(&logged_out), false),
            Some((222, "B".to_string()))
        );
    }

    #[test]
    fn test_multiple_logged_out_windows_preserve_source_ids() {
        let mut state = CycleState::new(vec![test_group("G1", &["A", "B"])]);

        state.add_window("".to_string(), 111);
        state.add_window("".to_string(), 222);

        assert_eq!(state.get_active_windows().len(), 2);
        assert_eq!(
            state.get_active_windows().get(&111).map(String::as_str),
            Some("")
        );
        assert_eq!(
            state.get_active_windows().get(&222).map(String::as_str),
            Some("")
        );
    }

    #[test]
    fn test_remove_logged_out_window_removes_exact_source_and_current() {
        let mut state = CycleState::new(vec![test_group("G1", &["A", "B"])]);
        state.add_window("".to_string(), 111);
        state.add_window("".to_string(), 222);

        assert!(state.set_current_by_window_with_character(111, Some("A")));

        state.remove_window(222);
        assert!(state.get_active_windows().contains_key(&111));
        assert!(!state.get_active_windows().contains_key(&222));
        assert_eq!(state.get_current_window(), Some(111));

        state.remove_window(111);
        assert_eq!(state.get_current_window(), None);
    }

    #[test]
    fn test_cycle_forward_uses_remembered_logged_out_identity() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &["A", "B"])]);
        state.add_window("".to_string(), 111);
        state.add_window("".to_string(), 222);

        let logged_out = HashMap::from([(111, "A".to_string()), (222, "B".to_string())]);

        assert_eq!(
            state.cycle_forward("G1", Some(&logged_out), false),
            Some((222, "B".to_string()))
        );
    }

    #[test]
    fn test_anonymous_logged_out_window_is_not_cycle_candidate() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &["A"])]);
        state.add_window("".to_string(), 111);

        assert!(state.set_current_by_window_with_character(111, None));
        assert_eq!(state.get_current_window(), Some(111));
        assert_eq!(
            state.cycle_forward("G1", Some(&HashMap::new()), false),
            None
        );
    }

    #[test]
    fn test_logged_out_cycling_is_disabled_without_logged_out_map() {
        let mut state = CycleState::new(vec![test_group("G1", &["A"])]);
        state.add_window("".to_string(), 111);

        assert_eq!(state.cycle_forward("G1", None, false), None);
        assert_eq!(state.activate_character("A", None), None);
    }

    #[test]
    fn test_shared_hotkey_starts_from_remembered_logged_out_current_window() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("Default", &["A", "B"])]);
        state.add_window("".to_string(), 111);
        state.add_window("".to_string(), 222);

        let logged_out = HashMap::from([(111, "A".to_string()), (222, "B".to_string())]);
        assert!(state.set_current_by_window_with_character(111, Some("A")));

        assert_eq!(
            state.activate_next_in_group(&["A".to_string(), "B".to_string()], Some(&logged_out)),
            Some((222, "B".to_string()))
        );
    }
}
