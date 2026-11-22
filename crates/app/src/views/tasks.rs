//! Tasks: the board, its tab strip and the drag-and-drop surface.
//!
//! Reads go through `kandrium_core::get_board_readonly`; every mutation is a
//! `Store` call into core (`move_area_card`). The drag session is gpui's own
//! (`on_drag` / `on_drag_move` / `on_drop`): `on_drag_move` records where a
//! drop would land, `on_drop` asks the shell to commit that target.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::TestSupportExt;
use gpui_kit::base::Scrollbar;
use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::{
    Animation, AnimationExt, AnyElement, App, Bounds, ClickEvent, Context, DragMoveEvent, Entity,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, Render,
    ScrollHandle, Window, canvas, div, px, relative, rgba,
};
use kandrium_core::{
    ALL_BOARD_ID, Board, BoardWithColumns, Card, CardStatus, Column, status_from_column_title,
};

use crate::anim::{
    LIFT, REFLOW, SHIFT, STAGGER_MAX, STAGGER_STEP, ease_standard, eased, shift_offset,
    stagger_order,
};
use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens, label_color};

/// Width of one board column, matching the egui app.
pub const COLUMN_W: f32 = 280.0;
/// Height of the reserved slot at the top of every card row. It keeps the
/// board's row rhythm and makes every row exactly one gap tall.
pub const SLOT_H: f32 = 6.0;
/// Gap height used before the source card has been measured.
pub const GAP_FALLBACK: f32 = 48.0;

/// Measured row rects, keyed by card id and by `gap:<dragged card id>`.
///
/// Written during prepaint (no notify) and read at drag start and drop, so
/// the shell can size the insertion gap and fly the ghost to a real rect.
pub type CardMetrics = Rc<RefCell<HashMap<String, Bounds<Pixels>>>>;

/// Payload carried by gpui's drag session for one card.
#[derive(Clone)]
pub struct CardDrag {
    pub card_id: String,
}

/// Where the pointer would drop right now. `before == None` appends.
#[derive(Clone, PartialEq)]
pub struct DropTarget {
    pub column_id: String,
    pub before: Option<String>,
    /// Bumped when the target changes so the indicator animation restarts.
    pub generation: u64,
}

/// Drag state shared unchanged by every column and card in one board render.
#[derive(Clone)]
struct DragStyle<'a> {
    target: Option<&'a DropTarget>,
    /// Where the gap was before the latest target change, used to glide the
    /// cards that the change displaced.
    previous: Option<&'a DropTarget>,
    dragging: Option<&'a str>,
    /// Row height of the dragged card, and of the gap that replaces it.
    gap_height: f32,
    metrics: &'a CardMetrics,
    /// Card that just landed, with the token of its settle fade.
    settled: Option<(&'a str, u64)>,
    tokens: &'a Tokens,
    reduced: bool,
    /// Card whose title is being edited in place, with its field.
    editing: Option<(&'a str, &'a Entity<InputState>)>,
    /// Card row under the pointer, which surfaces its hover pencil.
    hovered: Option<&'a str>,
    /// Per-lane card-list scroll handles, keyed by the lane's column id.
    scrolls: &'a HashMap<String, ScrollHandle>,
    /// Pointer position during a drag, written by the columns and read while
    /// rendering so the edge scroll keeps stepping while the pointer rests.
    pointer: Rc<Cell<Option<Point<Pixels>>>>,
}

/// The drag session as the board needs it: the current and previous
/// insertion targets, the card that is out of the flow, the measured row
/// height, the metric sink and the card that just landed.
#[derive(Clone)]
pub struct BoardDrag<'a> {
    pub target: Option<&'a DropTarget>,
    pub previous: Option<&'a DropTarget>,
    pub dragging: Option<&'a str>,
    pub gap_height: f32,
    pub metrics: &'a CardMetrics,
    pub settled: Option<(&'a str, u64)>,
    /// Card whose title is being edited in place, with its field.
    pub editing: Option<(&'a str, &'a Entity<InputState>)>,
    /// Card row under the pointer.
    pub hovered: Option<&'a str>,
    /// Per-lane card-list scroll handles, keyed by the lane's column id.
    pub scrolls: &'a HashMap<String, ScrollHandle>,
    /// Pointer position during a drag, shared with the columns.
    pub pointer: Rc<Cell<Option<Point<Pixels>>>>,
}

/// The tab list: the virtual All board first, then the visible (non-project)
/// boards, plus the open board when it is not among them — the
/// active-project-board rule.
pub fn task_tabs(boards: &[Board], active: Option<&Board>) -> Vec<Board> {
    let mut tabs = vec![Board {
        id: ALL_BOARD_ID.to_string(),
        title: "All".to_string(),
    }];
    tabs.extend(boards.iter().cloned());
    if let Some(active) = active
        && !tabs.iter().any(|board| board.id == active.id)
    {
        tabs.push(active.clone());
    }
    tabs
}

/// The board strip's card search: the input entity plus the query it holds.
#[derive(Clone, Copy)]
pub struct Search<'a> {
    pub input: &'a Entity<InputState>,
    pub query: &'a str,
}

/// The Archived page's presence in the strip: whether it is selected, and the
/// controls it contributes to the row (count, board filter, search).
pub struct StripArchive {
    pub selected: bool,
    pub controls: Option<AnyElement>,
}

/// The board tab strip. The active board carries its card count, the tabs own
/// the left, and the right edge is a fixed group of search, divider and
/// Archived so the entry never reads as another board.
///
/// The tab cluster is the only child that yields width: with many boards it
/// scrolls horizontally, so the search group keeps its place at the 940 px
/// minimum window width instead of overflowing the strip.
pub fn tabs_strip(
    tabs: &[Board],
    active_id: Option<&str>,
    active_cards: usize,
    archive: StripArchive,
    search: Search<'_>,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> impl IntoElement {
    let tokens = *t;
    let query = search.query;
    let archive_selected = archive.selected;
    let archive_controls = archive.controls;
    // The card search belongs to boards only; the Archived page hides it.
    let search_field = (!archive_selected).then(|| {
        div()
            .id("board-search")
            .test_support()
            .relative()
            .w(px(200.))
            .flex_shrink_0()
            .capture_key_down({
                let has_query = !query.is_empty();
                cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" && has_query {
                        cx.stop_propagation();
                        this.clear_board_search(window, cx);
                    }
                })
            })
            .child(
                crate::views::field_input(search.input, t)
                    .prefix(icon(Icon::Search, 11., t.text_faint))
                    .suffix(div().when(!query.is_empty(), |el| {
                        el.child(
                            div()
                                .id("board-search-clear")
                                .test_support()
                                .cursor_pointer()
                                .child(icon(Icon::X, 11., t.text_faint))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.clear_board_search(window, cx)
                                })),
                        )
                    })),
            )
            .into_any_element()
    });
    div()
        .id("board-tabs")
        .test_support()
        .w_full()
        .h(px(38.))
        .px(px(14.))
        .flex()
        .items_center()
        .gap(px(2.))
        .border_b_1()
        .border_color(t.border)
        // The tab cluster takes the shrinking: it is content-sized while it
        // fits and scrolls horizontally once the strip runs out of width, so
        // the search and Archived are never pushed past the right edge.
        .child(
            div()
                .id("board-tab-list")
                .test_support()
                .h(px(26.))
                .flex()
                .items_center()
                .gap(px(2.))
                .flex_shrink(1.)
                .min_w(px(0.))
                .overflow_x_scroll()
                .children(tabs.iter().map(|board| {
                    let selected = !archive_selected && active_id == Some(board.id.as_str());
                    let id = board.id.clone();
                    div()
                        .id(format!("board-tab-{}", board.id))
                        .test_support()
                        .flex_shrink_0()
                        .h(px(26.))
                        .px(px(9.))
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .rounded(px(5.))
                        .cursor_pointer()
                        .text(Text::Chrome)
                        .text_color(if selected { t.text } else { t.text_muted })
                        .when(selected, |el| el.bg(t.surface_2))
                        .when(!selected, |el| {
                            el.hover(move |style| style.bg(tokens.hover))
                        })
                        .child(board.title.clone())
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_board(&id, window, cx);
                        }))
                })),
        )
        // The flexible gap owns every spare pixel: the tab cluster keeps the
        // left, and the right group hugs the right edge.
        .child(div().flex_1())
        // Archived shares this row: its controls (count, board filter, search)
        // sit as one group immediately left of the divider and the Archived
        // entry, so the left half of the row is empty space.
        .children(archive_controls)
        .children((!archive_selected).then(|| {
            div()
                .id("board-task-count")
                .test_support()
                .flex_shrink_0()
                .mr(px(10.))
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(match active_cards {
                    1 => "1 task".to_string(),
                    count => format!("{count} tasks"),
                })
                .into_any_element()
        }))
        .children(search_field)
        .child(
            div()
                .id("board-tab-divider")
                .test_support()
                .w(px(1.))
                .h(px(16.))
                .flex_shrink_0()
                .mx(px(6.))
                .bg(t.divider),
        )
        .child(
            div()
                .id("board-tab-archived")
                .test_support()
                .flex_shrink_0()
                .h(px(26.))
                .px(px(9.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(5.))
                .cursor_pointer()
                .text(Text::Chrome)
                .text_color(if archive_selected {
                    t.text
                } else {
                    t.text_muted
                })
                .when(archive_selected, |el| el.bg(t.surface_2))
                .when(!archive_selected, |el| {
                    el.hover(move |style| style.bg(tokens.hover))
                })
                .child(icon(Icon::Archive, 12., t.icon))
                .child("Archived")
                .on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.open_archive(window, cx)),
                ),
        )
}

/// The inline "+ Add card" footer: whether it is shown at all (never on the
/// synthetic All board; a project board's own Tasks tab opts in) and the open
/// draft, if any.
#[derive(Clone, Copy)]
pub struct AddCard<'a> {
    pub allowed: bool,
    pub form: Option<(&'a str, &'a Entity<InputState>)>,
}

/// The board surface: a 20 px band, then the columns and the insertion gap.
pub fn board_view(
    board: &BoardWithColumns,
    drag: BoardDrag<'_>,
    add: AddCard<'_>,
    query: &str,
    pinned: Option<&str>,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> impl IntoElement {
    let reduced = cx.reduce_motion();
    let style = DragStyle {
        target: drag.target,
        previous: drag.previous,
        dragging: drag.dragging,
        gap_height: drag.gap_height,
        metrics: drag.metrics,
        settled: drag.settled,
        tokens: t,
        reduced,
        editing: drag.editing,
        hovered: drag.hovered,
        scrolls: drag.scrolls,
        pointer: drag.pointer,
    };
    div()
        .id("board-view")
        .test_support()
        .size_full()
        .flex()
        .flex_col()
        // A press anywhere on the board outside the draft cancels it, like
        // egui; the draft stops propagation for its own clicks.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, _window, cx| this.cancel_add_card(cx)),
        )
        .child(div().id("board-band").w_full().h(px(20.)).px(px(16.)))
        .child(
            div()
                .id("board-columns")
                .test_support()
                .flex_1()
                .min_h_0()
                .flex()
                // Columns shrink-wrap their cards: without this the row's
                // default `stretch` makes every column as tall as the board,
                // even when it holds one card.
                .items_start()
                .gap(px(12.))
                .px(px(16.))
                .pb(px(16.))
                .overflow_x_scroll()
                .overflow_y_hidden()
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, window, cx| {
                        this.cancel_card_drag(window, cx)
                    }),
                )
                .children(lanes(&board.columns).iter().map(|lane| {
                    let form = add
                        .form
                        .filter(|(column_id, _)| *column_id == lane.id)
                        .map(|(_, input)| input);
                    column_view(lane, style.clone(), add.allowed, form, query, pinned, cx)
                })),
        )
}

/// One rendered column. On a board with extra user columns, several SQLite
/// columns fold into one lane; the lane keeps its canonical column's id so a
/// drop writes to a real column.
struct Lane<'a> {
    id: &'a str,
    title: &'a str,
    cards: Vec<&'a Card>,
}

/// Group a board's columns into the three canonical lanes. A board that does
/// not carry all three canonical columns is rendered as it is, so no card is
/// ever hidden and no drop is ever impossible.
fn lanes(columns: &[Column]) -> Vec<Lane<'_>> {
    let statuses: Vec<Option<CardStatus>> = columns
        .iter()
        .map(|column| status_from_column_title(&column.title))
        .collect();
    let has_owner = |status: CardStatus| statuses.contains(&Some(status));
    if !CardStatus::ALL.into_iter().all(has_owner) {
        return columns
            .iter()
            .map(|column| Lane {
                id: &column.id,
                title: &column.title,
                cards: column.ordered_cards(),
            })
            .collect();
    }
    // Every non-canonical column joins the nearest canonical column before
    // it, or the nearest one after it (a column before "To Do"), and a
    // trailing column therefore lands in Done.
    let mut folded = statuses.clone();
    for index in 0..columns.len() {
        if folded[index].is_some() {
            continue;
        }
        let before = statuses[..index].iter().rev().flatten().next().copied();
        let after = statuses[index + 1..].iter().flatten().next().copied();
        folded[index] = before.or(after).or(Some(CardStatus::Done));
    }
    CardStatus::ALL
        .into_iter()
        .filter_map(|status| {
            let owner = statuses
                .iter()
                .position(|candidate| *candidate == Some(status))?;
            let mut cards: Vec<&Card> = columns
                .iter()
                .enumerate()
                .filter(|(index, _)| folded[*index] == Some(status))
                .flat_map(|(_, column)| column.ordered_cards())
                .collect();
            cards.sort_by_key(|card| card.display_position);
            Some(Lane {
                id: &columns[owner].id,
                title: &columns[owner].title,
                cards,
            })
        })
        .collect()
}

fn column_view(
    lane: &Lane<'_>,
    style: DragStyle<'_>,
    add_allowed: bool,
    add_form: Option<&Entity<InputState>>,
    query: &str,
    pinned: Option<&str>,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let column_id = lane.id.to_string();
    let t = style.tokens;
    // The typed query narrows every lane by title, case-insensitively. A card
    // with an open inline editor stays visible so the edit is never hidden.
    let needle = query.trim().to_lowercase();
    let filtered = !needle.is_empty();
    // The dragged card leaves the flow entirely; the insertion gap takes its
    // row, so the ghost and the layout never coexist.
    let visible: Vec<&Card> = lane
        .cards
        .iter()
        .copied()
        .filter(|card| style.dragging != Some(card.id.as_str()))
        .filter(|card| {
            !filtered
                || pinned == Some(card.id.as_str())
                || card.title.to_lowercase().contains(&needle)
        })
        .collect();
    let after: Vec<Option<String>> = (0..visible.len())
        .map(|index| visible.get(index + 1).map(|card| card.id.clone()))
        .collect();
    let previous_gap = gap_index(style.previous, &column_id, &visible);
    let next_gap = gap_index(style.target, &column_id, &visible);
    let generation = style
        .target
        .map(|target| target.generation)
        .filter(|_| style.dragging.is_some())
        .unwrap_or(0);
    let footer: Option<AnyElement> = if add_allowed {
        Some(match add_form {
            Some(input) => add_card_form(input, &column_id, query, t, cx),
            None => add_card_button(&column_id, t, cx),
        })
    } else {
        None
    };

    let mut rows: Vec<AnyElement> = Vec::with_capacity(visible.len() + 1);
    for (index, card) in visible.iter().enumerate() {
        if style.dragging.is_some() && next_gap == Some(index) {
            let before = visible.get(index).map(|card| card.id.clone());
            rows.push(gap_slot(style.clone(), generation, &column_id, before, cx));
        }
        let shift = shift_offset(previous_gap, next_gap, index, style.gap_height);
        let stagger = stagger_order(next_gap, index);
        rows.push(card_view(
            &column_id,
            card,
            after[index].as_deref(),
            shift,
            stagger,
            generation,
            style.clone(),
            cx,
        ));
    }
    if style.dragging.is_some() && next_gap == Some(visible.len()) {
        rows.push(gap_slot(style.clone(), generation, &column_id, None, cx));
    }

    let scroll = style.scrolls.get(lane.id).cloned();
    let mut list = div()
        .id(format!("board-column-cards-{}", lane.id))
        .test_support()
        // Growing (not `flex_1`, which would zero the base size and
        // collapse the column) lets the list take the leftover height
        // when the column is clamped, while content still sizes it.
        .flex_grow(1.)
        .min_h(px(30.))
        .flex()
        .flex_col()
        .px(px(8.))
        .pb(px(6.))
        .overflow_y_scroll()
        .children(rows)
        .when(visible.is_empty() && filtered, |el| {
            el.child(
                div()
                    .id(format!("board-column-filtered-empty-{}", lane.id))
                    .test_support()
                    .h(px(30.))
                    .px(px(4.))
                    .flex()
                    .items_center()
                    .text(Text::Small)
                    .text_color(t.text_faint)
                    .child("No matching cards"),
            )
        });
    if let Some(scroll) = &scroll {
        list = list.track_scroll(scroll);
    }

    div()
        .id(format!("board-column-{}", lane.id))
        .test_support()
        .flex_shrink_0()
        .w(px(COLUMN_W))
        // The viewport is the ceiling, not the height: a column is as tall as
        // its header, cards and drop lane until that would pass the board,
        // then the card list scrolls.
        .max_h(relative(1.))
        .flex()
        .flex_col()
        .relative()
        .rounded(px(t.radius_lg))
        .bg(t.card)
        .border_1()
        .border_color(t.border)
        .child(
            div()
                .w_full()
                .px(px(12.))
                .pt(px(9.))
                .pb(px(7.))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .text(Text::Strong)
                        .text_color(t.text)
                        .truncate()
                        .child(lane.title.to_string()),
                )
                .child(
                    div()
                        .h(px(16.))
                        .px(px(5.))
                        .flex()
                        .items_center()
                        .rounded_full()
                        .border_1()
                        .border_color(t.border)
                        .text(Text::Micro)
                        .text_color(t.text_faint)
                        .child(visible.len().to_string()),
                ),
        )
        .child(list)
        .when_some(footer, |el, footer| el.child(footer))
        // The floating bar follows the app's scroll treatment: it is painted
        // over the list, reserves no layout space, and only fades in while the
        // cards overflow, so card widths never shift. Anchoring it to the
        // list's viewport keeps it off the header and the pinned add row.
        .when_some(scroll, |el, scroll| {
            el.child(
                Scrollbar::vertical(&scroll)
                    .id(format!("board-column-scrollbar-{}", lane.id))
                    .viewport_bounds(scroll.bounds()),
            )
        })
        // The whole column accepts a drop, not just its card list: a column
        // that hugs its content is short, so its header covers the pointer
        // when a card is dragged onto the column body. Cards register their
        // own `on_drag_move` later in capture order (descendants run after
        // ancestors), so hovering a card still resolves to `before` it.
        // The column is the fallback for the space that no row covers (the
        // empty area below the last card, or an empty column). Rows register
        // their own `on_drag_move`, but this ancestor runs first, so a blind
        // append here would move the gap to the end on every pointer move and
        // the row listener would move it back — a full-column reflow twice per
        // move.
        .on_drag_move::<CardDrag>({
            let column_id = column_id.clone();
            let pointer = style.pointer.clone();
            cx.listener(move |this, event: &DragMoveEvent<CardDrag>, _window, cx| {
                pointer.set(Some(event.event.position));
                if !event.bounds.contains(&event.event.position) {
                    return;
                }
                if this.pointer_over_row(event.event.position) {
                    return;
                }
                this.set_drop_target(&column_id, None, cx);
            })
        })
        .on_drop::<CardDrag>({
            let column_id = column_id.clone();
            cx.listener(move |this, drag: &CardDrag, window, cx| {
                this.drop_card(&drag.card_id, &column_id, window, cx);
            })
        })
        .into_any_element()
}

/// The index in the in-flow list where the gap for `target` sits.
///
/// The dragged card is not in the list, so `before` is looked up among the
/// visible cards; a missing id (or `None`) appends.
fn gap_index(target: Option<&DropTarget>, column_id: &str, visible: &[&Card]) -> Option<usize> {
    let target = target.filter(|target| target.column_id == column_id)?;
    Some(match target.before.as_deref() {
        Some(before) => visible
            .iter()
            .position(|card| card.id == before)
            .unwrap_or(visible.len()),
        None => visible.len(),
    })
}

/// The insertion placeholder: one full card row, measured under
/// `gap:<dragged id>` so the release flight knows where to land. It keeps
/// its own target while hovered, so the pointer resting over the source row
/// resolves to the source slot rather than the column's append fallback.
fn gap_slot(
    style: DragStyle<'_>,
    generation: u64,
    column_id: &str,
    before: Option<String>,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let t = style.tokens;
    let reduced = style.reduced;
    let dragged = style.dragging.unwrap_or_default().to_string();
    let key = format!("gap:{dragged}");
    let column_id = column_id.to_string();
    div()
        .id("board-drop-gap")
        .test_support()
        .h(px(style.gap_height))
        .w_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        // The 6 px row rhythm stays empty, so the accent block is exactly one
        // card body tall and its measured rect is where a card lands.
        .child(div().h(px(SLOT_H)).w_full().flex_shrink_0())
        .child(
            div()
                .relative()
                .w_full()
                .flex_grow(1.)
                .child(measure(key, style.metrics))
                .child(
                    div()
                        .size_full()
                        .rounded(px(t.radius))
                        .bg(t.accent)
                        .with_animation(
                            format!("drop-gap-{generation}"),
                            Animation::new(REFLOW).with_easing(ease_standard),
                            move |el, delta| {
                                el.opacity(0.16 * eased(reduced, delta, ease_standard))
                            },
                        ),
                ),
        )
        .on_drag_move::<CardDrag>({
            let before = before.clone();
            cx.listener(move |this, event: &DragMoveEvent<CardDrag>, _window, cx| {
                if !event.bounds.contains(&event.event.position) {
                    return;
                }
                this.set_drop_target(&column_id, before.clone(), cx);
            })
        })
        .into_any_element()
}

/// Records an element's painted rect into `metrics` during prepaint. It is
/// absolutely positioned, so it never adds layout height.
fn measure(key: String, metrics: &CardMetrics) -> impl IntoElement {
    let metrics = metrics.clone();
    canvas(
        move |bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut App| {
            metrics.borrow_mut().insert(key, bounds);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// Wraps a card in the staggered glide from its pre-change slot to the new
/// one, or returns it untouched when it did not move.
fn glide(
    card: AnyElement,
    card_id: &str,
    shift: f32,
    stagger: u32,
    generation: u64,
    reduced: bool,
) -> AnyElement {
    if reduced || shift.abs() < 0.5 {
        return card;
    }
    let delay = (STAGGER_STEP * stagger).min(STAGGER_MAX);
    let total = SHIFT + STAGGER_MAX;
    let total_secs = total.as_secs_f32();
    let shift_secs = SHIFT.as_secs_f32();
    let delay_secs = delay.as_secs_f32();
    div()
        .w_full()
        .flex()
        .flex_col()
        .child(card)
        .with_animation(
            format!("card-shift-{card_id}-{generation}"),
            Animation::new(total),
            move |el, delta| {
                let raw = ((delta * total_secs - delay_secs) / shift_secs).clamp(0.0, 1.0);
                let eased = eased(false, raw, ease_standard);
                el.top(px(shift * (1.0 - eased)))
            },
        )
        .into_any_element()
}

/// The closed "+ Add card" row at the bottom of a normal-board column.
fn add_card_button(column_id: &str, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let tokens = *t;
    let id = column_id.to_string();
    div()
        .id(format!("board-add-card-{column_id}"))
        .test_support()
        .mx(px(8.))
        .mb(px(6.))
        .h(px(30.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(t.radius))
        .cursor_pointer()
        .text(Text::Small)
        .text_color(t.text_muted)
        .hover(move |style| style.bg(tokens.hover).text_color(tokens.text))
        .child(icon(Icon::Plus, 12., t.text_muted))
        .child("Add card")
        .on_click(
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.begin_add_card(&id, window, cx)
            }),
        )
        .into_any_element()
}

/// The open draft: a card-like field plus the submit/cancel row, in the same
/// place as the closed button so the column only grows downwards.
fn add_card_form(
    input: &Entity<InputState>,
    column_id: &str,
    query: &str,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let tokens = *t;
    let input = input.clone();
    // A draft that cannot match the active filter would vanish the moment it
    // becomes a card, so say so while it is still being typed. The draft
    // itself is never filtered out.
    let draft = input.read(cx).value().trim().to_string();
    let needle = query.trim().to_lowercase();
    let draft_hidden =
        !needle.is_empty() && !draft.is_empty() && !draft.to_lowercase().contains(&needle);
    div()
        .id(format!("board-add-form-{column_id}"))
        .test_support()
        .mx(px(8.))
        .mb(px(6.))
        .flex()
        .flex_col()
        .gap(px(4.))
        // Keep clicks inside the draft from reaching the board's
        // press-outside cancel.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .capture_key_down(
            cx.listener(|this, event, window, cx| this.handle_add_card_key(event, window, cx)),
        )
        .child(
            div()
                .w_full()
                .child(crate::views::field_input_tall(&input, t)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .id(format!("board-add-submit-{column_id}"))
                        .test_support()
                        .h(px(26.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .bg(t.accent)
                        .cursor_pointer()
                        .text(Text::Small)
                        .text_color(rgba(0xFFFFFFFF))
                        .hover(move |style| style.opacity(0.88))
                        .child("Add card")
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.submit_add_card(cx)
                        })),
                )
                .child(
                    div()
                        .id(format!("board-add-cancel-{column_id}"))
                        .test_support()
                        .size(px(26.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(move |style| style.bg(tokens.hover))
                        .child(icon(Icon::X, 12., t.text_muted))
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.cancel_add_card(cx)
                        })),
                ),
        )
        .when(draft_hidden, |el| {
            el.child(
                div()
                    .id(format!("board-add-filter-hint-{column_id}"))
                    .test_support()
                    .text(Text::Micro)
                    .text_color(t.text_faint)
                    .child("This draft won't match the active filter"),
            )
        })
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn card_view(
    column_id: &str,
    card: &Card,
    after: Option<&str>,
    shift: f32,
    stagger: u32,
    generation: u64,
    style: DragStyle<'_>,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let t = style.tokens;
    let card_id = card.id.clone();
    let column_id = column_id.to_string();
    let after = after.map(str::to_string);
    let tokens = *t;
    let metrics = style.metrics.clone();
    let shell = cx.entity().downgrade();
    let editing = style
        .editing
        .filter(|(id, _)| *id == card_id.as_str())
        .map(|(_, input)| input);

    // The title row. While editing it is the field; otherwise it is the title
    // plus the reserved pencil gutter (the egui card's constant gutter, so the
    // title never reflows when the hover pencil appears). The drag ghost builds
    // the same row without the pencil, so both put the title at the same x.
    let title_row: AnyElement = match editing {
        Some(input) => div()
            .id(format!("board-card-title-edit-{}", card.id))
            .test_support()
            .w_full()
            .child(crate::views::field_input_tall(input, t))
            .into_any_element(),
        None => {
            let show_pencil = style.hovered == Some(card_id.as_str()) && style.dragging.is_none();
            let pencil = show_pencil.then(|| {
                let pencil_id = card_id.clone();
                div()
                    .id(format!("board-card-edit-{pencil_id}"))
                    .test_support()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(move |style| style.bg(tokens.hover))
                    .child(icon(Icon::Pencil, 12., tokens.text_muted))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.begin_card_title_edit(&pencil_id, window, cx)
                    }))
                    .into_any_element()
            });
            title_row(card.title.clone(), pencil, t)
        }
    };

    let mut row = div()
        .id(format!("board-card-{}", card.id))
        .test_support()
        .w_full()
        .flex()
        .flex_col()
        // Keep the board's 6 px row rhythm; the gap also measures a full row.
        .child(div().h(px(SLOT_H)).w_full().flex_shrink_0())
        .child(measure(card_id.clone(), &metrics))
        .child(
            div()
                .w_full()
                .flex()
                .flex_col()
                // The inline field owns its presses: neither the board's
                // press-away commit nor a drag may start from inside it.
                .when(editing.is_some(), |el| {
                    el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            this.handle_card_title_key(event, window, cx)
                        }))
                })
                .child(card_body(
                    card,
                    card.meta_label.as_deref(),
                    title_row,
                    true,
                    t,
                )),
        )
        .on_hover({
            let card_id = card_id.clone();
            cx.listener(move |this, hovered: &bool, _window, cx| {
                this.set_card_hover(hovered.then(|| card_id.clone()), cx);
            })
        })
        // Right-click is the card menu: it never opens the modal or starts a
        // drag, and it is consumed here so the board's press handlers stay out.
        .on_mouse_down(MouseButton::Right, {
            let card_id = card_id.clone();
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.open_card_menu(&card_id, event.position, window, cx);
            })
        });

    // The editing card keeps its row but sheds its click and drag handlers, so
    // the field wins the mouse-down and nothing opens or moves underneath it.
    if editing.is_none() {
        row = row
            .cursor_pointer()
            .on_click({
                let card_id = card_id.clone();
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_card(&card_id, window, cx)
                })
            })
            .on_drag(
                CardDrag {
                    card_id: card_id.clone(),
                },
                {
                    let column_id = column_id.clone();
                    let after = after.clone();
                    // The ghost renders the card it came from, so it carries the
                    // whole row: meta label, due date and label dots included.
                    let ghost_card = card.clone();
                    move |drag: &CardDrag, offset, _window, cx| {
                        // A drag only begins after the pointer passes the threshold,
                        // so recording it here keeps a plain click from starting the
                        // motion layer.
                        let card_id = drag.card_id.clone();
                        if let Some(shell) = shell.upgrade() {
                            shell.update(cx, |this, cx| {
                                this.begin_card_drag(
                                    &card_id,
                                    &column_id,
                                    after.clone(),
                                    offset,
                                    cx,
                                )
                            });
                        }
                        cx.new(|_| CardGhost::new(ghost_card.clone(), tokens))
                    }
                },
            );
    }

    let row = row
        .on_drag_move::<CardDrag>({
            let card_id = card_id.clone();
            let column_id = column_id.clone();
            let after = after.clone();
            cx.listener(move |this, event: &DragMoveEvent<CardDrag>, _window, cx| {
                if !event.bounds.contains(&event.event.position) {
                    return;
                }
                // Top half inserts before this card, bottom half after it.
                let before = if event.event.position.y < event.bounds.center().y {
                    Some(card_id.clone())
                } else {
                    after.clone()
                };
                this.set_drop_target(&column_id, before, cx);
            })
        })
        .on_drop::<CardDrag>({
            let column_id = column_id.clone();
            cx.listener(move |this, drag: &CardDrag, window, cx| {
                this.drop_card(&drag.card_id, &column_id, window, cx);
            })
        })
        .into_any_element();

    // The ghost and the real card occupy the same rect on touchdown; fading
    // the card in over the handoff softens the swap between the floating
    // instance and the one that just entered the flow.
    let row = match style.settled {
        Some((settled_id, token)) if settled_id == card_id.as_str() => {
            let reduced = style.reduced;
            div()
                .w_full()
                .flex()
                .flex_col()
                .child(row)
                .with_animation(
                    format!("card-settle-{card_id}-{token}"),
                    Animation::new(REFLOW).with_easing(ease_standard),
                    move |el, delta| el.opacity(0.45 + 0.55 * eased(reduced, delta, ease_standard)),
                )
                .into_any_element()
        }
        _ => row,
    };

    glide(row, &card_id, shift, stagger, generation, style.reduced)
}

/// The card's title row: the title, then the always-reserved edit-button
/// gutter. The resting card, its hover state and the drag ghost all build the
/// row through here, so the title starts at the same offset and wraps at the
/// same width in every state — the ghost is the same card, not a variant.
///
/// Shared with the shell's settle/snap-back overlay, which paints the same
/// body while the card is in flight.
pub(crate) fn title_row(title: String, pencil: Option<AnyElement>, t: &Tokens) -> AnyElement {
    div()
        .w_full()
        .flex()
        .items_start()
        .gap(px(6.))
        .child(
            div()
                .flex_grow(1.)
                .min_w_0()
                .text(Text::CardTitle)
                .text_color(t.text)
                .child(title),
        )
        .child(
            div()
                .w(px(22.))
                .h(px(18.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .children(pencil),
        )
        .into_any_element()
}

/// The card surface below the row slot. `title` is the title row — the plain
/// title plus the hover pencil, or the inline editor while it is open — so the
/// meta rows below stay shared by both.
///
/// `hover` is false for the drag ghost: it follows the pointer, so the resting
/// card's hover wash would make the ghost change colour mid-drag. The shell's
/// flight overlay uses it the same way.
pub(crate) fn card_body(
    card: &Card,
    meta: Option<&str>,
    title: AnyElement,
    hover: bool,
    t: &Tokens,
) -> AnyElement {
    let tokens = *t;
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(7.))
        .rounded(px(t.radius))
        .bg(t.surface_2)
        .border_1()
        .border_color(t.border)
        .px(px(10.))
        .pt(px(9.))
        .pb(px(10.))
        .when(hover, |el| {
            el.hover(move |style| style.bg(tokens.surface_3).border_color(tokens.border_hover))
        })
        .child(title)
        .when_some(meta.map(str::to_string), |el, label| {
            el.child(
                div()
                    .text(Text::Micro)
                    .text_color(t.text_faint)
                    .child(label),
            )
        })
        .when(!card.due_date.is_empty() || !card.labels.is_empty(), |el| {
            el.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .when(!card.due_date.is_empty(), |row| {
                        row.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .child(icon(Icon::Calendar, 11., t.text_muted))
                                .child(
                                    div()
                                        .text(Text::Micro)
                                        .text_color(t.text_muted)
                                        .child(card.due_date.clone()),
                                ),
                        )
                    })
                    .children(card.labels.iter().filter_map(|index| {
                        label_color(*index).map(|color| div().size(px(6.)).rounded_full().bg(color))
                    })),
            )
        })
        .into_any_element()
}

/// The floating copy of the card that follows the pointer.
///
/// It renders the very same body as the resting card — title behind the 22 px
/// edit-button gutter, the board/project meta label, then the due date and
/// label dots — in the same width, padding, corner radius, fill and border.
/// The egui app builds its ghost the same way: it reuses `card_body` inside a
/// frame matching the chip and adds only elevation and translucency, so
/// picking a card up and putting it down never shifts its text or size.
struct CardGhost {
    card: Card,
    tokens: Tokens,
}

impl CardGhost {
    fn new(card: Card, tokens: Tokens) -> Self {
        Self { card, tokens }
    }
}

impl Render for CardGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.tokens;
        let reduced = cx.reduce_motion();
        let card = &self.card;
        let body = card_body(
            card,
            card.meta_label.as_deref(),
            title_row(card.title.clone(), None, &t),
            false,
            &t,
        );
        div()
            // The resting card fills the column minus its 8 px side padding;
            // matching that is what keeps the pickup free of a width jump.
            .w(px(COLUMN_W - 16.))
            .child(body)
            .with_animation(
                "card-ghost",
                Animation::new(LIFT).with_easing(ease_standard),
                move |el, delta| el.opacity(0.35 + 0.62 * eased(reduced, delta, ease_standard)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(id: &str, title: &str) -> Board {
        Board {
            id: id.into(),
            title: title.into(),
        }
    }

    fn column(id: &str, title: &str, cards: &[(&str, i64)]) -> Column {
        Column {
            id: id.into(),
            title: title.into(),
            cards: cards
                .iter()
                .map(|(card_id, position)| Card {
                    id: (*card_id).into(),
                    title: (*card_id).into(),
                    display_position: *position,
                    ..Default::default()
                })
                .collect(),
            mirrored: Vec::new(),
        }
    }

    #[test]
    fn an_extra_column_folds_into_the_next_canonical_lane() {
        let columns = [
            column("c1", "To Do", &[("a", 1)]),
            column("c2", "In Progress", &[]),
            column("c3", "Waiting", &[("b", 2)]),
            column("c4", "Done", &[("c", 3)]),
        ];
        let lanes = lanes(&columns);
        assert_eq!(
            lanes.iter().map(|lane| lane.title).collect::<Vec<_>>(),
            ["To Do", "In Progress", "Done"]
        );
        assert_eq!(lanes[1].id, "c2");
        assert_eq!(lanes[1].cards.len(), 1);
        assert_eq!(lanes[1].cards[0].id, "b");
        assert_eq!(lanes[2].id, "c4");
    }

    #[test]
    fn a_trailing_extra_column_falls_back_to_done() {
        let columns = [
            column("c1", "To Do", &[]),
            column("c2", "In Progress", &[]),
            column("c3", "Done", &[]),
            column("c4", "Someday", &[("d", 4)]),
        ];
        let lanes = lanes(&columns);
        assert_eq!(lanes.len(), 3);
        assert_eq!(lanes[2].id, "c3");
        assert_eq!(lanes[2].cards[0].id, "d");
    }

    #[test]
    fn a_duplicate_canonical_column_merges_into_its_lane() {
        let columns = [
            column("c1", "To Do", &[]),
            column("c2", "In Progress", &[("a", 1)]),
            column("c3", "in   progress", &[("b", 2)]),
            column("c4", "Done", &[]),
        ];
        let lanes = lanes(&columns);
        assert_eq!(
            lanes
                .iter()
                .flat_map(|lane| lane.cards.iter().map(|card| card.id.as_str()))
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn a_board_without_the_three_canonical_columns_is_rendered_as_it_is() {
        let columns = [
            column("c1", "Backlog", &[]),
            column("c2", "Waiting", &[("a", 1)]),
        ];
        let lanes = lanes(&columns);
        assert_eq!(
            lanes.iter().map(|lane| lane.id).collect::<Vec<_>>(),
            ["c1", "c2"]
        );
    }

    #[test]
    fn tabs_put_all_first_and_append_an_unlisted_active_board() {
        let listed = vec![board("a", "A"), board("b", "B")];
        let tabs = task_tabs(&listed, Some(&board("project", "P")));
        let ids: Vec<_> = tabs.iter().map(|board| board.id.as_str()).collect();
        assert_eq!(ids, ["all", "a", "b", "project"]);
    }

    #[test]
    fn tabs_keep_a_listed_active_board_in_place() {
        let listed = vec![board("a", "A"), board("b", "B")];
        let tabs = task_tabs(&listed, Some(&board("b", "B")));
        let ids: Vec<_> = tabs.iter().map(|board| board.id.as_str()).collect();
        assert_eq!(ids, ["all", "a", "b"]);
    }

    #[test]
    fn the_all_board_is_not_added_twice() {
        let listed = vec![board("a", "A")];
        let tabs = task_tabs(&listed, Some(&board(ALL_BOARD_ID, "All")));
        let ids: Vec<_> = tabs.iter().map(|board| board.id.as_str()).collect();
        assert_eq!(ids, ["all", "a"]);
    }
}
