# Tasks: titlebar chrome (drag surface + live city clock)

Ordered implementation steps. Each step is complete when its check passes.

- [x] **1. Window drag surface**
  - `crates/app-gpui/src/lib.rs::run()`: `app_owns_titlebar_drag: true` with
    the comment from the spec.
  - `crates/app-gpui/src/app.rs::titlebar()`: `titlebar_drag` only on
    `titlebar-drag-left` and `titlebar-drag-right`; row comment updated.
  - Check: `cargo build --bin kandrium-gpui` succeeds.

- [x] **2. Persisted selection key**
  - `crates/app-gpui/src/settings.rs`: `titlebar_clock()` /
    `set_titlebar_clock()` plus unit tests (round trip, absent key, blank).

- [x] **3. Shell state**
  - `AppShell.titlebar_clock`, `AppShell.titlebar_clock_menu`, seeding in
    `with_store`, write-back in `persist_settings`.
  - `resolved_titlebar_clock()` (saved id → first city → `None`) and
    `select_titlebar_clock()`.

- [x] **4. Titlebar control**
  - `titlebar()` places `titlebar_clock(cx)` at the far right, after the
    empty `titlebar-drag-right` stretch, with its 8 px right inset.
  - Ids `titlebar-clock`, `titlebar-clock-text`, `titlebar-clock-arrow`;
    one mono micro run `Wed 11 Nov 8:13 pm CityName` (`Text::Micro`, 12-hour,
    no leading zeros); chevron 10 px; click toggles the menu.

- [x] **5. City menu overlay**
  - `titlebar_clock_menu()` (backdrop + panel + one row per saved city, ids
    `titlebar-clock-backdrop`, `titlebar-clock-menu`,
    `titlebar-clock-item-{id}`), appended to the `app-root` overlay list.
  - Selecting a row persists and closes.

- [x] **6. Always-on minute tick**
  - `render()` takes `Timestamp::now()` and calls `ensure_clock_tick` in every
    section; no section gate in the loop.
  - `apply_external_change` reloads clocks in every section.

- [x] **7. Tests** (`crates/app-gpui/tests/shell.rs`)
  - `the_titlebar_drag_surface_excludes_the_section_control`
  - `the_titlebar_clock_fits_the_minimum_window_width` (940 × 700)
  - `the_titlebar_clock_shows_a_saved_city`
  - `the_titlebar_clock_menu_switches_and_persists`
  - `the_titlebar_clock_falls_back_when_its_city_is_removed`
  - `the_titlebar_clock_disappears_without_cities`
  - `settings.rs` `titlebar_clock_round_trips_and_clears`,
    `blank_titlebar_clock_reads_as_unset`; `app.rs`
    `titlebar_clock_format_matches_the_spec`.
  - Check: `cargo test` green (26 shell tests).

- [x] **8. Gates, rebuild, relaunch**
  - `cargo fmt`, `cargo clippy --all-targets`, `cargo test`,
    `cargo build --bin kandrium-gpui` inside `crates/app-gpui`.
  - `killall kandrium-gpui; open -n target/KandriumGpui-dev.app`.
