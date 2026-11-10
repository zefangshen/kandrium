//! Card editor modal.
//!
//! Mirrors the egui modal for the fields core supports: title, a markdown
//! description with edit/preview, the project picker on normal cards or the
//! area-board picker on project cards, labels and a due date. The description
//! is a plain textarea for now; vim is a later stage and is deliberately not
//! half-built here.
//!
//! The overlay is a plain absolutely-positioned element inside the app root
//! rather than `Dialog`: the shell already owns the open/closed state, so a
//! backdrop click and the Cancel button are enough to close it.

use gpui_kit::TestSupportExt;
use gpui_kit::base::text::markdown;
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, IntoElement, MouseButton, MouseDownEvent, Window,
    div, px, rgba,
};

use kandrium_core::{Board, ProjectChoice};

use crate::app::AppShell;
use crate::theme::{self, Text, TextStyled, Tokens};

/// One open card editor.
pub struct EditSession {
    pub card_id: String,
    /// The card's home board is a project board (hidden from the Tasks tabs).
    pub is_project_card: bool,
    pub title: Entity<InputState>,
    pub description: Entity<TextareaState>,
    pub due: Entity<InputState>,
    pub labels: Vec<i64>,
    pub project_id: Option<String>,
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
        .max_h(px(640.))
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
                .h(px(34.))
                .flex()
                .items_center()
                .rounded(px(t.radius))
                .bg(t.field)
                .border_1()
                .border_color(t.border)
                .px(px(8.))
                .child(div().flex_1().child(Input::new(&session.title))),
        );

    // Description: edit/preview toggle, then the editor or the markdown.
    let preview = session.preview;
    let description_text = session.description.read(cx).value().to_string();
    panel = panel.child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(field_label("Description", t))
            .child(
                div()
                    .id("card-description-toggle")
                    .test_support()
                    .cursor_pointer()
                    .rounded(px(4.))
                    .px(px(8.))
                    .py(px(2.))
                    .text(Text::SmallStrong)
                    .text_color(if preview { t.accent } else { t.text_muted })
                    .hover(|style| style.bg(t.hover))
                    .child(if preview { "Edit" } else { "Preview" })
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.toggle_editing_preview(cx)
                    })),
            ),
    );
    panel = panel.child(
        div()
            .id("card-description-field")
            .test_support()
            .w_full()
            .h(px(190.))
            .flex()
            .flex_col()
            .rounded(px(t.radius))
            .bg(t.field)
            .border_1()
            .border_color(t.border)
            .p(px(8.))
            .overflow_hidden()
            .when(preview, |this| {
                this.child(
                    div()
                        .id("card-description-preview")
                        .test_support()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .text(Text::Body)
                        .text_color(t.text)
                        .child(markdown(description_text.as_str())),
                )
            })
            .when(!preview, |this| {
                this.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(Textarea::new(&session.description)),
                )
            }),
    );

    // Project picker (normal cards) or area-board picker (project cards).
    if session.is_project_card {
        panel = panel
            .child(field_label("Show on Tasks boards", t))
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
            ));
    } else {
        panel = panel.child(field_label("Project", t)).child(choice_row(
            "card-project",
            ctx.projects
                .iter()
                .map(|project| {
                    (
                        project.id.clone(),
                        format!("{} — {}", project.identifier, project.name),
                    )
                })
                .collect(),
            session.project_id.as_deref(),
            t,
            AppShell::set_editing_project,
            cx,
        ));
    }

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
    panel = panel
        .child(field_label("Labels", t))
        .child(labels)
        .child(field_label("Due date", t))
        .child(
            div()
                .id("card-due-field")
                .test_support()
                .w(px(180.))
                .h(px(34.))
                .flex()
                .items_center()
                .rounded(px(t.radius))
                .bg(t.field)
                .border_1()
                .border_color(t.border)
                .px(px(8.))
                .child(div().flex_1().child(Input::new(&session.due))),
        );

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
        .into_any_element()
}

fn field_label(label: &'static str, t: &Tokens) -> impl IntoElement {
    div()
        .text(Text::SmallStrong)
        .text_color(t.text_muted)
        .child(label)
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
