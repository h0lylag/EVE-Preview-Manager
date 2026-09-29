//! Live source windows and what each was admitted as
//!
//! The registry is the single owner of which windows are tracked, their discovery order,
//! and the admission invariants: a registered window keeps its kind, and a custom source
//! its alias, until it is removed. Only an EVE client's character name changes. Cycling,
//! focus, visibility, and minimization read it; none of them own it.

use std::collections::HashMap;
use tracing::{debug, warn};
use x11rb::protocol::xproto::Window;

use crate::common::types::{SourceIdentity, SourceKind};

/// What a registered window was admitted as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackedSource {
    /// An EVE client; `character` is None while it shows the login screen.
    Eve { character: Option<String> },
    /// A configured custom source, named by its rule alias (which may be empty).
    Custom { alias: String },
}

impl TrackedSource {
    /// An EVE client, logged out when `character` is empty.
    pub fn eve(character: impl Into<String>) -> Self {
        let character = character.into();
        Self::Eve {
            character: (!character.is_empty()).then_some(character),
        }
    }

    pub fn custom(alias: impl Into<String>) -> Self {
        Self::Custom {
            alias: alias.into(),
        }
    }

    pub fn kind(&self) -> SourceKind {
        match self {
            Self::Eve { .. } => SourceKind::Eve,
            Self::Custom { .. } => SourceKind::Custom,
        }
    }

    /// The identity the window shows right now; None for a logged-out EVE client.
    pub fn live_identity(&self) -> Option<SourceIdentity> {
        match self {
            Self::Eve { character } => character.clone().map(SourceIdentity::eve),
            Self::Custom { alias } => Some(SourceIdentity::custom(alias.clone())),
        }
    }

    fn is_logged_out(&self) -> bool {
        matches!(self, Self::Eve { character: None })
    }

    /// Whether the window currently shows `identity`, compared without allocating.
    fn shows(&self, identity: &SourceIdentity) -> bool {
        match self {
            Self::Eve { character } => {
                identity.kind.is_eve() && character.as_deref() == Some(identity.name.as_str())
            }
            Self::Custom { alias } => identity.kind.is_custom() && *alias == identity.name,
        }
    }
}

#[cfg(test)]
impl From<SourceIdentity> for TrackedSource {
    fn from(identity: SourceIdentity) -> Self {
        match identity.kind {
            SourceKind::Eve => Self::eve(identity.name),
            SourceKind::Custom => Self::custom(identity.name),
        }
    }
}

#[derive(Debug, Default)]
#[cfg_attr(test, derive(Clone, PartialEq))]
pub struct SourceRegistry {
    sources: HashMap<Window, TrackedSource>,
    /// Registered windows in first-detected order, for unidentified logged-out cycling.
    order: Vec<Window>,
}

impl SourceRegistry {
    /// Admit a source window, or update the character of an admitted EVE client.
    ///
    /// Admission is final: a registered window keeps its kind, and a custom source its
    /// alias, until it is removed. Any other change is rejected without mutation.
    pub fn register(&mut self, window: Window, source: TrackedSource) {
        if let Some(existing) = self.sources.get(&window) {
            let keeps_admission = match (existing, &source) {
                (TrackedSource::Eve { .. }, TrackedSource::Eve { .. }) => true,
                (TrackedSource::Custom { alias }, TrackedSource::Custom { alias: new }) => {
                    alias == new
                }
                _ => false,
            };
            if !keeps_admission {
                warn!(existing = ?existing, rejected = ?source, window, "Ignoring identity change for admitted source");
                return;
            }
        } else {
            self.order.push(window);
        }
        debug!(source = ?source, window, "Registering source window");
        self.sources.insert(window, source);
    }

    /// Update an admitted EVE client's live character name (called on login/logout).
    /// Never admits a window and never applies to custom sources.
    pub fn update_character(&mut self, window: Window, new_name: String) {
        match self.sources.get_mut(&window) {
            Some(TrackedSource::Eve { character }) => {
                *character = (!new_name.is_empty()).then_some(new_name);
            }
            _ => debug!(
                window,
                "Ignoring character update for a window not admitted as EVE"
            ),
        }
    }

    /// Forget a destroyed source, returning what it was admitted as.
    pub fn remove(&mut self, window: Window) -> Option<TrackedSource> {
        let removed = self.sources.remove(&window)?;
        debug!(source = ?removed, window, "Removing source window");
        self.order.retain(|tracked| *tracked != window);
        Some(removed)
    }

    pub fn contains(&self, window: Window) -> bool {
        self.sources.contains_key(&window)
    }

    pub fn get(&self, window: Window) -> Option<&TrackedSource> {
        self.sources.get(&window)
    }

    /// Kind fixed at admission, or None for an unregistered window.
    pub fn kind(&self, window: Window) -> Option<SourceKind> {
        self.get(window).map(TrackedSource::kind)
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn windows(&self) -> impl Iterator<Item = Window> + '_ {
        self.sources.keys().copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Window, &TrackedSource)> {
        self.sources
            .iter()
            .map(|(&window, source)| (window, source))
    }

    /// Count EVE windows independently of rendering, groups, skip state, or live names.
    pub fn eve_client_count(&self) -> usize {
        self.sources
            .values()
            .filter(|source| source.kind().is_eve())
            .count()
    }

    /// Live identity first; a logged-out EVE client falls back to its remembered character.
    pub fn identity(
        &self,
        window: Window,
        remembered: Option<&HashMap<Window, String>>,
    ) -> Option<SourceIdentity> {
        match self.get(window)? {
            source if source.is_logged_out() => remembered
                .and_then(|map| map.get(&window))
                .map(|name| SourceIdentity::eve(name.clone())),
            source => source.live_identity(),
        }
    }

    /// A window currently showing `identity`. Duplicate identities resolve arbitrarily.
    pub fn window_for_identity(&self, identity: &SourceIdentity) -> Option<Window> {
        self.iter()
            .find_map(|(window, source)| source.shows(identity).then_some(window))
    }

    /// A logged-out EVE client whose remembered character is `identity`.
    pub fn logged_out_window_for(
        &self,
        identity: &SourceIdentity,
        remembered: Option<&HashMap<Window, String>>,
    ) -> Option<Window> {
        if !identity.kind.is_eve() || identity.name.is_empty() {
            return None;
        }

        let map = remembered?;

        self.iter()
            .filter(|(_, source)| source.is_logged_out())
            .find_map(|(window, _)| {
                map.get(&window)
                    .is_some_and(|last_char| last_char == &identity.name)
                    .then_some(window)
            })
    }

    /// Logged-out EVE clients with no remembered character, in discovery order.
    pub fn unidentified_logged_out(&self, remembered: &HashMap<Window, String>) -> Vec<Window> {
        self.order
            .iter()
            .copied()
            .filter(|window| {
                self.get(*window).is_some_and(TrackedSource::is_logged_out)
                    && !remembered.contains_key(window)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_prefers_live_source_and_requires_registration() {
        let mut sources = SourceRegistry::default();
        let remembered = HashMap::from([
            (1, "Old Eve".to_string()),
            (2, "Alice".to_string()),
            (3, "Stale".to_string()),
        ]);
        sources.register(1, TrackedSource::custom("Alice"));
        sources.register(2, TrackedSource::eve(""));
        sources.register(4, TrackedSource::eve(""));
        assert_eq!(
            sources.identity(1, Some(&remembered)),
            Some(SourceIdentity::custom("Alice"))
        );
        assert_eq!(
            sources.identity(2, Some(&remembered)),
            Some(SourceIdentity::eve("Alice"))
        );
        assert_eq!(sources.identity(2, None), None);
        assert_eq!(sources.identity(3, Some(&remembered)), None);
        assert_eq!(sources.identity(4, Some(&remembered)), None);
        sources.register(2, TrackedSource::eve("Bob"));
        assert_eq!(
            sources.identity(2, Some(&remembered)),
            Some(SourceIdentity::eve("Bob"))
        );
    }

    #[test]
    fn eve_count_tracks_windows_independently_of_names() {
        let mut sources = SourceRegistry::default();
        assert_eq!(sources.eve_client_count(), 0);
        sources.register(10, TrackedSource::eve("Alice"));
        sources.register(10, TrackedSource::eve("Alice"));
        assert_eq!(sources.eve_client_count(), 1);
        sources.register(20, TrackedSource::eve(""));
        sources.register(30, TrackedSource::custom("Alice"));
        sources.register(40, TrackedSource::custom(""));
        assert_eq!(sources.eve_client_count(), 2);
        sources.update_character(10, "Bob".into());
        sources.update_character(10, String::new());
        assert_eq!(sources.eve_client_count(), 2);
        assert_eq!(sources.remove(20), Some(TrackedSource::eve("")));
        assert_eq!(sources.remove(20), None);
        assert_eq!(sources.remove(999), None);
        assert_eq!(sources.eve_client_count(), 1);
        sources.remove(10);
        assert_eq!(sources.eve_client_count(), 0);
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn admission_fixes_kind_and_custom_alias() {
        let mut sources = SourceRegistry::default();
        sources.register(1, TrackedSource::custom("YouTube"));
        sources.register(2, TrackedSource::eve(""));
        sources.register(3, TrackedSource::eve("Pilot"));
        let order = sources.order.clone();

        // Custom sources can change neither kind nor alias, directly or via character updates.
        sources.register(1, TrackedSource::eve("Impostor"));
        sources.register(1, TrackedSource::eve(""));
        sources.register(1, TrackedSource::custom("Discord"));
        sources.update_character(1, "Impostor".into());
        sources.update_character(1, String::new());
        // EVE clients, logged in or out, never become custom sources.
        sources.register(2, TrackedSource::custom("YouTube"));
        sources.register(3, TrackedSource::custom("Pilot"));
        assert_eq!(sources.kind(1), Some(SourceKind::Custom));
        assert_eq!(sources.kind(2), Some(SourceKind::Eve));
        assert_eq!(sources.kind(3), Some(SourceKind::Eve));
        assert_eq!(sources.get(1), Some(&TrackedSource::custom("YouTube")));
        assert_eq!(sources.get(2), Some(&TrackedSource::eve("")));
        assert_eq!(sources.eve_client_count(), 2);

        // Login, swap and logout remain EVE name transitions.
        sources.update_character(2, "Alice".into());
        sources.register(3, TrackedSource::eve("Bob"));
        sources.update_character(3, String::new());
        assert_eq!(sources.get(2), Some(&TrackedSource::eve("Alice")));
        assert_eq!(sources.get(3), Some(&TrackedSource::eve("")));
        assert_eq!(sources.order, order);

        // Character updates never admit a window; removal ends the admission.
        sources.update_character(4, "Ghost".into());
        assert_eq!(sources.kind(4), None);
        assert_eq!(sources.eve_client_count(), 2);
        assert_eq!(sources.order, order);
        sources.remove(1);
        sources.register(1, TrackedSource::eve("Reused"));
        assert_eq!(sources.kind(1), Some(SourceKind::Eve));
    }

    #[test]
    fn logged_out_lookups_use_remembered_names_and_discovery_order() {
        let mut sources = SourceRegistry::default();
        sources.register(30, TrackedSource::eve(""));
        sources.register(10, TrackedSource::eve(""));
        sources.register(20, TrackedSource::eve(""));
        sources.register(40, TrackedSource::eve("Live"));
        sources.register(50, TrackedSource::custom("Alice"));
        let remembered = HashMap::from([(10, "Alice".to_string()), (40, "Old".to_string())]);

        assert_eq!(sources.unidentified_logged_out(&remembered), vec![30, 20]);
        let alice = SourceIdentity::eve("Alice");
        assert_eq!(
            sources.logged_out_window_for(&alice, Some(&remembered)),
            Some(10)
        );
        assert_eq!(sources.logged_out_window_for(&alice, None), None);
        assert_eq!(
            sources.logged_out_window_for(&SourceIdentity::custom("Alice"), Some(&remembered)),
            None
        );
        assert_eq!(
            sources.logged_out_window_for(&SourceIdentity::eve("Old"), Some(&remembered)),
            None
        );
        assert_eq!(sources.window_for_identity(&alice), None);
        assert_eq!(
            sources.window_for_identity(&SourceIdentity::custom("Alice")),
            Some(50)
        );
        assert_eq!(
            sources.window_for_identity(&SourceIdentity::eve("Live")),
            Some(40)
        );

        sources.remove(30);
        assert_eq!(sources.unidentified_logged_out(&remembered), vec![20]);
    }
}
