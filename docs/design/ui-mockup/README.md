# Kandrium — UI direction mockup

An evolution of the current Kandrium UI, not a redesign. One self-contained
page, no build step, no runtime dependency beyond the Google Fonts CDN
(Inter + JetBrains Mono).

Open [index.html](index.html) directly in a browser. It renders a 1320 × 880
app window and scales down to fit.

## Controls

| Input | Result |
| --- | --- |
| `1`–`5`, or the section control / top bar | Tasks, Projects, Notes, Documents, Clocks |
| `E`, or the top bar | Card editor overlay |
| `I`, or click a clock card | Clocks + people inspector |
| `T` | Dark / light |
| `Esc` | Close the overlay |

Deep links open a single state, useful for screenshots:
`?screen=projects`, `?screen=clocks&overlay=inspector`,
`?screen=tasks&overlay=editor&theme=light`.

All content is illustrative sample data. Nothing here touches the Rust crates.

## Fixed constraints (kept)

Five sections in the order Tasks, Projects, Notes, Documents, Clocks; 280 px
board columns; three columns visible; clocks in a wrapping three-across grid of
156 px cards. Rows are hairline-separated, controls stay 6 px, and there is one
accent.

## What changed versus the current app

1. **Surface ladder flattened.** Current dark tokens step
   `titlebar 1B1B1E → pasteboard 19191C → panel 26262A → card 2E2E33 → border
   3C3C43` — four close grays plus a fairly bright border. The mock uses
   chrome `#161618`, canvas `#131314`, surface `#19191B`, card `#1F2023`, line
   `#26262A`. Depth comes from the hairline and one fill step, not from a stack
   of panels. Light mode is the same idea at `#EEEEEF / #F3F3F4 / #FBFBFC`.
2. **Type scale split.** Current sizes are only 13 / 16 / 20. The mock adds a
   chrome tier (12.5 board and tab labels), a meta tier (9.5–10, uppercase
   section labels), 17 px section headings, and 14 px reading text. Body stays
   at 13 for card titles. Numerals, counts, identifiers and dates are mono.
3. **Cards are flat.** No shadow, 1 px line, card fill one step above the
   column. Hover raises contrast, not elevation.
4. **Board chrome compressed.** Tabs carry mono counts; the board header is one
   17 px title plus a mono count line; column headers are 12.5 with a 9.5 mono
   chip. The drag state is a dashed low-opacity source card plus one lifted
   ghost at the drop point.
5. **Project tree rows are name-first** with the identifier right-aligned in
   mono. Current tree labels projects by identifier only. Renaming, filtering
   and drag insertion are unchanged.
6. **Overview is a field grid**: a 92 px label column for Status / Identifier /
   Source, a five-cell summary strip, and prose capped at 620 px. The tab strip
   is unchanged (pinned Overview + Tasks, closable documents).
7. **Clocks cards breathe**: residents sit on a bottom hairline instead of
   floating mid-card, an away resident gets a ring, and an active travel window
   becomes a 19 px `VISITING` strip. Time remains a 30 px mono numeral.
8. **Markdown pages get a measure**: 660 px column, 30 px side padding, 90 px
   bottom padding, tighter heading rhythm, blockquotes as a 2 px rule.
9. **Preview-first notes.** Opening a note shows the rendered document; Edit
   swaps to a borderless 14 px source view. The vim mode strip stays in the tab
   bar / under the field.

## Cost in gpui

`Cheap` = token or single-function change. `Moderate` = one view or a shared
row, no new state. `Expensive` = needs a component rethink or new plumbing.

| Change | Cost | Notes |
| --- | --- | --- |
| Dark / light palette regrade | Cheap | `Tokens::dark/light` in `crates/app-gpui/src/theme.rs` (mirror the egui `crates/app/src/theme.rs`). Every widget reads tokens. |
| `radius_lg: 8.0` for cards, columns, clock cards | Cheap | Add one field; controls keep 6. |
| Section control restyle (neutral selected pill, accent icon) | Cheap | Single `section_control`; structure and hit areas stay. |
| Titlebar status cluster (⌘K, local dot, date) | Cheap | Pure layout in the titlebar. |
| Mono counts / identifiers / dates | Cheap per site, moderate in bulk | Add `Text::Meta` (9.5/14 mono); swap call sites. |
| New 12.5 / 13 / 17 / 14 scale | Moderate | Add `Text` variants, then re-decide every `Text::Body` (16 px) usage. Mechanical but wide-reaching; 16 px body is currently the default everywhere. |
| Board header + tab counts + column count chip | Moderate | `views/tasks.rs` layout only; data already exists. |
| Card meta row (identifier, due, right-aligned dots) | Moderate | Layout change inside the card row. |
| Project tree name + identifier | Moderate | `projects::show_tree` row composition; filter already matches both. |
| Overview field grid + summary strip | Moderate | New composition, no new state. Prose cap is a max-width wrapper. |
| Markdown measure, padding, heading rhythm | Moderate | Depends on how much of the renderer's block spacing is exposed; blockquote styling may need a custom node. |
| Preview-first notes, borderless source view | Moderate | Mode already exists; this flips the default and the visual treatment. |
| Documents tab strip with path crumbs + dirty dot | Moderate | Strip already exists; crumbs and dot are additive. |
| Clocks card bottom hairline, away ring, `VISITING` strip | Moderate | Inside the fixed 156 px card; the vertical rhythm changes and the strip may need to replace the date line on short cards. |
| Inspector row grid (name / meta / hover actions) | Moderate | Layout only. |
| Card editor: project **and** area as two equal pickers | Expensive | Today the area is a checkbox + conditional board picker. Two first-class pickers require a “none” option, an explicit project↔area exclusivity rule, and validation — product behaviour, not styling. |
| Lifted ghost with rotation / scale / shadow | Expensive | Needs a floating drag layer that can transform. If the ghost is already a positioned clone, cheap; if it is drawn by the scroll area, new plumbing. |
| Per-column hover-reveal scrollbars | Expensive | gpui scrollbar styling is global; making it per-column needs local scrollbar configuration or a custom bar. |

## What I would not change

- **Column width and clock grid.** 280 px columns and the three-across 156 px
  cards are load-bearing; do not make them responsive.
- **One accent.** Blue for selection, focus, drag target and primary action.
  Red and green stay semantic. No per-label colour in chrome.
- **Control radius.** 6 px for controls and fields. The mock's 8 px is only for
  large surfaces (cards, columns, modals).
- **Floating scrollbars with zero reserved width** and the 7 px content inset.
  This is hard-won; do not add a permanent gutter.
- **Drag timing and curves.** Keep the existing `cubic-bezier(0.20, 0, 0, 1)`
  reflow and `cubic-bezier(0.40, 0, 0.20, 1)` snap-back.
- **Vim-modal markdown editing and the mode strip.** Keep `:w`, `Esc`, the
  snapshot undo stack. Do not collapse it into a plain WYSIWYG field.
- **Identifier rules.** Lowercase, derived from the name, locked once edited.
- **Tree ordering and drag rules.** Folders above projects; dragging disabled
  while the filter box has text.
- **Clock placement semantics.** Latest-starting window wins, end exclusive,
  open-ended trips never expire. This is data behaviour, not UI.
- **Bottom-centre inverted toasts, native traffic lights, reduced-motion
  support.**
- **No new navigation.** The five-section control is the whole map; do not add
  a sidebar, command palette page, or sync UI. Kandrium is local-first.

## Licensing

This page is authored from scratch for Kandrium. It borrows stylistic
direction only — spacing rhythm, hairline surfaces, a compact type scale, mono
numerals — from current editor-UI conventions generally. No third-party source
code, CSS, markup, icon set, logo, font file or screenshot is copied or
included, and no external product name or branding is used. Icons are inline
SVG paths drawn for this document. Inter and JetBrains Mono are loaded from
Google Fonts under the SIL Open Font License; ship self-hosted files if the app
adopts them.
