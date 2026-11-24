# AGENTS.md

Guidance for AI coding agents working in this repository. Read before making
changes. User-facing overview lives in [README.md](README.md).

## What this is

Kandrium is a local-first Kanban app on a pure-Rust stack:

- **gpui 0.9** desktop UI (`crates/app`, binary `kandrium-app`)
- **rmcp 3.5** stdio MCP server (`crates/mcp`, binary `kandrium-mcp`)
- **rusqlite 0.40** (bundled SQLite) shared data layer (`crates/core`, crate `kandrium-core`)
- **pdf-render** PDF rasterization worker (`crates/pdf-render`)
- **cargo-packager** for macOS `.app`/`.dmg` bundling

Workspace, edition 2024. All comments, identifiers, and docs are in English.

## Architecture rules

- `kandrium-core` is the single source of truth for schema, models, and data
  operations. All service functions are synchronous and take
  `&rusqlite::Connection`. The app and the MCP server must stay consistent:
  do not implement data logic in a front-end crate — add it to `core` and
  expose it from `lib.rs`.
- The DB is SQLite WAL + `foreign_keys=ON` + 5 s busy timeout, so the app and
  MCP server processes share one file. The app detects external commits with a
  background watcher (`crates/core/src/db/watcher.rs`).
- Database work runs on the shell's worker path and reaches the UI through
  gpui entities/tasks; never mutate UI state from another thread. The shell
  re-reads the affected section after a watcher generation bump.
- The MCP wire format follows the legacy TypeScript conventions: camelCase
  JSON (`#[serde(rename_all = "camelCase")]`), pretty-printed text results,
  tool-level `isError` on failure. The server exposes 40 tools: the 17-tool
  Tasks subset, two Projects reads, three Clocks reads, and
  an 18-tool wiki/history surface.
  Projects are read-only over MCP (see
  `.trae/specs/mcp-v2/plan.md`). Column create/rename/delete/move are not
  exposed and have no app caller either; `archive_all_cards` is the only
  column-level mutation. Preserve the names
  and schemas of the exposed tools — MCP clients may depend on them.
- The SQL schema lives as numbered files at the workspace root
  (`migrations/0001_initial_schema.sql` … `migrations/0014_person_identity.sql`)
  and is embedded into `crates/core/src/db/migrations.rs` at compile time.
  `db::run_schema` (crate-private) applies them through `rusqlite_migration` via
  `PRAGMA user_version`. v1 (`0001_initial_schema.sql`) is identical in shape
  to the legacy Drizzle schema so the row-copy importer works. Keep both
  compatible; new schema steps must be idempotent `CREATE TABLE IF NOT EXISTS`
  / additive migrations. Additive steps go in a new numbered file under
  `migrations/` and are registered in `db::migrations` in order so earlier
  files stay byte-identical. v3 adds project identifiers and card ↔ project
  columns atomically (DDL + backfill + version bump in one transaction); v4
  lowercases existing identifiers (data-only, same NOCASE unique index); v5
  adds `area_card_order`, the unified display order for non-project boards; v6
  adds the reversible `projects.archived` / `archived_at` flag; v7 adds the
  free-form `projects.source` reference (local path or URL) shown on the
  Overview; v8–v10 add the Clocks data (`clocks`, then `clock_people`, then
  `clock_travels`); v11 adds `project_paths`, the folder references attached to
  a project; v12–v13 add the wiki index and run history; v14 adds the canonical
  `persons` / `person_expertise` and the Clocks placement/travel tables
  (`clock_placements`, `clock_travels_v2`). v14 backfills every
  `clock_people` row into a person + placement with the same id and copies
  travels, then `clock_people` / `clock_travels` are frozen and never read or
  written; a later migration may drop them. The `db` module is split into
  `db/connection.rs` (open/path/legacy
  migration/bootstrap), `db/migrations.rs` (schema + runner), and
  `db/backup.rs` (snapshots + integrity). `db::bootstrap(&mut Connection,
  &Path)` is the single boot path for both front-ends: it runs `db::prepare`
  and then seeds. `db::prepare` is the integrity check + snapshot + schema
  without the seed, and the legacy importer calls it directly — it runs
  `backup::check_integrity` (quarantine raw DB + `-wal`/`-shm` as
  `<stem>-corrupt-<stamp>.db` on failure), snapshots before a pending migration
  (`VACUUM INTO`, newest 5 kept) or once per local day (newest 7 kept), then
  `run_schema`. Snapshots live in `<db parent>/backups/`; every new boot path
  must call `bootstrap` (or `prepare` when it must not seed) with the real
  path, never `run_schema` directly.
- Projects (`crates/core/src/projects.rs`) own a board each. Those boards are
  hidden from the Tasks tabs (`list_boards`) with
  `NOT IN (SELECT board_id FROM projects)`; the Projects view re-adds the
  open project's board. The Projects view is
  `crates/app/src/views/projects.rs`. Projects reorder by drag among
  their own siblings (`projects::reorder_project` rewrites
  `projects.position` for that parent only); folders keep their own order and
  render above projects, and drag is disabled while the filter box has text.
- Every project has a short identifier (1–10 lowercase chars, `a–z 0–9 - _`,
  unique case-insensitively). It is derived from the name on create and
  regenerated by renames until the user edits it on the Overview page
  (`identifier_locked`). The tree labels projects with the identifier (full
  name in the tooltip) and the filter matches both.
- Cards carry a nullable `project_id`. Cards created on a project board inherit
  that project; cards on normal boards can be tagged with one (Project picker
  in the card modal). Project cards use the modal's "Show on Tasks boards"
  checkbox + board picker; "Move to board" is hidden for them.
  `area_board_id` files a project card onto one normal board. On such a board
  the card keeps its real project column in SQLite, but the board renders its
  own cards and the filed project cards as ONE free order: `area_card_order`
  (schema v5) stores a slot per `(board, card)`, `get_board` backfills and
  stamps `Card.display_position`, and the UI merges `Column.cards` +
  `Column.mirrored` via `Column::ordered_cards`. Mirrors are fully draggable
  and act as drop anchors. Drops on a Tasks board go through
  `move_area_card`: a mirrored card is never moved into the area column (a
  status change moves it to the matching column of its project board), while
  `Column.cards` / `Column.mirrored` keep their legacy shape for MCP clients.
  Use `set_card_project` / `set_card_area` for these links. `get_board`
  resolves `project_identifier` and the per-board `meta_label`: a project
  board shows the filed area's title, a normal/area board shows the project
  identifier. `meta_label` is UI-only (serde-skipped).
- The project detail pane is tabbed: pinned Overview and Tasks tabs plus one
  closable tab per open Markdown/text document. Markdown documents are
  read/written through `projects::read_doc` / `write_doc` (root-guarded,
  `<projects_root>/<id>/`).
- Markdown editing is vim-modal by default (`crates/app/src/vim.rs`),
  used by Wiki notes, document tabs, project docs, and the card description.
  The vim engine owns motions, operators, text objects, ex commands (`:w`,
  `:q`, `:wq`, `:x`) and the undo stack; gpui supplies the editor and the key
  routing. `Esc` in Normal mode never closes a document.
- App settings live in `gpui-settings.conf` next to the database. Keys:
  `vim_mode`, `section`, `titlebar_clock`, `wiki_tree_open`,
  `wiki_tree_width`, `projects_tree_open`, `projects_tree_width`,
  `wiki_pages_collapsed`, `wiki_documents_collapsed`, `wiki_split`,
  `tasks_archive`, `clocks_use_24h`. Unknown keys are preserved. When the
  file is absent, boot imports `vim_mode`, `section`, the merged tree flag,
  `projects_tree_open` and `clocks_use_24h` from the retired egui store
  (`<data_dir>/Kandrium/app.ron`) and never writes that file.
- IDs are 24 lowercase hex chars (`db::uid()`). Seed data uses fixed string
  ids (`board_career`, `col_work_todo`, ...).
- **Logging**: `kandrium_core::logging::init(component)` +
  `install_panic_hook(component)` run first thing in every binary's `main`.
  Logs go to `<data_dir>/kandrium/logs/<component>.log` (`KANDRIUM_LOG_DIR`
  overrides), rotate to `.log.1` at 5 MB, and are mirrored to stderr so MCP
  clients and CLI runs keep seeing them. Panics also write
  `crash-<pid>-<stamp>.log` with a backtrace. Never add new `eprintln!`/
  `println!` diagnostics in the app or MCP server — use `logging::info!` /
  `warn!` / `error!`. (`println!` would corrupt the MCP stdout transport.)
- **Atomic writes**: overwrite-style file writes (notes, project docs,
  Markdown autosave, PDF annotation sidecars) go through
  `fsutil::write_atomic` (unique sibling temp + `sync_all` + rename). Never
  truncate a user file in place with `fs::write`. New-file creation is the
  same helper.
- **Run history** (`crates/core/src/history/`) makes a task undoable as one
  unit. Files are captured before mutation into a content-addressed object
  store, and app-owned rows are diffed from a `VACUUM INTO` snapshot taken
  before and after the run. The JSON manifest at
  `<db parent>/history/runs/<run-id>.json` is authoritative; `run_history`,
  `run_row_changes` and `run_undo` mirror it. One explicit run holds the
  single `state = 'open'` row (partial unique index) and is opened and closed
  by `begin_wiki_run` / `end_wiki_run`; every other mutating call — MCP
  implicit runs and the app's per-action runs — is inserted `committed` at
  close. A run without a heartbeat for 10 minutes is reclaimed, and the boot
  sweep abandons 24-hour-old runs. Retention prunes per kind (implicit 200/7d,
  app 100/3d, explicit 20/30d) under a shared 2 GB ceiling. New mutating paths
  must run inside `Store::app_run`/`files_run` or the MCP `run_mutating`
  wrapper, and new file writes must use the `*_in_run` core functions so the
  pre-image is captured.

## Naming constraints (do not regress)

Product is **Kandrium** (renamed from Planly). Crates/binaries are
`kandrium-core` / `kandrium-app` / `kandrium-mcp` / `kandrium-import`, bundle
id `app.kandrium.desktop`, window title "Kandrium". The only intentional
remaining `planly` strings are the **legacy database paths**
(`planly-rs/planly.db` for auto-migration, `planly/planly.db` for the Electron
importer) and historical reference comments.

## Commands

```sh
cargo run -p kandrium-app          # desktop app (also: just run)
cargo run -p kandrium-mcp          # MCP stdio server
cargo test --workspace             # core + mcp + app
cargo clippy --workspace --all-targets
cargo fmt --all
cargo packager --config packager.json  # Kandrium.app + DMG (also: just bundle)
```

Before finishing a change: run `cargo fmt --all`, `cargo clippy --workspace
--all-targets`, and `cargo test --workspace`. All three must pass cleanly.
After any code change to the app, rebuild and restart the dev app
(`just dev`) so the user can see the result — never leave the running app on
stale code. `just dev` copies the fresh binary into the bundle; the bundle's
executable must be a copy, not a symlink or a script (LaunchServices hangs
before `main` when the debug binary is reached through them on this OS).

### macOS sandbox note for agent shells

A GUI launched from a sandboxed agent shell can read the default DB but every
SQLite write fails with `SQLITE_READONLY` (`~/Library/Application Support` is
not writable by the sandboxed process). `dangerouslyDisableSandbox` does not
bypass this. Launch through the dev bundle wrapper instead:

```sh
open -n target/Kandrium-dev.app
```

`target/Kandrium-dev.app` is a minimal `.app` bundle whose executable is a
copy of `target/debug/kandrium-app`. LaunchServices starts it as a real bundle
app, fully outside the agent sandbox (DB writes work) and with no Terminal
window hosting it — `open -n` on the bare binary re-parents under
Terminal.app, and closing that window kills the app. Restart after a rebuild
with `just dev` (or `killall kandrium-app`, copy the binary into
`Contents/MacOS/`, then `open -n target/Kandrium-dev.app`). The executable must
be a copy: symlinks and scripts make LaunchServices hang before `main` on this
OS.

The bundle lives in the cleanable `target/`; if missing, recreate it:
`Contents/MacOS/kandrium-app` as a copy of `target/debug/kandrium-app`, a
`Contents/Resources/icon.icns` copied from
`crates/app/assets/packaging/icon.icns`, and a bare `Contents/Info.plist`
(APPL, bundle id `app.kandrium.desktop.dev`, `CFBundleExecutable`
`kandrium-app`, `CFBundleIconFile` `icon.icns`). Without the icon resource and
key, LaunchServices shows the generic application icon in the Dock.

For testing with a writable database, set `KANDRIUM_DB` to a path under the
workspace or `/tmp`. Visual verification may be impossible from the agent
shell (screen capture blocked) — ask the user to confirm UI changes.

## gpui conventions

- `gpui-kit` and `gpui-component` are pinned to `=0.9.1`. The Markdown
  highlighting grammar comes from the `tree-sitter-markdown` feature on
  `gpui-component`; do not add unrelated grammar features without a reason.
- The shell is one `AppShell` entity (`crates/app/src/app.rs`). Sections
  are Tasks, Projects, Wiki (compiled pages + sources), Network (people) and
  Clocks. Views live under
  `crates/app/src/views/`, reusable widgets under `src/views/` helpers or
  `gpui-kit`.
- Every screen keeps its data in the shell's `Store`
  (`crates/app/src/store.rs`); the store wraps core services and never
  duplicates their rules.
- Design tokens (colors, radii, typography, spacing) live in
  `crates/app/src/theme.rs` as `Tokens`. Use the token fields, not literal
  colors or sizes, and keep the single blue accent.
- Icons are inline SVGs compiled in `crates/app/src/assets.rs` from
  `crates/app/assets/icons/`. Do not add a second icon crate.
- Drag and drop is hand-rolled in `crates/app/src/anim.rs` plus each
  view's drag session; easing constants live only there.
- Use `ui.push_id`/`.id(...)` for loop-generated elements and stable test
  paths. Widgets that tests click need an `.id("...")` and `.test_support()`.
- Editors: vim state is per editor and owned by the shell; insert mode is the
  plain textarea/input. Do not route text through a second buffer.
- Settings are written only from user actions (`persist_settings`), never per
  frame, and always through `fsutil::write_atomic`.

## Headless UI tests

UI behaviour is tested without a GPU with `#[gpui::test]` and
`TestAppContext` under `crates/app/tests/`. Build a shell with
`AppShell::with_store` over a temp database, click stable element ids
(`window.click("board-card-<id>", cx)`), press keys
(`window.press("tab", cx)`) and assert through the store or rendered test
paths. Call `gpui_kit::init` first. Keep every test on its own
`tempfile::tempdir()`; never touch the user's database.

Two framework behaviours matter when writing these tests:

- The modal focus trap reserves `Tab` for focus navigation, so a focused
  editor never receives it. Do not assert egui-style Tab-to-indent behaviour.
- A card modal opens with the title focused; click
  `card-description-field` before sending description keys.

## Design system

Neutral gray-on-gray with a single blue accent, Inter typography, rounded
fields, floating scrollbars, Lucide icons, and dark-only tokens.

## Database location / env vars

- `KANDRIUM_DB` overrides the DB path; default `<data_dir>/kandrium/kandrium.db`.
- `KANDRIUM_OLD_DB` overrides the legacy Electron source for `kandrium-import`.
- `KANDRIUM_NOTES` overrides the notes root; default
  `<data_dir>/kandrium/notes/` (plain `.md` files, no DB involvement).
- `KANDRIUM_DOCS` overrides the documents root; default
  `<data_dir>/kandrium/documents/` (PDFs + Markdown; PDF annotations live in
  `*.pdf.kandrium.json` sidecars).
- `KANDRIUM_PROJECTS` overrides the projects root; default
  `<data_dir>/kandrium/projects/` (one folder of plain files per project —
  the folder/project tree itself lives in SQLite, schema v2).
- `KANDRIUM_LOG_DIR` overrides the log directory; default
  `<data_dir>/kandrium/logs/` (one rolling `<component>.log` + panic crash
  files).
- Settings live in `gpui-settings.conf` beside the database; the retired egui
  store at `<data_dir>/Kandrium/app.ron` is read once when that file is
  missing.
- Auto-migration from `<data_dir>/planly-rs/planly.db` runs once at app and MCP
  boot (staged `VACUUM INTO`, never writes the source). A `KANDRIUM_DB`
  override opts out of auto-migration.
