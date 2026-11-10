//! Tasks: a read-only board rendered from the real database.
//!
//! Columns, cards, titles, due dates and the project / area label are drawn;
//! nothing is interactive yet (drag, the card modal and editing are later
//! stages).

use gpui_kit::TestSupportExt;
use gpui_kit::prelude::*;
use gpui_kit::{IntoElement, div, px};

use kandrium_core::{BoardWithColumns, Card, Column};

use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens, label_color};

/// Width of one board column, matching the egui app.
const COLUMN_W: f32 = 280.0;

pub fn board_view(board: &BoardWithColumns, t: &Tokens) -> impl IntoElement {
    div()
        .id("board-view")
        .test_support()
        .size_full()
        .flex()
        .flex_col()
        .child(header(board, t))
        .child(
            div()
                .id("board-columns")
                .flex_1()
                .min_h_0()
                .flex()
                .gap(px(12.))
                .p(px(16.))
                .overflow_x_scroll()
                .overflow_y_hidden()
                .children(board.columns.iter().map(|column| column_view(column, t))),
        )
}

fn header(board: &BoardWithColumns, t: &Tokens) -> impl IntoElement {
    let cards: usize = board.columns.iter().map(|c| c.ordered_cards().len()).sum();
    div()
        .w_full()
        .px(px(16.))
        .pt(px(14.))
        .flex()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .text(Text::Heading)
                .text_color(t.text)
                .child(board.title.clone()),
        )
        .child(
            div()
                .text(Text::Small)
                .text_color(t.text_faint)
                .child(format!("{} cards", cards)),
        )
        .child(div().flex_1())
        .child(
            div()
                .rounded(px(4.))
                .bg(t.accent_soft)
                .px(px(8.))
                .py(px(2.))
                .text(Text::SmallStrong)
                .text_color(t.accent_text)
                .child("Read-only preview"),
        )
}

fn column_view(column: &Column, t: &Tokens) -> impl IntoElement {
    let cards = column.ordered_cards();
    div()
        .id(format!("board-column-{}", column.id))
        .test_support()
        .flex_shrink_0()
        .w(px(COLUMN_W))
        .h_full()
        .flex()
        .flex_col()
        .rounded(px(8.))
        .bg(t.card)
        .border_1()
        .border_color(t.border)
        .child(
            div()
                .w_full()
                .px(px(12.))
                .py(px(10.))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .text(Text::Strong)
                        .text_color(t.text)
                        .truncate()
                        .child(column.title.clone()),
                )
                .child(
                    div()
                        .text(Text::SmallStrong)
                        .text_color(t.text_faint)
                        .child(cards.len().to_string()),
                ),
        )
        .child(
            div()
                .id(format!("board-column-cards-{}", column.id))
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(px(6.))
                .px(px(8.))
                .pb(px(8.))
                .overflow_y_scroll()
                .children(cards.into_iter().map(|card| card_view(card, t))),
        )
}

fn card_view(card: &Card, t: &Tokens) -> impl IntoElement {
    div()
        .id(format!("board-card-{}", card.id))
        .test_support()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(4.))
        .rounded(px(t.radius))
        .bg(t.card)
        .border_1()
        .border_color(t.border)
        .p(px(10.))
        .child(
            div()
                .text(Text::Button)
                .text_color(t.text)
                .child(card.title.clone()),
        )
        .when_some(card.meta_label.clone(), |el, label| {
            el.child(
                div()
                    .text(Text::Small)
                    .text_color(t.text_muted)
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
                                        .text(Text::Small)
                                        .text_color(t.text_muted)
                                        .child(card.due_date.clone()),
                                ),
                        )
                    })
                    .children(card.labels.iter().filter_map(|index| {
                        label_color(*index)
                            .map(|color| div().size(px(18.)).rounded_full().bg(color))
                    })),
            )
        })
}
