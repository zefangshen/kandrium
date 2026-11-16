# Spec: fix the normal-mode block cursor in the Markdown editor

Status: proposed (awaiting approval)
Scope: `crates/app` only. No schema, MCP, or dependency changes.

## Symptom

In Normal/Visual vim mode the filled block caret covers only part of the
character under it. The reported screenshot (14 px Reading text, "Writing")
shows the blue block spanning roughly 60 % of a capital `W`, with the rest of
the glyph still visible to the right. The block is also slightly shorter than
the line box, leaving thin gaps above and below.

## Root cause

Two independent geometry errors, plus a legibility problem.

### 1. Width is a fixed cell, not the shaped advance (primary)

`crates/app/src/vim.rs` L2080–2088 (`block_cursor_overlay`) does:

```rust
let width = window.text_system().ch_width(font_id, font_size)
    .unwrap_or(font_size * 0.6);
window.paint_quad(fill(Bounds::new(caret.origin, size(width, caret.size.height)), color));
```

`ch_width` is one typical advance for the font. The editor's font is
proportional (`Text::Reading` = Inter, 14 px) and the editor draws real shaped
text, so a wide glyph (`W`, `M`, CJK) is wider than `ch_width`; the block is
narrower than the glyph. Tabs (rendered as spaces) and wide glyphs are wrong
for the same reason; only the no-character-at-EOL fallback is correct.

### 2. Height is the engine's 85 % cursor height, not the line box

`caret_bounds` returns `PrepaintCaretGeometry::cursor_bounds()`
(`crates/app/src/vim.rs` L2034–2041). That value is built in the input
element as `size(CURSOR_WIDTH, cursor_height)` with
`cursor_height = 0.85 * line_height`, centred vertically
(`gpui-base-0.9.1/src/input/base/element.rs` L939 and L1046–1052). The
geometry also exposes the true `line_height()`
(`gpui-base-0.9.1/src/input/base/state.rs` L409–412). Painting the block at
`caret.size.height` therefore leaves ~7.5 % of the line above and below
uncovered.

### 3. The block paints over the glyph without inverting it

At all four call sites the overlay is appended after the editor element, so it
paints on top of the text (`notes.rs` L490–501, `documents.rs` L1015–1026,
`projects.rs` L2554–2565, `card_modal.rs` L347–358). An opaque accent quad
hides the part of the glyph it covers; the visible remainder outside the quad
is what reads as "the block does not cover the character". It also violates the
usual inverted-glyph convention: the character should be redrawn in the
surface colour on the filled block.

## Fix

### `crates/app/src/vim.rs`

1. Extend `CaretSource` (L2022–2029) and `impl_caret_source!` (L2031–2051):

```rust
fn caret_line_height(&self, cx: &App) -> Option<Pixels>;   // geometry.line_height()
fn caret_char(&self, cx: &App) -> Option<char>;            // rope.get_char(rope.byte_to_char(state.cursor()))
```

`caret_char` uses `ropey` index lookups (`Rope::byte_to_char` +
`Rope::get_char`), so it is O(log n) and allocates nothing.

2. Rewrite `block_cursor_overlay` (L2064–2091) with the extra
`text_color: Hsla` (surface) parameter:

- Resolve the font once. Keep `ch_width` as the fallback advance.
- Cell width:
  - `Some('\t')` → `ch_width * 4.0` (the input's default tab size; the app
    never calls `set_tab_size`).
  - `None`, `'\n'`, `'\r'`, other control characters → `ch_width`.
  - otherwise shape the single character with `text_system().shape_line(...)`
    using the same family/size and use `ShapedLine::width` (falling back to
    `ch_width` if the shape is zero).
- Cell height: `caret_line_height`, falling back to `caret.size.height`.
  Re-centre the origin: `caret.origin.y - (line_height - caret.size.height) / 2`
  so the block spans the full line box.
- Paint the accent quad, then, when the character is a visible non-whitespace
  glyph, paint the shaped character at the cell origin with
  `ShapedLine::paint(origin, line_height, TextAlign::Left, None, ..)` in
  `text_color`. This is the inverted glyph.
- Update the doc comment to describe width/height derivation and inversion.

3. Add the pure helper `block_width(ch, ch_width, shaped) -> Pixels` and unit
   tests: tab = 4 cells, `None`/newline = `ch_width`, ordinary glyph = shaped
   advance.

### Call sites

Pass the surface colour `t.field` (`Tokens::field`) as the inverted glyph
colour at all four sites: `notes.rs` L490, `documents.rs` L1015,
`projects.rs` L2554, `card_modal.rs` L347`. `t.field` is the editor surface:
`install_component_theme` sets the component `background` to `t.field`
(`crates/app/src/theme.rs`), and the card description's `field_textarea` uses
it explicitly (`crates/app/src/views/mod.rs` L168–170).

## Non-goals

- No change to vim motions, modes, `block_cursor()` gating, or the engine's own
  line caret behaviour.
- No change to the MCP server, database, or settings.
- No new dependencies.

## Verification

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets`
3. `cargo test --workspace` (including the new `block_width` tests)
4. `killall kandrium-app; just dev` with the sandbox disabled, then eyeball:
   Normal mode on `W`/`M`/CJK/space/tab/end-of-line: block covers the whole
   cell and the glyph reads as inverted (dark on accent), full line height.
