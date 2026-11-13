# gpui card editor — behavioural parity with the egui modal

## Goal

Bring the gpui card editor (`crates/app-gpui/src/views/card_modal.rs`) to
behavioural parity with the egui baseline (`crates/app/src/widgets/card_modal.rs`):
field order, the description field's markdown edit/preview behaviour, the
project/area pickers and their exclusivity, label chips, due date,
save/cancel/close affordances, Esc/focus, keyboard handling, and where the vim
layer sits. Keep the gpui design language (Tokens, 6 px radius, Inter, shared
field helpers). Close the gaps that matter; list the ones deliberately left
open with reasons.

## Baseline behaviour (egui, verified by reading the file)

Order of controls: Title → project block → Description → Due date → Labels →
Move to board → actions.

- Project card: read-only `Project` line (`identifier — name`), an
  "Show on Tasks boards" checkbox (disabled and forced off when there are no
  area boards), a `Board` combo that is auto-seeded to the first board, and a
  faint helper line ("Mirrors this card onto the selected board." vs "Not
  mirrored onto a Tasks board; hidden from All.").
- Normal card: `Project` combo with `None`, plus the faint helper
  "Shows the project identifier on this card."; hidden entirely when no
  projects exist.
- Description: edit/preview toggle on the label row; the field is a fixed
  140 px box that *includes* the vim strip in edit mode, so toggling never
  resizes the modal; preview shows "Nothing to preview." when the source is
  blank; hint "Add a more detailed description…".
- Due date: `YYYY-MM-DD` hint plus a calendar picker button.
- Labels: eight-color swatches, selected gets an inner ring.
- Move to board: combo for normal cards only; a pick moves the card.
- Actions: Save, Archive/Unarchive, Delete (delete asks for confirmation).
  Dismissal is Esc / backdrop / `should_close`; there is no Cancel button.
- Keyboard: Tab in the description inserts two spaces instead of moving focus;
  Esc in the description is consumed by vim before the modal (Insert → Normal),
  otherwise Esc closes; `:w` saves without closing, `:q`/`:wq`/`:x` save and
  close.

## Current gpui behaviour (before this change)

Order: Title → Description → picker → Labels → Due date → error → Cancel/Save.
Gaps that matter:

1. Field order differs in two places (picker before/after description; Due
   before/after Labels).
2. Project cards have no read-only `Project` line, no helper line explaining
   mirroring, and no empty-state for "no Tasks boards".
3. Normal cards have no helper line for the project tag.
4. Description has no hint text, preview has no empty state, and toggling
   preview removes the vim strip from the layout so the modal jumps ~32 px.
5. No "Move to board" action at all (gpui has no cross-board drag either, so
   a card cannot be moved between boards in the app).
6. Tab in the description moves focus instead of inserting two spaces.
7. The panel can no longer fit its content once the project/area helper lines
   and the move row exist (currently `max_h(640)` with no scrolling).

Already at parity and left as-is: exclusivity (project cards only pick an area
board, normal cards only pick a project; `None` clears the link), label chip
semantics, Save/Cancel/×/backdrop/Esc behaviour, vim placement relative to Esc
and `:w`/`:q`, required-title error, per-open reset of preview mode.

## Changes (decision complete)

### 1. `crates/app-gpui/src/store.rs`

Add one method next to `move_card`:

```rust
/// Move a card to another Tasks board, keeping its column by status when
/// core can (mirrors the egui modal's "Move to board").
pub fn move_card_to_board(&self, card_id: &str, board_id: &str) -> StoreResult<()> {
    kandrium_core::move_card_to_board(self.wconn()?, card_id, board_id)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
```

Add `move_card_to_board` to the existing `kandrium_core` import list at the
top of the file.

### 2. `crates/app-gpui/src/views/card_modal.rs`

`EditSession`: add `pub home_board_id: String`.

`modal()` child order becomes:

1. heading row (unchanged)
2. Title label + `card-title-field`
3. project block (one of the two variants below)
4. Description label row (label + `card-description-toggle`, unchanged)
5. description block (new wrapper, below)
6. Due date label + `card-due-field`
7. Labels label + `card-labels`
8. Move to board (normal cards, targets non-empty)
9. error
10. Cancel/Save row

Project block, project-card variant:

- Forced off / no chips when `ctx.area_boards.is_empty()`: show the
  "Show on Tasks boards" label and a faint
  `No Tasks boards available.` line (id `card-area-empty`).
- Otherwise the existing `choice_row("card-area", …)` chips stay exactly as
  they are (the `None` chip is the "uncheck" path).
- Add a read-only owning-project line above the chips:
  `field_label("Project")` + `div().id("card-modal-project").text(Text::Small)
  .text_color(t.text_muted)` with `identifier — name` resolved from
  `ctx.projects` by `session.project_id`, fallback `—`.
- Add a faint helper line below the chips, id `card-area-hint`:
  `Mirrors this card onto the selected board.` when `area_board_id` is
  `Some`, else `Not mirrored onto a Tasks board; hidden from All.`
  (`Text::Small`, `t.text_faint`).

Project block, normal-card variant (only when `!ctx.projects.is_empty()`):

- Existing `field_label("Project")` + `choice_row("card-project", …)` chips.
- New faint helper line, id `card-project-hint`:
  `Shows the project identifier on this card.`

Description block:

- Wrap the body and the vim strip in
  `div().id("card-description-block").w_full().flex().flex_col().gap(px(6.))
  .flex_shrink_0()`.
- Body keeps `h(px(190.))` in both modes (textarea in edit, preview box in
  preview).
- Edit mode appends `mode_strip(session, t, cx)` (unchanged).
- Preview mode appends `div().h(px(20.))` — the reserved strip height — so
  toggling does not change the modal height.
- Preview body: when `description_text.trim().is_empty()`, render a faint
  `Nothing to preview.` (`Text::Small`, `t.text_faint`) instead of the
  markdown element.

Move to board row (normal cards, `!session.is_project_card`, derived targets
non-empty):

- Targets are `ctx.area_boards` filtered to `board.id != session.home_board_id`.
- `field_label("Move to board", t)` plus a wrapped chip row
  `div().id("card-move").flex().flex_wrap().gap(px(6.))`.
- Each chip reuses `choice_chip` with id `format!("card-move-{}", board.id)`,
  label `board.title`, `selected = false`, and calls the new
  `AppShell::move_editing_card(board.id.clone(), cx)`.

Panel: `.max_h(px(720.))` (was 640) plus `.overflow_y_scroll()` so the taller
content scrolls instead of squeezing or clipping.

Labels keep `Text::Label` for field labels and the existing eight-color chip
loop, now placed after Due date.

### 3. `crates/app-gpui/src/app.rs`

`open_card`:

- Set `home_board_id: card.board_id.clone()` in the `EditSession` literal.

New method (place next to `save_editing_and_keep_open`):

```rust
/// Persist the editor, then move the card to `board_id` and close. Edits
/// are written first so a move never discards what the user typed; an
/// error keeps the modal open.
pub(crate) fn move_editing_card(&mut self, board_id: String, cx: &mut Context<Self>) {
    let Some(card_id) = self.editing.as_ref().map(|s| s.card_id.clone()) else {
        return;
    };
    if !self.persist_editing(cx) {
        return;
    }
    match self.store.move_card_to_board(&card_id, &board_id) {
        Ok(()) => {
            self.editing = None;
            self.reload_displayed_board();
        }
        Err(error) => {
            if let Some(session) = self.editing.as_mut() {
                session.error = Some(error);
            }
        }
    }
    cx.notify();
}
```

`handle_card_key`: unchanged. Tab insertion was attempted and dropped, see
"Deliberately not closed" below.

`open_card` description state gains
`.placeholder("Add a more detailed description…")`.

### 4. `crates/app-gpui/tests/shell.rs`

Add two headless tests (ids are stable):

1. `card_modal_moves_card_to_another_board` — seeded DB, open a card on the
   first board, click `card-move-<other board id>`, render a frame; assert the
   modal is closed and the card is in the other board's board (via
   `Store::open_path(..).board(..)`), and that the home board no longer lists
   it.
2. `card_modal_shows_project_and_move_rows` — open a normal card on the
   seeded DB after creating one project with a temp projects root; assert
   `card-project-hint` is visible, a `card-move` row exists, and
   `card-area-hint` / `card-modal-project` are absent.
3. `titlebar_clock_fits_at_minimum_window_width` — separate titlebar change:
   measures the row at 13 px mono and asserts the 940 px fit.

Existing tests (`card_modal_saves_a_new_title`,
`card_modal_label_toggle_persists`, vim Esc/`:w` cases,
`text_fields_paint_one_box_and_inset_the_text_once`) must stay green; no id
they use is renamed.

## Deliberately not closed (and why)

- **Archive/Unarchive and Delete buttons.** The gpui shell has no card Archive
  page (`list_archived_cards` is never called) and no destructive-confirm
  dialog for cards. Shipping Archive would hide cards with no in-app restore
  path; shipping Delete needs a confirm flow that only Clocks has today. Both
  are app features, not editor parity, and would be one-way data loss. Not
  rendered, and `CardModalOutcome`-style actions are not added.
- **Due-date calendar picker.** Follow-up: implemented. Clicking the due row
  (or its calendar icon) opens an anchored month grid with weekday header,
  dimmed adjacent-month days, today's ring, the selected day, prev/next
  arrows, Today and Clear. Picking writes `YYYY-MM-DD` through the card update
  path; Esc / outside click only close it.
- **Design-language differences (intentional).** Modal width 560 (egui
  min 420), 34 px tall title field instead of 40, Text `Cancel` + `×` next to
  `Save` instead of egui's icon-less Save/Archive/Delete row, and a text
  `Preview`/`Edit` toggle instead of the eye/pencil icon button. Geometry,
  typography and the shared `field_input`/`field_textarea` helpers stay as
  they are. Follow-up: the normal-card project picker is now a real anchored
  dropdown (the area picker keeps the chip row).
- **Vim plumbing architecture.** egui rewrites input events inside the
  TextEdit (`pre_edit`) with the status bar inside the description frame; gpui
  captures keys at the panel (`capture_key_down`) with the strip below the
  field. Behaviour is aligned (Esc consumed before the modal, `:w` keeps the
  editor open, `:q`/`:wq`/`:x` save and close); only the mechanism differs and
  is left alone.
- **Tab inserts two spaces in the description.** Attempted and dropped: gpui
  resolves `tab` through key bindings (the input's `IndentInline`, and the
  base Root's focus-trap `Tab`) before element key listeners run, so the
  modal's `capture_key_down` never sees the keystroke. Claiming Tab would
  require overriding shell-level bindings or window interceptors, which is
  framework surgery for a two-space nicety. Tab therefore keeps the host's
  behaviour (indent/no-op in the textarea, focus cycling in the trap).
- **Normal-card move resets the picker after the move** is irrelevant: the
  editor closes. No extra state is added.
- **Title autofocus.** Already better than egui (title focused on open, focus
  trapped in the panel); kept.

## Verification

Inside `crates/app-gpui`:

1. `cargo fmt`
2. `cargo clippy --all-targets` — warning-free
3. `cargo test` — all green (currently 124; the two new modal tests plus the
   titlebar clock test raise it to 127).
4. `cargo build --bin kandrium-gpui`
5. Relaunch: `killall kandrium-gpui; open -n target/KandriumGpui-dev.app`
   (needs `dangerouslyDisableSandbox: true`).

Manual checks after relaunch: normal card shows Project chips + helper, then
Due date, then Labels, then Move to board; the modal height does not change
when toggling Preview and the preview empty state reads "Nothing to preview.";
a project card shows the owning project, the mirror helper and no Move to
board; clicking a board chip moves the card and closes the editor with its
edits saved; Esc in Normal mode closes, Esc in Insert mode only returns to
Normal. Titlebar: the clock reads `Wed 11 Nov 8:13 pm City` in 13 px mono at
the far right, flush to the 8 px margin, with the drag stretch still alive.
