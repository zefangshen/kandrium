//! Trello-style motion for card drag-and-drop.
//!
//! Three coordinated pieces live here:
//!
//! * **FLIP tracks** — when in-flow cards change their laid-out rect (the
//!   insertion gap opens/closes/jumps, or a drop reorders the data), their
//!   painted shapes are translated from the old position to the new one with
//!   an easing curve and a small stagger. Only *paint* is translated
//!   (`PaintList::transform_range`); interaction rects stay on the real
//!   layout slots, so click/drag and drop-zone hit-testing never run against
//!   animated positions. Tracks are only created while a drag or ghost
//!   flight is active — scrolling, resizes and non-drag list edits snap
//!   instantly (translating paint also moves the clip rect, so animating
//!   scroll would let cards paint over chrome outside their ScrollArea).
//! * **Drag ghost** — the lifted card rendered in a foreground [`egui::Area`]
//!   while dragging (slightly scaled, shadowed, translucent) which then flies
//!   into its destination slot — or back to its source on cancel.
//! * **Insertion gap** — the placeholder slot that opens at the hovered
//!   insertion point; surrounding cards make room by reflowing, and the FLIP
//!   tracks turn that reflow into smooth motion.
//!
//! All state ([`CardRig`]) is kept in egui temp memory keyed by [`RIG_ID`],
//! so the widget tree needs no extra threading of animation parameters.
//!
//! # Tuning
//!
//! The feel of the whole system is defined by the `const`s and the two
//! easing functions below. Durations are in seconds. Trello's web client
//! moves cards on a fast decelerate curve in the 150–220ms range; the values
//! here mirror that profile.

use std::collections::HashMap;

use egui::{Color32, Id, Pos2, Rect, Ui, Vec2};
use kandrium_core::Card;

// -- Timing profile ---------------------------------------------------------
//
// Every duration pairs with one of the easing curves at the bottom of this
// module. Shrink durations for snappier feedback; lengthen for a heavier,
// more deliberate feel. Keep SETTLE_DURATION >= SHIFT_DURATION so the ghost
// never beats the surrounding cards into their new slots.

/// Pickup ramp (scale + shadow + opacity) when a card starts being dragged.
pub const LIFT_DURATION: f64 = 0.140;
/// In-flow cards sliding to their new slots.
pub const SHIFT_DURATION: f64 = 0.200;
/// Ghost flying into its final slot (or snapping back on a cancelled drop).
pub const SETTLE_DURATION: f64 = 0.220;
/// Insertion gap opening when the drag begins.
pub const GAP_DURATION: f64 = 0.160;
/// Extra start delay per card the moving card is away from the gap, producing
/// the subtle "wave" when several cards make room at once.
pub const STAGGER_STEP: f64 = 0.020;
/// Cap on the per-card stagger so far-away cards still move promptly.
pub const STAGGER_MAX: f64 = 0.100;

// -- Dragged-card look -------------------------------------------------------

/// Size multiplier at full lift (Trello lifts cards ~2-4%).
pub const DRAG_SCALE: f32 = 1.03;
/// Alpha multiplier at full lift. Deliberately close to 1: the card must stay
/// fully readable while clearly feeling "picked up".
pub const DRAG_OPACITY: f32 = 0.92;
/// Vertical shadow offset at rest / at full lift, in points.
pub const SHADOW_OFFSET_REST: f32 = 3.0;
pub const SHADOW_OFFSET_LIFT: f32 = 10.0;
/// Shadow blur radius at rest / at full lift, in points.
pub const SHADOW_BLUR_REST: f32 = 6.0;
pub const SHADOW_BLUR_LIFT: f32 = 20.0;
/// Shadow black-alpha at rest / at full lift (rest is effectively invisible,
/// lift is the pronounced Trello elevation).
pub const SHADOW_ALPHA_REST: f32 = 0.08;
pub const SHADOW_ALPHA_LIFT: f32 = 0.32;

/// Gap placeholder geometry/color (matches the chip's 6px radius).
const GAP_RADIUS: f32 = 6.0;
const GAP_FILL_ALPHA: u8 = 18;
const GAP_STROKE_ALPHA: u8 = 45;
/// Height used when the dragged card's real height is not known yet.
const GAP_FALLBACK_HEIGHT: f32 = 54.0;

/// Temp-memory key for the singleton rig.
fn rig_id() -> Id {
    Id::new("kandrium-card-rig")
}
/// Layer id of the floating ghost.
fn ghost_id() -> Id {
    Id::new("kandrium-drag-ghost")
}

// ---------------------------------------------------------------------------
// Motion state
// ---------------------------------------------------------------------------

/// One in-flight FLIP translation of a single card.
#[derive(Clone, Debug)]
struct Track {
    /// Center the card is moving from (its position before the reflow).
    from: Pos2,
    /// Center the card is moving towards; chased/updated while a gap jump is
    /// still moving the target.
    to: Pos2,
    /// Wall-clock time (`egui` input time, seconds) the first movement was seen.
    start: f64,
    /// Stagger delay before this card leaves `from`.
    delay: f64,
}

impl Track {
    /// Eased 0..1 progress, with the stagger delay applied.
    fn progress(&self, now: f64) -> f32 {
        ((now - self.start - self.delay) / SHIFT_DURATION).clamp(0.0, 1.0) as f32
    }

    fn done(&self, now: f64) -> bool {
        now - self.start - self.delay >= SHIFT_DURATION
    }

    fn center(&self, now: f64) -> Pos2 {
        let t = ease_standard(self.progress(now));
        lerp_pos(self.from, self.to, t)
    }
}

/// What the ghost is currently doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GhostKind {
    /// Following the pointer while the button is held.
    Floating,
    /// Button released: flying to the card's live slot (valid drop or cancel).
    Settling,
}

/// The lifted card rendered above the board.
#[derive(Clone, Debug)]
struct Ghost {
    /// Owned snapshot so ghost rendering never borrows view data.
    card: Card,
    /// Board badge text (All board only), captured at pickup.
    board_title: Option<String>,
    /// Unscaled chip size captured from the in-flow rect at pickup.
    size: Vec2,
    /// Pointer offset from the chip's top-left at pickup — keeps the grab
    /// point under the cursor instead of recentering the card.
    grab: Vec2,
    /// Wall-clock time the current phase (float / settle) started.
    started: f64,
    kind: GhostKind,
    /// Frozen visual rect when settling starts (the rect the flight begins
    /// from); `None` while floating.
    from_rect: Option<Rect>,
}

/// Singleton animation state stored in egui temp memory.
#[derive(Clone, Default)]
pub struct CardRig {
    /// Last in-flow rect per card id. Drives FLIP centers, the ghost grab
    /// offset/size and the settle target.
    rects: HashMap<String, Rect>,
    /// Cards currently gliding between positions.
    tracks: HashMap<String, Track>,
    /// The lifted card, if a drag is active or settling.
    ghost: Option<Ghost>,
    /// When the gap started opening (only set while a button is held).
    gap_started: Option<f64>,
    /// Id of the card currently held (gap stays open while this is `Some`).
    drag_id: Option<String>,
    /// Content rect of the placeholder slot painted this frame. While the
    /// ghost settles this IS the flight target: it is exactly where the real
    /// card's interaction rect will land once it re-enters the layout.
    gap_rect: Option<Rect>,
}

impl CardRig {
    fn load_mut<R>(ctx: &egui::Context, f: impl FnOnce(&mut CardRig) -> R) -> R {
        ctx.data_mut(|d| f(d.get_temp_mut_or_default(rig_id())))
    }

    /// Begin a drag: snapshot the held card and start the lift/gap clock.
    pub fn begin_drag(ctx: &egui::Context, card: Card, board_title: Option<String>, pointer: Pos2) {
        let now = ctx.input(|i| i.time);
        Self::load_mut(ctx, |rig| {
            let card_id = card.id.clone();
            // Start from a clean motion slate: tracks from the previous drag
            // cycle must not translate the freshly picked-up card.
            rig.tracks.remove(&card_id);
            rig.gap_rect = None;
            if let Some(rect) = rig.rects.get(&card_id).copied() {
                rig.ghost = Some(Ghost {
                    card,
                    board_title,
                    size: rect.size(),
                    grab: pointer - rect.min,
                    started: now,
                    kind: GhostKind::Floating,
                    from_rect: None,
                });
                rig.drag_id = Some(card_id);
                rig.gap_started = Some(now);
            }
        });
    }

    /// Button released: freeze the current floating rect and fly to the
    /// card's live slot. `pointer` is `None` if the pointer left the window;
    /// in that case the ghost is dropped without a flight.
    pub fn begin_settle(ctx: &egui::Context, pointer: Option<Pos2>) {
        let now = ctx.input(|i| i.time);
        Self::load_mut(ctx, |rig| {
            rig.drag_id = None;
            rig.gap_started = None;
            let Some(ghost) = rig.ghost.as_mut() else {
                return;
            };
            let Some(pointer) = pointer else {
                rig.ghost = None;
                return;
            };
            let lift = lift_progress(now, ghost.started);
            let scale = 1.0 + (DRAG_SCALE - 1.0) * lift;
            let size = ghost.size * scale;
            // Anchor the scaled rect to the grab point, exactly as while
            // floating, so the flight starts without a positional jump.
            let min = pointer - ghost.grab * scale;
            ghost.from_rect = Some(Rect::from_min_size(min, size));
            ghost.kind = GhostKind::Settling;
            ghost.started = now;
        });
    }

    /// Forget all drag-related motion (tab/view switches cancel a drag).
    pub fn clear_drag(ctx: &egui::Context) {
        Self::load_mut(ctx, |rig| {
            rig.ghost = None;
            rig.drag_id = None;
            rig.gap_started = None;
            rig.gap_rect = None;
            rig.tracks.clear();
        });
    }

    /// True while the ghost is flying to its slot after a button release.
    pub fn is_settling(ctx: &egui::Context) -> bool {
        Self::load_mut(ctx, |rig| {
            rig.ghost
                .as_ref()
                .is_some_and(|g| g.kind == GhostKind::Settling)
        })
    }

    /// Card id of the settling ghost, so views keep hiding the real card
    /// until the flight completes (the placeholder stands in for it).
    pub fn settling_card_id(ctx: &egui::Context) -> Option<String> {
        Self::load_mut(ctx, |rig| {
            rig.ghost
                .as_ref()
                .filter(|g| g.kind == GhostKind::Settling)
                .map(|g| g.card.id.clone())
        })
    }

    /// Record the placeholder slot's content rect (called from [`gap_slot`]).
    pub(crate) fn note_gap_rect(ctx: &egui::Context, rect: Rect) {
        Self::load_mut(ctx, |rig| rig.gap_rect = Some(rect));
    }

    /// Record that a card has been laid out at `rect` this frame and return
    /// the paint translation needed to glide it from its previous position.
    ///
    /// `stagger_order` is the card's distance (in card slots) below the open
    /// insertion gap — 0 for the nearest card and cards above the gap.
    ///
    /// The returned delta must be applied to *painted shapes only*; the
    /// interaction rect reported to egui stays at `rect`.
    pub fn card_laid_out(
        ctx: &egui::Context,
        card_id: &str,
        rect: Rect,
        stagger_order: u32,
    ) -> Vec2 {
        let now = ctx.input(|i| i.time);
        Self::load_mut(ctx, |rig| {
            let prev = rig.rects.insert(card_id.to_string(), rect);

            // While the ghost flies home, the in-flow instance already sits
            // at the destination: no FLIP for it (avoid a double image).
            let is_settling = rig
                .ghost
                .as_ref()
                .is_some_and(|g| g.kind == GhostKind::Settling && g.card.id == card_id);
            if is_settling {
                rig.tracks.remove(card_id);
                return Vec2::ZERO;
            }

            if let Some(prev) = prev {
                // FLIP glides exist for drag lifecycle motion only (gap
                // opening/jumping, drop reflow). Scrolling, window resizes
                // and non-drag list edits shift every rect at once;
                // animating those lets cards fly over chrome outside their
                // scroll clip, because `transform_range` moves the clip
                // rect too. In-flight tracks still finish below.
                let drag_active = rig.drag_id.is_some() || rig.ghost.is_some();
                if drag_active && (prev.center() - rect.center()).length() > 0.5 {
                    match rig.tracks.get_mut(card_id) {
                        // Target keeps moving while the gap jumps around:
                        // chase it without restarting the clock.
                        Some(track) => track.to = rect.center(),
                        None => {
                            rig.tracks.insert(
                                card_id.to_string(),
                                Track {
                                    from: prev.center(),
                                    to: rect.center(),
                                    start: now,
                                    delay: (stagger_order as f64 * STAGGER_STEP).min(STAGGER_MAX),
                                },
                            );
                        }
                    }
                }
            }

            match rig.tracks.get(card_id) {
                Some(track) if !track.done(now) => track.center(now) - rect.center(),
                Some(_) => {
                    rig.tracks.remove(card_id);
                    Vec2::ZERO
                }
                None => Vec2::ZERO,
            }
        })
    }

    /// Current animated height of the insertion gap (0 while not dragging).
    pub fn gap_height(ctx: &egui::Context) -> f32 {
        let now = ctx.input(|i| i.time);
        Self::load_mut(ctx, |rig| {
            // While the ghost flies home the placeholder stays fully open as
            // a transparent indicator — it is removed together with the ghost
            // once the flight completes.
            if let Some(ghost) = &rig.ghost
                && ghost.kind == GhostKind::Settling
            {
                return ghost.size.y;
            }
            match (rig.drag_id.as_ref(), rig.gap_started) {
                (Some(id), Some(start)) => {
                    let base = rig
                        .rects
                        .get(id)
                        .map(|r| r.height())
                        .unwrap_or(GAP_FALLBACK_HEIGHT);
                    let t = ease_standard(((now - start) / GAP_DURATION).clamp(0.0, 1.0) as f32);
                    base * t
                }
                _ => 0.0,
            }
        })
    }

    /// True while any animation still needs frames pumped.
    pub fn needs_repaint(ctx: &egui::Context) -> bool {
        Self::load_mut(ctx, |rig| {
            rig.ghost.is_some() || !rig.tracks.is_empty() || rig.drag_id.is_some()
        })
    }

    /// Render the ghost (if any) on a foreground layer and advance/finish
    /// its lifecycle. Must be called AFTER the board views so the settle
    /// target rects are already recorded.
    pub fn draw_ghost(ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);

        // Prune finished tracks even for cards scrolled out of view.
        Self::load_mut(ctx, |rig| rig.tracks.retain(|_, t| !t.done(now)));

        // Compute this frame's ghost placement + look. NB: read input BEFORE
        // taking the rig lock — egui's `input` and `data_mut` share the same
        // context RwLock, so nesting them would deadlock.
        let pointer = ctx.input(|i| i.pointer.latest_pos());
        let visual = Self::load_mut(ctx, |rig| {
            let ghost = rig.ghost.as_mut()?;
            match ghost.kind {
                GhostKind::Floating => {
                    let pointer = pointer?;
                    let lift = lift_progress(now, ghost.started);
                    let scale = 1.0 + (DRAG_SCALE - 1.0) * lift;
                    let size = ghost.size * scale;
                    let min = pointer - ghost.grab * scale;
                    Some(GhostVisual {
                        rect: Rect::from_min_size(min, size),
                        lift,
                        card: ghost.card.clone(),
                        board_title: ghost.board_title.clone(),
                    })
                }
                GhostKind::Settling => {
                    // Fly to the placeholder slot (exactly where the real
                    // card will re-enter); fall back to the last known rect
                    // if the gap is not on screen this frame.
                    let target = rig
                        .gap_rect
                        .or_else(|| rig.rects.get(&ghost.card.id).copied());
                    let Some(target) = target else {
                        // Card vanished (deleted, view switched away): drop.
                        rig.ghost = None;
                        return None;
                    };
                    let t_raw = ((now - ghost.started) / SETTLE_DURATION).clamp(0.0, 1.0) as f32;
                    if t_raw >= 1.0 {
                        // Touchdown: record the landing rect as the card's
                        // position so its re-entry next frame starts no FLIP
                        // track (the ghost IS the animation for this move).
                        rig.rects.insert(ghost.card.id.clone(), target);
                        rig.ghost = None;
                        rig.gap_rect = None;
                        return None;
                    }
                    let from = ghost.from_rect.unwrap_or(target);
                    Some(GhostVisual {
                        rect: lerp_rect(from, target, ease_snap(t_raw)),
                        // Elevation recedes as the card merges into its slot.
                        lift: 1.0 - ease_snap(t_raw),
                        card: ghost.card.clone(),
                        board_title: ghost.board_title.clone(),
                    })
                }
            }
        });

        let Some(visual) = visual else { return };

        egui::Area::new(ghost_id())
            .order(egui::Order::Foreground)
            .interactable(false)
            .constrain(false)
            .movable(false)
            .fixed_pos(visual.rect.min)
            .show(ctx, |ui| {
                // Pin the content to the animated rect in BOTH directions.
                // `card_body` stretches to the ui's available width, and an
                // unconstrained foreground Area reports a window-wide (or
                // infinite) available width — without the max, the picked-up
                // card ballooned across neighbouring columns.
                ui.set_min_size(visual.rect.size());
                ui.set_max_size(visual.rect.size());
                crate::widgets::card::ghost_card(
                    ui,
                    &visual.card,
                    visual.board_title.as_deref(),
                    visual.lift,
                );
            });
    }

    /// Painted rect of the ghost from the most recent [`draw_ghost`] pass;
    /// `None` when no ghost is on screen. Test/debug aid for layout
    /// assertions (e.g. the one-card-width regression test).
    #[cfg(test)]
    pub fn last_ghost_rect(ctx: &egui::Context) -> Option<Rect> {
        ctx.memory(|m| m.area_rect(ghost_id()))
    }
}

/// Per-frame placement of the ghost, resolved from [`CardRig`] state.
struct GhostVisual {
    rect: Rect,
    /// 0 = resting in its slot, 1 = fully lifted.
    lift: f32,
    card: Card,
    board_title: Option<String>,
}

/// Lifted-card alpha for a given lift amount.
pub(crate) fn ghost_opacity(lift: f32) -> f32 {
    1.0 - (1.0 - DRAG_OPACITY) * lift
}

/// Eased 0..1 pickup progress.
fn lift_progress(now: f64, started: f64) -> f32 {
    ease_standard(((now - started) / LIFT_DURATION).clamp(0.0, 1.0) as f32)
}

// -- Insertion gap -----------------------------------------------------------

/// Paint the placeholder slot reserved at the drag insertion point. The
/// surrounding cards make room via normal layout; their FLIP tracks animate
/// the resulting reflow.
pub fn gap_slot(ui: &mut Ui, height: f32) {
    // Mirror a chip's frame exactly — outer margin 3px, inner margin 8px and
    // a 1px (transparent) stroke — so the placeholder has precisely the
    // moving card's dimensions, padding and 6px corner radius.
    let frame = egui::Frame::new()
        .inner_margin(egui::Margin::same(8))
        .stroke(egui::Stroke::new(1.0, Color32::TRANSPARENT))
        .outer_margin(egui::Margin::symmetric(0, 3));

    let content_rect = frame
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), height.max(0.0)),
                egui::Sense::hover(),
            );
            // This content rect is exactly where the real card's interaction
            // rect will land — the settle flight targets it.
            CardRig::note_gap_rect(ui.ctx(), rect);
            rect
        })
        .inner;

    // Paint at the widget_rect (content + inner_margin + stroke), the same
    // rect egui paints the real chip's fill into. Do NOT use the frame's
    // outer_rect (response.rect): that also includes the 3px bottom
    // outer_margin, which would make the placeholder 3px taller than the
    // card (outer_margin is empty space outside the painted frame).
    if height > 0.5 {
        let widget_rect = frame.widget_rect(content_rect);
        let accent = crate::theme::Tokens::get(ui.ctx()).accent;
        ui.painter().rect_filled(
            widget_rect,
            GAP_RADIUS,
            accent.gamma_multiply(f32::from(GAP_FILL_ALPHA) / 255.0),
        );
        ui.painter().rect_stroke(
            widget_rect,
            GAP_RADIUS,
            egui::Stroke::new(
                1.0,
                accent.gamma_multiply(f32::from(GAP_STROKE_ALPHA) / 255.0),
            ),
            egui::epaint::StrokeKind::Inside,
        );
    }
}

/// Stagger distance for a card at filtered-list index `idx` while the gap
/// sits at insertion index `gap_idx`. Cards above the gap do not move.
pub(crate) fn stagger_order(gap_idx: Option<usize>, idx: usize) -> u32 {
    match gap_idx {
        Some(g) if idx >= g => (idx - g) as u32,
        _ => 0,
    }
}

// -- Easing ------------------------------------------------------------------
//
// CSS cubic-bezier solver (same algorithm WebKit/Blink use), so the curves
// below can be compared directly with the web client's timing functions.

const NEWTON_ITERATIONS: u32 = 4;
const NEWTON_MIN_SLOPE: f32 = 0.001;
const SUBDIVISION_PRECISION: f32 = 1e-7;
const SUBDIVISION_MAX_ITERATIONS: u32 = 10;

#[inline]
fn bezier_a(a1: f32, a2: f32) -> f32 {
    1.0 - 3.0 * a2 + 3.0 * a1
}
#[inline]
fn bezier_b(a1: f32, a2: f32) -> f32 {
    3.0 * a2 - 6.0 * a1
}
#[inline]
fn bezier_c(a1: f32) -> f32 {
    3.0 * a1
}

#[inline]
fn calc_bezier(t: f32, a1: f32, a2: f32) -> f32 {
    ((bezier_a(a1, a2) * t + bezier_b(a1, a2)) * t + bezier_c(a1)) * t
}

#[inline]
fn calc_slope(t: f32, a1: f32, a2: f32) -> f32 {
    3.0 * bezier_a(a1, a2) * t * t + 2.0 * bezier_b(a1, a2) * t + bezier_c(a1)
}

fn newton_raphson(x: f32, mut guess: f32, x1: f32, x2: f32) -> f32 {
    for _ in 0..NEWTON_ITERATIONS {
        let slope = calc_slope(guess, x1, x2);
        if slope.abs() < NEWTON_MIN_SLOPE {
            break;
        }
        guess -= (calc_bezier(guess, x1, x2) - x) / slope;
    }
    guess
}

fn binary_subdivide(mut a: f32, mut b: f32, x: f32, x1: f32, x2: f32) -> f32 {
    let mut t = x;
    for _ in 0..SUBDIVISION_MAX_ITERATIONS {
        t = a + (b - a) * 0.5;
        let val = calc_bezier(t, x1, x2) - x;
        if val.abs() < SUBDIVISION_PRECISION {
            break;
        }
        if val > 0.0 {
            b = t;
        } else {
            a = t;
        }
    }
    t
}

/// Evaluate a CSS `cubic-bezier(x1, y1, x2, y2)` (implicit endpoints
/// (0,0) and (1,1)) at time `x`.
fn cubic_bezier(x: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    // Newton–Raphson when the slope is well-behaved, bisection fallback near
    // the control-point ends where the curve flattens.
    let t = if calc_slope(0.0, x1, x2) < NEWTON_MIN_SLOPE
        || calc_slope(1.0, x1, x2) < NEWTON_MIN_SLOPE
    {
        binary_subdivide(0.0, 1.0, x, x1, x2)
    } else {
        newton_raphson(x, x, x1, x2)
    };
    calc_bezier(t, y1, y2)
}

/// Trello's standard decelerate curve: quick launch, gentle landing.
/// Used for card shifts, the lift ramp and the gap opening.
pub fn ease_standard(t: f32) -> f32 {
    cubic_bezier(t, 0.20, 0.00, 0.00, 1.00)
}

/// Symmetric accelerate/decelerate curve for the release flight and
/// snap-back, so the ghost eases both out and in.
pub fn ease_snap(t: f32) -> f32 {
    cubic_bezier(t, 0.40, 0.00, 0.20, 1.00)
}

// -- Small math helpers -------------------------------------------------------

fn lerp_pos(a: Pos2, b: Pos2, t: f32) -> Pos2 {
    Pos2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn lerp_rect(a: Rect, b: Rect, t: f32) -> Rect {
    Rect {
        min: lerp_pos(a.min, b.min, t),
        max: lerp_pos(a.max, b.max, t),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_endpoints_and_bounds() {
        for f in [ease_standard, ease_snap] {
            assert_eq!(f(0.0), 0.0);
            assert_eq!(f(1.0), 1.0);
            // Extrapolated input clamps to the endpoints.
            assert_eq!(f(-0.5), 0.0);
            assert_eq!(f(1.5), 1.0);
        }
    }

    #[test]
    fn easing_is_monotonic() {
        for f in [ease_standard, ease_snap] {
            let mut prev = 0.0f32;
            for i in 1..=100 {
                let v = f(i as f32 / 100.0);
                assert!(
                    v >= prev - 1e-6,
                    "curve went backwards at {i}: {v} < {prev}"
                );
                prev = v;
            }
        }
    }

    #[test]
    fn standard_curve_overshoots_linear_at_midpoint() {
        // A fast-out curve should be well past linear halfway through.
        assert!(ease_standard(0.5) > 0.7);
        // But it still has room to settle (doesn't finish instantly).
        assert!(ease_standard(0.5) < 0.95);
        // The snap curve moves slower early (accelerates into the flight).
        assert!(ease_snap(0.2) < 0.2);
    }

    #[test]
    fn track_progress_respects_delay_and_duration() {
        let track = Track {
            from: Pos2::new(0.0, 0.0),
            to: Pos2::new(0.0, 100.0),
            start: 1.0,
            delay: STAGGER_STEP,
        };
        // Frozen during the stagger delay.
        assert_eq!(track.progress(1.0), 0.0);
        assert_eq!(track.center(1.0 + STAGGER_STEP / 2.0), Pos2::new(0.0, 0.0));
        // Halfway (raw) inside the shift window.
        let half = 1.0 + STAGGER_STEP + SHIFT_DURATION / 2.0;
        let p = track.progress(half);
        assert!((0.48..=0.52).contains(&p));
        // The standard easing means the card has covered more than half.
        let c = track.center(half);
        assert!(c.y > 50.0, "eased center at half raw time = {}", c.y);
        // Finished (tiny epsilon absorbs 0.02 + 0.2 floating-point rounding).
        assert!(track.done(1.0 + STAGGER_STEP + SHIFT_DURATION + 1e-9));
        assert_eq!(track.progress(99.0), 1.0);
    }

    /// Scrolling jumps every card's rect at once; without an active drag
    /// that must NOT start a FLIP track (the gliding card would paint over
    /// chrome outside its scroll clip). The same jump during a drag still
    /// animates.
    #[test]
    fn scroll_jump_without_drag_creates_no_track() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let base = Rect::from_min_size(Pos2::new(10.0, 100.0), Vec2::new(248.0, 40.0));

        let frame = |ctx: &egui::Context, now: f64, f: &dyn Fn(&egui::Context)| {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    time: Some(now),
                    ..Default::default()
                },
                |ctx| f(ctx),
            );
            out.textures_delta.clear();
        };

        // Frame 1: baseline layout.
        frame(&ctx, 0.0, &|ctx| {
            CardRig::card_laid_out(ctx, "s1", base, 0);
        });

        // Frame 2: 300px scroll flick, no drag active → no track, no delta.
        frame(&ctx, 0.016, &|ctx| {
            let delta =
                CardRig::card_laid_out(ctx, "s1", base.translate(Vec2::new(0.0, -300.0)), 0);
            assert_eq!(delta, Vec2::ZERO, "scroll jump must not start a glide");
            assert!(
                !CardRig::needs_repaint(ctx),
                "rig must stay idle after a scroll jump"
            );
        });

        // Frame 3: the same jump while a drag is active → glide.
        let dragged = Card {
            id: "s2".into(),
            column_id: "c".into(),
            title: "Dragged card".into(),
            description: String::new(),
            due_date: String::new(),
            labels: vec![],
            ..Default::default()
        };
        frame(&ctx, 0.032, &|ctx| {
            CardRig::card_laid_out(ctx, "s2", base, 0);
            CardRig::begin_drag(ctx, dragged.clone(), None, base.center());
            let delta =
                CardRig::card_laid_out(ctx, "s1", base.translate(Vec2::new(0.0, -250.0)), 0);
            assert!(
                delta != Vec2::ZERO,
                "reflow during a drag must still animate"
            );
        });
    }

    #[test]
    fn stagger_grows_with_distance_below_gap() {
        assert_eq!(stagger_order(None, 0), 0);
        assert_eq!(stagger_order(Some(2), 0), 0); // above the gap
        assert_eq!(stagger_order(Some(2), 1), 0); // adjacent below
        assert_eq!(stagger_order(Some(2), 2), 0);
        assert_eq!(stagger_order(Some(2), 3), 1);
        assert_eq!(stagger_order(Some(2), 6), 4);
    }

    #[test]
    fn ghost_never_wider_than_one_card() {
        // Regression: the picked-up card used to stretch to the window width
        // because `card_body` pins its content to `available_width` and an
        // unconstrained foreground Area reports a huge available width.
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let card = Card {
            id: "g1".into(),
            column_id: "c".into(),
            title: "A fairly long card title to make the chip wrap".into(),
            description: String::new(),
            due_date: String::new(),
            labels: vec![],
            ..Default::default()
        };
        let slot = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(248.0, 40.0));
        // Seed the rig as if the card had been laid out, then pick it up.
        CardRig::card_laid_out(&ctx, "g1", slot, 0);
        CardRig::begin_drag(&ctx, card, None, Pos2::new(50.0, 30.0));

        // Render ONLY the ghost, at full lift (past LIFT_DURATION).
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(1.0),
                events: vec![egui::Event::PointerMoved(Pos2::new(120.0, 120.0))],
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 700.0))),
                ..Default::default()
            },
            |ctx| CardRig::draw_ghost(ctx),
        );
        output.textures_delta.clear();

        let ghost = CardRig::last_ghost_rect(&ctx).expect("ghost painted");
        let expected = slot.width() * DRAG_SCALE;
        assert!(
            ghost.width() <= expected + 1.0,
            "ghost width {} exceeds one-card width {}",
            ghost.width(),
            expected
        );
        assert!(
            ghost.width() > slot.width() * 0.9,
            "ghost unexpectedly narrow: {}",
            ghost.width()
        );
        // And it must never approach the window width (the original bug).
        assert!(
            ghost.width() < 400.0,
            "ghost bled across columns: {}",
            ghost.width()
        );
    }

    /// The insertion placeholder must paint at exactly the same rect as a
    /// real card chip's frame. Previously the gap painted at the frame's
    /// outer_rect (which includes the 3px bottom outer_margin), making it
    /// 3px taller than the card.
    #[test]
    fn gap_painted_rect_matches_card_chip() {
        let card = Card {
            id: "gap-match".into(),
            column_id: "c".into(),
            title: "A card whose placeholder must match it exactly".into(),
            description: String::new(),
            due_date: "2026-01-01".into(),
            labels: vec![],
            ..Default::default()
        };

        const SCREEN: Vec2 = Vec2::new(400.0, 700.0);

        // Render inside a CentralPanel so the frame's `is_rect_visible`
        // check passes (headless Areas have no clip rect and cull fills).
        let render = |contents: &dyn Fn(&mut egui::Ui)| -> Vec<egui::epaint::ClippedShape> {
            let ctx = egui::Context::default();
            crate::theme::install_fonts(&ctx);
            crate::theme::apply_visuals(&ctx, false);
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.set_width(280.0);
                        contents(ui);
                    });
                },
            );
            out.textures_delta.clear();
            out.shapes
        };

        // --- Render the real chip and capture its painted frame fill rect.
        let shapes = render(&|ui| {
            let _ = crate::widgets::card::card_chip(ui, &card, None, 0, None, true);
        });
        // Exclude the CentralPanel background (full-screen) and the
        // transparent gap-frame rect; keep the chip's fill.
        let card_fill = largest_rect_shape_below(&shapes, SCREEN)
            .expect("card chip should paint a frame fill rect");

        // The chip records its content rect; the gap height derives from it.
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let mut seed_out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.set_width(280.0);
                    let _ = crate::widgets::card::card_chip(ui, &card, None, 0, None, true);
                });
            },
        );
        seed_out.textures_delta.clear();
        let content_h = CardRig::load_mut(&ctx, |rig| {
            rig.rects
                .get("gap-match")
                .map(|r| r.height())
                .unwrap_or(0.0)
        });
        assert!(content_h > 0.0, "card content height not recorded");

        // --- Render the gap at full height and capture its painted fill rect.
        let shapes2 = render(&|ui| {
            gap_slot(ui, content_h);
        });
        let gap_fill = largest_rect_shape_below(&shapes2, SCREEN)
            .expect("gap should paint a placeholder fill rect");

        // Same width (column width − frame chrome) and same height (content
        // height + inner_margin + stroke, NOT including outer_margin).
        assert!(
            (gap_fill.width() - card_fill.width()).abs() < 0.5,
            "gap width {} != card width {}",
            gap_fill.width(),
            card_fill.width()
        );
        assert!(
            (gap_fill.height() - card_fill.height()).abs() < 0.5,
            "gap height {} != card height {} (gap bled into outer_margin?)",
            gap_fill.height(),
            card_fill.height()
        );
    }

    /// Largest `Shape::Rect` whose area is strictly smaller than `screen_area`,
    /// so the panel's full-screen background is skipped.
    fn largest_rect_shape_below(
        shapes: &[egui::epaint::ClippedShape],
        screen: Vec2,
    ) -> Option<Rect> {
        let screen_area = screen.x * screen.y;
        shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::Rect(rs) if rs.rect.area() < screen_area => Some(rs.rect),
                _ => None,
            })
            .max_by(|a, b| {
                a.area()
                    .partial_cmp(&b.area())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}
