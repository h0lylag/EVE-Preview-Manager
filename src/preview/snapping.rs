//! Thumbnail edge snapping
//!
//! Calculates snap positions when dragging thumbnails near other thumbnails.
//! Supports edge-to-edge and alignment snapping within a configurable threshold.

use crate::types::Position;

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn left(&self) -> i16 {
        self.x
    }

    pub fn right(&self) -> i16 {
        // Use saturating_add to prevent overflow when x + width > i16::MAX
        self.x.saturating_add(self.width as i16)
    }

    pub fn top(&self) -> i16 {
        self.y
    }

    pub fn bottom(&self) -> i16 {
        // Use saturating_add to prevent overflow when y + height > i16::MAX
        self.y.saturating_add(self.height as i16)
    }
}

#[derive(Debug)]
struct SnapCandidate {
    offset: i16,
    distance: i16,
}

/// Find the best snap position for a dragged thumbnail
/// Returns position if snapping should occur, None otherwise
pub fn find_snap_position(dragged: Rect, others: &[Rect], threshold: u16) -> Option<Position> {
    if threshold == 0 {
        return None; // Snapping disabled
    }

    let mut best_x: Option<SnapCandidate> = None;
    let mut best_y: Option<SnapCandidate> = None;
    let threshold = threshold as i16;

    for other in others {
        // Horizontal snapping (X-axis)
        // Edge-to-edge snapping (always allowed)
        check_snap(&mut best_x, dragged.left(), other.right(), threshold);
        check_snap(&mut best_x, dragged.right(), other.left(), threshold);

        // Alignment snapping (only if windows overlap or are close on Y-axis)
        // Check if there's vertical overlap or proximity
        let vertical_overlap = dragged.bottom() >= other.top() && dragged.top() <= other.bottom();
        let vertical_proximity = (dragged.top() - other.bottom()).abs() <= threshold
            || (dragged.bottom() - other.top()).abs() <= threshold;

        if vertical_overlap || vertical_proximity {
            check_snap(&mut best_x, dragged.left(), other.left(), threshold);
            check_snap(&mut best_x, dragged.right(), other.right(), threshold);
        }

        // Vertical snapping (Y-axis)
        // Edge-to-edge snapping (always allowed)
        check_snap(&mut best_y, dragged.top(), other.bottom(), threshold);
        check_snap(&mut best_y, dragged.bottom(), other.top(), threshold);

        // Alignment snapping (only if windows overlap or are close on X-axis)
        // Check if there's horizontal overlap or proximity
        let horizontal_overlap = dragged.right() >= other.left() && dragged.left() <= other.right();
        let horizontal_proximity = (dragged.left() - other.right()).abs() <= threshold
            || (dragged.right() - other.left()).abs() <= threshold;

        if horizontal_overlap || horizontal_proximity {
            check_snap(&mut best_y, dragged.top(), other.top(), threshold);
            check_snap(&mut best_y, dragged.bottom(), other.bottom(), threshold);
        }
    }

    // Apply snaps if found
    let snap_x = best_x.map(|s| dragged.x + s.offset);
    let snap_y = best_y.map(|s| dragged.y + s.offset);

    match (snap_x, snap_y) {
        (Some(x), Some(y)) => Some(Position::new(x, y)),
        (Some(x), None) => Some(Position::new(x, dragged.y)),
        (None, Some(y)) => Some(Position::new(dragged.x, y)),
        (None, None) => None,
    }
}

fn check_snap(best: &mut Option<SnapCandidate>, edge: i16, target: i16, threshold: i16) {
    let distance = (edge - target).abs();
    if distance <= threshold {
        let candidate = SnapCandidate {
            offset: target - edge,
            distance,
        };

        // Keep this candidate if it's closer than the current best
        if best
            .as_ref()
            .is_none_or(|b| candidate.distance < b.distance)
        {
            *best = Some(candidate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snap_disabled_when_threshold_zero() {
        let dragged = Rect {
            x: 100,
            y: 100,
            width: 50,
            height: 50,
        };
        let other = Rect {
            x: 155,
            y: 100,
            width: 50,
            height: 50,
        };
        let result = find_snap_position(dragged, &[other], 0);
        assert_eq!(result, None);
    }

    #[test]
    fn test_snap_left_edge_to_right_edge() {
        let dragged = Rect {
            x: 100,
            y: 100,
            width: 50,
            height: 50,
        };
        let other = Rect {
            x: 160,
            y: 100,
            width: 50,
            height: 50,
        };
        // Dragged right edge at 150, other left at 160 - distance 10, within threshold 15
        let result = find_snap_position(dragged, &[other], 15);
        assert_eq!(result, Some(Position::new(110, 100))); // Snapped: dragged.x moves by 10
    }

    #[test]
    fn test_snap_right_edge_to_left_edge() {
        let dragged = Rect {
            x: 100,
            y: 100,
            width: 50,
            height: 50,
        };
        let other = Rect {
            x: 40,
            y: 100,
            width: 50,
            height: 50,
        };
        // Dragged left edge at 100, other right at 90 - distance 10
        let result = find_snap_position(dragged, &[other], 15);
        assert_eq!(result, Some(Position::new(90, 100))); // Snapped: dragged.x moves to 90
    }

    #[test]
    fn test_align_left_edges() {
        let dragged = Rect {
            x: 105,
            y: 100,
            width: 50,
            height: 50,
        };
        // Other window in same row (Y exactly aligned) for horizontal alignment only
        let other = Rect {
            x: 100,
            y: 100,
            width: 50,
            height: 50,
        };
        let result = find_snap_position(dragged, &[other], 15);
        assert_eq!(result, Some(Position::new(100, 100))); // X aligned to 100
    }

    #[test]
    fn test_align_right_edges() {
        let dragged = Rect {
            x: 100,
            y: 100,
            width: 50,
            height: 50,
        };
        // Other window in same row (Y exactly aligned) for horizontal alignment only
        let other = Rect {
            x: 95,
            y: 100,
            width: 50,
            height: 50,
        };
        // Dragged right: 150, other right: 145, distance 5
        let result = find_snap_position(dragged, &[other], 15);
        assert_eq!(result, Some(Position::new(95, 100))); // X moves by -5
    }

    #[test]
    fn test_snap_top_edge_to_bottom_edge() {
        let dragged = Rect {
            x: 100,
            y: 100,
            width: 50,
            height: 50,
        };
        let other = Rect {
            x: 100,
            y: 160,
            width: 50,
            height: 50,
        };
        let result = find_snap_position(dragged, &[other], 15);
        assert_eq!(result, Some(Position::new(100, 110))); // Y snapped
    }

    #[test]
    fn test_snap_bottom_edge_to_top_edge() {
        let dragged = Rect {
            x: 100,
            y: 100,
            width: 50,
            height: 50,
        };
        let other = Rect {
            x: 100,
            y: 40,
            width: 50,
            height: 50,
        };
        let result = find_snap_position(dragged, &[other], 15);
        assert_eq!(result, Some(Position::new(100, 90))); // Y snapped
    }

    #[test]
    fn test_align_top_edges() {
        let dragged = Rect {
            x: 100,
            y: 105,
            width: 50,
            height: 50,
        };
        // Other window in same column (X exactly aligned) for vertical alignment only
        let other = Rect {
            x: 100,
            y: 100,
            width: 50,
            height: 50,
        };
        let result = find_snap_position(dragged, &[other], 15);
        assert_eq!(result, Some(Position::new(100, 100))); // Y aligned
    }

    #[test]
    fn test_both_axes_snap() {
        let dragged = Rect {
            x: 105,
            y: 105,
            width: 50,
            height: 50,
        };
        let other = Rect {
            x: 100,
            y: 100,
            width: 50,
            height: 50,
        };
        let result = find_snap_position(dragged, &[other], 15);
        assert_eq!(result, Some(Position::new(100, 100))); // Both X and Y snap
    }

    #[test]
    fn test_no_snap_when_too_far() {
        let dragged = Rect {
            x: 100,
            y: 100,
            width: 50,
            height: 50,
        };
        let other = Rect {
            x: 200,
            y: 200,
            width: 50,
            height: 50,
        };
        let result = find_snap_position(dragged, &[other], 15);
        assert_eq!(result, None); // Too far to snap
    }

    #[test]
    fn test_chooses_closest_snap_target() {
        let dragged = Rect {
            x: 100,
            y: 100,
            width: 50,
            height: 50,
        };
        let close = Rect {
            x: 155,
            y: 100,
            width: 50,
            height: 50,
        }; // 5 pixels away
        let far = Rect {
            x: 165,
            y: 100,
            width: 50,
            height: 50,
        }; // 15 pixels away
        let result = find_snap_position(dragged, &[close, far], 20);
        assert_eq!(result, Some(Position::new(105, 100))); // Snaps to closer one
    }

    #[test]
    fn test_multiple_windows_independent_axes() {
        let dragged = Rect {
            x: 105,
            y: 205,
            width: 50,
            height: 50,
        };
        // snap_x in same row (Y within threshold) for horizontal alignment
        let snap_x = Rect {
            x: 100,
            y: 200,
            width: 50,
            height: 50,
        };
        // snap_y in same column (X within threshold) for vertical alignment
        let snap_y = Rect {
            x: 100,
            y: 200,
            width: 50,
            height: 50,
        };
        let result = find_snap_position(dragged, &[snap_x, snap_y], 15);
        assert_eq!(result, Some(Position::new(100, 200))); // X from first, Y from second
    }
}
