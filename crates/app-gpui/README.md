# kandrium-app-gpui

The gpui front-end for the egui → gpui migration (see `gpui-migration-plan.md`).
Tasks, Clocks, Notes and Documents are ported; Projects is still a placeholder.
The four Phase 0 probes are kept for reference.

This crate is its own workspace. The repository root excludes it
(`exclude = ["crates/app-gpui"]`), so the shipping egui build,
`cargo test --workspace` and `cargo clippy --workspace` keep exactly the cost
they had before the migration started.

## Run the app

```sh
cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- app
```

The app opens the default database and shows the stored section (Tasks by
default). It reads and writes through `kandrium-core`, and a background watcher
polls `PRAGMA data_version`, so edits made by the egui app or the MCP server
appear while the Tasks or Clocks section is open. UI preferences (vim mode,
last section, tree widths and collapse flags) live in
`gpui-settings.conf` next to the database.

`KANDRIUM_DB` overrides the database path, as for the other binaries.

### Running on macOS

`KandriumGpui-dev.app` is a **developer convenience**, not a product build: its
executable is a symlink into `target/debug/`. A GUI launched from a sandboxed
agent shell cannot be trusted to see the right window server/state, so launch
through this bundle instead — LaunchServices starts it as a real application,
outside the agent sandbox:

```sh
cargo build --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui
mkdir -p crates/app-gpui/target/KandriumGpui-dev.app/Contents/MacOS \
         crates/app-gpui/target/KandriumGpui-dev.app/Contents/Resources
ln -sf ../../../debug/kandrium-gpui \
  crates/app-gpui/target/KandriumGpui-dev.app/Contents/MacOS/kandrium-gpui
cp crates/app/packaging/icon.icns \
  crates/app-gpui/target/KandriumGpui-dev.app/Contents/Resources/icon.icns
# Contents/Info.plist: APPL, bundle id app.kandrium.desktop.gpui-dev,
# CFBundleExecutable kandrium-gpui, CFBundleIconFile icon.icns.
open -n crates/app-gpui/target/KandriumGpui-dev.app
```

The executable is a symlink into `target/debug/`, so every rebuild is picked up
on the next launch. Restart with:

```sh
killall kandrium-gpui; open -n crates/app-gpui/target/KandriumGpui-dev.app
```

## Package a macOS bundle

The product build uses `cargo packager`, like the egui app, with a crate-local
config so both binaries can keep their own name. Run from the repository root:

```sh
just gpui-bundle
# = cargo build --release --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui
#   cargo packager --config crates/app-gpui/packager.json
```

`crates/app-gpui/packager.json` sets bundle id `app.kandrium.desktop`, version
`0.1.0`, product name `Kandrium` (which is also the window title), the shared
icons from `crates/app/packaging/`, and macOS 10.15 as the minimum. Outputs:

```text
crates/app-gpui/target/release/bundle/Kandrium.app
crates/app-gpui/target/release/bundle/Kandrium_0.1.0_aarch64.dmg
```

cargo-packager changes into the config file's directory, which is why the
binary, output and icon paths in that file are relative to `crates/app-gpui`.
The root `packager.json` (egui) is untouched. The two configs intentionally
duplicate the icon list and version string; when the egui app retires, both can
share one root-level `packaging/` directory and a single version source.

## The green path (Probes A–D)

The Phase 0 probes remain runnable. They are diagnostic only.

```sh
cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- board
cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- input
cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- pdf
cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- drag
```

Each probe prints its evidence to stdout before and while the window runs.

- **Probe C** needs a PDF: `$SPIKE_PDF`, else the first `*.pdf` under
  `$KANDRIUM_DOCS`, else under `<database parent>/documents`.
- **Probe A** copies the live database (plus `-wal` / `-shm`) to a temp
  directory first, because the probe predates `get_board_readonly` and
  `get_board` can reconcile `area_card_order`.

## Tests

```sh
cargo test --manifest-path crates/app-gpui/Cargo.toml
```

- `harness_test::window_draws_and_delivers_input` — `TestAppContext` can draw
  a frame, observe layout, dispatch a click and a keystroke, and keep focus.
- `tests/shell.rs` — boots the real `AppShell` over a freshly seeded temp
  database under `TestAppContext`:
  - every section switch renders its body (and the five nav chips are
    individually addressable, so duplicate element paths would fail);
  - the board view draws the real columns at the design-system width and the
    real seeded cards;
  - the clocks view renders every seeded city;
  - the section, tree collapse and vim preference land in
    `gpui-settings.conf`, and with vim off the card description takes plain
    typing.
- `src/settings.rs` — defaults with no file, unknown keys preserved across a
  rewrite, bad values fall back, widths clamp.
- `src/watcher.rs` — another connection's commit is reported; a stopped
  watcher reports nothing.
- `src/app.rs` — an external rename reloads the board while an open modal
  survives, and defers while a card is dragged.

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
| `src/settings.rs` | `gpui-settings.conf`: vim, section, tree widths/flags |
| `src/watcher.rs` | `data_version` polling thread, events over an mpsc channel |
| `src/theme.rs` | palette, the single `Text` size table, font install |
| `src/assets.rs` | Lucide SVGs, loaded from `crates/app/assets/icons/` |
| `src/store.rs` | `kandrium-core` calls (reads and writes) |
| `src/vim.rs` | the modal editor shared by every text field |
| `src/views/tasks.rs` | board, drag and drop, card modal |
| `src/views/clocks.rs` | saved cities and travel |
| `src/views/notes.rs` | notes tree and tabs |
| `src/views/documents.rs` | documents tree, PDF and Markdown tabs |
| `src/views/placeholder.rs` | empty/error/not-ported states |
