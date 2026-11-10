# Stage 4 — Task checklist

Legend: `[ ]` pending, `[~]` in progress, `[x]` done. Each task ends with the
stated gate. Commits follow the plan in `spec.md` §7.

## Part A — Clocks

### A1. Store: clock snapshot and write wrappers

- [x] `crates/app-gpui/src/store.rs`: delete `ClockRow` and `clocks()`;
      add `ClockData { clocks, people, travels }` and `clock_data()` reading
      through the read handle under one lock.
- [x] Add write wrappers: `add_clock`, `remove_clock`, `reorder_clocks`,
      `add_person`, `rename_person`, `remove_person`, `relocate_person`,
      `start_travel`, `end_travel`, `extend_travel`, `remove_travel`.
- [x] Every wrapper maps `KandriumError` with `e.to_string()`.
- **Gate:** `cargo clippy --manifest-path crates/app-gpui/Cargo.toml`. Passed.
- **Commit:** `Add the Clocks data bridge and the icon set` (1791ee3).

### A2. Pure layout, geometry and selection helpers

- [x] `views/clocks.rs`: `CARD_H`, `GAP`, `COLUMNS`, `MIN_COL_W`.
- [x] `StaticGrid { origin, width, count }` with `col_w`, `rows`,
      `slot_rect`, `slot_at` using half-gap row bands and horizontal
      clamping; insertion index clamped to `count - 1`.
- [x] `reordered`, `preview_slots`, `initials`, `matches_search`,
      `city_people`, `until_next_minute`, `until_next_travel`.
- [x] Unit tests in-file for each helper, including:
      - slot centres round-trip;
      - gap +3 px vs +9 px band split;
      - `reordered([0..5], 1, 3)` = `[0,2,3,1,4]`;
      - placeholder stability while the preview order differs.
- **Gate:** `cargo test --manifest-path crates/app-gpui/Cargo.toml --lib`.
  11 Clocks unit tests pass.

### A3. Grid, toolbar, empty state, drag

- [x] Toolbar: search field, city count, 12h/24h toggle, Add city.
- [x] Empty state with its own Add city button.
- [x] Grid rows of three flex slots at `CARD_H`/`GAP`; slot width equals
      `StaticGrid::col_w`.
- [x] Card: city, offset, time, period, date; initials chips (two + `+N`),
      `AWAY` ring, `VISITING` strip, planned-trip dot.
- [x] Drag: `.on_drag` with ghost (opacity 0.92, `LIFT` unless
      `cx.reduce_motion()`); container `on_drag_move` sets `hover` from
      static geometry; placement preview from `preview_slots`; `on_drop`
      commits `reorder_clocks` with a complete permutation and reloads.
- [x] Drag disabled while the search is non-empty.
- **Gate:** build + launch the dev app. Relaunched; cross-row drag is covered
  by the static-geometry unit tests, the on-screen pass is left to the user.

### A4. Zone picker

- [x] Modal with search `InputState`, `zone_matches` filtering, 200-row cap.
- [x] Added zones disabled and tagged "Added"; skipped by navigation.
- [x] `↑`/`↓`/`Enter`/`Esc` via capture-phase listener; click to add;
      Clear search; Cancel.
- [x] `store.add_clock` then reload, picker stays open.
- **Gate:** manual pass in the app (pending user confirmation).

### A5. People inspector and travel

- [x] Inspector modal: city header, residents with initials + status,
      add/rename/remove, visitors with Send home, travel action.
- [x] Rename inline input (Enter commit, Esc cancel); remove via confirm.
- [x] Travel form: Assign (destination day windows with
      `day_start_seconds`/`day_end_seconds`), Extend, Relocate, open-ended.
- [x] End early, extend, send home, remove; each goes through `Store`.
- [x] Relocation relies on core cancelling both legs.
- [x] Core errors shown inside the open panel.
- **Gate:** manual pass in the app (pending user confirmation).

### A6. Remove city with `ON DELETE RESTRICT`

- [x] Confirm dialog on the card menu.
- [x] On core error keep the dialog open and show the exact message.
- **Gate:** covered by `a_city_with_residents_refuses_removal`; the message is
  core's own text.

### A7. Minute tick

- [x] One `Timestamp::now()` per frame in `Render` while Clocks is active.
- [x] Generation-guarded spawn loop: wait = min(next minute, nearest travel
      boundary) + 50 ms, 0.05 s floor; exits on section change; retargets on
      mutation; no store access in the loop.
- **Gate:** leave/enter Clocks repeatedly; the generation guard stops the old
  loop. On-screen check left to the user.

### A8. Clocks tests and report

- [x] `tests/clocks.rs`: temp DB via `Store::open_path`; add/reorder/remove
      city; blocked removal message; people add/rename/remove; travel
      start/end/extend/remove; relocation cancels both legs.
- [x] Run all gates from `spec.md` §6.
- [x] Rebuild and relaunch the gpui dev app.
- [x] Commit tests (`487d85d`), then report.

## Part B — Notes

### B1. Store notes wrappers

- [x] `notes_root` (`ensure_root`), `notes_tree`, `read_note`,
      `write_note`, `create_note`, `create_folder`, `rename_note`,
      `delete_note` on `Store`, forwarding to `kandrium_core::notes`.
- [x] Core already exposed rename/delete (`rename_path`, `delete_path`);
      only name-validated creation was added (`create_named_note`,
      `create_named_folder`).
- **Gate:** `cargo clippy --manifest-path crates/app-gpui/Cargo.toml`. Passed.

### B2. Tree and tabs

- [x] Recursive tree honouring `KANDRIUM_NOTES`; expanded set; selected row.
- [x] New note / new folder from the tree header, rename / delete on rows,
      inline naming, confirm for delete.
- [x] File click opens a tab; tab strip; active tab; close saves first.
- [x] Collapse-to-rail (30 px) with a separate rail id.
- **Gate:** covered by `tests/notes.rs` (tree header + empty state render);
  on-screen pass left to the user.

### B3. Editor, preview, vim, autosave

- [x] `TextareaState` per note; `.readonly(!vim.is_insert())`.
- [x] Preview toggle using `gpui_kit::base::text::markdown`.
- [x] Vim capture listener: `Esc`, `:w` flush, `:q` saves and closes.
- [x] 400 ms debounced autosave (generation-guarded) through `write_note`;
      flush on tab close, section change, `:w`, `:q`.
- **Gate:** manual pass: type, wait, reopen, confirm persistence.

### B4. Notes tests and report

- [x] `tests/notes.rs`: temp notes root; create/read/write/rename/delete;
      collision and escape rejection; the section renders `notes-view`,
      `notes-tree`, `notes-new-note`, `notes-empty`.
- [x] Run all gates; relaunch the app; commit; report.
