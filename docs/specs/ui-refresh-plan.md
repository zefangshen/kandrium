# UI refresh plan — gpui app

Status: implemented through P7 on `gpui-migration`; this document stays the
spec and records the two corrections made while implementing it (§2 Tasks,
§2 Documents). Scope: visual system + the structural changes the mockup
settled. No new controls, no new navigation.

Inputs:

- `docs/design/ui-mockup/index.html` (screens `1`–`5`, `E`, `I`, `?theme=light`)
- `docs/design/ui-mockup/README.md` — binding do-not-change list

Files named below are the real ones in this tree; the gpui crate is its own
workspace (`crates/app-gpui/Cargo.toml`, `members = ["."]`), so nothing here
changes what the shipping egui app compiles or tests, except where a shared
`kandrium-core` change is called out.

---

## 1. Tokens

All colour tokens live in `crates/app-gpui/src/theme.rs`: the struct is
`Tokens` (L22–50), the palettes are `Tokens::dark()` (L54–77) and
`Tokens::light()` (L80–103). Every consumer reads fields, so a palette change
is data-only.

### 1.1 Colour tokens

| `Tokens` field | Now (dark) | Mockup dark | Mockup light | Mockup alias |
|---|---|---|---|---|
| `titlebar` | `#1B1B1E` | `#161618` | `#EEEEEF` | `--chrome` |
| `panel` | `#26262A` | `#1F2023` | `#FFFFFF` | `--surface-2` |
| `pasteboard` | `#19191C` | `#131314` | `#F3F3F4` | `--canvas` |
| `card` | `#2E2E33` | `#19191B` | `#FBFBFC` | `--surface` |
| **new** `surface_2` | — | `#1F2023` | `#FFFFFF` | `--surface-2` (card fill) |
| **new** `surface_3` | — | `#242528` | `#F4F4F6` | `--surface-3` (card hover) |
| `border` | `#3C3C43` | `#26262A` | `#E3E3E7` | `--line` |
| `divider` | `#333339` | `#26262A` | `#E3E3E7` | `--line` |
| **new** `border_hover` | — | `#34353A` | `#D2D2D8` | `--line-2` |
| `text` | `#ECECEF` | `#E6E6E9` | `#1C1C20` | `--ink` |
| `text_muted` | `#AEAEB6` | `#A0A0A8` | `#5B5B65` | `--ink-2` |
| `text_faint` | `#97979E` | `#6E6E77` | `#83838D` | `--ink-3` |
| **new** `text_ghost` | — | `#4C4C55` | `#ABABB4` | `--ink-4` |
| `icon` | `#D4D4DA` | `#A0A0A8` | `#5B5B65` | `--ink-2` |
| `hover` | `#34343A` | `rgba(255,255,255,.045)` | `rgba(20,20,24,.045)` | `--wash` |
| `pressed` | `#3E3E45` | `rgba(255,255,255,.07)` | `rgba(20,20,24,.075)` | `--wash-2` |
| `selected` | `#233A63` | `rgba(77,141,247,.13)` | `rgba(31,99,214,.09)` | `--accent-wash` |
| `accent` | `#4B8BF5` | `#4D8DF7` | `#1F63D6` | `--accent` |
| `accent_text` | `#8CB6FA` | `#A6C8FF` | `#1A53B6` | `--accent-ink` |
| `accent_soft` | `#243657` | `rgba(77,141,247,.13)` | `rgba(31,99,214,.09)` | `--accent-wash` |
| **new** `accent_line` | — | `rgba(77,141,247,.55)` | `rgba(31,99,214,.45)` | `--accent-line` |
| `field` | `#1E1E22` | `#101011` | `#FFFFFF` | `--field` |
| `danger` | `#EB5A46` | `#E5674F` | `#B3351F` | `--danger` |

Notes:

- `selected` and `accent_soft` become the same value in both schemes; keep both
  fields so existing call sites stay readable (selection wash vs. accent soft
  fill), but set them equal in `dark()`/`light()`.
- `ok` (`#7BAB6A` / `#4A7D3C`) is **not** added: nothing in the gpui app renders
  a success state today, and adding an unused token invites drift.
- `LABEL_COLORS` (L107–116) is content, not chrome — unchanged.

### 1.2 Text tiers

`Text` has eight slots and the only size table in the crate
(`Text::metrics`, theme.rs L159–171). The mockup's tiers do not fit those eight
without retuning, so the table becomes twelve slots. Existing names keep their
meaning; new names are added around them.

| Slot | Now (size/lh/weight) | After | Used for |
|---|---|---|---|
| `Small` | 13/18 regular | **12/17** regular | secondary labels, tree row names, tab labels |
| `Body` | 16/22 regular | **13/18** regular | body copy, modal field values |
| `Button` | 16/22 medium | **12.5/17 medium** | controls, card titles, buttons |
| `Heading` | 20/26 semibold | **17/22 semibold** | section headings, project title |
| `Monospace` | 16/22 mono | **13/18** mono | code blocks in previews |
| `Strong` | 16/22 semibold | **12.5/17 semibold** | column titles, city names |
| `SmallStrong` | 13/18 semibold | **12/16** semibold | field labels, chips |
| `ClockTime` | 20/26 mono | **30/35** mono | clock numerals |
| **new** `Chrome` | — | 12.5/17 medium | titlebar section control |
| **new** `Micro` | — | 10/14 mono | counts, identifiers, due dates, offsets, day line |
| **new** `Label` | — | 10/14 semibold | uppercase tree-head labels |
| **new** `Reading` | — | 14/23 regular | Markdown prose and editor source |

The mockup's 9.5 px mono variants collapse into one `Micro` slot at 10 px: a
0.5 px split cannot be seen reliably and would double the number of call-site
edits. The `text_sizes_match_the_design_system` test (theme.rs L241–257) and
`radius_is_six_pixels` (L259–263) must be updated with the table.

### 1.3 Radii and spacing

| Item | Now | After | Where |
|---|---|---|---|
| control / card radius | `t.radius = 6` | **6 (fixed)** | theme.rs L49 |
| large surface radius | hardcoded `8` | **`radius_lg = 8`** (new field) | `tasks.rs` L202 column shell; clock card |
| page gutter | 16 | **16** | board canvas, clocks, trees |
| board-canvas padding | `p(16)` | **`px 16`, `pb 16`** (20 px band on top) | `tasks.rs` L138 |
| column gap | 12 | **12 (fixed)** | `tasks.rs` L137 |
| column width | 280 | **280 (fixed)** | `tasks.rs` L22 |
| card gap | 7 via `SLOT_H` | **7 (fixed)** | `tasks.rs` L25 |
| clock card height / gap | 156 / 12 | **156 / 12 (fixed)** | `clocks.rs` L50–52 |
| tree width / rail | 240 / 30 | **240 / 30 (fixed)** | `projects.rs` L43–45, notes/documents L36–38 |
| tree row height | ~26 (`py(4)`) | **26 explicit** | all three trees |
| tree head height | ~34 | **38** | all three trees |
| board tab strip | ~38 (`pt(10)`) | **40** | `tasks.rs` L70 |
| board tab | 28 | **26** | `tasks.rs` L92 |
| detail tab strip | ~26 | **40** | `projects.rs` L1098, notes L477, documents L934 |
| clocks toolbar | ~56 | **46** | `clocks.rs` L713 |
| titlebar | **48** | **44**, `px 12`, gap 16 | `app.rs` L4146–4151 |
| tree indent | 12/level | **14/level** | `projects.rs` L755/L835 |

Fixed by the README and not touched: 280 px columns, three visible columns,
156 px clock cards three-across, 6 px control radius, one accent, floating
zero-width scrollbars with the 7 px inset, drag curves in `anim.rs`, vim-modal
editing and its mode strip, identifier rules, tree ordering and drag rules,
clock placement semantics, bottom-centre inverted toasts, native traffic
lights, reduced motion.

---

## 2. Per-section work

### Tasks — `crates/app-gpui/src/views/tasks.rs`

Changes:

1. `tabs_strip` (L70–107): 40 px strip, `px(16)`, gap 4, tabs 26 px tall with
   `px(10)`, radius 6, **no border**; selected = `surface_2` fill + `text`,
   unselected = `text_muted` + `--wash` hover. Add the active board's card count
   as a `Micro` child, right of the label, in `text_faint`.
2. Delete `header()` (L154–183) and replace its call (L130) with a band:
   `div().id("board-band").h(px(20.)).px(px(16.))`.
3. `board_view` (L124–152): drop the top 16 px from the canvas padding so the
   band is the only space between tabs and columns.
4. `column_view` (L194–263): fill `card` (the column surface) with
   `radius_lg`; header padding `px 12 / pt 9 / pb 7`; count becomes a bordered
   pill (`Micro`, radius full, `px 6`).
5. `card_body` (L388–441): fill `surface_2`, border `--line`, radius 6,
   padding `9/10/10`; `.hover` sets `surface_3` + `border_hover`. Title uses
   `Button` (now 12.5/17 medium), meta label uses `Micro` in `text_faint`.
   Due date uses `Micro`; the label dots shrink from 18 px circles to 6 px.
6. `CardGhost` (L444–480): keep as is (see §6).

Does not change: `COLUMN_W`, `SLOT_H`, drop-target logic, `move_area_card`
wiring, drag animations, `test_support()` ids (`board-column-<id>`,
`board-card-<id>`, `board-tabs`), All-board merge via `ordered_cards()`.

**Board header fate**

| Element | Today | After |
|---|---|---|
| Board title | `Text::Heading` in `header()` | Dropped as a separate row. The active tab already carries it, and `task_tabs` always includes the open board (`tasks.rs` L55–67), so the title is never lost. |
| Card count | `"{n} cards"` | Moves into the active tab as `Micro`. |
| Drag hint | "Drag a card to reorder" | Removed. It is tutorial copy, absent from the mockup. |
| Search / board actions | Do not exist in the gpui app | Not added; controls are out of scope. The egui app keeps its own. |

Counts for every tab would need one query per board. Phase 3 counts the active
board only; a per-tab count is a follow-up that caches `board → count` in
`AppShell` and refreshes on `reload_displayed_board`.

### Card editor — `crates/app-gpui/src/views/card_modal.rs`

Changes: `field_label` (L323) → `Label`; panel radius/padding and the three
field shells keep their geometry but move to `field`/`--line` values; the mode
strip badge uses `Micro`; `choice_chip` (L457) uses `SmallStrong` with the new
palette.

Does not change: 560 px width, 190 px description box, vim capture, focus trap,
preview toggle, label swatches (26×16), due-date width, `Save`/`Cancel`.

Explicitly not ported: the mockup's Project / "Show on Tasks boards" dropdown
selects. The chip rows stay — a dropdown is a new control.

### Projects — `crates/app-gpui/src/views/projects.rs` (+ `app.rs`, `assets.rs`)

Changes:

1. `tree_panel` header (L541–580): 38 px, `px 12`, `Label` title, mono count of
   active projects (`flat_projects(...).len()`), keep the three actions.
   Remove the leading folder glyph.
2. `folder_row` (L733–800): 26 px row, `pl(10 + depth*14)`, 14 px folder icon,
   12 px chevron, selected = `--wash-2` instead of the accent wash.
3. `project_row` (L802–923): **name first**, `Small` in `text`; identifier moves
   to the right edge as `Micro` in `text_faint`. New 14 px icon
   `Icon::FolderColumns` (folder outline plus two vertical strokes) added in
   `assets.rs` with one SVG under `crates/app/assets/icons/`.
   Keep: 2 px insertion markers, drag ghost, tooltip, selected/archived states.
4. `archived_row` (L925–974): same palette and icon changes.
5. Detail tab strip (L1098–1157): 40 px, `px 14`, tabs 26 px, active = `text`
   plus a 1 px accent underline, inactive = `text_muted` + wash hover.
6. `overview_panel` (L1186–1384): `px 20 / pb 26`, field label column 92 px,
   title `Heading` (17), summary counts as `Micro`, prose blocks capped at
   660 px.

`app.rs` owns the shell and titlebar only; no Projects logic moves out of the
view.

### Clocks — `crates/app-gpui/src/views/clocks.rs` (+ `clocks_panels.rs`)

Changes: toolbar 46 px with `px 20`, heading 17 px; grid `px 20 / pt 16 /
pb 26` (L971–978 already scrolls, keep that); card fill `surface_2`,
`radius_lg`, padding `11/12`, city `Strong`, offset `Micro`, clock `ClockTime`
(30/35), day `Micro`; people row gets a top hairline; avatar 22 px, away state
uses `accent_line` border + `accent_text` initials; `VISITING` strip 19 px,
`Micro`, accent wash.

Does not change: `CARD_H`, `GAP`, `COLUMNS`, `MIN_COL_W`, `GridZone` maths,
`preview_slots`, 12/24-hour state, minute tick, travel placement.

Deferred: hiding the remove button until hover. gpui group-hover needs checking
before relying on it; an always-visible affordance is the current, working
state.

### Notes — `crates/app-gpui/src/views/notes.rs`

Changes: tree head 38 px with `Label` (keep the folder/file row icons); tab
strip 40 px with the mockup's tab metrics; the Preview/Rename/Delete actions
stay on the right of the strip (`tab_action`, L601) but use `Small` + wash
hover; preview body gets a 660 px reading measure with 30 px side padding and
90 px bottom padding; editor gets the same measure and `Reading` source text.

Preview-first: a newly opened note starts in preview (`NoteTab.preview = true`
on open), with the existing "Edit" action switching to source. Mode is per tab
and not persisted, matching today.

Does not change: autosave debounce (400 ms), `write_note`/`write_atomic`,
vim capture, dirty dot, rename/delete flows, tree drag and collapse.

### Documents — `crates/app-gpui/src/views/documents.rs`

Changes: tree head 38 px / `Label`; tab strip 40 px; Markdown body gets the
660 px measure and the same padding as Notes; PDF viewer untouched.

Correction: `DocTab.saved` holds the last-written file text, not a timestamp,
so the mockup's `saved 09:41` label is not ported. The strip keeps the tabs,
the existing dirty dot and the preview action; no new save state is added.

Does not change: worker reader, page cache, annotation sidecars, tools,
zoom, persisted tab paths.

Explicitly not ported: breadcrumbs. The tree already shows location, and crumbs
would add path state to every tab.

---

## 3. Phases

Each phase compiles, runs and is reviewable on its own. Nothing later is
required for an earlier phase to be correct.

**Gate (every phase)**

```sh
cd crates/app-gpui
cargo fmt --manifest-path Cargo.toml
cargo clippy --manifest-path Cargo.toml --all-targets
cargo test --manifest-path Cargo.toml
cargo build --manifest-path Cargo.toml --bin kandrium-gpui
killall kandrium-gpui; open -n target/KandriumGpui-dev.app   # repo root
```

`target/KandriumGpui-dev.app` was created with the recipe already used for
`Kandrium-dev.app`, pointing `Contents/MacOS/kandrium-gpui` at
`../../../../crates/app-gpui/target/debug/kandrium-gpui`, with bundle id
`app.kandrium.desktop.gpui.dev` so both apps can run. If visual capture is
blocked, hand the eyeball list to the user.

| Phase | Work | Eyeball |
|---|---|---|
| **P1** Palette | `Tokens` fields + new colour fields, both schemes, all values in §1.1. No layout edits. | All five sections: no text disappears into a surface, accent still reads on `field`, hairlines visible in light. |
| **P2** Type table | `Text::metrics`, the two theme tests, then fix any call site that overflows (tabs, card titles, clock cards). | Board tabs, card titles, clock numerals inside 156 px, modal labels, tree rows at 26 px. |
| **P3** Tasks structure | 20 px band, delete `header()`, tab counts, column/card restyle, 6 px label dots. | Three columns, drag a card across columns, empty column, All board, project Tasks tab. |
| **P4** Projects tree | 38 px head, name-first project rows, new folder-with-columns icon, 14 px icons, indent 14. | Folder/project rows, archive view, filter, drag reorder, selected state. |
| **P5** Reading surfaces | Notes + Documents strips, 660 px measure, `saved` in the strip, preview-first note. | Open a note (preview), Edit, autosave, long lines wrap at the measure; a PDF tab still renders. |
| **P6** Clocks | 46 px toolbar, grid padding, card interior, people hairline, avatar, VISITING strip. | 3-across at narrow width, wrap to a second row, away/visiting states, inspector. |
| **P7** Cleanup | Remove now-unused tokens/imports, update any headless snapshot expectations. | Full pass in dark and light. |

Order is cheapest-first: P1 is data only, P2 is one table, P3–P6 are one file
each. P2 deliberately runs before P3–P6 so structural work is eyeballed at
final type sizes.

---

## 4. Where the mockup's CSS does not map to gpui

| CSS | Mockup | gpui reality | Cheapest faithful approximation |
|---|---|---|---|
| Hairlines | `border: 1px solid var(--line)` | `border_1()` is 1 logical px too, but it is inset and two adjacent elements double to 2 px | One owner per seam: columns draw their own border, the canvas draws none; hairline rows use `border_b_1` on the row, not on both neighbours. |
| Type tiers | 12, 12.5, 13, 14, 17, 30; 9.5/10 mono; weights 500/560 | f32 sizes are fine; weights are named (`MEDIUM`, `SEMIBOLD`) | 560 → `SEMIBOLD`; collapse 9.5/10 into `Micro` 10; keep 12.5 as-is. |
| Tree label tracking | `letter-spacing: .09em` | No tracking on the text styling path in use (`TextStyled::text`) | Uppercase + semibold at 10 px. Accept the tighter look. |
| Card elevation | Flat cards; shadows only on ghost, modal, window | `.hover` works; shadow support in gpui 0.9 must be verified | Cards: hover = `surface_3` + `border_hover`, no shadow. Modal: flat on the existing dimmed backdrop. Ghost: current accent border. |
| Focus rings | `0 0 0 3px accent-wash` | No CSS outline/ring | Keep the 1 px `border` → `accent` change on focused inputs; skip the outer ring. |
| Mono numerals | `tabular-nums`, `-.028em` tracking | JetBrains Mono is already fixed pitch; tracking may be unavailable | Nothing for tabular figures. If 30 px overflows, drop to 29 px rather than fake tracking. |
| 20 px band | `<header class="bhead" style="height:20px;padding:0 16px">` | Direct | `div().h(px(20.)).px(px(16.))`; exact. |
| `transition: 120ms` on colors | Global | gpui has explicit `with_animation` | Do not animate palette; only the existing drag animations run. |
| Prose line-height | `line-height: 23px` inside Markdown | `markdown()` owns its internal line height | Set the container measure and font only; accept the library's line height. |
| Group hover (clock delete) | Parent hover reveals child | Needs verification in gpui 0.9 | Leave the button always visible. |

---

## 5. Risks and open decisions

Risks:

- **Two shipping UIs.** `crates/app-gpui` is a separate workspace, so these
  changes cannot break the egui build. Anything that reaches into
  `kandrium-core` can. This plan keeps core untouched, including the
  three-column rule.
- **Contrast.** `text_faint` (`#6E6E77`) on `canvas` (`#131314`) is ≈3.6:1.
  Use it only for meta text, never for actionable labels.
- **Type shrink.** Body 16→13 changes hit areas in the modal and trees. P2 is
  isolated so this is caught before structural edits.
- **Label dots.** Card labels shrink 18→6 px, changing card height. Drag
  geometry is computed from rendered bounds, so P3 must re-run the drag tests
  in `harness_test.rs` / `drag_probe.rs`.
- **Counts in tabs.** Per-board counts are N queries if naively added; P3 only
  counts the active board.
- **Light theme has no switch.** `AppShell` hardcodes `Tokens::dark()`. Update
  `light()` values anyway, but do not add a theme toggle.
- **Existing tests pin element ids.** Keep every `test_support()` id and every
  `board-column-*` / `board-card-*` / `project-row-*` id stable.
- **`crates/app-gpui/src/views/projects.rs:128`** already carries a
  `large_enum_variant` clippy warning; do not let P4 add a second one.

### Decisions that need your call

**D1 — Board title and counts.** Recommendation: title lives only in the
active tab (no separate heading), the active tab carries the count, the drag
hint is removed, and the 20 px band stays empty. Alternative: keep a 17 px title
inside the band, which contradicts the reviewed mockup but keeps the board name
visible when tabs overflow.

**D2 — Three columns on non-canonical boards.** The repo seed creates exactly
`To Do / In Progress / Done` (`crates/core/src/seed.rs` L38–51), and the All
board is already three synthetic `CARD_STATUSES` columns. Arbitrary extra
columns are still reachable in existing databases (egui exposes column
create/rename/delete/move, and imported databases keep whatever they had — the
`Waiting` board in the reviewed database is one), so a rule is needed.
Recommendation: **presentation-only lane folding**. In `tasks.rs`, group
columns into three lanes by `status_from_column_title` where it matches, and
fold every non-matching column into the nearest canonical column before it
(falling back to the nearest one after it, so a trailing column lands in Done).
Never hide a card, never write data. A drop into a lane targets the existing
column when it belongs to that lane, otherwise the lane's canonical column (a
real status change through `move_area_card`). Alternatives: (a) one-shot
merge/rename migration in core — honest but irreversible and touches both UIs;
(b) render extra columns alongside the three — contradicts the mockup. If you
prefer (a), it becomes its own spec because it is a data migration, not a UI
change.

**D3 — Project tree row order.** Recommendation: adopt the mockup —
name first, mono identifier right-aligned — with filtering still matching both
(`project_matches`) and the full name still in the tooltip. Only presentation
changes. The current row is identifier-first, so this is the most visible tree
change after the icon.
Alternative: keep identifier-first and only replace the glyph; less faithful to
the mockup but a smaller diff.

Secondary decisions: notes preview-first by default (recommended yes); tree-head
glyph removed in all three trees (recommended yes, one line each); clock delete
stays always visible (recommended yes for this pass).

---

## 6. Not porting

| Mockup element | Why |
|---|---|
| Browser window frame, traffic-light offsets, drop shadow | The real window is native; traffic lights are a binding do-not-change item. |
| Card-editor Project / "Show on Tasks boards" dropdown selects | New control, listed as Expensive, and controls are out of scope. Chips are restyled instead. |
| Lifted drag ghost with transform, rotation and shadow | Expensive, and drag timing/curves are binding. The current ghost already reads as lifted. |
| Per-column hover-only scrollbars | Expensive, and scrollbars must stay zero-width and floating. |
| Documents breadcrumbs | Duplicates the tree; needs path state per tab. |
| Focus rings, avatar outer ring, insertion-line glow | CSS-only affordances; approximated by border/colour changes. |
| `saved 09:41` as a live timestamp | The existing saved string is ported to the strip; a live clock is new behaviour. |
| Mockup demo keys (`1`–`5`, `E`, `I`), theme query param | Presentation scaffolding, not product. |
| New navigation, buttons, toggles or shortcuts | Explicitly out of scope. |
| `--ok` token | No success state exists in the app. |
