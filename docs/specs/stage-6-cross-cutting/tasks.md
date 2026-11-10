# Stage 6 — task checklist

Order is strict: settings → watcher → packaging, one commit per part.

## Part 1 — settings persistence

- [ ] `crates/app-gpui/src/settings.rs`: `Settings` over a `BTreeMap`
      (unknown keys preserved), `TreePanel`, defaults, 160–420 px width clamp,
      `load`/`save` via `fsutil::write_atomic`, unit tests (missing file,
      roundtrip + unknown key, garbage width/section).
- [ ] `Section::from_key` in `app.rs`, mirroring `Section::id`.
- [ ] `AppShell`: `settings`, `settings_path`, `vim_mode`, `tree_drag`
      fields; load and seed in `with_store`; `persist_settings`; persist on
      section change, tree toggles, vim toggle, resize end.
- [ ] `NotesState.tree_w` / `DocumentsState.tree_w`, used by `tree_panel`;
      4 px splitter with drag-to-resize on both trees.
- [ ] `VimSession`: `enabled` (manual `Default`), `set_enabled`, `readonly`,
      disabled labels/empty pending; seed at all three editor creation sites.
- [ ] Handlers `handle_card_key`, `handle_notes_key`, `handle_documents_key`
      short-circuit when vim is off (`Esc` closes).
- [ ] Vim badge toggle in the card modal, Notes and Documents mode strips
      (`card-description-vim-toggle`, `note-vim-toggle`, `doc-vim-toggle`).
- [ ] `cargo fmt`, clippy, tests; commit part 1.

## Part 2 — data_version watcher

- [ ] `crates/app-gpui/src/watcher.rs`: thread polling `PRAGMA data_version`
      on its own connection, 250 ms poll + 120 ms debounce, stop flag, mpsc
      into the UI; never touches UI state.
- [ ] `AppShell`: `watcher`, `watcher_generation`, `pending_sync`; start the
      pump only for Tasks/Clocks in `render`; stop on leaving; scoped
      `apply_external_change`; defer while dragging; modal/tabs untouched.
- [ ] Tests: watcher thread emits and stops; external edit refreshes the
      board; drag defers; open modal survives.
- [ ] Verify the Clocks minute tick still renders from the cache with no DB
      query.
- [ ] `cargo fmt`, clippy, tests; commit part 2.

## Part 3 — packaging

- [ ] `crates/app-gpui/packager.json` (bundle id `app.kandrium.desktop`,
      version 0.1.0, shared icons, app + dmg, minimum macOS 10.15).
- [ ] `Justfile` recipe `gpui-bundle`; run it and inspect the `.app`/DMG.
- [ ] `crates/app-gpui/README.md`: packaging section, correct the stale
      read-only claim, keep the dev-bundle recipe as dev-only.
- [ ] Commit part 3.

## Final

- [ ] `cargo fmt` ×2, `cargo clippy --workspace --all-targets` ×2,
      `cargo test --workspace` + gpui tests; record counts.
- [ ] Relaunch `target/KandriumGpui-dev.app`.
- [ ] Report: persisted state, external-edit behaviour, packaging output and
      command, test counts, commit hashes, dropped parity, on-screen checks.
