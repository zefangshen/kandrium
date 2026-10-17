# Kandrium

Kandrium is a local-first Kanban desktop app with a companion [Model Context Protocol](https://modelcontextprotocol.io/) server. Boards, columns, and cards live in a single SQLite database that the egui desktop app and any MCP client (an AI assistant, scripts) can read and write at the same time.

- **Native desktop UI** built with [eframe/egui](https://github.com/emilk/egui) — no Electron, no web runtime
- **MCP server over stdio** exposing 17 tools for board/column/card operations
- **Multi-process sync** — changes made through MCP appear in the running app automatically
- **Virtual "All" board** that aggregates every card into To Do / In Progress / Done with its own ordering
- **Local SQLite storage** (WAL mode), no account and no network

## Features

- Multiple boards, each with freely ordered columns
- Cards with title, description, due date, and color labels
- Drag-and-drop cards and columns with animated reflow and settle flights
- Card editor modal, inline column creation, archive/restore, and per-column "archive all"
- Archived view as a filterable data table
- Dark / light appearance (persisted)
- One-click MCP access: create, move, update, archive, and query cards from an AI assistant

## Project layout

```
crates/
  core/   Shared data layer: SQLite schema, services, seed data,
          legacy-DB import, and the `kandrium-import` binary
  app/    eframe/egui desktop application (`kandrium-app`)
  mcp/    stdio MCP server (`kandrium-mcp`) built on rmcp
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
cargo test --workspace     # ~40 tests, including headless egui geometry tests
cargo clippy --workspace
cargo fmt
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
| Columns | `create_column`, `rename_column`, `delete_column`, `move_column`, `archive_all_cards` |
| Cards | `create_card`, `update_card`, `move_card`, `move_all_card`, `move_card_to_board`, `delete_card`, `archive_card`, `restore_card`, `list_archived_cards` |

Tool results are JSON objects with camelCase field names (e.g.
`boardId`, `dueDate`). All logging goes to stderr so stdout stays reserved for
the MCP transport.

## Data storage

| Path | Resolution |
| --- | --- |
| Database | `KANDRIUM_DB` env var, else `<data_dir>/kandrium/kandrium.db` |
| Legacy Electron DB (importer source) | `KANDRIUM_OLD_DB` env var, else `<data_dir>/planly/planly.db` |

On macOS `<data_dir>` is `~/Library/Application Support`, so the default
database is `~/Library/Application Support/kandrium/kandrium.db`.

On first boot, if no Kandrium database exists but a pre-rebrand
`<data_dir>/planly-rs/planly.db` is found, it is copied into place
automatically using a staged `VACUUM INTO` (the original file is never
modified). Both the app and the MCP server perform this check at startup.

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
cargo packager --release
```

This builds the release binary and produces `Kandrium.app` plus a DMG
(bundle identifier `app.kandrium.desktop`).

## License

Apache-2.0. See [LICENSE](LICENSE). Bundled Inter font files and Lucide icons
ship under their own licenses, included alongside the assets in
`crates/app/assets/`.
