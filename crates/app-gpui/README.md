# kandrium-app

The gpui desktop front-end: the `kandrium-app` binary in the root workspace.
Tasks, Clocks, Wiki (notes + documents) and Projects are all implemented here.

## Run the app

```sh
cargo run -p kandrium-app
# or
just run
```

The app opens the default database and shows the stored section (Tasks by
default). It reads and writes through `kandrium-core`, and a background watcher
polls `PRAGMA data_version`, so edits made by the MCP server appear while the
screen is open. UI preferences (vim mode, last section, tree widths and
collapse flags, 12/24-hour clocks) live in `gpui-settings.conf` next to the
database; a missing file is seeded once from the retired egui store.

`KANDRIUM_DB` overrides the database path, as for the other binaries.

### Running on macOS

`target/Kandrium-dev.app` is a **developer convenience**, not a product build:
its executable is a copy of the debug binary. A GUI launched from a sandboxed
agent shell cannot write the real database, so launch through this bundle
instead — LaunchServices starts it as a real application, outside the agent
sandbox:

```sh
just dev
```

`just dev` rebuilds, copies the fresh binary into the bundle and reopens it.
The copy matters: a symlink or launcher script makes LaunchServices hang
before `main` on this OS. The bundle is documented in
[AGENTS.md](../../AGENTS.md).

## Package a macOS bundle

Run from the repository root:

```sh
just bundle
# = cargo packager --config packager.json
```

The root `packager.json` sets bundle id `app.kandrium.desktop`, version
`0.1.0`, product name `Kandrium`, icons from
`crates/app-gpui/assets/packaging/`, and macOS 10.15 as the minimum. Outputs:

```text
Kandrium.app
Kandrium_0.1.0_aarch64.dmg
```

## The green path (Probes A–D)

The migration probes remain runnable. They are diagnostic only.

```sh
cargo run --bin kandrium-app -- board
cargo run --bin kandrium-app -- input
cargo run --bin kandrium-app -- pdf
cargo run --bin kandrium-app -- drag
```

Each probe prints its evidence to stdout before and while the window runs.

- **Probe C** needs a PDF: `$SPIKE_PDF`, else the first `*.pdf` under
  `$KANDRIUM_DOCS`, else under `<database parent>/documents`.
- **Probe A** copies the live database (plus `-wal` / `-shm`) to a temp
  directory first, because `get_board` can reconcile `area_card_order`.

## Tests

```sh
cargo test -p kandrium-app
```

- `harness_test::window_draws_and_delivers_input` — `TestAppContext` can draw
  a frame, observe layout, dispatch a click and a keystroke, and keep focus.
- `tests/shell.rs` — boots the real `AppShell` over a freshly seeded temp
  database under `TestAppContext`: every section renders, board/clocks cards
  draw, and settings round-trip.
- `tests/{clocks,documents,notes,projects,wiki}.rs` — section flows.
- `src/settings.rs` — defaults, the egui import, and unknown-key preservation.
- `src/watcher.rs` — another connection's commit is reported.
- `src/vim.rs` — the modal engine (motions, operators, text objects, ex).
- `src/store.rs` — core round trips, including the mirrored-area drop order.

No window server and no GPU are involved.

## Dependencies

Pinned exactly so behaviour stays reproducible:

| crate | version |
| --- | --- |
| `gpui-kit` | `=0.9.1` |
| `gpui-pre` (dev only) | `=0.3.12` |
| `gpui-component` / `gpui-base` (transitive) | `0.9.1` |
| `taffy` (transitive) | `0.15.0` |

`taffy` is **0.15.0**, not 0.15.1, because `gpui-kit/test-support` locks the
optional `gpui-fast` backend (`gpui-fast?/test-support`) even though it is never
built, and `gpui-fast` pins `taffy = "=0.15.0"` while `gpui-pre` accepts
`^0.15.0`. Both ranges accept 0.15.0, so there is no upstream conflict — but a
lock that already holds 0.15.1 can produce `all possible versions conflict`
until the pin is forced:

```sh
cargo update -p taffy --precise 0.15.0
```

## Layout

| file | role |
| --- | --- |
| `src/lib.rs` | `run()`: install fonts, open the window |
| `src/app.rs` | `AppShell`, `Section`, the segmented control, the watcher pump |
| `src/settings.rs` | `gpui-settings.conf` and the one-time egui import |
| `src/watcher.rs` | `data_version` polling thread, events over an mpsc channel |
| `src/theme.rs` | palette, typography tokens, font install |
| `src/assets.rs` | Lucide SVGs compiled from `assets/icons/` |
| `src/store.rs` | `kandrium-core` calls (reads and writes) |
| `src/vim.rs` | the modal editor shared by every text field |
| `src/views/tasks.rs` | board, drag and drop |
| `src/views/card_modal.rs` | card editor |
| `src/views/clocks.rs` | saved cities, travel and the time probe |
| `src/views/wiki.rs` | the Wiki split |
| `src/views/notes.rs` | notes tree and tabs |
| `src/views/documents.rs` | documents tree and Markdown tabs |
| `src/views/documents_pdf.rs` | PDF viewer and annotations |
| `src/views/projects.rs` | project tree and detail pane |
