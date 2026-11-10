# Stage 5 — Task checklist

Legend: `[ ]` pending, `[~]` in progress, `[x]` done. Each task ends with the
stated gate; commits follow `spec.md` §7 and are made incrementally.

## A. Extract `pdf-render` (commit 1)

### A1. New workspace crate

- [ ] `crates/pdf-render/Cargo.toml` (package `pdf-render`, lib `pdf_render`,
      edition 2024, `hayro` workspace dep, `kurbo = "0.13"`).
- [ ] `crates/pdf-render/src/lib.rs`: move `TextGlyph`, `TextLine`, `PageText`,
      `approx_quad_eq`, `rects_intersect`, `quad_from_rect`, `group_lines`,
      `GlyphCollector`, `extract_text`, `Request`, `Event`, `PdfWorker`,
      `worker_loop`, `render_page` and the four tests from
      `crates/app/src/pdf.rs`.
- [ ] Replace the `egui::Context` field/parameter with
      `wake: Arc<dyn Fn() + Send + Sync>` plus
      `open(path, wake: impl Fn() + Send + Sync + 'static)`.
- [ ] `PageText::selection_quads` calls the local `rects_intersect`.
- [ ] Root `Cargo.toml`: add `crates/pdf-render` to `members`.
- **Gate:** `cargo test -p pdf-render` — the four moved tests pass.

### A2. Keep the egui app identical

- [ ] `crates/app/src/pdf.rs` → re-export shim of the moved items with an
      extraction note.
- [ ] `crates/app/src/views/documents.rs`: pass
      `move || repaint.request_repaint()` to `PdfWorker::open`.
- [ ] `crates/app/Cargo.toml`: add `pdf-render` path dep, drop `hayro`/`kurbo`.
- **Gate:** `cargo test -p kandrium-app` (documents + pdf tests green), egui
  behaviour unchanged.

### A3. Wire the gpui crate

- [ ] `crates/app-gpui/Cargo.toml`: add `pdf-render`, add `rfd = "0.15"`, drop
      `egui`/`hayro`/`kurbo`, keep `image`/`smallvec`.
- [ ] `crates/app-gpui/src/main.rs`: remove the `#[path] mod pdf`.
- [ ] `crates/app-gpui/src/pdf_probe.rs`: import `pdf_render`, wake `|| {}`.
- **Gate:** `cargo clippy --workspace --all-targets` and
      `cargo clippy --manifest-path crates/app-gpui/Cargo.toml --all-targets`.
- **Commit 1:** `Extract the PDF renderer into a shared pdf-render crate`.

## B. Store bridge and session codec (commit 2)

### B1. `crates/app-gpui/src/store.rs`

- [ ] `documents_root`, `documents_tree`, `read_document`, `write_document`,
      `create_markdown`, `create_doc_folder`, `import_documents`,
      `rename_document`, `delete_document`, `load_annotations`,
      `save_annotations`.
- [ ] `import_documents` returns per-file `(path, Option<error>)`.
- [ ] Every wrapper maps errors with `e.to_string()`.

### B2. Session codec

- [ ] `format_session` / `parse_session` (tree flag, active index, one
      root-relative path per line; garbage ignored) in `views/documents.rs`.
- [ ] `rel_path` / `abs_path` helpers.
- [ ] In-file unit tests for the codec and the path helpers.
- **Gate:** `cargo test --manifest-path crates/app-gpui/Cargo.toml --lib`.
- **Commit 2:** `Add the Documents store bridge and the tab session codec`.

## C. Tree, tabs, Markdown (commit 3)

### C1. State and pure geometry

- [ ] `DocKind`, `DocTab`, `DocDraftKind`, `DocDraft`, `DocumentsState`, `Tool`
      in `views/documents.rs` with the constants from `spec.md` §4.2.
- [ ] Helpers `screen_to_page`, `page_to_screen`, `permille`, `hex_rgb`,
      `Selection::rect`, `note_anchor_quad`, `visible_pages` + unit tests.

### C2. Tree UI

- [ ] Panel + rail (`documents-tree`, `documents-tree-rail`, expand/collapse).
- [ ] Header actions: new folder, new file, upload (`Icon::Upload`, added to
      `assets.rs` from `app/assets/icons/upload.svg`; add `Icon::File` for
      PDFs if the icon set lacks it).
- [ ] Folder-first rows, folder toggle, file rows, active-tab highlight.
- [ ] Hover row actions: rename, delete; folders also get new file/folder.
- [ ] Inline create row with Enter/Esc and core error kept open.
- [ ] Inline rename remaps tabs, selection, expanded set; rescan; session write.
- [ ] Delete confirmation card; core removes PDF sidecars; affected tabs drop.
- [ ] Upload via `rfd` multi-select into the selected folder; partial failures
      surfaced; destination selected + expanded after success.

### C3. Tabs and Markdown content

- [ ] Tab strip: activate, close with X, dirty dot, horizontal scroll.
- [ ] Markdown tab: `TextareaState`, per-tab `VimSession`, Edit/Preview strip,
      vim badge/ex line, `:w` save, `:q` close.
- [ ] Edit observer, 400 ms generation-guarded autosave via `cx.spawn`, flush on
      close/section leave.
- [ ] Session save/restore on open, close, rename, delete, tree toggle, leave.

### C4. Section wiring

- [ ] `AppShell` fields + `load_documents` + all tree/tab/draft/delete/import
      methods from `spec.md` §4.4.
- [ ] `select` arms load/flush; `body` Documents arm calls `documents_view`;
      remove the placeholder for Documents.
- **Gate:** build, launch, manual tree/Markdown pass; `cargo fmt`, `cargo
  clippy --workspace --all-targets`, `cargo test --workspace`.
- **Commit 3:** `Port the Documents tree, tabs and Markdown editor to gpui`.

## D. PDF viewer (commit 4)

### D1. Worker pump

- [ ] `pump_documents` in `body()`: drain receivers, apply
      `Opened`/`OpenFailed`/`Rendered`/`Text`/`Failed`, BGRA conversion, page
      size, LRU cap 24, notify while work is outstanding.
- [ ] `open_document` PDF branch spawns `PdfWorker::open(path, || {})` and loads
      annotations through `Store`.

### D2. Viewer

- [ ] Toolbar: page nav with label, zoom out/percent reset/zoom in, tool
      toggles.
- [ ] Scroll container with `ScrollHandle`, direct-child page rows, `canvas`
      bounds capture, render-scale sizing (zoom = sizing).
- [ ] Visible-page request window with one-page pre-render margin; text
      requested once per page.
- [ ] Page jump through `scroll_to_top_of_item`; visible page from
      `top_item()`.
- [ ] Overlays: selection, highlights, note anchors.
- [ ] Pointer tools: Select (visual only), Highlight (quads + selected text),
      Note (anchor quad), click-to-open existing annotations.
- [ ] Popover: memo textarea, four colors, delete, close; clamped to viewport.
- [ ] Annotation autosave: 500 ms generation-guarded, flush on close/leave;
      `modified_at` bumped on every edit.
- [ ] Loading/error states.
- **Gate:** `cargo fmt`, `cargo clippy --workspace --all-targets`, `cargo test
  --workspace`, launch and open a real PDF.
- **Commit 4:** `Add PDF viewing with zoom, page navigation and annotation
  sidecars`.

## E. Tests, gates, relaunch (commit 5)

- [ ] `crates/app-gpui/tests/documents.rs`: the eleven cases from `spec.md` §6.
- [ ] Fix any regressions; keep the `vim.rs:2436` warning untouched.
- [ ] Final gates: `cargo fmt --all`, `cargo fmt --manifest-path
      crates/app-gpui/Cargo.toml`, `cargo clippy --workspace --all-targets`,
      `cargo clippy --manifest-path crates/app-gpui/Cargo.toml --all-targets`,
      `cargo test --workspace`, `cargo test --manifest-path
      crates/app-gpui/Cargo.toml`.
- [ ] Relaunch `killall kandrium-gpui; open -n
      crates/app-gpui/target/KandriumGpui-dev.app`.
- **Commit 5:** `Cover the Documents flows with headless tests`.
- [ ] Report: end-to-end flows, extraction impact on egui, test counts, commit
      hashes, parity dropped (§5), on-screen checks (§8).
