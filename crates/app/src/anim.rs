//! Motion constants shared by the interactive board.
//!
//! The curves and durations are the ones the drag probe measured and the egui
//! app already uses (`crates/app/src/anim.rs`): `cubic-bezier(0.20, 0, 0, 1)`
//! for reflow and lift, `cubic-bezier(0.40, 0, 0.20, 1)` for snap-back.
//! Tune timing here only.

use std::time::Duration;

/// Lift of the card being dragged.
pub const LIFT: Duration = Duration::from_millis(140);
/// Reflow of the drop indicator and the target gap.
pub const REFLOW: Duration = Duration::from_millis(160);
/// Glide of an in-flow card into its new slot.
pub const SHIFT: Duration = Duration::from_millis(200);
/// Extra delay per card the moving card is away from the gap, producing the
/// subtle wave when several cards make room at once.
pub const STAGGER_STEP: Duration = Duration::from_millis(20);
/// Cap on the per-card stagger so far-away cards still move promptly.
pub const STAGGER_MAX: Duration = Duration::from_millis(100);
/// Return of a cancelled drag, and the settle flight of a completed one.
pub const SNAP_BACK: Duration = Duration::from_millis(260);

/// CSS `cubic-bezier` sampling, same WebKit/Blink algorithm as `anim.rs`.
pub fn cubic_bezier(x: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let bezier_a = |a1: f32, a2: f32| 1.0 - 3.0 * a2 + 3.0 * a1;
    let bezier_b = |a1: f32, a2: f32| 3.0 * a2 - 6.0 * a1;
    let bezier_c = |a1: f32| 3.0 * a1;
    let calc = |t: f32, a1: f32, a2: f32| {
        ((bezier_a(a1, a2) * t + bezier_b(a1, a2)) * t + bezier_c(a1)) * t
    };
    let slope = |t: f32, a1: f32, a2: f32| {
        3.0 * bezier_a(a1, a2) * t * t + 2.0 * bezier_b(a1, a2) * t + bezier_c(a1)
    };

    let mut t = x;
    if slope(0.0, x1, x2) < 0.001 || slope(1.0, x1, x2) < 0.001 {
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..10 {
            t = lo + (hi - lo) * 0.5;
            let value = calc(t, x1, x2) - x;
            if value.abs() < 1e-7 {
                break;
            }
            if value > 0.0 {
                hi = t;
            } else {
                lo = t;
            }
        }
    } else {
        for _ in 0..4 {
            let s = slope(t, x1, x2);
            if s.abs() < 0.001 {
                break;
            }
            t -= (calc(t, x1, x2) - x) / s;
        }
    }
    calc(t, y1, y2)
}

/// Easing for lift and reflow.
pub fn ease_standard(t: f32) -> f32 {
    cubic_bezier(t, 0.20, 0.00, 0.00, 1.00)
}

/// Easing for snap-back.
pub fn ease_snap(t: f32) -> f32 {
    cubic_bezier(t, 0.40, 0.00, 0.20, 1.00)
}

/// An eased value, or the resting value when the system asks for reduced
/// motion.
pub fn eased(reduced: bool, t: f32, ease: fn(f32) -> f32) -> f32 {
    if reduced { 1.0 } else { ease(t) }
}

/// How far a card at `index` trails the gap when it makes room for it.
///
/// Cards above the gap do not move and get no delay; cards below it wait one
/// `STAGGER_STEP` per slot they are away from the gap.
pub fn stagger_order(gap: Option<usize>, index: usize) -> u32 {
    match gap {
        Some(gap) if index >= gap => (index - gap) as u32,
        _ => 0,
    }
}

/// Visual offset (`top`) a card at `index` starts from when the gap moves
/// from `previous` to `next`, before gliding back to zero.
///
/// The gap and every card row are the same height, so any card that ends up
/// on the other side of the gap moved by exactly `gap_height`. The returned
/// value is the old position minus the new one: negative means the card
/// starts above its new slot, positive means below.
pub fn shift_offset(
    previous: Option<usize>,
    next: Option<usize>,
    index: usize,
    gap_height: f32,
) -> f32 {
    match (previous, next) {
        // Gap opened in a column that had none: cards below moved down.
        (None, Some(next)) if index >= next => -gap_height,
        // Gap closed: cards below moved back up.
        (Some(previous), None) if index >= previous => gap_height,
        // Gap moved down: cards between the two slots moved up.
        (Some(previous), Some(next)) if next > previous && index >= previous && index < next => {
            gap_height
        }
        // Gap moved up: cards between the two slots moved down.
        (Some(previous), Some(next)) if previous > next && index >= next && index < previous => {
            -gap_height
        }
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_are_pinned_at_both_ends() {
        assert_eq!(ease_standard(0.0), 0.0);
        assert_eq!(ease_standard(1.0), 1.0);
        assert_eq!(ease_snap(0.0), 0.0);
        assert_eq!(ease_snap(1.0), 1.0);
    }

    #[test]
    fn standard_curve_front_loads() {
        // cubic-bezier(0.2, 0, 0, 1) is past the halfway point early.
        assert!(ease_standard(0.5) > 0.6);
        assert!(ease_standard(0.1) < ease_snap(0.1) + 1.0);
    }

    #[test]
    fn reduced_motion_returns_the_resting_value() {
        assert_eq!(eased(true, 0.0, ease_standard), 1.0);
        assert!(eased(false, 0.5, ease_standard) < 1.0);
    }

    #[test]
    fn a_gap_opening_pushes_the_cards_below_it_down() {
        // The layout moves the cards below down; they start at a negative
        // offset (above the new slot) and glide down to zero.
        assert_eq!(shift_offset(None, Some(1), 0, 40.0), 0.0);
        assert_eq!(shift_offset(None, Some(1), 1, 40.0), -40.0);
        assert_eq!(shift_offset(None, Some(1), 2, 40.0), -40.0);
    }

    #[test]
    fn a_gap_closing_pulls_the_cards_below_it_back_up() {
        assert_eq!(shift_offset(Some(2), None, 1, 40.0), 0.0);
        assert_eq!(shift_offset(Some(2), None, 2, 40.0), 40.0);
    }

    #[test]
    fn moving_the_gap_moves_only_the_cards_in_between() {
        // Gap moves from slot 1 down to slot 3: cards 1 and 2 moved up.
        assert_eq!(shift_offset(Some(1), Some(3), 0, 40.0), 0.0);
        assert_eq!(shift_offset(Some(1), Some(3), 1, 40.0), 40.0);
        assert_eq!(shift_offset(Some(1), Some(3), 2, 40.0), 40.0);
        assert_eq!(shift_offset(Some(1), Some(3), 3, 40.0), 0.0);
        // Gap moves from slot 3 up to slot 1: cards 1 and 2 moved down.
        assert_eq!(shift_offset(Some(3), Some(1), 1, 40.0), -40.0);
        assert_eq!(shift_offset(Some(3), Some(1), 2, 40.0), -40.0);
        assert_eq!(shift_offset(Some(3), Some(1), 3, 40.0), 0.0);
    }

    #[test]
    fn stagger_grows_below_the_gap_and_stops_above_it() {
        assert_eq!(stagger_order(Some(2), 1), 0);
        assert_eq!(stagger_order(Some(2), 2), 0);
        assert_eq!(stagger_order(Some(2), 3), 1);
        assert_eq!(stagger_order(Some(2), 5), 3);
        assert_eq!(stagger_order(None, 4), 0);
    }
}
