# Plan: merge Notes and Documents into one Wiki section

Status: approved; implementation follows the stage order in §12.
Scope: `crates/app-gpui` (the gpui front-end). `kandrium-core`, the egui app
(`crates/app`) and the MCP server are untouched.

## 1. Goal

Replace the two sibling sections Notes and Documents with one section, Wiki:

- The section nav becomes **Tasks, Projects, Wiki, Clocks**.
- Wiki's tree column splits vertically: notes tree on top, documents tree
  below, half the height each by default.
- Either half can be collapsed so the other takes the full height, and
  expanded again.
- One tab row and one detail area serve both tab sets, using the tab
  treatment the editor chrome already has (`views::tab_strip` /
  `views::pane_tab`).

### Non-goals

- No change to the notes/documents storage: notes remain plain `.md` files
  under `KANDRIUM_NOTES` (default `<data_dir>/kandrium/notes/`); documents stay
  under `KANDRIUM_DOCS` (`.md` + PDF with JSON sidecars).
- No cross-listing of one root in the other half, no moving files between
  roots, no unified search over both trees.
- No change to Projects (its own tree + Overview/Tasks/documents tabs), Tasks,
  Clocks, the card modal, the MCP server or the database schema.
- No new archive feature. (See §6 for the audit.)

## 2. What exists today

| Piece | Location |
| --- | --- |
| `Section` enum, `ALL` order, `label`/`id`/`icon`, `from_key` | `crates/app-gpui/src/app.rs` L56–113 |
| Settings file (`gpui-settings.conf`), `TreePanel`, section save/load | `crates/app-gpui/src/settings.rs` |
| State seeding from settings | `app.rs` L318–327 |
| `persist_settings` | `app.rs` L409–431 |
| `select()` — flush, blur, load per section | `app.rs` L509–545 |
| `NotesState` + `notes_view` | `crates/app-gpui/src/views/notes.rs` L73–164 |
| Notes tree panel (header, draft, rows) | `notes.rs` L203–475 |
| Notes tab strip + head + editor + status | `notes.rs` L476–639 |
| `DocumentsState`, `DocTab`, session codec | `crates/app-gpui/src/views/documents.rs` L288–360, L433–490 |
| Documents tree panel | `documents.rs` L561–918 |
| Documents tab strip + head + body + status | `documents.rs` L928–1052 |
| Shared tab chrome (`tab_strip`, `pane_tab`, `vim_indicator`, `mode_switch`, `path_label`, `gutter_rule`) | `crates/app-gpui/src/views/mod.rs` |
| Document session file (`gpui-documents.session`) | `app.rs` L4584–4662 |
| PDF pump | `app.rs` L5599; called from `body()` for Documents |
| DB watcher exemption for Notes/Documents | `app.rs` L3397 |

Both views already share the same shape: a fixed-width tree column, an
optional 5 px splitter, a detail column with a 38 px tab row, a 40 px head
row, a body, and a status bar. The tab row and head row are built separately
in each module, which is exactly what makes the merge cheap: only the tab
strip has to become shared, the head/body/status can stay per kind.

### What the current trees actually contain

Audited the two tree panels before planning the mapping:

- **Search**: neither the notes nor the documents tree has a search field.
  The only search in the app is the board search and the Archived page.
- **Upload**: documents only (`documents-upload`).
- **New file / new folder**: notes has new note + new folder; documents has
  new Markdown file + new folder (PDFs arrive by upload).
- **Context menus**: both trees have per-row rename/delete (documents also has
  the folder actions), each wired to its own handlers and confirm dialog.
- **Archive**: no archive affordance exists in either tree. "Archived" is
  Tasks-only (`showing_archive` + `views/archive.rs`).
- **Tree rows**: notes 26 px rows with `surface2` selection + 2 px accent bar;
  documents the same treatment.

So "map the search/upload/menus into the halves" is mostly "leave each half's
header actions where they are"; there is no search or archive to relocate.

## 3. Target layout

```
┌ titlebar: Tasks | Projects | Wiki | Clocks ─────────────── clock ┐
├──────────────────────────┬──────────────────────────────────────┤
│ NOTES              ▾ +(+) │ [note tab][doc tab][pdf tab] …       │ 38 tab row
│ ▸ folder                 ├──────────────────────────────────────┤
│   note.md                │ path · vim chip · Edit/Preview       │ 40 head
│ ──────── splitter ───────│                                      │
│ DOCUMENTS          ▴ +↑📁│   editor / preview / PDF viewer      │
│ ▸ folder                 │                                      │
│   a.pdf  b.md            ├──────────────────────────────────────┤
│                          │ vim · Ln/Col · words · save state    │ status
└──────────────────────────┴──────────────────────────────────────┘
```

Only the tree column splits vertically. The detail column stays one tab row,
one head row, one body — the tab row is the union of both tab sets, ordered
by a single list.

## 4. Section identity and settings

### 4.1 `Section`

- Delete `Section::Notes` and `Section::Documents`; add `Section::Wiki`.
- `Section::ALL = [Tasks, Projects, Wiki, Clocks]` (nav order).
- `label()` → `"Wiki"`, `id()` → `"wiki"` (also the nav test id `nav-wiki`
  and the body id `wiki-view`).
- `from_key("wiki")` resolves; `"notes"` and `"documents"` are **not** valid
  section ids in the new build.
- `icon()` is currently unused (the titlebar nav is text-only after the last
  titlebar change). Keep the method for parity; map Wiki to `Icon::Folder`
  (or `FileText`) so a future icon pass has a value.

### 4.2 Migrating the saved section

`Settings::section()` (settings.rs L113–118) currently maps unknown values to
Tasks. Add an explicit legacy map there, not in `from_key`:

```text
"notes"     -> Section::Wiki
"documents" -> Section::Wiki
"wiki"      -> Section::Wiki
otherwise   -> Section::Tasks
```

The next `persist_settings()` writes `section=wiki`, so the migration is
one-shot and self-healing. A rollback build (old binary) would read
`section=wiki` as unknown and land on Tasks — acceptable and lossless for
content, and it is the one-way edge of the migration.

### 4.3 Settings keys

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `section` | string | `tasks` | last section; `wiki` after the merge |
| `wiki_tree_open` | bool | `1` | whole-tree rail collapse (today's `notes_tree_open`) |
| `wiki_tree_width` | f32 | `240` | shared tree column width, clamped 160–420 |
| `wiki_split` | permille int | `500` | notes-half share of the split, clamped 150–850 |
| `wiki_notes_collapsed` | bool | `0` | notes half collapsed to its header |
| `wiki_documents_collapsed` | bool | `0` | documents half collapsed to its header |

Rules:

- Store the split as an integer permille (`wiki_split=500`) so the
  deterministic key-sorted file stays byte-stable, matching how widths are
  formatted (`{:.1}`).
- Keep reading the legacy `notes_tree_width` / `documents_tree_width` as a
  fallback for `wiki_tree_width` (prefer documents width, then notes, then
  default) so a user's tree width survives the upgrade.
- Replace `TreePanel::{Notes, Documents}` with `TreePanel::Wiki`; keep
  `TreePanel::Projects`. The per-panel `open_key`/`width_key` helpers stay.
- Unknown keys are already preserved by `Settings` — no change needed there.
- Add `wiki_split()/set_wiki_split()`, `wiki_half_collapsed(half)/
  set_wiki_half_collapsed(half, ..)` with a small `WikiHalf` enum.

### 4.4 Document session file

`gpui-documents.session` (app.rs L4584–4662) stores
`<tree_open>\n<active>\n<rel paths…>`. Keep the file name so users' open
document tabs survive the upgrade. Reinterpret it:

- The leading tree flag now mirrors `wiki_tree_open` (write it, ignore it on
  load — tree openness is owned by settings).
- The active index becomes "absolute index within the document subset of the
  merged order", or better: stop persisting an index and persist the active
  document path one line after the flag. Recommend the path form: it stays
  correct when note tabs are interleaved. This is a small format bump; parse
  the old 2-line-index form for one release (existing `parse_session` already
  tolerates trailing lines).
- Note tabs are **not** persisted today (there is no notes session file) and
  this plan does not silently add one; see open question Q3.

## 5. State model

Add `crates/app-gpui/src/views/wiki.rs` with the layout and a thin state
owner, and move the two existing states under it:

```rust
pub struct WikiState {
    pub notes: NotesState,
    pub documents: DocumentsState,
    /// Display order of the merged tab row, oldest first.
    pub order: Vec<TabKey>,
    /// The tab the detail column renders.
    pub active: Option<TabKey>,
    /// Notes-half share of the tree split (0.15–0.85).
    pub split: f32,
    pub notes_collapsed: bool,
    pub docs_collapsed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabKey { pub kind: TabKind, pub path: PathBuf }

pub enum TabKind { Note, Doc }
```

Why a `TabKey` order list instead of one `Vec<WikiTab>`:

- `NoteTab` and `DocTab` have different fields (vim session vs PDF worker and
  annotations) and are manipulated by ~20 existing `AppShell` handlers that
  index `notes.tabs` / `documents.tabs`. Keeping those vectors and layering
  an order list on top avoids rewriting all of them in the same commit and
  keeps the PDF machinery untouched.
- `TabKey` survives index invalidation on close (the failure mode of storing
  bare indices).
- `order` is the only authority for display order; the per-kind `active`
  indices remain for compatibility but must be kept in sync with
  `WikiState::active` whenever a tab is activated or closed.

Ownership change: `AppShell.notes` / `AppShell.documents` become
`AppShell.wiki.notes` / `AppShell.wiki.documents`. This is mechanical (sed-like)
but touches every call site; it is the first commit so the later ones are
small.

### Activation and close

- `activate_wiki_tab(key)`: set `wiki.active`, set the matching per-kind
  `active` index, focus the editor if the tab has one, `cx.notify()`.
- `close_wiki_tab(key)`: dispatch to the existing
  `close_note_tab_by_path` / `close_doc_tab_by_path`, then remove the key from
  `order` and pick the neighbouring tab as active (right neighbour, else
  left).
- Opening a note or a document appends its key at the end of `order`, so a
  newly opened tab always lands at the right end of the row (requirement).
- Re-opening an already-open path activates the existing tab in place.

## 6. Tree column: vertical split and collapse

### 6.1 Layout

The tree column keeps its fixed width (`wiki_tree_width`) and its existing
horizontal splitter to the detail. Inside it:

1. Notes half: its existing 38 px header (`NOTES` + actions), draft row,
   error, row list.
2. Vertical splitter: same construction as the tree splitter — a 5 px grab
   lane with `-2 px` side margins, a 1 px `t.divider` rule, cursor
   `ResizeUpDown`, `on_drag` carrying `WikiSplitDrag`.
3. Documents half: its existing header (`DOCUMENTS` + actions), draft, error,
   rows.

Default: splitter between two `flex_1` halves = 50/50. When the user drags,
the ratio is computed from the pointer y relative to the tree column bounds
(`f32::from(event.position.y) - f32::from(event.bounds.origin.y)`), exactly
the pattern the horizontal splitter uses for x.

Clamping:

- `MIN_HALF_H = 120.` — a half may not shrink below this while expanded.
- `split` is clamped to `150..=850` permille on save/load (a hard geometry
  clamp also runs per frame against the available height, so a short window
  cannot produce a negative half).
- `finish_wiki_split()` persists on drop, like `finish_tree_resize`.

### 6.2 Collapse model

- Each half gains one chevron button in its own header:
  `wiki-notes-collapse` / `wiki-notes-expand` and
  `wiki-documents-collapse` / `wiki-documents-expand` (the same element can
  toggle, but separate ids keep tests readable).
- Collapsed means "header only": the half renders its 38 px header bar with
  the title and an expand chevron, no draft/error/rows. This keeps a visible
  affordance to expand again; a 0 px half would be unreachable.
- The expanded half takes the remaining height. If both are collapsed the
  column shows two 38 px header strips; this is allowed and reversible
  (simplest model, no "last one wins" special case) — see Q5.
- Collapsing never closes tabs, drops drafts, or clears selection.
- Both flags persist immediately (a collapse is a user action, so
  `persist_settings()` runs, like the existing tree toggle).
- The whole-tree rail (`tree_open`) is unchanged: it collapses the entire
  tree column to `RAIL_W` (30 px) with one expand chevron, hiding both
  halves. Per-half collapse is orthogonal and preserved across a
  rail round-trip.

### 6.3 Optional: a "both halves" header

Not planned: no extra section header above the halves. Each half keeps its
own 38 px header, which already names it (`NOTES` / `DOCUMENTS`). This keeps
the vertical budget for rows at the 940×700 minimum.

## 7. Tab row and detail area

### 7.1 One tab row

Replace both content-level strips with a single builder in `wiki.rs`:

```text
for key in &wiki.order:
    active = wiki.active == Some(key)
    tab = pane_tab(format!("wiki-tab-{kind}-{path}"), active, t)
        .child(icon(if note { StickyNote } else { doc_icon(path) }, 12, faint))
        .child(name)
        .when(dirty, 3 px accent dot)
        .child(close id wiki-tab-close-…)
```

- Notes use `Icon::StickyNote` and documents keep `documents::doc_icon`, so a
  PDF is visually distinct in the same row.
- Keeping the existing `note-tab-*` / `doc-tab-*` ids would collide across
  kinds only if a path is opened as both; the prefixed `wiki-tab-*` ids make
  the merge explicit and let old tests be retargeted in one pass.
- Empty state: one placeholder when `order` is empty — "Nothing open. Pick a
  file in either tree, or use the new/upload buttons above."

### 7.2 Detail dispatch

Keep each module's head/body/status code and split `content()` into
"strip" and "detail":

- `notes::detail_body(state, index, t, window, cx)` — path label, vim chip,
  Edit/Preview switch, preview or `Editor` + gutter rule, status bar. The
  note status bar already reports PREVIEW/vim-off; unchanged.
- `documents::detail_body(state, index, t, window, cx)` — path label, vim +
  Edit/Preview for Markdown, `markdown_body` or the PDF viewer, status bar.
- `wiki.rs` renders `strip + match key.kind { Note => notes::detail_body(..),
  Doc => documents::detail_body(..) }`.

That removes duplicated strip construction (~55 lines in each module) without
touching editor internals, the PDF worker, vim or the status bars.

### 7.3 Per-kind id prefixes

To keep the shared detail honest:

- Notes head keeps `notes-head`, `notes-path`, `notes-vim`, `notes-preview`,
  `note-editor`, `note-preview`, `notes-status`.
- Documents keep `documents-head`, `documents-path`, `documents-vim`,
  `documents-preview`, `doc-markdown`, the PDF ids (`doc-pdf-*`), and its
  status bar.
- The merged container is `wiki-view`, with children `wiki-tree`,
  `wiki-notes-tree`, `wiki-documents-tree`, `wiki-tab-strip`, `wiki-content`.

## 8. Behaviour mapping

| Affordance | Today | After the merge |
| --- | --- | --- |
| Tree search | none in either tree | none; not added (Q6) |
| Upload (PDFs/Markdown) | documents header | stays in the documents half header, ids unchanged (`documents-upload`) |
| New file / new folder | notes: note+folder; documents: md+folder | unchanged per half; inline draft row renders inside its own half |
| Row context menus (rename/delete/folder actions) | per kind | unchanged per half |
| Archive | Tasks-only | nothing to move; no archive in Wiki |
| Tree width splitter | two independent widths | one shared `wiki_tree_width` |
| Tree rail collapse | per section | one rail for the whole column |
| Expanded folders | per section | per half, unchanged sets |
| Selection | per section | per half, unchanged |
| Dirty dot / save state | per tab | per tab, in the shared row and the matching status bar |
| PDF streaming repaint | Documents body | `pump_documents` runs while Wiki is foregrounded |

`select()` must be updated for one section: leaving Wiki flushes the active
note (`flush_active_note`), flushes documents and saves the session
(`flush_documents` + `save_documents_session`). Entering Wiki runs
`load_notes`, `load_documents`, and `restore_documents_session` once.

The DB watcher exemption at `app.rs` L3397 (`Notes | Documents => return`)
becomes `Wiki => return`.

## 9. Open tabs across the merge

- **At runtime** nothing is lost: both states are constructed in
  `AppShell::with_store` today and both are loaded when their section is
  entered. In the merged section both load, so every open tab from either
  pane is already in memory and simply appears in the shared row.
- **Across a restart**: document tabs restore from `gpui-documents.session`
  into the merged row, in their saved relative order. Note tabs do not
  restore today (there is no notes session file) and this plan keeps that
  behaviour unless Q3 says otherwise. When documents restore, append their
  keys to `order` in session order; notes reopen later (if opened before
  quitting nothing survives, same as today).
- **Ordering on restore**: notes (none) then documents, i.e. the same order as
  a fresh run. If Q3 enables note persistence, restore notes first, documents
  second.
- **Active tab on restore**: use the persisted active document path if it
  still exists; otherwise the last tab in `order`.
- **Duplicate paths**: notes and documents roots are different directories in
  the default setup, so the same path cannot appear in both. If the user
  points `KANDRIUM_NOTES` and `KANDRIUM_DOCS` at the same directory, the same
  file could be opened once as a note and once as a document. Decide: dedupe
  by canonical path, first open wins, the second attempt activates the
  existing tab. Recorded as an edge case rather than a feature.

## 10. Edge cases at the 940 px minimum

- **Titlebar**: the nav loses two chips, so the 940 px row gains room; update
  the min-width titlebar tests to the four-chip set and keep the "clock flush
  right, drag stretch survives" assertions.
- **Width**: the tree column is 160–420 px; at 940 with the default 240 the
  detail gets ~700 px, enough for the tab row and a PDF page. The vertical
  split does not consume width.
- **Height**: at 700 px window height the section body is ~662 px; two 38 px
  headers plus the 1 px splitter leave ~585 px for rows → ~292 px per half at
  50/50. `MIN_HALF_H = 120` is comfortably below that, so a drag can reach
  roughly 4:1 before clamping.
- **Collapsed + draft**: collapsing a half with an open inline name draft
  keeps the draft state but hides it; expanding restores it (the input entity
  is not dropped).
- **Splitter target**: use the same 5 px lane / `-2 px` margin trick as the
  horizontal splitter so the target stays usable at 940 px.
- **Tab overflow**: the strip has no horizontal scroll today (it ends with a
  `flex_1` filler). With both tab sets merged, many tabs will clip rather
  than scroll. This plan leaves the behaviour unchanged (Q4); if overflow
  matters, it is a follow-up, not part of the merge.
- **PDF at minimum size**: the viewer already fits pages by width; the detail
  width is unchanged by the merge, so no new edge case.

## 11. Test coverage

### Settings (unit, `settings.rs`)

- Legacy section mapping: `section=notes` and `section=documents` →
  `Section::Wiki`; `section=elsewhere` → Tasks.
- Round trip and deterministic save with the new keys
  (`section=wiki\nwiki_split=…\nwiki_tree_width=…`).
- Split clamp (0 → 150, 9999 → 850, garbage → 500), width clamp reused.
- Legacy width fallback: only `documents_tree_width=300` present → 300.
- Collapsed flags parse (`0/1`, `true/false`, garbage → false).

### Shell / layout (`tests/shell.rs`, new `tests/wiki.rs`)

- `every_section_renders_its_body` becomes four sections; Wiki renders
  `wiki-view`, both halves and the shared strip.
- Default geometry: both halves ~equal height inside `wiki-tree`;
  `wiki-splitter` between them; both headers visible.
- Dragging `wiki-splitter` changes the halves' heights in the expected
  direction and persists `wiki_split` only on drop.
- Collapse notes → notes half height == its header height, documents half
  grows; expand restores the ratio. Mirror for documents. Collapsing both
  leaves two headers.
- Whole-tree rail still collapses to 30 px and restores both halves with
  their collapse flags intact.
- 940×700: both halves ≥ `MIN_HALF_H`, splitter hit lane present, no
  overlap with the tab row.
- Titlebar min-width tests updated to four chips.

### Tabs (`tests/wiki.rs`)

- Open a note, then a document: one row, note first, document appended at the
  right; `wiki.active` on the document.
- Click the note tab: detail swaps to the note editor and its status bar;
  click the PDF tab: the viewer renders.
- Close the middle tab: neighbours and active selection follow the rule;
  closing a note leaves documents untouched and vice versa.
- Dirty dots: editing a note shows its dot; saving clears it; a dirty
  Markdown document behaves the same.
- Re-opening an already-open path activates it without duplicating the tab.
- Restore: a session with two document paths produces two tabs in saved
  order and the persisted active path selected; the tree flag in the file is
  ignored in favour of `wiki_tree_open`.

### Existing tests to retarget

- `tests/notes.rs` (`notes_view_renders_the_tree_and_empty_content`), 
  `tests/documents.rs` (`selects_the_documents_section_and_lists_the_tree`
  and the `open_documents` helper), `tests/shell.rs`
  (`settings_file_records_section_tree_and_vim`,
  `board_search_does_not_regain_focus_after_navigating_away`, the
  `nav-notes` / `nav-documents` clicks): swap to `nav-wiki` and the new ids.
- All gates: `cargo fmt`, `cargo clippy --all-targets`, `cargo test`
  (currently 8 suites, 79/1/5/10/3/1/53 + doc tests) must stay green.

## 12. Staged order of work

Each stage ends with `cargo fmt`, `cargo clippy --all-targets`,
`cargo test`, a rebuild and a visual check, and is its own commit so it can
be reverted independently.

1. **S1 — state composition (no visible change).**
   Add `views/wiki.rs` with `WikiState`/`TabKey`, move
   `AppShell.notes`/`AppShell.documents` under `shell.wiki.*`, update call
   sites, keep both sections and the nav untouched. Gates green.
   *Rollback point A*: pure refactor, behaviour identical.

2. **S2 — settings and identity.**
   Add `Section::Wiki`, the legacy section map, new settings keys,
   `TreePanel::Wiki`, split/collapse accessors and unit tests. Nav still has
   five entries; the new keys are written but inert.
   *Rollback point B*: revert removes the keys; unknown-key preservation
   means an older build ignores them.

3. **S3 — merged body and split tree (behind a temporary entry).**
   Build `wiki_view`: vertical split, both tree panels, one tab row, the
   detail dispatcher. Reach it through a temporary fifth/sixth nav entry
   (labelled Wiki) while Notes and Documents still exist, purely so the work
   can be driven and captured before the cutover. New tests exercise
   `wiki-view` directly.
   *Rollback point C*: revert; the old sections were never removed.

4. **S4 — cutover.**
   `Section::ALL` becomes `[Tasks, Projects, Wiki, Clocks]`; delete the
   Notes/Documents variants and their bodies from `body()`; retarget every
   test id; verify the section-setting migration by writing
   `section=documents` into a settings file and booting.
   *Point of no return for the settings id* (a reverted build reads
   `section=wiki` as Tasks once). Content on disk is untouched, so rollback
   loses only the last-section preference.

5. **S5 — sessions and cross-cutting behaviour.**
   Session format (active path), flush/load on section switches, PDF pump,
   watcher exemption, duplicate-path guard, `MIN_HALF_H` clamping against
   the live height, and the remaining 940 px assertions.

6. **S6 — polish and verification.**
   Full gates, rebuild, relaunch, computer-use capture at the 940 px minimum
   (nav with four items, both halves, merged tab row), and a short report of
   anything that still differs.

## 13. Open questions

1. **Detail split**: the requirement says the *tree panel* splits. Confirm
   the detail column stays a single pane (this plan assumes yes).
2. **Tab order across kinds**: is "notes tabs then documents tabs, new opens
   at the right end" acceptable, or must the order be true creation order
   across kinds (which the shared `order` list already gives if we seed it
   from open time rather than from kind)?
3. **Persist note tabs?** Documents already restore after a restart; notes do
   not. Should the merge also introduce a notes session so both halves restore
   (new behaviour), or keep parity with today?
4. **Tab overflow at 940 px**: leave clipping as today, or add a horizontal
   scroll lane to the merged strip?
5. **Both halves collapsed**: allowed (two header strips) or forbid
   collapsing the second one?
6. **Tree search**: since neither tree has a search field today, is a shared
   filter over both trees wanted as part of Wiki, or explicitly a follow-up?
7. **Archive**: confirm "archive affordances" referred to something you
   expected in Documents — no such action exists in either tree today.
8. **Wiki icon**: nav is text-only right now; pick `Folder` or `FileText` for
   a future icon pass (or drop `Section::icon` entirely).
9. **Tree width migration**: prefer the documents width over the notes width
   when both legacy keys exist (assumption above), or take the wider one?

### Decisions taken (approved defaults)

These answers are what the implementation follows; this list is the record of
the choice for each open question.

1. **Q1 — detail split**: yes, the detail column stays one pane. Only the
   tree column splits vertically.
2. **Q2 — tab order across kinds**: true open/creation order across kinds. The
   shared `order` list is appended when a tab opens, so an interleaved
   note/document sequence is preserved exactly.
3. **Q3 — persist note tabs?**: no. Notes keep parity with today (tabs live
   only for the session); only documents restore from `gpui-documents.session`.
4. **Q4 — tab overflow**: leave clipping as today. No horizontal scroll lane
   is added to the merged strip.
5. **Q5 — both halves collapsed**: allowed, showing two 38 px header strips.
   No "last-one-wins" special case.
6. **Q6 — tree search**: not part of Wiki. A shared filter over both trees is
   a follow-up.
7. **Q7 — archive**: nothing to map. Neither tree has an archive affordance
   today, and Wiki does not add one.
8. **Q8 — Wiki icon**: `Section::icon` returns `Icon::Folder` for Wiki. The
   nav stays text-only, so this is only for a future icon pass.
9. **Q9 — tree width migration**: prefer the documents width, then the notes
   width, then the 240 default.

## 14. Deliberately left out

- Any change to `kandrium-core`, the DB schema, the MCP tool surface or the
  egui app.
- Merging the notes and documents roots, moving files between them, or
  showing one tree inside the other.
- Drag-and-drop between halves, tab reordering, tab pinning.
- A new archive or trash for notes/documents.
- Per-half tree widths (one shared width keeps the detail width stable).
- Theming changes: the merged layout reuses the existing tokens and chrome.
- Any new keyboard shortcut for collapsing a half or switching the split.
