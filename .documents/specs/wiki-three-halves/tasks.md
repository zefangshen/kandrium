# Tasks: Knowledge rename and three-half Wiki tree

- [x] T1 `app.rs`: `Section::Wiki` label → `Knowledge`; add `setting_key()`.
- [x] T2 `settings.rs`: write `section=knowledge`, accept `knowledge` / `wiki`
      / `notes` / `documents` on load; update section tests.
- [x] T3 Section-level copy in `views/wiki.rs`: `Knowledge status` panel
      title, empty-state text; leave the Wiki half's own labels.
- [x] T4 `settings.rs`: `WikiHalf::Wiki` + `wiki_pages_collapsed`; defaults
      `wiki_split=333`, new `wiki_split_pages=667`; getters/setters/clamps and
      tests (defaults, round trip, clamp, old keys).
- [x] T5 `core::wiki`: remove the `KANDRIUM_NOTES` alias from
      `resolve_wiki_root`; update the alias test.
- [x] T6 `store.rs`: add `personal_notes_root()`; `notes_root()` stays the wiki
      root for the index calls.
- [x] T7 `app.rs`: load `wiki.notes` from the personal notes root and
      `wiki.pages` from the wiki root; route `open_note` / `save_note` by path
      containment; reindex only wiki-root saves; add
      `toggle_wiki_page_folder`, `set_wiki_pages_split`, persistence.
- [x] T8 `views/wiki.rs`: `WikiPagesState`, `half_heights`, three halves, two
      seams and drag payloads, pages header/body, `notes::tree_header` takes a
      label (`NOTES`).
- [x] T9 Update `crates/app/tests/wiki.rs` (three ids, 1/3 heights, both
      collapse toggles, both seams), any old-label assertions elsewhere.
- [x] T10 Gates: `cargo fmt --all`, `cargo clippy --workspace --all-targets`,
      `cargo test --workspace`.
- [x] T11 Rebuild and relaunch: `killall kandrium-app; just dev` (sandbox
      disabled); confirm the process is up.
