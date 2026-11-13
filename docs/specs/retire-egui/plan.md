# Plan: retire the egui app

Status: S0 audit complete (see §8); implementation not started.
Scope: remove `crates/app` (`kandrium-app`, eframe/egui) and promote
`crates/app-gpui` (`kandrium-gpui`) to the only desktop front-end, keeping the
`app.kandrium.desktop` bundle identity. `kandrium-core`, `kandrium-mcp` and
`pdf-render` are unchanged.

## 1. Goal

One desktop binary, one packaging config, one dev bundle, one documented way to
run the app. Success means a clean checkout builds and ships only the gpui app,
`cargo test --workspace` still covers everything, and a user upgrading from the
egui build keeps their database and their app identity.

## 2. Ground truth

| Fact | egui (`crates/app`) | gpui (`crates/app-gpui`) |
| --- | --- | --- |
| Package / binary | `kandrium-app` | `kandrium-app-gpui` / `kandrium-gpui` |
| Workspace | root member | separate workspace, root `exclude` |
| Sections | Tasks, Projects, Notes, Documents, Clocks (`src/app.rs` L53–62) | Tasks, Clocks, Wiki, Projects (`src/app.rs` L60–108) |
| Settings | eframe `app.ron`, app id `Kandrium` | `gpui-settings.conf` beside the DB |
| Database | `<data_dir>/kandrium/kandrium.db` via `core::db::resolve_db_path` | same function, same path |
| Packager | root `packager.json`, `identifier: app.kandrium.desktop` | `crates/app-gpui/packager.json`, same identifier |
| Dev bundle | `target/Kandrium-dev.app`, `app.kandrium.desktop.dev` | `crates/app-gpui/target/KandriumGpui-dev.app`, `app.kandrium.desktop.gpui-dev` |

The two front-ends share the database path and the production bundle id but
not the settings file or the dev bundle id. That is the core of the upgrade
story in §6.

## 3. Parity audit

Legend: **Covered** — the gpui app does it (possibly with different code);
**Missing** — the gpui app does not do it today; **Dropped** — deliberately not
carried over; **Unverified** — evidence was insufficient, must be checked
before deletion.

### 3.1 Sections and views

| Capability | egui source | gpui status |
| --- | --- | --- |
| Tasks board, columns, cards, drag and drop | `views/board.rs`, `widgets/{card,column}.rs`, `dnd.rs`, `anim.rs` | **Covered** — `views/tasks.rs`, `views/card_modal.rs` |
| Virtual "All" board | `views/all_board.rs` | **Covered** — `Store::all_board` (`store.rs` L522) |
| Archived page + archive-all | `views/archive.rs` | **Covered** — `views/archive.rs` |
| Projects tree, folders, identifiers, project board | `views/projects.rs` | **Covered** — `views/projects.rs` |
| Notes tree + tabs | `views/notes.rs` | **Covered**, folded into Wiki — `views/wiki.rs` + `views/notes.rs` |
| Documents tree, PDF tabs, Markdown tabs, uploads | `views/documents.rs`, `pdf.rs` | **Covered**, folded into Wiki — `views/documents.rs`, `views/documents_pdf.rs` |
| PDF annotation sidecars | `pdf.rs` | **Covered** — annotation kinds verified, §8.1 |
| Clocks (cities, people, travel, reorder) | `views/wclock.rs` | **Covered** — `views/clocks.rs`; parity of every relocation menu item **Unverified** |
| Clock time probe | — (does not exist) | gpui has it; no regression on retirement |

Notes and Documents are separate sections on the egui side and one Wiki section
with two halves on the gpui side. This is a structural change, not a capability
loss; the plan treats it as covered and calls it out in the release note.

### 3.2 Dialogs and destructive flows

| Flow | egui | gpui status |
| --- | --- | --- |
| Card modal (title, description, project/area, archive) | `widgets/card_modal.rs` | **Covered** — `views/card_modal.rs` |
| Confirmations (delete card, delete column, archive all) | `widgets/confirm.rs` | **Covered** — `CardConfirm`, `ConfirmState` |
| Column create/rename/delete/move | app-only in both clients | **Covered**; menu parity **Unverified** |
| Board rename/delete/create | `views/board.rs` | **Covered**; every entry point **Unverified** |
| File upload / new note / new folder pickers | `rfd` in `views/documents.rs` | **Covered** — `rfd` in `views/documents.rs` |
| Command palette / settings window | none found | none in either; not a gap |

### 3.3 Editors

| Feature | egui | gpui status |
| --- | --- | --- |
| Markdown edit/preview | `widgets/markdown.rs` (egui_commonmark + syntect) | **Covered** — `gpui_kit` markdown; highlighting verified, §8.3 |
| Vim modal editing | `widgets/vim.rs` | **Covered** — `src/vim.rs`; key surface verified, §8.2 |
| Autosave debounce | notes/project docs | **Covered** — 400 ms debounce in notes/documents/projects |
| Card description markdown | `widgets/card_modal.rs` | **Covered** |
| PDF annotations | `pdf.rs` | **Covered** — kinds verified, §8.1 |

### 3.4 Keyboard paths

| Path | egui | gpui status |
| --- | --- | --- |
| Vim in notes, docs, card description | yes | **Covered** |
| Section switching / board shortcuts | none (verified, §8.4) | none; no gap |
| `Esc` cancellation in editors and menus | per-widget | **Covered**; clock probe and pickers verified recently |
| Search focus shortcut | none found | none; not a gap |

### 3.5 Persisted settings

egui writes an eframe `SavedState` (`crates/app/src/app.rs` L96–138) to
`<data_dir>/Kandrium/app.ron` (`eframe` storage dir uses the `run_native` app
name, `"Kandrium"`). Keys: `active_board_id`, `dark_mode`, `section`,
`notes_open/notes_active`, `docs_open/docs_active`, `projects_selected`,
`projects_expanded`, `projects_docs_expanded`, `notes_tree_open`,
`docs_tree_open`, `projects_tree_open`, `projects_docs_open`, `vim_mode`,
`clocks_use_24h`.

gpui writes `gpui-settings.conf` beside the DB with: `vim_mode`, `section`,
`titlebar_clock`, `wiki_tree_open`, `wiki_tree_width`, `projects_tree_open`,
`projects_tree_width`, `wiki_notes_collapsed`, `wiki_documents_collapsed`,
`wiki_split`, `tasks_archive`. Open tabs live in `gpui-documents.session`.

| egui key | gpui status |
| --- | --- |
| `vim_mode` | **Covered** (`vim_mode`) |
| `section` | **Covered**; `notes`/`documents` map to Wiki |
| `notes_tree_open` / `docs_tree_open` | **Covered** by one `wiki_tree_open` |
| `projects_tree_open` | **Covered** |
| `notes_open` / `docs_open` / active tab | **Covered** by `gpui-documents.session` |
| `active_board_id` | **Missing** — gpui does not restore the last board |
| `projects_selected` / `projects_expanded` | **Missing** — gpui does not restore project selection |
| `projects_docs_open` | **Missing** — gpui pins the Overview tab |
| `clocks_use_24h` | **Missing** — `use_24h` is in memory, default 24h |
| `dark_mode` | **Missing** — gpui ships dark only; a light palette exists in `theme.rs` but no toggle was found (**Unverified**) |

These are preference-level gaps, not data gaps. §4/S3 decides per key.

### 3.6 Packaging

| Item | egui | gpui | Retirement action |
| --- | --- | --- | --- |
| Packager config | root `packager.json` | `crates/app-gpui/packager.json` | One root config drives the gpui build |
| Binary | `target/release/kandrium-app` | `crates/app-gpui/target/release/kandrium-gpui` | Rename gpui's binary to `kandrium-app` |
| Identifier | `app.kandrium.desktop` | `app.kandrium.desktop` | Keep — no stranded install |
| Icons | `crates/app/packaging/*.png` | `../app/packaging/*.png` (cross-crate) | Move into the gpui crate |
| Product / version / min macOS | Kandrium / 0.1.0 / 10.15 | same | Keep |

### 3.7 Dev bundle wrapper

- egui: `target/Kandrium-dev.app` → symlink
  `Contents/MacOS/kandrium-app` → `../../../debug/kandrium-app`, id
  `app.kandrium.desktop.dev`, icon from `crates/app/packaging/icon.icns`
  (AGENTS.md L181–193).
- gpui: `crates/app-gpui/target/KandriumGpui-dev.app` → `../../../debug/kandrium-gpui`,
  id `app.kandrium.desktop.gpui-dev`, documented in
  `crates/app-gpui/README.md` L27–45.

Both exist today. After retirement there must be exactly one.

### 3.8 Assets and icons

`crates/app-gpui` has compile-time includes that point **into the egui crate**:

- `src/theme.rs` L275–278: `include_bytes!("../../app/assets/fonts/Inter-*.ttf")`.
- `src/assets.rs` L50+: `include_bytes!("../../app/assets/icons/*.svg")` for
  every `Icon` variant.

Deleting `crates/app` before moving these breaks the build. The packaging PNGs
and `icon.icns` are the same kind of dependency at packaging time, not compile
time. This is the single largest hidden dependency in the removal.

### 3.9 Tests

- egui: 152 `#[test]` functions inside `crates/app/src` (settings serde, vim,
  board/card rules, layout helpers, importers).
- gpui: 171 tests across `src` unit tests and `tests/*.rs` (shell, tasks,
  clocks, wiki, notes, documents, settings, watcher).
- Overlap is high but not proven per module. **The 152-test inventory is a
  required S0 artifact**, not an assumption; deletion is blocked until each
  egui-only invariant is either already covered or ported.

### 3.10 Cross-crate dependencies

| Consumer | Dependency on `crates/app` |
| --- | --- |
| `crates/app-gpui` | fonts + icon SVGs via `include_bytes!`; packaging icons via `packager.json` |
| `crates/mcp` | none (`kandrium-core` only) |
| root `justfile` | `run` recipe builds `kandrium-app` |
| root `packager.json` | binary + icon paths |
| `AGENTS.md`, `README.md` | commands, dev bundle, `kandrium-app` naming |
| `docs/specs/**` | historical references only |

No crate declares a Cargo dependency on `kandrium-app`. **Unverified:** any
out-of-tree scripts or CI outside this repository.

## 4. Stage order

Every stage is one commit and leaves the tree green; the egui app keeps
building until S4 because S1–S3 copy rather than move.

### S0 — Audit artifacts (this document)

- Expand §3.9 into a test-by-test inventory (`egui test -> gpui/core
  counterpart or PORT`). Commit the list next to this plan.
- Run the manual smoke matrix on the gpui app before anything else: every
  section, card create/edit/archive, board and column management, note and
  document editing, PDF annotation, project doc, clocks travel relocation,
  and a restart with restored settings.
- Tag `pre-egui-removal` in git for a one-command revert point.

### S1 — Move shared files into the gpui crate

- Copy `crates/app/assets/fonts/**` and `crates/app/assets/icons/**` to
  `crates/app-gpui/assets/`; rewrite the two `include_bytes!` prefixes.
- Copy `crates/app/packaging/**` to `crates/app-gpui/assets/packaging/` and
  update the crate-local `packager.json` icon paths.
- Leave the originals in place, so `crates/app` still compiles.
- Gates: gpui build/tests, `cargo packager --config crates/app-gpui/packager.json`
  dry run, and a packaged `.app` that shows its icon.

### S2 — Port the tests that only exist on the egui side

- Move pure-logic tests (settings defaults, vim edge cases, day boundaries,
  archive filters, importer rules) to `kandrium-core` where the logic already
  lives.
- Port shell-level cases to `crates/app-gpui/tests/`.
- Add the missing `clocks_use_24h` persistence test and an
  "open an egui-created database" boot test.
- Gate: the ported list from S0 is fully green before S4.

### S3 — Settings bridge and the small gaps

- Add a one-time importer that reads `<data_dir>/Kandrium/app.ron` when
  `gpui-settings.conf` is absent and maps `vim_mode`, `section`,
  `notes_tree_open`/`docs_tree_open` → `wiki_tree_open`, `projects_tree_open`,
  `clocks_use_24h`. Leave `app.ron` untouched.
- Persist `use_24h` in `gpui-settings.conf` (`clocks_use_24h`), defaulting to
  on for new installs and importing the old value.
- Decide `active_board_id`/project selection/`projects_docs_open` per §7 Q6;
  record the outcome in this file.
- Rollback: the importer is additive; deleting the call restores previous boot.

### S4 — Workspace and command takeover (point of no return)

- Root `Cargo.toml`: add `crates/app-gpui` to `members`, drop `crates/app` and
  the `exclude`, keep `resolver = "2"`.
- Rename the gpui package and binary to `kandrium-app` (update
  `kandrium_app_gpui::` imports in tests, `Cargo.lock`, log component names).
- `git rm -r crates/app`; delete the crate-local `packager.json` once the root
  one points at the new binary (S5 may land in the same commit if the
  intermediate build would be unpackageable).
- `justfile`: `run` builds/runs `kandrium-app` from the root workspace; add
  `test`, `lint`, `fmt` recipes.
- Gates: `cargo fmt --all`, `cargo clippy --workspace --all-targets`,
  `cargo test --workspace`, plus the gpui integration suite.

### S5 — Packaging and bundles

- Root `packager.json`: `beforePackagingCommand` builds `-p kandrium-app`,
  `binaries[0].path` is `target/release/kandrium-app`, icons point at
  `crates/app-gpui/assets/packaging/`, identifier stays
  `app.kandrium.desktop`.
- Recreate `target/Kandrium-dev.app` with `Contents/MacOS/kandrium-app` →
  `../../../crates/app-gpui/target/debug/kandrium-app` and the moved
  `icon.icns`; keep id `app.kandrium.desktop.dev`.
- Remove `crates/app-gpui/target/KandriumGpui-dev.app` and unregister it
  (`lsregister -u`) so LaunchServices has one dev entry.
- Verify: `open -n target/Kandrium-dev.app`, DB writes work, icon in the Dock,
  `cargo packager --config packager.json` produces
  `target/release/bundle/.../Kandrium.app` + DMG.

### S6 — Docs

- `README.md`, `AGENTS.md`, `crates/app-gpui/README.md`: one app, one command
  set, the new dev bundle, the settings-bridge behaviour, and the Notes and
  Documents → Wiki merge.
- Leave historical `docs/specs/**` untouched; they describe what was built at
  the time.

### S7 — Sweep

- `cargo tree -d` and a `Cargo.lock` review: `eframe`, `egui`,
  `egui_commonmark`, `egui_extras`, `syntect`, `objc` should be gone.
- `git grep -n "egui\\|eframe\\|egui_commonmark"` must only hit historical
  specs.
- Final gates and one last manual smoke pass on the packaged app.

## 5. Commands after removal

```sh
cargo run -p kandrium-app              # or: just run
cargo test --workspace                 # core + mcp + app
cargo clippy --workspace --all-targets
cargo fmt --all --check
cargo packager --config packager.json  # Kandrium.app + DMG
```

If the workspace merge is deferred (Q2 fallback):

```sh
cargo test --manifest-path crates/app-gpui/Cargo.toml
```

## 6. Risks and mitigations

| Risk | Why it bites | Mitigation |
| --- | --- | --- |
| `include_bytes!` into `crates/app` (fonts, icons) | Deleting the crate breaks the gpui build at compile time | S1 copies first and repoints; CI builds the gpui crate alone before S4 |
| `packager.json` icons under `../app/packaging` | The DMG builds without an icon or fails path validation | S1 moves them into the gpui crate; S5 dry-runs `cargo packager` and inspects `Contents/Resources` |
| 152 egui tests disappear | Silent coverage loss in vim, settings, board and importer rules | S0 inventory, S2 port, and a gate that the list is green before deletion |
| Settings split (`app.ron` vs `gpui-settings.conf`) | A user sees a different section, vim off/on, or a collapsed tree and reads it as data loss | S3 one-time importer; release note says "preferences reset once, boards/cards untouched" |
| DB path differs | Would look like an empty app | Both already call `core::db::resolve_db_path` (`<data_dir>/kandrium/kandrium.db`); add a boot test against an egui-created DB |
| Bundle id change | An existing install is stranded next to the new app | Keep `app.kandrium.desktop` in both packagers; assert it in review; never ship the `.gpui` id |
| Workspace merge dependency conflict | gpui pins its own `gpui-kit` stack; unifying may bump `rusqlite`/`jiff` | S4 runs `cargo tree -d` first; fallback is Q2's separate-workspace option |
| Two dev bundles with different ids | LaunchServices launches the stale one; clicks go to an old build | S5 deletes `KandriumGpui-dev.app` and unregisters it, leaving only `target/Kandrium-dev.app` |
| `clocks_use_24h` default flips | Users see 24h where egui showed 12h | Import the value in S3; only new installs get the 24h default |
| Binary rename breaks habits/scripts | `just run` or external scripts reference `kandrium-gpui`/`kandrium-app` | Keep `just run` stable, document the change; outside scripts are **Unverified** and must be grepped by the user |
| Historical docs mention egui | Readers think both apps still exist | README/AGENTS updated in S6; specs stay historical with a status line |

## 7. Open questions (suggested default)

1. **Package name after takeover** — rename the package to `kandrium-app`, or
   keep `kandrium-app-gpui` and rename only the binary? Default: full rename;
   the lib import in tests is a mechanical sed, and log components read better.
2. **Workspace layout** — merge into the root workspace, or keep the separate
   one and run it from `just`? Default: merge; the original reason for the
   split (protecting the egui build) disappears.
3. **Settings migration** — import `app.ron` once, or start fresh? Default:
   import the five mappable keys; never touch the file.
4. **Bundle id** — keep `app.kandrium.desktop` or ship a new one? Default:
   keep; this is what makes the upgrade in-place.
5. **Dev bundle** — reuse `target/Kandrium-dev.app`, or keep the gpui-local
   bundle and delete the old one? Default: reuse the root path; one dev entry.
6. **Unmigrated preferences** (`active_board_id`, project selection,
   `projects_docs_open`, `dark_mode`) — implement now or accept the reset?
   Default: accept the reset for the first release, record it in the release
   note, and keep the light palette as a backlog item.
   **Taken:** accepted. `vim_mode`, `section`, the merged tree open flag,
   `projects_tree_open` and `clocks_use_24h` are imported once;
   the four preferences above reset once and the release note says so.
7. **Light theme** — port the `dark_mode` toggle before deleting egui? Default:
   no; gpui ships dark only, and the tokens for light already exist.
   **Taken:** no toggle; dark only.
8. **Test port scope** — port every non-duplicated egui test, or only
   core-level invariants? Default: every non-duplicated invariant; delete the
   widget-specific ones that exercise egui layout.
9. **Packaging asset home** — `crates/app-gpui/assets/packaging/` or a root
   `assets/` shared by future crates? Default: inside the gpui crate, the only
   consumer.
10. **Archive tag** — keep a `pre-egui-removal` tag or rely on history?
    Default: tag before S4.

## 8. S0 verification findings

Checked against the source on 2026-11-13. Each item below was **Unverified**
in §3 and is now resolved; the §3 rows point here.

### 8.1 PDF annotation-kind parity — verified, covered

- The sidecar model is core-owned: `AnnotationKind` has exactly
  `Highlight` and `Note` (`crates/core/src/documents.rs` L36–39), so both
  front-ends write the same wire format.
- Both use the same four halo colours
  `["#f6d448", "#a5d6a7", "#8ab4f8", "#f6a6c1"]` (egui
  `crates/app/src/views/documents.rs` L51; gpui
  `crates/app-gpui/src/views/documents.rs` L63), highlighting with
  `NOTE_COLORS[0]` and placing notes with `NOTE_COLORS[3]`.
- Both expose `Select` / `Highlight` / `Note` tools, a 20 pt note anchor, a
  90/255 highlight alpha, a popover with memo editing, colour choice and
  delete, and a debounced `core::documents::save_annotations` write.
- No annotation capability is lost by retiring egui.

### 8.2 Vim key surface — verified, covered; test depth is not

- `crates/app-gpui/src/vim.rs` L23–33 documents the ported command set, and
  the engine functions are the same code shape in both files: `motion_for`,
  `visual_operate`, `escape`, `special_key`, `parse_ex`, `cancel_command`,
  the `Buf` / `VimState` model, and the same ex parser (`:w`, `:q`, `:wq`,
  `:x`, `:xit`; `:q!` reported unknown).
- gpui additionally handles `Ctrl-r` redo, `Ctrl-z` undo, `Ctrl-c`,
  `Ctrl-d`/`Ctrl-u` half-page and named keys; nothing in the egui surface is
  missing.
- The glue differs by design (egui rewrites pending `TextEdit` events in
  `pre_edit`; gpui captures keys outside a read-only editor and writes the
  changed middle back), which is not a capability difference.
- Coverage gap: 47 vim tests on the egui side vs 8 coarser ones on the gpui
  side. S2 ports the missing engine cases; this is the main guard against a
  silent behaviour regression.

### 8.3 Code highlighting — verified, covered with a palette gap

- egui highlights Markdown source with syntect
  (`crates/app/src/views/notes_highlight.rs`, theme `base16-ocean.dark`) and
  renders previews with `egui_commonmark`.
- gpui creates its editors with `.language("markdown")` (`app.rs` L2880,
  L4393, L4966), enables the `tree-sitter-markdown` feature on
  `gpui-component`, and renders previews with `gpui_kit::base::text::markdown`.
  `gpui-component` ships a real highlighter (`src/highlighter/`, including a
  syntect backend).
- Markdown highlighting therefore survives. Two differences to record:
  the colour palette is not pixel-identical to the syntect theme, and only the
  Markdown grammar is enabled, so fenced code blocks in other languages are
  not highlighted. Both are accepted as known fidelity gaps / backlog items;
  no new grammar features are added by this work.

### 8.4 App-level shortcuts outside vim — verified, no gap

- `crates/app/src/app.rs` contains no `Key::` or `Modifiers` handling; the
  only keyboard paths are per-widget (`Esc` in editors, card modal,
  confirmations, popovers). There is no global section or board shortcut to
  lose.
- gpui's shell likewise has only per-view `Escape` handlers and vim capture.
- The egui "`Esc` closes the editor" behaviour is intentionally replaced by
  "Normal-mode `Esc` never closes the document" (already in the gpui vim
  header and working tree), so retirement changes no user-facing shortcut.

### 8.5 Test inventory — 152 egui tests, mapped

| egui module | Tests | gpui counterpart | Verdict |
| --- | --- | --- | --- |
| `widgets/vim.rs` | 47 | `src/vim.rs` (8) | PORT engine cases |
| `views/wclock.rs` | 30 | `views/clocks.rs` (17), `tests/clocks.rs` (5), core `clocks` | mostly covered; PORT formatting, empty states, unknown zone, repaint targets |
| `views/projects.rs` | 20 | `views/projects.rs` (6), `tests/projects.rs` (6) | PORT buckets, autosave, archive, identifier, rename, drop index |
| `widgets/card_modal.rs` | 12 | `views/card_modal.rs` (2) | PORT modal rules and vim-in-description |
| `views/documents.rs` | 11 | `views/documents.rs` (11), `tests/documents.rs` | geometry covered; PORT tab lifecycle, `:w`/`:q`, create/import |
| `dnd.rs` | 8 | `views/clocks.rs` slot math, `views/projects.rs` insert index | PORT card-list gap math where a gpui helper exists |
| `anim.rs` | 8 | `src/anim.rs` (7) | PORT the two generic timing cases; DROP egui paint-rect tests |
| `views/board.rs` | 5 | `views/tasks.rs` (7) | PORT mirrored drop-anchor invariant |
| `widgets/card.rs` | 4 | card rendering | PORT meta-label rules |
| `views/all_board.rs` | 3 | `views/tasks.rs`, `Store::all_board` | PORT add-form/gap rules where logic exists |
| `views/notes.rs` | 2 | `views/notes.rs` | DROP: egui headless duplicate-ID/rail tests |
| `app.rs` | 2 | `src/app.rs` (5) | DROP: egui title-band/segment layout |

`PORT` rows are the S2 checklist; `DROP` rows are egui layout/ID tests with no
gpui code path to assert.

### 8.6 Manual smoke matrix

The automated gate covers build, clippy and tests. The plan's manual matrix
(every section, card create/edit/archive, board and column management, note and
document editing, PDF annotation, project doc, clocks relocation, restart with
restored settings) still needs a human pass on the gpui bundle: screen capture
is blocked from the agent shell. Status: **pending user confirmation**; it does
not block S1.

### 8.7 Tag

`pre-egui-removal` is created in the S0 commit so the whole-app state can be
restored with one command before S4.
