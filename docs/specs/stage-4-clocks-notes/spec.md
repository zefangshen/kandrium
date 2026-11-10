# Stage 4 — Clocks parity, then Notes

Status: awaiting approval. Branch: `gpui-migration`. Working tree verified clean at
2026-11-10 (no leftovers from the interrupted turn).

## 1. Goal and scope

Port two remaining sections of the egui app onto the gpui shell
(`crates/app-gpui`), in this order:

1. **Clocks** to full behavioural parity with `crates/app/src/views/wclock.rs`.
2. **Notes** to parity with `crates/app/src/views/notes.rs`.

Hard constraints (from the caller):

- All data logic stays in `kandrium-core`. The gpui crate only formats and
  renders. If a core gap is found, stop and report it.
- No changes to `crates/app` or `migrations/`.
- Gates: `cargo fmt`, `cargo clippy --workspace --all-targets`,
  `cargo test --workspace`, plus the gpui crate's own tests.
- Commits on `gpui-migration`, author `openspex <zefang.sh@gmail.com>`,
  committer `GIT_COMMITTER_NAME=zefangshen
  GIT_COMMITTER_EMAIL=zefang.sh@gmail.com`, no `Co-Authored-By`, never
  `git add -A`, do not push.
- Report after Clocks (with the app relaunched), then after Notes.

## 2. Findings that drive the design

### 2.1 Core is complete

`kandrium_core::clocks` already exposes everything the UI needs:

- Reads: `list_clocks`, `list_people`, `list_all_people`,
  `list_travels`, `list_all_travels`, `placement_at`, `placement_for`.
- Writes: `add_clock`, `remove_clock`, `reorder_clocks`, `add_person`,
  `rename_person`, `remove_person`, `reorder_people`, `start_travel`,
  `end_travel`, `extend_travel`, `remove_travel`, `relocate_person`.
- Formatting/geometry helpers: `resolve_zone`, `available_zones`,
  `zone_matches`, `city_name`, `format_clock`, `format_offset`,
  `day_start_seconds`, `day_end_seconds`, `Timestamp` from `jiff`.

`kandrium_core::notes` already exposes `resolve_notes_root` (honours
`KANDRIUM_NOTES`), `ensure_root`, `scan_tree`, `read_note`, `write_note`,
`create_note`, `create_folder`, `rename_note`, `delete_note` (verify exact
rename/delete names during implementation; if missing, that is a core gap —
report, do not reimplement).

**No core changes are expected.**

### 2.2 Current gpui shell

- `crates/app-gpui/src/store.rs` has one `ClockRow` (`id`, `city`,
  `ClockText`) fed by `Store::clocks(now, use_24h)`. Read handle for queries,
  write handle (`db::bootstrap`) for mutations.
- `crates/app-gpui/src/app.rs` owns `clock_rows`, `clock_error`, `now`,
  `use_24h`; `load_clocks` formats rows; the Clocks body is only a
  `clocks_view(...)` call. Notes/Documents/Projects are placeholders.
- `crates/app-gpui/src/views/clocks.rs` is a minimal read-only grid.
- `anim.rs` already defines `LIFT`/`REFLOW`/`SNAP_BACK` and the two easing
  curves; `tasks.rs` shows the drag pattern (`on_drag` + ghost entity,
  `on_drag_move::<T>` with `DragMoveEvent { bounds, event }`, `on_drop::<T>`),
  `cx.reduce_motion()`, and `with_animation`.
- gpui itself has no `request_repaint_after`. Timed work is
  `cx.spawn_in(window, async |this, cx| { cx.background_executor().timer(d).await; ... })`
  (pattern proven in `input_probe.rs`).

### 2.3 The egui drag bugs to avoid

The egui view fixed two separate cross-row bugs; the port must keep the fixed
algorithm, not the naive one:

1. **Row bands are half-open intervals bounded by the midpoint of the
   inter-row gap** (`boundary = row_bottom + GAP / 2`), not the row's own
   centre. A pointer in the gap above a row belongs to the row whose band
   contains it.
2. **Drop geometry is computed from the static slot grid**, not from the
   reflowed preview. Reordering the visible cards during a drag must not move
   the insertion targets.

Supporting pure helpers from the egui view: `reordered`, `preview_slots`
(remove lifted, `rank = slot < from ? slot : slot - 1`,
`target = rank < hover ? rank : rank + 1`, placeholder `hover`), and
`GridLayout`.

## 3. Part A — Clocks architecture

### 3.1 `Store` additions (`crates/app-gpui/src/store.rs`)

Replace `ClockRow`/`clocks()` with a snapshot and thin write wrappers:

```rust
pub struct ClockData {
    pub clocks: Vec<Clock>,
    pub people: Vec<Person>,
    pub travels: Vec<Travel>,
}

pub fn clock_data(&self) -> StoreResult<ClockData>;      // read handle, one lock
pub fn add_clock(&self, tz_identifier: &str) -> StoreResult<()>;
pub fn remove_clock(&self, clock_id: &str) -> StoreResult<()>;
pub fn reorder_clocks(&self, ids: &[String]) -> StoreResult<()>;
pub fn add_person(&self, clock_id: &str, name: &str) -> StoreResult<()>;
pub fn rename_person(&self, person_id: &str, name: &str) -> StoreResult<()>;
pub fn remove_person(&self, person_id: &str) -> StoreResult<()>;
pub fn relocate_person(&self, person_id: &str, clock_id: &str) -> StoreResult<()>;
pub fn start_travel(&self, person_id: &str, destination: &str,
                    starts_at: i64, ends_at: Option<i64>) -> StoreResult<()>;
pub fn end_travel(&self, travel_id: &str, ends_at: i64) -> StoreResult<()>;
pub fn extend_travel(&self, travel_id: &str, ends_at: i64) -> StoreResult<()>;
pub fn remove_travel(&self, travel_id: &str) -> StoreResult<()>;
```

All mutations map `KandriumError` to `String` with `e.to_string()` so the
readable `ON DELETE RESTRICT` message reaches the dialog unchanged.

### 3.2 `views/clocks.rs` — state and pure logic

`AppShell` owns one `clocks::ClockState`; it caches rows so a tick never
touches the DB.

```rust
pub struct ClockEntry {
    pub id: String,
    pub tz_identifier: String,
    pub city: String,
    pub zone: Option<jiff::tz::TimeZone>,
    pub text: ClockText,
    pub people: Vec<Person>,
}

pub struct ClockState {
    entries: Vec<ClockEntry>,
    travels: Vec<Travel>,
    now: Timestamp,
    search: String,
    zones: Vec<String>,                 // available_zones(), lazily filled
    picker: Option<PickerState>,
    inspector: Option<InspectorState>,
    form: Option<FormState>,
    confirm: Option<ConfirmState>,
    drag: Option<DragState>,            // clock_id + source index
    hover: Option<usize>,               // static insertion index
    error: Option<String>,
}
```

Pure helpers (unit-tested, copied in shape from `wclock.rs`):

- `const CARD_H: f32 = 156.0; const GAP: f32 = 12.0; const COLUMNS: usize = 3;`
  (minimum column width 160, matching egui).
- `StaticGrid { origin, width, count }` with
  `col_w()`, `rows()`, `slot_rect(slot)`, and
  `slot_at(point) -> Option<usize>`:
  - row from half-open bands `[r*(H+GAP) - G/2, r*(H+GAP) + H + G/2)`,
    first band clamped to `0`, last to the grid bottom;
  - column from `((x - origin.x) / (col_w + GAP)).floor()`, clamped `0..=2`;
  - index `(row * 3 + col).min(count - 1)`.
- `reordered(ids, from, to) -> Vec<String>` (remove + insert, complete
  permutation).
- `preview_slots(count, from, hover) -> (Vec<usize>, usize)` as above.
- `initials(name)`, `matches_search(query, zone, residents)`.
- `city_people(clocks, people, travels, now, clock_id) -> CityPeople`
  (residents with `placement_for`, visitors from other cities).
- `until_next_minute(now)`, `until_next_travel(travels, now_s) -> Option<Duration>`.

The times shown are recomputed from `now` on every render via
`format_clock(now, &zone, use_24h)`. No SQL in render, no `Timestamp::now()`
inside the view.

### 3.3 Rendering and drag

Toolbar (always): search field, city count, 12h/24h segmented toggle,
"Add city" button. Search filters cards with `matches_search` (zone id or
resident name, case-insensitive). Drag is only armed while the search is
empty; a filtered grid does not reorder.

Grid: `div().relative()` with `on_drag_move::<ClockDrag>` and
`on_drop::<ClockDrag>` on the container itself. Inside, `ceil(n/3)` rows of
three `flex_1` slots at height `CARD_H`, gap `GAP`. Slot width therefore
equals `StaticGrid::col_w()` derived from the same container bounds, which is
what the insertion geometry uses.

Drag:

- `.on_drag(ClockDrag { clock_id, city, width }, ...)` on each card; the ghost
  is a mini card at ~276 px, opacity 0.92, `with_animation(LIFT)` for the
  lift, skipped when `cx.reduce_motion()`.
- The container's `on_drag_move` computes
  `StaticGrid::slot_at(event.event.position - event.bounds.origin)` and stores
  `hover`; ignores events while no drag is active. This is the static-slot
  geometry from §2.3(2).
- Rendering while `hover` is `Some`: `preview_slots` produces the target slot
  per visible card; the placeholder slot renders as a bordered accent-tinted
  well, the lifted card is dimmed. Reflow is instantaneous (no FLIP), which
  is the documented reduction from egui; it is identical under reduced motion.
- `on_drop` commits `reordered(ids, from, hover)`, calls
  `store.reorder_clocks`, reloads, and clears the drag. A release outside the
  grid clears state without writing.

Reduced motion: `cx.reduce_motion()` disables the lift/interpolations; the
instant reflow has no motion to disable. Snap-back flight is not implemented
(the drag simply ends), stated as dropped parity in the report.

### 3.4 Zone picker

Modal overlay (same scrim/frame pattern as the card modal):

- Opens from the toolbar and from the empty state.
- Filter by city, region or zone id using `zone_matches`; cap the rendered
  list (200) for responsiveness.
- Already-added zones render disabled with an "Added" tag and are skipped by
  arrow navigation.
- Keyboard: `↑`/`↓` move the selection, `Enter` chooses, `Esc` closes.
  Implemented with a capture-phase key listener on the panel (as in
  `card_modal`) so the `InputState` does not swallow Enter.
- Mouse: click a row to add; "Clear search" resets the query; "Cancel"
  closes.
- Adding calls `store.add_clock`, reloads, keeps the picker open with the new
  zone marked "Added" (matches egui).

### 3.5 People inspector

Per-city modal opened from the card's people row:

- Header: city, zone, resident count.
- Residents: initials chip (`initials`, first two words, uppercased), name,
  status line — `Here now`, `Away in <city> · until <time>`, or
  `Leaving for <city> · starts <time>`.
- Add: single-line input + Add button → `store.add_person`.
- Rename: inline input (Enter commits, Esc cancels) →
  `store.rename_person`.
- Remove: confirm dialog → `store.remove_person`.
- Travel: opens the travel form (below).
- Visitors section: `VISITING` strip is drawn on the city card; visitors are
  listed here with "Send home" → `store.end_travel(now)`.
- Resident search on the toolbar matches resident names via
  `matches_search`, so searching a person surfaces their city.

### 3.6 Travel

`FormState` mirrors the egui draft:

```rust
enum FormKind { Assign, Extend, Relocate }
```

- **Assign**: person, destination city, start date, end date, `Open-ended`
  checkbox. Starts are whole days in the **destination** zone
  (`day_start_seconds(date, zone)`); ends are the end of the end date
  (`day_end_seconds`). Calls `store.start_travel`.
- **End early**: "End now" on a running trip → `end_travel(now)`.
- **Extend**: date picker seeded with the current end → `extend_travel`.
- **Send home**: ends the visitor's trip now (same call as End early).
- **Remove**: deletes the trip row → `remove_travel`.
- **Relocate**: changes a person's home; core cancels trips to both the old
  and the new home. Calls `store.relocate_person`.
- Card decoration: `VISITING` strip with the visitor count and nearest
  arrival/expiry; `AWAY` ring (accent border) when a resident is away;
  planned trips render with a hollow dot and "Starts <date>".

Errors from core are shown inside the open form/confirm, never discarded.

### 3.7 Remove city and `ON DELETE RESTRICT`

Standard confirmation dialog. On confirm, `store.remove_clock` runs; if core
rejects because residents exist, the dialog stays open and shows the exact
`KandriumError` text. On success the grid reloads.

### 3.8 Minute tick

- Exactly one `Timestamp::now()` per frame, in `AppShell::render` while
  `section == Section::Clocks`, written into `ClockState::now`.
- Repaint scheduling uses a generation-guarded `cx.spawn_in` loop:
  `wait = min(until_next_minute(now), until_next_travel(travels, now))`
  (+50 ms guard, 0.05 s floor). Only started while Clocks is active; leaving
  the section bumps the generation so the loop exits, and re-entry starts a
  new one. Mutations bump the generation to retarget immediately.
- The tick only rewrites `now` and calls `cx.notify()`; it never queries the
  store.

## 4. Part B — Notes architecture

### 4.1 `Store` additions

Thin wrappers over `kandrium_core::notes`, all path-guarded by core:
`notes_root()` (`ensure_root`), `notes_tree()`, `read_note(path)`,
`write_note(path, text)`, `create_note(dir)`, `create_folder(dir)`,
`rename_entry(path, new_name)`, `delete_entry(path)`. Root comes from
`resolve_notes_root`, so `KANDRIUM_NOTES` is honoured with no extra code.

### 4.2 `views/notes.rs`

```rust
pub struct OpenNote {
    path: PathBuf,
    name: String,
    text: Entity<TextareaState>,
    vim: VimSession,
    preview: bool,
    dirty: bool,
    last_edit: Option<Instant>,
}

pub struct NotesState {
    root: PathBuf,
    tree: Vec<FsEntry>,
    expanded: HashSet<PathBuf>,
    open: Vec<OpenNote>,
    active: Option<usize>,
    tree_open: bool,
    selected: Option<PathBuf>,
    renaming: Option<(PathBuf, Entity<InputState>)>,
    creating: Option<CreateKind>,
    error: Option<String>,
}
```

- **Tree**: recursive rows from `scan_tree`; directories toggle
  `expanded`; files open a tab. Header buttons act on the selected row:
  New note, New folder, Rename (inline input), Delete (confirm). All calls go
  through `Store`, then reload the tree.
- **Editor**: tab strip of open notes; one `TextareaState` per note; preview
  toggle (`markdown_path_text`, the Stage 3 renderer); vim layer reused
  verbatim (`VimSession::sync_readonly`, capture key listener, `:w` flushes,
  `:q` closes, `Esc` returns to Normal). `readonly(!vim.is_insert())` is set
  on the `Textarea` element, not only on the state (Stage 3 lesson).
- **Autosave**: 400 ms debounce. Editing sets `dirty` + `last_edit`; a
  generation-guarded spawn loop wakes 400 ms after the last edit and calls
  `store.write_note`; switching tabs/sections and `:w`/`:q` flush
  synchronously.
- **Collapse to rail**: `tree_open` toggle with a 30 px rail and expand
  button; persisted only in memory for this stage (no `SavedState` port),
  matching the egui geometry but not its persistence. If gpui's layout makes
  the rail awkward, drop the rail and state it in the report.

## 5. Files touched

| File | Change |
| --- | --- |
| `crates/app-gpui/src/store.rs` | Remove `ClockRow`/`clocks`; add `ClockData` + clock/person/travel writes; add notes wrappers. |
| `crates/app-gpui/src/views/clocks.rs` | Replace with state, pure helpers, toolbar, grid, card, drag, empty state, unit tests. |
| `crates/app-gpui/src/views/clocks_panels.rs` | New: zone picker, inspector, travel form, confirm dialog. |
| `crates/app-gpui/src/views/notes.rs` | New: tree, tabs, editor, preview, vim wiring, autosave. |
| `crates/app-gpui/src/views/mod.rs` | Register `clocks_panels`, `notes`. |
| `crates/app-gpui/src/app.rs` | Replace clock fields with `clocks::ClockState`; add `notes::NotesState`; actions for every mutation; tick loop; wire sections. |
| `crates/app-gpui/src/assets.rs` | Add the icons used (ellipsis, pencil, plane, plus, search, trash-2, users, x, chevron-left/right, folder-plus, file-plus, save, eye, house). |
| `crates/app-gpui/tests/clocks.rs` | New headless tests (store wrappers + UI helpers via a temp DB). |
| `crates/app-gpui/tests/notes.rs` | New headless tests (tree CRUD + autosave flush). |
| `docs/specs/stage-4-clocks-notes/*` | This spec. |

No changes to `crates/app`, `crates/core`, or `migrations/` are planned.

## 6. Verification

1. `cargo fmt --all -- --check` (then `cargo fmt` before committing).
2. `cargo clippy --workspace --all-targets` — warning-free (except the
   pre-existing `vim.rs:2436` warning in untouched test code).
3. `cargo test --workspace`.
4. `cargo test --manifest-path crates/app-gpui/Cargo.toml` and
   `cargo clippy --manifest-path crates/app-gpui/Cargo.toml --all-targets`.
5. Rebuild the gpui dev app and relaunch:
   `cargo build --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui`
   then `killall kandrium-gpui; open -n crates/app-gpui/target/KandriumGpui-dev.app`.

### Cross-row drag evidence

Unit tests over `StaticGrid` with 5 cities:

- pointer at the centre of every static slot maps back to that slot;
- a pointer 3 px below a row's bottom edge (inside the gap) maps to that row,
  and 9 px below maps to the next row — proving the half-gap band;
- dragging slot 1 to slot 3 yields the permutation `[0,2,3,1,4]`;
- `preview_slots` placeholder equals the hovered static index even while the
  preview order differs.

## 7. Commit plan (incremental, guards against another cut-off)

1. `store: clock snapshot and write wrappers` — `store.rs` only.
2. `clocks: grid, drag, and toolbar` — `views/clocks.rs`, `assets.rs`,
   `views/mod.rs`, minimal `app.rs` wiring; app compiles and renders read-only
   parity plus picker/reorder if complete.
3. `clocks: people and travel panels` — `views/clocks_panels.rs`, remaining
   `app.rs` actions, tick.
4. `clocks: tests` — `tests/clocks.rs`.
5. `notes: tree, editor, autosave` — `views/notes.rs`, store wrappers,
   `app.rs`.
6. `notes: tests` — `tests/notes.rs`.

Report after commit 4 (Clocks, app relaunched) and after commit 6 (Notes).

## 8. Explicitly dropped / simplified (to be stated in the report)

- FLIP reflow and ghost snap-back flight are replaced by instantaneous
  reordering and a lift-only ghost. Reduced-motion semantics are preserved.
- Tree collapse state is not persisted across launches (this stage).
- Documents and Projects remain placeholders (out of scope).
