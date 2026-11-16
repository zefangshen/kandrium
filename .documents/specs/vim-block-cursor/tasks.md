# Tasks: fix the normal-mode block cursor

- [x] T1 `crates/app/src/vim.rs`: extend `CaretSource` with
      `caret_line_height` and `caret_char`; implement both in
      `impl_caret_source!` (`RopeExt::char_at` on the caret byte offset).
- [x] T2 `crates/app/src/vim.rs`: rewrite `block_cursor_overlay` to take
      `text_color`, size the block from the shaped advance of the caret
      character (tab = 4 cells, newline/None = `ch_width`), span
      `caret_line_height` re-centred on the caret, and repaint the glyph
      inverted on the filled block.
- [x] T3 Add `block_width` helper plus unit tests for tab, newline/None and a
      shaped glyph.
- [x] T4 Update the four call sites (`notes.rs`, `documents.rs`,
      `projects.rs`, `card_modal.rs`) to pass `t.field.into()`.
- [x] T5 Gates: `cargo fmt --all --check`, `cargo clippy --workspace
      --all-targets`, `cargo test --workspace`.
- [x] T6 Rebuild and relaunch: `killall kandrium-app; just dev` with the
      sandbox disabled; confirm the process is running and the block covers
      the full cell.
- [x] T7 Commit the change.
