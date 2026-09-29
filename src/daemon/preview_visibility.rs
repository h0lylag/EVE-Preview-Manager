//! Preview hiding policy and the source state awaiting confirmed focus observation.
//!
//! Creation and reconciliation use the same policy. Activation owns focus-loss timing;
//! this module receives only whether the previous active preview must stay hidden.

use std::collections::HashSet;

use x11rb::protocol::xproto::Window;

use super::focus::FocusOwner;
use super::session_state::SessionState;
use crate::common::types::SourceKind;
use crate::config::{DaemonConfig, DisplayConfig};

/// A snapshot shared by every preview in one reconciliation pass.
pub(super) struct VisibilityContext<'a> {
    display: &'a DisplayConfig,
    runtime_hidden: bool,
    focus_hidden: bool,
    eve_client_count: usize,
}

impl<'a> VisibilityContext<'a> {
    pub(super) fn new(
        display: &'a DisplayConfig,
        config: &DaemonConfig,
        session: &SessionState,
        eve_client_count: usize,
    ) -> Self {
        Self {
            display,
            runtime_hidden: config.runtime_hidden,
            focus_hidden: session.focus_hidden,
            eve_client_count,
        }
    }
}

#[derive(Default)]
pub(super) struct PreviewVisibility {
    active_source: Option<Window>,
    awaiting_focus: HashSet<Window>,
}

impl PreviewVisibility {
    /// Register before evaluating a new preview: its frame ownership is not known yet.
    pub(super) fn mark_unconfirmed(&mut self, source: Window) {
        self.awaiting_focus.insert(source);
    }

    /// A failed creation must not leave a provisional preview behind.
    pub(super) fn creation_failed(&mut self, source: Window) {
        self.awaiting_focus.remove(&source);
    }

    /// Called only after a successful observation. PointerRoot and frame ownership count
    /// for hiding; neither is sufficient to confirm an activation transaction.
    pub(super) fn observe(&mut self, owner: FocusOwner, retain_during_grace: bool) {
        self.active_source = match owner {
            FocusOwner::Source(source) | FocusOwner::Frame(source) => Some(source),
            // A preview overlay keeps the previous block, avoiding pointer-focus oscillation.
            FocusOwner::Preview(_) => self.active_source,
            FocusOwner::Outside(_) | FocusOwner::Root | FocusOwner::None => {
                self.active_source.filter(|_| retain_during_grace)
            }
        };
        self.awaiting_focus.clear();
    }

    pub(super) fn source_removed(&mut self, source: Window) {
        if self.active_source == Some(source) {
            self.active_source = None;
        }
        self.awaiting_focus.remove(&source);
    }

    /// Combine independent reasons; per-source rendering settings cannot bypass a block.
    pub(super) fn blocked(
        &self,
        source: Window,
        kind: SourceKind,
        context: &VisibilityContext<'_>,
    ) -> bool {
        context.runtime_hidden
            || (context.display.hide_when_no_focus && context.focus_hidden)
            || (context.display.hide_when_single_client
                && kind.is_eve()
                && context.eve_client_count == 1)
            || (context.display.hide_active
                && (self.active_source == Some(source) || self.awaiting_focus.contains(&source)))
    }

    #[cfg(test)]
    pub(super) fn active_source(&self) -> Option<Window> {
        self.active_source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::Profile;
    use std::collections::HashMap;

    fn config() -> DaemonConfig {
        DaemonConfig {
            profile: Profile::default(),
            character_thumbnails: HashMap::new(),
            custom_source_thumbnails: HashMap::new(),
            profile_hotkeys: HashMap::new(),
            runtime_hidden: false,
        }
    }

    #[test]
    fn preview_hiding_policy_combines_independent_reasons() {
        let mut config = config();
        for single in [false, true] {
            for active in [false, true] {
                for hide_focus in [false, true] {
                    config.profile.thumbnail_hide_when_single_client = single;
                    config.profile.thumbnail_hide_active = active;
                    config.profile.thumbnail_hide_not_focused = hide_focus;
                    let display = config.build_display_config();
                    assert_eq!(display.hide_when_single_client, single);
                    assert_eq!(display.hide_active, active);
                    for kind in [SourceKind::Eve, SourceKind::Custom] {
                        for count in 0..=3 {
                            for manual in [false, true] {
                                for lost_focus in [false, true] {
                                    for focused in [false, true] {
                                        config.runtime_hidden = manual;
                                        let mut session = SessionState {
                                            focus_hidden: lost_focus,
                                            ..SessionState::default()
                                        };
                                        session.preview_visibility.observe(
                                            if focused {
                                                FocusOwner::Source(10)
                                            } else {
                                                FocusOwner::Root
                                            },
                                            false,
                                        );
                                        let context = VisibilityContext::new(
                                            &display, &config, &session, count,
                                        );
                                        let expected = manual
                                            || (hide_focus && lost_focus)
                                            || (single && kind == SourceKind::Eve && count == 1)
                                            || (active && focused);
                                        assert_eq!(
                                            session.preview_visibility.blocked(10, kind, &context),
                                            expected
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn observations_preserve_preview_ownership_and_focus_loss_grace() {
        for (owner, retained, expected) in [
            (FocusOwner::Source(20), false, Some(20)),
            (FocusOwner::Frame(20), false, Some(20)),
            (FocusOwner::Preview(20), false, Some(10)),
            (FocusOwner::Outside(50), false, None),
            (FocusOwner::Root, false, None),
            (FocusOwner::None, false, None),
            (FocusOwner::Outside(50), true, Some(10)),
            (FocusOwner::Root, true, Some(10)),
            (FocusOwner::None, true, Some(10)),
        ] {
            let mut state = PreviewVisibility::default();
            state.observe(FocusOwner::Source(10), false);
            state.mark_unconfirmed(30);
            state.observe(owner, retained);
            assert_eq!(state.active_source, expected, "{owner:?}");
            assert!(state.awaiting_focus.is_empty());
        }
    }

    #[test]
    fn failed_creation_and_source_removal_clear_only_their_own_state() {
        let mut config = config();
        config.profile.thumbnail_hide_active = true;
        config.profile.thumbnail_hide_not_focused = false;
        let display = config.build_display_config();
        let mut session = SessionState::default();
        session
            .preview_visibility
            .observe(FocusOwner::Source(10), false);
        for source in [10, 20, 30] {
            session.preview_visibility.mark_unconfirmed(source);
        }
        session.preview_visibility.creation_failed(10);
        session.preview_visibility.source_removed(20);
        let context = VisibilityContext::new(&display, &config, &session, 3);
        assert!(
            session
                .preview_visibility
                .blocked(10, SourceKind::Eve, &context)
        );
        assert!(
            !session
                .preview_visibility
                .blocked(20, SourceKind::Eve, &context)
        );
        assert!(
            session
                .preview_visibility
                .blocked(30, SourceKind::Custom, &context)
        );
        session.remove_window(10);
        let context = VisibilityContext::new(&display, &config, &session, 2);
        assert!(
            !session
                .preview_visibility
                .blocked(10, SourceKind::Eve, &context)
        );
        assert!(
            session
                .preview_visibility
                .blocked(30, SourceKind::Custom, &context)
        );
    }
}
