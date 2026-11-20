//! Notes: one Markdown editor per open wiki page tab.
//!
//! The Markdown tab surface shared by the Knowledge section: one editor or
//! preview per open wiki page, plus the merged tab row's note kind.
//!
//! Every disk operation goes through `kandrium_core::notes`, so the wiki root
//! (`KANDRIUM_WIKI`), the path guard and the atomic writes are the same ones
//! the MCP server uses. The tree itself lives in [`super::wiki`]; this module
//! keeps only the open tabs.
//!
//! Editing is vim-modal by default, exactly like the card description: the
//! shared [`crate::vim::VimSession`] consumes Normal/Visual keys in a capture
//! listener and rewrites the textarea buffer, and `Esc` never edits text.
//! Autosave is a 400 ms debounce per tab; the last text written to disk is
//! kept on the tab so a reload (or the `default_value` we set ourselves) never
//! looks like an edit.

use std::path::{Path, PathBuf};

use gpui_kit::base::text::markdown;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, Entity, TestSupportExt, Window, div, px, relative,
};

use crate::app::AppShell;
use crate::draft::Draft;
use crate::theme::Tokens;
use crate::vim::VimSession;

/// One open note.
pub struct NoteTab {
    pub path: PathBuf,
    pub name: String,
    pub editor: Entity<EditorState>,
    pub vim: VimSession,
    pub preview: bool,
    /// Text as last written to disk, plus the debounce generation.
    pub draft: Draft,
}

/// Wiki section tab state: the open page tabs.
#[derive(Default)]
pub struct NotesState {
    pub tabs: Vec<NoteTab>,
    pub active: Option<usize>,
    /// Open or save failure shown above the page tree.
    pub error: Option<String>,
}

impl NotesState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tab_index(&self, path: &Path) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.path == path)
    }

    pub fn active_tab(&self) -> Option<&NoteTab> {
        self.active.and_then(|index| self.tabs.get(index))
    }
}

/// One note's head row, editor or preview, and status bar, below the tab row.
pub(crate) fn detail_body(
    state: &NotesState,
    index: usize,
    t: &Tokens,
    window: &Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let mut body = div()
        .id("notes-detail")
        .test_support()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col();

    let Some(tab) = state.tabs.get(index) else {
        return body.into_any_element();
    };
    let preview = tab.preview;
    body = body.child(
        div()
            .id("notes-head")
            .test_support()
            .h(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(14.))
            .border_b_1()
            .border_color(t.divider)
            .child(crate::views::path_label(
                "notes-path",
                tab.path.display().to_string(),
                t,
                cx,
            ))
            .child(div().flex_1())
            .child(crate::views::mode_switch(
                "notes-preview",
                preview,
                t,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if preview {
                        this.toggle_note_preview(window, cx);
                    }
                }),
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if !preview {
                        this.toggle_note_preview(window, cx);
                    }
                }),
            )),
    );

    if tab.preview {
        let text = tab.editor.read(cx).value().to_string();
        body = body.child(
            div()
                .id("note-preview")
                .test_support()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(30.))
                .pt(px(20.))
                .pb(px(30.))
                .child(markdown(text.as_str())),
        );
    } else {
        let lines = tab.editor.read(cx).value().lines().count();
        let (mono_family, mono_size) = {
            let theme = cx.theme();
            (theme.mono_font_family.clone(), theme.mono_font_size)
        };
        body = body
            .child(
                div()
                    .id("note-editor")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .flex_1()
                            .min_h_0()
                            .child(
                                div().absolute().inset_0().child(
                                    // The multi-line input auto-sizes to its content;
                                    // this pins the editor to the pane the wrapper
                                    // already occupies.
                                    Editor::new(&tab.editor)
                                        .h(relative(1.))
                                        .bordered(false)
                                        .readonly(!tab.vim.is_insert()),
                                ),
                            )
                            .when(tab.vim.block_cursor(), |this| {
                                this.child(
                                    crate::vim::block_cursor_overlay(
                                        tab.editor.clone(),
                                        mono_family,
                                        mono_size,
                                        t.accent.into(),
                                        t.field.into(),
                                    )
                                    .absolute()
                                    .inset_0(),
                                )
                            })
                            .child(crate::views::gutter_rule(lines, t, window)),
                    ),
            )
            .capture_key_down(
                cx.listener(|this, event, window, cx| this.handle_notes_key(event, window, cx)),
            );
    }

    body = body.child(status_bar(tab, t, cx));
    body.into_any_element()
}

/// The editor's bottom status bar: vim mode (also the toggle), pending keys,
/// ex line or error, cursor position, word count and save state.
fn status_bar(tab: &NoteTab, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let preview = tab.preview;
    let position = tab.editor.read(cx).cursor_position();
    let words = tab.editor.read(cx).value().split_whitespace().count();
    let pending = if preview {
        None
    } else if let Some(line) = tab.vim.ex_line() {
        Some((line.to_string(), false))
    } else if let Some(error) = tab.vim.ex_error() {
        Some((error.to_string(), true))
    } else {
        let pending = tab.vim.pending();
        (!pending.is_empty()).then_some((pending, false))
    };
    crate::views::editor_status_bar(
        "note-status",
        crate::views::EditorStatus {
            mode: if preview {
                "PREVIEW"
            } else {
                tab.vim.mode_label()
            },
            vim: tab.vim.is_enabled(),
            pending,
            line: position.line as usize + 1,
            column: position.character as usize + 1,
            words,
            dirty: tab.draft.is_dirty(),
            hint: if preview {
                "660 px measure"
            } else if tab.vim.is_enabled() {
                "Esc normal · i insert · :w save · :q close"
            } else {
                "click the badge for vim"
            },
        },
        t,
        cx.listener(|this, _: &ClickEvent, _window, cx| this.toggle_vim_mode(cx)),
    )
}
