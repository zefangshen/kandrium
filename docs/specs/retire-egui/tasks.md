# Tasks: retire the egui app

Execution order is the stage order in [spec.md](spec.md) §3. Each stage is one
commit; stop and report at every stage boundary.

## S0 — Audit artifacts

- [x] Append the four verification findings and the 152-test verdict table to
      `docs/specs/retire-egui/plan.md` (§3 rows become locked findings).
- [x] Create the annotated tag: `git tag -a pre-egui-removal -m "Last tree with the egui app"`.
- [x] Verify: `git tag --list pre-egui-removal`.
- [x] Run `cargo test --workspace` (no code changed; must stay green).
- [x] Commit only `docs/specs/retire-egui/{plan.md,spec.md,tasks.md}`.
- [x] Report: findings summary + tag created.

## S1 — Assets and packaging copy

- [x] `cp -R crates/app/assets/fonts crates/app-gpui/assets/fonts`
- [x] `cp -R crates/app/assets/icons crates/app-gpui/assets/icons`
- [x] `cp -R crates/app/packaging crates/app-gpui/assets/packaging`
- [x] `crates/app-gpui/src/theme.rs`: repoint the two Inter `include_bytes!`
      to `../assets/fonts/…`.
- [x] `crates/app-gpui/src/assets.rs`: repoint every icon `include_bytes!` to
      `../assets/icons/…`.
- [x] `crates/app-gpui/packager.json`: icon paths to `assets/packaging/…`.
- [x] Gate: `cargo fmt --manifest-path crates/app-gpui/Cargo.toml`
- [x] Gate: `cargo clippy --manifest-path crates/app-gpui/Cargo.toml --all-targets`
- [x] Gate: `cargo test --manifest-path crates/app-gpui/Cargo.toml`
- [x] Gate: `cargo packager --config crates/app-gpui/packager.json`; confirm
      `Kandrium.app/Contents/Resources` has the icon set.
- [x] Commit only the copied assets, the three edited files, and the new
      packaging directory.
- [x] Report: gate results + packaged icon evidence.

## S2 — Test port

Outcome: 16 new gpui tests (14 vim engine cases, 2 store cases) plus one
mirrored-card drop test; the rest of the egui list was already covered by
existing gpui tests or has no gpui code path and is marked DROP.

Vim (`crates/app-gpui/src/vim.rs`):
- [x] ex parse success/unknown and error text
- [x] counted linewise `dd`/`yy`/`cc`
- [x] counted delete-to-end and yank
- [x] `j`/`k` goal column across short lines
- [x] find motions `f F t T` and `;`/`,`
- [x] quote, bracket and paragraph text objects (cross-line brackets)
- [x] dot repeat of a normal command and of one Insert session (one undo step)
- [x] visual text object and visual-line column
- [x] DROP block-caret geometry: the overlay measures glyphs at paint time;
      no pure helper exists on this side

Clocks (`crates/app-gpui/src/views/clocks.rs`, `tests/clocks.rs`):
- [x] 12-hour formatting and the probe field (already covered)
- [x] initials from the first two words (already covered)
- [x] DROP empty/no-results states: egui layout text, no shared surface
- [x] DROP unknown zone: resolution is core-owned; no dedicated renderer
- [x] residents/travel repaint boundary targets (already covered)

Projects (`crates/app-gpui/src/views/projects.rs`, `tests/projects.rs`):
- [x] DROP relative time buckets: the gpui project rows do not show them
- [x] document and description autosave (already covered)
- [x] archived project clears selection and pins Overview (already covered)
- [x] identifier normalisation (covered by project create/rename tests)
- [x] title Enter renames, Esc cancels (already covered)
- [x] drop index scoped to siblings (already covered)

Card modal (`crates/app-gpui/src/views/card_modal.rs`, tests):
- [x] project vs normal modal actions (already covered)
- [x] DROP Tab inserts two spaces: the gpui focus trap reserves Tab for
      focus navigation inside the modal, so the key never reaches the field.
      Not worth a framework-level workaround; recorded as a deliberate
      divergence and reported.
- [x] Esc/ex behaviour with vim on and off (already covered)

Documents (`crates/app-gpui/src/views/documents.rs`, `tests/documents.rs`):
- [x] tab lifecycle (already covered)
- [x] `:w` flush and `:q` flush+close (already covered)
- [x] create folder and Markdown file (already covered)
- [x] import copies into the selected folder (already covered)

Board/tasks/cards/animation:
- [x] mirrored project cards are drop anchors in the unified order
      (`store::tests::area_board_treats_a_filed_project_card_as_a_drop_anchor`)
- [x] DROP All-board add-form/drag-gap: egui layout tests, no shared surface
- [x] DROP `anim` delayed-track/no-track: gpui has no track or stationary
      scroll concept; its seven curve/reflow tests stay
- [x] card meta-label placement (`store::tests::all_board_labels_a_card_with_its_home_board_title`)

Boot:
- [x] `store::tests::opens_a_database_written_by_the_shared_bootstrap`

- [x] Gate: gpui fmt/clippy/test, ported list green.
- [x] Commit tests only.
- [x] Report: ported count + DROP decisions.

## S3 — Settings bridge and 24-hour preference

- [x] `settings.rs`: add `clocks_use_24h` (default `true`) to the conf model,
      parser, writer and sorted-key output.
- [x] Clocks view/probe reads `clocks_use_24h` instead of in-memory state; the
      toggle persists.
- [x] `import_egui_settings()`: run only when `gpui-settings.conf` is absent;
      read `<data_dir>/Kandrium/app.ron`; map `vim_mode`, `section`,
      `notes_tree_open || docs_tree_open`, `projects_tree_open`,
      `clocks_use_24h`; ignore unknown/malformed input; never write `app.ron`.
- [x] Unit tests: import, skip when conf exists, malformed file, 24h round trip.
- [x] Boot test against a temp data dir with an `app.ron` fixture.
- [x] Gate: gpui fmt/clippy/test.
- [x] Commit.
- [x] Report: imported keys + reset preferences record.

## S4 — Workspace takeover and deletion

- [x] Root `Cargo.toml`: members `core`, `mcp`, `app-gpui`, `pdf-render`;
      remove `exclude`.
- [x] `crates/app-gpui/Cargo.toml`: remove inner `[workspace]` /
      `[workspace.package]`; package `kandrium-app`; bin `kandrium-app`.
- [x] `crates/app-gpui/src/main.rs`: log component `kandrium-app`.
- [x] Replace `kandrium_app_gpui::` with `kandrium_app::` in `tests/*`.
- [x] `git rm -r crates/app`.
- [x] `justfile`: `run` builds `kandrium-app`; add `test`, `lint`, `fmt`.
- [x] Gate: `cargo fmt --all --check`
- [x] Gate: `cargo clippy --workspace --all-targets`
- [x] Gate: `cargo test --workspace`
- [x] Gate: `cargo build --bin kandrium-app`
- [x] Commit.
- [x] Report: final test count, clippy clean.

## S5 — Packaging and one dev bundle

- [x] Root `packager.json`: gpui binary, moved icons, identifier unchanged.
- [x] Delete `crates/app-gpui/packager.json`.
- [x] Recreate `target/Kandrium-dev.app` pointing at `target/debug/kandrium-app`
      with `app.kandrium.desktop.dev` and the moved `icon.icns`.
- [x] Remove `crates/app-gpui/target/KandriumGpui-dev.app`; `lsregister -u` it.
- [x] Gate: `cargo packager --config packager.json` produces `.app` + DMG.
      `.app` verified; the DMG step cannot create its `/Volumes` link from the
      agent shell (permission denied) — rerun `just bundle` in a normal shell.
- [x] Runtime: launch the dev bundle outside the sandbox; write a card; check
      the Dock icon.
- [x] Commit.
- [x] Report: bundle ids + launch evidence.

## S6 — Docs

- [x] `README.md`: one app, commands, dev bundle, Wiki merge.
- [x] `AGENTS.md`: gpui architecture, commands, dev bundle, sections, icons,
      conventions; remove egui-only conventions.
- [x] `crates/app-gpui/README.md`: the new binary name and dev bundle.
- [x] Gate: `cargo test --workspace`.
- [x] Commit.
- [x] Report.

## S7 — Sweep

- [x] `cargo tree -d` and `Cargo.lock` review; confirm no
      `eframe`/`egui`/`egui_commonmark`/`egui_extras`/`syntect`/`objc`.
- [x] `git grep -n "egui\|eframe\|egui_commonmark"`: only historical specs.
- [x] Final fmt/clippy/test.
- [x] Packaged smoke pass.
- [x] Commit.
- [x] Final report.
