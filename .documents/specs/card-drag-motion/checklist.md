# Checklist — card drag motion

- [x] Pickup lifts the ghost and swaps the card's row for a same-height gap,
      so no sibling shifts on mouse-down.
- [x] The gap fades in over `REFLOW` with `ease_standard`.
- [x] Cards displaced by a target change glide with `.top()` over `SHIFT` +
      stagger (`STAGGER_STEP` / `STAGGER_MAX`) and `ease_standard`, instead of
      jumping.
- [x] A completed drop keeps the card hidden until the flight lands, then
      fades it in (no pop).
- [x] A refused drop, a release outside the board, and a drop on the card's
      own slot all park the gap at the origin and fly the ghost back with
      `ease_snap`.
- [x] `cx.reduce_motion()` teleports: no flight, no offsets, instant settle.
- [x] Shared curves are unchanged (`0.20,0,0,1` reflow, `0.40,0,0.20,1`
      snap-back).
- [x] Gates pass in `crates/app-gpui`: `cargo fmt`, `cargo clippy
      --all-targets`, `cargo test`, `cargo build --bin kandrium-gpui`.
- [x] The dev bundle was relaunched from freshly built code.
