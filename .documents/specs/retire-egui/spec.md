# Spec: retire the egui app

Status: ready for approval. Implementation has not started.
Source plan: [plan.md](plan.md). This spec records the S0 verification results,
the decisions taken from §7's suggested defaults, and the exact changes each
stage makes.

## 1. Decisions (plan §7 defaults, taken as written)

| # | Question | Decision |
| --- | --- | --- |
| 1 | Package name after takeover | Full rename: package and binary both `kandrium-app`. |
| 2 | Workspace layout | Merge `crates/app-gpui` into the root workspace; delete its `[workspace]` table. |
| 3 | Settings migration | One-time import of five keys from `app.ron`; never write `app.ron`. |
| 4 | Bundle id | Keep `app.kandrium.desktop`. |
| 5 | Dev bundle | Reuse `target/Kandrium-dev.app`; delete the gpui-local bundle. |
| 6 | Unmigrated preferences | Accept the reset for `active_board_id`, project selection, `projects_docs_open`, `dark_mode`; record it in the release note. |
| 7 | Light theme | Do not port the toggle; gpui stays dark. |
| 8 | Test port scope | Port every non-duplicated invariant that has a gpui-side surface; drop pure egui layout/ID tests. |
| 9 | Packaging asset home | `crates/app-gpui/assets/packaging/`. |
| 10 | Archive tag | Tag `pre-egui-removal` before S4 (created in S0). |

## 2. S0 verification findings

These four items were **Unverified** in the plan. Each was checked against the
source on 2026-11-13; the plan's §3 rows are updated with the result.

### 2.1 PDF annotation-kind parity — VERIFIED, covered

- Both front-ends serialize only the core kinds
  `AnnotationKind::{Highlight, Note}` (`crates/core/src/documents.rs` L36–39),
  so the sidecar wire format is shared by construction.
- Both use the same four halo colours
  `["#f6d448", "#a5d6a7", "#8ab4f8", "#f6a6c1"]` (egui
  `crates/app/src/views/documents.rs` L51; gpui
  `crates/app-gpui/src/views/documents.rs` L63) and the same default choices:
  highlight `NOTE_COLORS[0]`, note `NOTE_COLORS[3]`
  (egui L1401–1405 / gpui `app.rs` L5760–5764 and L5843–5847).
- Both offer the same three tools (`Select`, `Highlight`, `Note`), the same
  note anchor (`NOTE_ANCHOR = 20.0`), the same highlight alpha (`90`), the same
  popover actions (memo text, colour, delete) and the same debounced sidecar
  save through `core::documents::save_annotations`.
- No annotation capability is lost by retiring egui.

### 2.2 Vim key surface — VERIFIED, covered; test depth is not

- `crates/app-gpui/src/vim.rs` L23–33 states the ported command set, and the
  shared engine functions exist in both files with the same names and
  structure: `motion_for`, `visual_operate`, `escape`, `special_key`,
  `parse_ex`, `cancel_command`, the `Buf`/`VimState` model, and the same ex
  parser (`:w`, `:q`, `:wq`, `:x`, `:xit`; `:q!` unknown).
- gpui adds `Ctrl-r` redo, `Ctrl-z` undo, `Ctrl-c`, `Ctrl-d`/`Ctrl-u`
  half-page, and named-key handling; none of these is missing relative to egui.
- Only the glue differs: egui rewrites pending `TextEdit` events in `pre_edit`
  and uses `TextEditState`; gpui captures keys outside a read-only textarea /
  editor and writes the changed middle back.
- Coverage gap: egui has 47 vim tests, gpui has 8 coarser ones. S2 ports the
  missing engine cases; this is the main mitigation against a silent
  behaviour regression.

### 2.3 Code highlighting — VERIFIED, covered with a palette difference

- egui highlights the Markdown source with syntect (`MarkdownHighlighter`,
  `crates/app/src/views/notes_highlight.rs`, base16-ocean.dark) and renders
  previews with `egui_commonmark`.
- gpui creates its editors with
  `EditorState::new(...).language("markdown")` (`app.rs` L2877–2880,
  L4390–4393, L4963–4966) and enables the `tree-sitter-markdown` feature on
  `gpui-component`; the library ships a real highlighter
  (`gpui-component/src/highlighter/`, including a syntect backend). Preview is
  `gpui_kit::base::text::markdown`.
- Markdown syntax highlighting therefore survives. The colour palette is not
  pixel-identical (syntect theme vs gpui-component's dark highlight theme).
- Difference to record, not a blocker: gpui enables only the Markdown grammar,
  so fenced code blocks written in other languages are not highlighted in the
  editor/preview where syntect would have attempted it. Treated as a known
  fidelity gap and left as a backlog item; no features are added in this work.

### 2.4 App-level shortcuts outside vim — VERIFIED, no gap

- `crates/app/src/app.rs` has no `Key::` or `Modifiers` handling at all; the
  only keyboard paths are per-widget (`Esc` in editors, the card modal,
  confirmations, popovers). There is no section-switch or board shortcut to
  lose.
- gpui's shell likewise has per-view `Escape` handlers and vim capture, no
  global shortcut registry.
- The egui-side `Esc`-closes-editor behaviour is intentionally replaced by
  "Normal-mode `Esc` never closes the document"; this is already implemented
  in the working tree and is documented in the gpui vim header, so retirement
  changes no user-facing shortcut.

### 2.5 Test inventory (152 egui tests)

| egui module | Tests | gpui counterpart | Verdict |
| --- | --- | --- | --- |
| `widgets/vim.rs` | 47 | `src/vim.rs` (8) | PORT engine cases |
| `views/wclock.rs` | 30 | `views/clocks.rs` (17), `tests/clocks.rs` (5), core `clocks` | mostly covered; PORT formatting/empty-state/unknown-zone |
| `views/projects.rs` | 20 | `views/projects.rs` (6), `tests/projects.rs` (6) | PORT relative buckets, autosave, archive, identifier, rename, drop index |
| `widgets/card_modal.rs` | 12 | `views/card_modal.rs` (2) | PORT modal rules + vim-in-description |
| `views/documents.rs` | 11 | `views/documents.rs` (11), `tests/documents.rs` | geometry covered; PORT tab lifecycle, `:w`/`:q`, create/import |
| `dnd.rs` | 8 | `views/clocks.rs` slot math, `views/projects.rs` insert index | PORT card-list gap math only if the gpui helper exists |
| `anim.rs` | 8 | `src/anim.rs` (7) | PORT the two generic timing cases; drop egui paint rects |
| `views/board.rs` | 5 | `views/tasks.rs` (7) | PORT mirrored drop-anchor invariant |
| `widgets/card.rs` | 4 | card rendering | PORT meta-label rules |
| `views/all_board.rs` | 3 | `views/tasks.rs`, `Store::all_board` | PORT add-form/gap rules where logic exists |
| `views/notes.rs` | 2 | `views/notes.rs` | DROP: egui headless duplicate-ID/rail tests |
| `app.rs` | 2 | `src/app.rs` (5) | DROP: egui title-band/segment layout |

Every `PORT` row becomes a checkbox in [tasks.md](tasks.md); every `DROP` row is
an egui-layout test with no gpui code path to assert.

## 3. Stage contract

One commit per stage, in this order. Stages S1–S3 copy rather than move, so
`crates/app` keeps building until S4.

### S0 — Audit artifacts (docs only)

Files: `docs/specs/retire-egui/plan.md` (S0 findings appended),
`docs/specs/retire-egui/spec.md`, `docs/specs/retire-egui/tasks.md`.
Git: annotated tag `pre-egui-removal`.
Verification: `git tag --list pre-egui-removal`; `cargo test --workspace`
untouched (no code change).

### S1 — Assets and packaging move (copy)

Changes:
- `crates/app-gpui/assets/fonts/` ← copy of `crates/app/assets/fonts/`.
- `crates/app-gpui/assets/icons/` ← copy of `crates/app/assets/icons/`.
- `crates/app-gpui/assets/packaging/` ← copy of `crates/app/packaging/`.
- `crates/app-gpui/src/theme.rs` L275–278: `include_bytes!("../assets/fonts/Inter-Regular.ttf")`
  and `include_bytes!("../assets/fonts/Inter-SemiBold.ttf")`.
- `crates/app-gpui/src/assets.rs` L50+: every icon
  `include_bytes!("../assets/icons/<name>.svg")`.
- `crates/app-gpui/packager.json`: icons become `assets/packaging/16x16.png`
  … `assets/packaging/icon@2x.png`.
Verification: `cargo build --manifest-path crates/app-gpui/Cargo.toml`,
`cargo test --manifest-path crates/app-gpui/Cargo.toml`,
`cargo clippy --manifest-path crates/app-gpui/Cargo.toml --all-targets`,
`cargo packager --config crates/app-gpui/packager.json` (release build) and a
check that `Kandrium.app/Contents/Resources` contains the icon set.
Rollback: revert the commit; `crates/app` is untouched.

### S2 — Test port

Adds tests only (no product behaviour changes), per the PORT rows in §2.5:
- `crates/app-gpui/src/vim.rs` tests: ex parsing/error, counted linewise
  operations, delete-to-end/yank, `j`/`k` goal column, find motions and `;`/`,`,
  bracket/paragraph/quote text objects, dot repeat (normal and insert session),
  visual text object, visual line column, block-caret geometry.
- `crates/app-gpui/src/views/clocks.rs` / `tests/clocks.rs`: 12-hour period
  formatting, initials, empty and no-results states, unknown-zone label,
  residents-travel boundary repaint targets.
- `crates/app-gpui/src/views/projects.rs` / `tests/projects.rs`: relative time
  buckets, document autosave, description autosave, archived-project behaviour,
  identifier normalisation, title Enter/Esc, sibling-scoped drop index.
- `crates/app-gpui/src/views/card_modal.rs` / `tests/`: project vs normal modal
  actions, Tab inserts two spaces, Esc/ex behaviour with vim on and off.
- `crates/app-gpui/src/views/documents.rs` / `tests/documents.rs`: tab
  lifecycle, `:w` flush, `:q` flush+close, create folder/markdown file, import
  copy.
- `crates/app-gpui/src/views/tasks.rs`: mirrored project cards are valid drop
  anchors in the unified order; add-form and drag-gap rules for the All board.
- `crates/app-gpui/src/anim.rs`: delayed track progress and no-track scroll
  jump.
- `crates/app-gpui/src/views/cards` (wherever `meta_label` is resolved):
  meta-label placement rules.
- `crates/app-gpui/src/store.rs`: boot test that opens a database created by
  the egui schema/seed path and reads a card.
Verification: gpui fmt/clippy/test, and the ported list green.
Rollback: revert; no production code depends on the new tests.

### S3 — Settings bridge and `clocks_use_24h`

Changes:
- `crates/app-gpui/src/settings.rs`: add `clocks_use_24h: bool` (default
  `true`), persist it in `gpui-settings.conf`, and use it for the clocks
  view/probe formatting.
- Add `import_egui_settings()`: only when `gpui-settings.conf` is absent, read
  `<data_dir>/Kandrium/app.ron`, and map `vim_mode`, `section`
  (`notes`/`documents` → `wiki`), `notes_tree_open || docs_tree_open` →
  `wiki_tree_open`, `projects_tree_open`, `clocks_use_24h`. Tolerant
  line/key parser, unknown keys ignored, malformed file ignored, `app.ron`
  never written. Call it from the existing settings load path in
  `crates/app-gpui/src/settings.rs`.
- Record in `plan.md` that Q6 preferences reset once and dark mode stays out.
Verification: unit tests for import/skip/malformed/round-trip, gpui gates, and
a boot against a temporary data dir containing an `app.ron` fixture.
Rollback: remove the call; the conf file is untouched.

### S4 — Workspace takeover and deletion (point of no return)

Changes:
- Root `Cargo.toml`: members `["crates/core", "crates/mcp", "crates/app-gpui",
  "crates/pdf-render"]`; delete `exclude`.
- `crates/app-gpui/Cargo.toml`: delete the inner `[workspace]` and
  `[workspace.package]`; keep inherited `version`/`edition`/`license`;
  package `kandrium-app`; `[[bin]] name = "kandrium-app"`.
- `crates/app-gpui/src/main.rs`: log component `kandrium-app`.
- All `kandrium_app_gpui::` imports in `tests/*` → `kandrium_app::`.
- `git rm -r crates/app`.
- `justfile`: `run` builds/runs `kandrium-app`; add `test`, `lint`, `fmt`.
Verification: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`,
`cargo test --workspace`, `cargo build --bin kandrium-app`.
Rollback: revert the commit and restore `pre-egui-removal` if anything is
found later; S1–S3 commits remain independent.

### S5 — Packaging and one dev bundle

Changes:
- Root `packager.json`: `beforePackagingCommand` builds `-p kandrium-app`;
  binary `target/release/kandrium-app`; icons
  `crates/app-gpui/assets/packaging/*`; identifier unchanged.
- Delete `crates/app-gpui/packager.json`.
- Recreate `target/Kandrium-dev.app`: `Contents/MacOS/kandrium-app` →
  `../../../debug/kandrium-app`, id `app.kandrium.desktop.dev`, moved
  `icon.icns`.
- Remove `crates/app-gpui/target/KandriumGpui-dev.app` and unregister it.
Verification: `open -n target/Kandrium-dev.app` (outside the sandbox), a card
write persists, Dock icon is the Kandrium icon; `cargo packager --config
packager.json` produces `Kandrium.app` + DMG.
Rollback: revert; the previous dev bundle can be recreated from the tag.

### S6 — Docs

`README.md`, `AGENTS.md`, `crates/app-gpui/README.md`: one app, one command
set, `target/Kandrium-dev.app`, the settings bridge, the Notes/Documents →
Wiki merge, and gpui conventions replacing the egui-specific ones.
Historical `docs/specs/**` stays untouched.

### S7 — Sweep

`cargo tree -d` and `Cargo.lock` review: `eframe`, `egui`, `egui_commonmark`,
`egui_extras`, `syntect`, `objc` gone. `git grep -n "egui\|eframe\|egui_commonmark"`
hits only historical specs. Final fmt/clippy/test and one packaged smoke pass.

## 4. Verification gates per stage

| Stage | fmt | clippy | tests | build | runtime |
| --- | --- | --- | --- | --- | --- |
| S0 | — | — | `cargo test --workspace` | — | — |
| S1 | gpui | gpui `--all-targets` | gpui | gpui | package + icon |
| S2 | gpui | gpui `--all-targets` | gpui | gpui | — |
| S3 | gpui | gpui `--all-targets` | gpui | gpui | boot with fixture |
| S4 | root `--all` | root `--workspace` | root `--workspace` | root | — |
| S5 | root `--all` | root `--workspace` | root `--workspace` | release | dev bundle + DMG |
| S6 | — | — | `cargo test --workspace` | — | — |
| S7 | root `--all` | root `--workspace` | root `--workspace` | release | packaged smoke |

## 5. Rollback and reporting

- Each stage is one commit; S1–S3 are additive and reversible alone.
- `pre-egui-removal` is the whole-app restore point.
- Report at every stage boundary with the gate output summary; report
  immediately if a gate regresses or reality contradicts the audit.

## 6. Approval

Required before execution: approve this spec and [tasks.md](tasks.md). Nothing
outside S0's documentation changes has been made yet.
