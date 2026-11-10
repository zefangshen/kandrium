# Stage 5 — Documents on gpui, with the PDF renderer extracted

Status: awaiting approval. Branch: `gpui-migration`. Working tree verified
clean (tip `273fcfe`, Stage 4 accepted).

## 1. Goal and scope

Port the egui Documents section to the gpui shell with behavioural parity, and
extract the PDF renderer into a shared library crate so both front-ends use one
implementation. The caller lifted the old "no `crates/app` changes" rule *only*
for that extraction; the egui app must behave identically and keep its tests
green.

In scope, matching `crates/app/src/views/documents.rs` today:

- Tree over the documents root (`KANDRIUM_DOCS`): folders first, PDFs and
  Markdown listed; upload through the native picker into the selected folder
  with the `-2` unique-name rule and no sidecars; new Markdown file and new
  folder with inline naming; rename/delete; collapse-to-rail tree.
- Tabs for opened documents, persisted as root-relative paths.
- PDF viewing: page rendering off the UI thread, zoom, pan/scroll, page
  navigation, annotation sidecar read/write through core with atomic writes.
  Selection stays visual-only, exactly as egui.
- Markdown tabs: shared markdown preview, per-tab vim, debounced autosave.

Out of scope: Projects, any `migrations/`, the MCP surface, any new data
logic in a front-end crate.

Constraints (unchanged):

- Data logic only in `kandrium-core`; the gpui crate formats and renders.
- `cargo fmt`, `cargo clippy --workspace --all-targets`, `cargo test
  --workspace`, plus the gpui crate's clippy/tests. Leave
  `crates/app/src/widgets/vim.rs:2436` alone.
- Commits on `gpui-migration`: author `openspex <zefang.sh@gmail.com>`,
  committer env `GIT_COMMITTER_NAME=zefangshen
  GIT_COMMITTER_EMAIL=zefang.sh@gmail.com`, no `Co-Authored-By`, no assistant
  mentions, never `git add -A`, do not push. Commit incrementally.
- Report when done + app relaunched: end-to-end flows, renderer-extraction
  impact on egui, test counts, commit hashes, parity dropped, on-screen checks.

## 2. Findings that drive the design

### 2.1 Core is complete — no core gap

`kandrium_core::documents` already exposes everything:

- Root: `resolve_documents_root`, `ensure_root`.
- Tree/types: `scan_tree`, `DocEntry` (= `FsEntry`), `is_pdf`, `is_markdown`.
- Markdown: `read_text`, `write_text` (atomic inside core).
- Create: `create_folder`, `create_markdown` (bare name gains `.md`).
- Import: `import_file` (`-2`, `-3` … before the extension, sidecars *not*
  copied).
- Rename/delete: `rename_path` (moves an existing PDF sidecar), `delete_path`
  (removes a PDF sidecar, deletes folders recursively, refuses the root).
- Annotations: `AnnotationDoc`, `DocAnnotation`, `AnnotationKind`,
  `new_annotation`, `annotations_path`, `load_annotations`, `save_annotations`
  (atomic; empty doc removes the sidecar; foreign version errors).

### 2.2 The renderer is extractable

`crates/app/src/pdf.rs` depends on:

- `egui::Context` **only** to call `request_repaint()` after each worker event.
- one self-reference, `crate::pdf::rects_intersect`, inside `PageText`.

Everything else is plain Rust (`hayro`, `kurbo`). Replacing the `egui::Context`
with a `wake` callback and the self-reference with a local call makes the module
UI-agnostic with zero behaviour change.

### 2.3 The gpui shell

- `AppShell` (`crates/app-gpui/src/app.rs`) owns per-section state; `select`
  loads the section; `body` matches `Section`; Notes already provides the exact
  patterns to copy (tree + rail, inline `NameDraft` with `InputState`,
  `capture_key_down` for Enter/Esc, `TextareaState` + `VimSession`, `cx.observe`
  edit tracking, 400 ms generation-guarded autosave via `cx.spawn` +
  `background_executor().timer`).
- `Store` (`crates/app-gpui/src/store.rs`) forwards core calls; Notes wrappers
  are the template.
- gpui has no cross-thread repaint. The PDF worker therefore gets a no-op wake
  hook and the shell repaints while a render/text request is outstanding
  (`page_count == 0`, pending set non-empty, or a worker request in flight).
  Effect is the same as egui's `request_repaint()`.
- gpui has no eframe `SavedState`. Tabs persist through a tiny explicit session
  file (plain text, written via `core::fsutil::write_atomic`).
- `ScrollHandle::scroll_to_top_of_item(ix)` is public, so page navigation can
  jump to the page's child index in the scroll container, matching egui's
  `scroll_to_rect(rect, Align::Min)`.
- `RenderImage` must be BGRA: take the worker's straight RGBA8, swap bytes 0
  and 2 (spike-verified), then `image::Frame::new(RgbaImage::from_raw(..))`.
  There is no transform/scale style, so zoom is page *sizing* (render scale =
  fit × zoom and image width/height = page size × that scale), not scaling.

## 3. `crates/pdf-render` — the extracted crate

New workspace member `crates/pdf-render` (package `pdf-render`, lib
`pdf_render`, edition 2024, `publish = false`, license Apache-2.0).

`crates/pdf-render/Cargo.toml`

```toml
[package]
name = "pdf-render"
version = "0.1.0"
edition = "2024"
license = "Apache-2.0"
description = "PDF rasterization and text extraction for Kandrium"
publish = false

[dependencies]
hayro = { workspace = true }
kurbo = "0.13"
```

`crates/pdf-render/src/lib.rs` — moved verbatim from `crates/app/src/pdf.rs`:

- `TextGlyph`, `TextLine`, `PageText` (`line_glyphs`, `selection_quads`,
  `selection_text`), `approx_quad_eq`, `rects_intersect`, `quad_from_rect`,
  `group_lines`, `GlyphCollector`, `extract_text`, `Request`, `Event`,
  `PdfWorker`, `worker_loop`, `render_page`.
- The four existing tests (`worker_renders_and_extracts_real_pdf`,
  `group_lines_clusters_by_baseline`, `selection_quads_merges_per_line`,
  `rects_intersect_detects_overlap`) and the `minimal_text_pdf` helper.
- The only behavioural edits:
  - `PageText` calls the local `rects_intersect` instead of
    `crate::pdf::rects_intersect`.
  - `pub fn open(path: PathBuf, wake: impl Fn() + Send + Sync + 'static) ->
    (Self, Receiver<Event>)`; the worker stores
    `Arc<dyn Fn() + Send + Sync>` and calls it where it used to call
    `repaint.request_repaint()`.
- Module doc updated: the worker owns the parsed `Pdf` and `RenderCache` on one
  thread; the wake hook is the only UI coupling.

Wiring:

- Root `Cargo.toml`: add `crates/pdf-render` to `members`.
- `crates/app/src/pdf.rs` becomes a re-export shim with the extraction note:
  `pub use pdf_render::{Event, PageText, PdfWorker, Request, TextGlyph,
  TextLine, group_lines, rects_intersect};`
- `crates/app/src/views/documents.rs`: the single `PdfWorker::open(path,
  repaint)` call becomes `PdfWorker::open(path, move || repaint.request_repaint())`.
  No other change.
- `crates/app/Cargo.toml`: add `pdf-render = { path = "../pdf-render" }`, drop
  `hayro` and `kurbo` (no other use in the crate).
- `crates/app-gpui/Cargo.toml`: add `pdf-render = { path = "../pdf-render" }`
  and `rfd = "0.15"`; drop `egui`, `hayro`, `kurbo`; keep `image` and
  `smallvec` (needed to build `RenderImage`).
- `crates/app-gpui/src/main.rs`: drop the `#[path = "../../app/src/pdf.rs"]
  mod pdf;` block. `pdf_probe.rs` imports `pdf_render::{Event, PdfWorker,
  Request}` and passes `|| {}` as the wake hook.

Expected gate effect: the four moved tests leave `kandrium-app` and appear in
`pdf-render`, so the workspace total stays 279.

## 4. gpui Documents architecture

New files:

- `crates/app-gpui/src/views/documents.rs` — state, tree, tabs, Markdown
  content, pure helpers, session codec.
- `crates/app-gpui/src/views/documents_pdf.rs` — PDF toolbar, page stack,
  image cache, overlays, annotation popover, pointer math.
- `crates/app-gpui/src/views/mod.rs`: add `pub mod documents; pub mod
  documents_pdf;`.
- `crates/app-gpui/tests/documents.rs` — headless tests.

### 4.1 `Store` additions (`crates/app-gpui/src/store.rs`)

Thin forwards, each mapping `KandriumError` with `e.to_string()`:

```rust
pub fn documents_root(&self) -> StoreResult<PathBuf>;   // ensure_root + canonicalize
pub fn documents_tree(&self, root: &Path) -> StoreResult<Vec<DocEntry>>;
pub fn read_document(&self, path: &Path, root: &Path) -> StoreResult<String>;
pub fn write_document(&self, path: &Path, root: &Path, text: &str) -> StoreResult<()>;
pub fn create_markdown(&self, parent: &Path, root: &Path, name: &str) -> StoreResult<PathBuf>;
pub fn create_doc_folder(&self, parent: &Path, root: &Path, name: &str) -> StoreResult<PathBuf>;
pub fn import_documents(&self, sources: &[PathBuf], dest: &Path, root: &Path)
    -> StoreResult<Vec<(PathBuf, Option<String>)>>;     // per-file errors, never aborts the batch
pub fn rename_document(&self, path: &Path, root: &Path, name: &str) -> StoreResult<PathBuf>;
pub fn delete_document(&self, path: &Path, root: &Path) -> StoreResult<()>;
pub fn load_annotations(&self, pdf: &Path, root: &Path) -> StoreResult<AnnotationDoc>;
pub fn save_annotations(&self, pdf: &Path, root: &Path, doc: &AnnotationDoc) -> StoreResult<()>;
```

`import_documents` loops `documents::import_file`, collecting
`(source, Option<error>)` so the UI can report partial failures like egui does.

### 4.2 State (`views/documents.rs`)

```rust
pub enum DocKind { Pdf, Markdown }

pub struct DocTab {
    pub kind: DocKind,
    pub path: PathBuf,
    pub name: String,
    // Markdown
    pub editor: Option<Entity<TextareaState>>,   // Markdown only
    pub vim: Option<VimSession>,                 // per tab, Markdown only
    pub preview: bool,
    pub saved: String,
    pub dirty: bool,
    pub generation: u64,
    // PDF
    pub worker: Option<PdfWorker>,
    pub events: Option<Receiver<pdf_render::Event>>,
    pub page_count: usize,
    pub error: Option<String>,
    pub zoom: f32,                               // multiplier over fit-width
    pub page_size: Option<(f32, f32)>,           // points, from the first render
    pub images: HashMap<(usize, u32), Arc<RenderImage>>,
    pub stamps: HashMap<(usize, u32), u64>,
    pub pending: HashSet<(usize, u32)>,
    pub page_texts: HashMap<usize, PageText>,
    pub text_requested: HashSet<usize>,
    pub annotations: Vec<DocAnnotation>,
    pub ann_dirty: bool,
    pub ann_generation: u64,
    pub selection: Option<Selection>,
    pub popover: Option<String>,
    pub visible_page: usize,
    pub jump_to: Option<usize>,
    pub page_bounds: HashMap<usize, Bounds<Pixels>>,
}

pub enum DocDraftKind { File, Folder, Rename }
pub struct DocDraft { kind, target: PathBuf, input: Entity<InputState> }

pub struct DocumentsState {
    pub root: Option<PathBuf>,
    pub entries: Vec<DocEntry>,
    pub expanded: HashSet<PathBuf>,
    pub selected: Option<PathBuf>,
    pub tabs: Vec<DocTab>,
    pub active: Option<usize>,
    pub tree_open: bool,
    pub error: Option<String>,
    pub draft: Option<DocDraft>,
    pub confirm: Option<PathBuf>,
    pub tool: Tool,                 // Select | Highlight | Note
    pub session_loaded: bool,
}
```

Constants mirror egui: `TREE_W = 240`, `RAIL_W = 30`, `ROW_H = 26`,
`INDENT = 14`, `PAGE_MARGIN = 8`, `ZOOM_STEP = 1.25`, `ZOOM_MIN = 0.25`,
`ZOOM_MAX = 4.0`, `TEXTURE_CACHE_CAP = 24`, `NOTE_ANCHOR = 20`,
`HIGHLIGHT_ALPHA = 90`, `AUTOSAVE = 400 ms` (Markdown),
`ANNOTATION_AUTOSAVE = 500 ms`, `NOTE_COLORS` identical.

Pure helpers, unit-tested in-file:

- `screen_to_page(origin, scale, point) -> [f32; 2]`,
  `page_to_screen(origin, scale, rect) -> Bounds<Pixels>`,
  `permille(scale)`, `hex_rgb`, `Selection::rect`.
- `note_anchor_quad`, `visible_pages(top, viewport_h, page_h, count)` (the
  ±0.75-page pre-render window egui uses).
- `format_session(tree_open, active, rel_paths) -> String` and
  `parse_session(&str) -> (bool, Option<usize>, Vec<String>)` — plain text:
  line 1 `tree_open=0|1`, line 2 `active=<n>`, then one root-relative path
  per line. Unknown/garbage lines are ignored.
- `rel_path(root, path)` and `abs_path(root, rel)`.

### 4.3 Session persistence

File: `resolve_db_path().parent().join("gpui-documents.session")` (next to the
database, so the `KANDRIUM_DB` override isolates tests automatically). Written
with `core::fsutil::write_atomic` after every tab/open/close/rename/delete and
tree toggle. Read once on the first `load_documents()`; restores tabs by
`abs_path` + `open_document` only for paths that still exist, then clamps the
active index. PDFs restored this way spawn fresh workers; Markdown tabs reload
from disk.

### 4.4 `AppShell` methods (`app.rs`)

Following the Notes implementations one-for-one:

- `load_documents()` — `store.documents_root()`, `store.documents_tree`,
  first-run session restore.
- `toggle_documents_tree`, `toggle_doc_folder`, `select_doc_folder`.
- `open_document(path, window, cx)` — reuse tab; PDF: `PdfWorker::open(path,
  || {})`, load annotations via `Store`, push tab; Markdown: `read_document`,
  new `TextareaState`, `VimSession`, `cx.observe` → `doc_tab_edited`.
- `activate_doc_tab(path)`, `close_doc_tab(path, cx)` (flush both kinds, keep
  the tab when a save fails, exactly egui).
- `doc_tab_edited(editor, cx)` (dirty + generation + 400 ms debounce),
  `save_document_tab(path, cx)` (Markdown write or annotation sidecar),
  `flush_documents(cx)`, `flush_annotations(path, cx)`.
- `begin_doc_draft(kind, parent, window, cx)`, `begin_doc_rename`,
  `handle_doc_draft_key` (Enter commit, Esc cancel), `commit_doc_draft`
  (folder → select + expand; file → open the new tab; error keeps the row open).
- `ask_delete_document`, `cancel_delete_document`,
  `confirm_delete_document` (drop tabs under the path without flushing,
  rescan, rewrite session).
- `pick_and_import_documents` (`rfd::FileDialog` filter pdf/md/markdown,
  multi-select) → `import_documents` → expand destination, rescan, surface
  per-file errors.
- `handle_documents_key` — vim for the active Markdown tab, `:w` saves, `:q`
  closes, same as Notes.
- `pump_documents(cx)` — called at the top of `body()` when
  `section == Documents`; drains every tab's receiver and applies
  `Opened`/`OpenFailed`/`Rendered`/`Text`/`Failed` (BGRA conversion, page size
  from the first render, LRU eviction at 24), then `cx.notify()` while work is
  outstanding (initial parse, non-empty `pending`, or a request just sent).
- Viewer mutations: `set_doc_zoom(path, zoom)`, `reset_doc_zoom`,
  `jump_doc_page(path, delta)`, `set_doc_tool(tool)`, `open_annotation(path,
  id)`, `close_annotation_popover`, `set_annotation_text`, `set_annotation_color`,
  `delete_annotation`, `mark_annotations_dirty(path, cx)` (500 ms debounce,
  generation-guarded).
- `select` arms: on entering Documents call `load_documents()`; on leaving
  call `flush_documents()` and `save_documents_session()`.

### 4.5 Tree and tabs (`views/documents.rs`)

- Panel/rail identical to Notes (`TREE_W`/`RAIL_W`, `documents-tree`,
  `documents-tree-rail`, expand/collapse ids).
- Header: collapse, new folder, new file, upload — same icon buttons as Notes
  (`FolderPlus`, `FilePlus`, plus a new `Icon::Upload` mapped to
  `app/assets/icons/upload.svg`).
- Rows: folders first (chevron + folder icon, click toggles + selects), files
  (`FileText` for Markdown, `File` — add the icon if absent — for PDFs), active
  tab highlighted. Hover actions on every row: rename (Pencil), delete
  (Trash2); folders additionally get new-file / new-folder. This replaces
  egui's row context menu (gpui has no built-in context menu in this kit);
  reported as a UI substitution, all actions preserved.
- Inline create row at the top of the target folder (`documents-draft`,
  `InputState`), placeholder `name.md` / `Folder name`; Enter commits, Esc
  cancels, invalid names keep the row and show the core message.
- Inline rename replaces the row label with an `InputState` seeded with the
  name; Enter commits, Esc cancels; success remaps tabs, selection and expanded
  set through `remap_path`, rescans, rewrites the session.
- Delete is an inline confirmation card under the tab strip (Notes pattern):
  `documents-delete-confirm`, Cancel, Delete; the PDF sidecar disappears
  because core's `delete_path` removes it.
- Tabs: one per open document, name = file name, dirty dot for Markdown and
  annotation changes, X to close, whole strip horizontally scrollable.
- Markdown content: mode strip with Edit/Preview, vim badge + pending/ex
  line/hint (`doc-mode`), `Textarea` bound to the tab's editor, readonly
  outside insert mode, `capture_key_down` routed to `handle_documents_key`;
  preview renders `gpui_kit::base::text::markdown`.
- Empty state: `placeholder::message` "No document open".

### 4.6 PDF viewer (`views/documents_pdf.rs`)

- Toolbar: previous/next page (disabled until `page_count > 0`), `n / total`,
  zoom out / percent (click resets to 100 %) / zoom in, and the three tool
  toggles (Select, Highlight, Note) with active tint.
- Page stack in one `.overflow_scroll().track_scroll(&handle)` container;
  each page is a direct child row (`page_bounds` recorded via `canvas`), with
  a white page rect, hairline border, `Rendering…` placeholder until the image
  arrives, and the `img(Arc<RenderImage>).w(..).h(..)` on top.
- Fit width = `(viewport − inset − 2·margin) / page_width`; render scale =
  fit × zoom; pages are sized at `page_size × scale` (zoom is sizing).
- Requests: for visible pages (and one page either side), send
  `Request::Render { page, scale }` when `(page, permille(scale))` is absent and
  not pending, and `Request::Text { page }` once; a sent request also triggers
  a repaint (see §4.4).
- Page navigation: `handle.scroll_to_top_of_item(page)` on the pending
  `jump_to`; `handle.top_item()` feeds `visible_page` for the toolbar label.
- Overlays, in page view space transformed by `× scale + origin`:
  - live selection rect (accent at 25 %),
  - highlights as translucent fills at `HIGHLIGHT_ALPHA`,
  - note anchors as a 20 pt colored square with a sticky-note icon and border.
  - Rotated text is highlighted by its axis-aligned box (gpui has no polygon
    primitive); egui uses convex polygons. Reported as the one rendering
    difference.
- Interaction: pointer down/move/up on the page row converts window coords via
  the recorded `page_bounds`:
  - Select tool: drag updates `selection`, on release it is cleared (visual
    only, as egui).
  - Highlight tool: on release, `page_text.selection_quads(&rect)` +
    `selection_text` create a `new_annotation(Highlight, …)` (tiny drags
    ignored, memo = selected text), and its popover opens.
  - Note tool: click inside the page creates a `new_annotation(Note, …)` with
    `note_anchor_quad` and opens its popover.
  - Clicking an existing annotation toggles its popover (per-quad hit test).
- Popover: absolutely positioned card anchored at the annotation's top-left
  (clamped to the viewport), memo `Textarea`, four color swatches, delete, close.
  Every edit bumps `modified_at`, marks annotations dirty and debounces a save.
- Errors (`OpenFailed`, `Failed`) render centred like egui's; `Loading…` until
  the first `Opened`.

## 5. Parity: what changes, what is dropped

Kept: root/`KANDRIUM_DOCS`, folder-first order, `-2` imports with no sidecars,
Markdown-only creation, inline naming, rename moving sidecars, delete removing
them, root-relative tab session, collapse-to-rail, per-tab mode, vim, 400 ms
autosave, PDF page rendering off-thread, zoom clamp 25–400 %, page jump,
annotations (highlight/note, colors, memo, delete) with 500 ms atomic sidecar
writes, visual-only selection.

Deliberate differences (all to be listed in the final report):

1. Row context menu → per-row hover actions (same actions).
2. Highlight overlays are axis-aligned boxes, not convex polygons (affects
   rotated text only).
3. No cross-thread wake: the shell repaints while a PDF request is outstanding.
4. Tab session is a small text file next to the DB, not eframe `SavedState`.
5. Tree panel is fixed-width (Notes precedent), not drag-resizable.
6. gpui's own scrollbars replace egui's floating style inside the viewer.

## 6. Tests and gates

New `crates/app-gpui/tests/documents.rs` (headless `gpui::test`,
`TestAppContext`, seeded temp DB, `AppShell::with_store`, `KANDRIUM_DOCS` via a
temporary root passed to the state constructors where possible):

1. `selects_the_documents_section_and_lists_the_tree` — click nav, folders
   before files, PDF + Markdown visible.
2. `creates_a_markdown_file_and_opens_its_tab`.
3. `imports_copy_with_the_unique_name_rule_and_report_partial_failures`.
4. `renames_a_pdf_and_its_sidecar_follows` (store-level + tab remap).
5. `deleting_a_document_drops_its_tabs_and_sidecar`.
6. `markdown_autosave_debounces_and_writes_once`.
7. `vim_ex_close_saves_and_closes`.
8. `annotation_round_trip_and_empty_document_removes_the_sidecar`.
9. `session_round_trip_is_root_relative` (pure codec + restore drops missing
   files).
10. `tree_collapses_to_a_rail_and_expands_back`.
11. Page-geometry unit tests in `views/documents.rs`
    (`screen_to_page`/`page_to_screen` round-trip, `note_anchor_quad`,
    `hex_rgb`, `Selection::rect`, `visible_pages`, session codec).

Gates, in this order: `cargo fmt --all`, `cargo fmt --manifest-path
crates/app-gpui/Cargo.toml`, `cargo clippy --workspace --all-targets`,
`cargo clippy --manifest-path crates/app-gpui/Cargo.toml --all-targets`,
`cargo test --workspace` (279 + the 4 moved = expected 279 total, unchanged),
`cargo test --manifest-path crates/app-gpui/Cargo.toml` (56 + new). Only the
pre-existing `vim.rs:2436` warning may remain.

## 7. Commits (incremental, in this order)

1. `Extract the PDF renderer into a shared pdf-render crate` — new crate, app
   shim + call-site, gpui dependency/probe wiring; all gates. This commit is
   self-contained so the egui app is never left broken.
2. `Add the Documents store bridge and the tab session codec`.
3. `Port the Documents tree, tabs and Markdown editor to gpui` (tree, rail,
   inline create/rename/delete, upload, Markdown tabs with vim/autosave, session
   restore, section wiring).
4. `Add PDF viewing with zoom, page navigation and annotation sidecars`.
5. `Cover the Documents flows with headless tests` (tests + final gates).

Relaunch after commit 5: `killall kandrium-gpui; open -n
crates/app-gpui/target/KandriumGpui-dev.app`.

## 8. On-screen checks requested from the user

1. Upload a PDF and a Markdown file into a subfolder; upload the same PDF again
   and confirm `report-2.pdf` appears (and no `*.kandrium.json` was copied).
2. Create a folder and a Markdown file with inline naming; bad names keep the
   row open with the core message.
3. Open a PDF: page renders, zoom in/out and the percent reset, previous/next
   page jumps, horizontal scrolling appears when zoomed past the viewport.
4. Highlight a text passage (Highlight tool) and place a sticky note (Note
   tool); change color/memo, delete one, then relaunch and confirm the sidecar
   restored them (and that deleting the PDF removes the sidecar).
5. Rename a PDF in the tree; confirm its annotations survive.
6. Edit a Markdown tab, wait for the dot to clear, use `:q`, reopen; toggle
   Preview.
7. Collapse the tree to the rail, reopen, relaunch the app and confirm tabs,
   active tab and tree state are restored.
