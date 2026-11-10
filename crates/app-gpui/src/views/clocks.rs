//! Clocks: saved cities rendered from the real database.
//!
//! The layout is deliberately simpler than the egui view (no drag reordering,
//! no people or travel windows yet) but the data and formatting come from
//! `kandrium-core`, so the times are the same ones the shipping app shows.

use gpui_kit::TestSupportExt;
use gpui_kit::prelude::*;
use gpui_kit::{App, ClickEvent, IntoElement, Window, div, px};

use crate::assets::{Icon, icon};
use crate::store::ClockRow;
use crate::theme::{Text, TextStyled, Tokens};

/// Card width. The egui view fits three across; a wrapping row keeps the same
/// card proportions without a custom layout pass.
const CARD_W: f32 = 280.0;

pub fn clocks_view(
    rows: &[ClockRow],
    use_24h: bool,
    t: &Tokens,
    toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id("clocks-view")
        .test_support()
        .size_full()
        .flex()
        .flex_col()
        .child(header(use_24h, t, toggle))
        .child(
            div()
                .id("clocks-grid")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_wrap()
                .gap(px(16.))
                .px(px(16.))
                .pb(px(16.))
                .overflow_y_scroll()
                .children(rows.iter().map(|row| card(row, t)).collect::<Vec<_>>())
                .when(rows.is_empty(), |el| {
                    el.child(
                        div()
                            .text(Text::Body)
                            .text_color(t.text_muted)
                            .child("No saved cities yet."),
                    )
                }),
        )
}

fn header(
    use_24h: bool,
    t: &Tokens,
    toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .w_full()
        .px(px(16.))
        .py(px(14.))
        .flex()
        .items_center()
        .gap(px(10.))
        .child(icon(Icon::Globe, 18., t.accent))
        .child(div().text(Text::Heading).text_color(t.text).child("Clocks"))
        .child(div().flex_1())
        .child(
            div()
                .id("clocks-toggle")
                .test_support()
                .flex()
                .items_center()
                .gap(px(2.))
                .p(px(2.))
                .rounded(px(6.))
                .bg(t.field)
                .border_1()
                .border_color(t.border)
                .cursor_pointer()
                .on_click(toggle)
                .child(chip("12h", !use_24h, t))
                .child(chip("24h", use_24h, t)),
        )
}

fn chip(label: &'static str, selected: bool, t: &Tokens) -> impl IntoElement {
    div()
        .px(px(8.))
        .h(px(24.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .text(Text::SmallStrong)
        .text_color(if selected {
            t.accent_text
        } else {
            t.text_muted
        })
        .when(selected, |el| el.bg(t.accent_soft))
        .child(label)
}

fn card(row: &ClockRow, t: &Tokens) -> impl IntoElement {
    div()
        .id(format!("clock-card-{}", row.id))
        .test_support()
        .w(px(CARD_W))
        .flex()
        .flex_col()
        .gap(px(6.))
        .rounded(px(t.radius))
        .bg(t.card)
        .border_1()
        .border_color(t.border)
        .p(px(14.))
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .child(
                    div()
                        .text(Text::Strong)
                        .text_color(t.text)
                        .truncate()
                        .child(row.city.clone()),
                )
                .child(
                    div()
                        .text(Text::Small)
                        .text_color(t.text_faint)
                        .child(row.text.offset.clone()),
                ),
        )
        .child(
            div()
                .flex()
                .items_baseline()
                .gap(px(6.))
                .child(
                    div()
                        .text(Text::ClockTime)
                        .text_color(t.text)
                        .child(row.text.time.clone()),
                )
                .when_some(row.text.period.clone(), |el, period| {
                    el.child(
                        div()
                            .text(Text::Small)
                            .text_color(t.text_muted)
                            .child(period),
                    )
                }),
        )
        .child(
            div()
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(row.text.date.clone()),
        )
}
