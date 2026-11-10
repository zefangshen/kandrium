//! D. Drag animation: lift, reflow gap and snap-back on one card, expressed
//! with gpui's `with_animation`.
//!
//! The timeline and curves are deliberately the same numbers as `anim.rs`
//! (`LIFT_DURATION` 140 ms, `GAP_DURATION` 160 ms, 260 ms snap-back,
//! `cubic-bezier(0.20,0,0,1)` / `cubic-bezier(0.40,0,0.20,1)`), so the
//! judgement is about the *host*, not about changed timing.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use gpui_kit::*;

/// Timeline checkpoints, printed once each, to prove gpui drives the animation
/// frame by frame and that the curves produce the expected values.
static NEXT_MARK: AtomicU8 = AtomicU8::new(0);
const MARKS: [f32; 4] = [0.25, 0.5, 0.75, 1.0];

const LIFT_S: f32 = 0.140;
const GAP_S: f32 = 0.160;
const SNAP_S: f32 = 0.260;
const TOTAL_S: f32 = 0.560;
const CARD_W: f32 = 180.0;
const GAP: f32 = 12.0;

pub fn run() {
    application().run(|cx| {
        gpui_kit::init(cx);
        open_window(WindowOptions::default(), cx, |_window, cx| {
            cx.new(|_| DragProbe {
                generation: 0,
                logged_motion: false,
                logged_generation: usize::MAX,
            })
        })
        .expect("open drag window");
    });
}

struct DragProbe {
    generation: usize,
    logged_motion: bool,
    logged_generation: usize,
}

/// CSS `cubic-bezier`, same WebKit/Blink algorithm as `anim.rs` (Kandrium's
/// own code, Apache-2.0; reproduced here so the spike stays self-contained).
fn cubic_bezier(x: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
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

fn ease_standard(t: f32) -> f32 {
    cubic_bezier(t, 0.20, 0.00, 0.00, 1.00)
}

fn ease_snap(t: f32) -> f32 {
    cubic_bezier(t, 0.40, 0.00, 0.20, 1.00)
}

/// (lift offset px, gap px) at time `t` seconds into the timeline.
fn sample(t: f32) -> (f32, f32) {
    let lift = ease_standard((t / LIFT_S).clamp(0.0, 1.0));
    let gap = ease_standard(((t - LIFT_S) / GAP_S).clamp(0.0, 1.0));
    let snap = ease_snap(((t - (LIFT_S + GAP_S)) / SNAP_S).clamp(0.0, 1.0));
    (10.0 * lift * (1.0 - snap), CARD_W * gap * (1.0 - snap))
}

impl Render for DragProbe {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reduced = cx.reduce_motion();
        if !self.logged_motion {
            self.logged_motion = true;
            println!("[D] reduce_motion={reduced}; timeline=lift 140ms/gap 160ms/snap 260ms");
        }
        let generation = self.generation;
        if self.logged_generation != generation {
            self.logged_generation = generation;
            NEXT_MARK.store(0, Ordering::Relaxed);
            println!(
                "[D] playing generation {generation} ({} ms timeline)",
                (TOTAL_S * 1000.0) as u32
            );
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1f22))
            .text_color(rgb(0xd9d9d9))
            .p_6()
            .gap_4()
            .child(div().text_xl().child("D · drag animation probe"))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x9aa0a6))
                    .child("middle card lifts, neighbours open a gap, then it snaps back"),
            )
            .child(
                div()
                    .id("replay")
                    .cursor_pointer()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(0x2f3136))
                    .child("replay")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.generation += 1;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(GAP))
                    .children((0..3).map(|index| {
                        let card = div()
                            .w(px(CARD_W))
                            .h(px(96.0))
                            .p_3()
                            .rounded_md()
                            .bg(rgb(0x2f3136))
                            .child(format!("card {index}"));

                        if reduced {
                            // Reduced motion: render the settled state with no
                            // animation at all.
                            return card.into_any_element();
                        }

                        card.with_animation(
                            SharedString::from(format!("card-{index}-{generation}")),
                            Animation::new(Duration::from_secs_f32(TOTAL_S)),
                            move |el, delta| {
                                let (lift, gap) = sample(delta * TOTAL_S);
                                if index == 1 {
                                    let mark = NEXT_MARK.load(Ordering::Relaxed);
                                    if (mark as usize) < MARKS.len()
                                        && delta >= MARKS[mark as usize]
                                    {
                                        NEXT_MARK.store(mark + 1, Ordering::Relaxed);
                                        println!(
                                            "[D] delta={delta:.2} t={:.0}ms lift={lift:.1}px gap={gap:.1}px",
                                            delta * TOTAL_S * 1000.0
                                        );
                                    }
                                    el.relative().top(px(-lift))
                                } else if index > 1 {
                                    el.ml(px(gap))
                                } else {
                                    el
                                }
                            },
                        )
                        .into_any_element()
                    })),
            )
    }
}
