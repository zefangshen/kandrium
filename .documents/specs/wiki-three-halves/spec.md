# Spec: rename Wiki to Knowledge and split the tree into Notes / Wiki / Sources

Status: proposed (awaiting approval)
Scope: `crates/app` and a small `crates/core` root-resolution change. No schema,
MCP tool, or dependency changes.

## 1. Rename the section to Knowledge (user-visible only)

The internal module, env vars, file layout and MCP tool names stay `wiki`.

- `crates/app/src/app.rs`: `Section::label()` for `Section::Wiki` returns
  `"Knowledge"`. `Section::id()` stays `"wiki"` (element ids, test paths, nav
  key). Add `Section::setting_key()` returning `"knowledge"` for Wiki and
  `id()` for every other section.
- `crates/app/src/settings.rs`:
  - `set_section` writes `section.setting_key()`.
  - `section()` parses `knowledge`, keeps accepting the old `wiki` value plus
    the retired `notes` / `documents` ids, all mapping to `Section::Wiki`.
  - `import_egui` maps `notes` / `documents` / `wiki` / `knowledge` to Wiki.
- User-visible section-level strings: the detail panel title `Wiki status`
  becomes `Knowledge status`; the empty-state text becomes "Pick a note, wiki
  page or source in either tree, or use the new and upload buttons above."
  (`views/wiki.rs`). The Wiki *half* keeps its own name, so `wiki-empty` and
  the page tree label stay as they are. No other "Wiki" heading or tooltip is
  section-level.

## 2. Three stacked halves

Top→bottom: **Notes** (personal notes root), **Wiki** (compiled-page root),
**Sources** (documents root, unchanged). Each half keeps the current header:
collapse toggle on the left, `ChevronRight` collapsed / `ChevronDown`
expanded, separator only while collapsed.

### State

`crates/app/src/views/wiki.rs`:

- `WikiState.notes: NotesState` — the personal notes tree (label `NOTES`,
  keeps new-note/new-folder/whole-tree collapse).
- `WikiState.pages: WikiPagesState` — new: `root: Option<PathBuf>`,
  `entries: Vec<NoteEntry>`, `expanded: HashSet<PathBuf>`,
  `selected: Option<PathBuf>`, `error: Option<String>`. Rendered by a new
  `pages_half` / `pages_body` with only the half toggle in its header (label
  `WIKI`, no create/upload actions).
- `WikiState.documents: DocumentsState` — unchanged (`SOURCES`).
- New `pages_collapsed: bool`, new `pages_split: f32`.
- `notes_height` is replaced by a pure
  `half_heights(total) -> (notes_h, pages_h, sources_h)`:
  - subtract `HEADER_H` per collapsed half and `SPLITTER_H` per visible seam
    from `total` (`SPLITTER_H = 1.0`, `HEADER_H = 38.0` as today);
  - expanded halves share the remainder by weights `split`,
    `pages_split - split`, `1 - pages_split`, normalised over the expanded
    halves (equal weights if a weight is zero or non-positive);
  - a collapsed half gets exactly `HEADER_H`.
- Seam 1 (Notes↔Wiki) is drawn only when both are expanded; seam 2
  (Wiki↔Sources) only when both are expanded. Separate drag payloads:
  `WikiSplitDrag` (writes `split`) and `WikiPagesSplitDrag` (writes
  `pages_split`), each converting pointer y over the column bounds into a
  full-column share and clamping so seam 1 stays above `MIN_SPLIT` and below
  `pages_split - MIN_SPLIT`, and seam 2 stays between `split + MIN_SPLIT` and
  `MAX_SPLIT`.

### Settings (`crates/app/src/settings.rs`)

- `WikiHalf { Notes, Wiki, Sources }`; keys `wiki_notes_collapsed` (Notes),
  `wiki_pages_collapsed` (new Wiki half), `wiki_documents_collapsed`
  (Sources keeps the old key so existing files still restore).
- `wiki_split()` = Notes share, default `333` permille; new
  `wiki_split_pages()` = cumulative Notes+Wiki share, default `667`.
  Both clamp to `150..=850` and round-trip as permille. `set_wiki_split` /
  new `set_wiki_split_pages` write them.
- Shell wiring in `crates/app/src/app.rs` (`with_store`, `persist_settings`,
  `toggle_wiki_half`, `set_wiki_split` / `finish_wiki_split`, plus a
  `set_wiki_pages_split` / `finish_wiki_pages_split` pair).

## 3. Distinct roots and routing

- `crates/core/src/wiki/mod.rs`: drop the `KANDRIUM_NOTES` read alias from
  `resolve_wiki_root` / `resolve_root_from`; the wiki root is `KANDRIUM_WIKI`
  or `<data_dir>/kandrium/wiki`. Update its alias test to assert the new
  default and that `KANDRIUM_NOTES` no longer redirects it.
- `crates/app/src/store.rs`: add `personal_notes_root()` using
  `notes::ensure_root()`; keep `notes_root()` as the wiki root used by the
  index calls (`wiki_lint`, `refresh_wiki`, `reindex_wiki_page`).
- `crates/app/src/app.rs`: `load_notes` fills `wiki.notes` from
  `personal_notes_root`; a new `load_wiki_pages` fills `wiki.pages` from
  `notes_root`. `open_note` / `save_note` pick the root by path containment:
  under the personal notes root → the notes tree; under the wiki root → the
  pages tree. Both kinds keep using `wiki.notes.tabs` + `TabKey::Note`, so
  the merged tab strip is unchanged. `save_note` reindexes the wiki only when
  the saved path is under the wiki root. Add
  `toggle_wiki_page_folder` and `open_wiki_page` (row click → `open_note`).

## Non-goals

- No renaming of Rust types/modules, `KANDRIUM_*` env vars, MCP tools, or the
  wiki file layout.
- No changes to Sources behaviour, PDF annotations, or the database schema.
- No new dependencies.

## Verification

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets`
3. `cargo test --workspace` — with tests updated for the three-half ids and
   heights (`crates/app/tests/wiki.rs`), the `section=knowledge` value and
   new split defaults (`settings.rs` tests), and any old-label assertions in
   `crates/app/tests/notes.rs` / `documents.rs`.
4. `killall kandrium-app; just dev` with the sandbox disabled, then eyeball:
   three headers in order, each collapse toggle works, both seams drag, the
   nav reads Knowledge, and note/page/source tabs still open and switch.
