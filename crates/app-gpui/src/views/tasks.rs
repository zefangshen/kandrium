//! Tasks: the board, its tab strip and the drag-and-drop surface.
//!
//! Reads go through `kandrium_core::get_board_readonly`; every mutation is a
//! `Store` call into core (`move_area_card`). The drag session is gpui's own
//! (`on_drag` / `on_drag_move` / `on_drop`): `on_drag_move` records where a
//! drop would land, `on_drop` asks the shell to commit that target.

use gpui_kit::TestSupportExt;
use gpui_kit::prelude::*;
use gpui_kit::{
    Animation, AnimationExt, AnyElement, ClickEvent, Context, DragMoveEvent, IntoElement,
    MouseButton, MouseUpEvent, Render, Window, div, px,
};
use kandrium_core::{Board, BoardWithColumns, Card, Column};

use crate::anim::{LIFT, REFLOW, ease_standard, eased};
use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens, label_color};

/// Width of one board column, matching the egui app.
const COLUMN_W: f32 = 280.0;
/// Height of the always-reserved slot that holds the drop indicator, so
/// showing the indicator never shifts the layout.
const SLOT_H: f32 = 7.0;

/// Payload carried by gpui's drag session for one card.
#[derive(Clone)]
pub struct CardDrag {
    pub card_id: String,
    pub title: String,
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
#[derive(Clone, Copy)]
struct DragStyle<'a> {
    target: Option<&'a DropTarget>,
    dragging: Option<&'a str>,
    tokens: &'a Tokens,
    reduced: bool,
}

/// The tab list: the visible (non-project) boards, plus the open board when it
/// is not among them — the active-project-board rule.
pub fn task_tabs(boards: &[Board], active: Option<&Board>) -> Vec<Board> {
    let mut tabs = boards.to_vec();
    if let Some(active) = active
        && !tabs.iter().any(|board| board.id == active.id)
    {
        tabs.push(active.clone());
    }
    tabs
}

/// The board tab strip.
pub fn tabs_strip(
    tabs: &[Board],
    active_id: Option<&str>,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> impl IntoElement {
    div()
        .id("board-tabs")
        .test_support()
        .w_full()
        .px(px(16.))
        .pt(px(10.))
        .flex()
        .items_center()
        .gap(px(6.))
        .children(tabs.iter().map(|board| {
            let selected = active_id == Some(board.id.as_str());
            let id = board.id.clone();
            div()
                .id(format!("board-tab-{}", board.id))
                .test_support()
                .h(px(28.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(t.radius))
                .cursor_pointer()
                .text(Text::Button)
                .text_color(if selected { t.text } else { t.text_muted })
                .border_1()
                .border_color(if selected { t.accent } else { t.border })
                .when(selected, |el| el.bg(t.selected))
                .child(board.title.clone())
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.open_board(&id, cx);
                }))
        }))
}

/// The board surface: header, columns and the drop indicators.
pub fn board_view(
    board: &BoardWithColumns,
    target: Option<&DropTarget>,
    dragging: Option<&str>,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> impl IntoElement {
    let reduced = cx.reduce_motion();
    let style = DragStyle {
        target,
        dragging,
        tokens: t,
        reduced,
    };
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
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _window, cx| this.clear_drop_target(cx)),
                )
                .children(
                    board
                        .columns
                        .iter()
                        .map(|column| column_view(column, style, cx)),
                ),
        )
}

fn header(board: &BoardWithColumns, t: &Tokens) -> AnyElement {
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
                .child(format!("{cards} cards")),
        )
        .child(div().flex_1())
        .child(
            div()
                .text(Text::Small)
                .text_color(t.text_faint)
                .child("Drag a card to reorder"),
        )
        .into_any_element()
}

fn column_view(column: &Column, style: DragStyle<'_>, cx: &mut Context<AppShell>) -> AnyElement {
    let cards = column.ordered_cards();
    let column_id = column.id.clone();
    let after: Vec<Option<String>> = (0..cards.len())
        .map(|index| cards.get(index + 1).map(|card| card.id.clone()))
        .collect();
    let t = style.tokens;
    let reduced = style.reduced;

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
                .px(px(8.))
                .pb(px(8.))
                .overflow_y_scroll()
                // Ancestor of every card: runs first on capture, so a card's
                // own `on_drag_move` overrides this "append" target. Every
                // registered listener receives every move, so the bounds check
                // is what limits this to the column under the pointer.
                .on_drag_move::<CardDrag>({
                    let column_id = column_id.clone();
                    cx.listener(move |this, event: &DragMoveEvent<CardDrag>, _window, cx| {
                        if !event.bounds.contains(&event.event.position) {
                            return;
                        }
                        this.set_drop_target(&column_id, None, cx);
                    })
                })
                .on_drop::<CardDrag>({
                    let column_id = column_id.clone();
                    cx.listener(move |this, drag: &CardDrag, _window, cx| {
                        this.drop_card(&drag.card_id, &column_id, cx);
                    })
                })
                .children(cards.iter().enumerate().map(|(index, card)| {
                    card_view(column, card, after[index].as_deref(), style, cx)
                }))
                .child(append_slot(style.target, &column_id, t, reduced)),
        )
        .into_any_element()
}

fn card_view(
    column: &Column,
    card: &Card,
    after: Option<&str>,
    style: DragStyle<'_>,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let t = style.tokens;
    let reduced = style.reduced;
    let target = style.target;
    let is_dragged = style.dragging == Some(card.id.as_str());
    let wants_before = target.is_some_and(|target| {
        target.column_id == column.id && target.before.as_deref() == Some(card.id.as_str())
    });
    let generation = target.map(|target| target.generation).unwrap_or(0);
    let card_id = card.id.clone();
    let column_id = column.id.clone();
    let after = after.map(str::to_string);
    let tokens = *t;
    let shell = cx.entity().downgrade();

    div()
        .id(format!("board-card-{}", card.id))
        .test_support()
        .w_full()
        .flex()
        .flex_col()
        .cursor_pointer()
        .when(is_dragged, |el| el.opacity(0.4))
        .child(indicator_slot(
            wants_before,
            format!("drop-{}-{}", card.id, generation),
            t,
            reduced,
        ))
        .child(card_body(card, t))
        .on_click({
            let card_id = card_id.clone();
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_card(&card_id, window, cx)
            })
        })
        .on_drag(
            CardDrag {
                card_id: card_id.clone(),
                title: card.title.clone(),
            },
            move |drag: &CardDrag, _offset, _window, cx| {
                // A drag only begins after the pointer passes the threshold, so
                // recording it here keeps a plain click from dimming the card.
                let card_id = drag.card_id.clone();
                if let Some(shell) = shell.upgrade() {
                    shell.update(cx, |this, cx| this.begin_card_drag(&card_id, cx));
                }
                cx.new(|_| CardGhost::new(drag.title.clone(), tokens))
            },
        )
        .on_drag_move::<CardDrag>({
            let card_id = card_id.clone();
            let column_id = column_id.clone();
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
            cx.listener(move |this, drag: &CardDrag, _window, cx| {
                this.drop_card(&drag.card_id, &column_id, cx);
            })
        })
        .into_any_element()
}

/// One always-reserved slot that holds the animated drop indicator.
fn indicator_slot(visible: bool, id: String, t: &Tokens, reduced: bool) -> AnyElement {
    div()
        .h(px(SLOT_H))
        .w_full()
        .flex_shrink_0()
        .flex()
        .items_center()
        .child(
            div()
                .h(px(if visible { 2.0 } else { 0.0 }))
                .w_full()
                .rounded_full()
                .bg(t.accent)
                .with_animation(
                    id,
                    Animation::new(REFLOW).with_easing(ease_standard),
                    move |el, delta| el.h(px(2.0 * eased(reduced, delta, ease_standard))),
                ),
        )
        .into_any_element()
}

fn append_slot(
    target: Option<&DropTarget>,
    column_id: &str,
    t: &Tokens,
    reduced: bool,
) -> AnyElement {
    let visible =
        target.is_some_and(|target| target.column_id == column_id && target.before.is_none());
    let generation = target.map(|target| target.generation).unwrap_or(0);
    indicator_slot(
        visible,
        format!("drop-append-{}-{}", column_id, generation),
        t,
        reduced,
    )
}

fn card_body(card: &Card, t: &Tokens) -> AnyElement {
    div()
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
        .into_any_element()
}

/// The floating copy of the card that follows the pointer.
struct CardGhost {
    title: String,
    tokens: Tokens,
}

impl CardGhost {
    fn new(title: String, tokens: Tokens) -> Self {
        Self { title, tokens }
    }
}

impl Render for CardGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.tokens;
        let reduced = cx.reduce_motion();
        div()
            .w(px(COLUMN_W - 16.))
            .flex()
            .flex_col()
            .rounded(px(t.radius))
            .bg(t.card)
            .border_1()
            .border_color(t.accent)
            .p(px(10.))
            .with_animation(
                "card-ghost",
                Animation::new(LIFT).with_easing(ease_standard),
                move |el, delta| el.opacity(0.35 + 0.62 * eased(reduced, delta, ease_standard)),
            )
            .child(
                div()
                    .text(Text::Button)
                    .text_color(t.text)
                    .child(self.title.clone()),
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

    #[test]
    fn tabs_append_the_active_board_when_it_is_not_listed() {
        let listed = vec![board("a", "A"), board("b", "B")];
        let tabs = task_tabs(&listed, Some(&board("project", "P")));
        let ids: Vec<_> = tabs.iter().map(|board| board.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "project"]);
    }

    #[test]
    fn tabs_keep_a_listed_active_board_in_place() {
        let listed = vec![board("a", "A"), board("b", "B")];
        let tabs = task_tabs(&listed, Some(&board("b", "B")));
        let ids: Vec<_> = tabs.iter().map(|board| board.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }
}
