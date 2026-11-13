# Tasks — card drag motion

Status: implemented.

- [x] 1. `anim.rs`: add `SHIFT` (200 ms), `STAGGER_STEP` (20 ms),
      `STAGGER_MAX` (100 ms), `stagger_order`, `shift_offset`; keep
      `LIFT` / `REFLOW` / `SNAP_BACK` and both curves. Unit tests for the
      offset table and stagger.
- [x] 2. `tasks.rs`: add `CardMetrics` + `measure(key, metrics)` and thread
      metrics, `previous_target` and `gap_height` through `board_view`,
      `column_view`, `DragStyle`.
- [x] 3. `tasks.rs`: skip the dragged card, render the full-height gap at
      the target index (fading in over `REFLOW`, measured under
      `gap:<card_id>`), and wrap affected cards in the staggered `top`
      offset animation using `ease_standard`. Delete `indicator_slot` /
      `append_slot`.
- [x] 4. `tasks.rs`: seed the source target from `on_drag`
      (`column_id`, `after`), pass `window` to `drop_card`, and route the
      board `on_mouse_up` to `cancel_card_drag`.
- [x] 5. `app.rs`: add drag fields (`previous_target`, `drag_gap_height`,
      `drag_origin`, `drag_origin_target`, `drag_flight`,
      `drag_flight_token`, `settled_card`, `drag_metrics`); implement
      `begin_card_drag`, `set_drop_target`, `drop_card`,
      `cancel_card_drag`, `start_drag_flight`, `finish_drag_flight`,
      `clear_drag_state`.
- [x] 6. `app.rs`: render `drag_flight_overlay` (`ease_snap`, `SNAP_BACK`)
      above the board, honour `cx.reduce_motion()` by teleporting, and
      clear drag state on board/section switches.
- [x] 7. `projects.rs`: pass the new `board_view` arguments.
- [x] 8. Tests: `shift_offset` / `stagger_order` unit tests and the
      headless drag-state / refused-drop / reduced-motion tests in
      `tests/shell.rs`.
- [x] 9. Gates: `cargo fmt`, `cargo clippy --all-targets`, `cargo test`,
      `cargo build --bin kandrium-gpui` in `crates/app-gpui`.
- [x] 10. Relaunch `target/KandriumGpui-dev.app` and report what animates
      and what cannot (scale/shadow, flight ghost fidelity, hit-rect glide).
