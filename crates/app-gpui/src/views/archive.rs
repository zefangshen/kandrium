//! Archived cards: search, board filter, sortable table, restore, delete.
//!
//! Ported from the egui `views/archive.rs`. The table keeps a sticky header
//! outside the scroll area and fixed columns over a horizontal floor, so the
//! rows stay readable at the minimum window width.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, Context, Entity, IntoElement, MouseButton, Pixels,
    TestSupportExt, Window, canvas, div, px,
};
use kandrium_core::{ArchivedCard, Board};

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens, label_color};

const HEADER_H: f32 = 30.0;
const ROW_H: f32 = 34.0;
const CARD_MIN_W: f32 = 180.0;
const W_LABELS: f32 = 90.0;
const W_BOARD: f32 = 130.0;
const W_COLUMN: f32 = 120.0;
const W_ARCHIVED: f32 = 170.0;
const W_DUE: f32 = 100.0;
const W_ACTIONS: f32 = 76.0;
/// Every column, its gaps and the scrollbar lane.
const MIN_TABLE_W: f32 =
    CARD_MIN_W + W_LABELS + W_BOARD + W_COLUMN + W_ARCHIVED + W_DUE + W_ACTIONS + 7.0 * 8.0 + 16.0;

/// Runtime state of the archive page (not persisted, like egui).
pub struct ArchiveState {
    pub search: Entity<InputState>,
    /// `"all"` or a board id. Never empty: egui's default is `"all"`.
    pub board_filter: String,
    /// `true` = newest first, `false` = oldest first (egui's default).
    pub sort_newest: bool,
    pub filter_open: bool,
}

impl ArchiveState {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {
            search: cx.new(|cx| InputState::new(window, cx).placeholder("Search archived cards…")),
            board_filter: "all".to_string(),
            sort_newest: false,
            filter_open: false,
        }
    }
}

/// The archive page body. `anchor` receives the board-filter trigger's window
/// bounds, which the root-level overlay uses to float the filter menu.
pub fn archive_view(
    cards: &[ArchivedCard],
    boards: &[Board],
    state: &ArchiveState,
    anchor: &Rc<RefCell<Option<Bounds<Pixels>>>>,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let query = state.search.read(cx).value().trim().to_lowercase();
    let filter = state.board_filter.clone();
    let mut filtered: Vec<&ArchivedCard> = cards
        .iter()
        .filter(|card| {
            (query.is_empty() || card.title.to_lowercase().contains(&query))
                && (filter == "all" || card.board_id == filter)
        })
        .collect();
    filtered.sort_by(|a, b| {
        let order = a.archived_at.cmp(&b.archived_at);
        if state.sort_newest {
            order.reverse()
        } else {
            order
        }
    });
    let count = filtered.len();

    let filter_label = if filter == "all" {
        "All Boards".to_string()
    } else {
        boards
            .iter()
            .find(|board| board.id == filter)
            .map(|board| board.title.clone())
            .unwrap_or_else(|| filter.clone())
    };

    let toolbar =
        div()
            .id("archive-toolbar")
            .test_support()
            .h(px(40.))
            .px(px(30.))
            .flex()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(t.border)
            .child(div().w(px(260.)).child(
                crate::views::field_input(&state.search, t).prefix(icon(
                    Icon::Search,
                    12.,
                    t.text_faint,
                )),
            ))
            .child(
                div()
                    .id("archive-board-filter")
                    .test_support()
                    .relative()
                    .h(px(26.))
                    .min_w(px(140.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .rounded(px(5.))
                    .bg(t.field)
                    .border_1()
                    .border_color(t.border)
                    .cursor_pointer()
                    .text(Text::Body)
                    .text_color(t.text)
                    .child(filter_label)
                    .child(icon(Icon::ChevronDown, 12., t.text_muted))
                    .child({
                        // The menu is an overlay at the app root, so the trigger
                        // publishes its window bounds for anchoring.
                        let anchor = anchor.clone();
                        canvas(
                            move |bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut App| {
                                *anchor.borrow_mut() = Some(bounds);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0()
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.toggle_archive_filter(window, cx)
                    })),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text(Text::SmallStrong)
                    .text_color(t.text_faint)
                    .child(format!("{count} card{}", if count == 1 { "" } else { "s" })),
            );

    let table_header = div()
        .h(px(HEADER_H))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(flex_header_cell("Card", t))
        .child(header_cell("Labels", W_LABELS, t))
        .child(header_cell("Board", W_BOARD, t))
        .child(header_cell("Column", W_COLUMN, t))
        .child(sort_header(state.sort_newest, t, cx))
        .child(header_cell("Due", W_DUE, t))
        .child(header_cell("", W_ACTIONS, t));

    let body = if filtered.is_empty() {
        div()
            .id("archive-empty")
            .test_support()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .text(Text::Body)
            .text_color(t.text_muted)
            .child("No archived cards found.")
            .into_any_element()
    } else {
        div()
            .id("archive-table-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div().min_w(px(MIN_TABLE_W)).flex().flex_col().children(
                    filtered
                        .iter()
                        .enumerate()
                        .map(|(index, card)| archive_row(index, card, t, cx)),
                ),
            )
            .into_any_element()
    };

    div()
        .id("archive-view")
        .test_support()
        .size_full()
        .flex()
        .flex_col()
        .child(toolbar)
        // One inset for the sticky header and the rows, matching the 30 px
        // page gutters of Notes and Documents.
        .child(
            div()
                .id("archive-table")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .px(px(30.))
                .child(
                    div()
                        .min_w(px(MIN_TABLE_W))
                        .flex()
                        .flex_col()
                        .border_b_1()
                        .border_color(t.divider)
                        .bg(t.pasteboard)
                        .child(table_header),
                )
                .child(body),
        )
        .into_any_element()
}

/// The floating board filter menu. Rendered at the app root so it overlays the
/// table instead of taking layout space; anchored under the trigger and
/// clamped inside the window.
pub fn filter_menu_panel(
    anchor: Bounds<Pixels>,
    boards: &[Board],
    board_filter: &str,
    viewport_w: Pixels,
    viewport_h: Pixels,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let width = anchor.size.width.max(px(180.));
    let height = px(8.) + px(26.) * (boards.len() as f32 + 1.);
    let x = anchor
        .origin
        .x
        .min((viewport_w - width - px(8.)).max(px(8.)))
        .max(px(8.));
    let below = anchor.origin.y + anchor.size.height + px(4.);
    let y = below
        .min((viewport_h - height - px(8.)).max(px(8.)))
        .max(px(8.));
    let tokens = *t;
    div()
        .id("archive-board-filter-menu")
        .test_support()
        .absolute()
        .left(x)
        .top(y)
        .w(width)
        .p(px(4.))
        .flex()
        .flex_col()
        .gap(px(1.))
        .rounded(px(tokens.radius))
        .bg(tokens.card)
        .border_1()
        .border_color(tokens.border)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(filter_row(
            "all",
            "All Boards",
            board_filter == "all",
            t,
            cx,
        ))
        .children(
            boards
                .iter()
                .map(|board| filter_row(&board.id, &board.title, board_filter == board.id, t, cx)),
        )
        .into_any_element()
}

fn header_cell(text: &str, width: f32, t: &Tokens) -> AnyElement {
    div()
        .w(px(width))
        .flex_shrink_0()
        .text(Text::Label)
        .text_color(t.text_faint)
        .child(text.to_uppercase())
        .into_any_element()
}

fn flex_header_cell(text: &str, t: &Tokens) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .text(Text::Label)
        .text_color(t.text_faint)
        .child(text.to_uppercase())
        .into_any_element()
}

fn sort_header(newest_first: bool, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    div()
        .id("archive-sort-archived")
        .test_support()
        .w(px(W_ARCHIVED))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(4.))
        .cursor_pointer()
        .rounded(px(3.))
        .hover(move |style| style.bg(t.hover))
        .text(Text::Label)
        .text_color(t.text_faint)
        .child("ARCHIVED")
        .child(
            div()
                .text(Text::Micro)
                .text_color(t.text_faint)
                .child(if newest_first { "▾" } else { "▴" }),
        )
        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| this.toggle_archive_sort(cx)))
        .into_any_element()
}

fn filter_row(
    board_id: &str,
    label: &str,
    selected: bool,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let id = board_id.to_string();
    let tokens = *t;
    div()
        .id(format!("archive-filter-{board_id}"))
        .test_support()
        .w_full()
        .h(px(26.))
        .px(px(8.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .cursor_pointer()
        .hover(move |style| style.bg(tokens.hover))
        .text(Text::Body)
        .text_color(if selected { t.text } else { t.text_muted })
        .child(label.to_string())
        .on_click(
            cx.listener(move |this, _: &ClickEvent, _window, cx| this.set_archive_filter(&id, cx)),
        )
        .into_any_element()
}

fn archive_row(
    index: usize,
    card: &ArchivedCard,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let card_id = card.id.clone();
    let archived_at = if card.archived_at.is_empty() {
        "—".to_string()
    } else {
        card.archived_at.clone()
    };
    let due = if card.due_date.is_empty() {
        "—".to_string()
    } else {
        card.due_date.clone()
    };
    let labels: Vec<AnyElement> = card
        .labels
        .iter()
        .take(4)
        .filter_map(|index| label_color(*index))
        .map(|color| {
            div()
                .w(px(14.))
                .h(px(6.))
                .rounded(px(3.))
                .bg(color)
                .into_any_element()
        })
        .collect();

    div()
        .id(format!("archive-row-{}", card.id))
        .test_support()
        .h(px(ROW_H))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .when(index % 2 == 1, |el| el.bg(t.hover))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text(Text::Button)
                .text_color(t.text)
                .truncate()
                .child(card.title.clone()),
        )
        .child(
            div()
                .w(px(W_LABELS))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(3.))
                .children(labels),
        )
        .child(muted_cell(&card.board_title, W_BOARD, t))
        .child(muted_cell(&card.column_title, W_COLUMN, t))
        .child(muted_cell(&archived_at, W_ARCHIVED, t))
        .child(muted_cell(&due, W_DUE, t))
        .child(
            div()
                .w(px(W_ACTIONS))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(4.))
                .child(row_action(
                    format!("archive-restore-{}", card.id),
                    Icon::RotateCcw,
                    t.text_muted,
                    t,
                    {
                        let card_id = card_id.clone();
                        cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.restore_archived_card(&card_id, cx)
                        })
                    },
                ))
                .child(row_action(
                    format!("archive-delete-{}", card.id),
                    Icon::Trash2,
                    t.danger,
                    t,
                    cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.request_delete_archived_card(&card_id, cx)
                    }),
                )),
        )
        .into_any_element()
}

fn muted_cell(text: &str, width: f32, t: &Tokens) -> AnyElement {
    div()
        .w(px(width))
        .flex_shrink_0()
        .text(Text::Body)
        .text_color(t.text_muted)
        .truncate()
        .child(text.to_string())
        .into_any_element()
}

fn row_action(
    id: String,
    glyph: Icon,
    color: gpui_kit::Rgba,
    t: &Tokens,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let tokens = *t;
    div()
        .id(id)
        .test_support()
        .w(px(24.))
        .h(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .cursor_pointer()
        .hover(move |style| style.bg(tokens.hover))
        .child(icon(glyph, 14., color))
        .on_click(handler)
        .into_any_element()
}
