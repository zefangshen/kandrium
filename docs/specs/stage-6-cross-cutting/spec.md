# Stage 6 — the cross-cutting layer: settings, data_version watcher, packaging

Status: implemented on `gpui-migration`. Three commits plus the spec commit;
see `tasks.md` for the checklist.

## 1. Goal

Three pieces, in this order, all on the gpui shell:

1. **Settings persistence** — the gpui shell has no settings store, so vim has
   no on/off preference there and nothing survives a restart.
2. **`data_version` watcher** — a second process (MCP server or the egui app)
   editing the database stays invisible until a section or board switch.
3. **Packaging** — a real macOS `.app` + DMG for the gpui binary, equivalent
   to what `cargo packager` produces for egui, with a repeatable command.

Constraints (unchanged):

- Data logic only in `kandrium-core`; the gpui crate formats, renders and
  persists UI preferences.
- `cargo fmt` (root and the gpui nested workspace),
  `cargo clippy --workspace --all-targets` (root and gpui),
  `cargo test --workspace` plus the gpui crate's tests.
- Leave the pre-existing `crates/app/src/widgets/vim.rs:2436` warning alone.
- Do not change `crates/app` behaviour beyond what Stage 5 already did, do not
  touch `migrations/`, the MCP surface, or the Projects placeholder.
- Commits on `gpui-migration`: author `openspex <zefang.sh@gmail.com>`,
  committer env `GIT_COMMITTER_NAME=zefangshen
  GIT_COMMITTER_EMAIL=zefang.sh@gmail.com`, no `Co-Authored-By`, no assistant
  mentions, never `git add -A`, do not push. One commit per part.

## 2. Part 1 — settings persistence

### 2.1 File and format (decision)

New file `crates/app-gpui/src/settings.rs`.

- Location: next to the database, `<db parent>/gpui-settings.conf`, matching
  the existing `gpui-documents.session` precedent. A `KANDRIUM_DB` override
  isolates it automatically.
- Format: plain `key=value` lines, one per line, UTF-8, sorted by key on
  write. Blank lines and lines starting with `#` are ignored. Parsing splits
  on the **first** `=` only.
- Forward compatibility: the backing store is a `BTreeMap<String, String>`.
  Unknown keys are kept verbatim and re-emitted on the next save. Missing or
  unparsable known keys fall back to their default (never an error).
- Atomic write through `kandrium_core::fsutil::write_atomic` (the same helper
  the session file uses).

Covered keys (all with defaults):

| key | type | default |
| --- | --- | --- |
| `vim_mode` | `1`/`0` | `1` (egui `SavedState.vim_mode`) |
| `section` | `tasks`/`clocks`/`notes`/`documents`/`projects` | `tasks` |
| `notes_tree_open` / `documents_tree_open` / `projects_tree_open` | `1`/`0` | `1` |
| `notes_tree_width` / `documents_tree_width` / `projects_tree_width` | f32 px | `240` |

Widths are clamped to 160–420 px on read and write. `projects_*` is stored for
forward compatibility only: there is no Projects view yet (placeholder), so
nothing applies it.

### 2.2 API

```rust
pub enum TreePanel { Notes, Documents, Projects }

pub struct Settings { values: BTreeMap<String, String> }

impl Settings {
    pub fn load(path: &Path) -> Self;
    pub fn save(&self, path: &Path) -> std::io::Result<()>;
    pub fn vim_mode(&self) -> bool;
    pub fn set_vim_mode(&mut self, on: bool);
    pub fn section(&self) -> Section;
    pub fn set_section(&mut self, section: Section);
    pub fn tree_width(&self, panel: TreePanel) -> f32;
    pub fn set_tree_width(&mut self, panel: TreePanel, width: f32);
    pub fn tree_open(&self, panel: TreePanel) -> bool;
    pub fn set_tree_open(&mut self, panel: TreePanel, open: bool);
}
```

`Section` gains `pub fn from_key(key: &str) -> Option<Section>` mirroring the
existing `Section::id()` values; unknown section values resolve to `Tasks`.

### 2.3 Shell wiring (`crates/app-gpui/src/app.rs`)

- New fields on `AppShell`: `settings: Settings`, `settings_path: PathBuf`,
  `vim_mode: bool`.
- `with_store` builds the path from `store.path.parent()` (fall back to the
  current directory), loads settings, seeds `vim_mode`, `section`,
  `notes.tree_open/tree_w` and `documents.tree_open/tree_w`, then loads the
  stored section's data. A test database in a temp directory has no file, so
  behaviour stays `Tasks` + defaults, and existing headless tests are
  unaffected.
- `persist_settings(&mut self)` copies live state into the map (vim mode,
  section, both tree widths and both open flags) and saves; a failure is
  logged with `logging::warn!`, never surfaced as a UI error.
- Persist points: `select()` (section), `toggle_notes_tree`,
  `toggle_documents_tree`, `toggle_vim_mode`, and the end of a tree resize.
  No per-frame writes.
- Tree widths get a real value: `NotesState` and `DocumentsState` gain
  `pub tree_w: f32` (default `TREE_W`), used instead of the constant in
  `tree_panel`. A 4 px splitter at the tree's right edge starts a
  `TreeDrag` on pointer drag, `on_drag_move` updates the live width from the
  window x coordinate (the tree is flush left; clamp 160–420), and
  `on_drop` ends and persists it. `on_mouse_up` outside the row keeps the last
  width, which the next persist point saves.

### 2.4 Vim preference end to end

- `VimSession` (`crates/app-gpui/src/vim.rs`) gains `enabled: bool`
  (default `true`; manual `Default` impl because the derived one would be
  `false`). When disabled: `handle_key` returns `KeyOutcome::default()`
  immediately, `sync_readonly` leaves the textarea editable, `mode_label`
  reports `"vim"`, `pending`/`ex_line`/`ex_error` are empty.
- `set_enabled(&mut self, on, input, cx)` resets pending/ex state and returns
  to Normal mode; it calls `sync_readonly`.
- New `readonly(&self) -> bool` = `enabled && !is_insert()`. Notes and
  Documents views use it instead of `!vim.is_insert()`.
- Every editor construction seeds the session: card modal (`open_card`),
  Notes tabs (`open_note`), Documents Markdown tabs (`open_document`).
- Key handlers (`handle_card_key`, `handle_notes_key`,
  `handle_documents_key`) short-circuit when vim is off: `Esc` closes the
  modal/tab, every other key reaches the plain textarea.
- Mode strips become the toggle, like egui's status bar badge:
  `card-modal` (`card-description-vim-toggle`), notes (`note-vim-toggle`),
  documents (`doc-vim-toggle`). The badge shows the mode label when on and
  `vim` when off; clicking calls `AppShell::toggle_vim_mode`, which flips the
  flag, persists, updates every open session so the textarea follows
  immediately, and notifies.

## 3. Part 2 — the `data_version` watcher

### 3.1 Module

New file `crates/app-gpui/src/watcher.rs`, ported from
`crates/app/src/watcher.rs`:

```rust
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);
pub const DEBOUNCE: Duration = Duration::from_millis(120);

pub struct Watcher { rx: Receiver<()>, stop: Arc<AtomicBool> }

impl Watcher {
    pub fn spawn(db_path: PathBuf) -> Self;
    pub fn drain(&self) -> bool;  // true if ≥1 event was queued
    pub fn stop(self);            // sets the flag; thread exits ≤ POLL_INTERVAL
}
```

The thread opens its own connection (busy timeout 5 s), reads
`PRAGMA data_version`, then polls while the stop flag is clear; a change is
debounced 120 ms and sent as one unit over `std::sync::mpsc`. It never
touches an entity, a window or any UI state. Only other processes bump
`data_version`, so there is no echo loop after our own writes.

### 3.2 Shell wiring

- New fields: `watcher: Option<Watcher>`, `watcher_generation: u64`,
  `pending_sync: bool`.
- Polling is scoped: a watcher exists only while the foreground section is
  database-backed (`Tasks`, `Clocks`). Notes/Documents/Projects are
  filesystem-backed and never start one. Leaving those sections stops the
  thread (stop flag) and bumps the generation so the pump loop exits.
- Wake-up: one `cx.spawn` pump per generation, ticking every
  `POLL_INTERVAL`, calls `Watcher::drain()`; on an event it calls
  `apply_external_change` and notifies. `render` starts the pump via
  `ensure_watcher` exactly as `ensure_clock_tick` does for Clocks.
- `apply_external_change(&mut self, cx)` refreshes **only the foreground
  section**: `Tasks` → `load_tasks()` (board list + held board, falling back
  to the first board if the held one vanished), `Clocks` → `load_clocks()`.
  Nothing else is reloaded.
- In-flight guards: while `dragging_card`/`drop_target` is active the change
  is deferred (`pending_sync = true`) and applied on a later frame once the
  drag ends. The card modal (`editing`), note/document tabs and their dirty
  buffers are never touched by a reload; an open modal stays open and saves
  through core as before. This is the trap from the earlier stages.
- The Clocks minute tick stays as it is: it renders from cached rows and
  never queries SQLite; the watcher does not replace or feed it.

### 3.3 Tests

- `watcher.rs` unit tests: a separate connection commits, `drain()` reports
  the event within a bounded wait; `stop()` makes the thread exit
  (no event after stop).
- `app.rs` `#[cfg(test)]` (unit, so private access is legal): with a temp
  database and `TestAppContext`, an external `update_card` followed by
  `apply_external_change` refreshes the board snapshot; with a drag recorded
  the call defers and `pending_sync` is set; an open `editing` session is
  still present afterwards.

## 4. Part 3 — packaging

### 4.1 Artifact

New `crates/app-gpui/packager.json` (the root config is untouched):

```json
{
  "productName": "Kandrium",
  "name": "kandrium-gpui",
  "identifier": "app.kandrium.desktop",
  "version": "0.1.0",
  "description": "Kandrium Kanban desktop app (gpui)",
  "category": "Productivity",
  "beforePackagingCommand": "cargo build --release --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui",
  "binaries": [{ "path": "crates/app-gpui/target/release/kandrium-gpui", "main": true }],
  "icons": [<the same six PNGs from crates/app/packaging>],
  "formats": ["app", "dmg"],
  "macos": { "minimumSystemVersion": "10.15" }
}
```

- Bundle id `app.kandrium.desktop`, version `0.1.0`, product/app name
  `Kandrium` (the window title is already `Kandrium` in `lib.rs`), same icon
  set as egui.
- Repeatable command, added to the `Justfile` (existing `run` untouched):

  ```sh
  just gpui-bundle
  # = cargo packager --release --manifest-path crates/app-gpui/Cargo.toml
  ```

  Output lands under `crates/app-gpui/target/release/bundle/`. The command is
  verified by running it once and inspecting the `.app` (`defaults read`
  bundle id/version, `Contents/MacOS/kandrium-gpui` present, DMG produced).

### 4.2 Duplication

Two thin configs (root for egui, crate-local for gpui) are deliberate: one
`packager.json` cannot name two different binaries/products. What is real
duplication is the icon list and the version string; the honest share is a
root-level `packaging/` icon set plus a single version source, applied when
the egui app is retired. Not restructured now (explicitly out of scope).

### 4.3 README

`crates/app-gpui/README.md` gains a "Package a macOS bundle" section with the
command, the output paths, and the note that the dev bundle recipe remains a
developer convenience only. The stale "does not write to the database" claim
(Stage 2+) is corrected while the file is edited.

## 5. Verification

1. `cargo fmt` at the root and `cargo fmt --manifest-path
   crates/app-gpui/Cargo.toml`.
2. `cargo clippy --workspace --all-targets` and
   `cargo clippy --manifest-path crates/app-gpui/Cargo.toml --all-targets`;
   both warning-free (`vim.rs:2436` excluded from the count).
3. `cargo test --workspace` (baseline 279) and
   `cargo test --manifest-path crates/app-gpui/Cargo.toml` (baseline 74).
   Note: `tests/clocks.rs` has two wall-clock-sensitive tests
   (`a_trip_can_be_assigned_extended_ended_and_removed`,
   `relocation_cancels_trips_to_both_homes`) that fail after 23:00 local time
   because a Tokyo "day window" starts/ends an hour before local midnight.
   They are untouched by this stage and reproduced identically on `88a030c`
   (Stage 5 tip) in a throwaway worktree.
4. `just gpui-bundle`; inspect the `.app` and DMG.
5. Rebuild and relaunch the dev app:
   `killall kandrium-gpui; open -n crates/app-gpui/target/KandriumGpui-dev.app`
   (unsandboxed), then hand the on-screen checks to the user.

## 6. On-screen checks for the user

1. Switch to Documents/Notes, quit and relaunch: the same section is open,
   tree collapsed state and width survive.
2. Drag the Notes tree edge: the width follows and survives a relaunch.
3. Click the vim badge in Notes, Documents and the card description: the
   badge reads `vim`, typing needs no `i`, and after a relaunch vim is still
   off; clicking again restores Normal mode.
4. With the app on Tasks, run an external edit (MCP tool or a second
   `kandrium-gpui`/`sqlite3` write): the board updates within about a second
   without switching sections. Do the same mid card-drag: no jump, and the
   change lands at the drop. Repeat on Clocks.
5. Open a card modal, then trigger an external edit: the modal stays open
   with its typed text.
6. Open the built `Kandrium.app`: correct Dock icon, window title
   `Kandrium`, and `defaults read …/Info.plist CFBundleIdentifier` equals
   `app.kandrium.desktop`.

## 7. Commit plan

1. `Persist gpui shell settings and wire the vim preference` — `settings.rs`,
   `app.rs`, `vim.rs`, notes/documents views, card modal, tests.
2. `Watch data_version from gpui and refresh the foreground section` —
   `watcher.rs`, `app.rs`, tests.
3. `Package the gpui app as a macOS bundle and DMG` — `packager.json`,
   `Justfile`, `README.md`.
