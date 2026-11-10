# Stage 5 — Verification checklist

- [ ] `crates/pdf-render` owns the renderer: `PdfWorker::open(path, wake)`, the
      text model and the four moved tests; `crates/app/src/pdf.rs` is a
      re-export shim and the egui app still passes its own tests.
- [ ] The gpui crate depends on `pdf-render` and no longer includes `pdf.rs`
      through `#[path]`; `pdf_probe` uses the shared crate.
- [ ] The tree lists folders before PDFs and Markdown, sorted by name, over the
      documents root (`KANDRIUM_DOCS`), and collapses to a 30 px rail.
- [ ] New file creates Markdown only, bare names gain `.md`; new folder and new
      file both use inline naming with Enter commit and Esc cancel.
- [ ] Upload copies through the native dialog into the selected folder, applies
      the `-2` unique-name rule, creates no sidecars, and reports partial
      failures without aborting the batch.
- [ ] Rename and delete are offered from row hover/context actions; rename
      remaps open tabs, selection and the expanded set, moves the PDF sidecar,
      and delete closes the affected tabs and removes the sidecar.
- [ ] Tabs activate, close with X, show a dirty dot, and are persisted as
      root-relative paths in `gpui-documents.session` (tree flag, active index,
      paths only) and restored on the next start.
- [ ] Markdown tabs reuse the shared editor and preview; vim is per tab; `:w`
      saves and `:q`/`:x` close after flushing; dirty buffers autosave after
      400 ms of quiet, once per change.
- [ ] PDF pages render off the UI thread, are converted to BGRA for
      `RenderImage`, and are cached with a 24-entry LRU.
- [ ] Zoom is render-scale sizing (25 %–400 %, 1.25 step), pan is scrolling,
      page navigation moves through `scroll_to_top_of_item`, and the visible
      page label follows the scroll position.
- [ ] Annotations load from and save to `*.pdf.kandrium.json` through core,
      with atomic writes and a 500 ms debounce; an empty document removes the
      sidecar; selection remains visual-only as in egui.
- [ ] `cargo fmt --all`, both clippy runs, `cargo test --workspace` and the
      gpui crate tests pass; the pre-existing `vim.rs:2436` warning is the only
      one left.
- [ ] The gpui dev app is relaunched from the fresh build.
