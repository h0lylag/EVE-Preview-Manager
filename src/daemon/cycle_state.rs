//! Hotkey cycle state management
//!
//! Tracks active source windows and their cycle order for hotkey-based navigation.
//! Normal cycling follows configured cycle groups. Optional logged-out modes can
//! include remembered logged-out clients or unidentified login-screen clients.

use std::collections::{HashMap, HashSet};
use tracing::{debug, warn};
use x11rb::protocol::xproto::Window;

use crate::common::types::SourceIdentity;

/// State for a single cycle group
#[derive(Debug, Clone)]
struct GroupState {
    order: Vec<SourceIdentity>,
    current_index: usize,
}

#[derive(Debug, Clone, Copy)]
enum CycleDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CycleCandidate {
    Named(SourceIdentity),
    Unidentified(Window),
}

pub type CycleActivation = (Window, Option<SourceIdentity>);

/// Tracks source windows and their typed identities for cycle order.
pub struct CycleState {
    /// Active cycle groups: group_name -> GroupState
    groups: HashMap<String, GroupState>,

    /// Currently focused active window (if any).
    /// Used to resolve starting position for cycling, especially for detached sources.
    current_window: Option<Window>,

    /// Active windows: source_window_id -> live typed identity.
    /// Logged-out EVE windows keep None, while remembered identity is
    /// resolved from SessionState by source window id when needed.
    active_windows: HashMap<Window, Option<SourceIdentity>>,

    /// Source windows in first-detected order for unidentified logged-out cycling.
    active_window_order: Vec<Window>,

    /// Sources temporarily skipped from cycling.
    skipped_sources: HashSet<SourceIdentity>,

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
                            crate::config::profile::CycleSlot::Eve(name) => {
                                SourceIdentity::eve(name.clone())
                            }
                            crate::config::profile::CycleSlot::Source(name) => {
                                SourceIdentity::custom(name.clone())
                            }
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
            active_window_order: Vec::new(),
            skipped_sources: HashSet::new(),
            last_active_group: None,
        }
    }

    /// Register a source window (called from CreateNotify / initial scan).
    pub fn add_window(&mut self, identity: Option<SourceIdentity>, window: Window) {
        debug!(identity = ?identity, window = window, "Adding window for source");
        if !self.active_windows.contains_key(&window) {
            self.active_window_order.push(window);
        }
        self.active_windows.insert(window, identity);

        // Cycle commands filter this superset against configured groups, remembered
        // logged-out identities, or unidentified login-screen candidates.
    }

    /// Remove window (called from DestroyNotify)
    pub fn remove_window(&mut self, window: Window) {
        if let Some(identity) = self.active_windows.remove(&window) {
            debug!(identity = ?identity, window = window, "Removing window for source");
            self.active_window_order
                .retain(|tracked| *tracked != window);

            // If a tracked window disappeared, keep group indices in range.
            self.clamp_indices();

            // Clear current_window if it matches
            if self.current_window == Some(window) {
                self.current_window = None;
            }
        }
    }

    /// Update an EVE client's live character name (called on login/logout).
    pub fn update_character(&mut self, window: Window, new_name: String) {
        let identity = (!new_name.is_empty()).then(|| SourceIdentity::eve(new_name));
        self.add_window(identity, window);
    }

    fn active_window_for_identity(
        active_windows: &HashMap<Window, Option<SourceIdentity>>,
        identity: &SourceIdentity,
    ) -> Option<Window> {
        active_windows
            .iter()
            .find_map(|(&window, active)| (active.as_ref() == Some(identity)).then_some(window))
    }

    fn unidentified_logged_out_windows(
        active_windows: &HashMap<Window, Option<SourceIdentity>>,
        active_window_order: &[Window],
        logged_out_map: &HashMap<Window, String>,
    ) -> Vec<Window> {
        active_window_order
            .iter()
            .copied()
            .filter(|window| {
                active_windows.get(window).is_some_and(Option::is_none)
                    && !logged_out_map.contains_key(window)
            })
            .collect()
    }

    fn cycle_candidates(
        order: &[SourceIdentity],
        active_windows: &HashMap<Window, Option<SourceIdentity>>,
        active_window_order: &[Window],
        unidentified_logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Vec<CycleCandidate> {
        let mut candidates: Vec<CycleCandidate> =
            order.iter().cloned().map(CycleCandidate::Named).collect();

        if let Some(map) = unidentified_logged_out_map {
            candidates.extend(
                Self::unidentified_logged_out_windows(active_windows, active_window_order, map)
                    .into_iter()
                    .map(CycleCandidate::Unidentified),
            );
        }

        candidates
    }

    fn logged_out_window_for_identity(
        active_windows: &HashMap<Window, Option<SourceIdentity>>,
        identity: &SourceIdentity,
        logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Option<Window> {
        if !identity.kind.is_eve() || identity.name.is_empty() {
            return None;
        }

        let map = logged_out_map?;

        active_windows
            .iter()
            .filter(|(_, live_identity)| live_identity.is_none())
            .find_map(|(&window, _)| {
                map.get(&window)
                    .is_some_and(|last_char| last_char == &identity.name)
                    .then_some(window)
            })
    }

    fn identity_for_window(
        &self,
        window: Window,
        logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Option<SourceIdentity> {
        match self.active_windows.get(&window)? {
            Some(identity) => Some(identity.clone()),
            None => logged_out_map
                .and_then(|map| map.get(&window))
                .map(|name| SourceIdentity::eve(name.clone())),
        }
    }

    /// Toggle skip status for a source.
    /// Returns new skipped state (true = skipped, false = active)
    pub fn toggle_skip(&mut self, identity: &SourceIdentity) -> bool {
        if self.skipped_sources.contains(identity) {
            debug!(identity = ?identity, "Unskipping source");
            self.skipped_sources.remove(identity);
            false
        } else {
            debug!(identity = ?identity, "Skipping source");
            self.skipped_sources.insert(identity.clone());
            true
        }
    }

    /// Check if a source is currently skipped.
    pub fn is_skipped(&self, identity: Option<&SourceIdentity>) -> bool {
        identity.is_some_and(|identity| self.skipped_sources.contains(identity))
    }

    /// Move to the next source in the specified group (forward cycle hotkey).
    /// Returns the window and typed identity to activate, or None if no source is active.
    ///
    /// # Parameters
    /// - `group_name`: Name of the cycle group to use
    /// - `logged_out_map`: Optional window→last_character mapping for including logged-out windows
    pub fn cycle_forward(
        &mut self,
        group_name: &str,
        logged_out_map: Option<&HashMap<Window, String>>,
        reset_on_switch: bool,
    ) -> Option<CycleActivation> {
        self.cycle_group(
            group_name,
            logged_out_map,
            None,
            reset_on_switch,
            CycleDirection::Forward,
        )
    }

    /// Move to next configured group entry, then any unidentified logged-out
    /// clients appended in discovery order.
    pub fn cycle_forward_with_unidentified(
        &mut self,
        group_name: &str,
        logged_out_map: Option<&HashMap<Window, String>>,
        unidentified_logged_out_map: &HashMap<Window, String>,
        reset_on_switch: bool,
    ) -> Option<CycleActivation> {
        self.cycle_group(
            group_name,
            logged_out_map,
            Some(unidentified_logged_out_map),
            reset_on_switch,
            CycleDirection::Forward,
        )
    }

    /// Move to the previous source in the specified group (backward cycle hotkey).
    pub fn cycle_backward(
        &mut self,
        group_name: &str,
        logged_out_map: Option<&HashMap<Window, String>>,
        reset_on_switch: bool,
    ) -> Option<CycleActivation> {
        self.cycle_group(
            group_name,
            logged_out_map,
            None,
            reset_on_switch,
            CycleDirection::Backward,
        )
    }

    /// Move to previous configured group entry, including unidentified
    /// logged-out clients appended after the configured entries.
    pub fn cycle_backward_with_unidentified(
        &mut self,
        group_name: &str,
        logged_out_map: Option<&HashMap<Window, String>>,
        unidentified_logged_out_map: &HashMap<Window, String>,
        reset_on_switch: bool,
    ) -> Option<CycleActivation> {
        self.cycle_group(
            group_name,
            logged_out_map,
            Some(unidentified_logged_out_map),
            reset_on_switch,
            CycleDirection::Backward,
        )
    }

    fn cycle_group(
        &mut self,
        group_name: &str,
        logged_out_map: Option<&HashMap<Window, String>>,
        unidentified_logged_out_map: Option<&HashMap<Window, String>>,
        reset_on_switch: bool,
        direction: CycleDirection,
    ) -> Option<CycleActivation> {
        let group_order = match self.groups.get(group_name) {
            Some(group) => group.order.clone(),
            None => {
                warn!(group = group_name, "Cycle group not found");
                return None;
            }
        };

        let candidates = Self::cycle_candidates(
            &group_order,
            &self.active_windows,
            &self.active_window_order,
            unidentified_logged_out_map,
        );

        if candidates.is_empty() {
            if unidentified_logged_out_map.is_some() {
                warn!(
                    group = group_name,
                    "Cycle group has no configured entries or unidentified logged-out clients"
                );
            } else {
                warn!(
                    group = group_name,
                    "Cycle group order is empty - add sources to this group in settings"
                );
            }
            return None;
        }

        if self.active_windows.is_empty() && logged_out_map.is_none() {
            warn!(
                active_windows = self.active_windows.len(),
                "No active windows to cycle"
            );
            return None;
        }

        let group_state = self
            .groups
            .get_mut(group_name)
            .expect("cycle group was checked above");

        if group_state.current_index >= candidates.len() {
            group_state.current_index = 0;
        }

        if reset_on_switch {
            let group_changed = self.last_active_group.as_deref() != Some(group_name);
            if group_changed {
                match direction {
                    CycleDirection::Forward => {
                        debug!(
                            group = group_name,
                            "Switched to new cycle group with reset enabled - resetting index to prev"
                        );
                        group_state.current_index = candidates.len().saturating_sub(1);
                    }
                    CycleDirection::Backward => {
                        debug!(
                            group = group_name,
                            "Switched to new cycle group with reset enabled - resetting index to 0"
                        );
                        group_state.current_index = 0;
                    }
                }
            }
        }
        self.last_active_group = Some(group_name.to_string());

        let start_index = group_state.current_index;
        loop {
            group_state.current_index = match direction {
                CycleDirection::Forward => (group_state.current_index + 1) % candidates.len(),
                CycleDirection::Backward => {
                    if group_state.current_index == 0 {
                        candidates.len() - 1
                    } else {
                        group_state.current_index - 1
                    }
                }
            };

            match &candidates[group_state.current_index] {
                CycleCandidate::Named(identity) => {
                    if self.skipped_sources.contains(identity) {
                        if group_state.current_index == start_index {
                            warn!("All active sources in group are skipped");
                            return None;
                        }
                        continue;
                    }

                    if let Some(window) =
                        Self::active_window_for_identity(&self.active_windows, identity)
                    {
                        debug!(group = group_name, identity = ?identity, index = group_state.current_index, direction = ?direction, "Cycling to active source");
                        return Some((window, Some(identity.clone())));
                    }

                    if let Some(window) = Self::logged_out_window_for_identity(
                        &self.active_windows,
                        identity,
                        logged_out_map,
                    ) {
                        debug!(group = group_name, identity = ?identity, index = group_state.current_index, window = window, direction = ?direction, "Cycling to logged-out EVE character");
                        return Some((window, Some(identity.clone())));
                    }
                }
                CycleCandidate::Unidentified(window) => {
                    debug!(group = group_name, window = window, index = group_state.current_index, direction = ?direction, "Cycling to unidentified logged-out client");
                    return Some((*window, None));
                }
            }

            if group_state.current_index == start_index {
                return None;
            }
        }
    }

    /// Activate specific source by typed identity.
    /// Returns target window and identity to activate, or None if not active.
    /// Updates current_index to maintain consistency with cycle state
    pub fn activate_identity(
        &mut self,
        identity: &SourceIdentity,
        logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Option<CycleActivation> {
        if let Some(window) = Self::active_window_for_identity(&self.active_windows, identity) {
            debug!(identity = ?identity, window = window, "Activating source via direct hotkey");

            for group in self.groups.values_mut() {
                if let Some(index) = group
                    .order
                    .iter()
                    .position(|candidate| candidate == identity)
                {
                    group.current_index = index;
                }
            }

            return Some((window, Some(identity.clone())));
        }

        if let Some(window) =
            Self::logged_out_window_for_identity(&self.active_windows, identity, logged_out_map)
        {
            debug!(identity = ?identity, window = window, "Activating logged-out EVE character via direct hotkey");

            for group in self.groups.values_mut() {
                if let Some(index) = group
                    .order
                    .iter()
                    .position(|candidate| candidate == identity)
                {
                    group.current_index = index;
                }
            }

            return Some((window, Some(identity.clone())));
        }

        debug!(identity = ?identity, "Source not active, cannot activate");
        None
    }

    fn set_current_group_index(&mut self, identity: &SourceIdentity) -> bool {
        if identity.name.is_empty() {
            return false;
        }

        let mut found_in_any_group = false;

        for group in self.groups.values_mut() {
            if let Some(index) = group
                .order
                .iter()
                .position(|candidate| candidate == identity)
            {
                group.current_index = index;
                found_in_any_group = true;
            }
        }

        if found_in_any_group {
            debug!(identity = ?identity, "Updated current cycle group index");
            true
        } else {
            false
        }
    }

    pub fn cycle_unidentified_logged_out_forward(
        &mut self,
        logged_out_map: &HashMap<Window, String>,
    ) -> Option<CycleActivation> {
        self.cycle_unidentified_logged_out(logged_out_map, CycleDirection::Forward)
    }

    pub fn cycle_unidentified_logged_out_backward(
        &mut self,
        logged_out_map: &HashMap<Window, String>,
    ) -> Option<CycleActivation> {
        self.cycle_unidentified_logged_out(logged_out_map, CycleDirection::Backward)
    }

    fn cycle_unidentified_logged_out(
        &mut self,
        logged_out_map: &HashMap<Window, String>,
        direction: CycleDirection,
    ) -> Option<CycleActivation> {
        let candidates = Self::unidentified_logged_out_windows(
            &self.active_windows,
            &self.active_window_order,
            logged_out_map,
        );

        if candidates.is_empty() {
            warn!("No unidentified logged-out clients to cycle");
            return None;
        }

        let start_pos = if let Some(current_window) = self.current_window {
            candidates
                .iter()
                .position(|window| *window == current_window)
                .unwrap_or_else(|| match direction {
                    CycleDirection::Forward => candidates.len().saturating_sub(1),
                    CycleDirection::Backward => 0,
                })
        } else {
            match direction {
                CycleDirection::Forward => candidates.len().saturating_sub(1),
                CycleDirection::Backward => 0,
            }
        };

        let next_pos = match direction {
            CycleDirection::Forward => (start_pos + 1) % candidates.len(),
            CycleDirection::Backward => {
                if start_pos == 0 {
                    candidates.len() - 1
                } else {
                    start_pos - 1
                }
            }
        };

        let window = candidates[next_pos];
        debug!(window = window, direction = ?direction, "Cycling to unidentified logged-out client");
        Some((window, None))
    }

    /// Set current cycle position by exact window ID, with an optional typed identity for
    /// logged-out EVE windows whose live thumbnail name is empty.
    pub fn set_current_by_window_with_identity(
        &mut self,
        window: Window,
        identity: Option<&SourceIdentity>,
    ) -> bool {
        self.current_window = Some(window);

        if let Some(active_identity) = self.active_windows.get(&window).cloned() {
            if let Some(active_identity) = active_identity {
                self.set_current_group_index(&active_identity);
            } else if let Some(identity) = identity {
                self.set_current_group_index(identity);
            }
            return true;
        }

        identity
            .map(|identity| self.set_current_group_index(identity))
            .unwrap_or(false)
    }

    /// Clamp index to valid range in all groups after removing sources.
    fn clamp_indices(&mut self) {
        for group in self.groups.values_mut() {
            if !group.order.is_empty() && group.current_index >= group.order.len() {
                group.current_index = 0;
            }
        }
    }

    /// Cycles to the next available source within a specific subgroup.
    /// Used for shared hotkeys (e.g. F1 bound to multiple sources) to toggle between them.
    ///
    /// # Sorting Logic
    /// 1. Sources present in the Default cycle group are prioritized by that group order.
    /// 2. Sources outside the Default group are appended in stable name/kind order.
    pub fn activate_next_in_group(
        &mut self,
        group: &[SourceIdentity],
        logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Option<CycleActivation> {
        // Prefer Default-group order when available, then append remaining shared-hotkey
        // candidates alphabetically for stable cycling.
        let mut in_group_indices: Vec<(usize, &SourceIdentity)> = Vec::new();
        let mut out_of_group: Vec<&SourceIdentity> = Vec::new();

        for identity in group {
            let in_default = self
                .groups
                .get("Default")
                .and_then(|g| g.order.iter().position(|candidate| candidate == identity));

            if let Some(idx) = in_default {
                in_group_indices.push((idx, identity));
            } else {
                out_of_group.push(identity);
            }
        }

        in_group_indices.sort_by_key(|(idx, _)| *idx);
        out_of_group.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| (left.kind as u8).cmp(&(right.kind as u8)))
        });

        let sorted_candidates: Vec<&SourceIdentity> = in_group_indices
            .into_iter()
            .map(|(_, identity)| identity)
            .chain(out_of_group)
            .collect();

        if sorted_candidates.is_empty() {
            debug!("No sources found in hotkey group");
            return None;
        }

        // Start from the exact current window when possible, so remembered
        // logged-out EVE clients cycle from the clicked or focused source.
        let start_pos = if let Some(curr_win) = self.current_window
            && let Some(curr_identity) = self.identity_for_window(curr_win, logged_out_map)
            && let Some(pos) = sorted_candidates
                .iter()
                .position(|candidate| **candidate == curr_identity)
        {
            pos
        } else if let Some(default_group) = self.groups.get("Default")
            && !default_group.order.is_empty()
        {
            // Fallback to "Default" group index logic if available
            let current_identity = &default_group.order[default_group.current_index];
            if let Some(pos) = sorted_candidates
                .iter()
                .position(|candidate| *candidate == current_identity)
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
            let identity = sorted_candidates[idx];

            // Respect skipped status
            if self.skipped_sources.contains(identity) {
                continue;
            }

            if let Some((window, identity)) = self.activate_identity(identity, logged_out_map) {
                debug!(identity = ?identity, "Activated next in group (advanced)");
                return Some((window, identity));
            }
        }

        debug!("No active sources found in extended hotkey group");
        None
    }

    /// Get the window ID of the currently focused window (if known)
    pub fn get_current_window(&self) -> Option<Window> {
        self.current_window
    }

    /// Get all active source windows known to cycle state.
    pub fn get_active_windows(&self) -> &HashMap<Window, Option<SourceIdentity>> {
        &self.active_windows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::{CycleGroup, CycleSlot};

    fn test_group(name: &str, characters: &[&str]) -> crate::config::profile::CycleGroup {
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

    fn mixed_group(name: &str, slots: Vec<CycleSlot>) -> CycleGroup {
        CycleGroup {
            name: name.to_string(),
            cycle_list: slots,
            hotkey_forward: None,
            hotkey_backward: None,
        }
    }

    fn add_eve(state: &mut CycleState, name: &str, window: Window) {
        state.add_window(Some(SourceIdentity::eve(name.to_string())), window);
    }

    fn add_source(state: &mut CycleState, name: &str, window: Window) {
        state.add_window(Some(SourceIdentity::custom(name.to_string())), window);
    }

    fn add_logged_out(state: &mut CycleState, window: Window) {
        state.add_window(None, window);
    }

    fn eve_activation(window: Window, name: &str) -> Option<CycleActivation> {
        Some((window, Some(SourceIdentity::eve(name.to_string()))))
    }

    fn source_activation(window: Window, name: &str) -> Option<CycleActivation> {
        Some((window, Some(SourceIdentity::custom(name.to_string()))))
    }

    fn unidentified_activation(window: Window) -> Option<CycleActivation> {
        Some((window, None))
    }

    #[test]
    fn test_cycle_forward_multi_group() {
        let group1 = test_group("G1", &["A", "B"]);
        let mut state = CycleState::new(vec![group1]);
        add_eve(&mut state, "A", 100);
        add_eve(&mut state, "B", 200);

        assert_eq!(
            state.cycle_forward("G1", None, false),
            eve_activation(200, "B")
        );
    }

    #[test]
    fn test_cycle_reset_on_group_switch() {
        let group1 = test_group("G1", &["A", "B", "C"]);
        let group2 = test_group("G2", &["D", "E"]);

        let mut state = CycleState::new(vec![group1, group2]);
        add_eve(&mut state, "A", 100);
        add_eve(&mut state, "B", 200);
        add_eve(&mut state, "C", 300);
        add_eve(&mut state, "D", 400);
        add_eve(&mut state, "E", 500);

        assert_eq!(
            state.cycle_forward("G1", None, false),
            eve_activation(200, "B")
        );
        assert_eq!(
            state.cycle_forward("G2", None, false),
            eve_activation(500, "E")
        );
        assert_eq!(
            state.cycle_forward("G1", None, false),
            eve_activation(300, "C")
        );
        assert_eq!(
            state.cycle_forward("G2", None, false),
            eve_activation(400, "D")
        );
        assert_eq!(
            state.cycle_forward("G1", None, true),
            eve_activation(100, "A")
        );
    }

    #[test]
    fn test_logged_out_click_preserves_clicked_window() {
        use std::collections::HashMap;

        let group = test_group("G1", &["A", "B"]);
        let mut state = CycleState::new(vec![group]);

        // Multiple logged-out thumbnails all have an empty live character name,
        // so clicks must anchor current_window to the exact source window.
        add_logged_out(&mut state, 111);
        add_logged_out(&mut state, 222);

        let identity = SourceIdentity::eve("A".to_string());
        assert!(state.set_current_by_window_with_identity(111, Some(&identity)));
        assert_eq!(state.get_current_window(), Some(111));
        assert_eq!(state.groups.get("G1").unwrap().current_index, 0);

        let logged_out = HashMap::from([(111, "A".to_string()), (222, "B".to_string())]);
        assert_eq!(
            state.cycle_forward("G1", Some(&logged_out), false),
            eve_activation(222, "B")
        );
    }

    #[test]
    fn test_multiple_logged_out_windows_preserve_source_ids() {
        let mut state = CycleState::new(vec![test_group("G1", &["A", "B"])]);

        add_logged_out(&mut state, 111);
        add_logged_out(&mut state, 222);

        assert_eq!(state.get_active_windows().len(), 2);
        assert_eq!(state.get_active_windows().get(&111), Some(&None));
        assert_eq!(state.get_active_windows().get(&222), Some(&None));
    }

    #[test]
    fn test_remove_logged_out_window_removes_exact_source_and_current() {
        let mut state = CycleState::new(vec![test_group("G1", &["A", "B"])]);
        add_logged_out(&mut state, 111);
        add_logged_out(&mut state, 222);

        let identity = SourceIdentity::eve("A".to_string());
        assert!(state.set_current_by_window_with_identity(111, Some(&identity)));

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
        add_logged_out(&mut state, 111);
        add_logged_out(&mut state, 222);

        let logged_out = HashMap::from([(111, "A".to_string()), (222, "B".to_string())]);

        assert_eq!(
            state.cycle_forward("G1", Some(&logged_out), false),
            eve_activation(222, "B")
        );
    }

    #[test]
    fn test_unidentified_logged_out_window_is_not_cycle_candidate() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &["A"])]);
        add_logged_out(&mut state, 111);

        assert!(state.set_current_by_window_with_identity(111, None));
        assert_eq!(state.get_current_window(), Some(111));
        assert_eq!(
            state.cycle_forward("G1", Some(&HashMap::new()), false),
            None
        );
    }

    #[test]
    fn test_logged_out_cycling_is_disabled_without_logged_out_map() {
        let mut state = CycleState::new(vec![test_group("G1", &["A"])]);
        add_logged_out(&mut state, 111);

        assert_eq!(state.cycle_forward("G1", None, false), None);
        assert_eq!(
            state.activate_identity(&SourceIdentity::eve("A".to_string()), None),
            None
        );
    }

    #[test]
    fn test_unidentified_logged_out_cycle_uses_discovery_order() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &[])]);
        add_logged_out(&mut state, 111);
        add_logged_out(&mut state, 222);

        let logged_out = HashMap::new();

        assert_eq!(
            state.cycle_unidentified_logged_out_forward(&logged_out),
            unidentified_activation(111)
        );

        assert!(state.set_current_by_window_with_identity(111, None));
        assert_eq!(
            state.cycle_unidentified_logged_out_forward(&logged_out),
            unidentified_activation(222)
        );
    }

    #[test]
    fn test_unidentified_logged_out_backward_starts_from_current_source() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &[])]);
        add_logged_out(&mut state, 111);
        add_logged_out(&mut state, 222);

        assert!(state.set_current_by_window_with_identity(111, None));
        assert_eq!(
            state.cycle_unidentified_logged_out_backward(&HashMap::new()),
            unidentified_activation(222)
        );
    }

    #[test]
    fn test_identified_logged_out_window_is_not_unidentified_candidate() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &[])]);
        add_logged_out(&mut state, 111);
        add_logged_out(&mut state, 222);

        let logged_out = HashMap::from([(111, "A".to_string())]);

        assert_eq!(
            state.cycle_unidentified_logged_out_forward(&logged_out),
            unidentified_activation(222)
        );
    }

    #[test]
    fn test_removed_unidentified_window_leaves_discovery_order() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &[])]);
        add_logged_out(&mut state, 111);
        add_logged_out(&mut state, 222);
        state.remove_window(111);

        assert_eq!(
            state.cycle_unidentified_logged_out_forward(&HashMap::new()),
            unidentified_activation(222)
        );
    }

    #[test]
    fn test_append_mode_cycles_group_entries_then_unidentified_clients() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &["A", "B"])]);
        add_eve(&mut state, "A", 100);
        add_eve(&mut state, "B", 200);
        add_logged_out(&mut state, 333);

        assert_eq!(
            state.cycle_forward_with_unidentified("G1", None, &HashMap::new(), false),
            eve_activation(200, "B")
        );
        assert_eq!(
            state.cycle_forward_with_unidentified("G1", None, &HashMap::new(), false),
            unidentified_activation(333)
        );
    }

    #[test]
    fn test_unidentified_logged_out_cycle_disabled_preserves_group_behavior() {
        let mut state = CycleState::new(vec![test_group("G1", &["A", "B"])]);
        add_eve(&mut state, "A", 100);
        add_eve(&mut state, "B", 200);
        add_logged_out(&mut state, 333);

        assert_eq!(
            state.cycle_forward("G1", None, false),
            eve_activation(200, "B")
        );
        assert_eq!(
            state.cycle_forward("G1", None, false),
            eve_activation(100, "A")
        );
    }

    #[test]
    fn test_shared_hotkey_starts_from_remembered_logged_out_current_window() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("Default", &["A", "B"])]);
        add_logged_out(&mut state, 111);
        add_logged_out(&mut state, 222);

        let logged_out = HashMap::from([(111, "A".to_string()), (222, "B".to_string())]);
        let identity = SourceIdentity::eve("A".to_string());
        assert!(state.set_current_by_window_with_identity(111, Some(&identity)));

        assert_eq!(
            state.activate_next_in_group(
                &[
                    SourceIdentity::eve("A".to_string()),
                    SourceIdentity::eve("B".to_string())
                ],
                Some(&logged_out)
            ),
            eve_activation(222, "B")
        );
    }

    #[test]
    fn test_same_name_eve_and_custom_source_are_distinct() {
        let mut state = CycleState::new(vec![mixed_group(
            "G1",
            vec![
                CycleSlot::Eve("h0ly lag".to_string()),
                CycleSlot::Source("h0ly lag".to_string()),
            ],
        )]);

        add_eve(&mut state, "h0ly lag", 100);
        add_source(&mut state, "h0ly lag", 200);

        assert_eq!(
            state.cycle_forward("G1", None, false),
            source_activation(200, "h0ly lag")
        );
        assert_eq!(
            state.cycle_forward("G1", None, false),
            eve_activation(100, "h0ly lag")
        );
    }

    #[test]
    fn test_eve_cycle_slot_does_not_match_same_name_custom_source() {
        let mut state = CycleState::new(vec![test_group("G1", &["h0ly lag"])]);
        add_source(&mut state, "h0ly lag", 200);

        assert_eq!(state.cycle_forward("G1", None, false), None);
    }
}
