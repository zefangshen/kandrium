//! Notes: a file tree over the notes root and one markdown editor per open tab.
//!
//! Every disk operation goes through `kandrium_core::notes`, so the notes root
//! (`KANDRIUM_NOTES`), the path guard and the atomic writes are the same ones
//! the egui app uses. The tree is a straight render of `notes::scan_tree`
//! (directories first, then files, each tier by name) and only the expanded
//! set is kept here.
//!
//! Editing is vim-modal by default, exactly like the card description: the
//! shared [`crate::vim::VimSession`] consumes Normal/Visual keys in a capture
//! listener and rewrites the textarea buffer, and `Esc` never edits text.
//! Autosave is a 400 ms debounce per tab; the last text written to disk is
//! kept on the tab so a reload (or the `default_value` we set ourselves) never
//! looks like an edit.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::base::text::markdown;
use gpui_kit::component::input::{Editor, EditorState, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, TestSupportExt, Window, div, px, relative, rgba,
};

use kandrium_core::notes::NoteEntry;

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens};
use crate::vim::VimSession;

/// Tree panel width when expanded, matching the other tree sections.
pub const TREE_W: f32 = 240.0;
/// Collapsed rail width.
pub const RAIL_W: f32 = 30.0;
/// Debounce before a dirty tab is written.
pub const AUTOSAVE: Duration = Duration::from_millis(400);

/// What the inline name field is creating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftKind {
    Note,
    Folder,
    Rename,
}

/// An inline name field in the tree header.
pub struct NameDraft {
    pub kind: DraftKind,
    /// Directory the new entry goes into, or the path being renamed.
    pub target: PathBuf,
    pub input: Entity<InputState>,
}

/// One open note.
pub struct NoteTab {
    pub path: PathBuf,
    pub name: String,
    pub editor: Entity<EditorState>,
    pub vim: VimSession,
    pub preview: bool,
    /// Text as last written to disk; the edit observer compares against it.
    pub saved: String,
    pub dirty: bool,
    /// Bumped on every real edit so a stale debounce can bow out.
    pub generation: u64,
}

/// Notes section state.
pub struct NotesState {
    pub root: Option<PathBuf>,
    pub entries: Vec<NoteEntry>,
    pub expanded: std::collections::HashSet<PathBuf>,
    pub selected: Option<PathBuf>,
    pub tabs: Vec<NoteTab>,
    pub active: Option<usize>,
    pub tree_open: bool,
    /// Expanded tree width in points; persisted by the shell.
    pub tree_w: f32,
    pub error: Option<String>,
    pub draft: Option<NameDraft>,
    /// Path waiting for a delete confirmation.
    pub confirm: Option<PathBuf>,
}

impl Default for NotesState {
    fn default() -> Self {
        Self {
            root: None,
            entries: Vec::new(),
            expanded: std::collections::HashSet::new(),
            selected: None,
            tabs: Vec::new(),
            active: None,
            tree_open: true,
            tree_w: TREE_W,
            error: None,
            draft: None,
            confirm: None,
        }
    }
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

    /// Where a new note or folder lands: the selected folder, the selected
    /// file's folder, or the root.
    pub fn selected_dir(&self, root: &Path) -> PathBuf {
        match &self.selected {
            Some(path) if path.is_dir() => path.clone(),
            Some(path) => path
                .parent()
                .map(|parent| parent.to_path_buf())
                .unwrap_or_else(|| root.to_path_buf()),
            None => root.to_path_buf(),
        }
    }
}

/// The half's 38 px title row: name, new-note, new-folder, whole-tree collapse
/// and an optional trailing control (the Wiki half toggle).
pub(crate) fn tree_header(
    t: &Tokens,
    cx: &mut Context<AppShell>,
    trailing: Option<AnyElement>,
) -> AnyElement {
    div()
        .h(px(38.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(12.))
        .border_b_1()
        .border_color(t.divider)
        .child(
            div()
                .flex_1()
                .truncate()
                .text(Text::Label)
                .text_color(t.text_muted)
                .child("NOTES"),
        )
        .child(tree_action(
            "notes-new-note",
            Icon::FilePlus,
            t,
            cx.listener(|this, _: &ClickEvent, window, cx| {
                this.begin_note_draft(DraftKind::Note, window, cx)
            }),
        ))
        .child(tree_action(
            "notes-new-folder",
            Icon::FolderPlus,
            t,
            cx.listener(|this, _: &ClickEvent, window, cx| {
                this.begin_note_draft(DraftKind::Folder, window, cx)
            }),
        ))
        .child(tree_action(
            "notes-collapse",
            Icon::ChevronLeft,
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| this.toggle_wiki_tree(cx)),
        ))
        .children(trailing)
        .into_any_element()
}

/// The inline name field, any error and the tree rows. Scrolls inside the
/// height its parent gives it.
pub(crate) fn tree_body(state: &NotesState, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let mut panel = div()
        .id("notes-tree-body")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0();

    if let Some(draft) = &state.draft {
        let label = match draft.kind {
            DraftKind::Note => "New note",
            DraftKind::Folder => "New folder",
            DraftKind::Rename => "Rename",
        };
        panel = panel.child(
            div()
                .id("notes-draft")
                .test_support()
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(4.))
                .px(px(10.))
                .py(px(8.))
                .border_b_1()
                .border_color(t.divider)
                .capture_key_down(cx.listener(|this, event, window, cx| {
                    this.handle_note_draft_key(event, window, cx)
                }))
                .child(
                    div()
                        .text(Text::Small)
                        .text_color(t.text_muted)
                        .child(label),
                )
                .child(crate::views::field_input(&draft.input, t)),
        );
    }

    if let Some(error) = &state.error {
        panel = panel.child(
            div()
                .id("notes-error")
                .test_support()
                .flex_none()
                .mx(px(8.))
                .my(px(6.))
                .p(px(8.))
                .rounded(px(t.radius))
                .bg(rgba(0x5c1d1dcc))
                .text(Text::Small)
                .text_color(t.text)
                .child(error.clone()),
        );
    }

    let mut rows = div()
        .id("notes-tree-rows")
        .test_support()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll();
    if state.entries.is_empty() && state.draft.is_none() {
        rows = rows.child(
            div()
                .px(px(12.))
                .py(px(10.))
                .text(Text::Small)
                .text_color(t.text_faint)
                .child("No notes yet."),
        );
    }
    for entry in &state.entries {
        rows = rows.child(tree_row(entry, 0, state, t, cx));
    }

    panel.child(rows).into_any_element()
}

fn tree_action(
    id: impl Into<gpui_kit::ElementId>,
    glyph: Icon,
    t: &Tokens,
    listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .p(px(3.))
        .rounded(px(4.))
        .cursor_pointer()
        .hover(|style| style.bg(t.hover))
        .on_click(listener)
        .child(icon(glyph, 13., t.text_muted))
        .into_any_element()
}

fn tree_row(
    entry: &NoteEntry,
    depth: usize,
    state: &NotesState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let path = entry.path.clone();
    let selected = state.selected.as_deref() == Some(entry.path.as_path());
    let open = state.expanded.contains(&entry.path);
    let mut row = div()
        .id(format!("note-row-{}", entry.path.display()))
        .test_support()
        .mx(px(6.))
        .relative()
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(26.))
        .rounded(px(4.))
        .pl(px(8.0 + depth as f32 * 12.0))
        .pr(px(8.))
        .cursor_pointer()
        .when(selected, |this| this.bg(t.surface_2))
        .hover(|style| style.bg(t.hover));

    if selected {
        row = row.child(
            div()
                .absolute()
                .left(px(0.))
                .top(px(6.))
                .w(px(2.))
                .h(px(14.))
                .rounded(px(1.))
                .bg(t.accent),
        );
    }

    if entry.is_dir {
        let toggle = path.clone();
        let rename = path.clone();
        let delete = path.clone();
        let row_id = entry.path.display().to_string();
        row = row
            .child(icon(
                if open {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                12.,
                t.text_faint,
            ))
            .child(icon(Icon::Folder, 13., t.text_muted))
            .child(
                div()
                    .flex_1()
                    .text(Text::Small)
                    .text_color(t.text)
                    .child(entry.name.clone()),
            )
            .child(tree_action(
                format!("note-folder-rename-{row_id}"),
                Icon::Pencil,
                t,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.begin_note_rename(rename.clone(), window, cx)
                }),
            ))
            .child(tree_action(
                format!("note-folder-delete-{row_id}"),
                Icon::Trash2,
                t,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.ask_delete_note(delete.clone(), cx)
                }),
            ))
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.toggle_notes_folder(toggle.clone(), cx)
            }));
    } else {
        row = row
            .child(div().w(px(12.)))
            .child(icon(Icon::FileText, 13., t.text_muted))
            .child(
                div()
                    .flex_1()
                    .text(Text::Small)
                    .text_color(t.text)
                    .child(entry.name.clone()),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_note(path.clone(), window, cx)
            }));
    }

    let mut column = div().flex().flex_col().child(row);
    if entry.is_dir && open {
        for child in entry.children.iter().flatten() {
            column = column.child(tree_row(child, depth + 1, state, t, cx));
        }
    }
    column.into_any_element()
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
            .child(crate::views::vim_indicator("notes-vim", &tab.vim, t))
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
                            .child(crate::views::gutter_rule(lines, t, window)),
                    ),
            )
            .capture_key_down(
                cx.listener(|this, event, window, cx| this.handle_notes_key(event, window, cx)),
            );
    }

    body = body.child(status_bar(tab, t, cx));

    if let Some(confirm) = &state.confirm {
        body = body.child(delete_confirm(confirm, t, cx));
    }
    body.into_any_element()
}

fn tab_action(
    id: &'static str,
    label: &'static str,
    t: &Tokens,
    listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .px(px(8.))
        .py(px(3.))
        .rounded(px(t.radius))
        .cursor_pointer()
        .hover(|style| style.bg(t.hover))
        .on_click(listener)
        .child(
            div()
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(label),
        )
        .into_any_element()
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
            dirty: tab.dirty,
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

fn delete_confirm(path: &Path, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let target = path.to_path_buf();
    div()
        .id("notes-delete-confirm")
        .test_support()
        .mx(px(16.))
        .mb(px(16.))
        .p(px(12.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(t.radius))
        .bg(t.card)
        .border_1()
        .border_color(t.border)
        .child(
            div()
                .flex_1()
                .text(Text::Small)
                .text_color(t.text)
                .child(format!("Delete \"{name}\"? This cannot be undone.")),
        )
        .child(tab_action(
            "notes-delete-cancel",
            "Cancel",
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| this.cancel_delete_note(cx)),
        ))
        .child(tab_action(
            "notes-delete-confirm-yes",
            "Delete",
            t,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.confirm_delete_note(target.clone(), cx)
            }),
        ))
        .into_any_element()
}
