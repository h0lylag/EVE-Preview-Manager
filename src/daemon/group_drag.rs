//! State and pure transformations for moving visible thumbnails as a group.

use x11rb::protocol::xproto::{KeyButMask, Window};

use crate::common::constants::mouse;
use crate::common::types::Position;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GroupDragMember {
    pub source_window: Window,
    pub start_position: Position,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChordButtons {
    Left,
    Right,
    Both,
}

impl ChordButtons {
    fn contains_left(self) -> bool {
        matches!(self, Self::Left | Self::Both)
    }

    fn contains_right(self) -> bool {
        matches!(self, Self::Right | Self::Both)
    }

    fn intersect(self, state: KeyButMask) -> Option<Self> {
        match (
            self.contains_left() && state.contains(KeyButMask::BUTTON1),
            self.contains_right() && state.contains(KeyButMask::BUTTON3),
        ) {
            (true, true) => Some(Self::Both),
            (true, false) => Some(Self::Left),
            (false, true) => Some(Self::Right),
            (false, false) => None,
        }
    }

    fn after_release(self, released_button: u8) -> Option<Self> {
        match (self, released_button) {
            (Self::Both, mouse::BUTTON_LEFT) => Some(Self::Right),
            (Self::Both, mouse::BUTTON_RIGHT) => Some(Self::Left),
            (Self::Left, mouse::BUTTON_LEFT) | (Self::Right, mouse::BUTTON_RIGHT) => None,
            (remaining, _) => Some(remaining),
        }
    }

    fn contains_button(self, button: u8) -> bool {
        match button {
            mouse::BUTTON_LEFT => self.contains_left(),
            mouse::BUTTON_RIGHT => self.contains_right(),
            _ => false,
        }
    }

    fn with_button(self, button: u8) -> Self {
        match button {
            mouse::BUTTON_LEFT if self.contains_right() => Self::Both,
            mouse::BUTTON_LEFT => Self::Left,
            mouse::BUTTON_RIGHT if self.contains_left() => Self::Both,
            mouse::BUTTON_RIGHT => Self::Right,
            _ => self,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GroupDragDelta {
    x: i32,
    y: i32,
}

#[derive(Debug, Default)]
pub(super) enum GroupDragState {
    #[default]
    Idle,
    Active {
        anchor: Window,
        pointer_start: Position,
        members: Vec<GroupDragMember>,
    },
    SuppressingRelease(ChordButtons),
}

impl GroupDragState {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active { .. })
    }

    pub fn finish_active(&mut self, released_button: u8) -> Option<Vec<GroupDragMember>> {
        let Self::Active { members, .. } = std::mem::take(self) else {
            return None;
        };

        *self = match released_button {
            mouse::BUTTON_LEFT => Self::SuppressingRelease(ChordButtons::Right),
            mouse::BUTTON_RIGHT => Self::SuppressingRelease(ChordButtons::Left),
            _ => Self::Idle,
        };
        Some(members)
    }

    pub fn consume_suppressed_release(&mut self, released_button: u8) -> bool {
        let Self::SuppressingRelease(remaining) = self else {
            return false;
        };

        if !remaining.contains_button(released_button) {
            return false;
        }

        if let Some(remaining) = remaining.after_release(released_button) {
            *self = Self::SuppressingRelease(remaining);
        } else {
            *self = Self::Idle;
        }
        true
    }

    pub fn should_suppress_press(
        &mut self,
        pressed_button: u8,
        buttons_before_press: KeyButMask,
    ) -> bool {
        let Self::SuppressingRelease(remaining) = self else {
            return false;
        };

        let Some(still_held) = remaining.intersect(buttons_before_press) else {
            *self = Self::Idle;
            return false;
        };

        *self = Self::SuppressingRelease(still_held);
        if !matches!(pressed_button, mouse::BUTTON_LEFT | mouse::BUTTON_RIGHT) {
            return false;
        }

        *self = Self::SuppressingRelease(still_held.with_button(pressed_button));
        true
    }

    pub fn anchor(&self) -> Option<Window> {
        match self {
            Self::Active { anchor, .. } => Some(*anchor),
            Self::Idle | Self::SuppressingRelease(_) => None,
        }
    }

    /// Stops tracking a non-anchor preview that disappeared during a drag.
    pub fn remove_member(&mut self, source_window: Window) {
        if let Self::Active { members, .. } = self {
            members.retain(|member| member.source_window != source_window);
        }
    }

    /// Cancels an active chord and returns its captured layout for restoration.
    pub fn cancel_active(&mut self) -> Option<Vec<GroupDragMember>> {
        let Self::Active { members, .. } = std::mem::take(self) else {
            return None;
        };

        *self = Self::SuppressingRelease(ChordButtons::Both);
        Some(members)
    }
}

pub(super) fn is_group_chord_press(detail: u8, state: KeyButMask) -> bool {
    (detail == mouse::BUTTON_LEFT && state.contains(KeyButMask::BUTTON3))
        || (detail == mouse::BUTTON_RIGHT && state.contains(KeyButMask::BUTTON1))
}

pub(super) fn shared_delta(
    members: &[GroupDragMember],
    pointer_start: Position,
    pointer_now: Position,
) -> GroupDragDelta {
    if members.is_empty() {
        return GroupDragDelta { x: 0, y: 0 };
    }

    let desired_x = i32::from(pointer_now.x) - i32::from(pointer_start.x);
    let desired_y = i32::from(pointer_now.y) - i32::from(pointer_start.y);
    let minimum_x = members
        .iter()
        .map(|member| i32::from(i16::MIN) - i32::from(member.start_position.x))
        .max()
        .expect("non-empty group has a minimum x delta");
    let maximum_x = members
        .iter()
        .map(|member| i32::from(i16::MAX) - i32::from(member.start_position.x))
        .min()
        .expect("non-empty group has a maximum x delta");
    let minimum_y = members
        .iter()
        .map(|member| i32::from(i16::MIN) - i32::from(member.start_position.y))
        .max()
        .expect("non-empty group has a minimum y delta");
    let maximum_y = members
        .iter()
        .map(|member| i32::from(i16::MAX) - i32::from(member.start_position.y))
        .min()
        .expect("non-empty group has a maximum y delta");

    GroupDragDelta {
        x: desired_x.clamp(minimum_x, maximum_x),
        y: desired_y.clamp(minimum_y, maximum_y),
    }
}

pub(super) fn translated_position(start: Position, delta: GroupDragDelta) -> Position {
    let x = i32::from(start.x) + delta.x;
    let y = i32::from(start.y) + delta.y;
    Position::new(
        i16::try_from(x).expect("shared group delta keeps x representable"),
        i16::try_from(y).expect("shared group delta keeps y representable"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_chord_in_either_press_order() {
        assert!(is_group_chord_press(
            mouse::BUTTON_LEFT,
            KeyButMask::BUTTON3
        ));
        assert!(is_group_chord_press(
            mouse::BUTTON_RIGHT,
            KeyButMask::BUTTON1
        ));
    }

    #[test]
    fn ordinary_button_presses_are_not_chords() {
        assert!(!is_group_chord_press(
            mouse::BUTTON_LEFT,
            KeyButMask::default()
        ));
        assert!(!is_group_chord_press(
            mouse::BUTTON_RIGHT,
            KeyButMask::default()
        ));
        assert!(!is_group_chord_press(
            mouse::BUTTON_LEFT,
            KeyButMask::BUTTON1
        ));
        assert!(!is_group_chord_press(
            mouse::BUTTON_RIGHT,
            KeyButMask::BUTTON3
        ));
    }

    #[test]
    fn translation_preserves_relative_offsets() {
        let pointer_start = Position::new(100, 200);
        let pointer_now = Position::new(135, 180);
        let members = [
            GroupDragMember {
                source_window: 1,
                start_position: Position::new(10, 20),
            },
            GroupDragMember {
                source_window: 2,
                start_position: Position::new(70, 95),
            },
        ];
        let delta = shared_delta(&members, pointer_start, pointer_now);
        let first = translated_position(members[0].start_position, delta);
        let second = translated_position(members[1].start_position, delta);

        assert_eq!(first, Position::new(45, 0));
        assert_eq!(second, Position::new(105, 75));
        assert_eq!(second.x - first.x, 60);
        assert_eq!(second.y - first.y, 75);
    }

    #[test]
    fn coordinate_bounds_preserve_relative_offsets() {
        let members = [
            GroupDragMember {
                source_window: 1,
                start_position: Position::new(i16::MAX - 10, 0),
            },
            GroupDragMember {
                source_window: 2,
                start_position: Position::new(i16::MAX - 20, 100),
            },
        ];
        let delta = shared_delta(&members, Position::new(0, 0), Position::new(100, 100));
        let first = translated_position(members[0].start_position, delta);
        let second = translated_position(members[1].start_position, delta);

        assert_eq!(first, Position::new(i16::MAX, 100));
        assert_eq!(second, Position::new(i16::MAX - 10, 200));
        assert_eq!(first.x - second.x, 10);
        assert_eq!(second.y - first.y, 100);
    }

    #[test]
    fn finishing_a_drag_suppresses_only_the_remaining_button() {
        for released_button in [mouse::BUTTON_LEFT, mouse::BUTTON_RIGHT] {
            let mut state = GroupDragState::Active {
                anchor: 1,
                pointer_start: Position::new(0, 0),
                members: Vec::new(),
            };

            assert!(state.finish_active(released_button).is_some());
            let remaining_button = match released_button {
                mouse::BUTTON_LEFT => mouse::BUTTON_RIGHT,
                mouse::BUTTON_RIGHT => mouse::BUTTON_LEFT,
                _ => unreachable!("test only uses chord buttons"),
            };
            assert!(state.consume_suppressed_release(remaining_button));
            assert!(matches!(state, GroupDragState::Idle));
        }
    }

    #[test]
    fn interrupted_drag_suppresses_both_release_orders() {
        for first_release in [mouse::BUTTON_LEFT, mouse::BUTTON_RIGHT] {
            let mut state = GroupDragState::Active {
                anchor: 1,
                pointer_start: Position::new(0, 0),
                members: Vec::new(),
            };
            assert!(state.cancel_active().is_some());

            assert!(state.consume_suppressed_release(first_release));
            assert!(matches!(state, GroupDragState::SuppressingRelease(_)));
            let second_release = match first_release {
                mouse::BUTTON_LEFT => mouse::BUTTON_RIGHT,
                mouse::BUTTON_RIGHT => mouse::BUTTON_LEFT,
                _ => unreachable!("test only uses chord buttons"),
            };
            assert!(state.consume_suppressed_release(second_release));
            assert!(matches!(state, GroupDragState::Idle));
        }
    }

    #[test]
    fn stale_suppression_recovers_on_a_new_click() {
        let mut state = GroupDragState::SuppressingRelease(ChordButtons::Both);

        assert!(!state.should_suppress_press(mouse::BUTTON_LEFT, KeyButMask::default()));
        assert!(matches!(state, GroupDragState::Idle));
    }

    #[test]
    fn chord_repress_is_suppressed_through_both_releases() {
        let mut state = GroupDragState::SuppressingRelease(ChordButtons::Right);

        assert!(state.should_suppress_press(mouse::BUTTON_LEFT, KeyButMask::BUTTON3));
        assert!(matches!(
            state,
            GroupDragState::SuppressingRelease(ChordButtons::Both)
        ));
        assert!(state.consume_suppressed_release(mouse::BUTTON_LEFT));
        assert!(state.consume_suppressed_release(mouse::BUTTON_RIGHT));
        assert!(matches!(state, GroupDragState::Idle));
    }

    #[test]
    fn suppression_does_not_consume_unrelated_button_releases() {
        let mut state = GroupDragState::SuppressingRelease(ChordButtons::Both);

        assert!(!state.consume_suppressed_release(2));
        assert!(matches!(
            state,
            GroupDragState::SuppressingRelease(ChordButtons::Both)
        ));
    }

    #[test]
    fn suppression_does_not_consume_unrelated_button_presses() {
        let mut state = GroupDragState::SuppressingRelease(ChordButtons::Right);

        assert!(!state.should_suppress_press(2, KeyButMask::BUTTON3));
        assert!(matches!(
            state,
            GroupDragState::SuppressingRelease(ChordButtons::Right)
        ));
    }

    #[test]
    fn destroying_member_keeps_drag_active() {
        let mut state = GroupDragState::Active {
            anchor: 1,
            pointer_start: Position::new(10, 20),
            members: vec![
                GroupDragMember {
                    source_window: 1,
                    start_position: Position::new(0, 0),
                },
                GroupDragMember {
                    source_window: 2,
                    start_position: Position::new(100, 100),
                },
            ],
        };

        state.remove_member(2);
        let GroupDragState::Active { members, .. } = state else {
            panic!("group drag should remain active");
        };
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].source_window, 1);
    }

    #[test]
    fn destroying_anchor_cancels_drag() {
        let mut state = GroupDragState::Active {
            anchor: 1,
            pointer_start: Position::new(10, 20),
            members: Vec::new(),
        };

        assert_eq!(state.anchor(), Some(1));
        assert!(state.cancel_active().is_some());
        assert!(matches!(
            state,
            GroupDragState::SuppressingRelease(ChordButtons::Both)
        ));
    }
}
