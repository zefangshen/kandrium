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

- [ ] `cp -R crates/app/assets/fonts crates/app-gpui/assets/fonts`
- [ ] `cp -R crates/app/assets/icons crates/app-gpui/assets/icons`
- [ ] `cp -R crates/app/packaging crates/app-gpui/assets/packaging`
- [ ] `crates/app-gpui/src/theme.rs`: repoint the two Inter `include_bytes!`
      to `../assets/fonts/…`.
- [ ] `crates/app-gpui/src/assets.rs`: repoint every icon `include_bytes!` to
      `../assets/icons/…`.
- [ ] `crates/app-gpui/packager.json`: icon paths to `assets/packaging/…`.
- [ ] Gate: `cargo fmt --manifest-path crates/app-gpui/Cargo.toml`
- [ ] Gate: `cargo clippy --manifest-path crates/app-gpui/Cargo.toml --all-targets`
- [ ] Gate: `cargo test --manifest-path crates/app-gpui/Cargo.toml`
- [ ] Gate: `cargo packager --config crates/app-gpui/packager.json`; confirm
      `Kandrium.app/Contents/Resources` has the icon set.
- [ ] Commit only the copied assets, the three edited files, and the new
      packaging directory.
- [ ] Report: gate results + packaged icon evidence.

## S2 — Test port

Vim (`crates/app-gpui/src/vim.rs`):
- [ ] ex parse success/unknown and error text
- [ ] counted linewise `dd`/`yy`/`cc`
- [ ] counted delete-to-end and yank
- [ ] `j`/`k` goal column across short lines
- [ ] find motions `f F t T` and `;`/`,`
- [ ] quote, bracket and paragraph text objects (cross-line brackets)
- [ ] dot repeat of a normal command and of one Insert session (one undo step)
- [ ] visual text object, visual-line column, block caret geometry/fallback

Clocks (`crates/app-gpui/src/views/clocks.rs`, `tests/clocks.rs`):
- [ ] 12-hour formatting adds the lowercase period
- [ ] initials from the first two words
- [ ] empty and no-results states
- [ ] unknown zone renders a readable label
- [ ] residents/travel repaint boundary targets

Projects (`crates/app-gpui/src/views/projects.rs`, `tests/projects.rs`):
- [ ] relative time buckets
- [ ] document and description autosave
- [ ] archived project clears selection and shows Overview only
- [ ] identifier lowercases and accepts ten chars
- [ ] title Enter renames, Esc cancels
- [ ] drop index scoped to siblings

Card modal (`crates/app-gpui/src/views/card_modal.rs`, tests):
- [ ] project modal offers "Show on Tasks boards", not "Move to board"
- [ ] normal modal keeps "Move to board"
- [ ] Tab inserts two spaces
- [ ] Esc/ex behaviour with vim on and off

Documents (`crates/app-gpui/src/views/documents.rs`, `tests/documents.rs`):
- [ ] tab lifecycle
- [ ] `:w` flushes without closing
- [ ] `:q` flushes and closes
- [ ] create folder and Markdown file
- [ ] import copies into the selected folder

Board/tasks/cards/animation:
- [ ] mirrored project cards are drop anchors in the unified order
- [ ] All-board add-form and drag-gap rules
- [ ] `anim`: delayed track progress and no-track scroll jump
- [ ] card meta-label placement rules

Boot:
- [ ] `crates/app-gpui/src/store.rs`: open a DB created by the egui
      schema/seed path and read a card

- [ ] Gate: gpui fmt/clippy/test, ported list green.
- [ ] Commit tests only.
- [ ] Report: ported count + any DROP decision.

## S3 — Settings bridge and 24-hour preference

- [ ] `settings.rs`: add `clocks_use_24h` (default `true`) to the conf model,
      parser, writer and sorted-key output.
- [ ] Clocks view/probe reads `clocks_use_24h` instead of in-memory state; the
      toggle persists.
- [ ] `import_egui_settings()`: run only when `gpui-settings.conf` is absent;
      read `<data_dir>/Kandrium/app.ron`; map `vim_mode`, `section`,
      `notes_tree_open || docs_tree_open`, `projects_tree_open`,
      `clocks_use_24h`; ignore unknown/malformed input; never write `app.ron`.
- [ ] Unit tests: import, skip when conf exists, malformed file, 24h round trip.
- [ ] Boot test against a temp data dir with an `app.ron` fixture.
- [ ] Gate: gpui fmt/clippy/test.
- [ ] Commit.
- [ ] Report: imported keys + reset preferences record.

## S4 — Workspace takeover and deletion

- [ ] Root `Cargo.toml`: members `core`, `mcp`, `app-gpui`, `pdf-render`;
      remove `exclude`.
- [ ] `crates/app-gpui/Cargo.toml`: remove inner `[workspace]` /
      `[workspace.package]`; package `kandrium-app`; bin `kandrium-app`.
- [ ] `crates/app-gpui/src/main.rs`: log component `kandrium-app`.
- [ ] Replace `kandrium_app_gpui::` with `kandrium_app::` in `tests/*`.
- [ ] `git rm -r crates/app`.
- [ ] `justfile`: `run` builds `kandrium-app`; add `test`, `lint`, `fmt`.
- [ ] Gate: `cargo fmt --all --check`
- [ ] Gate: `cargo clippy --workspace --all-targets`
- [ ] Gate: `cargo test --workspace`
- [ ] Gate: `cargo build --bin kandrium-app`
- [ ] Commit.
- [ ] Report: final test count, clippy clean.

## S5 — Packaging and one dev bundle

- [ ] Root `packager.json`: gpui binary, moved icons, identifier unchanged.
- [ ] Delete `crates/app-gpui/packager.json`.
- [ ] Recreate `target/Kandrium-dev.app` pointing at `target/debug/kandrium-app`
      with `app.kandrium.desktop.dev` and the moved `icon.icns`.
- [ ] Remove `crates/app-gpui/target/KandriumGpui-dev.app`; `lsregister -u` it.
- [ ] Gate: `cargo packager --config packager.json` produces `.app` + DMG.
- [ ] Runtime: launch the dev bundle outside the sandbox; write a card; check
      the Dock icon.
- [ ] Commit.
- [ ] Report: bundle ids + launch evidence.

## S6 — Docs

- [ ] `README.md`: one app, commands, dev bundle, Wiki merge.
- [ ] `AGENTS.md`: gpui architecture, commands, dev bundle, sections, icons,
      conventions; remove egui-only conventions.
- [ ] `crates/app-gpui/README.md`: the new binary name and dev bundle.
- [ ] Gate: `cargo test --workspace`.
- [ ] Commit.
- [ ] Report.

## S7 — Sweep

- [ ] `cargo tree -d` and `Cargo.lock` review; confirm no
      `eframe`/`egui`/`egui_commonmark`/`egui_extras`/`syntect`/`objc`.
- [ ] `git grep -n "egui\|eframe\|egui_commonmark"`: only historical specs.
- [ ] Final fmt/clippy/test.
- [ ] Packaged smoke pass.
- [ ] Commit.
- [ ] Final report.
