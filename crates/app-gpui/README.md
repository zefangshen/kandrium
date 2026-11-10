# kandrium-app-gpui

The gpui front-end for the egui → gpui migration (see `gpui-migration-plan.md`).
Stage 1 is a **foundation**: an app shell with five sections, a read-only Tasks
board and a real Clocks view, plus the four Phase 0 probes kept for reference.

This crate is its own workspace. The repository root excludes it
(`exclude = ["crates/app-gpui"]`), so the shipping egui build,
`cargo test --workspace` and `cargo clippy --workspace` keep exactly the cost
they had before the migration started.

## Run the app

```sh
cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- app
```

The app opens the default database **read-only** and shows the first Tasks
board and the saved Clocks. Notes, Documents and Projects are honest
placeholders until later stages. It does not write to the database: the egui
app and the MCP server keep full ownership of writes.

`KANDRIUM_DB` overrides the database path, as for the other binaries.

### Running on macOS

Like the egui app, a GUI launched from a sandboxed agent shell cannot be trusted
to see the right window server/state. Launch it through a minimal `.app` bundle
instead — LaunchServices starts it as a real application, outside the agent
sandbox:

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
  - the clocks view renders every seeded city.

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
| `src/app.rs` | `AppShell`, `Section`, the segmented control |
| `src/theme.rs` | palette, the single `Text` size table, font install |
| `src/assets.rs` | Lucide SVGs, loaded from `crates/app/assets/icons/` |
| `src/store.rs` | read-only `kandrium-core` calls |
| `src/views/tasks.rs` | read-only board |
| `src/views/clocks.rs` | saved cities |
| `src/views/placeholder.rs` | empty/error/not-ported states |
