# Titlebar chrome: drag surface and live city clock

## Scope

Two changes to the gpui shell's 44 px titlebar row, in
`crates/app-gpui`:

1. The window-drag surface must exclude the section control (chips, their
   gaps and the control's 2 px padding), while the empty stretch to the right
   of the last chip keeps moving and zooming the window.
2. A live city clock in that right-hand stretch: the current time and name of
   one saved Clocks city, a down-arrow menu of saved cities, persisted
   selection, and a minute tick that runs in every section.

Non-goals: the shipping egui app (`crates/app`) is untouched. The board
drag-and-drop parity work is tracked separately and is not part of this spec.

## 1. Window drag surface

### Symptom

A press or drag anywhere in the titlebar row moves the window, including on
the section chips, the 2 px gaps between them and the control's 2 px padding.
Double-clicking there zooms.

### Root cause

`crates/app-gpui/src/lib.rs::run()` opens the window with
`TitlebarOptions { appears_transparent: true, .. }` and leaves
`WindowOptions::app_owns_titlebar_drag` at its default `false`. On macOS,
`gpui-pre-macos-0.3.12/src/window.rs:3660-3677`
(`opaque_rect_for_window_move_when_in_titlebar`) returns an empty rect in that
case, so AppKit keeps its native titlebar dragging over the whole transparent
titlebar region. The `titlebar_drag` handlers on `titlebar-drag-left` /
`titlebar-drag-right` in `app.rs::titlebar()` never gate that: AppKit handles
the move before gpui routes the press to our elements. The chips therefore
move the window even though they have no drag handler.

### Change

- `crates/app-gpui/src/lib.rs::run()`: add
  `app_owns_titlebar_drag: true` to the `WindowOptions`, with a comment that
  the app draws its own titlebar and only its empty stretches call
  `start_window_move`.
- `crates/app-gpui/src/app.rs::titlebar()`: keep `titlebar_drag` exactly on
  the two empty regions — `titlebar-drag-left` (78 px traffic-light inset) and
  `titlebar-drag-right` (`flex_1` stretch). The section control, the clock and
  their descendants carry no drag handler. Extend the row comment to state
  that invariant.
- `titlebar_drag` itself is unchanged: one press calls
  `window.start_window_move()`, `click_count == 2` calls `window.zoom_window()`.

### Consequences

- Pressing or dragging on the section control (including its padding and
  internal gaps) selects or does nothing; it never moves or zooms the window.
- The empty stretch right of the last chip keeps drag and double-click zoom.
- Traffic lights keep working: they are native subviews above our content.
- Double-click zoom is now app-driven and always zooms; the macOS
  "double-click the titlebar" system preference is no longer consulted. This
  matches the requested behaviour but is a deliberate difference from the
  previous AppKit path.

## 2. Live city clock

### Existing building blocks

- `crates/app-gpui/src/views/clocks.rs`:
  - `ClockEntry { id, tz_identifier, city, zone: Option<TimeZone>, people }`.
  - `ClockState.entries` is the saved city list in saved order;
    `ClockState.now: Timestamp` is the frame's clock reading.
  - `ClockState::next_wait()` = nearer of `until_next_minute(now)` and the
    nearest travel boundary.
  - Cards render `format_clock(state.now, zone, use_24h)` and use
    `Text::ClockTime` for the time, `Text::Micro` (10 px JetBrains Mono) for
    meta text.
- `crates/app-gpui/src/app.rs`:
  - `load_clocks()` reads one snapshot through `Store::clock_data()` and
    invalidates the tick.
  - `ensure_clock_tick()` keeps a loop alive that refreshes
    `clocks.now` from `Timestamp::now()` and repaints, never touching SQLite.
  - `render()` currently takes one `Timestamp::now()` per frame only when
    `section == Clocks`.
- `crates/app-gpui/src/settings.rs`: `key=value` store next to the database.

### Settings

New key `titlebar_clock` holding the selected clock id:

- `Settings::titlebar_clock(&self) -> Option<&str>` — `None` for a missing or
  empty value.
- `Settings::set_titlebar_clock(&mut self, clock_id: Option<&str>)` — inserts
  the id, or removes the key when `None`.
- Covered by `settings.rs` tests: round trip, absent key, empty value.

### Shell state

- `AppShell.titlebar_clock: Option<String>` — seeded from settings at
  `with_store`, written back by `persist_settings`.
- `AppShell.titlebar_clock_menu: bool` — whether the dropdown is open.
- `AppShell::resolved_titlebar_clock(&self) -> Option<&ClockEntry>`:
  1. the saved id, when `clocks.entries` still contains it;
  2. otherwise the first saved city;
  3. `None` when no cities are saved.
  Resolution never writes settings, so a stale id is left in the file until
  the user picks a city again.

### Rendering

`app.rs::titlebar()` child order becomes:

```
titlebar-drag-left (78 px, drag)
section-control    (no drag)
titlebar-drag-right (flex_1, drag)
titlebar-clock     (no drag; omitted when no cities)
```

The clock is flush with the window's right edge apart from its 8 px right
inset; whatever empty stretch is left over stays the drag surface, now to the
left of the clock.

`AppShell::titlebar_clock(cx) -> Option<AnyElement>`:

- Container id `titlebar-clock`, `test_support`, h 26 px, px 8, mr 8,
  `flex_shrink_0`, `cursor_pointer`, hover `bg(t.hover)`, flex row, gap 6,
  vertically centered; `aria_label` is the full one-line label.
- `titlebar-clock-text` — one mono micro run
  `Wed 11 Nov 8:13 pm CityName` (`format_titlebar_clock`): `%a %-d %b`
  in the destination zone, then a lowercase 12-hour `%-I:%M %P`, then the
  city name. Single run, no mixed weights, `whitespace_nowrap`.
- `titlebar-clock-arrow` — `Icon::ChevronDown` at 10 px, `t.text_muted`.
- `zone == None` falls back to `--:-- CityName`.
- The titlebar is always 12-hour, independent of the Clocks page toggle.
- `on_click` toggles `titlebar_clock_menu` and notifies.

`AppShell::titlebar_clock_menu(&self, cx) -> Option<AnyElement>` is appended to
the `app-root` overlay list (same place as the card modal overlay, so it paints
above the section body and is never clipped):

- `titlebar-clock-backdrop`: `absolute`, inset 0, transparent; mouse-down
  closes the menu. Because it is the topmost overlay, any press outside the
  panel dismisses.
- `titlebar-clock-menu`: `absolute`, `top(px(40.))`, `right(px(8.))`, w 220,
  `bg(t.card)`, 1 px border, `radius_lg`, p 4, flex column, gap 1. It stops
  mouse-down propagation so panel presses never reach the backdrop.
- One row per `clocks.entries`: id `titlebar-clock-item-{clock_id}`, 26 px,
  px 8, rounded 4, `Text::Micro`; city left (`t.text` when selected,
  `t.text_muted` otherwise) and the city's current time right
  (`t.text_faint`); hover `bg(t.hover)`; a 6 px accent bar on the selected
  row. `on_click` calls `select_titlebar_clock`.

No menu row exists when `entries` is empty, and the whole control is omitted
then, as required.

### Selection and persistence

`AppShell::select_titlebar_clock(&mut self, clock_id: &str, cx)`:

1. Ignore the id when it is not in `clocks.entries`.
2. Set `titlebar_clock`, close the menu, `persist_settings()`, `cx.notify()`.

`persist_settings()` gains
`self.settings.set_titlebar_clock(self.titlebar_clock.as_deref())` so the key
is written with the rest of the preferences.

### Tick

- `render()` always runs:
  `self.clocks.set_now(Timestamp::now()); self.ensure_clock_tick(window, cx);`
  (one `Timestamp::now()` per frame; removing the `Section::Clocks` gate).
- `ensure_clock_tick` drops the `this.section != Section::Clocks` exit so the
  loop lives while any section is open. It still exits when
  `clock_tick_running` no longer matches the generation, and still waits on
  `next_wait()` (next minute or nearest travel boundary), refreshing only the
  cached `now` and repainting.
- `apply_external_change` calls `load_clocks()` for every section before its
  per-section reload, so a city added or removed from another process updates
  the titlebar everywhere without polling. The `Section::Clocks` arm then no
  longer needs its own call.

### Chrome interaction

Neither the clock container, its arrow, nor the menu panel carries
`titlebar_drag`; with `app_owns_titlebar_drag: true`, AppKit cannot move the
window from them either. The remaining `titlebar-drag-right` stretch keeps
drag and double-click zoom.

## Files and functions changed

- `crates/app-gpui/src/lib.rs` — `run()`: `app_owns_titlebar_drag: true`.
- `crates/app-gpui/src/settings.rs` — `titlebar_clock` accessors + tests.
- `crates/app-gpui/src/app.rs` —
  `AppShell` fields `titlebar_clock`, `titlebar_clock_menu`; `with_store`
  seeding; `persist_settings`; `resolved_titlebar_clock`;
  `titlebar_clock`; `titlebar_clock_menu`; `select_titlebar_clock`;
  `titlebar()` child order + comments; `render()` tick + overlay;
  `ensure_clock_tick` guard; `apply_external_change` clocks reload.
- `crates/app-gpui/tests/shell.rs` — new headless tests (below).

## Tests

Headless (`crates/app-gpui/tests/shell.rs`, `#[gpui::test]`, `gpui_kit::init`
first, real seeded temp DB per test):

1. `the_titlebar_drag_surface_excludes_the_section_control`
   — the control sits between the left inset and the empty stretch, and the
   clock sits after the stretch, so no chip or clock pixel is a drag surface.
2. `the_titlebar_clock_fits_the_minimum_window_width`
   — resize to 940 × 700; the control, the empty stretch and the clock all
   lay out without collision, and the clock ends 8 px before the right edge.
3. `the_titlebar_clock_shows_a_saved_city`
   — seed data renders, `titlebar-clock` is visible, its label ends with the
   first city's name and `titlebar-clock-text` is present.
4. `the_titlebar_clock_menu_switches_and_persists`
   — click `titlebar-clock`; assert `titlebar-clock-menu` is visible and has
   one row per city; click the second `titlebar-clock-item-{id}`; assert the
   label changed, the menu closed, and `<db parent>/gpui-settings.conf`
   contains `titlebar_clock=<id>`; rebuild the shell from that file and assert
   it resolves the same city.
5. `the_titlebar_clock_falls_back_when_its_city_is_removed`
   — select the second city, remove it with `Store::remove_clock`, switch to
   Clocks and back (the existing `load_clocks` path), assert the control
   shows the first remaining city and the stale id is still in the file.
6. `the_titlebar_clock_disappears_without_cities`
   — remove every clock, reload, assert `titlebar-clock` is not rendered.
7. `settings.rs` unit test for the `titlebar_clock` key (round trip, absent,
   empty).
8. `app.rs` `titlebar_clock_format_matches_the_spec` — fixed UTC instants
   render `Wed 11 Nov 8:13 pm` and `Mon 5 Jan 12:05 am` (no leading zeros).

What is not testable headlessly: AppKit's titlebar move/zoom decision. Test 1
pins the layout invariant; the actual move/zoom behaviour needs a human check
after relaunch.

## Verification

Inside `crates/app-gpui`:

```sh
cargo fmt
cargo clippy --all-targets
cargo test
cargo build --bin kandrium-gpui
```

Then relaunch the dev bundle
(`killall kandrium-gpui; open -n target/KandriumGpui-dev.app`) and manually
confirm: dragging from a chip does nothing; dragging the empty right stretch
moves the window; double-clicking the stretch zooms; the clock ticks across a
minute in Tasks; the menu opens and switches cities; relaunch keeps the city.

## Remaining differences / risks

- Double-click titlebar zoom no longer follows the macOS system preference; it
  is always zoom (as requested).
- The menu is a shell overlay (absolute panel + backdrop), not a native menu:
  no keyboard navigation or type-ahead, and it has no submenu behaviour. The
  request only calls for a list of saved cities.
- If the selected city is deleted, the control silently shows the first saved
  city; the settings file keeps the old id until the user picks again.
- `Text::Micro` is 10 px mono, the Clocks cards' meta size, so the clock is
  compact chrome by design.
- The titlebar clock always uses a 12-hour lowercase time, independent of the
  Clocks page's 12/24-hour toggle; the Clocks cards keep their own layout and
  formatting.
- At 940 px width the row measures: control 78 → 539, empty drag stretch
  539 → 762 (223 px), clock 762 → 932 (170 px) with the 8 px right inset.
