//! Live source windows and what each was admitted as
//!
//! The registry is the single owner of which windows are tracked, their discovery order,
//! the admission invariants, and each EVE client's session history. A registered window
//! keeps its kind, and a custom source its alias, until it is removed. Only an EVE
//! client's session changes, and it records the last accepted title observation even if
//! the preview fails to follow. Cycling, focus, visibility, minimization, and previews
//! read it; none of them own it.

use std::collections::HashMap;
use tracing::{debug, warn};
use x11rb::protocol::xproto::Window;

use crate::common::types::{SourceIdentity, SourceKind};

/// An EVE client's session, as last observed from its window title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EveSession {
    LoggedIn(String),
    /// On the login screen; `last` is the character it was last logged in as, if any.
    LoggedOut {
        last: Option<String>,
    },
}

impl EveSession {
    fn new(character: String) -> Self {
        if character.is_empty() {
            Self::LoggedOut { last: None }
        } else {
            Self::LoggedIn(character)
        }
    }

    /// Apply an accepted title observation: a character logs in or swaps; the login
    /// screen logs out and keeps the previous character as history.
    fn observe(&mut self, character: String) {
        if !character.is_empty() {
            *self = Self::LoggedIn(character);
        } else if let Self::LoggedIn(previous) = self {
            *self = Self::LoggedOut {
                last: Some(std::mem::take(previous)),
            };
        }
    }

    fn live(&self) -> Option<&str> {
        match self {
            Self::LoggedIn(character) => Some(character),
            Self::LoggedOut { .. } => None,
        }
    }

    /// The live character, or the last one while logged out.
    fn remembered(&self) -> Option<&str> {
        match self {
            Self::LoggedIn(character) => Some(character),
            Self::LoggedOut { last } => last.as_deref(),
        }
    }
}

/// What a registered window was admitted as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackedSource {
    Eve(EveSession),
    /// A configured custom source, named by its rule alias (which may be empty).
    Custom {
        alias: String,
    },
}

impl TrackedSource {
    /// An EVE client, logged out with no history when `character` is empty.
    pub fn eve(character: impl Into<String>) -> Self {
        Self::Eve(EveSession::new(character.into()))
    }

    pub fn custom(alias: impl Into<String>) -> Self {
        Self::Custom {
            alias: alias.into(),
        }
    }

    pub fn kind(&self) -> SourceKind {
        match self {
            Self::Eve(_) => SourceKind::Eve,
            Self::Custom { .. } => SourceKind::Custom,
        }
    }

    /// The identity the window shows right now; None for a logged-out EVE client.
    pub fn live_identity(&self) -> Option<SourceIdentity> {
        match self {
            Self::Eve(session) => session.live().map(SourceIdentity::eve),
            Self::Custom { alias } => Some(SourceIdentity::custom(alias.clone())),
        }
    }

    /// Whether the window currently shows `identity`, compared without allocating.
    fn shows(&self, identity: &SourceIdentity) -> bool {
        match self {
            Self::Eve(session) => {
                identity.kind.is_eve() && session.live() == Some(identity.name.as_str())
            }
            Self::Custom { alias } => identity.kind.is_custom() && *alias == identity.name,
        }
    }
}

#[cfg(test)]
impl TrackedSource {
    /// A logged-out EVE client that was last logged in as `character`.
    pub fn logged_out_after(character: impl Into<String>) -> Self {
        Self::Eve(EveSession::LoggedOut {
            last: Some(character.into()),
        })
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
    /// Admit a source window, or apply a title observation to an admitted EVE client.
    ///
    /// Admission is final: a registered window keeps its kind, and a custom source its
    /// alias, until it is removed. Any other change is rejected without mutation. For an
    /// admitted EVE client only the observed live character matters; its history is kept.
    pub fn register(&mut self, window: Window, source: TrackedSource) {
        match (self.sources.get_mut(&window), source) {
            (None, source) => {
                debug!(source = ?source, window, "Registering source window");
                self.order.push(window);
                self.sources.insert(window, source);
            }
            (Some(TrackedSource::Eve(session)), TrackedSource::Eve(observed)) => {
                session.observe(match observed {
                    EveSession::LoggedIn(character) => character,
                    EveSession::LoggedOut { .. } => String::new(),
                });
            }
            (Some(TrackedSource::Custom { alias }), TrackedSource::Custom { alias: new })
                if *alias == new => {}
            (Some(existing), rejected) => {
                warn!(existing = ?existing, rejected = ?rejected, window, "Ignoring identity change for admitted source");
            }
        }
    }

    /// Apply an accepted title observation to an admitted EVE client (login, swap, or
    /// logout). Never admits a window and never applies to custom sources.
    pub fn update_character(&mut self, window: Window, new_name: String) {
        match self.sources.get_mut(&window) {
            Some(TrackedSource::Eve(session)) => session.observe(new_name),
            _ => debug!(
                window,
                "Ignoring character update for a window not admitted as EVE"
            ),
        }
    }

    /// Forget a destroyed source and its history, returning what it was admitted as.
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

    /// The identity the window shows right now; None for a logged-out EVE client.
    pub fn live_identity(&self, window: Window) -> Option<SourceIdentity> {
        self.get(window)?.live_identity()
    }

    /// Live identity first; a logged-out EVE client falls back to its last character.
    pub fn effective_identity(&self, window: Window) -> Option<SourceIdentity> {
        match self.get(window)? {
            TrackedSource::Eve(session) => session.remembered().map(SourceIdentity::eve),
            source => source.live_identity(),
        }
    }

    /// An EVE client's live character, or its last one while logged out.
    pub fn remembered_character(&self, window: Window) -> Option<&str> {
        match self.get(window)? {
            TrackedSource::Eve(session) => session.remembered(),
            TrackedSource::Custom { .. } => None,
        }
    }

    /// A window currently showing `identity`. Duplicate identities resolve arbitrarily.
    pub fn window_for_identity(&self, identity: &SourceIdentity) -> Option<Window> {
        self.iter()
            .find_map(|(window, source)| source.shows(identity).then_some(window))
    }

    /// A logged-out EVE client last logged in as `identity`.
    pub fn logged_out_window_for(&self, identity: &SourceIdentity) -> Option<Window> {
        if !identity.kind.is_eve() || identity.name.is_empty() {
            return None;
        }

        self.iter().find_map(|(window, source)| {
            matches!(source, TrackedSource::Eve(EveSession::LoggedOut { last: Some(last) })
                if *last == identity.name)
            .then_some(window)
        })
    }

    /// Logged-out EVE clients that were never identified, in discovery order.
    pub fn unidentified_logged_out(&self) -> Vec<Window> {
        self.order
            .iter()
            .copied()
            .filter(|window| {
                matches!(
                    self.get(*window),
                    Some(TrackedSource::Eve(EveSession::LoggedOut { last: None }))
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_queries_prefer_live_names_and_require_registration() {
        let mut sources = SourceRegistry::default();
        sources.register(1, TrackedSource::custom("Alice"));
        sources.register(2, TrackedSource::logged_out_after("Alice"));
        sources.register(4, TrackedSource::eve(""));
        let custom = Some(SourceIdentity::custom("Alice"));
        assert_eq!(sources.live_identity(1), custom);
        assert_eq!(sources.effective_identity(1), custom);
        assert_eq!(sources.remembered_character(1), None);
        assert_eq!(sources.live_identity(2), None);
        assert_eq!(
            sources.effective_identity(2),
            Some(SourceIdentity::eve("Alice"))
        );
        assert_eq!(sources.remembered_character(2), Some("Alice"));
        for window in [3, 4] {
            assert_eq!(sources.effective_identity(window), None);
            assert_eq!(sources.remembered_character(window), None);
        }
        sources.register(2, TrackedSource::eve("Bob"));
        assert_eq!(sources.live_identity(2), Some(SourceIdentity::eve("Bob")));
        assert_eq!(
            sources.effective_identity(2),
            Some(SourceIdentity::eve("Bob"))
        );
    }

    #[test]
    fn eve_sessions_keep_history_across_logout_until_removal() {
        let mut sources = SourceRegistry::default();
        sources.register(1, TrackedSource::eve("Alice"));
        assert_eq!(sources.remembered_character(1), Some("Alice"));

        // Logging out keeps the character; repeated logouts and re-detection keep it too.
        sources.update_character(1, String::new());
        sources.update_character(1, String::new());
        sources.register(1, TrackedSource::eve(""));
        assert_eq!(
            sources.get(1),
            Some(&TrackedSource::logged_out_after("Alice"))
        );

        // Login, swap, and a later logout remember the newest character.
        sources.update_character(1, "Bob".into());
        assert_eq!(sources.get(1), Some(&TrackedSource::eve("Bob")));
        sources.register(1, TrackedSource::eve("Carol"));
        sources.update_character(1, String::new());
        assert_eq!(
            sources.get(1),
            Some(&TrackedSource::logged_out_after("Carol"))
        );

        // Custom and unregistered windows never gain history.
        sources.register(2, TrackedSource::custom("YouTube"));
        sources.update_character(2, "Impostor".into());
        sources.update_character(3, "Ghost".into());
        assert_eq!(sources.remembered_character(2), None);
        assert_eq!(sources.get(3), None);

        // Removal drops history, so a reused window ID starts fresh.
        assert_eq!(
            sources.remove(1),
            Some(TrackedSource::logged_out_after("Carol"))
        );
        sources.register(1, TrackedSource::eve(""));
        assert_eq!(sources.remembered_character(1), None);
        assert_eq!(sources.unidentified_logged_out(), vec![1]);
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

        // Login, swap and logout remain EVE session transitions.
        sources.update_character(2, "Alice".into());
        sources.register(3, TrackedSource::eve("Bob"));
        sources.update_character(3, String::new());
        assert_eq!(sources.get(2), Some(&TrackedSource::eve("Alice")));
        assert_eq!(
            sources.get(3),
            Some(&TrackedSource::logged_out_after("Bob"))
        );
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
    fn logged_out_lookups_use_history_and_discovery_order() {
        let mut sources = SourceRegistry::default();
        sources.register(30, TrackedSource::eve(""));
        sources.register(10, TrackedSource::logged_out_after("Alice"));
        sources.register(20, TrackedSource::eve(""));
        sources.register(40, TrackedSource::eve("Live"));
        sources.register(50, TrackedSource::custom("Alice"));

        assert_eq!(sources.unidentified_logged_out(), vec![30, 20]);
        let alice = SourceIdentity::eve("Alice");
        assert_eq!(sources.logged_out_window_for(&alice), Some(10));
        assert_eq!(
            sources.logged_out_window_for(&SourceIdentity::custom("Alice")),
            None
        );
        // A live client is not a logged-out candidate for its own character.
        assert_eq!(
            sources.logged_out_window_for(&SourceIdentity::eve("Live")),
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
        assert_eq!(sources.unidentified_logged_out(), vec![20]);
    }
}
