# Kandrium Clocks Section — Kwora Port Plan

Status: shipped · 2026-11-05 · companion to [plan.md](plan.md)

Phases 0–4 are implemented and verified (schema v8–v10, core rules, the
view, travel/relocate, and tests). Phase 5 is the closing docs and edge-case
pass. The city area later changed from a paged three-column page to one
scrolling grid of wrapping cards with drag-to-reorder; the layout section and
parity table below reflect that. The analysis is otherwise kept as the
original design record; where it differs from the code, the code is
authoritative.

Kandrium is a local-first Kanban desktop app on a pure-Rust stack
(eframe/egui UI, rusqlite data layer, rmcp MCP server). Kwora is a separate
SwiftUI macOS world-clock panel at
`/Users/zefang/Documents/career/projects/wclock` — 23 Swift files, 2,152 lines,
SwiftUI + SwiftData, two commits. It is a small always-on-top panel with three
city columns per page, search, colleague avatars, saved zones and temporary
travel assignments, not a task surface.

This document plans adding Kwora's behavior as a fifth top-level Kandrium
section, "Clocks", alongside Tasks, Projects, Notes and Documents. It is
self-contained; architecture rules live in [AGENTS.md](AGENTS.md).

## Decision: port, do not embed

Port the behavior to Rust/egui. The domain rules (zone list, colleague
placement, travel windows) go to `crates/core`; the view goes to
`crates/app/src/views/wclock.rs`.

Embedding alternatives are rejected:

- **Host SwiftUI inside eframe** (`NSHostingView` bridged into the winit
  window): egui and AppKit would compete for event routing, focus, IME, hit
  testing and repaint scheduling; it is macOS-only; and it would make
  `cargo packager` depend on an Xcode-built Swift bundle.
- **Swift ↔ Rust FFI for data only**: still leaves a SwiftUI view hierarchy to
  host, adds a second runtime and store, and duplicates the theme/layout system
  for no gain.
- **Ship Kwora as a separate app**: does not satisfy the in-app section
  requirement and splits navigation between two products.

The port is modest: most of the 2,152 lines are view code, while
`ClockFormat.swift`, `EffectiveZone.swift` and `TravelService.swift` are small,
pure rules that map cleanly to core functions and unit tests.

## UI mapping to Kandrium conventions

- **Navigation**: append `Section::Clocks` to the enum at `app.rs:53-61`; grow
  the local `SECTIONS` array and segment list at `app.rs:763-786` from 4 to 5
  with `{ label: "Clocks", icon: "globe" }`, rendered through the existing
  `theme::segmented_control` (`theme.rs:504`). Append rather than insert so the
  existing tab order and persisted `section` values stay stable.
- **Dispatch**: add a `Section::Clocks` arm to the exhaustive match in
  `app.rs` (`content` around lines 1030-1085). There is no board-tab row for
  Clocks, so the section gets the full body; no `View::Board`/`View::Archived`
  interaction is needed.
- **State**: add `clocks_state: wclock::WorldClockState` to `KandriumApp` and
  construct it in `new()` (mirroring `notes_state` / `documents_state`).
  `WorldClockState` caches the clock list, people/travel rows and resolved
  `TimeZone` handles; load once on section entry and after each mutation, never
  per frame. Search, drag/reflow and inspector are session-only fields. Register
  `pub mod wclock;` in `crates/app/src/views.rs`.
- **Persistence**: `SavedState` (`app.rs:96-134`) gains only
  `#[serde(default)] clocks_use_24h: bool` (default `false`, matching Kwora).
  Search text, open inspector and form drafts are session state and are not
  persisted. `SavedState` is eframe's storage blob; it must not become the
  store for city/person rows.
- **Toolbar convention**: the repo defines `TOOLBAR_H_MARGIN` (10 px,
  `app.rs:29`) and uses it for full-bleed hairlines. There is no
  `TOP_TOOLBAR_H_MARGIN` constant today — reuse the existing one rather than
  adding a synonym. Clocks has no row-2 tabs, so the existing title/section
  hairlines remain untouched.
- **View layout**: `views/wclock.rs` draws its own toolbar (search, 12/24
  toggle, add-city button, filtered city count) over one vertically scrolling
  page of city cards. Cards keep Kwora's three-across width
  (`(available − 2 gaps) / 3`, 160 px floor) and **wrap onto as many rows as
  needed** — there is no pagination, page state or page clamping. Dragging a
  card reorders the grid (`dnd::GridZone` insertion index, `anim` reflow and
  snap-back curves), and the committed permutation goes through
  `clocks::reorder_clocks`; the "Move earlier / Move later" menu items remain
  as a keyboard-free fallback. Drag is offered only while the search is empty.
  No horizontal scroll.
- **Fonts**: use only `text_styles` from `theme.rs:406-421` and resolve via
  `theme::font_id`. Map city name to `small_strong()` (13 semibold), date and
  GMT offset to `TextStyle::Small` (13 regular, muted tokens), and the clock
  time to a new named `clock-time` style added to that table. Recommend
  `clock-time` = `FontId::monospace(20.0)`: Kwora uses
  `.monospacedDigit()` (`ZoneColumnView.swift`) so digits do not jitter as
  minutes change; Kandrium's monospace family (JetBrains Mono) gives the same
  stability. If visual review prefers Inter, use `TextStyle::Heading` and
  accept proportional-digit jitter. Never pass raw pixel sizes or port Kwora's
  9 pt/10.5 pt meta sizes.
- **Icons**: extend the embedded Lucide table (`icons.rs:10`) with `globe`
  (section segment), `plane` (travelling badge) and `house` (relocate); reuse
  `search`, `chevron-left/right`, `plus`, `ellipsis`, `x`. No emoji and no new
  icon crate.
- **Design system**: use `theme::Tokens` (`theme.rs:35`) — 6 px radius,
  gray-on-gray surfaces, single blue accent for selected/travelling state.
  Kwora's 22 px outer radius, dark glass/panel background and hover wash are
  not ported. Avatar chips are neutral with initials; the travel badge uses
  the accent ring instead of Kwora's five `avatarSeed` colors (see Risks and
  open decisions).
- **Inspector**: clicking a city opens an `egui::Modal` (`theme::dialog_frame`,
  same pattern as the card modal) with residents, visitors, add-colleague,
  travel/relocate forms, rename/remove, and zone move/remove actions. Use a
  fixed-height inner `ScrollArea` once the list can overflow; apply
  `widgets::CONTENT_RIGHT_INSET` inside that scroll as the existing card
  surfaces do.
- **Destructive actions**: zone and colleague removal use an explicit confirm
  step with the `confirm.rs` copy style. Do not silently delete people or
  their travel history.

## Data layer: core + migration, not SavedState

**Recommendation: put clock rows in `kandrium-core` with a new additive
migration.** `SavedState` should hold only display preferences.

Why:

- Cities are ordered, unique domain records; colleague home assignments and
  travel windows are relational data with overlap/date rules. SavedState is an
  opaque eframe JSON blob with no transactions or constraints.
- All data access must stay in `crates/core` per the architecture rules; a
  UI-only store would bypass that single source of truth.
- A future export, MCP surface, or second front-end needs the same rows; moving
  user data out of a UI blob later is a painful migration.
- Display preferences (12/24h) are genuinely per-device and belong in
  `SavedState`.

Concrete shape:

- `migrations/0008_clocks.sql` (Phase 0): `clocks` table only —
  `id TEXT PRIMARY KEY`, `tz_identifier TEXT NOT NULL UNIQUE`,
  `position INTEGER NOT NULL`, `created_at TEXT NOT NULL`, plus an index on
  `position`. `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS`.
- `migrations/0009_clock_people.sql` (Phase 3): `clock_people` (`id`, `name`,
  `home_clock_id` → `clocks` with `ON DELETE RESTRICT`, `created_at`) and
  `clock_travels` (`id`, `person_id` → `clock_people` `ON DELETE CASCADE`,
  nullable `origin_clock_id` / `destination_clock_id` → `clocks` with
  `ON DELETE SET NULL`, `starts_at` and `ends_at` as Unix epoch seconds (UTC),
  `cancelled_at`, `created_at`). Keeping the second step separate avoids dead
  schema if the colleague phase is cut; `SET NULL` lets `remove_clock` keep
  historical trips after cancelling active visits. Kwora's `label`,
  `avatarSeed`, `team` and `isArchived` columns have no UI and are not ported.
- Register both files in `crates/core/src/db/migrations.rs` and bump
  `SCHEMA_VERSION` in order (7 → 8 → 9); never edit earlier files. Keep the
  fresh / v1…v7 upgrade tests passing.
- New module `crates/core/src/clocks.rs` with synchronous `&Connection`
  functions, exported from `lib.rs`:
  `list_clocks`, `add_clock`, `move_clock`, `remove_clock`,
  `list_people`, `add_person`, `rename_person`, `remove_person`,
  `relocate_person`, `start_travel`, `return_home`, and a pure
  `placement_at(person, instant)`. Multi-step writes use a transaction.
- `remove_clock` must reject a zone with residents and cancel active travel to
  the zone before deleting it (parity with `AppServices.removeZone`).
  `remove_person` cascades their travel history. `relocate_person` cancels
  active trips whose destination is the old or new home. `start_travel`
  captures the origin from the resolver at `max(start, now)`, keeps home
  immutable, and enforces the one-open-ended/overlap rules in the same
  transaction.
- **Seed policy**: seed Kyiv / Tokyo / Los Angeles only for a fresh install.
  `db::seed` already returns early when boards exist (`seed.rs:18`); add the
  clock seed inside that fresh path (or a `clocks::seed_defaults` called only
  when `board_count == 0`). This prevents deleted clocks from reappearing on
  every boot. Use `Europe/Kyiv` with a `Europe/Kiev` fallback.
- **No SwiftData import**: Kwora's `WClock.store` (`Persistence.swift`) is
  SwiftData-owned. Reading it requires a Swift helper; a one-off JSON export
  from a small Swift CLI would be the only sane path. Recommend out of scope
  unless the user needs their existing data carried over.

## Time zones in Rust

- Use **`jiff` 0.2**, already a workspace dependency used by core and app
  (`Cargo.toml:15`, `crates/core`, `crates/app`). No new time-zone crate.
  Its default features include `tz-system`, `tzdb-bundle-platform` and
  `tzdb-zoneinfo`, so IANA IDs resolve from the macOS system database and
  bundled platform data on Windows.
- Store the **IANA identifier** (e.g. `Europe/Kyiv`), never a fixed offset.
  Resolve `TimeZone::get(identifier)` on add and cache the handle in
  `WorldClockState`; invalid identifiers are rejected at the form.
- Render from one absolute instant:
  `Timestamp::now().to_zoned(time_zone)`. Format 24h as `%H:%M`, 12h as
  `%I:%M` + lowercase `%p`; day as the `EEE, d MMM.` equivalent; offset from
  `offset().seconds()` with `GMT`, `GMT+5`, `GMT+5:30`. This reproduces
  `ClockFormat.swift` and is DST-correct because the offset is computed at the
  displayed instant.
- Travel dates: resolve start-of-day and 23:59:59 **in the destination zone**
  to epoch seconds, preserving `TravelService` semantics. Handle skipped/
  ambiguous local times explicitly (jiff can disambiguate or error) and test
  spring-forward/fall-back, half-hour offsets (`Asia/Kolkata`), 45-minute
  offsets, and southern-hemisphere DST.
- **Tick cadence**: Kwora's ticker fires every second but never renders
  seconds (`ClockTicker.swift`, `ClockFormat.swift`); Kandrium only needs a
  minute. In `wclock::show`, compute `now` once per frame and call
  `ui.ctx().request_repaint_after(next_minute + 50ms)`. Also schedule at the
  nearest travel `ends_at` so a person reverts home on time. Only schedule
  while Clocks is the active section; do not run a perpetual 1 s or 60 fps
  timer. If a seconds toggle is ever added, use a 1 s repaint only while
  visible and enabled.
- **Watcher coexistence**: the tick is read-only UI work — no SQLite reads or
  writes per frame and no `data_version` change. City/person/travel CRUD
  writes are explicit user actions through core and use the existing
  write/reload path. Do not call the board reload from the tick; the 250 ms
  `watcher.rs` poll and the clock schedule are independent.

## Feature parity checklist

| Kwora feature (source) | Kandrium work | Decision |
| --- | --- | --- |
| 3 city columns per page, page chevrons (`CardView.swift`) | Wrapping grid: three across, further rows instead of paging, drag-to-reorder plus Move earlier/later | Port (adapted) |
| Search cities/regions/identifiers/people, empty + no-results states (`CardView.swift`) | `theme::search_field`; match display name, region, IANA id, person name; filtered count in the header | Port |
| Time 24/12h, `EEE, d MMM.`, GMT offset (`ClockFormat.swift`) | jiff `strftime` + offset formatter; lowercase am/pm | Port |
| City name from last IANA segment, `_`→space, optional label (`Models.swift`) | Derive on display; no label column (Kwora has no label editor) | Port (derived only) |
| Default Kyiv/Tokyo/Los Angeles, Kyiv fallback (`Seed.swift`) | Fresh-install seed in core with alias fallback | Port |
| Add city with searchable system IANA list, 200 cap, Added state, offset preview (`AddCityView.swift`) | Modal with `jiff::tz::db().available()` list + search; disable already-added rows | Port |
| Resident/visiting avatar strip, 2 + "+N", plane badge (`ZoneColumnView.swift`) | Neutral initials chips, accent travel ring, `plane` icon, 2 + "+N" | Port (design-adapted) |
| Inspector: residents/visitors, add/rename/remove person, zone move/remove (`ZoneInspectorView.swift`) | `egui::Modal` with the same actions and guards | Port |
| Travel: destination, effective day, auto-return 23:59 destination, open-ended, overlap check, close older open trip (`TravelFormView.swift`, `TravelService.swift`) | Core transaction with same rules; date pickers in the modal | Port |
| Return home now; relocate home; cancel active trips to old/new home (`AppServices.swift`) | Core `return_home`, `relocate_person` | Port |
| Resolver: latest active travel wins, home immutable, archived people hidden (`EffectiveZone.swift`) | Pure `clocks::placement_at` + tests | Port |
| SwiftData store at `WClock.store`, memory fallback (`Persistence.swift`) | Kandrium SQLite via core; no SwiftData import | Replace |
| 1 s ticker while visible / 60 s occluded (`ClockTicker.swift`) | Minute-aligned repaint while the section is visible; travel-expiry wake | Adapt |
| Settings: 12/24, always-on-top, launch at login, reset position (`SettingsView.swift`) | Only 12/24 persisted in `SavedState`; rest not applicable | Partial |
| Ellipsis menu: add city, settings, pin, reset, quit (`EllipsisMenuView.swift`) | Direct in-section controls; no pin/reset/quit | Drop |
| Menu-bar globe, accessory activation, non-activating floating panel, always-on-top, per-display position (`AppDelegate.swift`, `PanelController.swift`) | Kandrium is a normal window with a section tab | Drop |
| Dark glass panel, 22 px radius, 9-10 pt meta text (`Theme.swift`, `ZoneColumnView.swift`) | Kandrium tokens, 6 px radius, 13 px minimum text | Drop |
| Team field and `isArchived` (models only; no UI action) | Omit columns until a real archive/team UI exists | Defer |
| Day/night visuals | No day/night rendering exists in the Kwora source (`Theme.swift` is static) | Drop |
| Localization | Hardcoded English, `en_US_POSIX`; Kandrium stays English | Drop |
| JSON export/import, menu-bar widget, notifications | Not present in the Kwora source; out of scope | Drop |

## Deliberately dropped

- Floating/non-activating panel, always-on-top/pin, per-display position
  memory, menu-bar globe, launch at login, quit action, window shadow/glass.
  These are macOS shell features, not a section inside a normal window.
- 1 s ticker and occlusion detection — a minute-aligned repaint is enough for
  a minutes-only display and costs one frame per minute.
- 9 pt/10.5 pt type and the 260/3 column width — Kandrium's design system
  floors text at 13 px and the section has real window width.
- Five-colour avatar seeds, 22 px radii and the dark panel palette — deliberate
  design-system exception in favour of neutral chips + one blue accent.
- Team/archive fields, JSON export/import, WidgetKit/menu-bar extras,
  day/night backgrounds, localization — not present or not actionable in the
  Kwora source.
- SwiftData store migration/import.

## Phased milestones

Each phase is independently mergeable and shippable; Phase 1 is the first
user-visible milestone.

| Phase | Status | Scope | Main files | Verification |
| --- | --- | --- | --- | --- |
| 0 | Shipped | Clock schema, core models, seed, format/offset helpers, placement resolver | `migrations/0008_clocks.sql`, `crates/core/src/clocks.rs`, `db/migrations.rs`, `seed.rs`, `lib.rs` | `cargo test -p kandrium-core` (fresh + v7 upgrade, DST format table, seed rules) |
| 1 | Shipped | Clocks section shell: 5th segment, read-only 3-city page, search, 12/24 toggle, minute tick, empty state | `app.rs` (`Section`, `SavedState`, dispatch), `views/wclock.rs`, `views.rs`, `theme.rs` (`clock-time`), `icons.rs` | Headless layout test + `cargo build -p kandrium-app`; relaunch and watch a minute rollover |
| 2 | Shipped | Zone management: searchable IANA add dialog, remove guard, move left/right, pagination, search filtering | `views/wclock.rs`, `crates/core/src/clocks.rs` | Core CRUD tests, headless filter/page tests, manual add/remove/reorder |
| 3 | Shipped | People: `0009_clock_people.sql`, add/rename/remove, residents/visitors, avatar strips, inspector modal | `migrations/0009_clock_people.sql`, `clocks.rs`, `views/wclock.rs` | Core relationship tests, headless inspector render, manual flows |
| 4 | Shipped | Travel and relocation: forms, open-ended/overlap rules, return home, expiry-aware repaint, relocate cancellation | `clocks.rs`, `views/wclock.rs` | Travel/DST unit tests, resolver tests, manual travel across a day boundary |
| 5 | Done in this change | Polish: header width at 940 px, tooltips, empty/no-results copy, light/dark, reduced motion, docs | `app.rs`, `views/wclock.rs`, `README.md`, `AGENTS.md` | Baseline commands + manual pass |

## Verification

Every phase runs the repo baseline:

```sh
cargo fmt
cargo clippy --workspace
cargo test --workspace
```

Feature-specific checks:

- **Core**: a fresh DB and a v7 snapshot both migrate to the current head
  (8 after Phase 0, 9 after Phase 3); seed only runs on a fresh database;
  unique IANA ids; delete guard; travel overlap/open-trip closing/return-home/
  relocate semantics; resolver picks the latest active trip; offset/format
  table across DST transitions.
- **Headless UI**: in `views/wclock.rs` tests call `theme::install_fonts` and
  `theme::apply_visuals` first, clear `textures_delta`, and scan shapes for
  duplicate-ID warnings. Assert 5 segments fit the 940 px minimum
  (`main.rs:321-322`), wrapping-grid math (one row, several rows, filtered
  count), scroll containment, drag reorder index math, search filtering,
  empty/no-results, and that the minute schedule is `request_repaint_after`,
  not a busy loop.
- **App**: rebuild and relaunch through the dev bundle:

  ```sh
  cargo build -p kandrium-app
  killall kandrium-app; open -n target/Kandrium-dev.app
  ```

  Verify the fifth segment, a minute rollover, add/remove/reorder, a person
  travelling and reverting, 12/24 switch, light/dark, and the modal on the
  940 px window. Ask the user to confirm visuals that the agent shell cannot
  capture.
- **MCP**: no clock tools are added, so `cargo test -p kandrium-mcp` and the
  curated 13-tool test must stay unchanged and green.

## Risks and open decisions

Each item states the recommendation first; the decision still belongs to the
product owner.

- **Five segments at 940 px**: adding "Clocks" widens the section row.
  Recommend "Clocks" and no width change until a headless measurement; if it
  overflows, switch to icon-only segments below a threshold or bump the minimum
  width.
- **Numeral style**: monospace 20 gives stable digit width but a different
  texture from Inter; Inter Heading looks closer to Kwora but jitters.
  Recommend `clock-time` = monospace 20 for stability, pending a visual
  sign-off. Adding `clock-time` to `text_styles` is the only compliant way to
  introduce the size.
- **Scope**: full parity includes colleagues and travel (Phases 3-4).
  Recommend shipping all five phases; if that is too much, Phases 0-2 deliver
  "my city clocks" and the 0008/0009 split keeps people/travel open for later.
- **Data location**: recommend DB-backed clocks in `kandrium-core` rather than
  per-device config; this is the main architectural commitment here.
- **Seed scope**: recommend seeding only on a fresh install, so an existing
  Kandrium DB opens an empty Clocks section until the user adds a city. If
  defaults are wanted for existing users, use a one-time `clocks_seeded` flag
  in `SavedState` on first open (never re-seed after a deliberate delete).
- **Existing Kwora data**: no import path for `WClock.store`. Recommend no
  import; if carry-over is required, a one-off Swift JSON exporter is the
  lowest-risk route.
- **Avatar palette**: recommend neutral chips to keep the single-accent rule;
  Kwora's colour-coded people would be a documented design-system exception.
- **MCP**: recommend no clock tools by default. Clocks are a personal
  presentation surface, and expanding the curated 13-tool contract adds
  mutation risk with no task-management value. If ever needed, start with a
  read-only `list_clocks`.
- **Time-zone source**: macOS/Linux use the system tzdb via jiff defaults; a
  future Windows build relies on the bundled platform database. Recommend
  verifying before any cross-platform release.
- **Deletion semantics**: recommend blocking deletion of a clock with
  residents but cancelling active visits; deleting a person cascades travel
  history. Confirm this matches user expectations.
- **Font coverage**: clock numerals and `GMT±H:MM` are ASCII and covered by
  Inter/JetBrains Mono. User-entered colleague names may be non-Latin; CJK
  coverage is an app-wide font limitation, not introduced here.
