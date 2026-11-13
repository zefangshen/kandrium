# Stage 7 — task checklist

Each task names the file and the deciding function. Commit groups are marked;
do not push.

## T1 — Core-facing store wrappers (commit: tree)

- [x] `crates/app-gpui/src/store.rs`: add the wrappers listed in spec §3,
      reading through `kandrium_core::projects`, using the read handle for
      reads and the writable handle for writes.
- [x] Add store tests for: create folder + project and read the tree back;
      rename/reorder within one parent; archive hides from `project_tree` and
      appears in `archived_projects`; doc round-trip
      `create_project_doc` → `write_project_doc` → `read_project_doc`.

## T2 — Projects state, tree and CRUD (commit: tree)

- [x] `crates/app-gpui/src/views/mod.rs`: register `pub mod projects;`.
- [x] `crates/app-gpui/src/views/projects.rs`:
  - state types from spec §4.1;
  - `project_matches`, `visible_tree`, `insert_index`, `close_index` + unit
    tests;
  - tree renderer with persisted collapse/width rail, filter input, folders
    above projects, identifier label + `.tooltip(name)`, archived toggle;
  - sibling-only drag (`ProjectDrag`, `on_drag`, `on_drag_move`, `on_drop`)
    disabled while the filter is non-empty;
  - folder/project inline create and folder rename drafts, folder delete with
    inline confirmation;
  - a minimal `projects_view` detail area that shows the selected project's
    name/identifier and a placeholder (full pane lands in T3) so this commit
    compiles and runs.
- [x] `crates/app-gpui/src/app.rs`: `projects: ProjectsState` field;
  `with_store` seeds `projects_tree_open/width`; `persist_settings` writes
  them; `select` loads/unloads Projects; `load_projects`,
  `reload_project_detail`, `select_project`, `toggle_projects_tree`,
  `set_projects_tree_width`, `begin_project_draft`,
  `handle_project_draft_key`, `commit_project_draft`,
  `cancel_project_draft`, `begin_project_folder_rename`, `hover_project_drop`,
  `drop_project`.
- [x] Tests: filter matches identifier and name (unit); reorder only among
  siblings (store + unit `sibling_projects`); creation, tree selection and a
  committed inline rename exercised end-to-end in `tests/projects.rs`.
- [x] Gate + commit.

## T3 — Detail pane and board reuse (commit: detail)

- [x] `views/projects.rs`: `ProjectTab::{Overview, Tasks, Doc}`,
  `activate_project_tab`, pinned-tab close guard, tab strip,
  `overview_panel` with:
  - double-click title edit (`TitleEdit` + `InputState`; Enter/Blur commit,
    Esc capture cancel);
  - identifier input + Save → `set_project_identifier` (locks);
  - source input + Save → `set_project_source`;
  - status chips → `update_project(None, Some(status))`;
  - description textarea with vim session + Save →
    `update_project(Some(text), None)`;
  - summary line; Archive/Restore (immediate) and Delete (inline confirm);
  - `detail_editor` rebuilt only when the selected project changes or an
    explicit save/reload occurs.
- [x] `app.rs`: `project_board_shown`; split `load_board` from a new
  `load_project_board` that does not overwrite `active_board_id`;
  `sync_project_board` at the top of the Projects body; `reload_displayed_board`
  used by `drop_card` and `persist_editing`; Tasks tab renders
  `tasks::board_view` with `board`, `drop_target`, `dragging_card` (same
  surface, same handlers).
- [x] Handlers: `begin_project_title_edit`, `commit_project_title`,
  `cancel_project_title`, `save_project_fields`, `set_project_status`,
  `archive_selected_project`, `restore_selected_project`,
  `ask_delete_selected_project`, `ask_delete_folder`,
  `cancel_project_confirm`, `confirm_project_delete`.
- [x] Tests: `tests/projects.rs` proves the project Tasks tab renders
  `board-view` with the project's card, that the pinned tabs are not closable
  (unit), that a title rename regenerates the identifier and syncs the board
  title, and that leaving Projects restores the Tasks section's own board
  (the project board never becomes `active_board_id`). Archive/delete go
  through the store tests.
- [x] Gate + commit.

## T4 — Project documents (commit: documents)

- [x] `views/projects.rs`: Overview document list from
  `store.project_files`; New file / New folder inline drafts (Enter/Esc);
  `open_project_doc` reads through `store.read_project_doc`; doc tab editor
  and preview; per-tab vim/preview; mode strip badge.
- [x] `app.rs`: `ProjectDocTab` observer → generation → 400 ms debounce →
  `store.write_project_doc`; `flush_project_docs` on tab close, project
  switch and section switch; `handle_project_doc_key` for Esc/`:w` semantics
  shared with Notes; errors surface inline, dirty tabs are never dropped.
- [x] Watcher: Projects joins `wants_watcher`; external changes are applied
  only when no drag/draft/title edit/confirm is in flight (otherwise
  `pending_sync`), and never rebuild tab editors or a dirty description.
- [x] Tests: `tests/projects.rs` opens a project document row and asserts the
  shared editor and body render; the store covers create/write/read of a
  document; debounce, close and project-switch flush share the Notes
  implementation and are on the on-screen check list.
- [x] Gate + commit.

## T5 — Clocks wall-clock tests (commit: tests)

- [x] `crates/app-gpui/tests/clocks.rs`: add `today_in(zone)` and
  `active_window(zone)`; test 1 builds its window in the destination zone;
  test 2 builds it in the new home's zone and derives the unrelated trip's
  date from the new-home window's end; assertions strengthened with a
  "window contains now" check, not loosened.
- [x] Audit both workspaces for test code using `Zoned::now`,
  `Timestamp::now(...).date` or `today()` to build windows. Only the two
  gpui Clocks tests had the assumption; the egui Clocks tests use offsets
  from `now` and core's window tests use fixed dates.
- [x] Reasoning documented in the comments: after 23:00 the destination's
  civil date is already the next day, so a window built from the
  process-local date has ended; deriving it in the owning zone keeps `now`
  inside the window at every hour.
- [x] Gate + commit.

## T6 — Final gate and report

- [x] `cargo fmt`; `cargo fmt --manifest-path crates/app-gpui/Cargo.toml`.
- [x] `cargo clippy --workspace --all-targets`; gpui clippy `--all-targets`.
- [x] `cargo test --workspace`; `cargo test --manifest-path
      crates/app-gpui/Cargo.toml`; counts recorded in the report.
- [x] Rebuild and relaunch the gpui dev bundle.
- [x] Report: end-to-end capabilities, proof Tasks reuses the board surface,
      test-fix evidence, counts, hashes, parity dropped, on-screen checks,
      and everything remaining before cutover.
