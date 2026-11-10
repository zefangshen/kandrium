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
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, Window, div, px, rgba,
};

use kandrium_core::{Board, ProjectChoice};

use crate::app::AppShell;
use crate::theme::{self, Text, TextStyled, Tokens};
use crate::vim::VimSession;

/// One open card editor.
pub struct EditSession {
    pub card_id: String,
    /// The card's home board is a project board (hidden from the Tasks tabs).
    pub is_project_card: bool,
    pub title: Entity<InputState>,
    pub description: Entity<TextareaState>,
    pub due: Entity<InputState>,
    /// Focus trap handle for the panel; keeps Tab inside the editor.
    pub focus: FocusHandle,
    /// Modal state of the description field.
    pub vim: VimSession,
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
    // The textarea only covers the text it renders, so clicking the empty part
    // of the box would otherwise leave focus on the board behind the modal.
    let focus_description = session.description.clone();
    panel =
        panel.child(
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
                            .overflow_y_scroll()
                            .text(Text::Body)
                            .text_color(t.text)
                            .child(markdown(description_text.as_str())),
                    )
                })
                .when(!preview, |this| {
                    this.child(div().flex_1().min_h_0().child(
                        Textarea::new(&session.description).readonly(!session.vim.is_insert()),
                    ))
                }),
        );
    if !preview {
        panel = panel.child(mode_strip(session, t, cx));
    }

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
        .into_any_element()
}

fn field_label(label: &'static str, t: &Tokens) -> impl IntoElement {
    div()
        .text(Text::SmallStrong)
        .text_color(t.text_muted)
        .child(label)
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
