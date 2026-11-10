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
/// Return of a cancelled drag.
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
}
