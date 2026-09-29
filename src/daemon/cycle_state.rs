//! Hotkey cycle state management
//!
//! Navigates the sources in a `SourceRegistry` for hotkey-based cycling. Normal cycling
//! follows configured cycle groups. Optional logged-out modes can include remembered
//! logged-out clients or unidentified login-screen clients.

use std::collections::{HashMap, HashSet};
use tracing::{debug, warn};
use x11rb::protocol::xproto::Window;

use super::source_registry::SourceRegistry;
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

/// Cycle positions, cursor, and skip state over the registered sources.
pub struct CycleState {
    /// Active cycle groups: group_name -> GroupState
    groups: HashMap<String, GroupState>,

    /// Latest requested source while activation is pending, otherwise observed focus.
    /// Used to resolve starting position for cycling, especially for detached sources.
    current_window: Option<Window>,

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
            skipped_sources: HashSet::new(),
            last_active_group: None,
        }
    }

    /// Keep cycle positions valid after `window` left the registry.
    /// Call only when the registry actually removed it.
    pub(super) fn source_removed(&mut self, window: Window) {
        // If a tracked window disappeared, keep group indices in range.
        self.clamp_indices();

        // Clear current_window if it matches
        if self.current_window == Some(window) {
            self.current_window = None;
        }
    }

    fn cycle_candidates(
        order: &[SourceIdentity],
        sources: &SourceRegistry,
        unidentified_logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Vec<CycleCandidate> {
        let mut candidates: Vec<CycleCandidate> =
            order.iter().cloned().map(CycleCandidate::Named).collect();

        if let Some(map) = unidentified_logged_out_map {
            candidates.extend(
                sources
                    .unidentified_logged_out(map)
                    .into_iter()
                    .map(CycleCandidate::Unidentified),
            );
        }

        candidates
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
        sources: &SourceRegistry,
        group_name: &str,
        logged_out_map: Option<&HashMap<Window, String>>,
        reset_on_switch: bool,
    ) -> Option<CycleActivation> {
        self.cycle_group(
            sources,
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
        sources: &SourceRegistry,
        group_name: &str,
        logged_out_map: Option<&HashMap<Window, String>>,
        unidentified_logged_out_map: &HashMap<Window, String>,
        reset_on_switch: bool,
    ) -> Option<CycleActivation> {
        self.cycle_group(
            sources,
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
        sources: &SourceRegistry,
        group_name: &str,
        logged_out_map: Option<&HashMap<Window, String>>,
        reset_on_switch: bool,
    ) -> Option<CycleActivation> {
        self.cycle_group(
            sources,
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
        sources: &SourceRegistry,
        group_name: &str,
        logged_out_map: Option<&HashMap<Window, String>>,
        unidentified_logged_out_map: &HashMap<Window, String>,
        reset_on_switch: bool,
    ) -> Option<CycleActivation> {
        self.cycle_group(
            sources,
            group_name,
            logged_out_map,
            Some(unidentified_logged_out_map),
            reset_on_switch,
            CycleDirection::Backward,
        )
    }

    fn cycle_group(
        &mut self,
        sources: &SourceRegistry,
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

        let candidates = Self::cycle_candidates(&group_order, sources, unidentified_logged_out_map);

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

        if sources.is_empty() && logged_out_map.is_none() {
            warn!(active_windows = sources.len(), "No active windows to cycle");
            return None;
        }

        let group_state = self
            .groups
            .get_mut(group_name)
            .expect("cycle group was checked above");

        if group_state.current_index >= candidates.len() {
            group_state.current_index = 0;
        }

        // Appended clients have no named slot for the focus handler to synchronize.
        // Resolve their current index against the live candidate list before cycling.
        if let Some(window) = self.current_window
            && let Some(index) = candidates
                .iter()
                .position(|candidate| *candidate == CycleCandidate::Unidentified(window))
        {
            group_state.current_index = index;
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

                    if let Some(window) = sources.window_for_identity(identity) {
                        debug!(group = group_name, identity = ?identity, index = group_state.current_index, direction = ?direction, "Cycling to active source");
                        return Some((window, Some(identity.clone())));
                    }

                    if let Some(window) = sources.logged_out_window_for(identity, logged_out_map) {
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
        sources: &SourceRegistry,
        identity: &SourceIdentity,
        logged_out_map: Option<&HashMap<Window, String>>,
    ) -> Option<CycleActivation> {
        if let Some(window) = sources.window_for_identity(identity) {
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

        if let Some(window) = sources.logged_out_window_for(identity, logged_out_map) {
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
        sources: &SourceRegistry,
        logged_out_map: &HashMap<Window, String>,
    ) -> Option<CycleActivation> {
        self.cycle_unidentified_logged_out(sources, logged_out_map, CycleDirection::Forward)
    }

    pub fn cycle_unidentified_logged_out_backward(
        &mut self,
        sources: &SourceRegistry,
        logged_out_map: &HashMap<Window, String>,
    ) -> Option<CycleActivation> {
        self.cycle_unidentified_logged_out(sources, logged_out_map, CycleDirection::Backward)
    }

    fn cycle_unidentified_logged_out(
        &mut self,
        sources: &SourceRegistry,
        logged_out_map: &HashMap<Window, String>,
        direction: CycleDirection,
    ) -> Option<CycleActivation> {
        let candidates = sources.unidentified_logged_out(logged_out_map);

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
        sources: &SourceRegistry,
        window: Window,
        identity: Option<&SourceIdentity>,
    ) -> bool {
        self.current_window = Some(window);

        if let Some(source) = sources.get(window) {
            if let Some(active_identity) = source.live_identity() {
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
        sources: &SourceRegistry,
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
            && let Some(curr_identity) = sources.identity(curr_win, logged_out_map)
            && let Some(pos) = sorted_candidates
                .iter()
                .position(|candidate| **candidate == curr_identity)
        {
            pos
        } else if let Some(default_group) = self.groups.get("Default")
            && let Some(current_identity) = default_group.order.get(default_group.current_index)
        {
            // Fallback to "Default" group index logic if available
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

            if let Some((window, identity)) =
                self.activate_identity(sources, identity, logged_out_map)
            {
                debug!(identity = ?identity, "Activated next in group (advanced)");
                return Some((window, identity));
            }
        }

        debug!("No active sources found in extended hotkey group");
        None
    }

    /// Forget the focus/cycle cursor without losing per-group cycle positions.
    pub(super) fn clear_current_window(&mut self) {
        self.current_window = None;
    }

    /// Get the requested or observed source cursor (if known)
    pub fn get_current_window(&self) -> Option<Window> {
        self.current_window
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::{CycleGroup, CycleSlot};
    use crate::daemon::source_registry::TrackedSource;

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

    fn add_eve(sources: &mut SourceRegistry, name: &str, window: Window) {
        sources.register(window, TrackedSource::eve(name));
    }

    fn add_source(sources: &mut SourceRegistry, name: &str, window: Window) {
        sources.register(window, TrackedSource::custom(name));
    }

    fn add_logged_out(sources: &mut SourceRegistry, window: Window) {
        sources.register(window, TrackedSource::eve(""));
    }

    /// Mirror forget_source: cycle cleanup runs only for an actual registry removal.
    fn remove(state: &mut CycleState, sources: &mut SourceRegistry, window: Window) {
        if sources.remove(window).is_some() {
            state.source_removed(window);
        }
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
        let mut sources = SourceRegistry::default();
        add_eve(&mut sources, "A", 100);
        add_eve(&mut sources, "B", 200);

        assert_eq!(
            state.cycle_forward(&sources, "G1", None, false),
            eve_activation(200, "B")
        );
    }

    #[test]
    fn test_cycle_reset_on_group_switch() {
        let group1 = test_group("G1", &["A", "B", "C"]);
        let group2 = test_group("G2", &["D", "E"]);

        let mut state = CycleState::new(vec![group1, group2]);

        let mut sources = SourceRegistry::default();
        add_eve(&mut sources, "A", 100);
        add_eve(&mut sources, "B", 200);
        add_eve(&mut sources, "C", 300);
        add_eve(&mut sources, "D", 400);
        add_eve(&mut sources, "E", 500);

        assert_eq!(
            state.cycle_forward(&sources, "G1", None, false),
            eve_activation(200, "B")
        );
        assert_eq!(
            state.cycle_forward(&sources, "G2", None, false),
            eve_activation(500, "E")
        );
        assert_eq!(
            state.cycle_forward(&sources, "G1", None, false),
            eve_activation(300, "C")
        );
        assert_eq!(
            state.cycle_forward(&sources, "G2", None, false),
            eve_activation(400, "D")
        );
        assert_eq!(
            state.cycle_forward(&sources, "G1", None, true),
            eve_activation(100, "A")
        );
    }

    #[test]
    fn test_logged_out_click_preserves_clicked_window() {
        use std::collections::HashMap;

        let group = test_group("G1", &["A", "B"]);
        let mut state = CycleState::new(vec![group]);
        let mut sources = SourceRegistry::default();

        // Multiple logged-out thumbnails all have an empty live character name,
        // so clicks must anchor current_window to the exact source window.
        add_logged_out(&mut sources, 111);
        add_logged_out(&mut sources, 222);

        let identity = SourceIdentity::eve("A".to_string());
        assert!(state.set_current_by_window_with_identity(&sources, 111, Some(&identity)));
        assert_eq!(state.get_current_window(), Some(111));
        assert_eq!(state.groups.get("G1").unwrap().current_index, 0);

        let logged_out = HashMap::from([(111, "A".to_string()), (222, "B".to_string())]);
        assert_eq!(
            state.cycle_forward(&sources, "G1", Some(&logged_out), false),
            eve_activation(222, "B")
        );
    }

    #[test]
    fn test_multiple_logged_out_windows_preserve_source_ids() {
        let mut sources = SourceRegistry::default();

        add_logged_out(&mut sources, 111);
        add_logged_out(&mut sources, 222);

        assert_eq!(sources.len(), 2);
        assert_eq!(sources.get(111), Some(&TrackedSource::eve("")));
        assert_eq!(sources.get(222), Some(&TrackedSource::eve("")));
    }

    #[test]
    fn test_remove_logged_out_window_removes_exact_source_and_current() {
        let mut state = CycleState::new(vec![test_group("G1", &["A", "B"])]);
        let mut sources = SourceRegistry::default();
        add_logged_out(&mut sources, 111);
        add_logged_out(&mut sources, 222);

        let identity = SourceIdentity::eve("A".to_string());
        assert!(state.set_current_by_window_with_identity(&sources, 111, Some(&identity)));

        remove(&mut state, &mut sources, 222);
        assert!(sources.contains(111));
        assert!(!sources.contains(222));
        assert_eq!(state.get_current_window(), Some(111));

        remove(&mut state, &mut sources, 111);
        assert_eq!(state.get_current_window(), None);
    }

    #[test]
    fn test_cycle_forward_uses_remembered_logged_out_identity() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &["A", "B"])]);

        let mut sources = SourceRegistry::default();
        add_logged_out(&mut sources, 111);
        add_logged_out(&mut sources, 222);

        let logged_out = HashMap::from([(111, "A".to_string()), (222, "B".to_string())]);

        assert_eq!(
            state.cycle_forward(&sources, "G1", Some(&logged_out), false),
            eve_activation(222, "B")
        );
    }

    #[test]
    fn test_unidentified_logged_out_window_is_not_cycle_candidate() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &["A"])]);

        let mut sources = SourceRegistry::default();
        add_logged_out(&mut sources, 111);

        assert!(state.set_current_by_window_with_identity(&sources, 111, None));
        assert_eq!(state.get_current_window(), Some(111));
        assert_eq!(
            state.cycle_forward(&sources, "G1", Some(&HashMap::new()), false),
            None
        );
    }

    #[test]
    fn test_logged_out_cycling_is_disabled_without_logged_out_map() {
        let mut state = CycleState::new(vec![test_group("G1", &["A"])]);
        let mut sources = SourceRegistry::default();
        add_logged_out(&mut sources, 111);

        assert_eq!(state.cycle_forward(&sources, "G1", None, false), None);
        assert_eq!(
            state.activate_identity(&sources, &SourceIdentity::eve("A".to_string()), None),
            None
        );
    }

    #[test]
    fn test_unidentified_logged_out_cycle_uses_discovery_order() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &[])]);

        let mut sources = SourceRegistry::default();
        add_logged_out(&mut sources, 111);
        add_logged_out(&mut sources, 222);

        let logged_out = HashMap::new();

        assert_eq!(
            state.cycle_unidentified_logged_out_forward(&sources, &logged_out),
            unidentified_activation(111)
        );

        assert!(state.set_current_by_window_with_identity(&sources, 111, None));
        assert_eq!(
            state.cycle_unidentified_logged_out_forward(&sources, &logged_out),
            unidentified_activation(222)
        );
    }

    #[test]
    fn test_unidentified_logged_out_backward_starts_from_current_source() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &[])]);

        let mut sources = SourceRegistry::default();
        add_logged_out(&mut sources, 111);
        add_logged_out(&mut sources, 222);

        assert!(state.set_current_by_window_with_identity(&sources, 111, None));
        assert_eq!(
            state.cycle_unidentified_logged_out_backward(&sources, &HashMap::new()),
            unidentified_activation(222)
        );
    }

    #[test]
    fn test_identified_logged_out_window_is_not_unidentified_candidate() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &[])]);

        let mut sources = SourceRegistry::default();
        add_logged_out(&mut sources, 111);
        add_logged_out(&mut sources, 222);

        let logged_out = HashMap::from([(111, "A".to_string())]);

        assert_eq!(
            state.cycle_unidentified_logged_out_forward(&sources, &logged_out),
            unidentified_activation(222)
        );
    }

    #[test]
    fn test_removed_unidentified_window_leaves_discovery_order() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &[])]);

        let mut sources = SourceRegistry::default();
        add_logged_out(&mut sources, 111);
        add_logged_out(&mut sources, 222);
        remove(&mut state, &mut sources, 111);

        assert_eq!(
            state.cycle_unidentified_logged_out_forward(&sources, &HashMap::new()),
            unidentified_activation(222)
        );
    }

    #[test]
    fn test_append_mode_cycles_group_entries_then_unidentified_clients() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("G1", &["A", "B"])]);

        let mut sources = SourceRegistry::default();
        add_eve(&mut sources, "A", 100);
        add_eve(&mut sources, "B", 200);
        add_logged_out(&mut sources, 333);

        assert_eq!(
            state.cycle_forward_with_unidentified(&sources, "G1", None, &HashMap::new(), false),
            eve_activation(200, "B")
        );
        assert_eq!(
            state.cycle_forward_with_unidentified(&sources, "G1", None, &HashMap::new(), false),
            unidentified_activation(333)
        );
    }

    #[test]
    fn test_unidentified_logged_out_cycle_disabled_preserves_group_behavior() {
        let mut state = CycleState::new(vec![test_group("G1", &["A", "B"])]);
        let mut sources = SourceRegistry::default();
        add_eve(&mut sources, "A", 100);
        add_eve(&mut sources, "B", 200);
        add_logged_out(&mut sources, 333);

        assert_eq!(
            state.cycle_forward(&sources, "G1", None, false),
            eve_activation(200, "B")
        );
        assert_eq!(
            state.cycle_forward(&sources, "G1", None, false),
            eve_activation(100, "A")
        );
    }

    #[test]
    fn test_shared_hotkey_starts_from_remembered_logged_out_current_window() {
        use std::collections::HashMap;

        let mut state = CycleState::new(vec![test_group("Default", &["A", "B"])]);

        let mut sources = SourceRegistry::default();
        add_logged_out(&mut sources, 111);
        add_logged_out(&mut sources, 222);

        let logged_out = HashMap::from([(111, "A".to_string()), (222, "B".to_string())]);
        let identity = SourceIdentity::eve("A".to_string());
        assert!(state.set_current_by_window_with_identity(&sources, 111, Some(&identity)));

        assert_eq!(
            state.activate_next_in_group(
                &sources,
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
        let mut sources = SourceRegistry::default();

        add_eve(&mut sources, "h0ly lag", 100);
        add_source(&mut sources, "h0ly lag", 200);

        assert_eq!(
            state.cycle_forward(&sources, "G1", None, false),
            source_activation(200, "h0ly lag")
        );
        assert_eq!(
            state.cycle_forward(&sources, "G1", None, false),
            eve_activation(100, "h0ly lag")
        );
    }

    #[test]
    fn test_eve_cycle_slot_does_not_match_same_name_custom_source() {
        let mut state = CycleState::new(vec![test_group("G1", &["h0ly lag"])]);
        let mut sources = SourceRegistry::default();
        add_source(&mut sources, "h0ly lag", 200);

        assert_eq!(state.cycle_forward(&sources, "G1", None, false), None);
    }

    #[test]
    fn direct_hotkey_after_appended_unidentified_client() {
        let mut state = CycleState::new(vec![test_group("Default", &["Pilot"])]);
        let mut sources = SourceRegistry::default();
        add_eve(&mut sources, "Pilot", 10);
        add_logged_out(&mut sources, 20);
        assert_eq!(
            state.cycle_forward_with_unidentified(
                &sources,
                "Default",
                None,
                &HashMap::new(),
                false
            ),
            unidentified_activation(20)
        );
        state.set_current_by_window_with_identity(&sources, 20, None);

        assert_eq!(
            state.activate_next_in_group(&sources, &[SourceIdentity::eve("Pilot")], None),
            eve_activation(10, "Pilot")
        );
    }

    #[test]
    fn manually_focused_appended_client_sets_forward_and_backward_start() {
        let mut state = CycleState::new(vec![test_group("Default", &["Pilot"])]);
        let mut sources = SourceRegistry::default();
        add_eve(&mut sources, "Pilot", 10);
        add_logged_out(&mut sources, 20);
        add_logged_out(&mut sources, 30);
        state.set_current_by_window_with_identity(&sources, 30, None);

        assert_eq!(
            state.cycle_forward_with_unidentified(
                &sources,
                "Default",
                None,
                &HashMap::new(),
                false
            ),
            eve_activation(10, "Pilot")
        );
        assert_eq!(
            state.cycle_backward_with_unidentified(
                &sources,
                "Default",
                None,
                &HashMap::new(),
                false
            ),
            unidentified_activation(20)
        );

        // Removing an earlier appended client must not leave a stale index.
        remove(&mut state, &mut sources, 20);
        assert_eq!(
            state.cycle_backward_with_unidentified(
                &sources,
                "Default",
                None,
                &HashMap::new(),
                false
            ),
            eve_activation(10, "Pilot")
        );
    }

    #[test]
    fn group_reset_takes_precedence_over_focused_appended_client() {
        let mut state = CycleState::new(vec![test_group("Default", &["Pilot"])]);
        let mut sources = SourceRegistry::default();
        add_eve(&mut sources, "Pilot", 10);
        add_logged_out(&mut sources, 20);
        add_logged_out(&mut sources, 30);
        state.set_current_by_window_with_identity(&sources, 20, None);

        assert_eq!(
            state.cycle_forward_with_unidentified(&sources, "Default", None, &HashMap::new(), true),
            eve_activation(10, "Pilot")
        );
    }
    #[test]
    fn clearing_focus_retains_cycle_group_positions() {
        let mut state = CycleState::new(vec![crate::config::profile::CycleGroup {
            name: "Default".into(),
            cycle_list: vec![
                CycleSlot::Eve("Alice".into()),
                CycleSlot::Eve("Bob".into()),
                CycleSlot::Eve("Charlie".into()),
            ],
            hotkey_forward: None,
            hotkey_backward: None,
        }]);
        let mut sources = SourceRegistry::default();
        for (window, name) in [(10, "Alice"), (20, "Bob"), (30, "Charlie")] {
            sources.register(window, TrackedSource::eve(name));
        }
        state.set_current_by_window_with_identity(&sources, 20, None);
        let index = state.groups["Default"].current_index;
        state.clear_current_window();
        assert_eq!(state.get_current_window(), None);
        assert_eq!(state.groups["Default"].current_index, index);
        assert_eq!(
            state.cycle_forward(&sources, "Default", None, false),
            Some((30, Some(SourceIdentity::eve("Charlie"))))
        );
    }
}
