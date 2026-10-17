# AGENTS.md

Guidance for AI coding agents working in this repository. Read before making
changes. User-facing overview lives in [README.md](README.md).

## What this is

Kandrium is a local-first Kanban app on a pure-Rust stack:

- **eframe/egui 0.36** desktop UI (`crates/app`, binary `kandrium-app`)
- **rmcp 3.5** stdio MCP server (`crates/mcp`, binary `kandrium-mcp`)
- **rusqlite 0.40** (bundled SQLite) shared data layer (`crates/core`, crate `kandrium-core`)
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
  background thread polling `PRAGMA data_version` (`crates/app/src/watcher.rs`).
- Async work reaches the egui UI thread through tokio/`std::sync::mpsc`
  channels; never touch UI state from another thread.
- The MCP wire format mirrors the legacy TypeScript server exactly:
  camelCase JSON (`#[serde(rename_all = "camelCase")]`), pretty-printed text
  results, tool-level `isError` on failure. Preserve tool names and schemas —
  MCP clients may depend on them.
- The SQL schema (`crates/core/src/schema.sql`, v1, applied via
  `PRAGMA user_version`) is identical in shape to the legacy Drizzle schema so
  the row-copy importer works. Keep both compatible; new schema steps must be
  idempotent `CREATE TABLE IF NOT EXISTS` / additive migrations.
- IDs are 24 lowercase hex chars (`db::uid()`), matching the old app. Seed data
  uses fixed string ids (`board_career`, `col_work_todo`, ...).

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
cargo test --workspace             # ~40 tests, must stay green
cargo clippy --workspace           # must be warning-free
cargo fmt
cargo packager --release           # build Kandrium.app + DMG (packager.json)
```

Before finishing a change: run `cargo fmt`, `cargo clippy --workspace`, and
`cargo test --workspace`. All three are expected to pass cleanly.

### macOS sandbox note for agent shells

A GUI launched from a sandboxed agent shell can read the default DB but every
SQLite write fails with `SQLITE_READONLY` (`~/Library/Application Support` is
not writable by the sandboxed process). Launch detached instead:

```sh
open -n target/debug/kandrium-app
```

For testing with a writable database, set `KANDRIUM_DB` to a path under the
workspace or `/tmp`. `dangerouslyDisableSandbox` does not bypass this. Visual
verification may be impossible from the agent shell (screen capture blocked) —
ask the user to confirm UI changes.

## egui conventions (hard-won — read before touching UI code)

- **Fonts**: all font sizes live ONLY in the `text_styles` table in
  `theme.rs::apply_visuals` (Small 13 / Body 16 / Button 16 / Heading 20 /
  Monospace 16, plus named `strong` and `small-strong`). Widgets pick a
  `TextStyle` and resolve it with `theme::font_id(ui, style)` for painter text.
  Never introduce a separate scaling layer or pass explicit pixel sizes.
- **Icons**: inline Lucide SVGs via `crates/app/src/icons.rs`. Do not re-add
  `egui-phosphor`.
- **Tables**: use `egui_extras::TableBuilder` (fixed columns, sticky header,
  striped rows) — see `crates/app/src/views/archive.rs`. Archived page board
  filter default must be `"all"`, never `""`.
- **Drag-and-drop**: hand-rolled (`dnd.rs` + `anim.rs`), no community crate.
  Easing constants: `cubic-bezier(0.20, 0, 0, 1)` for reflow/lift and
  `cubic-bezier(0.40, 0, 0.20, 1)` for snap-back. Tune timing only in
  `anim.rs`.
- **Scrollbars are floating and reserve zero global layout space**
  (`ScrollStyle` in `theme.rs`). Content that must not sit under a bar gets the
  scoped constant gutter: `widgets::CONTENT_RIGHT_INSET` (7 px) via
  `.content_margin(...)` inside card-list ScrollAreas or `widgets::right_inset()`
  for direct layout. Set `scroll.floating_allocated_width` only locally for
  contexts needing a reserved lane, and reset it at the top of a content
  closure that spawns nested scroll areas.
- **Nested scroll inside horizontal scroll**: `ScrollArea::horizontal` gives
  infinite available width, which collapses nested vertical scrolls to glyph
  width. Allocate columns with
  `ui.allocate_ui_with_layout(vec2(280.0, h), Layout::top_down(Align::LEFT), ...)`.
  Never call `set_min_width/set_max_width` on a horizontal row placer. Capture
  the height ceiling BEFORE entering `ui.horizontal_top(...)`, floor empty
  drop-zone bodies at ~30 px, and allocate the 280 px width unconditionally.
- **Frames shrink-wrap content**: call `ui.set_min_width(ui.available_width())`
  first inside a frame closure on a vertical placer.
- **Loop-generated widgets need explicit IDs**: wrap per-entity content in
  `ui.push_id(("prefix", entity.id.as_str()), |ui| ...)` to avoid duplicate
  `ScrollArea`/widget ID clashes.
- **Fixed-height multiline editor**: outer fixed bordered `Frame` +
  inner `ScrollArea::vertical().max_height(h).auto_shrink([false; 2])` +
  frameless `TextEdit::multiline` (egui 0.36 multiline has no internal scroll).
  Reference: `description_editor()` in `widgets/card_modal.rs`.
- **egui context lock is not reentrant**: never call `ctx.input()` inside
  `ctx.data_mut()`; read input first.
- eframe's app-creator closure runs before `applicationDidFinishLaunching`;
  do "once the app is running" work (e.g. dock icon) on the first frame in
  `app.rs::logic()` with `ctx.request_repaint()` retries.

## Headless UI tests

Geometry/widget logic can be tested without a GPU with
`egui::Context::default()` + `ctx.run_ui(RawInput::default(), ...)`. In every
headless test module call `theme::install_fonts(&ctx)` and
`theme::apply_visuals(&ctx, false)` first, and clear
`output.textures_delta` before the context drops (avoids a drop panic). Scan
`FullOutput.shapes` for `"use of"` text to catch duplicate-ID warnings.

## Design system

Neutral gray-on-gray PdfCraft style: single blue accent, Inter typography,
6 px corner radius, transparent hover/press wash with 1 px content offset on
press, floating scrollbars, Lucide icons, bottom-center inverted toasts,
reduced-motion support.

## Database location / env vars

- `KANDRIUM_DB` overrides the DB path; default `<data_dir>/kandrium/kandrium.db`.
- `KANDRIUM_OLD_DB` overrides the legacy Electron source for `kandrium-import`.
- Auto-migration from `<data_dir>/planly-rs/planly.db` runs once at app and MCP
  boot (staged `VACUUM INTO`, never writes the source). A `KANDRIUM_DB`
  override opts out of auto-migration.
