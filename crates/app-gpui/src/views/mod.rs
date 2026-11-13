//! Section bodies for the gpui shell.

use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::{Sizable, Size};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Entity, InteractiveElement, IntoElement, Styled, TestSupportExt,
    Window, div, px,
};

use crate::theme::{Text, TextStyled, Tokens};

pub mod archive;
pub mod card_modal;
pub mod clocks;
pub mod clocks_panels;
pub mod documents;
pub mod documents_pdf;
pub mod notes;
pub mod placeholder;
pub mod projects;
pub mod tasks;

/// A single-line field at the mockup's `.input` metrics: 8 px side padding,
/// `field` fill and a 5 px radius.
///
/// `gpui-component`'s default (Medium) input brings 10 px side padding and a
/// 32 px height, which reads heavier than the mockup's fields; Small drops the
/// vertical padding.
///
/// The returned input paints its own fill, border and inset. Never wrap it in
/// another bordered or filled frame: that shows a second box inside the
/// field's box and pushes the caret and the selection highlight inward.
pub fn field_input(state: &Entity<InputState>, t: &Tokens) -> Input {
    Input::new(state)
        .with_size(Size::Small)
        .h(px(26.))
        .rounded(px(5.))
        .bg(t.field)
}

/// [`field_input`] at the 34 px row height the card editor and the board's
/// inline "Add card" draft use.
///
/// The height goes through [`Styled::h`] because [`Input::h`] only sizes
/// multi-line inputs.
pub fn field_input_tall(state: &Entity<InputState>, t: &Tokens) -> Input {
    Styled::h(field_input(state, t), px(34.))
}

/// The shared Markdown Edit/Preview segmented control, used by the card
/// description, Notes, project documents and document `.md` tabs.
///
/// Both segments are always rendered and use the same selected fill as the
/// section control and board tabs. Clicking the active segment is a no-op;
/// callers guard their toggle so the handler only runs on a real change.
pub fn mode_switch(
    id: &'static str,
    preview: bool,
    t: &Tokens,
    on_edit: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_preview: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .test_support()
        .flex()
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(7.))
        .bg(t.hover)
        .child(segmented_option(
            format!("{id}-edit"),
            "Edit",
            !preview,
            false,
            t,
            on_edit,
        ))
        .child(segmented_option(
            format!("{id}-preview"),
            "Preview",
            preview,
            false,
            t,
            on_preview,
        ))
}

/// [`mode_switch`] at the compact metrics the project Overview uses on its
/// description row: shorter segments and a tighter container, same fill and
/// the same two always-visible options.
pub fn mode_switch_compact(
    id: &'static str,
    preview: bool,
    t: &Tokens,
    on_edit: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_preview: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .test_support()
        .flex()
        .items_center()
        .gap(px(1.))
        .p(px(1.))
        .rounded(px(6.))
        .bg(t.hover)
        .child(segmented_option(
            format!("{id}-edit"),
            "Edit",
            !preview,
            true,
            t,
            on_edit,
        ))
        .child(segmented_option(
            format!("{id}-preview"),
            "Preview",
            preview,
            true,
            t,
            on_preview,
        ))
}

/// One segment of a segmented control: the active option is filled with
/// `surface_2`, the others stay muted and take a pressed wash on hover.
///
/// Shared by [`mode_switch`] and the project status control, so every
/// segmented control on the page has the same geometry.
pub fn segmented_option(
    id: String,
    label: &'static str,
    active: bool,
    compact: bool,
    t: &Tokens,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let tokens = *t;
    div()
        .id(id)
        .test_support()
        .h(px(if compact { 22. } else { 24. }))
        .px(px(if compact { 7. } else { 9. }))
        .flex()
        .items_center()
        .rounded(px(4.))
        .cursor_pointer()
        .text(Text::SmallStrong)
        .text_color(if active { t.text } else { t.text_muted })
        .when(active, |el| el.bg(t.surface_2))
        .when(!active, |el| {
            el.hover(move |style| style.bg(tokens.pressed))
        })
        .child(label)
        .on_click(handler)
        .into_any_element()
}

/// A multi-line field that owns its box: fill, border and the text inset all
/// come from the textarea itself, exactly as they do for [`field_input`].
///
/// A hand-built frame around it would double the edge; size it with
/// `Textarea::h` instead.
pub fn field_textarea(state: &Entity<TextareaState>, t: &Tokens) -> Textarea {
    Textarea::new(state).bg(t.field).rounded(px(t.radius))
}

/// The open file's path for a document header: a middle-ellipsized label with
/// the full path as its tooltip. It truncates first so the vim indicator and
/// Edit | Preview stay on one row at the minimum window width.
pub fn path_label(
    id: &'static str,
    full: String,
    t: &Tokens,
    _cx: &mut Context<crate::app::AppShell>,
) -> AnyElement {
    let short = crate::views::projects::ellipsize_middle(&full, 48);
    let tooltip = full.clone();
    div()
        .id(id)
        .test_support()
        .max_w(px(260.))
        .truncate()
        .text(Text::Small)
        .text_color(t.text_faint)
        .child(short)
        .tooltip(move |_window, cx| {
            cx.new(|_| crate::views::projects::NameTooltip {
                label: tooltip.clone(),
            })
            .into()
        })
        .into_any_element()
}

/// Live vim state for a document header: the modal label, any pending
/// operator or ex text, and a quiet "vim off" when the global toggle is off.
///
/// It sits next to Edit | Preview and is informational; the status bar's
/// badge remains the toggle.
pub fn vim_indicator(id: &'static str, vim: &crate::vim::VimSession, t: &Tokens) -> AnyElement {
    let enabled = vim.is_enabled();
    let insert = vim.mode_label() == "INSERT";
    let pending = if enabled {
        vim.pending()
    } else {
        String::new()
    };
    div()
        .id(id)
        .test_support()
        .flex()
        .items_center()
        .gap(px(5.))
        .h(px(22.))
        .px(px(7.))
        .rounded(px(4.))
        .border_1()
        .border_color(t.border)
        .hover(|style| style.border_color(t.border_hover))
        .text(Text::Micro)
        .text_color(if !enabled {
            t.text_ghost
        } else if insert {
            t.accent_text
        } else {
            t.text_muted
        })
        .when(enabled, |this| {
            this.child(div().size(px(4.)).rounded(px(2.)).bg(t.accent))
        })
        .child(if enabled { vim.mode_label() } else { "vim off" })
        .when(!pending.is_empty(), |this| {
            this.child(
                div()
                    .text(Text::Micro)
                    .text_color(t.text_faint)
                    .child(pending),
            )
        })
        .into_any_element()
}

/// The mockup's `.tabs` row: full-height segments on the titlebar surface with
/// the body starting directly under it.
pub fn tab_strip(t: &Tokens) -> gpui_kit::Div {
    div()
        .flex()
        .items_stretch()
        .h(px(38.))
        .flex_none()
        .bg(t.titlebar)
        .border_b_1()
        .border_color(t.border)
}

/// One `.tab` segment. Callers add the click handler and label; the active
/// state paints the card fill the body below uses, so the tab reads as part of
/// the pane, and inactive tabs stay on the titlebar surface.
pub fn pane_tab(id: String, active: bool, t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .h_full()
        .flex()
        .items_center()
        .gap(px(7.))
        .px(px(12.))
        .border_r_1()
        .border_color(t.border)
        .when(active, |this| this.bg(t.card))
        .id(id)
}

/// The editor component paints the line-number gutter itself and exposes no
/// border for it, so the hairline is an overlay placed on the gutter's right
/// edge — the same x the text starts at. The component reserves
/// `width("+" x digits) + 6 px` for the numbers, with the digit count clamped to
/// 3..=7 exactly like its own `line_number_len`; shaping the same string with
/// the ambient text style keeps the rule on that edge at every digit count.
/// The rule never moves: the gutter is fixed while the text scrolls under it.
pub fn gutter_rule(lines: usize, t: &Tokens, window: &Window) -> AnyElement {
    let digits = ((lines.max(1).ilog10() as usize) + 1).clamp(3, 7);
    let style = window.text_style();
    let run = gpui_kit::TextRun {
        len: digits,
        font: style.font(),
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let width = window
        .text_system()
        .shape_line(
            "+".repeat(digits).into(),
            style.font_size.to_pixels(window.rem_size()),
            &[run],
            None,
        )
        .width;
    div()
        .absolute()
        .left(width + px(6.))
        .top(px(0.))
        .bottom(px(0.))
        .w(px(1.))
        .bg(t.divider)
        .into_any_element()
}

/// Data the shared editor status bar shows. Every Markdown surface fills this
/// from its own editor so the bar cannot drift from the buffer.
pub struct EditorStatus<'a> {
    /// `NORMAL` / `INSERT` / `vim` while editing, `PREVIEW` in preview mode.
    pub mode: &'a str,
    /// Whether vim owns the buffer; the badge is the vim toggle only then.
    pub vim: bool,
    /// Pending vim keys, `:` line or its error, with `true` for an error.
    pub pending: Option<(String, bool)>,
    pub line: usize,
    pub column: usize,
    pub words: usize,
    pub dirty: bool,
    /// Quiet key hint for the surface; empty when there is nothing to teach.
    pub hint: &'a str,
}

/// The editor's bottom status bar: mode (also the vim toggle), any in-flight
/// vim command, cursor position, word count, format and save state.
///
/// It is deliberately a plain row rather than a second header: the mockup puts
/// all transient editor state at the bottom edge so the top strip stays about
/// the document.
pub fn editor_status_bar(
    id: &'static str,
    status: EditorStatus<'_>,
    t: &Tokens,
    on_mode: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let toggle = status.vim && status.mode != "PREVIEW";
    let mode_label = if status.mode == "PREVIEW" || status.vim {
        status.mode.to_string()
    } else {
        "vim off".to_string()
    };
    div()
        .id(id)
        .test_support()
        .h(px(26.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(12.))
        .border_t_1()
        .border_color(t.border)
        .bg(t.card)
        .child(
            div()
                .id(format!("{id}-mode"))
                .test_support()
                .px(px(6.))
                .py(px(1.))
                .rounded(px(4.))
                .bg(if toggle { t.accent_soft } else { t.surface_2 })
                .text(Text::Micro)
                .text_color(if toggle { t.accent_text } else { t.text_muted })
                .when(toggle, |this| {
                    this.cursor_pointer()
                        .hover(|style| style.bg(t.selected))
                        .on_click(on_mode)
                })
                .child(mode_label),
        )
        .when_some(status.pending, |this, (text, error)| {
            this.child(
                div()
                    .text(Text::Micro)
                    .text_color(if error { t.danger } else { t.text_muted })
                    .child(text),
            )
        })
        .child(
            div()
                .text(Text::Micro)
                .text_color(t.text_faint)
                .child(format!("Ln {}, Col {}", status.line, status.column)),
        )
        .child(div().size(px(3.)).rounded(px(2.)).bg(t.text_ghost))
        .child(
            div()
                .text(Text::Micro)
                .text_color(t.text_faint)
                .child(format!("{} words", status.words)),
        )
        .when(!status.hint.is_empty(), |this| {
            this.child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text(Text::Micro)
                    .text_color(t.text_ghost)
                    .child(status.hint.to_string()),
            )
        })
        .child(div().flex_1())
        .child(
            div()
                .text(Text::Micro)
                .text_color(t.text_faint)
                .child("Markdown"),
        )
        .child(div().size(px(3.)).rounded(px(2.)).bg(t.text_ghost))
        .child(
            div()
                .text(Text::Micro)
                .text_color(if status.dirty {
                    t.text_muted
                } else {
                    t.text_faint
                })
                .child(if status.dirty { "Saving…" } else { "Saved" }),
        )
        .into_any_element()
}
