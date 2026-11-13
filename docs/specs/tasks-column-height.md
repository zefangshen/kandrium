# Tasks column height in the gpui app

Status: implemented. Scope: `crates/app-gpui/src/views/tasks.rs`.

## Symptom

On the gpui Tasks board every column was as tall as the whole board, even when
it held one card. The column frame, its hover/border, and the card-list drop
area all ran to the bottom of the window, and a one-card column was
indistinguishable in height from an overfull one.

## How egui sizes a column

egui has no flexbox; the board allocates explicit sizes.

1. `crates/app/src/views/board.rs::show` captures the ceiling **before**
   entering the horizontal scroller: `max_col_height = ui.available_height() -
   BOTTOM_GUTTER` (line 87; `BOTTOM_GUTTER = 15` at line 17 is the 8 px panel
   gutter plus the 7 px lane the horizontal scrollbar reserves). The comment
   above it records why this must happen first: inside the horizontal placer
   the nested vertical `available_height` is corrupt.
2. `ui.horizontal_top(...)` (line 101) lays the columns out, and each one is
   allocated with `allocate_ui_with_layout(vec2(col_width, f32::INFINITY),
   Layout::top_down(Align::LEFT), ...)` (lines 114–124). The finite **width**
   is mandatory because the horizontal `ScrollArea` reports infinite available
   width; the **infinite height** is what makes a column shrink-wrap its
   content.
3. `crates/app/src/widgets/column.rs::show_column` receives that ceiling as
   `max_col_height`. It measures what the header already used
   (`used_so_far = ui.cursor().top() - ui.min_rect().top()`, line 155) and
   derives `max_body = (max_col_height - used_so_far -
   add_form_height(...)).max(0.0)` (lines 156–157).
4. The card list only becomes a scroll area when it would overflow: it
   estimates `ordered.len() * 108.0` (line 170) and compares that with
   `max_body` (line 171). Below the cap, cards are rendered directly and grow
   the column naturally; above it, `ScrollArea::vertical().max_height(max_body)`
   (lines 189–204) caps the list and scrolls, so the column never passes the
   window bottom.
5. The drop body is floored so an empty column stays landable:
   `min_body = 30.0`, `body_bottom = body_bottom.max(body_top + min_body)`
   (lines 224–225). `body_rect` is what the drop logic hit-tests.

Net: column height = header + cards + add row, capped at the captured ceiling,
with an internal scroll only past the cap. The ceiling is a *viewport* value,
never a height the column is told to fill.

## The gpui cause

Before the fix, three separate layout instructions combined:

1. `crates/app-gpui/src/views/tasks.rs::column_view` gave the column root
   `.h_full()` — the column was explicitly told to take the full board height.
2. The parent row in `board_view` (`#board-columns`) is a horizontal flex row
   and never set `align-items`, so it kept the flexbox default `stretch`:
   children are stretched to the row's height. Removing `.h_full()` alone does
   not help while the row stretches.
3. The card list used `.flex_1()`, which is `flex: 1 1 0` — a zero base size.
   In a content-sized column that base collapses the item, so the only thing
   keeping the list visible was the stretch from (2). It also gave the column
   no height ceiling: nothing ever told a tall list to stop and scroll.

So the column was never measuring content; each level was told to fill its
parent, and the parent was the full board.

## The fix

All in `crates/app-gpui/src/views/tasks.rs`:

- Row `#board-columns` (lines 147–172): `.items_start()` (line 157) overrides
  the default `stretch`, so each column is laid out at its own height and
  aligned to the top.
- Column root (`column_view`, lines 246–260): `.h_full()` replaced by
  `.max_h(relative(1.))` (line 254). The viewport becomes a ceiling, not a
  height. `relative(1.)` is the row's content box (the row still has `pb(16)`,
  which keeps a capped column off the window's bottom edge).
- Card list (lines 292–308): `.flex_1()` replaced by `.flex_grow(1.)`
  (line 298) with `.min_h(px(30.))` (line 299). `flex_grow` keeps the automatic
  base size, so content still sizes the column, while growth lets the list fill
  the leftover space once `max_h` clamps the column. The 30 px floor is the
  drop-zone floor that keeps an empty column landable.
  `.min_h_0()` was removed here because a later `.min_h()` overrides it; the
  row keeps its own `.min_h_0()`.
- Drag/drop routing: the `on_drag_move` / `on_drop` handlers moved from the
  card list to the column root (lines 310–329). With the column now only as
  tall as its content, a short column's header covers the centre of the column,
  and a drop aimed there previously hit no handler at all. Cards keep their own
  `on_drag_move`, and descendants run later in capture order, so hovering a card
  still resolves to "insert before this card".

`overflow_y_scroll()` on the list (line 304) stays: once the column is clamped
at `max_h`, the list is the scroll container.

## What it costs

- **Drop targets.** A column's drop area is now exactly its rendered body. The
  empty band below a short column is no longer part of any column, matching
  egui, where the body rect is content-sized too. Empty columns still expose a
  body (30 px list floor plus the header) and the whole column accepts a drop,
  so a card can be dropped anywhere over a column, including its header.
  Card-precise insertion still wins over the column-level "append" target.
- **Internal scrolling.** A column taller than the row clamps at the row's
  content height (measured 960 px inside a 976 px row) and only the card list
  scrolls; the header and the add row stay pinned outside the scroll. There is
  no estimate step, unlike egui's 108 px-per-card heuristic — the
  browser-grade flex algorithm decides exactly.
- **Drag behaviour.** Unchanged apart from coverage: `on_drag_move` still
  bounds-checks, cards still override the column target, and the row-wide
  `on_mouse_up` still clears the target. Because the column no longer covers
  the empty board area, a drag hovering *beside* a short column produces no
  target at all, which is also egui's behaviour.
- **Drop-indicator lane.** The 6 px `SLOT_H` lane (`indicator_slot` /
  `append_slot`) is the last child of the card list, so it is part of the
  hugged height and showing the indicator never shifts the column. In a capped
  column it sits at the end of the scroll content, so appending at the bottom
  of a long list needs a scroll — same as egui, whose body rect and add row are
  also inside/after the scrolling cards.
- **Add-card footer.** The footer is the column's last child, after the scroll
  list, so it stays visible while cards scroll and adds its own height to the
  hugged column (30 px row, or the 34 px field plus the button row while the
  draft is open). Opening a draft shrinks the scroll list first; the column
  only grows past its previous height when the list is already at its floor.

## The add-card footer

Shown only on real boards — the synthetic All board has no column to write to,
and project boards create cards through the modal. The button passes `lane.id`,
so on a board where extra SQLite columns fold into a canonical lane the card is
written to the lane's real owner column, never a synthetic id.

- Look: a full-width 30 px ghost row under the cards, `Text::Small` in
  `text_muted`, a 12 px plus icon, a `hover` wash and `text` colour on hover,
  8 px side margins so it lines up with the card list.
- Click opens an inline draft, not the card modal: a 34 px `field`-filled
  input (same metrics as the card modal's title field) with an accent
  "Add card" submit and an `X` cancel, in the same place as the button.
- Enter submits and the form closes; an empty draft keeps it open (egui
  behaviour) without writing anything. Escape and a press anywhere else on the
  board cancel it. The draft text is stashed per column, so reopening restores
  it; a successful add clears it.
- Empty columns show the same button below the 30 px body, so the column grows
  from 65 px to 101 px and stays a drop target.


## Verification

`crates/app-gpui/tests/shell.rs::columns_hug_their_cards_within_the_viewport`
boots the real shell on a seeded temp database, then:

- adds 20 cards to the first column (overfull),
- leaves the second column with its one seeded card,
- deletes the third column's seeded card (empty),
- renders one frame and measures `#board-columns` and the three columns.

Measured on a 976 px test window: row `976`, overfull column `960`, one-card
column `140`, empty column `101` (the last two include the 30 px add row). The
assertions require the one-card column to be at least 40 px shorter than the
row, the overfull column to stop at the row but come within 24 px of it, and
the empty column to be shorter than the row yet taller than 55 px.

The footer has its own tests in the same file:

- `the_column_footer_adds_a_card` — every column of a real board shows the row,
  clicking it opens the draft, typing and Enter writes the card to that column
  and closes the form.
- `the_add_draft_survives_escape_on_an_empty_column` — an emptied column still
  offers the row; Escape writes nothing and keeps the text, and reopening plus
  Enter commits the restored draft.
- `the_all_board_has_no_add_row` — the synthetic All board renders no footer
  after switching to its tab.

The test was also run with `.items_start()` removed: the one-card column became
960 px and the first assertion failed, confirming the row's default `stretch` —
not `.h_full()` alone — was the primary cause.

Gates in `crates/app-gpui`: `cargo fmt --check`, `cargo clippy --all-targets`,
`cargo test`, `cargo build --bin kandrium-gpui` — all clean. The project Tasks
tab reuses the same `board_view`, and `tests/projects.rs` (which drops a card at
a column's centre) also passes with the column-root handlers.

## Left different from egui

- **Add row placement.** egui draws `theme::ghost_button` after the card body
  and before the board's bottom gutter, outside the scroll area; gpui does the
  same, but the footer is a sibling inside the shrink-wrapped column rather than
  at a fixed offset. The draft form is inline in both, not the card modal.
- **No estimate step.** egui pre-computes `max_body` from the header height and
  the add-form height, then decides between direct layout and a scroll area.
  gpui lets flex resolve it: content-sized up to `max_h`, then `flex_grow`
  distributes the clamped height to the list. The outcomes match; the gpui path
  cannot under-estimate a card's height.
- **Bottom gutter.** egui subtracts a named `BOTTOM_GUTTER = 15` (panel margin
  plus scrollbar lane). gpui reserves `pb(16)` on the row, so a capped column
  stops 16 px above the row's bottom; there is no separate scrollbar-lane term
  because gpui scrollbars float over the content.
- **Empty-column height.** egui floors the card body at 30 px on top of the add
  row; gpui's 30 px floor is the card list itself (which also contains the 6 px
  indicator lane and its 6 px bottom padding), then the 30 px footer follows,
  producing a 101 px empty column. Slightly shorter than egui's, still
  comfortably landable.
- **Column folding.** gpui's `lanes` (lines 187–235) folds extra non-canonical
  SQLite columns into the three canonical lanes. That is orthogonal to height,
  but it is why the height test uses the seeded canonical columns rather than
  creating a fourth one.
