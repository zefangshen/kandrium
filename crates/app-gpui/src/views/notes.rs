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
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, CursorStyle, DragMoveEvent, Entity, Render,
    TestSupportExt, Window, div, px, rgba,
};

use kandrium_core::notes::NoteEntry;

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::placeholder;
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
    pub editor: Entity<TextareaState>,
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

/// Render the whole section.
pub fn notes_view(
    state: &NotesState,
    t: &Tokens,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let mut row = div()
        .id("notes-view")
        .test_support()
        .size_full()
        .flex()
        .child(tree_panel(state, t, cx));

    if state.tree_open {
        row = row
            .child(tree_splitter(t))
            .child(div().w(px(1.)).h_full().bg(t.divider));
    }
    row.on_drag_move::<NotesTreeDrag>(cx.listener(
        |this, event: &DragMoveEvent<NotesTreeDrag>, _window, cx| {
            let width = f32::from(event.event.position.x) - f32::from(event.bounds.origin.x);
            this.set_notes_tree_width(width, cx);
        },
    ))
    .on_drop::<NotesTreeDrag>(cx.listener(|this, _drag: &NotesTreeDrag, _window, cx| {
        this.finish_tree_resize(cx);
    }))
    .child(content(state, t, cx))
    .into_any_element()
}

/// Drawn as a wider hit area than its 1 px rule. Dragging reports the pointer
/// x back to the shell, which clamps and persists the width.
fn tree_splitter(t: &Tokens) -> AnyElement {
    div()
        .id("notes-tree-splitter")
        .test_support()
        .w(px(5.))
        .h_full()
        .flex_none()
        .cursor(CursorStyle::ResizeLeftRight)
        .hover(|style| style.bg(t.hover))
        .on_drag(NotesTreeDrag, |_drag, _offset, _window, cx| {
            cx.new(|_| TreeDragGhost)
        })
        .child(div().w(px(1.)).h_full().bg(t.divider))
        .into_any_element()
}

/// Payload for a tree-splitter drag. Only its type selects the listener.
pub struct NotesTreeDrag;

struct TreeDragGhost;

impl Render for TreeDragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(1.)).h(px(1.))
    }
}

/// Header (collapse, new note, new folder), optional name field, tree rows.
fn tree_panel(state: &NotesState, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    if !state.tree_open {
        return div()
            .id("notes-tree-rail")
            .test_support()
            .w(px(RAIL_W))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(10.))
            .border_r_1()
            .border_color(t.divider)
            .child(
                div()
                    .id("notes-expand")
                    .test_support()
                    .p(px(4.))
                    .rounded(px(t.radius))
                    .cursor_pointer()
                    .hover(|style| style.bg(t.hover))
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, _window, cx| this.toggle_notes_tree(cx)),
                    )
                    .child(icon(Icon::ChevronRight, 14., t.text_muted)),
            )
            .into_any_element();
    }

    let mut panel = div()
        .id("notes-tree")
        .test_support()
        .w(px(state.tree_w))
        .h_full()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(t.divider)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(10.))
                .py(px(8.))
                .border_b_1()
                .border_color(t.divider)
                .child(icon(Icon::StickyNote, 14., t.text_muted))
                .child(
                    div()
                        .flex_1()
                        .text(Text::SmallStrong)
                        .text_color(t.text)
                        .child("Notes"),
                )
                .child(tree_action(
                    "notes-collapse",
                    Icon::ChevronLeft,
                    t,
                    cx.listener(|this, _: &ClickEvent, _window, cx| this.toggle_notes_tree(cx)),
                ))
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
                )),
        );

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
                .child(Input::new(&draft.input)),
        );
    }

    if let Some(error) = &state.error {
        panel = panel.child(
            div()
                .id("notes-error")
                .test_support()
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
        .w_full()
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(10.0 + depth as f32 * 12.0))
        .pr(px(8.))
        .py(px(4.))
        .cursor_pointer()
        .when(selected, |this| this.bg(t.selected))
        .hover(|style| style.bg(t.hover));

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

/// Tab strip plus the editor, preview or empty state.
fn content(state: &NotesState, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let mut body = div()
        .id("notes-content")
        .test_support()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col();

    if state.tabs.is_empty() {
        return body
            .child(placeholder::message(
                "notes-empty",
                Icon::StickyNote,
                "No note open",
                "Pick a note in the tree, or use the new-note button.",
                t,
            ))
            .into_any_element();
    }

    let mut strip = div()
        .id("notes-tabs")
        .test_support()
        .flex()
        .items_center()
        .gap(px(2.))
        .px(px(8.))
        .py(px(4.))
        .border_b_1()
        .border_color(t.divider);
    for (index, tab) in state.tabs.iter().enumerate() {
        let active = state.active == Some(index);
        let path = tab.path.clone();
        let close_path = tab.path.clone();
        strip = strip.child(
            div()
                .id(format!("note-tab-{}", tab.path.display()))
                .test_support()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(8.))
                .py(px(3.))
                .rounded(px(t.radius))
                .when(active, |this| this.bg(t.selected))
                .hover(|style| style.bg(t.hover))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.activate_note_tab(path.clone(), window, cx)
                }))
                .child(
                    div()
                        .text(Text::Small)
                        .text_color(if active { t.text } else { t.text_muted })
                        .child(tab.name.clone()),
                )
                .when(tab.dirty, |this| {
                    this.child(div().size(px(4.)).rounded(px(2.)).bg(t.accent))
                })
                .child(
                    div()
                        .id(format!("note-tab-close-{}", tab.path.display()))
                        .p(px(1.))
                        .rounded(px(3.))
                        .hover(|style| style.bg(t.pressed))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.close_note_tab_by_path(close_path.clone(), cx)
                        }))
                        .child(icon(Icon::X, 11., t.text_faint)),
                ),
        );
    }
    strip = strip.child(div().flex_1()).child(tab_action(
        "notes-preview",
        if state.active_tab().is_some_and(|tab| tab.preview) {
            "Edit"
        } else {
            "Preview"
        },
        t,
        cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_note_preview(window, cx)),
    ));
    if let Some(tab) = state.active_tab() {
        let rename_path = tab.path.clone();
        let delete_path = tab.path.clone();
        strip = strip
            .child(tab_action(
                "notes-rename",
                "Rename",
                t,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.begin_note_rename(rename_path.clone(), window, cx)
                }),
            ))
            .child(tab_action(
                "notes-delete",
                "Delete",
                t,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.ask_delete_note(delete_path.clone(), cx)
                }),
            ));
    }

    body = body.child(strip);
    let Some(tab) = state.active_tab() else {
        return body.into_any_element();
    };

    if tab.preview {
        let text = tab.editor.read(cx).value().to_string();
        body = body.child(
            div()
                .id("note-preview")
                .test_support()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p(px(20.))
                .child(markdown(text.as_str())),
        );
    } else {
        body = body
            .child(mode_strip(tab, t, cx))
            .child(
                div()
                    .id("note-editor")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .p(px(12.))
                    .child(Textarea::new(&tab.editor).readonly(!tab.vim.is_insert())),
            )
            .capture_key_down(
                cx.listener(|this, event, window, cx| this.handle_notes_key(event, window, cx)),
            );
    }

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

/// Mode badge (also the vim on/off toggle), pending keys, ex line or error,
/// and a short hint.
fn mode_strip(tab: &NoteTab, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let vim = &tab.vim;
    let enabled = vim.is_enabled();
    let mut row = div()
        .id("note-mode")
        .test_support()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(12.))
        .py(px(3.))
        .border_b_1()
        .border_color(t.divider)
        .child(
            div()
                .id("note-vim-toggle")
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
                .child(line.to_string()),
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
                "Esc normal · i insert · :w save · :q close"
            } else {
                "click the badge for vim"
            }),
    )
    .into_any_element()
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
