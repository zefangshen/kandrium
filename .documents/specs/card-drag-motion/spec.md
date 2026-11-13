# Card drag motion

## Scope

Bring the gpui board's drag animation up to the egui reference
(`crates/app/src/anim.rs` + `crates/app/src/widgets/column.rs`), without
touching the shipping egui app. Files: `crates/app-gpui/src/anim.rs`,
`crates/app-gpui/src/views/tasks.rs`, `crates/app-gpui/src/app.rs`,
`crates/app-gpui/src/views/projects.rs`, plus tests.

## What the egui reference actually does

Read from `crates/app/src/anim.rs` and the board/column widgets:

1. **Pickup / lift** — the held card leaves the flow and a foreground ghost
   takes over. Over `LIFT_DURATION` (140 ms, `ease_standard`) it ramps scale
   `1.03`, opacity `0.92`, and a shadow. The grab point stays under the
   cursor (`grab = pointer - rect.min`).
2. **Gap opening** — an insertion placeholder of the card's own height opens
   at the hovered insertion point over `GAP_DURATION` (160 ms,
   `ease_standard`). The dragged card is skipped in the in-flow list, so the
   placeholder is the only thing occupying its row.
3. **Reflow** — every card whose laid-out rect changed gets a FLIP track:
   paint is translated from its previous centre to the new one over
   `SHIFT_DURATION` (200 ms, `ease_standard`), with a per-card delay of
   `STAGGER_STEP` (20 ms) per slot distance below the gap, capped at
   `STAGGER_MAX` (100 ms). Only paint moves; hit rectangles stay at the real
   layout slots.
4. **Settle** — on release the ghost freezes its current floating rect and
   flies to the live placeholder rect over `SETTLE_DURATION` (220 ms,
   `ease_snap`), its elevation receding to 0. The real card stays hidden
   until touchdown, so the handoff is seamless.
5. **Snap-back** — a release outside every zone (refused) or a release that
   lands on the card's own slot parks the placeholder at the source slot and
   flies the ghost back there with the same `ease_snap` flight. Dropping onto
   itself is therefore a cancel, never a move.
6. **Reduced motion** — `eased()` returns the resting value, so gaps open
   instantly and tracks snap; the ghost is dropped without a flight.

## What gpui-pre can and cannot express

| egui piece | gpui-pre capability | Ships instead |
| --- | --- | --- |
| Paint-only FLIP transform (`PaintList::transform_range`) | No transform matrix and no paint-only range on elements. `Position::Relative` insets (`.top(px(..))`) shift an element's painted box after layout without moving siblings. | Animate `.top()` on a per-card wrapper over `SHIFT` + stagger. Geometry is equivalent to the FLIP translation; hit rects still follow the shifted paint, which egui avoided — acceptable because a moving card cannot be clicked mid-flight in practice. |
| Ghost stays alive after mouse-up | gpui tears down `active_drag` on release; `on_drag`'s view is not retained. | The shell owns an **overlay flight ghost**: an absolutely positioned card-like box in `app-root`, driven by `with_animation` + `ease_snap`, from the release point to the destination slot rect. |
| Scale `1.03` on lift | No scale/transform on divs. | Keep the opacity lift (`0.35 → 0.97`) and accent border. No scale; documented as a deliberate difference. |
| Animated shadow | No shadow builder confirmed on `Div`; `box_shadow` is a raw style field. | Skip the shadow ramp. The accent border is the lift cue. |
| Real insertion gap above the dragged card's row | Animating `h` on a slot reflows siblings every frame, but needs the card's height. | The gap is a fixed-height slot equal to the source card's measured row height, recorded by a zero-cost `canvas` measurement. No height animation is needed: the displaced cards glide, and the gap fades in. |
| FLIP needs previous rects | `Canvas::prepaint` receives the laid-out `Bounds`. | A `CardMetrics` map (`Rc<RefCell<HashMap<String, Bounds<Pixels>>>>`) records each card row and the gap slot. The map is read only at drag start and drop, so it needs no per-frame diffing. |

## State (in `AppShell`)

```rust
/// Measured row rects, keyed by card id and `gap:<dragged id>`.
drag_metrics: CardMetrics,              // Rc<RefCell<HashMap<String, Bounds<Pixels>>>>
drop_target: Option<DropTarget>,        // already exists
previous_target: Option<DropTarget>,    // where the gap was before this change
dragging_card: Option<String>,          // already exists
drop_generation: u64,                   // already exists
drag_gap_height: f32,                   // source row height, fallback 48.0
drag_origin: Option<Bounds<Pixels>>,
drag_origin_target: Option<DropTarget>, // source insertion point
drag_flight: Option<DragFlight>,
drag_flight_token: u64,
settled_card: Option<(String, u64)>,
```

```rust
struct DragFlight {
    card_id: String,
    title: String,
    from: Point<Pixels>,      // release pointer, window coords
    to: Bounds<Pixels>,       // destination gap rect
    token: u64,
}
```

## Handlers

- `begin_card_drag(card_id, column_id, before, cx)` — called from `on_drag`
  with the source column id and the id of the card after it (the `after`
  already computed in `card_view`). Seeds `drop_target` at the source slot
  (`generation += 1`), sets `previous_target = None`, reads
  `drag_gap_height` and `drag_origin` from `drag_metrics` (fallback 48 px),
  stores `drag_origin_target`, sets `dragging_card`.
- `set_drop_target(column_id, before, cx)` — unchanged behaviour plus
  `previous_target = old target` before replacing it, so the renderer can
  derive each card's glide offset.
- `drop_card(card_id, column_id, window, cx)`:
  1. Resolve `before` from the current target for that column, dropping it
     when it equals `card_id`.
  2. `same_slot` when `column_id` + `before` equal `drag_origin_target`.
  3. `same_slot` or no target → **cancel**: destination is `drag_origin`,
     target is reset to `drag_origin_target`, no DB write.
  4. Otherwise commit `move_all_card` / `move_card` as today, reload the
     displayed board, and set the target to `{column_id, before}`; the
     destination is `drag_metrics["gap:<card_id>"]` (fallback
     `drag_origin`).
  5. `start_drag_flight(from = window.mouse_position(), to, …)`.
- `cancel_card_drag(window, cx)` — new, called from the board's
  `on_mouse_up` and from the root's `on_mouse_up` when no drop handler ran
  (drop listeners stop propagation, so a release outside the board still
  cancels): same cancel path as (3).
- `start_drag_flight` — when `cx.reduce_motion()`, finish immediately
  (teleport) instead of spawning; otherwise set `drag_flight`, bump
  `drag_flight_token`, and `cx.spawn` a `background_executor().timer(SNAP_BACK)`
  that calls `finish_drag_flight(token, cx)`.
- `finish_drag_flight(token, cx)` — ignore stale tokens; clear
  `dragging_card`, `drop_target`, `previous_target`, `drag_flight`; set
  `settled_card = Some((card_id, token))`; notify.
- The real card stays hidden (`dragging_card`) for the whole flight, and the
  gap stays at the destination, so the card reappears exactly where the
  ghost landed.
- `open_board` / section switches clear all drag state, including
  `drag_flight`.

## Rendering (`crates/app-gpui/src/views/tasks.rs`)

- `board_view` gains `previous_target: Option<&DropTarget>`,
  `gap_height: f32`, `metrics: &CardMetrics`; `DragStyle` carries them.
- `column_view`:
  - Build `visible: Vec<&Card>` skipping `style.dragging`.
  - `gap_index(target, &visible)` = `before`'s position in `visible`, or
    `visible.len()`; `None` when the target belongs to another lane.
  - Render the current gap at that index: `div().h(px(gap_height))`, full
    width, a 6 px empty slot on top, then the accent block (one card body
    tall) carrying `measure(format!("gap:{card_id}"), metrics)` so the
    flight lands on the body, not the row. Fade the block in over `REFLOW`
    with `ease_standard`.
  - Each visible card gets `shift_offset(prev_gap, next_gap, index, gap_height)`:
    ```text
    open  (None→g):   index >= g            => -h   // layout moved down
    close (g→None):  index >= g            => +h   // layout moved up
    move down (a<b): a <= index < b        => +h
    move up   (b<a): b <= index < a        => -h
    otherwise                              =>  0
    ```
    Wrap that card in `with_animation(("shift", card_id, generation),
    Animation::new(SHIFT + STAGGER_MAX).with_easing(linear), …)` where the
    closure applies the per-card delay
    `stagger_order(next_gap, index) * STAGGER_STEP` capped at `STAGGER_MAX`
    and then `ease_standard`, setting `.top(px(offset * (1 - eased)))`.
    With `reduced`, `eased == 1` → no wrapper animation and no offset.
  - Drop the old always-reserved 6 px indicator and `append_slot`; the gap
    replaces the dragged card's row instead. (Keep the 6 px `indicator_slot`
    inside cards so row heights keep their 6 px rhythm and the measured row
    height equals the gap height.)
  - Remove `.when(is_dragged, opacity 0.4)`; the card is skipped instead.
  - `on_drag` passes `column_id` and `after` to `begin_card_drag`.
  - `on_drop` passes `window` to `drop_card`.
  - The board's `on_mouse_up` calls `cancel_card_drag(window, cx)` only when
    a drag is still active (no drop committed).
- `measure(key, metrics)` helper: `canvas` with `.absolute().inset_0()`
  recording `bounds` into `metrics` in `prepaint`. No notify.
- `card_view` root gains `.child(measure(card.id.clone(), metrics))`.
- `CardGhost` keeps the 140 ms opacity lift, adds the accent border and a
  slightly elevated background; no scale/shadow (documented).

## `anim.rs`

Add the egui durations and the stagger helper, keeping the existing curves:

```rust
pub const SHIFT: Duration = Duration::from_millis(200);
pub const STAGGER_STEP: Duration = Duration::from_millis(20);
pub const STAGGER_MAX: Duration = Duration::from_millis(100);

pub fn stagger_order(gap: Option<usize>, index: usize) -> u32;
pub fn shift_offset(
    previous: Option<usize>,
    next: Option<usize>,
    index: usize,
    gap_height: f32,
) -> f32;
```

`LIFT`, `REFLOW`, `SNAP_BACK`, `ease_standard`, `ease_snap`, `eased` stay.

## `app.rs` flight overlay

`drag_flight_overlay(cx) -> Option<AnyElement>` appended to the `app-root`
overlay list (same place as the clock menu):

- `.absolute()`, animated `.left()` / `.top()` interpolating
  `flight.from → flight.to.origin` over `SNAP_BACK` with `ease_snap`
  (key `("drag-flight", token)`).
- Width `COLUMN_W - 16`, card background, accent border, the card title,
  opacity easing from the lifted value to 1 as it lands.
- `reduced` teleports (the flight is not created at all).

## Files and functions changed

- `crates/app-gpui/src/anim.rs` — `SHIFT`, `STAGGER_STEP`, `STAGGER_MAX`,
  `stagger_order`, `shift_offset`, tests.
- `crates/app-gpui/src/views/tasks.rs` — `CardMetrics`, `measure`,
  `DragStyle` fields, `column_view`, `card_view`, `CardGhost`; delete
  `indicator_slot` / `append_slot`.
- `crates/app-gpui/src/app.rs` — new fields, `begin_card_drag`,
  `set_drop_target`, `drop_card`, `cancel_card_drag`,
  `start_drag_flight`, `finish_drag_flight`, `clear_drag_state`,
  `drag_flight_overlay`, both `board_view` call sites, overlay in `render`.
- `crates/app-gpui/src/views/projects.rs` — pass `previous_target`,
  `gap_height`, `metrics` to `board_view`.
- `crates/app-gpui/tests/shell.rs` — headless regression tests.

## Tests

Unit (`anim.rs`):
1. `shift_offset` open/close/move-down/move-up/adjacent cases and the
   zero-offset identity.
2. `stagger_order` grows with distance and stops above the gap.
3. Existing curve endpoint tests stay green.

Headless (`app.rs::settings_tests`), using the real seeded temp DB:
4. `a_drag_opens_a_same_height_gap_at_the_source` — `begin_card_drag` seeds
   the source target and a non-zero `drag_gap_height`.
5. `a_cancelled_drag_flies_back_before_the_card_returns` — the card stays
   hidden while the flight exists, then reappears with the settle token.
6. `a_drop_on_its_own_slot_does_not_move_the_card` — no reorder, drag state
   cleared after the flight.
7. `reduced_motion_cancels_without_a_flight` — teleports, no `drag_flight`.

## Verification

```sh
cd crates/app-gpui
cargo fmt
cargo clippy --all-targets
cargo test
cargo build --bin kandrium-gpui
```

Then relaunch the dev bundle:

```sh
killall kandrium-gpui; open -n target/KandriumGpui-dev.app
```

Manual check list: lift on pickup; gap opens at the pointer; cards glide when
the insertion point changes; a normal drop settles without a pop; a release
outside a column (or onto the card's own slot) flies back to the origin and
only then reveals the card; reduced motion teleports.

## Deliberate differences (cannot be expressed)

- No scale (1.03) and no animated shadow on the lifted card.
- The flight ghost is a shell overlay, not gpui's drag view; it is the same
  card-like box, not a pixel copy of the card body.
- Reflow moves both paint and hit rect (gpui has no paint-only transform);
  a card gliding under the pointer can be hit at its animated position.
