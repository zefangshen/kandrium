//! Card editor modal.
//!
//! Mirrors the egui modal for the fields core supports: title, a markdown
//! description with edit/preview, the project picker on normal cards or the
//! area-board picker on project cards, labels and a due date. The description
//! is vim-modal: keys are captured by the panel and applied by
//! [`crate::vim`] before the textarea sees them (see the module docs there).
//!
//! The overlay is a plain absolutely-positioned element inside the app root
//! rather than `Dialog`: the shell already owns the open/closed state, so a
//! backdrop click, Escape and the Cancel button are enough to close it. The
//! panel is registered as a focus trap so Tab cannot move focus to the board
//! behind the editor.

use gpui_kit::TestSupportExt;
use gpui_kit::base::FocusTrapElement;
use gpui_kit::base::text::markdown;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, Context, Entity, FocusHandle, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, Pixels, Size, Window, canvas, div, px, rgba,
};

use std::cell::RefCell;
use std::rc::Rc;

use kandrium_core::{Board, ProjectChoice};

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::theme::{self, Text, TextStyled, Tokens};
use crate::vim::VimSession;

/// One open card editor.
pub struct EditSession {
    pub card_id: String,
    /// The card's home board is a project board (hidden from the Tasks tabs).
    pub is_project_card: bool,
    /// Board the card lives on right now; excluded from "Move to board".
    pub home_board_id: String,
    pub title: Entity<InputState>,
    pub description: Entity<TextareaState>,
    pub due: Entity<InputState>,
    /// The due-date calendar is open, and the month it displays (first day).
    pub due_menu: bool,
    pub due_month: jiff::civil::Date,
    pub due_anchor: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// Viewport measured with the due row; the calendar clamps against its
    /// right edge and the outside-click strips stop at the row.
    pub due_viewport: Rc<RefCell<Option<Size<Pixels>>>>,
    /// Focus trap handle for the panel; keeps Tab inside the editor.
    pub focus: FocusHandle,
    /// Modal state of the description field.
    pub vim: VimSession,
    pub labels: Vec<i64>,
    pub project_id: Option<String>,
    /// The project dropdown is open. Its menu is an anchored overlay, so the
    /// fields below never move.
    pub project_menu: bool,
    /// Painted bounds of the project picker, written in prepaint; the menu
    /// anchors to it.
    pub project_anchor: Rc<RefCell<Option<Bounds<Pixels>>>>,
    pub area_board_id: Option<String>,
    pub preview: bool,
    pub error: Option<String>,
}

/// Data the modal renders besides the session itself.
pub struct ModalContext<'a> {
    pub session: &'a EditSession,
    pub area_boards: &'a [Board],
    pub projects: &'a [ProjectChoice],
    pub tokens: &'a Tokens,
}

/// The dialog overlay for the open session.
pub fn modal(ctx: ModalContext<'_>, cx: &mut Context<AppShell>) -> AnyElement {
    let t = ctx.tokens;
    let session = ctx.session;

    let mut panel = div()
        .id("card-modal")
        .test_support()
        .w(px(560.))
        .max_h(px(720.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(12.))
        .rounded(px(t.radius))
        .bg(t.card)
        .border_1()
        .border_color(t.border)
        .p(px(16.))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text(Text::Heading)
                        .text_color(t.text)
                        .child("Edit card"),
                )
                .child(
                    div()
                        .id("card-modal-close")
                        .test_support()
                        .cursor_pointer()
                        .rounded(px(4.))
                        .px(px(8.))
                        .py(px(2.))
                        .text(Text::Body)
                        .text_color(t.text_muted)
                        .hover(|style| style.bg(t.hover))
                        .child("×")
                        .on_click(
                            cx.listener(|this, _: &ClickEvent, _window, cx| this.close_card(cx)),
                        ),
                ),
        )
        .child(field_label("Title", t))
        .child(
            div()
                .id("card-title-field")
                .test_support()
                .w_full()
                .child(crate::views::field_input_tall(&session.title, t)),
        );

    // Project first, matching the egui modal: a project card shows the owning
    // project read-only plus its mirror board, a normal card tags a project.
    if session.is_project_card {
        let project_label = session
            .project_id
            .as_ref()
            .and_then(|id| ctx.projects.iter().find(|p| &p.id == id))
            .map(|p| format!("{} — {}", p.identifier, p.name))
            .unwrap_or_else(|| "—".to_string());
        panel = panel
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(field_label("Project", t))
                    .child(
                        div()
                            .id("card-modal-project")
                            .test_support()
                            .text(Text::Small)
                            .text_color(t.text_muted)
                            .child(project_label),
                    ),
            )
            .child(field_label("Show on Tasks boards", t));
        if ctx.area_boards.is_empty() {
            panel = panel.child(hint_line(
                "card-area-empty",
                "No Tasks boards available.",
                t,
            ));
        } else {
            panel = panel
                .child(choice_row(
                    "card-area",
                    ctx.area_boards
                        .iter()
                        .map(|board| (board.id.clone(), board.title.clone()))
                        .collect(),
                    session.area_board_id.as_deref(),
                    t,
                    AppShell::set_editing_area,
                    cx,
                ))
                .child(hint_line(
                    "card-area-hint",
                    if session.area_board_id.is_some() {
                        "Mirrors this card onto the selected board."
                    } else {
                        "Not mirrored onto a Tasks board; hidden from All."
                    },
                    t,
                ));
        }
    } else {
        let project_label = session
            .project_id
            .as_ref()
            .and_then(|id| ctx.projects.iter().find(|p| &p.id == id))
            .map(|p| format!("{} — {}", p.identifier, p.name))
            .unwrap_or_else(|| "No project".to_string());
        panel = panel.child(field_label("Project", t));
        if ctx.projects.is_empty() {
            panel = panel.child(
                div()
                    .id("card-project-empty")
                    .test_support()
                    .h(px(30.))
                    .w_full()
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .bg(t.field)
                    .border_1()
                    .border_color(t.border)
                    .text(Text::Body)
                    .text_color(t.text_faint)
                    .child("No projects yet"),
            );
        } else {
            let anchor = session.project_anchor.clone();
            let open = session.project_menu;
            let has_project = session.project_id.is_some();
            panel = panel.child(
                div()
                    .id("card-project-picker")
                    .test_support()
                    .relative()
                    .h(px(30.))
                    .w_full()
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.))
                    .rounded(px(5.))
                    .bg(t.field)
                    .border_1()
                    .border_color(if open { t.accent } else { t.border })
                    .cursor_pointer()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text(Text::Body)
                            .text_color(if has_project { t.text } else { t.text_faint })
                            .child(project_label),
                    )
                    .child(icon(Icon::ChevronDown, 12., t.text_muted))
                    .child(
                        canvas(
                            move |bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut App| {
                                *anchor.borrow_mut() = Some(bounds);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.toggle_editing_project_menu(cx)
                    })),
            );
        }
        panel = panel.child(hint_line(
            "card-project-hint",
            "Shows the project identifier on this card.",
            t,
        ));
    }

    // Description: edit/preview toggle, then the editor or the markdown.
    let preview = session.preview;
    let description_text = session.description.read(cx).value().to_string();
    panel = panel.child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(field_label("Description", t))
            .child(crate::views::mode_switch(
                "card-description-toggle",
                preview,
                t,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    if preview {
                        this.toggle_editing_preview(cx);
                    }
                }),
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    if !preview {
                        this.toggle_editing_preview(cx);
                    }
                }),
            )),
    );
    // The textarea owns the field box in edit mode, so the wrapper only sizes
    // the row and redirects clicks outside the text to the field. The vim
    // strip keeps its slot in preview mode so toggling never resizes the modal.
    let focus_description = session.description.clone();
    let (body_family, body_size) = {
        let theme = cx.theme();
        (theme.font_family.clone(), theme.font_size * 0.875)
    };
    let description_body = div()
        .id("card-description-field")
        .test_support()
        .relative()
        .w_full()
        .h(px(188.))
        .flex()
        .flex_col()
        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
            focus_description.update(cx, |state, cx| state.focus(window, cx));
        })
        .when(preview, |this| {
            this.child(
                div()
                    .id("card-description-preview")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .rounded(px(t.radius))
                    .bg(t.field)
                    .border_1()
                    .border_color(t.border)
                    .p(px(8.))
                    .overflow_y_scroll()
                    .text(Text::Body)
                    .text_color(t.text)
                    .when(description_text.trim().is_empty(), |this| {
                        this.child(
                            div()
                                .text(Text::Small)
                                .text_color(t.text_faint)
                                .child("Nothing to preview."),
                        )
                    })
                    .when(!description_text.trim().is_empty(), |this| {
                        this.child(markdown(description_text.as_str()))
                    }),
            )
        })
        .when(!preview, |this| {
            this.child(
                crate::views::field_textarea(&session.description, t)
                    .readonly(!session.vim.is_insert())
                    .h(px(188.)),
            )
            .when(session.vim.block_cursor(), |this| {
                this.child(
                    crate::vim::block_cursor_overlay(
                        session.description.clone(),
                        body_family,
                        body_size,
                        t.accent.into(),
                        t.field.into(),
                    )
                    .absolute()
                    .inset_0(),
                )
            })
        });
    panel = panel.child(
        div()
            .id("card-description-block")
            .test_support()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .flex_shrink_0()
            .child(description_body)
            .when(preview, |this| this.child(div().h(px(20.))))
            .when(!preview, |this| this.child(mode_strip(session, t, cx))),
    );

    // Due date: the field stays typable; clicking anywhere on the row (or the
    // calendar button) opens the month grid, which is an anchored overlay so
    // the fields below never move.
    let due_anchor = session.due_anchor.clone();
    let due_viewport = session.due_viewport.clone();
    let due_open = session.due_menu;
    // The field hugs its content: the placeholder width at rest, growing with
    // a typed value up to a cap, so it never stretches across the row.
    let due_width = due_input_width(&session.due.read(cx).value());
    panel = panel.child(field_label("Due date", t)).child(
        div()
            .id("card-due-picker")
            .test_support()
            .relative()
            .w(due_width + px(25.)) // gap 4 + calendar button 21
            .flex()
            .items_center()
            .gap(px(4.))
            .rounded(px(5.))
            .child(
                div()
                    .w(due_width)
                    .child(crate::views::field_input_tall(&session.due, t)),
            )
            .child(
                div()
                    .id("card-due-calendar")
                    .test_support()
                    .cursor_pointer()
                    .rounded(px(4.))
                    .p(px(4.))
                    .hover(|style| style.bg(t.hover))
                    .when(due_open, |this| this.bg(t.hover))
                    .child(icon(
                        Icon::Calendar,
                        13.,
                        if due_open { t.accent } else { t.text_muted },
                    )),
            )
            .child(
                canvas(
                    move |bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App| {
                        *due_anchor.borrow_mut() = Some(bounds);
                        *due_viewport.borrow_mut() = Some(window.viewport_size());
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                    this.toggle_editing_due_menu(cx)
                }),
            ),
    );

    // Labels: the same eight-color palette the egui modal uses.
    let mut labels = div().id("card-labels").flex().items_center().gap(px(6.));
    for index in 0..theme::LABEL_COLORS.len() as i64 {
        let selected = session.labels.contains(&index);
        let color = theme::label_color(index).unwrap_or(t.text_muted);
        labels = labels.child(
            div()
                .id(format!("card-label-{index}"))
                .test_support()
                .w(px(26.))
                .h(px(16.))
                .rounded(px(4.))
                .bg(color)
                .cursor_pointer()
                .border_1()
                .border_color(if selected { t.text } else { color })
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.toggle_editing_label(index, cx)
                })),
        );
    }
    panel = panel.child(field_label("Labels", t)).child(labels);

    // Move to board: only for cards that live on a Tasks board, and never to
    // the board they are already on.
    if !session.is_project_card {
        let targets: Vec<&Board> = ctx
            .area_boards
            .iter()
            .filter(|board| board.id != session.home_board_id)
            .collect();
        if !targets.is_empty() {
            let mut row = div()
                .id("card-move")
                .test_support()
                .flex()
                .flex_wrap()
                .gap(px(6.));
            for board in targets {
                let target = board.id.clone();
                row = row.child(choice_chip(
                    format!("card-move-{}", board.id),
                    board.title.clone(),
                    false,
                    t,
                    cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.move_editing_card(target.clone(), cx)
                    }),
                ));
            }
            panel = panel.child(field_label("Move to board", t)).child(row);
        }
    }

    if let Some(error) = &session.error {
        panel = panel.child(
            div()
                .id("card-modal-error")
                .text(Text::Small)
                .text_color(t.danger)
                .child(error.clone()),
        );
    }

    panel = panel.child(
        div()
            .flex()
            .justify_end()
            .gap(px(8.))
            .child(action_button(
                "card-modal-cancel",
                "Cancel",
                false,
                t,
                cx.listener(|this, _: &ClickEvent, _window, cx| this.close_card(cx)),
            ))
            .child(action_button(
                "card-modal-save",
                "Save",
                true,
                t,
                cx.listener(|this, _: &ClickEvent, _window, cx| this.save_editing(cx)),
            )),
    );

    // The capture listener runs before the description's own key handling, so
    // vim can consume Normal/Visual keys and `Esc` never edits the text.
    let panel = panel
        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            this.handle_card_key(event, window, cx)
        }))
        .focus_trap("card-modal", &session.focus);

    div()
        .id("card-modal-backdrop")
        .test_support()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x00000099))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, _window, cx| this.close_card(cx)),
        )
        .child(panel)
        .when(session.project_menu, |this| {
            this.child(project_menu_layer(session, ctx.projects, t, cx))
        })
        .when(session.due_menu, |this| {
            this.children(due_menu_layer(session, t, cx))
        })
        .into_any_element()
}

/// The due-date calendar: a month grid to the right of the due row's calendar
/// icon, top-aligned with the field. Picking a day writes `YYYY-MM-DD` into
/// the field and persists through the card update path; the arrows and Today
/// move the displayed month, Clear empties it.
fn due_menu_layer(
    session: &EditSession,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> Vec<AnyElement> {
    let Some(anchor) = *session.due_anchor.borrow() else {
        return Vec::new();
    };
    let today = jiff::Zoned::now().date();
    let selected = session
        .due
        .read(cx)
        .value()
        .trim()
        .parse::<jiff::civil::Date>()
        .ok();
    let month = session.due_month.first_of_month();
    let previous = shift_month(month, -1);
    let next = shift_month(month, 1);

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(6.))
        .child(month_arrow(
            "card-due-prev",
            Icon::ChevronLeft,
            previous,
            t,
            cx,
        ))
        .child(
            div()
                .text(Text::SmallStrong)
                .text_color(t.text)
                .child(month.strftime("%B %Y").to_string()),
        )
        .child(month_arrow(
            "card-due-next",
            Icon::ChevronRight,
            next,
            t,
            cx,
        ));

    let mut grid = div().flex().flex_col().gap(px(2.));
    let mut week_header = div().flex().gap(px(2.));
    for label in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
        week_header = week_header.child(
            div()
                .w(px(36.))
                .h(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .text(Text::Small)
                .text_color(t.text_faint)
                .child(label),
        );
    }
    grid = grid.child(week_header);

    let offset = month.weekday().to_monday_one_offset() as i64 - 1;
    let mut date = month;
    for _ in 0..offset {
        date = date.yesterday().unwrap_or(date);
    }
    for _ in 0..6 {
        let mut row = div().flex().gap(px(2.));
        for _ in 0..7 {
            row = row.child(due_day_cell(date, month, today, selected, t, cx));
            date = date.tomorrow().unwrap_or(date);
        }
        grid = grid.child(row);
    }

    let actions = div()
        .flex()
        .items_center()
        .justify_between()
        .pt(px(4.))
        .child(text_action(
            "card-due-clear",
            "Clear",
            t,
            cx.listener(|this, _: &ClickEvent, window, cx| this.clear_editing_due(window, cx)),
        ))
        .child(text_action(
            "card-due-today",
            "Today",
            t,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.set_editing_due_month(today, cx)
            }),
        ));

    let viewport = session
        .due_viewport
        .borrow()
        .map(|size| size.width)
        .unwrap_or(px(1280.));
    let place = due_menu_placement(anchor, viewport);
    let panel = div()
        .id("card-due-menu")
        .test_support()
        .absolute()
        .left(place.left)
        .top(place.top)
        .w(place.width)
        .p(px(8.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .rounded(px(t.radius))
        .bg(t.card)
        .border_1()
        .border_color(t.border)
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(header)
        .child(grid)
        .child(actions);

    // Outside-click strips: four absolute rects around the due row instead of
    // one full-window catcher, so the row itself (date text and calendar icon)
    // keeps receiving clicks while the popup is open and acts as a toggle.
    let (viewport_w, viewport_h) = session
        .due_viewport
        .borrow()
        .map(|size| (size.width, size.height))
        .unwrap_or((px(1280.), px(800.)));
    let clamp = |value: Pixels| if value < px(0.) { px(0.) } else { value };
    let row_right = anchor.origin.x + anchor.size.width;
    let row_bottom = anchor.origin.y + anchor.size.height;
    let strips: [(&'static str, Pixels, Pixels, Pixels, Pixels); 4] = [
        (
            "card-due-catcher-top",
            px(0.),
            px(0.),
            viewport_w,
            anchor.origin.y,
        ),
        (
            "card-due-catcher-bottom",
            px(0.),
            row_bottom,
            viewport_w,
            clamp(viewport_h - row_bottom),
        ),
        (
            "card-due-catcher-left",
            px(0.),
            anchor.origin.y,
            clamp(anchor.origin.x),
            anchor.size.height,
        ),
        (
            "card-due-catcher-right",
            row_right,
            anchor.origin.y,
            clamp(viewport_w - row_right),
            anchor.size.height,
        ),
    ];

    let mut layer: Vec<AnyElement> = strips
        .into_iter()
        .map(|(id, left, top, width, height)| {
            div()
                .id(id)
                .test_support()
                .absolute()
                .left(left)
                .top(top)
                .w(width)
                .h(height)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                        this.close_editing_menu(cx)
                    }),
                )
                .into_any_element()
        })
        .collect();
    layer.push(panel.into_any_element());
    layer
}

/// The due field's width: the placeholder's width at rest, widened by the
/// typed value (roughly 7.8 px per character at Body size) and capped so a
/// rogue value cannot stretch the row across the modal.
fn due_input_width(value: &str) -> Pixels {
    const PLACEHOLDER_CHARS: usize = 10; // `YYYY-MM-DD`
    const CHAR_W: f32 = 7.8;
    const INSET: f32 = 22.; // 8 px inset per side plus caret breathing room
    const CAP: f32 = 150.;
    let chars = value.chars().count().max(PLACEHOLDER_CHARS) as f32;
    px((chars * CHAR_W + INSET).min(CAP))
}

/// Where the calendar goes: right of the due row's icon, top-aligned with the
/// field. `left` is clamped so the panel never crosses the right window edge;
/// when the window is narrower than the panel plus its gutters, `left` falls
/// back to the 8 px gutter so the calendar stays fully visible instead of
/// clipping.
struct DueMenuPlacement {
    left: Pixels,
    top: Pixels,
    width: Pixels,
}

fn due_menu_placement(anchor: Bounds<Pixels>, viewport: Pixels) -> DueMenuPlacement {
    const WIDTH: f32 = 268.;
    const GAP: f32 = 6.;
    const GUTTER: f32 = 8.;
    let width = px(WIDTH);
    let right_of_icon = anchor.right() + px(GAP);
    let max_left = viewport - width - px(GUTTER);
    DueMenuPlacement {
        left: right_of_icon.min(max_left).max(px(GUTTER)),
        top: anchor.origin.y,
        width,
    }
}

/// First day of the month `delta` months away from `month`.
fn shift_month(month: jiff::civil::Date, delta: i32) -> jiff::civil::Date {
    let mut year = month.year() as i32;
    let mut index = month.month() as i32 + delta;
    while index < 1 {
        index += 12;
        year -= 1;
    }
    while index > 12 {
        index -= 12;
        year += 1;
    }
    jiff::civil::Date::new(year as i16, index as i8, 1).unwrap_or(month)
}

fn month_arrow(
    id: &'static str,
    glyph: Icon,
    target: jiff::civil::Date,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .w(px(22.))
        .h(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .cursor_pointer()
        .hover(|style| style.bg(t.hover))
        .child(icon(glyph, 12., t.text_muted))
        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
            this.set_editing_due_month(target, cx)
        }))
        .into_any_element()
}

fn text_action(
    id: &'static str,
    label: &'static str,
    t: &Tokens,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .px(px(6.))
        .py(px(2.))
        .rounded(px(4.))
        .cursor_pointer()
        .text(Text::Small)
        .text_color(t.text_muted)
        .hover(|style| style.bg(t.hover))
        .child(label)
        .on_click(on_click)
        .into_any_element()
}

fn due_day_cell(
    date: jiff::civil::Date,
    month: jiff::civil::Date,
    today: jiff::civil::Date,
    selected: Option<jiff::civil::Date>,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let in_month = date.year() == month.year() && date.month() == month.month();
    let is_today = date == today;
    let is_selected = selected == Some(date);
    let color = if is_selected {
        t.accent_text
    } else if in_month {
        t.text
    } else {
        t.text_faint
    };
    div()
        .id(format!("card-due-day-{date}"))
        .test_support()
        .w(px(36.))
        .h(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .text(Text::Small)
        .text_color(color)
        .cursor_pointer()
        .when(is_selected, |this| this.bg(t.accent))
        .when(is_today && !is_selected, |this| {
            this.border_1().border_color(t.accent)
        })
        .hover(|style| style.bg(t.hover))
        .child(date.day().to_string())
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.pick_editing_due(date, window, cx)
        }))
        .into_any_element()
}

/// The project picker's menu, anchored to the picker's painted bounds and
/// painted above the panel so opening it never moves the fields below.
///
/// Outside-click handling uses four strips around the picker + menu union
/// instead of one full-window catcher, so the picker itself stays clickable
/// while the menu is open and acts as a toggle.
fn project_menu_layer(
    session: &EditSession,
    projects: &[ProjectChoice],
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let Some(anchor) = *session.project_anchor.borrow() else {
        return div().into_any_element();
    };
    let selected = session.project_id.clone();
    let menu_top = anchor.origin.y + anchor.size.height + px(4.);
    // Upper bound for the menu's painted height: padding, one row per option
    // ("No project" plus every project), and the 1 px gaps between rows.
    let rows = projects.len() as f32 + 1.;
    let menu_height = (px(8.) + px(27.) * rows).min(px(240.));
    let mut menu = div()
        .id("card-project-menu")
        .test_support()
        .absolute()
        .left(anchor.origin.x)
        .top(menu_top)
        .w(anchor.size.width)
        .max_h(px(240.))
        .p(px(4.))
        .flex()
        .flex_col()
        .gap(px(1.))
        .rounded(px(t.radius))
        .bg(t.card)
        .border_1()
        .border_color(t.border)
        .overflow_y_scroll()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    menu = menu.child(project_option(
        "card-project-option-none".to_string(),
        "No project".to_string(),
        selected.is_none(),
        t,
        cx.listener(|this, _: &ClickEvent, _window, cx| {
            this.set_editing_project(String::new(), cx)
        }),
    ));
    for project in projects {
        let id = project.id.clone();
        menu = menu.child(project_option(
            format!("card-project-option-{}", project.id),
            format!("{} — {}", project.identifier, project.name),
            selected.as_deref() == Some(project.id.as_str()),
            t,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.set_editing_project(id.clone(), cx)
            }),
        ));
    }

    let (viewport_w, viewport_h) = session
        .due_viewport
        .borrow()
        .map(|size| (size.width, size.height))
        .unwrap_or((px(1280.), px(800.)));
    let clamp = |value: Pixels| if value < px(0.) { px(0.) } else { value };
    let union_right = anchor.origin.x + anchor.size.width;
    let union_bottom = (menu_top + menu_height).min(viewport_h);
    let band_top = anchor.origin.y;
    let band_height = clamp(union_bottom - band_top);
    let strips: [(&'static str, Pixels, Pixels, Pixels, Pixels); 4] = [
        (
            "card-project-catcher-top",
            px(0.),
            px(0.),
            viewport_w,
            clamp(anchor.origin.y),
        ),
        (
            "card-project-catcher-bottom",
            px(0.),
            union_bottom,
            viewport_w,
            clamp(viewport_h - union_bottom),
        ),
        (
            "card-project-catcher-left",
            px(0.),
            band_top,
            clamp(anchor.origin.x),
            band_height,
        ),
        (
            "card-project-catcher-right",
            union_right,
            band_top,
            clamp(viewport_w - union_right),
            band_height,
        ),
    ];
    let mut layer: Vec<AnyElement> = strips
        .into_iter()
        .map(|(id, left, top, width, height)| {
            div()
                .id(id)
                .test_support()
                .absolute()
                .left(left)
                .top(top)
                .w(width)
                .h(height)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                        this.close_editing_menu(cx)
                    }),
                )
                .into_any_element()
        })
        .collect();
    layer.push(menu.into_any_element());
    div()
        .id("card-project-menu-layer")
        .absolute()
        .inset_0()
        .children(layer)
        .into_any_element()
}

/// One row of the project dropdown: accent dot, label, selected tint.
fn project_option(
    id: String,
    label: String,
    selected: bool,
    t: &Tokens,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .h(px(26.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(4.))
        .cursor_pointer()
        .hover(|style| style.bg(t.hover))
        .child(div().w(px(6.)).h(px(14.)).rounded_full().bg(if selected {
            t.accent
        } else {
            t.titlebar
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text(Text::Small)
                .text_color(if selected { t.text } else { t.text_muted })
                .child(label),
        )
        .on_click(on_click)
        .into_any_element()
}

fn field_label(label: &'static str, t: &Tokens) -> impl IntoElement {
    div()
        .text(Text::Label)
        .text_color(t.text_muted)
        .child(label)
}

/// Faint one-line explanation under a picker (the egui modal's helper text).
fn hint_line(id: &'static str, line: &'static str, t: &Tokens) -> impl IntoElement {
    div()
        .id(id)
        .test_support()
        .text(Text::Small)
        .text_color(t.text_faint)
        .child(line)
}

/// Mode badge (also the vim on/off toggle), pending keys or the `:` command
/// line, and a short hint.
fn mode_strip(session: &EditSession, t: &Tokens, cx: &mut Context<AppShell>) -> impl IntoElement {
    let vim = &session.vim;
    let enabled = vim.is_enabled();
    let mut row = div()
        .id("card-description-mode")
        .test_support()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .id("card-description-vim-toggle")
                .test_support()
                .px(px(6.))
                .py(px(1.))
                .rounded(px(4.))
                .bg(t.panel)
                .text(Text::SmallStrong)
                .text_color(if enabled { t.text_muted } else { t.text_faint })
                .cursor_pointer()
                .hover(|style| style.bg(t.hover))
                .child(vim.mode_label())
                .on_click(
                    cx.listener(|this, _: &ClickEvent, _window, cx| this.toggle_vim_mode(cx)),
                ),
        );
    if let Some(line) = vim.ex_line() {
        row = row.child(
            div()
                .text(Text::Small)
                .text_color(t.text)
                .child(format!(":{line}")),
        );
    } else if let Some(error) = vim.ex_error() {
        row = row.child(
            div()
                .text(Text::Small)
                .text_color(t.danger)
                .child(error.to_string()),
        );
    } else {
        let pending = vim.pending();
        if !pending.is_empty() {
            row = row.child(
                div()
                    .text(Text::Small)
                    .text_color(t.text_muted)
                    .child(pending),
            );
        }
    }
    row.child(
        div()
            .flex_1()
            .text(Text::Small)
            .text_color(t.text_faint)
            .child(if enabled {
                "i insert · Esc normal · :w save"
            } else {
                "click the badge for vim"
            }),
    )
}

fn action_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    t: &Tokens,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (bg, fg) = if primary {
        (t.accent, t.accent_text)
    } else {
        (t.field, t.text)
    };
    div()
        .id(id)
        .test_support()
        .cursor_pointer()
        .rounded(px(t.radius))
        .bg(bg)
        .px(px(14.))
        .py(px(6.))
        .text(Text::Button)
        .text_color(fg)
        .hover(move |style| style.opacity(0.85))
        .child(label)
        .on_click(on_click)
}

/// A wrapped row of selectable chips plus a "None" chip. The setter receives
/// an empty string for "None".
fn choice_row(
    id: &'static str,
    choices: Vec<(String, String)>,
    selected: Option<&str>,
    t: &Tokens,
    setter: fn(&mut AppShell, String, &mut Context<AppShell>),
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let mut row = div().id(id).test_support().flex().flex_wrap().gap(px(6.));
    row = row.child(choice_chip(
        format!("{id}-none"),
        "None".to_string(),
        selected.is_none(),
        t,
        cx.listener(move |this, _: &ClickEvent, _window, cx| setter(this, String::new(), cx)),
    ));
    for (index, (value, label)) in choices.into_iter().enumerate() {
        let is_selected = selected == Some(value.as_str());
        let chip_value = value.clone();
        row = row.child(choice_chip(
            format!("{id}-{index}"),
            label,
            is_selected,
            t,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                setter(this, chip_value.clone(), cx)
            }),
        ));
    }
    row.into_any_element()
}

fn choice_chip(
    id: String,
    label: String,
    selected: bool,
    t: &Tokens,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .test_support()
        .cursor_pointer()
        .rounded(px(t.radius))
        .px(px(10.))
        .py(px(4.))
        .text(Text::Small)
        .text_color(if selected { t.accent_text } else { t.text })
        .bg(if selected { t.accent } else { t.field })
        .border_1()
        .border_color(if selected { t.accent } else { t.border })
        .hover(|style| style.border_color(t.accent))
        .child(label)
        .on_click(on_click)
}

#[cfg(test)]
mod tests {
    use super::{due_input_width, due_menu_placement};
    use gpui_kit::{Bounds, point, px, size};

    /// The field is placeholder-sized at rest, grows with the value, and caps.
    #[test]
    fn due_field_hugs_its_content() {
        let rest = due_input_width("");
        assert_eq!(rest, px(100.));
        assert_eq!(due_input_width("2026-03-05"), px(100.));
        assert!(due_input_width("2026-03-05 12:00") > rest);
        assert_eq!(due_input_width("x".repeat(80).as_str()), px(150.));
        assert!(rest > px(80.), "placeholder must not be crowded");
    }

    fn due_row() -> Bounds<gpui_kit::Pixels> {
        Bounds {
            origin: point(px(206.), px(500.)),
            size: size(px(200.), px(34.)),
        }
    }

    /// The calendar opens to the right of the icon, top-aligned with the
    /// field, and clamps to the right window edge at narrow widths.
    #[test]
    fn due_calendar_sits_right_of_the_field_and_clamps() {
        let place = due_menu_placement(due_row(), px(940.));
        assert_eq!(place.left, px(412.));
        assert_eq!(place.top, px(500.));
        assert!(place.left >= due_row().right(), "must not cover the field");
        assert!(place.left + place.width <= px(940.));

        let clamped = due_menu_placement(due_row(), px(600.));
        assert_eq!(clamped.left, px(324.));
        assert!(clamped.left + clamped.width <= px(600.));

        // No room at all: fall back to the gutter rather than going negative.
        let tight = due_menu_placement(due_row(), px(280.));
        assert_eq!(tight.left, px(8.));
        assert!(tight.left + tight.width <= px(280.));
    }
}
