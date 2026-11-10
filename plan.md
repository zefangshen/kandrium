# Kandrium Plan

Roadmap as of **2026-11-05**. Version `0.1.0`, schema `v7`
(`migrations/0007_project_source.sql`). Architecture constraints live in
[AGENTS.md](AGENTS.md); this file is forward-looking work, not a restatement of
those rules.

## Where we are now

- **Stack**: eframe/egui 0.36 desktop app, rmcp stdio MCP server, rusqlite
  (bundled SQLite) shared data layer, all in one workspace
  (`crates/core`, `crates/app`, `crates/mcp`).
- **Tasks**: real boards with columns, drag-and-drop cards/columns, inline
  title editing, card modal (markdown description + due date + labels), a
  virtual **All** board with per-status ordering, and an **Archived** table
  with search, board filter, sort, restore and delete.
- **Projects**: DB-backed folder/project tree; each project owns a board, a
  document folder, an inline-editable overview (identifier, status,
  description, source) and mirrored "area" cards on normal boards.
- **Notes** and **Documents**: file-system trees with multi-tab editors/readers;
  Markdown (shared editor, preview, debounced autosave), PDFs (worker
  rendering, highlights, sticky notes in JSON sidecars).
- **Markdown editing**: vim-modal by default in Notes, project docs, Documents
  `.md` tabs and the card description field.
- **Data safety**: WAL + busy timeout, integrity check at boot, pre-migration
  and daily `VACUUM INTO` snapshots, atomic file writes, rotating logs and
  panic crash files.
- **MCP**: curated 13-tool subset (boards, `archive_all_cards`, cards).
- **Tests**: 211 `#[test]` functions across 25 files, including headless egui
  geometry tests. (README still says "~185" — see gaps.)

## Top three

1. **Board lifecycle** — create/rename/delete/reorder normal boards; today only
   the three seeded boards exist and there is no UI or MCP path to add another.
2. **MCP project tools + non-destructive card updates** — projects are
   first-class in the app but invisible to MCP clients, and `update_card`
   currently overwrites every field.
3. **Find/filter** — search across the active board plus filters for the
   Notes/Documents trees; only the Archived table has search today.

## Near-term work (ordered by value vs. risk)

### 1. Board lifecycle (create / rename / delete / reorder)

- **Why**: `services::board::create_board` exists and is used only by
  `projects.rs`, but the app has no "new board", rename or delete action. The
  Tasks tab is permanently limited to Career / Work / Life. Board deletion
  already cascades correctly (`ON DELETE CASCADE`), and project-owned boards
  are hidden by `list_boards`, so a destructive delete needs one guard.
- **Change**:
  - `crates/core/src/services/board.rs`: add `rename_board`, `delete_board`,
    `reorder_boards`; reject deleting/renaming a project-owned board.
  - `crates/core/src/lib.rs`: export the new functions.
  - `crates/app/src/app.rs` (header/tab strip): add a "+" tab affordance,
    inline tab rename on double-click, context-menu delete, tab drag reorder
    (reuse `widgets::collapse_button` / existing inline-edit patterns).
  - `crates/app/src/widgets/confirm.rs`: confirm delete with the card count.
- **Verify**: core tests in `crates/core/tests/services.rs` for rename trim
  rules, reorder stability, cascade of cards + `all_card_order` +
  `area_card_order`, and the project-board guard. Manual: create, rename,
  reorder, delete a board; relaunch and confirm ordering persisted.
- **Risk**: medium (destructive delete). Keep the confirm dialog explicit and
  never delete a project board.

### 2. MCP project tools and partial card updates

- **Why**: `projects.rs` explicitly notes "No MCP tool exposes them yet", so an
  AI client cannot list, read or create projects. `update_card` requires
  `title` and rewrites description/due date/labels, so a client changing one
  field must resend everything.
- **Change**:
  - `crates/mcp/src/main.rs`: add read-only `list_projects` / `get_project`
    (and `list_project_choices`), then `create_project` / `update_project` /
    `archive_project` once the read path is proven.
  - `update_card`: make `title` optional and apply only provided fields in
    `crates/core/src/services/card.rs`; keep the existing tool name and
    camelCase wire shape. Add a dedicated core test for "description-only
    update leaves title/due date/labels untouched".
  - Update `tool_router_exposes_the_curated_set` (it asserts an exact sorted
    list) and the README tool table.
- **Verify**: MCP end-to-end smoke with `KANDRIUM_DB` pointing at a temp file
  (`tools/list`, create project, create card in a project board, patch one
  field); `cargo test -p kandrium-mcp`.
- **Risk**: low/medium. Adding tools is additive, but the `update_card`
  semantic change needs a compatibility note because the current description
  promises "full replace".

### 3. Find/filter across Tasks, Notes and Documents

- **Why**: only the Archived table has a search box
  (`crates/app/src/views/archive.rs`, `matches_filter` matches title only).
  The board header has no quick filter, and the Notes/Documents trees have no
  filter equivalent to the Projects tree's.
- **Change**:
  - `crates/app/src/app.rs` // `views/board.rs`: a toolbar search box matching
    title, description and project identifier. Decide one behavior for
    drag-and-drop while filtering: either disable card DnD while the filter is
    non-empty (same as the Projects tree) or map filtered indices back to the
    full list. Prefer disabling first — it avoids index-mapping bugs.
  - `views/notes.rs`, `views/documents.rs`: reuse
    `theme::search_field` + the Projects tree's name filter pattern.
  - Keep filter state session-only; persist nothing.
- **Verify**: headless filter tests (pure `matches_filter` helpers), then
  manual: filter a board by title/description, verify drop targets are
  unaffected when the box is empty.
- **Risk**: low if DnD is disabled while filtering.

### 4. Archive bulk actions and CSV export

- **Why**: restoring or deleting many archived cards is one-by-one today;
  there is no export path for backups or reporting.
- **Change**: multi-select in `views/archive.rs` (checkbox column + shift
  ranges), bulk restore/delete routed through `confirm.rs`, and a CSV export
  button using an `rfd` save dialog. Add batch helpers in
  `crates/core/src/services/card.rs` only if a single transaction is needed.
- **Verify**: unit tests for selection/filter interaction; manual export of a
  filtered set and re-import into a scratch DB via the importer for sanity.
- **Risk**: low.

### 5. Remaining vim gaps most likely to be used

- **Why**: `widgets/vim.rs` documents `/` search, marks, named registers, `%`,
  `>>`/`<<`, `s` and Visual-op repeat as deliberately absent; `:q!` is
  rejected. `/` search is the most noticeable gap for editing notes/docs.
- **Change**: implement `/` and `n`/`N` in `widgets/vim.rs` (buffer search,
  highlight matches, status-strip feedback), then `>>`/`<<` if cheap. Leave
  marks/registers explicitly out of scope for now.
- **Verify**: extend the 47 existing vim tests (search wraps, no-match message,
  Esc restores Normal mode, caret lands on first match).
- **Risk**: low, isolated to one module.

### 6. CI baseline and one real MCP integration test

- **Why**: `.github` contains only issue templates; no CI runs fmt/clippy/tests.
  The MCP crate's only test checks the tool router — no test exercises a real
  stdio session against a shared DB.
- **Change**: add `.github/workflows/ci.yml` with `cargo fmt --check`,
  `cargo clippy --workspace -- -D warnings`, `cargo test --workspace`
  (Linux runner is enough for headless egui); optionally a macOS packaging
  job. Add a test that spawns `target/debug/kandrium-mcp` with `KANDRIUM_DB`
  under a temp dir, sends `initialize` + `tools/list` + `create_card`, and
  asserts the app-side `get_board` sees the card.
- **Verify**: workflow green; integration test passes locally and in CI.
- **Risk**: low. Do this first if board lifecycle is still being designed.

### 7. Backup visibility and restore flow

- **Why**: snapshots are created at boot only (pre-migration + daily) and
  restoring is a documented manual `cp` (`README.md`). Users have no in-app
  way to see what is available or recover.
- **Change**: `crates/core/src/db/backup.rs` gains a `list_snapshots` helper
  and configurable retention; the app gets a "Backups" action (header menu or
  a small panel) listing snapshots with timestamps, "Reveal in Finder", and a
  guarded restore that requires quitting/relaunching.
- **Verify**: unit tests for listing/pruning; manual: take a snapshot, mutate
  data, restore, relaunch, confirm content.
- **Risk**: medium (file replacement). Restore must refuse while the MCP
  server or app holds the DB open in the same process; require an explicit
  confirm.

### 8. PDF text selection, copy and find

- **Why**: `documents.rs` labels selection "visual only, v1"; `pdf.rs`
  extracts per-page text but does no search, so users can highlight but not
  copy or find text.
- **Change**: on drag-select, resolve the nearest text range from `PageText`
  and support Cmd-C; add a find bar that searches extracted page text and
  jumps/scrolls to matches. Reuse the worker extraction already used for
  annotations.
- **Verify**: pure tests for selection→text mapping and match offsets; manual
  copy/find on a multi-page PDF.
- **Risk**: medium (geometry mapping), but no schema or file-format change.

## Medium-term / larger efforts

- **Card model expansion** — checklists/subtasks, activity history, file
  attachments. New additive migration (`0008_*`), core services, card modal
  sections, and MCP tools. Requires a careful decision on where attachments
  live (documents root vs. per-card sidecar).
- **Explicit status model** — replace title-based status matching with a
  persisted canonical status (column or board setting). This fixes custom
  columns being dropped from the All board and area ordering (see gaps).
- **Undo journal** — a short-lived action log for archive/delete/move so
  destructive actions are reversible across app and MCP writes.
- **External file watching** — watch Notes/Documents roots (e.g. `notify`) and
  offer reload/conflict prompts; today only the SQLite DB is watched.
- **Command palette / keyboard shortcuts** — quick capture, board switching,
  search launcher, and a shortcut reference; currently actions are mouse-first.
- **Packaging and release** — universal macOS binary, code signing +
  notarization, version derived from one source, a Linux/Windows evaluation,
  and an update strategy (even if it is just a release checklist).
- **MCP breadth** — read-only notes/documents search tools, batch card
  operations, and project document access, keeping the existing tool names and
  schemas stable.

## Clocks (Kwora port)

The Kwora world-clock port is planned separately in [clocks-plan.md](clocks-plan.md).

## Known gaps, rough edges and tech debt

### Data model / board behavior

- **No board lifecycle** in core or app beyond `create_board`
  (`crates/core/src/services/board.rs` has only `list_boards`, `create_board`,
  `get_board`, `get_all_board`). `BoardActions` in
  `crates/app/src/views/board.rs` has no board-level actions.
- **Non-canonical columns are silently dropped from the All board and from
  area ordering**: `get_all_board` and the mirror merge only consider columns
  whose title matches To Do / In Progress / Done
  (`crates/core/src/services/board.rs`, the `status_from_column_title` filters
  in `get_board` and `apply_area_order`). A "Backlog" column never appears in
  the All board, and its project mirrors are skipped.
- **Cross-board moves require canonical columns**: `move_card_to_board` /
  `move_card_to_status` error when the target board lacks the matching
  To Do / In Progress / Done column
  (`crates/core/src/services/card.rs`, around the `status_from_column_title`
  lookup). The error message is good, but custom workflows cannot move cards
  across boards.
- **All-board add form target resets to Career each launch** and only one
  bucket's form is open at a time (`crates/app/src/app.rs`, `all_add_board` /
  `all_add_status`). Session-only by design, but it is a repeated paper cut.
- `EditSession.archived` is documented as "always false today"
  (`crates/app/src/app.rs`), i.e. archived cards cannot be opened in the
  modal; the field is effectively dead state.

### MCP surface

- The server exposes exactly 13 tools; project CRUD, notes, documents, board
  create/rename/delete and column create/rename/delete/move are app-only
  (`crates/mcp/src/main.rs` module doc and
  `crates/core/src/projects.rs` "No MCP tool exposes them yet").
- `update_card` requires `title` and replaces description/due date/labels
  (`crates/mcp/src/main.rs` `UpdateCardInput`; `crates/core/src/services/card.rs`
  `update_card`), so partial updates are impossible.
- Only `archive_all_cards` remains as a column mutation; this is intentional
  (legacy compatibility), but it means an AI client cannot reorganize columns.
- No `get_card` / `list_cards` / search tool; clients must fetch whole boards.

### Editor behavior

- Vim: `/` search, marks, named registers, `%`, `>>`/`<<`, `s`, and Visual
  repeat are absent; `:q!` reports "Not a command"
  (`crates/app/src/widgets/vim.rs`, module doc and `parse_ex`).
- Markdown editors keep a per-editor `Vec<char>` per frame and a private
  undo stack; very large notes are not a target (documented in `vim.rs`).
- PDF selection is visual-only and there is no text search or annotation
  export/import (`crates/app/src/views/documents.rs` `Tool::Select` comments;
  `crates/app/src/pdf.rs` "no search").
- Notes/Documents/Projects tree rows are rescanned on demand; external file
  changes are invisible until a rescan, and there is no conflict prompt.

### Persistence, sync and safety

- The external-change watcher polls `PRAGMA data_version` every 250 ms and
  triggers a full reload (`crates/app/src/watcher.rs`); changes during a drag
  are deferred (`crates/app/src/app.rs`, `pending_sync`). There is no per-field
  merge, so an external update can still overwrite an unsaved modal draft.
- Backups are boot-triggered only and retention is fixed at 5 pre-migration /
  7 daily (`crates/core/src/db/backup.rs`); restoring is manual.
- Destructive UI actions rely on a confirm dialog; there is no undo
  (`crates/app/src/widgets/confirm.rs`).

### Docs, tests and release

- README drift: it says "~185 tests" while there are 211 `#[test]` functions;
  its feature list and data-storage table predate Projects, Notes and
  Documents (no `KANDRIUM_NOTES` / `KANDRIUM_DOCS` / `KANDRIUM_PROJECTS`
  entries). AGENTS.md is the current source of truth.
- No `.github/workflows`; lint/test/migration checks are manual.
- The only MCP test is the tool-router list; no stdio integration test
  (`crates/mcp/src/main.rs` tests).
- Packaging is macOS-only (`.app` + `.dmg`, minimum 10.15) with no signing,
  notarization, universal binary or update path (`packager.json`). Version is
  duplicated in `Cargo.toml`, `packager.json` and the README.
- Test coverage is strongest in core services, migrations, vim and animation;
  thin for full app flows (drag-drop lives in headless geometry tests, not an
  end-to-end launch) and for PDF annotation persistence.

## How we verify

Before finishing any change to source:

```sh
cargo fmt
cargo clippy --workspace        # must be warning-free
cargo test --workspace          # ~211 tests, must stay green
```

Additional checks by area:

- **Schema changes**: add a new numbered `migrations/0008_*.sql`, register it
  in `crates/core/src/db/migrations.rs`, bump `SCHEMA_VERSION`, and keep the
  fresh / v1 / v2 / v3 upgrade tests passing.
- **MCP changes**: `cargo test -p kandrium-mcp` plus a stdio smoke run with
  `KANDRIUM_DB=/tmp/kandrium-test.db cargo run -p kandrium-mcp`, sending
  `initialize` and `tools/list`; verify `update_card` and project tools against
  a scratch DB.
- **App changes**: rebuild and restart through the dev bundle so the running
  instance is not stale:

  ```sh
  cargo build -p kandrium-app
  killall kandrium-app; open -n target/Kandrium-dev.app
  ```

  Visual behavior may not be capturable from the agent shell; ask the user to
  confirm UI changes.
- **Headless UI tests**: `theme::install_fonts` + `theme::apply_visuals` first,
  clear `textures_delta`, and scan shapes for duplicate-ID warnings (see
  AGENTS.md).
- **File-system work**: use `KANDRIUM_NOTES` / `KANDRIUM_DOCS` /
  `KANDRIUM_PROJECTS` with temp dirs so tests never touch real user data.
