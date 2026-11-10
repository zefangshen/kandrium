# Kandrium — egui → gpui Migration Plan

Status: **planning only, not approved** · 2026-11-10 · companion to
[plan.md](plan.md) and [AGENTS.md](AGENTS.md)

This document evaluates replacing the eframe/egui desktop UI with
[gpui](https://crates.io/crates/gpui) + [gpui-kit](https://github.com/longbridge/gpui-kit).
It is deliberately pessimistic: the cost side is quantified from the actual
code, the benefit side is not taken on faith.

---

## 1. Verdict

**Do not migrate now.** gpui-kit is the only credible component stack, but
adopting it means rewriting **~22,750 lines** of UI containing **~2,083 egui
API call sites**, including a 2,902-line vim editor built on `egui::TextEdit`
internals that has no counterpart in gpui's text model — in exchange for no
capability this app is currently blocked on.

Migrate only if **all** of these become true:

1. A concrete product requirement is *blocked by egui* — not "gpui is nicer".
   Today the app is a 940×600-minimum single window with a Kanban board, three
   document surfaces and a clock grid; none of those are at egui's limits.
2. Someone owns the vim editor rewrite as a dedicated, multi-phase workstream.
3. You accept tracking a **third-party gpui fork** (Longbridge's `gpui-pre`)
   as your UI foundation, indefinitely.

It is a mistake if done:

- **before** there is CI and a tagged release (the egui app is currently
  unverifiable except by hand — see §13),
- to chase "native feel" or animation polish: the hand-rolled `anim.rs` already
  delivers the Trello-style motion, and gpui makes that class of work *harder*,
  not easier (§5.2),
- with the expectation that the vim editor survives intact. It does not (§5.1).

**The honest alternative:** the cheapest improvement per unit of risk is to
keep egui and spend the same effort on the real gaps in
[plan.md](plan.md) §"Known gaps" — CI, board lifecycle, find/filter, PDF text
selection. A UI-toolkit migration should be a *response to a measured limit*,
and no such measurement exists in this repo.

Mapping every hard part to Zed's reference implementation (§6) lowers execution
**risk** — drag/drop and animation move from High to Medium, tests from High to
Medium — but not the **volume**: it remains ~22.7k lines of second
implementation. The verdict below is unchanged by it.

---

## 2. What actually has to be rewritten

`crates/app/src`, 25 files, 22,750 lines. "egui sites" = occurrences of
`ui.` + `ctx.` + `egui::` (a proxy for API surface, not a precise LOC count).

| File | Lines | egui sites | What it is |
| --- | ---: | ---: | --- |
| `views/projects.rs` | 3,731 | 372 | Tree, Overview, doc tabs, inline title edit |
| `views/wclock.rs` | 3,179 | 245 | Clocks grid, picker, inspector, travel forms |
| `widgets/vim.rs` | 2,902 | 157 | Vim modal layer over `TextEdit` |
| `app.rs` | 2,116 | 117 | Shell, nav, actions, `SavedState` |
| `views/documents.rs` | 1,973 | 189 | PDF/MD tabs, texture cache, annotations |
| `widgets/card_modal.rs` | 1,022 | 211 | Card editor + markdown description |
| `anim.rs` | 964 | 63 | FLIP tracks, drag ghost, gap |
| `views/all_board.rs` | 907 | 95 | Virtual All board |
| `views/board.rs` | 888 | 81 | Column h-scroll, card DnD |
| `theme.rs` | 851 | 120 | Tokens, fonts, `text_styles`, widgets |
| `views/notes.rs` | 762 | 95 | Notes tree + tabs + autosave |
| `pdf.rs` | 679 | **2** | hayro render worker (**portable**) |
| `widgets/card.rs` | 586 | 117 | Card chip |
| `widgets/column.rs` | 391 | 46 | Column frame |
| `views/archive.rs` | 360 | 43 | `egui_extras` table |
| `dnd.rs` | 349 | 13 | Hit-test math (**nearly portable**) |
| `main.rs` | 338 | 1 | objc titlebar/dock, `eframe::run_native` |
| `icons.rs` | 196 | 17 | Lucide SVG table |
| `widgets/markdown.rs` | 129 | 30 | Editor/preview body |
| `widgets/tabs.rs` | 121 | 32 | Tab strip |
| `views/notes_highlight.rs` | 87 | 6 | syntect → `LayoutJob` |
| `widgets/confirm.rs` | 80 | 11 | Confirm dialog |
| `widgets.rs` | 66 | 19 | Shared constants/helpers |
| `watcher.rs` | 63 | 1 | `PRAGMA data_version` poll |
| `views.rs` | 10 | 0 | Module list |

Tests: **156** in `crates/app` by `#[test]` count (119 in `crates/core`, 1 in
`crates/mcp`; AGENTS.md's "264" figure is stale relative to the tree). Ten
modules build a headless `egui::Context` for geometry assertions: `vim` (47),
`wclock` (30), `projects` (20), `card_modal` (12), `documents` (11), `dnd` (8),
`anim` (8), `board` (5), `card` (4), `all_board` (3), `notes` (2), `app` (2).

Rough split by portability:

- **Ports with edits (~15%)**: `pdf.rs` (2 egui sites — a repaint handle),
  `dnd.rs` rect math, `anim.rs` easing/stagger/track math, `vim.rs` state
  machine, `theme.rs` token values, `notes_highlight.rs` styling data,
  `watcher.rs`, and everything in `kandrium-core`.
- **Rewritten (~85%)**: all layout, all widget composition, all input handling,
  all painting. That is not an editing task; it is a second UI implementation.

---

## 3. Dependency decision

Three options, no fourth.

| Option | Crate identity | Component coverage | Verdict |
| --- | --- | --- | --- |
| **Stay on egui 0.36** | `egui`/`eframe` (registry, Zed) | Hand-rolled, already built | **Recommended** |
| **gpui-kit** | `gpui-kit` + **`gpui-pre =0.3.12`** (Longbridge's fork) | 75+ components, weekly releases, 20.1k stars, v0.9.1 (2026-11-09) | Only viable migration target |
| **gpui-ce** + `gpui_ce_components` | `gpui-ce` 0.3.0 fork + separate components crate | Smaller, community fork | Worst of both: fork *and* thinner components |
| **`guise-ui`** | registry `gpui ^0.2.2` | Small | Dead end — pins the year-stale 0.2.2 |
| **`adabraka-ui`** | registry gpui | Small | Dormant since Feb 2026 — not an option |

**Recommendation if migration happens: gpui-kit**, and only gpui-kit. It is the
only library with the breadth (virtualized table, dock, tree-sitter editor,
markdown, theming) to shorten this rewrite, and the only one shipping weekly.

### The fork-identity risk, stated plainly

- crates.io `gpui` is **0.2.2, published 2025-10-22 — 12+ months stale**, and
  marked `trustpub_only: true`. You cannot realistically get a current gpui
  from the registry.
- gpui-kit does **not** depend on registry `gpui`. It pins **`gpui-pre =0.3.12`**,
  published by Longbridge, not Zed.
- Consequence: those are **two different crate identities**. `gpui 0.2.2` and
  `gpui-pre 0.3.12` cannot be mixed — types, `TypeId`s and trait impls are
  disjoint. There is no adapter, no `From`, no bridging layer short of a
  rewrite. Choosing gpui-kit means *inheriting* its gpui snapshot as the
  app's UI runtime.
- Upstream drift: Zed's fixes land in Zed's monorepo and reach you only when
  Longbridge republishes. You are on their schedule, at their snapshot, with
  their patch set (and there is a second, rendering-optimised fork `gpui-fast`
  in the same ecosystem — another divergence vector).
- Version churn: gpui-kit ships **weekly**. Pin `=0.9.x` exactly, commit
  `Cargo.lock`, and treat every bump as a small port. Both sides are 0.x: no
  semver stability, no deprecation runway.
- Verify in the spike (§11): the exact package/`package =` rename, whether
  `gpui-pre` re-exports as `gpui`, and whether gpui-kit's editor already has a
  vim mode (that single fact moves §5.1 from XL to M/L — and hands your editor
  semantics to a third party).

---

## 4. Feature-by-feature portability

Effort: **S** ≤ a day of focused work, **M** a few days, **L** a week-plus,
**XL** a workstream. No calendar estimates — the sizes are relative. Risk is
about *unknowns and regression surface*, not difficulty.

| Feature | egui mechanism | gpui / gpui-kit mechanism | Effort | Risk |
| --- | --- | --- | --- | --- |
| App shell, 5 sections, header | `CentralPanel` + `theme::segmented_control` | gpui `div()` tree + gpui-kit tabs/segmented control | M | Low — pure layout |
| Custom titlebar band | `fullsize_content_view` + objc `traffic_light_metrics()` measuring NSWindow buttons | gpui `TitlebarOptions` (`appears_transparent`, `traffic_light_position`) | S | **Lower than today** — deletes ~150 lines of objc |
| Board: 3 columns, h-scroll | `ScrollArea::horizontal` + `allocate_ui_with_layout` workarounds | `overflow_x_scroll` + flex row; nested infinite-width trap disappears | L | Medium — the layout *idioms* differ, not just the syntax |
| Card chip, inline title edit | `Frame` + `TextEdit` singleline | styled `div` + the shared single-line editor (Zed `ui_input::InputField`) | S/M | Low |
| Card modal (+ markdown desc) | `egui::Modal`, fixed-height editor, `CommonMarkViewer` | gpui `Modal`/overlay + gpui-kit markdown | L | Medium |
| **Vim editor** | `pre_edit` rewrites pending `Event`s, `TextEditState` caret, galley block caret, own undo stack | Vim as a separate entity driving a shared editor (Zed `crates/vim`), not a layer inside the input | **XL** | Medium — see §5.1, §6.4 |
| Markdown preview | `egui_commonmark` 0.25 + `better_syntax_highlighting` | gpui-kit markdown component | M | Medium — loses syntect theming parity (§5.3) |
| Markdown source highlight | `syntect` → `LayoutJob` | gpui text styling runs / tree-sitter alternative | M | Medium |
| Notes / Documents trees | `CollapsingHeader`-style rows, collapse rail | gpui-kit tree | L | Low/Medium |
| Documents tabs + autosave | `tabs.rs`, debounce in `logic()` | entity timers (`cx.spawn` + `Timer`) | M | Low |
| **PDF view + annotations** | `pdf.rs` worker → `ColorImage` → `TextureHandle`; selection quads painted | same worker → `RenderImage`/`ImageSource`; overlay via `div` quads (Zed `image_viewer`) | L | Medium-Low — backend reused, surface redrawn (§5.4) |
| Projects tree + Overview | 3,731 lines of tree/tab/inline-edit | gpui-kit tree + docks, custom Overview form | XL | Medium/High |
| Clocks grid + drag reorder | wrapping grid, `dnd::GridZone`, FLIP | flex-wrap + `GridZone` port + new animation host | L | Medium |
| Drag-and-drop | hand-rolled: `Response.dragged()`, `DropZone`, `Area` ghost | gpui `on_drag`/`drag_over`/`on_drop` typed payloads (Zed `pane.rs` tabs), keep own insertion-index math | **M/L** | Medium (§5.2, §6.4) |
| Drag animation | `PaintList::transform_range` translating painted shapes | per-element `AnimationExt::with_animation` — **no paint-list hack** | **M** | Medium — signature feel (§6.4) |
| Theming | `Tokens` + `Visuals` + `text_styles` table | gpui-kit `Theme` + `LabelSize`-style enums (Zed `crates/theme`) | M | Low |
| Fonts (Inter, JetBrains Mono) | `FontDefinitions`, named families | gpui `TextSystem` font registration | S/M | Low |
| Lucide icons | `egui::Image` + SVG loader, runtime tint | gpui `svg()` + `AssetSource`/`SvgRenderer`, `text_color` tint | S | Low |
| Archive table | `egui_extras::TableBuilder` | gpui-kit virtualized table / Zed `DataTable` | **S/M** | Low |
| Toasts / confirm | bottom-center toast, `confirm.rs` | gpui-kit notification/dialog | S | Low |
| Search / filter pickers (Clocks city, board filter, card project picker) | bespoke `TextEdit` + list | gpui-kit picker / `command_palette` | S/M | Low |
| `SavedState` persistence | eframe `get_value`/`set_value` + storage file | serde_json + `fsutil::write_atomic` | S | Low (§7.6) |
| Reduced motion | `ctx.global_style().animation_time > 0.0` | read `NSWorkspace` reduce-motion or an app setting | S | Low — must be re-plumbed |
| File dialogs | `rfd` 0.15 (blocking) | `rfd` unchanged, non-blocking API from the main thread | S | Low/Medium — no blocking on the gpui loop |
| Dock icon | objc `setApplicationIconImage:` + `NSDockTile` override, first frame | same objc, new lifecycle owner (gpui owns NSApplication) | S | Medium — verify (§7.7) |
| Packaging | `cargo-packager`, dev `.app` wrapper | unchanged | S | Low |
| Headless UI tests | `Context::default()` + `run_ui` + shape scanning | `#[gpui::test]` + `TestAppContext`/`VisualTestContext` | L | Medium — coverage regresses (§7.9, §6.4) |

Cells that cite **§6.4** were revised after mapping each hard part to Zed's
reference code; §6.4 lists exactly which estimates moved and why.

---

## 5. The four problems that do not port cleanly

### 5.1 Vim editor — the single largest item, and it is a rewrite

`widgets/vim.rs` is 2,902 lines with 47 tests and **157 egui call sites**. It is
not a vim implementation that happens to use egui; it is a *parasite on
`egui::TextEdit`*:

- `pre_edit()` reads `ctx.input_mut(...)`, **rewrites the pending event queue**
  (`Event::Text`, `Event::Key`, `Event::Paste`) and returns before the widget
  runs. Insert mode is "let egui handle it".
- The caret is moved by loading and storing `TextEditState` under a pinned
  widget id, using `CCursor`/`CCursorRange` (with the app's own inclusive
  visual cursor because egui stores exclusive ends).
- The block caret is painted from `TextEditOutput`'s galley geometry.
- Esc reaches the handler only because `event_filter(... escape: true)` is
  installed — a documented workaround for egui's focus pass.
- Undo is a private snapshot stack because direct buffer edits clear egui's
  undoer.

gpui has no "pending input events I can rewrite before the widget sees them",
no `TextEditState`, and no equivalent of `PaintList::transform_range`. Its input
model is entity + actions + key bindings; text input is a text system with its
own selection/IME handling.

**What survives (conceptually, and mostly line-for-line):** `VimMode`,
`VimState`, counts, pending operators, `feed`, motions, text objects, the ex
parser, the snapshot undo stack, and the `.`-repeat recording. These are pure
functions over a `Vec<char>` and a cursor index — roughly the bulk of the file.
Most of the 47 tests feed key events and assert buffer/cursor, so they can be
re-hosted on a new harness.

**What must be rewritten:** the event interception (gpui: consume keys in
`on_key_down`/actions, or drive a custom text input), caret and selection
integration, the block-caret paint, Esc/focus behavior, and IME coexistence.
The *interface* between vim and the editor is the part that disappears.

**Verdict: XL, 2,902 lines + 47 tests** — and it is the item that makes this
migration a multi-phase project rather than a long sprint. Risk was rated High
before reading Zed's `crates/vim`; it is now **Medium**, because that crate is a
working proof of the target architecture (§6.2), not because the work shrank.
If gpui-kit's editor already ships vim bindings, this drops to M/L — but then
the feature set is theirs, not yours, and `/`, marks and `>>`/`<<` (already
absent here) become someone else's roadmap.

### 5.2 Drag-and-drop and animation — mechanism lost, math kept

`dnd.rs` (349 lines, 13 sites) is almost pure geometry: `DropZone::insertion_index`
(midpoint rule), `GridZone::insertion_index` (row bands anchored to the gap),
`hit_test`. It ports to `Bounds<Pixels>`/`Point<Pixels>` mechanically, and its
8 tests are pure math — port them.

`anim.rs` (964 lines, 63 sites) is the risk. It relies on
`PaintList::transform_range` to **translate already-painted shapes** from old to
new positions while interaction rects stay on the real layout slots — a clever
egui-specific trick that makes FLIP trivial. gpui has no paint-list
post-transform: animation is per-element style/bounds plus an animation frame
ticker. So the tracks, easing curves (`cubic-bezier(0.20,0,0,1)` lift/reflow,
`cubic-bezier(0.40,0,0.20,1)` snap-back), stagger and ghost *math* survive;
the *host* must be rebuilt, and every one of the 8 animation tests that asserts
painted-shape geometry must be rewritten.

gpui's built-in `on_drag`/`drag_over`/`on_drop` gives you a typed drag payload
carried by the platform plus drop targets that can compute an index on hover;
Zed's tab and panel drags (`crates/workspace/src/pane.rs`, `dock.rs`) are the
worked example. It still does **not** provide Kanban insertion indexing, gap
placeholders, or cross-column reflow — keep the hand-rolled `dnd.rs` model on
top of it.

For the animation host, gpui's intended mechanism is per-element
`AnimationExt::with_animation` (`crates/gpui/src/elements/animation.rs`) plus
`spring.rs`; the paint-list translation trick has no equivalent, so the tracks
are re-hosted, not ported.

**Verdict: M/L, medium risk** (was L / high before §6). Not because it is
unknowable, but because this is the app's most carefully tuned surface and it
will visibly regress before it looks right again.

### 5.3 Markdown preview — small loss, real loss

`egui_commonmark` 0.25 with `better_syntax_highlighting` renders the preview;
the app pairs it with a syntect theme (`base16-ocean.dark` / `InspiredGitHub`)
and a hand-written source highlighter (`notes_highlight.rs`). gpui-kit ships a
markdown component, but its code-block highlighting, heading/quote/list styling
hooks and font resolution are its own. What you lose, concretely:

- per-token synthesised colors that currently match dark/light app themes,
- `CommonMarkCache`-style control over what is re-laid out per frame,
- fine styling control (the preview is inside a specific padded, floating-
  scrollbar frame today).

**Verdict: M, medium risk.** Acceptable loss *if* the new component exposes
enough styling to match the tokens; unknown until the spike. Do not assume
it does.

### 5.4 PDF — the one genuinely reusable layer

`pdf.rs` (679 lines, **2** egui call sites) is a worker thread owning
`hayro`'s `Pdf` + `RenderCache`, producing `Vec<u8>` pixels and page geometry in
page-view space. Its only egui dependency is a `repaint: egui::Context` used to
wake the UI. Replace that with a wake-up callback, and the **entire backend
ports**: rendering, text extraction, `PageText::selection_quads`,
`selection_text`, line grouping, and its 4 tests.

What must be redrawn: `views/documents.rs`'s surface — the LRU texture cache
(`HashMap<(page, scale), TextureHandle>`), zoom/pan, annotation hit-testing and
drawing, tab strip. That is 1,973 lines with 189 sites; the cache becomes
`HashMap<(usize, u32), RenderImage>`/image handles, and quads become elements.

**Verdict: backend S (near-verbatim), surface L, medium-low risk** — the surface
risk drops because Zed's `crates/image_viewer` is a direct reference for pixel
upload + zoom/pan; Zed has no PDF path, so rendering and annotation have no
reference at all (§6.1).

---

## 6. Zed as the reference implementation

gpui-kit supplies components; it does not supply *architecture*. The reference
app for gpui is Zed itself, which hand-rolls every hard surface in-tree
(`crates/ui`, `crates/editor`, `crates/vim`, `crates/markdown`,
`crates/workspace`). When a Kandrium feature has no gpui-kit answer, read Zed.

**Read it, do not copy it.** Verified at the pinned commit below: `gpui` and
`gpui_macros` are **Apache-2.0**; the application crates cited here (`vim`,
`editor`, `ui`, `markdown`, `workspace`, `picker`, `theme`, `icons`,
`image_viewer`, `title_bar`, `command_palette`, and `collab`) are
**GPL-3.0-or-later** (sampled `Cargo.toml`s, not an exhaustive audit). Kandrium
is Apache-2.0; copying GPL expression into it would relicense the combined work.
Architectural reading is fine; snippets, vendoring and GPL dependencies are not.
See §8.

All paths below are relative to **`f0d896f2c18a543335eb155f729d36b624695591`**
(2026-11-09), not `main`.

### 6.1 Map: Kandrium hard part → Zed reference

| Kandrium hard part | gpui-kit coverage | Zed reference @ `f0d896f` | What to learn | License |
| --- | --- | --- | --- | --- |
| **Vim mode** | none known | `crates/vim/src/vim.rs` (`init`, `observe_new(Vim::register)`), `state.rs` (`Mode`/`Operator`), `normal.rs`, `motion.rs`, `object.rs`, `visual.rs`, `insert.rs`, `command.rs`, `mode_indicator.rs`, `test.rs`; bindings loaded from `assets/keymaps/` | Vim is an *entity layered over the editor*, driven by actions and keymaps — not input rewriting | GPL — read only |
| **Text editing, caret, selection** | gpui-kit editor (quality unverified) | `crates/editor/src/element.rs` (custom `Element` that paints text, cursors, selections), `editor.rs`, `selection.rs`, `movement.rs`, `input.rs` (IME / marked text), `display_map.rs` | What an editor-grade gpui text surface actually costs | GPL — read only |
| **Single-line inputs, search fields** | reported `Input` | `crates/ui_input/src/input_field.rs` — a single-line field that **wraps an `Editor`** | One text component for everything, not a separate lightweight widget | GPL — read only |
| **Markdown preview** | reported markdown component | `crates/markdown/src/markdown.rs` (`MarkdownElement`, `MarkdownStyle`, syntax theme), `crates/markdown_preview/src/markdown_preview_view.rs` | Element-based rendering with injectable styles | GPL — read only |
| **Markdown source highlighting** | none | `crates/editor/src/element.rs` + language highlight ranges, `crates/theme` (syntax theme) | Highlight ranges → styled text runs | GPL — read only |
| **Drag and reorder** | reported dock drag | `crates/gpui/src/elements/div.rs` (`on_drag`, `drag_over`, `on_drag_move`, `on_drop`), `crates/workspace/src/pane.rs` (`DraggedTab`), `crates/workspace/src/dock.rs`, `crates/project_panel/src/project_panel.rs` | Typed drag payload carried by the platform; drop index computed on hover | Apache-2.0 (gpui) / GPL (call sites) |
| **Animation host** | none | `crates/gpui/src/elements/animation.rs` (`AnimationExt::with_animation`), `crates/gpui/src/spring.rs` | Time-driven per-element animation replaces paint-list transforms | Apache-2.0 |
| **Dock / panel layout** | reported dock | `crates/workspace/src/{dock.rs,pane_group.rs,pane.rs,persistence.rs}` | Resizable rails and tab strips only; the pane-group model is overkill here | GPL — read only |
| **Modals** | reported modal | `crates/ui/src/components/modal.rs`, `crates/workspace/src/modal_layer.rs` | Backdrop, focus trap, Esc precedence | GPL — read only |
| **Command palette / pickers** | reported picker | `crates/command_palette/src/command_palette.rs`, `crates/picker/src/picker.rs` | Filter + keyboard nav + virtualized results | GPL — read only |
| **Toasts** | reported notification | `crates/notifications/src/status_toast.rs`, `crates/workspace/src/toast_layer.rs` | Layered ephemeral UI, auto-dismiss | GPL — read only |
| **Archive table** | reported virtualized table | `crates/ui/src/components/data_table.rs` (`DataTable` + delegate trait), `crates/tabular_data_preview` | Virtualized rows/columns, sticky header | GPL — read only |
| **Theming** | reported theme | `crates/theme/src/{theme.rs,default_colors.rs,icon_theme.rs,scale.rs,font_family_cache.rs}` | Theme registry, per-component token lookup | GPL — read only |
| **Typography scale** | none | `crates/ui/src/styles/{typography.rs,spacing.rs,units.rs}`, `crates/ui/src/components/label/label.rs` | Size **enums** (`LabelSize`/`TextSize`) instead of raw px — the structural equivalent of `text_styles` | GPL — read only |
| **Icons** | none | `crates/icons/src/icons.rs` (generated icon-name enum), `crates/ui/src/components/icon.rs`, `crates/theme/src/icon_theme.rs` | Icon name → theme-tinted SVG, no per-widget paths | GPL — read only |
| **Titlebar / window** | unknown | `crates/title_bar/src/title_bar.rs`, `crates/gpui/src/platform/` | Transparent titlebar with traffic-light offsets | GPL / Apache-2.0 |
| **Images / PDF** | reported image component | `crates/image_viewer/src/image_viewer.rs` (`RenderImage`, zoom/pan), `crates/svg_preview`, `crates/mermaid_render` | Pixel upload + zoom/pan surface. **Zed has no PDF path** — nothing to copy for rendering or annotation | GPL — read only |
| **Headless tests** | n/a | `crates/gpui/src/app/test_context.rs` (`TestAppContext`), `app/visual_test_context.rs`, `crates/gpui/src/test.rs` (`#[gpui::test]`), `crates/editor/src/editor_tests.rs`, `crates/component_preview/src/component_preview.rs` | The harness shape, and editor_tests as the model for a key-driven test matrix | Apache-2.0 / GPL |

### 6.2 The finding that changes the vim design

Zed's vim is not inside the input widget. `crates/vim` is its own crate: it
registers a `Vim` entity onto every `Editor` via `observe_new`, binds a separate
keymap context, keeps `Mode`/`Operator` stacks in `state.rs`, and applies
motions/operators by calling editor APIs (selections, movement, transactions).
It never rewrites a pending input queue, because gpui has no such queue.

Kandrium's `vim.rs` is the opposite: `pre_edit()` rewrites `Event::Text`/
`Event::Key`/`Event::Paste` *before* `TextEdit` sees them, then patches
`TextEditState`. That interface disappears entirely. Consequences:

- The reusable part is everything above the text layer (modes, counts, motions,
  text objects, operators, ex parsing, `.`-repeat, snapshot undo) — consistent
  with §5.1, and it is most of the file's logic and most of its 47 tests.
- The new requirement is an **editor-grade text component with a programmatic
  caret/selection API** — precisely the thing `crates/editor` is 500 KB-ish of.
  That is why vim stays XL: the work is "own an editor and drive it", not
  "port a key handler".
- Because `ui_input::InputField` wraps the same `Editor`, one component then
  serves the card modal, board rename, filters and pickers. That is a real
  simplification compared with today's mix of `TextEdit` variants — and the one
  place where the gpui move could *reduce* total code.
- **The spike must answer:** does gpui-kit's editor expose selection/caret
  programmatically, and can a keystroke be consumed before text insertion? If
  yes, layer vim on it as Zed does. If no, you write the element, and vim goes
  back to "XL plus an editor".

### 6.3 What does not transfer

- **The workspace/pane/dock model.** Zed serializes pane trees, split layouts
  and multi-window state (`workspace/src/persistence.rs` is a large subsystem).
  Kandrium is one window with five fixed sections and an optional collapsible
  rail. Porting that model would add complexity with no user-visible gain.
- **`multi_buffer` / rope / anchors.** Zed's selections survive concurrent
  edits because they are anchored in a rope with explicit biases. Kandrium's
  documents are whole-file `String`s with char indices. Adopt Zed's text model
  only if you adopt its editor wholesale; otherwise keep `String` + index and
  accept the simpler semantics (which is what `vim.rs` already does).
- **Everything IDE-shaped**: LSP, project search, terminals, git, collab,
  settings UI, `component_preview` as a shipped surface.
- **Zed's styling values.** Dark/light tokens, spacing and type scale are not
  Kandrium's; take the *mechanism*, not the numbers.
- **The code itself** (§8).

### 6.4 Revised estimates

Only rows that moved; everything else in §4 stands.

| Item | Was | Now | Why |
| --- | --- | --- | --- |
| Vim editor | XL / **High** | XL / **Medium** | Architecture proven (Zed `crates/vim`); size unchanged, unknown reduced. Risk returns to High if gpui-kit's editor has no caret API |
| Single-line inputs, filters, pickers | M / Medium (folded into "card chip") | **S/M** / Low | One shared text component serves all of them (Zed `ui_input`) |
| Drag-and-drop | L / **High** | **M/L** / Medium | Typed drag payload + hover-computed drop index is a first-class gpui API with real call sites (`pane.rs`) |
| Drag animation | L / **High** | **M** / Medium | `AnimationExt::with_animation` is the intended mechanism; only the curve/host code is new, not the concept |
| Archive table | M / Low | **S/M** / Low | `DataTable` + delegate trait is a closer match than `egui_extras` |
| Theming | M / Low–Medium | M / **Low** | `crates/theme` shows the registry + token shape |
| Typography (`text_styles` rule) | M / Low–Medium | **S/M** / Low | Zed already enforces the rule with size enums (`styles/typography.rs`) |
| PDF surface | L / Medium | L / **Medium-Low** | `image_viewer` is a direct reference for pixel upload + zoom/pan |
| Headless tests | L / **High** | L / **Medium** | First-class `#[gpui::test]` + `VisualTestContext`; the duplicate-ID scan is still lost |
| Markdown preview | M / Medium | M / Medium | Unchanged — but if gpui-kit's component cannot match tokens, writing your own `MarkdownElement` is **L** |
| Test harness | L / **High** | **S / Low** | Resolved in the spike: `TestAppContext` + `.test_support()` + `#[gpui::test]`; only the egui duplicate-ID scan is lost |
| Read-only board path | not estimated | **S** | Found during the spike: `get_board` writes on read; split into derive/persist + `get_board_readonly` |

Net effect: **risk falls across the board; volume does not.** It is still a
~22.7k-line second implementation, two XL items remain (vim, Projects), and the
verdict in §1 is unchanged — a cheaper-to-execute migration is not a
better-justified one.

### 6.5 How to study Zed without drowning

- **Pin the commit**: `f0d896f2c18a543335eb155f729d36b624695591` (2026-11-09).
  `main` moves daily. Clone shallow, or browse
  `github.com/zed-industries/zed/tree/<sha>/crates/...`. Never cite `main` in
  this document.
- **API drift is guaranteed, not possible.** At that commit `crates/gpui` is
  `0.2.2`; gpui-kit pins Longbridge's `gpui-pre 0.3.12`. Treat Zed as
  *architecture*; when a call is missing from gpui-kit, check the snapshot's
  own vendored examples before concluding it cannot be done.
- **Read order per question**: (1) `crates/gpui/src/elements/{div,animation}.rs`
  for the primitive; (2) one component in `crates/ui/src/components/` for the
  idiom; (3) one call site in `crates/workspace` or `crates/project_panel`;
  (4) `crates/editor/src/element.rs` only if you must paint text.
- **Grep, never read linearly.** `element.rs`, `pane.rs` and `markdown.rs` are
  each several hundred KB; `editor_tests.rs` is over a megabyte. `git log -S
  "with_animation"` / `-S"on_drag"` finds the introducing commit, and its tests
  are the real documentation.
- **Start at `crates/component_preview`**, not the 250-crate root.
- Record findings in this section of this file (do not start a parallel notes
  file), so the rationale stays with the plan.

### 6.6 License constraint

Short version: `gpui`/`gpui_macros` are Apache-2.0, so reading the *framework*
is friction-free; every app-level crate in the map above (`vim`, `editor`, `ui`,
`markdown`, `workspace`, `theme`, `image_viewer`, …) is **GPL-3.0-or-later**, so
they are read-only reference material and nothing more.

The obligations, the copy/no-copy discipline, the per-dependency verdicts and
the gpui-kit taint analysis are all in **§8** — kept in one place so this
subsection cannot drift out of date. The only rule this map needs inline: every
row of §6.1 above is a *design* reference, never a source to port.

---

## 7. Where the rest of the app stands

### 7.1 `kandrium-core` — the one change Stage 1 had to make

`crates/core` has no UI dependency (`dirs`, `jiff`, `log`, `rand`, `rusqlite`,
`rusqlite_migration`, `serde`, `serde_json`, `tempfile`, `thiserror`). All
schema/migration/backup logic is invariant. This is the plan's main safety
property: every phase can be abandoned without data risk.

One exception was found during the spike and fixed in Stage 1:
`services::board::get_board` **writes on read** for non-project boards (it
backfills and reconciles `area_card_order`). That is fine when the app is the
only front-end, but a second, read-only consumer would fail with
`SQLITE_READONLY`, and every read would bump `PRAGMA data_version` and wake the
app's watcher thread.

The fix keeps both behaviours and adds a read-only variant:

- `get_board` → `get_board_impl(db, id, reconcile = true)` — unchanged for the
  app and MCP;
- `get_board_readonly` → `get_board_impl(db, id, reconcile = false)` — derives
  the identical order and stamps `display_position` without persisting;
- the derivation is split out as `derive_area_order` (pure reads) and
  `persist_area_order` (IMMEDIATE transaction, no-op when the plan is empty);
- `db::open_db_readonly` opens with `SQLITE_OPEN_READ_ONLY`.

Two tests pin the contract: a read-only connection renders an unbackfilled area
board without error, and the derived order (including `display_position`)
equals what the writing path stores. **The MCP tool surface is untouched.**

### 7.2 `kandrium-mcp` — untouched, contract unaffected

`crates/mcp` depends only on `kandrium-core`, `rmcp`, `rusqlite`, `serde`,
`serde_json`, `tokio` — no egui, no app crate. The 13-tool camelCase wire
contract is produced by the MCP process, which reads the same SQLite file the
app reads. A UI rewrite is invisible to it: same DB, same WAL, same
`data_version` polling. The one app-side contract to preserve is the DB write
pattern (all writes through core), which the migration does not change.

### 7.3 Async → UI thread plumbing

Today: `mpsc` channels drained in `app::logic()` each frame (watcher, PDF
worker), plus `ctx.request_repaint_after` for autosave debounce and the Clocks
minute tick. In gpui: `cx.spawn` tasks + `cx.notify()`/entity events, with
timers. The *shape* (worker thread → channel → UI drain) stays valid; the
drain point becomes an entity update rather than `logic()`. M effort.

### 7.4 Immediate mode → retained entities

The app's idiom — a mutable `KandriumApp` struct, per-frame `&mut String`
drafts, and an `*Actions` struct returned from each view — maps *reasonably*
to gpui (`Actions` → events/`EventEmitter`; `&mut` drafts → entity fields), but
every view's signature changes. This is an app-wide refactor, not a
translation. It is the hidden cost behind most `L` rows in §4.

### 7.5 Theme, fonts, and the "no pixel sizes outside `text_styles`" rule

`theme.rs` holds `Tokens` (18 colors incl. `titlebar`), a floating-scrollbar
`ScrollStyle`, and the `text_styles` table (Small 13 / Body 16 / Button 16 /
Heading 20 / Monospace 16 + `strong`, `small-strong`, `clock-time`).

- gpui-kit has its own `Theme`/component theming → map `Tokens` onto theme
  variables; colors port as values.
- gpui styles are per-element `StyleRefinement` structs, so there is **no
  global style table**. The rule can be *preserved as a convention* by routing
  every font size through one helper (`theme::text_style(Name) -> TextStyle`)
  and banning raw `px()` in view code — enforced by review and clippy, not by
  the framework.
- Fonts: the four bundled TTFs (`Inter-Regular/Medium/SemiBold`,
  `JetBrainsMono-Regular`) register with gpui's text system; named
  medium/semibold families become font-weight selection instead of separate
  families — arguably cleaner.
- Icons: same Lucide SVGs, new loader (`svg()` + `AssetSource`), tint via
  `text_color`. Keep the inline table; do not re-introduce an icon crate.

**Verdict: M, low/medium risk.** The typography *discipline* survives; the
*enforcement mechanism* does not.

### 7.6 Persistence

`SavedState` is 16 serde fields (board, dark mode, section, open tabs, tree
visibility, vim on/off, 12/24h) persisted by `eframe::get_value`/`set_value`
under `eframe::APP_KEY` in eframe's storage file.

Replacement: `serde_json` to `<data_dir>/kandrium/ui-state.json` via
`fsutil::write_atomic`, debounced on change. Load once at boot with a default
fallback. If carrying over existing state matters, add a one-time reader for
eframe's storage file — S effort, and the only data-migration item in the whole
plan (it is preferences, not user content).

### 7.7 macOS platform integration

- **Titlebar**: today `main.rs` forces `fullSizeContentView` + hidden title via
  objc and *measures the traffic lights every call* because layout settles
  late. gpui's `TitlebarOptions` is the supported path (Zed does exactly this).
  Expect this to get simpler, but the band's geometry must be re-tuned.
- **Dock icon**: `set_dock_icon()` uses `setApplicationIconImage:` with an
  `NSDockTile` content-view override, applied from the first frame. gpui owns
  `NSApplication`, so timing changes; the tile override should still work.
  **Verify in the spike** — it is a documented no-op on this macOS for bare
  executables.
- **Dev launch**: `open -n target/Kandrium-dev.app` (symlink to
  `target/debug/kandrium-app`) still applies verbatim — same binary name, same
  bundle wrapper. `cargo packager` (packager.json, bundle id
  `app.kandrium.desktop`) is cargo-built and unaffected.
- **`rfd`**: unchanged, but the blocking `pick_file()` must move off the gpui
  main thread (use the async API + `cx.spawn`).

### 7.8 Logging

`kandrium_core::logging` + panic hook are untouched. Keep the "no
`eprintln!`/`println!`" rule (the MCP rule is unchanged anyway).

### 7.9 Tests — the biggest *silent* loss

156 app tests; 10 modules use headless egui (`Context::default()` + `run_ui`,
`textures_delta.clear()` to dodge a drop panic, and scanning `FullOutput.shapes`
for `"use of"` to catch duplicate widget ids).

gpui's equivalent is `TestAppContext` / `VisualTestContext`, which can build a
window at a size, run `update`, and simulate input. What changes:

- **Recoverable**: pure-logic tests (`dnd` 8, `pdf` 4, most of `vim`'s 47,
  `wclock`'s grid math) — re-host on the new harness.
- **Lost**: the duplicate-ID shape scan has no counterpart (gpui has no
  persistent widget `Id` namespace to collide), and pixel-level geometry
  assertions need a real window size and a different shape API. Expect a
  material drop in layout regression coverage during the migration, exactly
  when layout churn is highest.
- **Verify**: exact API names and whether input simulation covers IME are
  unconfirmed — treat `TestAppContext` claims here as *needs verification*,
  not fact.

---

## 8. Licensing posture

*Engineering guidance, not legal advice.* Everything below was checked against
the actual files on 2026-11-10 and says where the uncertainty sits. A lawyer
settles the parts that matter commercially; this section exists so the
practical boundary is unambiguous for whoever writes the code.

This is the single home for licensing in this plan. §6.6 is a pointer to it.

### 8.1 What GPL-3.0 actually requires

**Obligations attach to distribution, not to reading, studying or private use.**
The licence grants unlimited permission to run and modify the software for any
purpose; the conditions in §§4–6 only fire when you *convey* copies to someone
else.

A strictly personal, undistributed build triggers **none** of it: no source
offer, no notice file, no relicensing. The trigger is handing a build to anyone
outside your own machines — a DMG, a public repo, a binary on a website, a
build passed to a colleague.

If you do ship a `.app`/DMG that contains GPL-3.0 code, then:

| Obligation | What it means in practice |
| --- | --- |
| **Whole work is GPL-3.0** | The combined work — Kandrium's app binary, not just the ported file — must be licensed GPL-3.0 (or later) when conveyed. |
| **Complete corresponding source** | Provide the preferred form for modification of *everything* needed to build and run that binary: your source, the exact dependency versions (lockfile), build scripts, and the scripts that package the `.app`. A link to "the repo" is only enough if the repo is exactly the shipped tree. |
| **Notices preserved** | Keep every copyright notice, licence text, and NOTICE-file attribution from the GPL code and from the Apache-2.0 parts. Do not strip headers. |
| **No added restrictions** | GPLv3 §10 forbids extra terms — no field-of-use limit, no "no commercial use", no EULA clause that narrows the granted rights. Your own EULA has to be compatible with that. |
| **Mac App Store: closed** | Apple's store terms add usage/DRM restrictions that GPLv3 §10 forbids. The FSF has held this since 2011 (VLC was pulled from the store for exactly this). A GPL Kandrium cannot ship on the MAS; a DMG/notarised build can. |
| **Anti-tivoization (brief)** | GPLv3 §6 requires "Installation Information" for *User Products* — the user must be able to install a modified build. For a source-available Mac app on an ordinary Mac this is normally satisfied by publishing the source and build steps; the clause bites on locked hardware, firmware, or DRM-gated distribution. It is not a reason to avoid GPL for this app, but it is why "ship it signed and unbuildable" is not a safe posture. |

Because that list is all-or-nothing, the cheapest posture is the one in force
today: **Kandrium is Apache-2.0, depends only on permissive code, and reads GPL
code without incorporating it.**

### 8.2 Using patterns without copying code

Copyright protects **expression**, not **ideas**. Architecture, data flow, state
machines, invariants, wire formats and algorithms are not copyrightable; the
literal code, and the *selection and arrangement* of the non-literal parts, are.

The discipline, as a checklist:

1. **Read for the model.** Understand the problem it solves, the invariants it
   maintains, the data it moves.
2. **Close the source.** No second monitor running the GPL file while typing.
3. **Write the model down in your own words.** §6.2 of this document is the
   template: it describes how Zed layers vim over the editor without
   reproducing a line of it.
4. **Implement from the note.** If the note does not fit Kandrium's structures,
   change the design — divergence is evidence of independent expression.
5. **Never transcribe, even briefly.** "Type it out then rename everything" is
   a derivative work, not a reimplementation. So is pasting a signature block
   to "get the types right".
6. **Do not copy identifiers, comments, doc strings, error messages, test
   fixtures or constant tables.** These are the cheapest things to copy and the
   most damaging to find.
7. **Do not vendor, fork, submodule, or paste with `// TODO rewrite`.**

**Non-literal infringement is the real risk.** Even with zero textual overlap,
an implementation that reproduces the same functions in the same order with the
same file layout and the same type names preserves protected structure
("structure, sequence and organisation"). The test courts apply is the
**abstraction–filtration–comparison** test from *Computer Associates v. Altai*:
abstract the work into levels, filter out the unprotectable parts (ideas,
efficiency-driven choices, public-domain elements, external constraints), and
compare what remains. What remains is exactly the creative decomposition — so
"reimplement every function in the same order with the same modules" is a bad
idea even without literal copying.

Concretely for this plan: reproducing Zed's
`display_map/{wrap_map, fold_map, text_wrapper, inline_line}` split, or its
`FoldMap` → `FoldSnapshot` → `FoldPoint` → `FoldChunk` type ladder, would carry
structure even if every line were new. §6.2 deliberately describes the vim
layering as a *relationship*, not as a file list. Kandrium's own `vim.rs` is
prior art (2,902 lines, its own state machine and test suite), so porting *that*
is not the risk — the risk is bolting Zed's model on top of it without writing
the intermediate model first.

### 8.3 Per-dependency status

Verified against repository files and published crate metadata, not the GitHub
detector.

| Dependency | Licence (verified) | What it means for Kandrium |
| --- | --- | --- |
| `gpui`, `gpui_macros` (Zed) | **Apache-2.0** | Permissive. Flows into a GPL-3.0 work fine if that ever happens; imposes nothing on Kandrium's licence today. Using gpui does **not** make Kandrium GPL. |
| Zed app crates: `vim`, `editor`, `ui`, `markdown`, `workspace`, `theme`, `image_viewer` (also `picker`, `command_palette`, `title_bar`, `icons`, `collab`, `markdown_preview`, `terminal`, `syntax_theme`, `text`, `rope`, `language`, `lsp`, `settings`) | **GPL-3.0-or-later** | Read-only reference. No linking, no vendoring, no copying. Repo root ships `LICENSE-GPL` + `LICENSE-APACHE`; no AGPL crate in the sample. |
| `gpui-kit` 0.9.1 | **Apache-2.0, not dual-licensed** | Safe to depend on. `LICENSE-APACHE` only (© 2024–2026 Longbridge); no `LICENSE-MIT` exists. `LICENSE-DOCS.md` is **CC BY 4.0** and covers the website prose only — code stays Apache-2.0. All eight published crates declare `license = "Apache-2.0"`; the root workspace is `publish = false`. GitHub's `NOASSERTION` is a manifest artefact: the workspace has no `[workspace.package] license` key, so the detector reads no identifier — the licence file itself is unambiguous. |
| `gpui-pre` 0.3.12 (also `gpui-pre-platform`, `gpui-pre-web`, `gpui-pre-sum-tree`) | **Apache-2.0** | Published by Longbridge (`huacnlee`), pinned `=0.3.12`, `repository = zed-industries/zed`, described as a "snapshot of zed@…". It carries upstream's Apache-2.0, not a Longbridge-specific licence. Safe. |
| `gpui-fast` 0.1.14 | **Apache-2.0** | Optional feature of gpui-kit, **not on by default**. Safe if enabled; keep it off unless the spike needs it. |
| `kandrium-core`, `kandrium-mcp` | Apache-2.0, in-repo | Unaffected. They are separate processes that share a SQLite file; no linking, no derived work, no licence interaction with the UI at all. |

The practical answer for the plan: **the only GPL in the picture is Zed's
application code, which is read and never linked.** gpui, gpui-kit and every
`gpui-pre` crate are Apache-2.0, so a gpui build keeps Kandrium's licence
exactly as it is today.

### 8.4 What the dependency tree pulls in

Copyleft scan of the published manifests for `gpui-base` and `gpui-component`
(the two crates that carry the components this plan would adopt): no GPL, AGPL
or MPL crate. Notable entries: `ropey` (MIT/Apache-2.0), `syntect` (MIT),
`markdown` from markdown-rs (MIT), `similar` (Apache-2.0), `two-face`
(MIT/Apache-2.0), and `sum-tree`, which is `gpui-pre-sum-tree` (Apache-2.0) —
not a copyleft crate despite the bare import name in the source.

Assets:

- **Icons:** 1,830 Lucide SVGs under `crates/assets/assets/icons/`.
  `crates/assets/LICENSE-LUCIDE` is **ISC** (© 2026 Lucide Icons and
  Contributors) plus a **MIT** section (© Cole Bemis) for the subset derived
  from Feather. Both permissive; keep the file if the SVGs are bundled.
- **Fonts:** the only fonts in the repo are under `crates/story-web/fonts/`
  (IBM Plex Sans, Inter, JetBrains Mono, Noto Sans SC) — story-site assets,
  not pulled in by `gpui-base`/`gpui-component`. Their individual licences were
  not read; do not ship them without checking.
- **Vendored code:** none observed in the published crates beyond these icon
  assets.

### 8.5 The GPL-taint question

The question that matters is narrower than "is gpui-kit clean": gpui itself is
Apache-2.0, so *using gpui* is not the issue. The issue is whether gpui-kit's
**editor, markdown and dock components** were ported from Zed's GPL-3.0 crates
(`crates/editor`, `crates/markdown`, `crates/workspace`, `crates/ui`).

| Check | Finding |
| --- | --- |
| Attribution: NOTICE files, source headers, README credits | **None.** No source file credits Zed; the only Zed reference in the ecosystem is the `gpui-pre` crate description ("snapshot of zed@…"). No NOTICE file in the repo. |
| Zed's distinctive editor identifiers in gpui-kit sources | **Zero hits** for `ChunkRenderer`, `MultiBuffer`, `FoldPoint`, `InlayMap`, `BlockMap`, `TabMap`, `HighlightedEditableText`. (A plain text search for "Zed" only matches words like "normalized" and "sized".) |
| Text model | gpui-kit's editor is built on **`ropey`** (MIT/Apache-2.0) plus its own `RopeExt` — not on Zed's GPL `text` / `rope` / `multi_buffer` stack. This is the strongest single signal: a mechanical port of Zed's editor would drag those crates in. |
| Data-structure shape | gpui-kit's `display_map/fold_map.rs` is **446 lines** with private `HiddenRows` / `hidden_before` / `dense_projection`. Zed's `FoldMap` is a ~2,570-line public type family (`FoldSnapshot`, `FoldPoint`, `FoldChunk`, `ChunkRenderer`). Different scale, different API, different data model. |
| Git history | Incremental and issue-numbered (#2691 "input: Split Input, Textarea, and Editor APIs", #2837 "input: Multi cursors", #3583, #3622) — consistent with independent development, not a bulk import of Zed sources. |
| Vim | There is **no vim module in gpui-kit at all**, so the largest GPL-derived risk surface in this plan does not exist. |
| Module decomposition | Similar in places: `display_map/{wrap_map, fold_map, text_wrapper, inline_line}`, `lsp/{semantic_tokens, completions, hover, document_colors}`, `dock/layout/{node, tree, normalize}`. This is the residual SSO concern (§8.2) — but these are also the conventional decomposition of the problem, and the contents differ materially. |

**Verdict: clean on the available evidence — which is not the same as a
clearance.** Confidence is **moderate-high** that no literal copying occurred
(identifier and data-model evidence is strong, and the absence of Zed's text
stack is hard to fake). Confidence is **low-to-moderate** that no
structure/sequence/organisation derivation occurred in the dock and markdown
modules, because I compared shape by inspection rather than forensically.
Nothing found raises the risk to "tainted", and there is no positive evidence
of derivation.

Two honest qualifiers. First, this analysis cannot see what happened before the
tagged release's history was rewritten or squashed; I read the visible history
only. Second, if a component *were* derived from GPL code, the exposure lands on
Longbridge as publisher and on anyone who redistributes it — which, if Kandrium
linked that crate, would include Kandrium. That is why the question is asked at
all.

If the spike finds something suspicious in a specific module, the mitigation is
structural: gpui-kit is a workspace of separate crates, and the editor lives in
its own component area, so it can be excluded. The dock and markdown components
would then need replacements (a hand-rolled split layout is the cheapest; the
markdown renderer is the harder one). Do that *before* the port, not after.

### 8.6 Kandrium's own licence

`LICENSE` at the repository root is the **Apache License, Version 2.0**
(January 2004), and all four crates inherit it via `license.workspace = true`.

The compatibility is **one-way**:

- **Apache-2.0 → GPL-3.0: allowed.** Apache-2.0 code can be incorporated into a
  GPL-3.0 work; its patent and notice terms do not conflict with GPLv3. So if
  Kandrium ever had to become GPL-3.0, nothing in the current dependency set
  blocks it.
- **GPL-3.0 → Apache-2.0: not allowed.** You cannot take GPL code, link or copy
  it, and convey the result under Apache-2.0. Relicensing back would need every
  copyright holder's permission, which for a GPL-3.0-or-later Zed crate means
  everyone who ever contributed — i.e. practically impossible.

So the blast radius of a single transcribed Zed file is the **whole distributed
application**, not that file. It would also close the Mac App Store path
(§8.1). Both facts argue for keeping the boundary mechanical rather than
relying on care.

### 8.7 Pre-merge rule

Enforceable, short, and checkable in review:

> Any commit that ports a design from Zed must name the reference crate and
> path at the pinned SHA, and state that the implementation was written from a
> written model rather than transcribed. If you cannot name where the design
> came from, it is not ready to merge.

And, mechanically, in the same change: no GPL/AGPL/MPL crate appears in
`Cargo.lock`. A `cargo deny` licence allow-list (Apache-2.0, MIT, BSD-*,
ISC, Unicode-3.0, Zlib, OFL-1.1, plus build-only exceptions) in CI is the
cheapest way to make that automatic; until CI exists (precondition §13.3),
make it a review checklist item and run `cargo tree` by hand.

If a GPL file does appear in the tree, **revert the commit** rather than
"fixing it forward" — a rewritten file does not undo the copy.

### 8.8 What was not verified

- No maintainer statement from Longbridge asserting independent authorship. The
  verdict in §8.5 rests on code evidence only.
- No forensic similarity scan; the comparison was structural and by identifier
  sampling, not a token-level diff.
- Manifests for `gpui-shell`, `gpui-webview` and `gpui-fps` were not scanned,
  and the full transitive tree was not resolved. `cargo deny` at adoption time
  is the real gate.
- The `gpui-pre` snapshot was not diffed against Zed at the matching revision,
  so I cannot say which Zed files it contains or omits.
- Font licences under `crates/story-web/fonts/` were not read (the files are
  present; the licences are not recorded here).

---

## 9. Phased sequence

Each phase has an explicit fallback: **abandon it**. Because `kandrium-core`
and `kandrium-mcp` are untouched and the egui app remains in the workspace,
every phase is revertible with no data consequence.

| Phase | Scope | Status | Fallback |
| --- | --- | --- | --- |
| 0 | Freeze: commit tree, tag, CI green (§13) | Done | n/a |
| 1 | **Spike** (§11), throwaway crate | **Done — PASS**, harness resolved (§11.1) | Delete crate |
| 2 | **Foundation** (Stage 1): own workspace, core read-only board path, theme/fonts/icons, 5-section shell, read-only board + Clocks | **Done** — runnable preview build | Delete crate |
| 3 | Board interactivity: DnD, board switching, card modal, column ops, animation host | **Next** — concrete tasks §9.1 | Delete crate |
| 4 | Markdown editor + vim (largest; after the shell proves out) | **Next** — concrete tasks §9.2 | Keep egui app default |
| 5 | Notes + Documents (trees, tabs, autosave, PDF surface) | Pending | Keep egui app default |
| 6 | Projects (tree, Overview, doc tabs) + Clocks parity + Archived table | Pending | Keep egui app default |
| 7 | Parity pass, packaging, flip the default binary, delete `crates/app` | Pending | Ship the previous release |

### 9.1 Stage 2 — board interactivity (concrete tasks)

Prerequisite: the `gpui-migration` branch at the Stage 1 commits; `crates/app`
and `crates/app-gpui` both green.

1. **Writable handle, lazily.** `src/store.rs` gains `write_db: Option<Connection>`
   opened with `kandrium_core::open_db` (not `bootstrap` — the file is already
   schema-current). Reads stay on the read-only handle; every mutation takes the
   write handle. Rule to enforce in review: no `get_board` on the read-only
   handle, no `_readonly` call on the write handle.
2. **Board switching.** `views/tasks.rs` header becomes a tab row from
   `Store::boards()`; `AppShell` stores `active_board: Option<String>` and
   reloads through `Store::board`. Test: switching from the seed board to the
   third board changes the column set.
3. **Drag state.** New `src/dnd.rs` porting the `dnd::GridZone` insertion-index
   idea and the `drag_probe` timeline (`LIFT_S 0.140`, `GAP_S 0.160`,
   `SNAP_S 0.260`, `TOTAL_S 0.560`) onto `with_animation`. Reuse
   `cubic-bezier(0.20, 0, 0, 1)` for lift/reflow and
   `cubic-bezier(0.40, 0, 0.20, 1)` for snap-back.
4. **Drop.** On drop call `kandrium_core::services::board::move_area_card` for
   Tasks boards (mirrors included — a mirrored card is never moved into the area
   column) and `move_card` for project boards; reload from the write handle.
   Tests: a native card dropped into another column changes `position`; a
   mirrored card dropped into a status column preserves `project_id` and
   `area_board_id`.
5. **Card modal shell.** `src/views/card_modal.rs` reading the card through
   core, title editable, description read-only (the editor is Stage 3), Escape
   and click-outside close. No image/attachment surface yet.
6. **Column ops** (app-only by design): create / rename / delete / move through
   `kandrium_core::services::column`; keep the MCP tool surface untouched.
7. **Tests.** Headless: drag across columns mutates the DB and re-renders; modal
   opens for the clicked card id; no duplicate element paths after a reorder.
   Physical-mouse verification stays manual.

### 9.2 Stage 3 — markdown editing and vim (concrete tasks)

1. **Editor component.** Wrap gpui's `TextInput`/`EntityInputHandler` in
   `src/widgets/editor.rs`, matching the egui fixed-height frame + inner scroll
   + frameless multiline reference in `widgets/card_modal.rs`.
2. **Vim layer.** Port `crates/app/src/widgets/vim.rs` with its 47-case matrix:
   `pre_edit` equivalent consumes key events before insertion; Normal/Visual
   commands mutate the buffer; caret movement uses the entity's selection API;
   own snapshot stack for `u` / `Ctrl-r` (direct buffer edits bypass the
   built-in undoer). Ex commands `:w`, `:q`, `:wq`, `:x`.
3. **Markdown preview.** Evaluate gpui-kit's markdown component first; if it
   cannot match tokens, write a `MarkdownElement` over `pulldown-cmark` (§5.3).
   Preview toggle mirrors `markdown::mode_toggle`.
4. **Notes tree + autosave.** Tree from `kandrium_core::notes`, debounced writes
   through `fsutil::write_atomic`, `SavedState` JSON parity for `tree_open` and
   vim mode.
5. **Tests.** Re-run the vim behaviour matrix headlessly; markdown round-trip;
   autosave writes are atomic (temp file + rename).


### Is a partial migration possible?

**Not within one window.** egui and gpui both want to own the platform window,
the event loop and the input stream:

- egui can be embedded in a non-egui renderer in principle (`egui-wgpu` is
  renderer-agnostic), but gpui does not expose its wgpu device/queue or a
  paint-tree insertion point, and you would have to translate gpui input into
  `RawInput` and fight two separate focus/IME/repaint models. That is a research
  project, not a migration strategy.
- Coexistence as a *shell command + embedded viewport* is not supported by
  either toolkit.

The only genuinely partial path is **two binaries**: `kandrium-app` (egui, the
product) and `kandrium-app-gpui` (new, internal), sharing `kandrium-core` and
the same DB. That is exactly the phase structure above — and note it is still a
**full second implementation** of 22.7k lines; the fallback buys safety, not
effort. The operating rules for that period — what is duplicated, the parity
gate, and rollback — are in §10.

---

## 10. Coexistence and cutover

> **This section is conditional.** The recommendation in §1 is *do not migrate
> now*. What follows is the pre-agreed operating rule **if** that decision is
> reversed — written now so it is not improvised mid-rewrite. It is not a
> scheduled project.

### 10.1 Strategy, stated unambiguously

- **The egui app remains the shipping product and the source of truth for
  behaviour for the entire migration.** `crates/app` is not deleted, gutted,
  feature-frozen or moved behind a flag at any point before the parity gate
  (§10.5) passes.
- The egui app keeps receiving bug fixes and features. Freezing it would mean
  the fallback rots and the "source of truth" becomes a museum piece.
- If the gpui port and the egui app disagree, **egui is correct** and the port
  has a bug. That rule is what prevents the two from drifting in intent.
- `kandrium-core` and `kandrium-mcp` are never duplicated, forked or
  reimplemented. There is one schema, one set of services, one wire contract.

### 10.2 The coexistence mechanism

A single window cannot host both (§9, "Is a partial migration possible?"), so
coexistence means a **parallel app crate** in the same workspace:

```
crates/
  core/           untouched, shared
  mcp/            untouched, shared
  app/            egui — shipping product, untouched
  app-gpui/       new — binary `kandrium-app-gpui`, same `kandrium-core`
```

- Same SQLite file, same `KANDRIUM_DB` default, same WAL. Both apps talk to one
  database through core; no syncing, no export/import.
- Shared surface is only: `kandrium-core`, fonts/icons/image assets, packaging
  metadata. Do **not** try to share view modules — the widget models are
  incompatible and a "shared UI abstractions" crate would become a third thing
  to maintain. If asset paths become painful across crates, add one small
  `crates/assets` crate; not before.
- During development, point the gpui build at a **separate** `KANDRIUM_DB`
  (e.g. a copy under `/tmp` or the workspace) so a broken preview cannot damage
  the real database. Both binaries must never be bundled into the same `.app`
  or shipped in the same release.

**What that costs, precisely**

| Cost | Consequence |
| --- | --- |
| Duplicated UI work | ~22,750 lines of view code written twice at peak; every screen exists in two idioms |
| Two bug surfaces | A UX bug found in one app may exist, differently, in the other. Triage has to say *which* app, and fixes may need two PRs |
| Core features exercised twice | A new schema feature must be wired into two UIs. A Clocks-sized feature (~3.7k UI lines, 3 migrations) would be built twice while both apps live |
| Divergent state | Two preference stores (`SavedState` vs `ui-state.json`), two sets of defaults, two sets of empty-state copy — each a place to disagree |
| Review load | Every gpui PR is reviewed against an egui implementation that is the spec; this is slower than greenfield work |

### 10.3 How the two are kept from drifting

1. **Port, do not improve.** The gpui build reproduces egui behaviour, including
   quirks. Any deliberate deviation is recorded as a parity bug in this
   document, not smuggled in as a "fix".
2. **One behaviour spec**: `AGENTS.md`'s UI conventions plus the §10.5 gate are
   the contract. If a behaviour is not written down there, it is not part of
   the port until someone writes it down.
3. **Same fixture DB** for manual verification of both apps (a copy of a real
   database with a project, notes, documents, a PDF and clocks).
4. **Changelog discipline**: one line per user-visible change, naming both
   apps; a change that lands in only one app is visibly incomplete.
5. **Weekly parity sweep**: run the gate table's rows that exist so far against
   the gpui build. Rows fail loudly, early.

### 10.4 Branch strategy — recommended

Two options, and the recommendation is the first:

| Option | Shape | Fit |
| --- | --- | --- |
| **A. Long-lived branch (recommended)** | Tag the current state on `main`, cut `gpui-migration` from the tag; per-phase branches merge into it; `main` stays egui-only and shippable | **Best today**: no CI, no tags, 5 commits, low activity — nothing on `main` needs to see the churn |
| B. Trunk + gated second binary | `crates/app-gpui` lives on `main`, never packaged, hidden behind a build flag | Better only if `main` is busy; without CI it puts an unfinished 22k-line UI in the default workspace build for everyone |

Why A, given today's repo state: with no tags and no CI, a broken `main` is
unrecoverable except by hand. A branch makes the tag the restore point and
keeps `main` releasable at every moment. The cost of A is drift: UI fixes that
land on `main` must be re-applied to the port.

**Decision rule**: if `crates/app` starts receiving more than occasional fixes
(roughly: more than one merged UI change per week), drift outruns the branch and
option B becomes safer. Recorded here so the switch is a decision, not a
surprise.

Regardless of option: core changes land on `main` first and are merged into the
port; the migration branch must not change core APIs.

### 10.5 Parity gate — the cutover checklist

The gpui build must pass **every** row before the egui app is retired. Judged on
a release-build run launched from a dev bundle, at 940×600 and 1280×800.

| Area | Pass criteria (observable) |
| --- | --- |
| Task boards | All seeded + user boards render with identical columns and card order; column create/rename/move/delete; card create/edit/archive/restore; drag within a column and across columns produces the same persisted order; drop on an empty column works |
| Card modal | Every field present: title, markdown description (edit **and** preview), vim normal/insert, due date, labels, project picker, area-board picker, "show on Tasks boards"; `:w` saves without closing; Esc semantics match; modal opens for cards from Tasks, All, and project boards |
| All board | Shows every non-archived card with the correct area label; scrolling 1,000+ cards leaves no blank or duplicated rows |
| Archive | Table renders with sticky header; board filter defaults to **all**; search filters; restore and delete work |
| Projects | Tree with folders/projects above projects; identifiers shown, full name in tooltip; drag reorder among siblings; filter matches name **and** identifier; Overview inline rename commits on Enter/blur and cancels on Esc; identifier regenerates until locked; doc tabs open/read/write under the projects root; Tasks tab shows the project board |
| Notes | Tree, open tabs, autosave debounce, edit/preview toggle, vim, rename and delete |
| Documents | Folder tree; new folder/new file inline naming; upload via native dialog with unique `-2` names; Markdown tabs autosave; PDF renders at min/100%/max zoom; page scroll; text selection produces the same quads/text; annotations persist to the sidecar and survive reload; a corrupt PDF fails the same way |
| Clocks | Search filters cities; 12/24 toggle; add/remove city; drag reorder persists a complete permutation; move-earlier/later still available; people add/remove; travel add/end/extend/relocate; placement at expiry and the minute tick behave as today |
| Theme and type | Dark and light tokens visually match; every font size comes from the shared type table (no stray `px()` in view code); all icons render (no blank boxes); reduced motion disables animation |
| Platform | Custom titlebar band with traffic lights in the same positions; dock icon set; window minimum 940×600; dev bundle launches; packaged `.app`/DMG opens |
| Persistence | Section, board, dark mode, open tabs, tree visibility and `vim_mode` survive a restart; missing/corrupt state file falls back to defaults |
| Data safety | Boot integrity check and quarantine path; backup snapshot cadence; atomic writes; an external MCP write appears without restarting the app |
| Engineering | `cargo fmt --check`, `cargo clippy --workspace` warning-free, `cargo test --workspace` green; no `println!`/`eprintln!` in app code; log files written and rotated |
| Tests | Ported test count stated; every intentionally dropped test listed with a reason; the vim command matrix in `AGENTS.md` passes |
| **No silent drops** | Any row that cannot pass is either fixed or added to a **Dropped** table in this document with explicit sign-off. "It looked fine" is not evidence |

### 10.6 Retirement, rollback, and abandonment

**Retire egui when**: every §10.5 row passes, the gpui build has been the
default binary for one release whose rollback is a one-line revert, and no P0
or P1 parity bug is open.

**Cutover mechanics**: flip the default binary in `packager.json` /
`default-run`; keep `crates/app` in-tree for one release as
`kandrium-app-legacy` (not shipped); then delete it. Core is untouched
throughout, so cutover involves no data migration.

**Rollback after cutover** — revert the default binary and ship a patch from
`main`. Triggers:

- any P0: data loss or corruption, crash on launch, corrupted PDF sidecar,
  truncated autosave;
- more than three open P1 parity bugs at the first release review;
- drag-and-drop or the vim editor unusable in daily use.

Rollback cost is low by construction: both crates exist, core is shared, and
`ui-state.json` is additive (egui keeps reading its own storage; only
gpui-side preferences set during the window are lost).

**Abandon the attempt** — the spike's kill criteria (§11) plus:

- **Phase 4 exit review is a hard stop.** If vim parity is not demonstrable
  there, stop; do not carry an unfinished editor into Phase 5 hoping it
  improves.
- Two consecutive gpui-kit/gpui-pre bumps that cannot be fixed within a normal
  patch cycle → stop; the foundation is not stable enough to build on.
- The owner records an absolute deadline at Phase 2 start. This document does
  not set a date, because the recommendation is not to start.
- **If abandoned**: delete `crates/app-gpui` and the `gpui-migration` branch.
  Keep the spike notes and this document as the record. Nothing in core, the
  DB, or the shipped app changes.

### 10.7 The honest cost line

Duplicated maintenance spans **Phase 2 through cutover** — i.e. the entire
migration, not a slice of it. Because `crates/app` is never gutted, **no phase
reduces the duplication**; that is the explicit price of the "egui is always
the fallback" guarantee. At peak you maintain ~22.7k lines of egui UI and a
second, partially complete gpui UI, with two preference stores and two bug
surfaces.

The cheapest honest reading: if the migration is worth doing, it is worth doing
with this duplication. If the duplication is unacceptable, the migration is not
worth doing — which is the current recommendation.

---

## 11. Spike first (do this before any migration decision is binding)

One throwaway crate, ~2–3 days, answering only the questions that can kill the
project. No production code.

1. **Board read path**: open the real `kandrium.db` read-only via
   `kandrium-core`, render 3 columns × ~8 cards in gpui with gpui-kit, at
   940×600 and 1280×800. Does the layout idiom match (h-scroll + per-column
   v-scroll, no infinite-width trap)? Does scrolling behave?
2. **Text editing + keys**: a multi-line input bound to an entity, with
   Normal/Insert handling for `h j k l x i Esc` and a caret you can place from
   code. Measure: can you *read* the caret index, *set* it, can a keystroke be
   **consumed before insertion**, and does Esc reach your handler? This is the
   go/no-go for §5.1, and it decides whether vim layers on gpui-kit's editor
   the way Zed's does (§6.2).
3. **PDF pixel upload**: take one PNG-equivalent RGBA frame from the existing
   `pdf.rs` worker and display it with zoom/pan. Proves §5.4's surface.
4. **Drag**: one column of cards with a drop index and a 200 ms move
   animation. Proves §5.2's host replacement.
5. **Fork hygiene**: build with `gpui-kit = "=0.9.1"`, confirm what `gpui-pre`
   resolves to, check `cargo tree` for a single gpui identity, run the licence
   gate over the resolved tree (§8.4, §8.7), and try one patch release bump to
   see the breakage class.
6. **Reference check**: confirm that the gpui APIs you relied on in 1–4 exist
   in `gpui-pre 0.3.12`, not just in Zed's `0.2.2` (§6.5). Any gap here is the
   real cost of the fork.

**Kill criteria**: if (2) needs more than a week of caret plumbing, or gpui-kit
cannot match the app's token styling for markdown/cards, stop and stay on egui.
Record the result in this file either way.

### 11.1 Spike result — PASS (2026-11-10)

All four probes ran against the real database and real UI code:

| Probe | Question | Result |
| --- | --- | --- |
| A | 3 columns × real cards, h-scroll + per-column v-scroll | Pass — layout idiom matches, no infinite-width trap |
| B | caret read/set, key consumed before insert, Esc handling | Pass |
| C | PDF page → RGBA → gpui image, zoom + pan | Pass (`hayro` output reused unchanged) |
| D | lift / reflow / snap-back animation host | Pass (`with_animation`, curves from `anim.rs`) |

**Harness resolved.** `TestAppContext` works. The two blockers were both
mechanical:

- enable `test-support` on **both** `gpui-pre` and `gpui-kit` (the latter turns
  on `gpui-base/test-support`, which is what makes observation synchronous);
- force `taffy` to `0.15.0` (`cargo update -p taffy --precise 0.15.0`). The
  `gpui-fast?/test-support` optional backend pins `=0.15.0` even though it is
  never built; a pre-existing lock holding `0.15.1` reports a phantom conflict.
  A fresh resolve picks 0.15.0 on its own.

One thing the spike changed: observation is **opt-in per element**
(`.test_support()`), and `#[gpui::test]` (not `#[gpui_kit::test]`, which the
gpui-kit docs mention but does not exist) is the attribute to use. A headless
test can assert render count, laid-out `bounds()`, click dispatch, and Esc
delivery with focus retained.

**Estimate that moved**: headless tests went from *L / High* to **S / Low** for
shell and board-level tests — better than §6.4 predicted. The duplicate-ID
warning scan is the only egui test capability not matched.

**Finding that changed the plan**: `kandrium_core::services::board::get_board`
writes on read (it reconciles `area_card_order` for non-project boards), so a
second, read-only front-end fails with `SQLITE_READONLY` and spurious
`data_version` bumps would wake the app's watcher. This added a core change to
Stage 1 (§7.1), which the original phase table did not anticipate.

### 11.2 Stage 1 foundation — DONE (2026-11-10)

Runnable, judged before later stages build on it.

**Delivered**

- **Workspace isolation.** `crates/app-gpui` is its own workspace
  (`members = ["."]`) and the root `Cargo.toml` has
  `exclude = ["crates/app-gpui"]`. `default-members` was rejected because it
  does not affect `cargo test --workspace`; only a separate workspace keeps the
  egui build/test/clippy cost identical. Run with
  `cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- app`.
- **Core read path** (§7.1): `get_board_readonly`, `derive_area_order` /
  `persist_area_order`, `db::open_db_readonly`; `get_board` semantics
  unchanged. Two new core tests.
- **Theme.** `src/theme.rs` owns the palette (dark + light, same hex values as
  the egui app), 6 px radius, Inter/JetBrains Mono loaded from
  `crates/app/assets/fonts/`, and a `Text` enum whose `metrics()` is the single
  size table. `TextStyled::text()` is the only way to apply a size. Structural,
  not a convention: there is no other public size source.
- **Icons.** `src/assets.rs` renders the existing
  `crates/app/assets/icons/*.svg` through `svg().data(include_bytes!(…))`; no
  copied assets, no asset-path map.
- **Shell.** Title bar with the segmented control (icon + label, accent-soft
  selected chip) and five real sections. Tasks and Clocks render live data;
  Notes / Documents / Projects show explicit not-ported cards.
- **Board view.** Read-only: 280 px columns, card titles, due dates, labels,
  and the `meta_label` (project identifier on Tasks boards, filed-area title on
  project boards). No drag, no modal.
- **Clocks view.** Saved cities with `format_clock` output and a working 12/24
  toggle.
- **macOS.** Dev bundle technique documented in `crates/app-gpui/README.md`
  (`target/KandriumGpui-dev.app` with a symlinked executable); the app was
  launched from that bundle against the real database.

**Gates**: `cargo fmt` clean; `cargo clippy --workspace --all-targets` clean
for the gpui workspace, `cargo clippy --workspace` clean at the root;
`cargo test --workspace` 278 green; `cargo test` in `crates/app-gpui` 10 green
(2 unit, 5 spike/harness, 3 shell).

**Estimate that moved**: the shell + theme + read-only board was ~1 stage of
the table's phase 2 and landed without surprises. The phase order held. The one
thing to watch is that Stage 2 needs a *second* (writable) connection — the
read-only default was the right call for Stage 1 but cannot serve mutations.



---

## 12. What breaks, and what you'd be agreeing to lose

**Hard losses**

- **Vim fidelity for a long tail.** The 47-test behavior matrix (`dd`, `ciw`,
  `f/t`, `.` repeat, text objects, inclusive visual cursor) must be rebuilt on
  a new input model. Months of tuning, and no guarantee the same edge cases
  (IME, clipboard, mouse-then-Normal) behave identically.
- **Layout regression coverage.** gpui has a real headless harness
  (`TestAppContext` / `VisualTestContext`), but the duplicate-ID shape scan has
  no counterpart and every geometry assertion has to be rebuilt (§7.9).
- **The paint-list FLIP trick.** Animation becomes per-element; the exact feel
  of `anim.rs` is re-derived, not ported.
- **`egui_commonmark`'s styling control and syntect token colors** in preview.
- **Framework ownership.** You no longer control your UI toolkit's release
  cadence; Longbridge does, weekly, on their fork.

**What you'd be agreeing to**

- Pin gpui-kit exactly, commit `Cargo.lock`, and budget a port on every bump.
  With both dependencies at 0.x and one of them shipping weekly, the app is
  permanently on a moving surface. A breaking change can arrive mid-feature.
- Accept the `gpui-pre` snapshot as the runtime — including its GPU/renderer
  behavior on macOS versions you have not tested.
- Accept that the egui app must stay buildable as the fallback until Phase 7,
  which means two UI crates of maintenance for the duration.

**What does *not* break**

- All data: schema, migrations, backups, notes/documents/projects files,
  PDF sidecars. The Stage 1 core change is additive (`get_board_readonly` +
  `open_db_readonly`); `get_board` keeps its exact writing semantics.
- `kandrium-mcp` and its 13-tool camelCase contract.
- Logging, panic reports, atomic writes, DB safety.
- `cargo packager` and the dev `.app` workflow.

---

## 13. Preconditions (all hard, none negotiable)

1. **Commit the working tree.** It is currently dirty: 12 modified files
   (+564/−9) and **5,328 lines of untracked Clocks work**, including
   `migrations/0008_clocks.sql` and `crates/core/src/clocks.rs`. Migrating on
   top of an uncommitted feature is asking for data loss.
2. **Tag a release** (`v0.1.0` or the shipped state). There are no tags today
   and only 5 commits on `main`. The tag is the egui fallback's restore point.
3. **CI green first.** `.github/workflows` does not exist; `plan.md` item 6
   already asks for fmt/clippy/test. Without it you cannot prove the egui
   baseline stayed correct, nor catch a gpui-side regression, during a
   multi-phase rewrite.
4. **Branch strategy**: migration on `gpui-migration`, `main` stays shippable
   and egui-only. No phase merges to `main` until Phase 7.
5. **Ownership**: name the person who owns the vim rewrite. If nobody does,
   the project dies at Phase 4 — plan accordingly (or drop vim from scope and
   accept a non-modal editor, which is a product decision, not a port).
6. **Pin and record versions**: `gpui-kit = "=0.9.1"`, its `gpui-pre
   =0.3.12`, committed lockfile, and the rustc version — in this document.
7. **Parity checklist before the flip**: every row of §4 demonstrated on the
   gpui build, plus 940×600 and 1280×800 window checks, dark/light, reduced
   motion, and the vim test matrix (§10.5).
8. **Add a licence gate with the dependency.** gpui-kit's licence is verified
   **Apache-2.0** (§8.3), so nothing blocks adoption; the remaining action is
   mechanical — a `cargo deny` allow-list in CI so a GPL/AGPL/MPL crate cannot
   enter the tree (§8.7). Until CI exists, run `cargo tree` by hand on the
   adoption commit.

---

## 14. Uncertainty register

Things this plan does **not** know, and where they are resolved:

| Unknown | Why it matters | Resolved in |
| --- | --- | --- |
| Does gpui-kit's editor expose vim bindings / caret control, and can a key be consumed before insertion? | §5.1 is XL or M/L | **Probe B: pass.** Full vim port is Stage 3 (§9.2) |
| Exact `gpui-pre` package identity and re-export shape | Dependency hygiene | **Resolved**: `gpui-pre 0.3.12`, lib name `gpui`, `taffy` 0.15.0, harness in §11.1 |
| gpui-kit markdown styling hooks (tokens, code blocks) | §5.3 loss is quantifiable | Stage 3 (§9.2) — not needed for the shell |
| Whether animation can target arbitrary element bounds at 60 fps | §5.2 feel | **Probe D: pass** with `with_animation` |
| `TestAppContext`/`VisualTestContext` API details, input simulation incl. IME | §7.9 scope | **Resolved**: `#[gpui::test]`, `.test_support()`, click/key simulation (§11.1) |
| Dock-icon override still works under gpui's `NSApplication` | macOS polish | Not yet tested; Stage 7 |
| gpui's macOS minimum + GPU requirement vs. the app's 10.15 floor | Packaging claim in README | Not yet tested; before Phase 7 |
| Which Zed APIs exist in `gpui-pre 0.3.12` vs. Zed `0.2.2` | Every §6 reference is potentially stale | Verified per API as used; recurring task |
| Whether Longbridge will state in writing that the editor/markdown/dock components are independent of Zed's GPL crates | §8.5's "clean" verdict is evidence-based, not a clearance | Ask upstream; otherwise the §8.7 rule and a `cargo deny` allow-list are the mitigation |

---

## 15. Recommendation summary

> **Status (2026-11-10)**: the decision was reversed after the user reviewed the
> spike. All four probes passed, the harness was resolved, and the migration was
> approved. Stage 1 (foundation) is complete on the `gpui-migration` branch; the
> egui app is still the shipping product. §9.1 and §9.2 are the executable next
> stages.

1. **At planning time: no.** Keep egui 0.36. It is not the bottleneck. The
   portal is now approved against that recommendation, with the fallback intact.
2. **Toolchain**: gpui-kit + its `gpui-pre` snapshot, nothing else, and never
   mixed with registry `gpui`.
3. **Before anything**: commit, tag, add CI. (Branch `gpui-migration`, `main`
   stays egui-only.)
4. **Spike**: done — §11.1. It decided in favour of proceeding.
5. **Plan for**: XL vim editor, XL projects, L PDF surface, M animation host
   (revised in §6.4). The rest is ordinary but voluminous layout work.
