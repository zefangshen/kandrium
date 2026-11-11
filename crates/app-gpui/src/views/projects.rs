//! Projects: a DB-backed tree (folders + projects) and a tabbed detail pane.
//!
//! The tree, project rows and documents all come from `kandrium_core::projects`
//! through `Store`; nothing here touches SQLite directly. The right pane has
//! two pinned tabs — Overview and Tasks — plus one closable tab per open
//! document. Tasks deliberately renders through [`super::tasks::board_view`]:
//! the shell points its normal board state at the project's board, so cards,
//! drag-and-drop and the card modal are the same implementation as the Tasks
//! section, not a copy.
//!
//! Project creation, deletion, archiving, identifier locking and document
//! path guards are core's (`projects::create_project`, `delete_project`,
//! `archive_project`, `set_identifier`, `read_doc`/`write_doc`). The view keeps
//! only presentation state: expansion, filter, drafts, tabs and the drag
//! session.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::base::text::markdown;
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, CursorStyle, DragMoveEvent, Entity, Render,
    TestSupportExt, Window, div, px, rgba,
};

use kandrium_core::BoardWithColumns;
use kandrium_core::fsutil::FsEntry;
use kandrium_core::projects::{
    PROJECT_STATUSES, Project, ProjectNode, ProjectSummary, find_project,
};

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::tasks::DropTarget;
use crate::views::{placeholder, tasks};
use crate::vim::VimSession;

/// Tree panel width when expanded, matching the other tree sections.
pub const TREE_W: f32 = 240.0;
/// Collapsed rail width.
pub const RAIL_W: f32 = 30.0;
/// Debounce before a dirty document tab is written.
pub const AUTOSAVE: Duration = Duration::from_millis(400);

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// What the tree's inline name field is creating or renaming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectDraftKind {
    Project,
    Folder,
    RenameFolder,
}

/// Inline name field in the tree header.
pub struct ProjectDraft {
    pub kind: ProjectDraftKind,
    /// Folder the new project/folder lands in (`None` = tree root).
    pub folder: Option<String>,
    pub input: Entity<InputState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectConfirmKind {
    DeleteProject,
    DeleteFolder,
}

pub struct ProjectConfirm {
    pub kind: ProjectConfirmKind,
    pub id: String,
    pub name: String,
}

/// Overview title inline editor.
pub struct TitleEdit {
    pub project_id: String,
    pub input: Entity<InputState>,
}

/// Overview inputs: identifier, source, description. Rebuilt only when the
/// selected project changes or the user saves, never on an external reload.
pub struct DetailEditor {
    pub project_id: String,
    pub identifier: Entity<InputState>,
    pub source: Entity<InputState>,
    pub description: Entity<TextareaState>,
    pub vim: VimSession,
    pub saved_description: String,
}

/// Whether the documents draft creates a file or a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocDraftKind {
    File,
    Folder,
}

/// Inline name field in the Overview documents list.
pub struct DocDraft {
    pub kind: DocDraftKind,
    /// Project-relative folder (`Path::new("")` is the project root).
    pub folder: PathBuf,
    pub input: Entity<InputState>,
}

/// One open project document.
pub struct ProjectDocTab {
    /// Project-relative path, as core expects for `read_doc` / `write_doc`.
    pub rel: PathBuf,
    pub name: String,
    pub editor: Entity<TextareaState>,
    pub vim: VimSession,
    pub preview: bool,
    /// Text as last written to disk.
    pub saved: String,
    pub dirty: bool,
    /// Bumped on every real edit so a stale debounce can bow out.
    pub generation: u64,
}

pub enum ProjectTab {
    Overview,
    Tasks,
    Doc(ProjectDocTab),
}

impl ProjectTab {
    pub fn is_closable(&self) -> bool {
        matches!(self, ProjectTab::Doc(_))
    }
}

pub struct ProjectDragState {
    pub id: String,
    pub from_folder: Option<String>,
    pub hover: Option<(String, bool)>,
}

/// Drag payload for a project row.
#[derive(Clone)]
pub struct ProjectDrag {
    pub id: String,
    pub from_folder: Option<String>,
}

pub struct ProjectsState {
    pub root: Option<PathBuf>,
    pub tree: Vec<ProjectNode>,
    /// Expanded folder ids.
    pub expanded: HashSet<String>,
    pub selected: Option<String>,
    /// Folder new projects/folders land in (`None` = root).
    pub selected_folder: Option<String>,
    pub tabs: Vec<ProjectTab>,
    pub active: usize,
    /// Project the tabs belong to; a selection change rebuilds the pinned tabs.
    pub tabs_project: Option<String>,
    pub detail: Option<Project>,
    pub summary: Option<ProjectSummary>,
    pub files: Vec<FsEntry>,
    pub filter: String,
    pub filter_input: Option<Entity<InputState>>,
    pub show_archived: bool,
    pub archived: Vec<Project>,
    pub tree_open: bool,
    pub tree_w: f32,
    pub error: Option<String>,
    pub draft: Option<ProjectDraft>,
    pub confirm: Option<ProjectConfirm>,
    pub title_edit: Option<TitleEdit>,
    pub detail_editor: Option<DetailEditor>,
    pub drag: Option<ProjectDragState>,
    /// Project-relative folder targeted by the documents draft.
    pub doc_folder: PathBuf,
    pub doc_draft: Option<DocDraft>,
    /// When the title row was last clicked, for double-click detection.
    pub last_title_click: Option<(String, std::time::Instant)>,
}

impl Default for ProjectsState {
    fn default() -> Self {
        Self {
            root: None,
            tree: Vec::new(),
            expanded: HashSet::new(),
            selected: None,
            selected_folder: None,
            tabs: vec![ProjectTab::Overview, ProjectTab::Tasks],
            active: 0,
            tabs_project: None,
            detail: None,
            summary: None,
            files: Vec::new(),
            filter: String::new(),
            filter_input: None,
            show_archived: false,
            archived: Vec::new(),
            tree_open: true,
            tree_w: TREE_W,
            error: None,
            draft: None,
            confirm: None,
            title_edit: None,
            detail_editor: None,
            drag: None,
            doc_folder: PathBuf::new(),
            doc_draft: None,
            last_title_click: None,
        }
    }
}

impl ProjectsState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active_tab(&self) -> Option<&ProjectTab> {
        self.tabs.get(self.active)
    }

    pub fn active_is_tasks(&self) -> bool {
        matches!(self.active_tab(), Some(ProjectTab::Tasks))
    }

    pub fn active_doc_index(&self) -> Option<usize> {
        matches!(self.active_tab(), Some(ProjectTab::Doc(_))).then_some(self.active)
    }

    /// Whether a tree interaction owns the pointer/keyboard and an external
    /// reload should wait for the next frame.
    pub fn is_busy(&self) -> bool {
        self.drag.is_some()
            || self.draft.is_some()
            || self.title_edit.is_some()
            || self.confirm.is_some()
            || self.doc_draft.is_some()
    }

    /// The board the Tasks tab should show, if that tab is active.
    pub fn project_board_id(&self) -> Option<&str> {
        if !self.active_is_tasks() {
            return None;
        }
        self.detail
            .as_ref()
            .map(|project| project.board_id.as_str())
    }

    pub fn project_dir(&self) -> Option<PathBuf> {
        let root = self.root.as_ref()?;
        let id = self.selected.as_ref()?;
        Some(root.join(id))
    }

    /// Project-relative path for a file inside the project directory.
    pub fn rel_path(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(self.project_dir()?)
            .ok()
            .map(Path::to_path_buf)
    }

    pub fn tab_index(&self, rel: &Path) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| matches!(tab, ProjectTab::Doc(doc) if doc.rel == rel))
    }

    pub fn active_doc(&self) -> Option<&ProjectDocTab> {
        match self.active_tab() {
            Some(ProjectTab::Doc(doc)) => Some(doc),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested)
// ---------------------------------------------------------------------------

/// Filter on identifier and full name, case-insensitively.
pub fn project_matches(project: &Project, filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        return true;
    }
    project.identifier.to_lowercase().contains(&filter)
        || project.name.to_lowercase().contains(&filter)
}

fn node_matches(node: &ProjectNode, filter: &str) -> bool {
    match node {
        ProjectNode::Project(project) => project_matches(project, filter),
        ProjectNode::Folder { children, .. } => {
            children.iter().any(|child| node_matches(child, filter))
        }
    }
}

/// Insertion index among siblings *after* removing the dragged project.
/// `target` must be one of `siblings`.
pub fn insert_index(siblings: &[String], target: &str, after: bool) -> usize {
    let position = siblings
        .iter()
        .position(|id| id == target)
        .unwrap_or(siblings.len());
    if after { position + 1 } else { position }
}

/// New active index after closing `closed`, or `None` when the tab is pinned.
pub fn close_index(closable: &[bool], active: usize, closed: usize) -> Option<usize> {
    if !closable.get(closed).copied().unwrap_or(false) {
        return None;
    }
    if closable.len() <= 1 {
        return Some(0);
    }
    Some(match active {
        index if index > closed => index - 1,
        index if index == closed => closed.min(closable.len() - 2),
        index => index,
    })
}

/// First project in tree order.
pub fn first_project(nodes: &[ProjectNode]) -> Option<&Project> {
    nodes.iter().find_map(|node| match node {
        ProjectNode::Project(project) => Some(project),
        ProjectNode::Folder { children, .. } => first_project(children),
    })
}

/// All projects in tree order with their folder depth, used by the filter and
/// by tests.
pub fn flat_projects(nodes: &[ProjectNode]) -> Vec<(&Project, usize)> {
    fn walk<'a>(nodes: &'a [ProjectNode], depth: usize, out: &mut Vec<(&'a Project, usize)>) {
        for node in nodes {
            match node {
                ProjectNode::Folder { children, .. } => walk(children, depth + 1, out),
                ProjectNode::Project(project) => out.push((project, depth)),
            }
        }
    }
    let mut out = Vec::new();
    walk(nodes, 0, &mut out);
    out
}

/// Children of the folder `id`, or `None` when no such folder exists.
fn folder_children<'a>(nodes: &'a [ProjectNode], id: &str) -> Option<&'a [ProjectNode]> {
    for node in nodes {
        if let ProjectNode::Folder {
            id: folder,
            children,
            ..
        } = node
        {
            if folder == id {
                return Some(children);
            }
            if let Some(found) = folder_children(children, id) {
                return Some(found);
            }
        }
    }
    None
}

/// Folder id containing the project, or `None` when it sits at the root.
pub fn project_folder(nodes: &[ProjectNode], id: &str) -> Option<String> {
    for node in nodes {
        if let ProjectNode::Folder {
            id: folder,
            children,
            ..
        } = node
        {
            if children
                .iter()
                .any(|child| matches!(child, ProjectNode::Project(project) if project.id == id))
            {
                return Some(folder.clone());
            }
            if let Some(found) = project_folder(children, id) {
                return Some(found);
            }
        }
    }
    None
}

/// Display name of a folder anywhere in the tree.
pub fn folder_name(nodes: &[ProjectNode], id: &str) -> Option<String> {
    for node in nodes {
        if let ProjectNode::Folder {
            id: folder,
            name,
            children,
        } = node
        {
            if folder == id {
                return Some(name.clone());
            }
            if let Some(found) = folder_name(children, id) {
                return Some(found);
            }
        }
    }
    None
}

/// Project ids sharing `folder` (`None` = root), in tree order. Folders are
/// skipped: dragging reorders projects among projects only.
pub fn sibling_projects(nodes: &[ProjectNode], folder: Option<&str>) -> Vec<String> {
    let level = match folder {
        Some(id) => folder_children(nodes, id).unwrap_or(&[]),
        None => nodes,
    };
    level
        .iter()
        .filter_map(|node| match node {
            ProjectNode::Project(project) => Some(project.id.clone()),
            ProjectNode::Folder { .. } => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Section render
// ---------------------------------------------------------------------------

pub fn projects_view(
    state: &ProjectsState,
    board: Option<&BoardWithColumns>,
    drop_target: Option<&DropTarget>,
    dragging: Option<&str>,
    t: &Tokens,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let mut row = div()
        .id("projects-view")
        .test_support()
        .size_full()
        .flex()
        .child(tree_panel(state, t, cx));

    if state.tree_open {
        row = row
            .child(tree_splitter(t))
            .child(div().w(px(1.)).h_full().bg(t.divider));
    }
    row.on_drag_move::<ProjectsTreeDrag>(cx.listener(
        |this, event: &DragMoveEvent<ProjectsTreeDrag>, _window, cx| {
            let width = f32::from(event.event.position.x) - f32::from(event.bounds.origin.x);
            this.set_projects_tree_width(width, cx);
        },
    ))
    .on_drop::<ProjectsTreeDrag>(cx.listener(|this, _drag: &ProjectsTreeDrag, _window, cx| {
        this.finish_tree_resize(cx);
    }))
    .child(detail_pane(state, board, drop_target, dragging, t, cx))
    .into_any_element()
}

fn tree_splitter(t: &Tokens) -> AnyElement {
    div()
        .id("projects-tree-splitter")
        .test_support()
        .w(px(5.))
        .h_full()
        .flex_none()
        .cursor(CursorStyle::ResizeLeftRight)
        .hover(|style| style.bg(t.hover))
        .on_drag(ProjectsTreeDrag, |_drag, _offset, _window, cx| {
            cx.new(|_| TreeDragGhost)
        })
        .child(div().w(px(1.)).h_full().bg(t.divider))
        .into_any_element()
}

pub struct ProjectsTreeDrag;

struct TreeDragGhost;

impl Render for TreeDragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(1.)).h(px(1.))
    }
}

// ---------------------------------------------------------------------------
// Tree
// ---------------------------------------------------------------------------

fn tree_panel(state: &ProjectsState, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    if !state.tree_open {
        return div()
            .id("projects-tree-rail")
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
                    .id("projects-expand")
                    .test_support()
                    .p(px(4.))
                    .rounded(px(t.radius))
                    .cursor_pointer()
                    .hover(|style| style.bg(t.hover))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.toggle_projects_tree(cx)
                    }))
                    .child(icon(Icon::ChevronRight, 14., t.text_muted)),
            )
            .into_any_element();
    }

    let mut panel = div()
        .id("projects-tree")
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
                .gap(px(4.))
                .px(px(10.))
                .py(px(8.))
                .border_b_1()
                .border_color(t.divider)
                .child(icon(Icon::Folder, 14., t.text_muted))
                .child(
                    div()
                        .flex_1()
                        .text(Text::SmallStrong)
                        .text_color(t.text)
                        .child("Projects"),
                )
                .child(tree_action(
                    "projects-collapse",
                    Icon::ChevronLeft,
                    t,
                    cx.listener(|this, _: &ClickEvent, _window, cx| this.toggle_projects_tree(cx)),
                ))
                .child(tree_action(
                    "projects-new-folder",
                    Icon::FolderPlus,
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.begin_project_draft(ProjectDraftKind::Folder, window, cx)
                    }),
                ))
                .child(tree_action(
                    "projects-new-project",
                    Icon::FilePlus,
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.begin_project_draft(ProjectDraftKind::Project, window, cx)
                    }),
                )),
        )
        .child(
            div()
                .px(px(10.))
                .py(px(6.))
                .border_b_1()
                .border_color(t.divider)
                .child(match &state.filter_input {
                    Some(input) => Input::new(input).into_any_element(),
                    None => div().into_any_element(),
                }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(2.))
                .px(px(8.))
                .py(px(4.))
                .border_b_1()
                .border_color(t.divider)
                .child(view_chip(
                    "projects-active-list",
                    "Active",
                    !state.show_archived,
                    t,
                    cx.listener(|this, _: &ClickEvent, _window, cx| this.show_active_projects(cx)),
                ))
                .child(view_chip(
                    "projects-archived-list",
                    "Archived",
                    state.show_archived,
                    t,
                    cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.show_archived_projects(cx)
                    }),
                )),
        );

    if let Some(draft) = &state.draft {
        let label = match draft.kind {
            ProjectDraftKind::Project => "New project",
            ProjectDraftKind::Folder => "New folder",
            ProjectDraftKind::RenameFolder => "Rename folder",
        };
        panel = panel.child(
            div()
                .id("projects-draft")
                .test_support()
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(px(10.))
                .py(px(8.))
                .border_b_1()
                .border_color(t.divider)
                .capture_key_down(cx.listener(|this, event, window, cx| {
                    this.handle_project_draft_key(event, window, cx)
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
                .id("projects-error")
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
        .id("projects-tree-rows")
        .test_support()
        .flex_1()
        .overflow_y_scroll();

    if state.show_archived {
        if state.archived.is_empty() {
            rows = rows.child(empty_row("No archived projects.", t));
        }
        for project in &state.archived {
            rows = rows.child(archived_row(project, state, t, cx));
        }
    } else {
        let visible = if state.filter.trim().is_empty() {
            state.tree.clone()
        } else {
            state
                .tree
                .iter()
                .filter(|node| node_matches(node, &state.filter))
                .cloned()
                .collect()
        };
        if visible.is_empty() && state.draft.is_none() {
            rows = rows.child(empty_row("No projects yet.", t));
        }
        rows = rows.child(tree_rows(&visible, 0, state, t, cx));
    }

    panel.child(rows).into_any_element()
}

fn tree_rows(
    nodes: &[ProjectNode],
    depth: usize,
    state: &ProjectsState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let filtering = !state.filter.trim().is_empty();
    let mut column = div().flex().flex_col();
    for node in nodes {
        match node {
            ProjectNode::Folder { id, name, children } => {
                let open = filtering || state.expanded.contains(id);
                column = column.child(folder_row(id, name, open, depth, state, t, cx));
                let shown: Vec<ProjectNode> = if filtering {
                    children
                        .iter()
                        .filter(|child| node_matches(child, &state.filter))
                        .cloned()
                        .collect()
                } else {
                    children.clone()
                };
                if open && !shown.is_empty() {
                    column = column.child(tree_rows(&shown, depth + 1, state, t, cx));
                }
            }
            ProjectNode::Project(project) => {
                column = column.child(project_row(project, depth, state, t, cx));
            }
        }
    }
    column.into_any_element()
}

fn folder_row(
    id: &str,
    name: &str,
    open: bool,
    depth: usize,
    state: &ProjectsState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let folder_id = id.to_string();
    let selected = state.selected_folder.as_deref() == Some(id);
    let rename_id = id.to_string();
    let delete_name = name.to_string();
    let delete_id = id.to_string();
    let row_id = format!("project-folder-{id}");
    div()
        .id(row_id.clone())
        .test_support()
        .w_full()
        .flex()
        .items_center()
        .gap(px(5.))
        .pl(px(10.0 + depth as f32 * 12.0))
        .pr(px(6.))
        .py(px(4.))
        .cursor_pointer()
        .when(selected, |this| this.bg(t.selected))
        .hover(|style| style.bg(t.hover))
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
                .child(name.to_string()),
        )
        .child(tree_action(
            format!("project-folder-rename-{id}"),
            Icon::Pencil,
            t,
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.begin_project_folder_rename(&rename_id, window, cx)
            }),
        ))
        .child(tree_action(
            format!("project-folder-delete-{id}"),
            Icon::Trash2,
            t,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                this.ask_delete_folder(&delete_id, &delete_name, cx)
            }),
        ))
        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
            this.toggle_project_folder(folder_id.clone(), cx)
        }))
        .into_any_element()
}

fn project_row(
    project: &Project,
    depth: usize,
    state: &ProjectsState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let selected = state.selected.as_deref() == Some(project.id.as_str());
    let filtering = !state.filter.trim().is_empty();
    let project_id = project.id.clone();
    let folder = project.folder_id.clone();
    let drag_id = project.id.clone();
    let drag_folder = project.folder_id.clone();
    let tooltip = project.name.clone();
    let suffix = filtering.then(|| project.name.clone());
    let hover = state
        .drag
        .as_ref()
        .and_then(|drag| drag.hover.as_ref())
        .filter(|(id, _)| id == &project.id)
        .map(|(_, after)| *after);
    let is_dragged = state
        .drag
        .as_ref()
        .is_some_and(|drag| drag.id == project.id);

    let mut row = div()
        .id(format!("project-row-{}", project.id))
        .test_support()
        .w_full()
        .flex()
        .items_center()
        .gap(px(5.))
        .pl(px(10.0 + depth as f32 * 12.0))
        .pr(px(8.))
        .py(px(4.))
        .cursor_pointer()
        .when(selected, |this| this.bg(t.selected))
        .when(is_dragged, |this| this.opacity(0.4))
        .hover(|style| style.bg(t.hover))
        .child(div().w(px(12.)))
        .child(icon(Icon::FolderKanban, 13., t.text_muted))
        .child(
            div()
                .text(Text::SmallStrong)
                .text_color(t.text)
                .child(project.identifier.clone()),
        )
        .when_some(suffix, |this, name| {
            this.child(
                div()
                    .flex_1()
                    .truncate()
                    .text(Text::Small)
                    .text_color(t.text_faint)
                    .child(name),
            )
        })
        .when(!filtering, |this| this.child(div().flex_1()))
        .tooltip(move |_window, cx| {
            cx.new(|_| NameTooltip {
                label: tooltip.clone(),
            })
            .into()
        })
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.select_project(&project_id, window, cx)
        }));

    if !filtering && !state.show_archived {
        let shell = cx.entity().downgrade();
        row = row
            .on_drag(
                ProjectDrag {
                    id: drag_id.clone(),
                    from_folder: drag_folder.clone(),
                },
                move |drag: &ProjectDrag, _offset, _window, cx| {
                    let id = drag.id.clone();
                    if let Some(shell) = shell.upgrade() {
                        shell.update(cx, |this, cx| this.begin_project_drag(&id, cx));
                    }
                    cx.new(|_| ProjectDragGhost {
                        label: id.chars().take(6).collect(),
                    })
                },
            )
            .on_drag_move::<ProjectDrag>({
                let row_id = project.id.clone();
                let row_folder = folder.clone();
                cx.listener(
                    move |this, event: &DragMoveEvent<ProjectDrag>, _window, cx| {
                        if !event.bounds.contains(&event.event.position) {
                            return;
                        }
                        if event.drag(cx).from_folder != row_folder {
                            return;
                        }
                        let after = event.event.position.y >= event.bounds.center().y;
                        this.hover_project_drop(&row_id, after, cx);
                    },
                )
            })
            .on_drop::<ProjectDrag>(
                cx.listener(|this, drag: &ProjectDrag, _window, cx| this.drop_project(drag, cx)),
            );
    }

    let mut column = div().flex().flex_col();
    if hover == Some(false) {
        column = column.child(div().h(px(2.)).w_full().bg(t.accent));
    } else {
        column = column.child(div().h(px(2.)).w_full());
    }
    column = column.child(row);
    if hover == Some(true) {
        column = column.child(div().h(px(2.)).w_full().bg(t.accent));
    } else {
        column = column.child(div().h(px(2.)).w_full());
    }
    column.into_any_element()
}

fn archived_row(
    project: &Project,
    state: &ProjectsState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let selected = state.selected.as_deref() == Some(project.id.as_str());
    let id = project.id.clone();
    let tooltip = project.name.clone();
    div()
        .id(format!("project-archived-{}", project.id))
        .test_support()
        .w_full()
        .flex()
        .items_center()
        .gap(px(6.))
        .pl(px(10.))
        .pr(px(8.))
        .py(px(5.))
        .cursor_pointer()
        .when(selected, |this| this.bg(t.selected))
        .hover(|style| style.bg(t.hover))
        .child(icon(Icon::FolderKanban, 13., t.text_faint))
        .child(
            div()
                .text(Text::SmallStrong)
                .text_color(t.text_muted)
                .child(project.identifier.clone()),
        )
        .child(
            div()
                .flex_1()
                .truncate()
                .text(Text::Small)
                .text_color(t.text_faint)
                .child(project.name.clone()),
        )
        .tooltip(move |_window, cx| {
            cx.new(|_| NameTooltip {
                label: tooltip.clone(),
            })
            .into()
        })
        .on_click(
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.select_project(&id, window, cx)
            }),
        )
        .into_any_element()
}

fn empty_row(message: &'static str, t: &Tokens) -> AnyElement {
    div()
        .px(px(12.))
        .py(px(10.))
        .text(Text::Small)
        .text_color(t.text_faint)
        .child(message)
        .into_any_element()
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

fn view_chip(
    id: impl Into<gpui_kit::ElementId>,
    label: &'static str,
    selected: bool,
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
        .when(selected, |this| this.bg(t.selected))
        .hover(|style| style.bg(t.hover))
        .text(Text::Small)
        .text_color(if selected { t.text } else { t.text_muted })
        .on_click(listener)
        .child(label)
        .into_any_element()
}

/// Ghost drawn under the pointer while a project row is dragged.
struct ProjectDragGhost {
    label: String,
}

impl Render for ProjectDragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(3.))
            .rounded(px(4.))
            .bg(rgba(0x2a2a2eee))
            .text(Text::Small)
            .text_color(rgba(0xffffffff))
            .child(self.label.clone())
    }
}

/// Small floating label used as the project row's tooltip.
struct NameTooltip {
    label: String,
}

impl Render for NameTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(3.))
            .rounded(px(4.))
            .bg(rgba(0x1e1e22f2))
            .border_1()
            .border_color(rgba(0x3c3c43ff))
            .text(Text::Small)
            .text_color(rgba(0xececefff))
            .child(self.label.clone())
    }
}

// ---------------------------------------------------------------------------
// Detail pane
// ---------------------------------------------------------------------------

fn detail_pane(
    state: &ProjectsState,
    board: Option<&BoardWithColumns>,
    drop_target: Option<&DropTarget>,
    dragging: Option<&str>,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let mut body = div()
        .id("projects-detail")
        .test_support()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col();

    if state.detail.is_none() && state.selected.is_none() {
        return body
            .child(placeholder::message(
                "projects-empty",
                Icon::Folder,
                "No project selected",
                "Pick a project in the tree, or create one with the new-project button.",
                t,
            ))
            .into_any_element();
    }

    let mut strip = div()
        .id("project-tabs")
        .test_support()
        .flex()
        .items_center()
        .gap(px(2.))
        .px(px(8.))
        .py(px(4.))
        .border_b_1()
        .border_color(t.divider);
    for (index, tab) in state.tabs.iter().enumerate() {
        let active = state.active == index;
        let (label, closable) = match tab {
            ProjectTab::Overview => ("Overview".to_string(), false),
            ProjectTab::Tasks => ("Tasks".to_string(), false),
            ProjectTab::Doc(doc) => (doc.name.clone(), true),
        };
        let mut item = div()
            .id(format!("project-tab-{index}"))
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
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.activate_project_tab(index, cx)
            }))
            .child(
                div()
                    .text(Text::Small)
                    .text_color(if active { t.text } else { t.text_muted })
                    .child(label),
            );
        if let ProjectTab::Doc(doc) = tab {
            if doc.dirty {
                item = item.child(div().size(px(4.)).rounded(px(2.)).bg(t.accent));
            }
            if closable {
                item = item.child(
                    div()
                        .id(format!("project-tab-close-{index}"))
                        .p(px(1.))
                        .rounded(px(3.))
                        .hover(|style| style.bg(t.pressed))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            cx.stop_propagation();
                            this.close_project_doc(index, cx)
                        }))
                        .child(icon(Icon::X, 11., t.text_faint)),
                );
            }
        }
        strip = strip.child(item);
    }
    body = body.child(strip);

    match state.active_tab() {
        Some(ProjectTab::Overview) => body.child(overview_panel(state, t, cx)).into_any_element(),
        Some(ProjectTab::Tasks) => {
            let content = match (state.detail.as_ref(), board) {
                (Some(_), Some(board)) => div()
                    .id("project-task-board")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .child(tasks::board_view(board, drop_target, dragging, t, cx))
                    .into_any_element(),
                _ => placeholder::message(
                    "project-task-loading",
                    Icon::FolderKanban,
                    "Loading the project board",
                    "This project's board is read through the same pipeline as the Tasks section.",
                    t,
                )
                .into_any_element(),
            };
            body.child(content).into_any_element()
        }
        Some(ProjectTab::Doc(_)) => body.child(doc_body(state, t, cx)).into_any_element(),
        None => body.into_any_element(),
    }
}

fn overview_panel(state: &ProjectsState, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let Some(project) = &state.detail else {
        return div().into_any_element();
    };
    let archived = find_project(&state.tree, &project.id).is_none();

    let mut panel = div()
        .id("project-overview")
        .test_support()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p(px(20.))
        .flex()
        .flex_col()
        .gap(px(14.));

    // Title row: inline editor on double-click, otherwise the name.
    let editing_title = state
        .title_edit
        .as_ref()
        .is_some_and(|edit| edit.project_id == project.id);
    let mut title_row =
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(icon(Icon::FolderKanban, 18., t.text_muted));
    if editing_title {
        let input = state.title_edit.as_ref().map(|edit| edit.input.clone());
        if let Some(input) = input {
            title_row = title_row
                .child(div().flex_1().child(Input::new(&input)))
                .capture_key_down(cx.listener(|this, event, window, cx| {
                    this.handle_project_title_key(event, window, cx)
                }))
                .child(action_button(
                    "project-title-save",
                    "Save",
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.commit_project_title(window, cx)
                    }),
                ));
        }
    } else {
        let id = project.id.clone();
        let edit_id = project.id.clone();
        title_row = title_row
            .child(
                div()
                    .id("project-title-name")
                    .test_support()
                    .flex_1()
                    .text(Text::Heading)
                    .text_color(t.text)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.project_title_clicked(&id, window, cx)
                    }))
                    .child(project.name.clone()),
            )
            .child(tree_action(
                "project-title-edit",
                Icon::Pencil,
                t,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.begin_project_title_edit(&edit_id, window, cx)
                }),
            ));
    }
    if archived {
        title_row = title_row.child(action_button(
            "project-restore",
            "Restore",
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| this.restore_selected_project(cx)),
        ));
    } else {
        title_row = title_row.child(action_button(
            "project-archive",
            "Archive",
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| this.archive_selected_project(cx)),
        ));
    }
    title_row = title_row.child(action_button(
        "project-delete",
        "Delete",
        t,
        cx.listener(|this, _: &ClickEvent, _window, cx| this.ask_delete_selected_project(cx)),
    ));
    panel = panel.child(title_row);

    // Status chips.
    let mut status_row = div().flex().items_center().gap(px(6.)).child(
        div()
            .w(px(80.))
            .text(Text::Small)
            .text_color(t.text_muted)
            .child("Status"),
    );
    for (key, label) in PROJECT_STATUSES {
        let selected = project.status == key;
        status_row = status_row.child(view_chip(
            format!("project-status-{key}"),
            label,
            selected,
            t,
            cx.listener(move |this, _: &ClickEvent, _window, cx| this.set_project_status(key, cx)),
        ));
    }
    panel = panel.child(status_row);

    if let Some(editor) = &state.detail_editor {
        panel = panel
            .child(field_row(
                "project-identifier",
                "Identifier",
                &editor.identifier,
                t,
            ))
            .child(field_row("project-source", "Source", &editor.source, t))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(action_button(
                        "project-save-fields",
                        "Save fields",
                        t,
                        cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.save_project_fields(cx)
                        }),
                    ))
                    .child(
                        div()
                            .text(Text::Small)
                            .text_color(t.text_faint)
                            .child("Saving the identifier locks it against renames."),
                    ),
            );
    }

    if let Some(summary) = &state.summary {
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(format!("To do {}", summary.todo))
                .child(format!("In progress {}", summary.in_progress))
                .child(format!("Done {}", summary.done))
                .child(format!("Total {}", summary.total))
                .child(format!("Files {}", summary.file_count)),
        );
    }

    if let Some(editor) = &state.detail_editor {
        panel = panel
            .child(vim_strip(
                &editor.vim,
                "project-description-vim-toggle",
                "Esc normal · i insert",
                t,
                cx,
            ))
            .child(
                div()
                    .id("project-description")
                    .test_support()
                    .h(px(150.))
                    .rounded(px(t.radius))
                    .border_1()
                    .border_color(t.border)
                    .p(px(8.))
                    .child(Textarea::new(&editor.description).readonly(!editor.vim.is_insert()))
                    .capture_key_down(cx.listener(|this, event, window, cx| {
                        this.handle_project_description_key(event, window, cx)
                    })),
            )
            .child(action_button(
                "project-save-description",
                "Save description",
                t,
                cx.listener(|this, _: &ClickEvent, _window, cx| this.save_project_description(cx)),
            ));
    }

    panel = panel.child(documents_panel(state, project, t, cx));

    if let Some(confirm) = &state.confirm {
        panel = panel.child(confirm_bar(confirm, t, cx));
    }
    panel.into_any_element()
}

fn field_row(
    id: &'static str,
    label: &'static str,
    input: &Entity<InputState>,
    t: &Tokens,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .w(px(80.))
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(label),
        )
        .child(div().flex_1().child(Input::new(input)))
        .into_any_element()
}

fn action_button(
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
        .border_1()
        .border_color(t.border)
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

fn vim_strip(
    vim: &VimSession,
    toggle_id: &'static str,
    hint: &'static str,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let enabled = vim.is_enabled();
    let mut row = div()
        .id("project-mode")
        .test_support()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .id(toggle_id)
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
                hint
            } else {
                "click the badge for vim"
            }),
    )
    .into_any_element()
}

fn confirm_bar(confirm: &ProjectConfirm, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let verb = match confirm.kind {
        ProjectConfirmKind::DeleteProject => "Delete project",
        ProjectConfirmKind::DeleteFolder => "Delete folder",
    };
    div()
        .id("project-confirm")
        .test_support()
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
                .child(format!(
                    "{verb} \"{}\"? This cannot be undone.",
                    confirm.name
                )),
        )
        .child(action_button(
            "project-confirm-cancel",
            "Cancel",
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| this.cancel_project_confirm(cx)),
        ))
        .child(action_button(
            "project-confirm-yes",
            "Delete",
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| this.confirm_project_delete(cx)),
        ))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Project documents
// ---------------------------------------------------------------------------

fn documents_panel(
    state: &ProjectsState,
    project: &Project,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let mut panel = div()
        .id("project-documents")
        .test_support()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .text(Text::SmallStrong)
                        .text_color(t.text)
                        .child("Documents"),
                )
                .child(tree_action(
                    "project-doc-new-folder",
                    Icon::FolderPlus,
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.begin_project_doc_draft(DocDraftKind::Folder, window, cx)
                    }),
                ))
                .child(tree_action(
                    "project-doc-new-file",
                    Icon::FilePlus,
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.begin_project_doc_draft(DocDraftKind::File, window, cx)
                    }),
                )),
        );

    if let Some(draft) = &state.doc_draft {
        let label = match draft.kind {
            DocDraftKind::File => "New document (Markdown)",
            DocDraftKind::Folder => "New folder",
        };
        panel = panel.child(
            div()
                .id("project-doc-draft")
                .test_support()
                .flex()
                .flex_col()
                .gap(px(4.))
                .p(px(6.))
                .rounded(px(t.radius))
                .border_1()
                .border_color(t.border)
                .capture_key_down(cx.listener(|this, event, window, cx| {
                    this.handle_project_doc_draft_key(event, window, cx)
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

    if state.files.is_empty() {
        panel = panel.child(
            div()
                .py(px(6.))
                .text(Text::Small)
                .text_color(t.text_faint)
                .child("No documents yet."),
        );
    } else {
        panel = panel.child(div().flex().flex_col().children(doc_rows(
            &state.files,
            0,
            state,
            project,
            t,
            cx,
        )));
    }
    panel.into_any_element()
}

fn doc_rows(
    entries: &[FsEntry],
    depth: usize,
    state: &ProjectsState,
    _project: &Project,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    for entry in entries {
        let name = entry.name.clone();
        let is_dir = entry.is_dir;
        let rel = state.rel_path(&entry.path);
        let row_id = format!("project-doc-{}", entry.path.display());
        let mut row = div()
            .id(row_id.clone())
            .test_support()
            .w_full()
            .flex()
            .items_center()
            .gap(px(6.))
            .pl(px(8.0 + depth as f32 * 14.0))
            .pr(px(6.))
            .py(px(3.))
            .rounded(px(4.))
            .hover(|style| style.bg(t.hover))
            .child(icon(
                if is_dir { Icon::Folder } else { Icon::FileText },
                13.,
                t.text_muted,
            ))
            .child(
                div()
                    .flex_1()
                    .text(Text::Small)
                    .text_color(t.text)
                    .child(name),
            );
        if is_dir {
            let target = rel.clone().unwrap_or_default();
            let file_target = target.clone();
            row = row
                .child(tree_action(
                    format!("project-doc-new-file-{row_id}"),
                    Icon::FilePlus,
                    t,
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.begin_project_doc_draft_in(
                            file_target.clone(),
                            DocDraftKind::File,
                            window,
                            cx,
                        )
                    }),
                ))
                .child(tree_action(
                    format!("project-doc-new-folder-{row_id}"),
                    Icon::FolderPlus,
                    t,
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.begin_project_doc_draft_in(
                            target.clone(),
                            DocDraftKind::Folder,
                            window,
                            cx,
                        )
                    }),
                ));
        } else if let Some(rel) = rel.filter(|rel| is_text_doc(rel)) {
            let open = rel.clone();
            row = row.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_project_doc(open.clone(), window, cx)
            }));
        } else {
            row = row.child(
                div()
                    .text(Text::Small)
                    .text_color(t.text_faint)
                    .child("external"),
            );
        }
        rows.push(row.into_any_element());
        if is_dir && let Some(children) = &entry.children {
            rows.extend(doc_rows(children, depth + 1, state, _project, t, cx));
        }
    }
    rows
}

fn is_text_doc(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md" | "markdown" | "txt")
    )
}

fn doc_body(state: &ProjectsState, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let Some(tab) = state.active_doc() else {
        return div().into_any_element();
    };
    let mut body = div()
        .id("project-doc-body")
        .test_support()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col();
    body = body.child(
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(3.))
            .border_b_1()
            .border_color(t.divider)
            .child(vim_strip(
                &tab.vim,
                "project-doc-vim-toggle",
                "Esc normal · i insert · :w save · :q close",
                t,
                cx,
            ))
            .child(action_button(
                "project-doc-preview",
                if tab.preview { "Edit" } else { "Preview" },
                t,
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.toggle_project_doc_preview(window, cx)
                }),
            )),
    );

    if tab.preview {
        let text = tab.editor.read(cx).value().to_string();
        body = body.child(
            div()
                .id("project-doc-preview")
                .test_support()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p(px(20.))
                .child(markdown(text.as_str())),
        );
    } else {
        body = body
            .child(
                div()
                    .id("project-doc-editor")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .p(px(12.))
                    .child(Textarea::new(&tab.editor).readonly(!tab.vim.is_insert())),
            )
            .capture_key_down(cx.listener(|this, event, window, cx| {
                this.handle_project_doc_key(event, window, cx)
            }));
    }
    body.into_any_element()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn project(id: &str, name: &str, identifier: &str, folder: Option<&str>) -> Project {
        Project {
            id: id.to_string(),
            folder_id: folder.map(str::to_string),
            name: name.to_string(),
            identifier: identifier.to_string(),
            description: String::new(),
            source: String::new(),
            status: "active".to_string(),
            board_id: format!("board-{id}"),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn filter_matches_identifier_and_name() {
        let project = project("p1", "Apollo Launch", "apollo", None);
        assert!(project_matches(&project, ""));
        assert!(project_matches(&project, "apol"));
        assert!(project_matches(&project, "launch"));
        assert!(project_matches(&project, "  APOLLO "));
        assert!(!project_matches(&project, "zeus"));
    }

    #[test]
    fn insert_index_uses_the_post_removal_list() {
        let siblings = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(insert_index(&siblings, "a", false), 0);
        assert_eq!(insert_index(&siblings, "a", true), 1);
        assert_eq!(insert_index(&siblings, "c", false), 2);
        assert_eq!(insert_index(&siblings, "c", true), 3);
    }

    #[test]
    fn pinned_tabs_are_not_closable() {
        let closable = [false, false];
        assert_eq!(close_index(&closable, 0, 0), None);
        assert_eq!(close_index(&closable, 1, 1), None);
    }

    #[test]
    fn close_index_keeps_a_neighbour_selected() {
        let closable = [false, false, true, true];
        // Closing the active tab selects the one that shifts into its slot.
        assert_eq!(close_index(&closable, 2, 2), Some(2));
        assert_eq!(close_index(&closable, 3, 2), Some(2));
        assert_eq!(close_index(&closable, 0, 3), Some(0));
        // Closing the active last tab falls back to the previous one.
        assert_eq!(close_index(&closable, 3, 3), Some(2));
    }

    #[test]
    fn tasks_tab_exposes_the_project_board_to_the_shared_surface() {
        let mut state = ProjectsState::new();
        state.detail = Some(project("p1", "Apollo", "apollo", None));
        state.active = 1;
        assert_eq!(state.project_board_id(), Some("board-p1"));
        state.active = 0;
        assert_eq!(state.project_board_id(), None);
    }

    #[test]
    fn drag_groups_are_the_sibling_projects_of_one_folder() {
        let tree = vec![
            ProjectNode::Folder {
                id: "f1".to_string(),
                name: "Work".to_string(),
                children: vec![
                    ProjectNode::Project(project("p2", "Two", "two", Some("f1"))),
                    ProjectNode::Project(project("p3", "Three", "three", Some("f1"))),
                ],
            },
            ProjectNode::Project(project("p1", "One", "one", None)),
        ];
        assert_eq!(sibling_projects(&tree, None), vec!["p1"]);
        assert_eq!(sibling_projects(&tree, Some("f1")), vec!["p2", "p3"]);
        assert_eq!(project_folder(&tree, "p3").as_deref(), Some("f1"));
        assert!(project_folder(&tree, "p1").is_none());
        assert_eq!(folder_name(&tree, "f1").as_deref(), Some("Work"));
        assert!(folder_name(&tree, "missing").is_none());
    }
}
