# Stage 4 — Verification checklist

Checkpoints are verified against the code and the gates in `spec.md` §6.
Visual/manual passes are called out explicitly; they need the running app.

## Part A — Clocks

- [x] `Store::clock_data` returns one snapshot of cities, people and trips and
      is the only read the view and the tick use.
- [x] Every Clocks mutation goes through a `Store` wrapper into
      `kandrium-core`; no SQL and no time-zone math lives in `crates/app-gpui`.
- [x] `StaticGrid::slot_at` splits rows at `row_bottom + GAP / 2` and clamps
      the in-row insertion index to the static slot count, not the preview.
- [x] A cross-row drop produces a complete permutation of every saved id
      (`reordered` never drops or duplicates an id).
- [x] The drag ghost honours `cx.reduce_motion()`; reflow is instant.
- [x] The picker filters by city, region and zone id, disables added zones,
      and supports `↑` / `↓` / `Enter` / `Esc`.
- [x] Removing a city with residents keeps the dialog open and shows core's
      `This city still has N person/people. Remove them first.` message.
- [x] The inspector adds, renames (Enter/Esc) and removes people, and offers
      travel, relocate and send-home actions.
- [x] Trip windows are whole days in the destination zone via
      `day_start_seconds` / `day_end_seconds`.
- [x] Ending early writes `ends_at`, extending can clear it (open ended), and
      removing deletes the row.
- [x] Relocation cancels the active trip to the new home and leaves unrelated
      trips alone (core behaviour, covered by `tests/clocks.rs`).
- [x] The minute tick takes one `Timestamp::now()` per frame, is
      generation-guarded and never touches the database.
- [x] `cargo fmt`, `cargo clippy --workspace --all-targets`,
      `cargo test --workspace` and the gpui crate gates are clean.
- [ ] Manual pass: drag a city from the last column of one row into the first
      column of the next and confirm the on-screen order matches the drop.
- [ ] Manual pass: open the picker, type a region, add a zone, and confirm it
      appears as "Added" and stays open.
- [ ] Manual pass: leave Clocks for a few minutes and confirm the times keep
      advancing at the minute boundary.

## Part B — Notes

- [x] Store wrappers over `kandrium_core::notes` honour `KANDRIUM_NOTES`.
- [x] The tree creates, opens, renames and deletes notes through core.
- [x] The editor previews markdown and keeps the vim layer working.
- [x] Autosave is debounced and writes atomically.
- [x] Notes tests and gates pass.
- [ ] Manual pass: type in a note, wait for the dirty dot to clear, close the
      tab and reopen it; confirm the text came back.
- [ ] Manual pass: create a folder, create a note inside it, rename the folder
      and confirm open tabs follow the move, then delete the folder.
