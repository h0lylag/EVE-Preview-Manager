use std::collections::HashMap;

use tracing::warn;
use x11rb::protocol::xproto::Window;

use super::cycle_state::CycleState;
use super::font::FontRenderer;
use super::thumbnail::Thumbnail;
use crate::common::types::ThumbnailState;
use crate::config::DisplayConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusedBorderUpdate {
    Focus(Window),
    Unfocus(Window),
}

fn plan_focused_border_updates<I>(states: I, focused_window: Window) -> Vec<FocusedBorderUpdate>
where
    I: IntoIterator<Item = (Window, ThumbnailState)>,
{
    states
        .into_iter()
        .filter_map(|(window, state)| match (window == focused_window, state) {
            (true, ThumbnailState::Normal { focused: true }) => None,
            (true, _) => Some(FocusedBorderUpdate::Focus(window)),
            (false, ThumbnailState::Normal { focused: true }) => {
                Some(FocusedBorderUpdate::Unfocus(window))
            }
            (false, _) => None,
        })
        .collect()
}

pub(crate) fn sync_focused_borders(
    eve_clients: &mut HashMap<Window, Thumbnail<'_>>,
    cycle_state: &CycleState,
    display_config: &DisplayConfig,
    font_renderer: &FontRenderer,
    focused_window: Window,
    reason: &str,
) {
    let updates = plan_focused_border_updates(
        eve_clients
            .iter()
            .map(|(window, thumbnail)| (*window, thumbnail.state)),
        focused_window,
    );

    for update in updates {
        match update {
            FocusedBorderUpdate::Focus(window) => {
                if let Some(thumbnail) = eve_clients.get_mut(&window) {
                    thumbnail.state = ThumbnailState::Normal { focused: true };
                    if let Err(e) = thumbnail.border(
                        display_config,
                        true,
                        cycle_state.is_skipped(&thumbnail.character_name),
                        font_renderer,
                    ) {
                        warn!(
                            window = window,
                            character = %thumbnail.character_name,
                            reason = %reason,
                            error = %e,
                            "Failed to draw focused border"
                        );
                    }
                }
            }
            FocusedBorderUpdate::Unfocus(window) => {
                if let Some(thumbnail) = eve_clients.get_mut(&window) {
                    thumbnail.state = ThumbnailState::Normal { focused: false };
                    if let Err(e) = thumbnail.border(
                        display_config,
                        false,
                        cycle_state.is_skipped(&thumbnail.character_name),
                        font_renderer,
                    ) {
                        warn!(
                            window = window,
                            character = %thumbnail.character_name,
                            reason = %reason,
                            error = %e,
                            "Failed to clear focused border"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREVIOUS: Window = 100;
    const NEXT: Window = 200;
    const OTHER: Window = 300;
    const MINIMIZED: Window = 400;

    fn plan(
        states: &[(Window, ThumbnailState)],
        focused_window: Window,
    ) -> Vec<FocusedBorderUpdate> {
        plan_focused_border_updates(states.iter().copied(), focused_window)
    }

    #[test]
    fn switching_focus_updates_previous_and_next() {
        let updates = plan(
            &[
                (PREVIOUS, ThumbnailState::Normal { focused: true }),
                (NEXT, ThumbnailState::Normal { focused: false }),
            ],
            NEXT,
        );

        assert_eq!(
            updates,
            vec![
                FocusedBorderUpdate::Unfocus(PREVIOUS),
                FocusedBorderUpdate::Focus(NEXT)
            ]
        );
    }

    #[test]
    fn already_focused_target_emits_no_updates() {
        let updates = plan(&[(NEXT, ThumbnailState::Normal { focused: true })], NEXT);

        assert!(updates.is_empty());
    }

    #[test]
    fn unrelated_unfocused_thumbnails_emit_no_updates() {
        let updates = plan(
            &[
                (NEXT, ThumbnailState::Normal { focused: true }),
                (OTHER, ThumbnailState::Normal { focused: false }),
            ],
            NEXT,
        );

        assert!(updates.is_empty());
    }

    #[test]
    fn minimized_non_target_emits_no_updates() {
        let updates = plan(
            &[
                (NEXT, ThumbnailState::Normal { focused: true }),
                (MINIMIZED, ThumbnailState::Minimized),
            ],
            NEXT,
        );

        assert!(updates.is_empty());
    }

    #[test]
    fn minimized_target_is_focused() {
        let updates = plan(&[(MINIMIZED, ThumbnailState::Minimized)], MINIMIZED);

        assert_eq!(updates, vec![FocusedBorderUpdate::Focus(MINIMIZED)]);
    }

    #[test]
    fn multiple_stale_focused_thumbnails_are_cleared() {
        let updates = plan(
            &[
                (PREVIOUS, ThumbnailState::Normal { focused: true }),
                (OTHER, ThumbnailState::Normal { focused: true }),
                (NEXT, ThumbnailState::Normal { focused: true }),
            ],
            NEXT,
        );

        assert_eq!(
            updates,
            vec![
                FocusedBorderUpdate::Unfocus(PREVIOUS),
                FocusedBorderUpdate::Unfocus(OTHER)
            ]
        );
    }
}
