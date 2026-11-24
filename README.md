# Kandrium

Kandrium is a local-first Kanban desktop app with a companion [Model Context Protocol](https://modelcontextprotocol.io/) server. Boards, columns, and cards live in a single SQLite database that the gpui desktop app and any MCP client (an AI assistant, scripts) can read and write at the same time.

- **Native desktop UI** built with [gpui](https://gpui.rs) — no Electron, no web runtime
- **MCP server over stdio** exposing 40 tools for board, card, project, clocks, wiki and history operations
- **Multi-process sync** — changes made through MCP appear in the running app automatically
- **Virtual "All" board** that aggregates every card into To Do / In Progress / Done with its own ordering
- **Wiki and Projects** alongside the board — Markdown notes and PDF/Markdown documents in one Wiki section, plus project workspaces with their own board and files
- **Clocks** — a world-clock page with saved cities, residents, visitors, and temporary travel/relocation
- **Network** — a people address book; placing a contact on a saved city card also shows them on Clocks
- **Local SQLite storage** (WAL mode), no account and no network

## Features

- Multiple boards whose columns fold into the three status lanes
- Cards with title, description, due date, and color labels
- Drag-and-drop cards with animated reflow and settle flights
- Card editor modal, archive/restore, and per-column "archive all"
- Archived view as a filterable data table
- Wiki: Markdown notes and PDF/Markdown documents side by side, with debounced autosave, vim-modal editing, and PDF annotation sidecars
- Projects: a folder/project tree, one board per project, short unique identifiers, a tabbed Overview/Tasks detail pane, and per-project documents
- Clocks: searchable city list, 12/24-hour toggle, three cities per page, residents and visitors, and per-person travel windows that expire on their own
- Network: searchable people list, profile details, status and expertise filters, and placing a contact on a saved city card (hide/show without losing the retained slot)
- Dark appearance with a single blue accent
- One-click MCP access: create, move, update, archive, and query cards from an AI assistant

## Project layout

```
crates/
  core/       Shared data layer: SQLite schema, services, seed data,
              legacy-DB import, and the `kandrium-import` binary
  app/        gpui desktop application (`kandrium-app`)
  mcp/        stdio MCP server (`kandrium-mcp`) built on rmcp
  pdf-render/ PDF rasterization worker shared by the app
```

The app and the MCP server are thin front-ends over `kandrium-core`. All
service functions are synchronous and take a `&rusqlite::Connection`; the
database runs in WAL mode with a busy timeout so both processes can share one
file safely.

## Prerequisites

- Rust (edition 2024; tested with rustc 1.99)
- macOS for the preconfigured `.app`/`.dmg` packaging (minimum macOS 10.15). The app itself uses only cross-platform crates.

SQLite is bundled statically via the `rusqlite/bundled` feature — no system
SQLite dependency is needed.

## Build and run

```sh
# Run the desktop app
cargo run -p kandrium-app
# (equivalent)
just run

# Run the MCP server (stdio; meant to be launched by an MCP client)
cargo run -p kandrium-mcp
```

Release build:

```sh
cargo build --release -p kandrium-app
```

## Test and lint

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all
```

## MCP server configuration

`kandrium-mcp` speaks MCP over stdio. Point an MCP client at the binary, for
example:

```json
{
  "mcpServers": {
    "kandrium": {
      "command": "/absolute/path/to/target/debug/kandrium-mcp"
    }
  }
}
```

The server exposes the following tools:

| Area | Tools |
| --- | --- |
| Boards | `list_boards`, `get_board`, `get_all_board` |
| Columns | `archive_all_cards` |
| Cards | `create_card`, `update_card`, `move_card`, `move_all_card`, `move_card_to_board`, `move_card_to_status`, `move_area_card`, `set_card_project`, `set_card_area`, `delete_card`, `archive_card`, `restore_card`, `list_archived_cards` |
| Wiki | `get_wiki_instructions`, `get_wiki_status`, `list_wiki_sources`, `read_wiki_source`, `list_wiki_pages`, `read_wiki_page`, `search_wiki`, `write_wiki_page`, `patch_wiki_page`, `delete_wiki_page`, `record_wiki_run`, `list_wiki_dedup_candidates`, `lint_wiki` |
| Projects (read) | `list_projects`, `get_project` |
| Clocks (read) | `list_clocks`, `list_clock_people`, `list_travels` |
| History | `begin_wiki_run`, `heartbeat_wiki_run`, `end_wiki_run`, `list_runs`, `undo_run` |

Tasks, Projects (read-only), Clocks (read-only), Wiki and History have an MCP
surface. `list_projects` hides archived projects unless `includeArchived` is
true. Column editing is not available
over MCP or in the app — a board renders whatever columns its rows already
have.

`begin_wiki_run` … `end_wiki_run` groups a batch of writes into one undoable
task; without it, each mutating call is its own History entry, and
`undo_run` restores one entry's files and rows.

Knowledge page lifecycle (creating, rewriting, patching and deleting compiled
pages) is MCP-only today: the app's Knowledge editor saves Markdown directly
and does not enforce the reviewed flag, `expectedHash` or the instructions
version. Document and PDF management is app-only by design. Closing the
Knowledge gap is a follow-up, not part of this MCP surface.

Tool results are JSON objects with camelCase field names (e.g.
`boardId`, `dueDate`). All logging goes to stderr so stdout stays reserved for
the MCP transport.

## Data storage

| Path | Resolution |
| --- | --- |
| Database | `KANDRIUM_DB` env var, else `<data_dir>/kandrium/kandrium.db` |
| Backups | `<database parent>/backups/` |
| History | `<database parent>/history/` (manifests, file pre-images, snapshots, diffs) |
| Logs | `KANDRIUM_LOG_DIR` env var, else `<data_dir>/kandrium/logs/` |
| Notes root | `KANDRIUM_NOTES` env var, else `<data_dir>/kandrium/notes/` |
| Documents root | `KANDRIUM_DOCS` env var, else `<data_dir>/kandrium/documents/` |
| Projects root | `KANDRIUM_PROJECTS` env var, else `<data_dir>/kandrium/projects/` |
| App settings | `gpui-settings.conf` next to the database |
| Legacy Electron DB (importer source) | `KANDRIUM_OLD_DB` env var, else `<data_dir>/planly/planly.db` |

Clocks are stored in SQLite (schema v8–v10: `clocks`, `clock_people`,
`clock_travels`) and have no environment variable of their own. Notes,
documents, and project files do not live in the database.

On macOS `<data_dir>` is `~/Library/Application Support`, so the default
database is `~/Library/Application Support/kandrium/kandrium.db`.

On first boot with no `gpui-settings.conf`, the app imports `vim_mode`,
`section`, the tree-open flag, `projects_tree_open` and `clocks_use_24h` from
the old egui store at `<data_dir>/Kandrium/app.ron` and never writes that file
again. The database is the same file both builds used, so boards, cards,
clocks and documents carry over. The last open board, project selection and
window theme are not migrated and reset once.

On first boot, if no Kandrium database exists but a pre-rebrand
`<data_dir>/planly-rs/planly.db` is found, it is copied into place
automatically using a staged `VACUUM INTO` (the original file is never
modified). Both the app and the MCP server perform this check at startup.

### Data safety

Every app and MCP boot runs `PRAGMA quick_check` before touching the schema.
If the check fails, the raw database file (plus its `-wal`/`-shm` sidecars)
is copied to the backup directory as `<stem>-corrupt-<stamp>.db`, and the app
shows a dialog pointing at the copy and the log directory instead of starting
on a broken file.

Snapshots are written with `VACUUM INTO`, so they are a single consistent
file even while WAL is active:

- **Pre-migration** — one snapshot before any pending schema migration,
  named `<stem>-pre-migration-v<old-version>-<YYYYMMDD-HHMMSS>.db`. The 5
  newest are kept.
- **Daily** — at most one per local calendar day, named
  `<stem>-daily-<YYYYMMDD-HHMMSS>.db`. The 7 newest are kept.

Snapshots are ordinary SQLite files, so restoring is a file copy:

```sh
# 1. Quit the app and any MCP server using the database.
# 2. Move the broken/live database aside (keep it until you have verified).
mv ~/Library/Application\ Support/kandrium/kandrium.db{,.broken}
# 3. Restore the snapshot, then relaunch Kandrium.
cp ~/Library/Application\ Support/kandrium/backups/<snapshot>.db \
   ~/Library/Application\ Support/kandrium/kandrium.db
```

Notes, project documents, and Markdown files are written atomically (unique
temp file + fsync + rename), so an interrupted autosave never truncates the
previous contents.

### Logs and crash reports

Both binaries log to `<data_dir>/kandrium/logs/<component>.log` (app and MCP
keep separate files, rotated to `.log.1` at 5 MB) and still mirror every line
to stderr for CLI runs. Uncaught panics are logged with a backtrace and leave
a one-off `crash-<pid>-<YYYYMMDD-HHMMSS>.log` in the same directory — this is
the place to look when a packaged `.app` misbehaves, since macOS discards its
stderr.

### Importing from the legacy Electron app

For the older Electron/Planly database, use the standalone importer:

```sh
cargo run -p kandrium-core --bin kandrium-import -- [OLD_DB] [NEW_DB]
```

It copies all boards, columns, cards, and All-board ordering rows while
preserving ids and positions, and refuses to run if the target already
contains data.

## Packaging (macOS)

The repository is configured for [cargo-packager](https://github.com/crabnebula-dev/cargo-packager)
(see [packager.json](packager.json)):

```sh
just bundle
# (equivalent)
cargo packager --config packager.json
```

This builds the release binary and produces `Kandrium.app` plus
`Kandrium_0.1.0_<arch>.dmg` in the repository root (bundle identifier
`app.kandrium.desktop`).

## License

Apache-2.0. See [LICENSE](LICENSE). Bundled Inter font files and Lucide icons
ship under their own licenses, included alongside the assets in
`crates/app/assets/`.
