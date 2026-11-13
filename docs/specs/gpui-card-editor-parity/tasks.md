# Tasks — gpui card editor parity

Ordered. Each task ends in a compile-clean state. Do not skip the gates.

- [x] 1. `crates/app-gpui/src/store.rs` — add `move_card_to_board(card_id,
      board_id)` wrapping `kandrium_core::move_card_to_board`; extend the core
      import list. No other store changes.
- [x] 2. `crates/app-gpui/src/views/card_modal.rs` — add
      `EditSession.home_board_id`; reorder the panel children to Title →
      project block → Description → Due date → Labels → Move to board →
      actions.
- [x] 3. `crates/app-gpui/src/views/card_modal.rs` — project-card block:
      read-only `card-modal-project` line, `card-area-empty` when there are no
      area boards, `card-area-hint` helper line. Normal-card block:
      `card-project-hint` helper line, hidden when there are no projects.
- [x] 4. `crates/app-gpui/src/views/card_modal.rs` — description block wrapper
      (`flex_col`, `gap 6`, `flex_shrink_0`) with a fixed 190 px body and a
      reserved 20 px strip slot in preview; preview empty state
      "Nothing to preview.".
- [x] 5. `crates/app-gpui/src/views/card_modal.rs` — Move to board chip row
      (`card-move-<board id>`) for normal cards, targets = area boards minus
      `home_board_id`; panel `max_h(720)` + `overflow_y_scroll`.
- [x] 6. `crates/app-gpui/src/app.rs` — set `home_board_id` in `open_card`;
      add `move_editing_card` (persist, then move, then close);
      description placeholder "Add a more detailed description…".
- [x] 7. `crates/app-gpui/src/app.rs` — Tab in the description: attempted,
      then reverted. gpui resolves `tab` through key bindings before element
      key listeners, so the modal cannot claim it; recorded in spec.md under
      "Deliberately not closed".
- [x] 8. `crates/app-gpui/tests/shell.rs` — add
      `card_modal_moves_card_to_another_board`,
      `card_modal_shows_project_and_move_rows`, and the separate
      `titlebar_clock_fits_at_minimum_window_width`.
- [x] 9. Gates inside `crates/app-gpui`: `cargo fmt`,
      `cargo clippy --all-targets` (warning-free), `cargo test` (127 passed).
- [x] 10. `cargo build --bin kandrium-gpui`; relaunch with
      `killall kandrium-gpui; open -n target/KandriumGpui-dev.app` using
      `dangerouslyDisableSandbox: true`.
- [x] 11. Report: out-of-parity list, changes, deliberate differences
      (Archive/Delete, date picker, Tab, design-language and vim-architecture
      differences), titlebar clock size and fit numbers, test counts, and what
      to look at in the running app.
