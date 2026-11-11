# Stage 7 — Projects, and the wall-clock test fix

Status: proposed on `gpui-migration`. Implementation follows `tasks.md` after
approval.

## 1. Goal

Finish the last gpui section and remove the two time-dependent Clocks test
failures.

1. **Projects section** matching `crates/app/src/views/projects.rs`:
   - left tree: folders above projects within a level, sibling-only drag
     reorder, filter on identifier + full name, identifier label with the full
     name in a tooltip, persisted `projects_tree_open` / `projects_tree_width`;
   - detail pane: pinned non-closable Overview and Tasks tabs plus one closable
     tab per opened markdown/text document;
   - Tasks reuses the existing board surface against the project's board;
   - Overview: double-click title rename (Enter/blur commits, Esc cancels)
     that regenerates the identifier until locked and syncs the board title;
     editable identifier, source, archived flag;
   - project documents through core `projects::read_doc` / `write_doc`,
     root-guarded, shared Markdown editor + vim, per-tab mode, debounced
     autosave, flush on close;
   - create folders/projects, delete, archive with an inline confirmation.
2. **Clocks tests**: fix
   `a_trip_can_be_assigned_extended_ended_and_removed` and
   `relocation_cancels_trips_to_both_homes` by deriving the trip date in the
   destination zone instead of the process-local zone. Assertions stay
   unchanged. Audit both workspaces for the same class of assumption.

Constraints: data logic only in `kandrium-core`; no second board renderer;
`cargo fmt`, `cargo clippy --workspace --all-targets`, `cargo test --workspace`
plus the gpui crate tests; leave `crates/app/src/widgets/vim.rs:2436` alone.
Commits on `gpui-migration` as specified by the user (author `openspex`,
committer `zefangshen`, no `Co-Authored-By`, no assistant mentions, never
`git add -A`, no push), one commit per area: tree, detail pane, documents,
tests.

## 2. Findings that drive the design

- `kandrium_core::projects` already owns every operation needed:
  `list_tree`, `get_project`, `project_summary`, `project_files`,
  `list_archived_projects`, `create_project`, `create_folder`,
  `rename_folder`, `delete_folder`, `rename_project`, `update_project`,
  `set_source`, `set_identifier`, `archive_project`, `unarchive_project`,
  `delete_project`, `reorder_project`, `read_doc`, `write_doc`,
  `create_doc_file`, `create_doc_folder`, `ensure_root`, `find_project`.
  No new core data function is required; only thin `Store` wrappers.
- `reorder_project(id, index)` already clamps and rewrites only that
  `folder_id`'s positions, so sibling-only reordering needs no new SQL.
- The gpui shell's board surface is `views::tasks::board_view`, driven by
  `AppShell::{board, drop_target, dragging_card}`. `load_board` currently
  overwrites `active_board_id`; that would corrupt the Tasks section if reused
  for a project board, so the shell needs a display-only board load.
- `InputEvent` in `gpui-base` has `PressEnter`, `Focus` and `Blur`, so
  Enter/blur commit and Esc cancel are all expressible.
- gpui elements support a native `.tooltip(...)` (used by gpui-component), so
  the identifier row can carry the full name as a tooltip.
- The Clocks failure at 23:00 Asia/Shanghai is a date-zone bug, not an
  assertion bug: Tokyo is already on the next civil day, so the "today"
  window built from the process-local date has already ended and
  `end_travel`/`relocate_person` see an expired trip.

## 3. Data layer — `crates/app-gpui/src/store.rs`

Add thin wrappers, all through `kandrium_core::projects` (no SQL in the app):

```rust
pub fn projects_root(&self) -> StoreResult<PathBuf>;                  // ensure_root
pub fn project_tree(&self) -> StoreResult<Vec<ProjectNode>>;          // list_tree
pub fn project(&self, id: &str) -> StoreResult<Project>;              // get_project
pub fn project_summary(&self, root: &Path, id: &str) -> StoreResult<ProjectSummary>;
pub fn project_files(&self, root: &Path, id: &str) -> StoreResult<Vec<FsEntry>>;
pub fn archived_projects(&self) -> StoreResult<Vec<Project>>;
pub fn create_project(&self, root: &Path, name: &str, folder: Option<&str>)
    -> StoreResult<Project>;                                           // template "blank"
pub fn create_project_folder(&self, name: &str, parent: Option<&str>)
    -> StoreResult<ProjectFolder>;
pub fn rename_project_folder(&self, id: &str, name: &str) -> StoreResult<ProjectFolder>;
pub fn delete_project_folder(&self, root: &Path, id: &str) -> StoreResult<()>;
pub fn rename_project(&self, id: &str, name: &str) -> StoreResult<Project>;
pub fn update_project(&self, id: &str, description: Option<&str>, status: Option<&str>)
    -> StoreResult<Project>;
pub fn set_project_source(&self, id: &str, source: &str) -> StoreResult<Project>;
pub fn set_project_identifier(&self, id: &str, identifier: &str) -> StoreResult<Project>;
pub fn archive_project(&self, id: &str) -> StoreResult<()>;
pub fn unarchive_project(&self, id: &str) -> StoreResult<()>;
pub fn delete_project(&self, root: &Path, id: &str) -> StoreResult<()>;
pub fn reorder_project(&self, id: &str, index: usize) -> StoreResult<()>;
pub fn read_project_doc(&self, root: &Path, id: &str, rel: &Path) -> StoreResult<String>;
pub fn write_project_doc(&self, root: &Path, id: &str, rel: &Path, text: &str) -> StoreResult<()>;
pub fn create_project_doc(&self, root: &Path, id: &str, parent_rel: &Path, name: &str)
    -> StoreResult<PathBuf>;
pub fn create_project_doc_folder(&self, root: &Path, id: &str, parent_rel: &Path, name: &str)
    -> StoreResult<PathBuf>;
```

Writes use the existing writable handle; reads use the read-only handle.
`update_project` is called once per Save (description and/or status).

## 4. Projects view — `crates/app-gpui/src/views/projects.rs`

### 4.1 State

```rust
pub const AUTOSAVE: Duration = Duration::from_millis(400);
pub const RAIL_W: f32 = 30.0;
pub const TREE_W: f32 = 240.0;

pub struct ProjectDocTab {
    pub rel: PathBuf,                 // project-relative
    pub name: String,
    pub editor: Entity<TextareaState>,
    pub vim: VimSession,
    pub preview: bool,
    pub saved: String,
    pub dirty: bool,
    pub generation: u64,
}

pub enum ProjectTab { Overview, Tasks, Doc(ProjectDocTab) }

pub struct ProjectsState {
    pub root: Option<PathBuf>,
    pub tree: Vec<ProjectNode>,
    pub expanded: HashSet<String>,        // folder ids
    pub selected: Option<String>,         // project id
    pub selected_folder: Option<String>,  // inline-create parent (None = root)
    pub tabs: Vec<ProjectTab>,            // [Overview, Tasks, docs...]
    pub active: usize,
    pub tabs_project: Option<String>,     // tab owner; selection change rebuilds
    pub detail: Option<Project>,
    pub summary: Option<ProjectSummary>,
    pub files: Vec<FsEntry>,
    pub filter: String,
    pub filter_input: Option<Entity<InputState>>,
    pub show_archived: bool,
    pub archived: Vec<Project>,
    pub tree_open: bool,
    pub tree_w: f32,
    pub error: Option<String>,
    pub draft: Option<ProjectDraft>,
    pub confirm: Option<ProjectConfirm>,
    pub title_edit: Option<TitleEdit>,
    pub detail_editor: Option<DetailEditor>,  // identifier/source/description inputs
    pub drag: Option<ProjectDragState>,
}
```

- `ProjectDraft { kind: ProjectDraftKind, target_folder: Option<String>, project_id: Option<String>, input }`
  where `kind ∈ {Project, Folder, RenameFolder, RenameProject}`.
- `ProjectConfirm { kind: DeleteProject | DeleteFolder | Archive, id, name }`.
- `TitleEdit { project_id, input: Entity<InputState> }`.
- `DetailEditor { project_id, identifier: Entity<InputState>, source: Entity<InputState>,
  description: Entity<TextareaState>, description_vim: VimSession, saved_description: String }`.
- `ProjectDragState { id, from_folder: Option<String>, hover: Option<(String, bool)> }`
  (`bool` = insert after the hovered sibling).

Pure helpers (unit-tested):
- `project_matches(project, filter) -> bool` — case-insensitive substring of
  identifier or name; empty filter matches.
- `visible_tree(nodes, filter) -> Vec<VisibleNode>` — prunes folders with no
  matching descendants; when filtering, matching projects render at their
  normal depth and folders stay expanded.
- `insert_index(siblings_without_dragged, target, after) -> usize`.
- `close_index(tabs_len, active, closed) -> Option<usize>` — pinned tabs are
  never closed.

### 4.2 Tree panel

- Header: Projects icon, title, collapse, New project, New folder, Archived
  toggle.
- Filter `InputState`, created once; a Change subscription copies the value
  into `state.filter` and notifies.
- Rows:
  - folder: chevron, folder icon, name, rename/delete hover actions; click
    toggles expansion and sets `selected_folder`;
  - project: chevron (unused for docs), project icon, **identifier** label
    with `.tooltip(project.name.clone())`, full name as a faint suffix only
    when the filter is non-empty. Click selects the project.
- Folder rows always precede project rows because `list_tree` assembles in
  that order; the renderer never reorders across tiers.
- Drag: project rows get `.on_drag(ProjectDrag{id, from_folder}, …)` only when
  `filter.trim().is_empty()` (and not archived). Each project row has
  `.on_drag_move::<ProjectDrag>` computing above/below from the row midpoint;
  only a row whose `folder_id` equals the dragged project's parent is a valid
  target. `.on_drop` calls `AppShell::drop_project`, which computes the
  post-removal insert index with `insert_index` and calls
  `store.reorder_project`. A 2 px accent rule marks the insertion slot.
- Collapsed rail: same pattern as Notes/Documents (`projects-tree-rail`,
  `projects-tree-splitter`, width persisted by the shell).
- Archived mode: when `show_archived`, the rows are the archived projects
  (newest first); clicking one selects it and opens Overview with Restore /
  Delete actions instead of Archive / Delete.

### 4.3 Detail pane

- Tab strip: Overview, Tasks (both pinned, no close button), then
  `Doc` tabs with name, dirty dot and X.
- `active_is_tasks()` drives the shell's board indirection.
- Empty selection: placeholder message.
- Overview body:
  - Header: title. Single click selects; double-click starts `TitleEdit`.
    `InputState` is focused on creation; an `InputEvent` subscription commits
    on `PressEnter` and `Blur`, a capture handler on the header cancels on
    Escape.
  - Status chip (active / on hold / done) as three selectable chips calling
    `store.update_project(None, Some(status))`.
  - Identifier input + Save: `store.set_project_identifier` (locks the
    identifier); inline error on invalid/duplicate.
  - Source input + Save: `store.set_project_source`.
  - Summary line: todo / in progress / done / total / files / updated.
  - Description: fixed-height `Textarea` with the shared vim session and mode
    strip (identical contract to Notes) plus a Save button →
    `store.update_project(Some(text), None)`.
  - Archive / Restore and Delete actions. Delete opens the inline confirm bar
    (“Delete "name"? This cannot be undone.” with Cancel/Delete); Archive is
    immediate, matching the egui menu item.
  - Documents list: `project_files` tree, directories first, with Open on
    files (`.md`/`.markdown`/`.txt`), per-folder New file / New folder, and a
    header New file / New folder. Inline name draft, Enter commits, Esc
    cancels. Non-text files are listed but not openable.
- Doc tab body: mode strip (vim badge/toggle), preview toggle, editor. Vim,
  autosave, flush-on-close and per-tab mode follow the Notes implementation
  exactly (observer → generation → 400 ms debounce → `write_project_doc`;
  close and project/section switches flush first).
- Tasks body: `tasks::board_view(board, drop_target, dragging_card, t, cx)`
  with the project's board; no tab strip and no second renderer.

### 4.4 Shell wiring — `crates/app-gpui/src/app.rs`

- New fields: `projects: ProjectsState`, `project_board_shown: Option<String>`.
- `with_store`: seed `projects.tree_open/tree_w` from settings; when the saved
  section is Projects, call `load_projects()`.
- `persist_settings`: add `TreePanel::Projects` open/width.
- `select`: before leaving Projects, `flush_project_docs`; on entering,
  `load_projects()`. Leaving Projects clears `project_board_shown` (the Tasks
  reload restores its own board).
- `load_board` keeps setting `active_board_id`; add
  `load_project_board(id)` that loads into `board` without touching
  `active_board_id`. `sync_project_board` runs at the top of the Projects
  body: when the active tab is Tasks and a project is selected, load that
  board once; when it is not, clear `project_board_shown` without reloading.
- `reload_displayed_board`: project board when shown, otherwise
  `active_board_id`. `drop_card` and `persist_editing` use it.
- Handlers (all `pub(crate)`): `load_projects`, `reload_project_detail`,
  `select_project`, `toggle_projects_tree`, `set_projects_tree_width`,
  `begin_project_draft`, `begin_project_folder_rename`,
  `handle_project_draft_key`, `commit_project_draft`, `cancel_project_draft`,
  `begin_project_title_edit`, `commit_project_title`,
  `cancel_project_title`, `save_project_details`,
  `set_project_status`, `archive_selected_project`, `unarchive_project`,
  `ask_delete_project`, `ask_delete_folder`, `cancel_project_confirm`,
  `confirm_project_delete`, `activate_project_tab`, `open_project_doc`,
  `close_project_doc`, `project_doc_edited`, `save_project_doc`,
  `flush_project_docs`, `handle_project_doc_key`, `hover_project_drop`,
  `drop_project`.
- Watcher: Projects joins `wants_watcher`. `apply_external_change` on Projects
  calls `load_projects` only when
  `projects.drag/draft/title_edit/confirm` are all `None`; otherwise it sets
  `pending_sync`. Reloads never rebuild tab editors or a dirty description.
- `body`: `Section::Projects` → `sync_project_board`, then
  `projects::projects_view(&mut self.projects, displayed_board, …)`.
- Section-switch flush: Notes/Documents/Projects each flush their dirty
  buffers before the section changes.

## 5. Clocks test fix — `crates/app-gpui/tests/clocks.rs`

Add:

```rust
/// The civil date at `now` in `zone`. Using the process-local date is wrong
/// when the destination is already on the next day (23:00 Asia/Shanghai is
/// 00:00 Asia/Tokyo): the "today" window would be over before the trip ends.
fn today_in(zone: &str) -> jiff::civil::Date {
    let zone = resolve_zone(zone).expect("known zone");
    jiff::Timestamp::now().to_zoned(zone).date()
}
```

- Test 1: `let today = today_in(&destination.tz_identifier);` (both windows
  keep using `day_window(&destination.tz_identifier, …)`).
- Test 2: `let today = today_in(&new_home.tz_identifier);`. The unrelated
  trip's date is derived explicitly after the new-home window ends:
  `Timestamp::from_second(ends_at + 1)`, converted into the unrelated zone,
  then `day_window(&elsewhere.tz_identifier, that_date)`. This guarantees
  `starts_at > ends_at` in every zone combination.
- Assertions are unchanged. Add comments stating why the date is taken in the
  destination zone.
- Audit: grep both workspaces for `Zoned::now`, `Timestamp::now(...).date` and
  `today()` inside tests; fix any other test whose window depends on the
  process-local date. Only test code changes.

## 6. Verification

1. `cargo fmt` and `cargo fmt --manifest-path crates/app-gpui/Cargo.toml`.
2. `cargo clippy --workspace --all-targets` and
   `cargo clippy --manifest-path crates/app-gpui/Cargo.toml --all-targets`;
   clean except the known `vim.rs:2436`.
3. `cargo test --workspace` (baseline 279) and
   `cargo test --manifest-path crates/app-gpui/Cargo.toml` (baseline 83 with
   the two Clocks failures; target: all pass plus the new tests).
4. Rebuild and relaunch:
   `killall kandrium-gpui; open -n crates/app-gpui/target/KandriumGpui-dev.app`
   (unsandboxed).
5. Capture test counts before/after and commit hashes for the report.

## 7. Deliberate parity differences

- Project documents render in the Overview panel, not inline under each
  project in the left tree (the tree keeps folders + projects). Opening,
  editing, autosave, vim and tabs are unchanged.
- Project creation uses the `blank` template; the egui template picker is not
  reproduced (templates are a create-time nicety, not data).
- Non-text project files are listed but not opened; PDF viewing for project
  documents stays a Documents-section feature, matching the egui project
  overview's external-open behaviour.
- Archived projects appear behind an Archived toggle in the tree rather than
  a separate panel; actions (Restore / Delete) are in the Overview.

## 8. On-screen checks for the user

1. Create a folder and a project inside it; the tree shows folders above
   projects, the project label is its identifier, and hovering shows the full
   name.
2. Type in the filter: only identifier/name matches remain; drag handles do
   nothing while the filter has text.
3. Drag a project above/below a sibling: order changes and survives a
   relaunch; dragging near a project in another folder is refused.
4. Double-click the Overview title, rename, press Enter; the tree label and
   the project's board title update. Repeat with Esc (no change) and with a
   click elsewhere (commits). Change the identifier, Save, rename again: the
   identifier stays.
5. Open the Tasks tab inside a project: the normal board appears (columns,
   cards, drag and drop, card modal) and edits persist. Switch to another
   project: Tasks stays selected and shows that project's board.
6. Open a `.md` doc from the Overview: edit with vim, watch autosave, close
   the tab (content saved). Reopen after a relaunch.
7. Archive the project: it leaves the active tree; the Archived toggle lists
   it and Restore brings it back. Delete another project: the inline
   confirmation appears and the folder goes away.
8. With Projects open, edit a project via the MCP server or a second
   process: the tree/overview refresh without closing an in-progress rename,
   a draft, or a dirty doc tab.

## 9. Remaining before cutover (report material)

- Port/retire the remaining egui test suites and the shared visual checks.
- Documentation: README/user docs still describe the egui app as primary.
- Packaging unification: two `packager.json` configs and icon copies; one
  version source and a shared icon set.
- Any egui-only features not yet audited after each gpui section (PDF
  annotations parity, card modal pickers, Clocks inspector details).
- Final decision to remove or freeze `crates/app` and the legacy import path.
