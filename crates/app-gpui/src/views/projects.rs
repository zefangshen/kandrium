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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::base::text::markdown;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Editor, EditorState, InputState, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, CursorStyle, DragMoveEvent, Entity, Render,
    TestSupportExt, Window, div, px, relative, rgba,
};

use kandrium_core::BoardWithColumns;
use kandrium_core::fsutil::FsEntry;
use kandrium_core::projects::{
    PROJECT_STATUSES, Project, ProjectNode, ProjectPath, ProjectSummary, find_project,
};

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::{placeholder, tasks};
use crate::vim::VimSession;

/// Tree panel width when expanded, matching the other tree sections.
pub const TREE_W: f32 = 240.0;
/// Collapsed rail width.
pub const RAIL_W: f32 = 30.0;
/// Base left inset inside a tree row, after the 6 px pill margin. Chosen so
/// the row's leading control lines up with the header's 10 px text inset.
const TREE_BASE_INSET: f32 = 4.0;
/// Horizontal step per nesting level. The base moves; this does not.
const TREE_INDENT: f32 = 12.0;
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
    /// Name when the editor opened, restored by Esc.
    pub original: String,
    /// Name last written through core, so autosave never renames twice.
    pub saved: String,
    /// Bumped on every keystroke so a stale autosave can bow out.
    pub generation: u64,
}

/// Overview inputs: identifier, source, description. Rebuilt only when the
/// selected project changes or the user saves, never on an external reload.
pub struct DetailEditor {
    pub project_id: String,
    pub identifier: Entity<InputState>,
    pub description: Entity<TextareaState>,
    /// Description as persisted; autosave diffs against it.
    pub saved_description: String,
    /// Whether the description renders the Markdown preview instead.
    pub preview: bool,
    /// Bumped on every keystroke so a stale autosave can bow out.
    pub description_generation: u64,
    /// Identifier as persisted; Esc restores it and Save diffs against it.
    pub saved_identifier: String,
    /// Whether the identifier field differs from `saved_identifier`.
    pub identifier_dirty: bool,
    /// Inline validation message shown next to the identifier.
    pub identifier_error: Option<String>,
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
    pub editor: Entity<EditorState>,
    pub vim: VimSession,
    pub preview: bool,
    /// Text as last written to disk.
    pub saved: String,
    pub dirty: bool,
    /// Bumped on every real edit so a stale debounce can bow out.
    pub generation: u64,
}

// The document variant owns an editor and a vim session; the data-free
// variants are cheap either way, so boxing would only add indirection.
#[allow(clippy::large_enum_variant)]
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
    /// Folder references attached to the selected project.
    pub paths: Vec<ProjectPath>,
    pub summary: Option<ProjectSummary>,
    pub files: Vec<FsEntry>,
    pub filter: String,
    pub filter_input: Option<Entity<InputState>>,
    pub show_archived: bool,
    pub archived: Vec<Project>,
    /// Whether the footer's Archived disclosure shows its inline list.
    pub archived_open: bool,
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
    /// Projects whose documents are expanded in the tree (session state).
    pub docs_open: HashSet<String>,
    /// Document folders expanded inside an open project, keyed by
    /// `"<project id>/<absolute folder path>"`.
    pub doc_folders_open: HashSet<String>,
    /// Cached document tree per project, refreshed with the project list.
    pub doc_trees: HashMap<String, Vec<FsEntry>>,
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
            paths: Vec::new(),
            summary: None,
            files: Vec::new(),
            filter: String::new(),
            filter_input: None,
            show_archived: false,
            archived: Vec::new(),
            archived_open: false,
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
            docs_open: HashSet::new(),
            doc_folders_open: HashSet::new(),
            doc_trees: HashMap::new(),
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

/// Whether a node survives the filter. A project also survives when one of
/// its documents matches, mirroring how a folder survives on any matching
/// descendant — the document rows are filtered too, so only the hits show.
fn node_visible(node: &ProjectNode, filter: &str, state: &ProjectsState) -> bool {
    let needle = filter.trim().to_lowercase();
    match node {
        ProjectNode::Project(project) => {
            project_matches(project, filter)
                || state.doc_trees.get(&project.id).is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| doc_entry_matches(entry, &needle))
                })
        }
        ProjectNode::Folder { children, .. } => children
            .iter()
            .any(|child| node_visible(child, filter, state)),
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

/// Every project id in tree order, used to warm the per-project document
/// caches that the tree's in-place disclosure reads.
pub fn project_ids(nodes: &[ProjectNode]) -> Vec<String> {
    flat_projects(nodes)
        .into_iter()
        .map(|(project, _)| project.id.clone())
        .collect()
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
    drag: tasks::BoardDrag<'_>,
    t: &Tokens,
    window: &mut Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let mut row = div()
        .id("projects-view")
        .test_support()
        .size_full()
        .flex()
        .child(tree_panel(state, t, cx));

    if state.tree_open {
        // The splitter carries the only rule between the panel and the
        // content: it is the 5 px drag target with a 1 px line inside, so the
        // seam stays a single stroke while remaining full height.
        row = row.child(tree_splitter(t));
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
    .child(detail_pane(state, board, drag, t, window, cx))
    .into_any_element()
}

fn tree_splitter(t: &Tokens) -> AnyElement {
    div()
        .id("projects-tree-splitter")
        .test_support()
        // Visually 1 px, flush on both sides: the 5 px element is centred on
        // the rule with -2 px side margins, so its slack overlaps the two
        // neighbours instead of leaving an empty strip, and the resize target
        // stays comfortably wide.
        .w(px(5.))
        .mx(px(-2.))
        .h_full()
        .flex()
        .justify_center()
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

    // The rail keeps its own right rule (there is no splitter while closed),
    // so the open panel must not add a second one beside the splitter's line.
    let has_project = state.selected.is_some();
    let mut panel = div()
        .id("projects-tree")
        .test_support()
        .w(px(state.tree_w))
        .h_full()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(38.))
                .flex()
                .items_center()
                .gap(px(6.))
                .pl(px(10.))
                .pr(px(6.))
                .border_b_1()
                .border_color(t.divider)
                .child(
                    div()
                        .min_w(px(0.))
                        .flex_shrink(1.)
                        .truncate()
                        .text(Text::Label)
                        .text_color(t.text_muted)
                        .child("PROJECTS"),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text(Text::Micro)
                        .text_color(t.text_ghost)
                        .child(count_projects(&state.tree).to_string()),
                )
                .child(div().flex_1())
                // The four actions read left to right as project, folder,
                // file, upload. Only New project works without a selection;
                // the other three act inside the selected project, so they
                // dim (same box, same row height) until one is chosen.
                .child(header_action(
                    "projects-new-project",
                    Icon::FolderKanban,
                    "New project",
                    true,
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.begin_project_draft(ProjectDraftKind::Project, window, cx)
                    }),
                ))
                .child(header_action(
                    "projects-new-folder",
                    Icon::FolderPlus,
                    "New folder",
                    has_project,
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.begin_project_doc_draft(DocDraftKind::Folder, window, cx)
                    }),
                ))
                .child(header_action(
                    "projects-new-file",
                    Icon::FilePlus,
                    "New file",
                    has_project,
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.begin_project_doc_draft(DocDraftKind::File, window, cx)
                    }),
                ))
                .child(header_action(
                    "projects-upload",
                    Icon::Upload,
                    "Upload",
                    has_project,
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.upload_project_documents(window, cx)
                    }),
                ))
                // The collapse control is the last item, so it sits on the
                // panel's right edge whatever width the user drags the tree
                // to; the flex spacer above keeps the action group together.
                .child(tree_action(
                    "projects-collapse",
                    Icon::ChevronLeft,
                    t,
                    cx.listener(|this, _: &ClickEvent, _window, cx| this.toggle_projects_tree(cx)),
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
                .child(crate::views::field_input(&draft.input, t)),
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
        .overflow_y_scroll()
        .pb(px(8.));

    // The search sits at the top of the scrolling body, so it scrolls away
    // with the rows instead of staying pinned under the header.
    if let Some(input) = &state.filter_input {
        rows = rows.child(
            div()
                .id("projects-search")
                .test_support()
                .mx(px(6.))
                .mt(px(4.))
                .mb(px(6.))
                .child(crate::views::field_input(input, t).prefix(icon(
                    Icon::Search,
                    11.,
                    t.text_faint,
                ))),
        );
    }

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
                .filter(|node| node_visible(node, &state.filter, state))
                .cloned()
                .collect()
        };
        if visible.is_empty() && state.draft.is_none() {
            let message = if state.filter.trim().is_empty() {
                "No projects yet."
            } else {
                "No matching projects."
            };
            rows = rows.child(empty_row(message, t));
        }
        rows = rows.child(tree_rows(&visible, 0, state, t, cx));
    }

    panel = panel.child(rows);
    // The Archived row is the list's header, so it always sits directly above
    // the disclosure. Collapsed, the active rows are `flex_1` and the row
    // stays pinned to the bottom of the tree, where it reads as the tree's
    // footer entry and keeps its old position.
    panel = panel.child(archived_toggle_row(state, t, cx));
    if state.archived_open && !state.show_archived {
        panel = panel.child(archived_disclosure_list(state, t, cx));
    }
    panel.into_any_element()
}

/// The Archived disclosure's inline list. It shares the tree's remaining
/// height with the active rows (both are `flex_1`), so it always takes about
/// half the tree area and scrolls on its own.
fn archived_disclosure_list(
    state: &ProjectsState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let mut list = div()
        .id("projects-archived-list")
        .test_support()
        .min_h(px(0.))
        .py(px(4.))
        .when(!state.archived.is_empty(), |this| {
            this.flex_1().overflow_y_scroll()
        });
    if state.archived.is_empty() {
        list = list.child(empty_row("No archived projects.", t));
    }
    for project in &state.archived {
        list = list.child(archived_tree_row(project, state, t, cx));
    }
    list.into_any_element()
}

/// An archived project inside the disclosure list. It reads like a tree
/// project row: identifier label, full name on the label's tooltip, hover wash
/// and the selection pill. Selecting it opens the project the same way the
/// archived view does (`select_project`), which shows its Overview with the
/// Restore action.
fn archived_tree_row(
    project: &Project,
    state: &ProjectsState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let selected = state.selected.as_deref() == Some(project.id.as_str());
    let id = project.id.clone();
    let tooltip = project.name.clone();
    let label = if project.identifier.is_empty() {
        project.name.clone()
    } else {
        project.identifier.clone()
    };
    div()
        .id(format!("project-archived-tree-{}", project.id))
        .test_support()
        .relative()
        .mx(px(6.))
        .h(px(26.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(4.))
        .pl(px(TREE_BASE_INSET))
        .pr(px(8.))
        .cursor_pointer()
        .when(selected, |this| this.bg(t.surface_2))
        .when(selected, |this| this.child(selected_marker(t)))
        .hover(|style| style.bg(t.hover))
        // Same slot a project row reserves for its disclosure chevron, so the
        // icon and identifier line up with normal project rows at depth 0.
        .child(div().w(px(12.)))
        .child(icon(Icon::FolderColumns, 14., t.text_muted))
        .child(
            div()
                .id(format!("project-archived-name-{}", project.id))
                .test_support()
                .flex_shrink(1.)
                .min_w(px(0.))
                .truncate()
                .text(Text::Monospace)
                .text_color(t.text)
                .child(label)
                .tooltip(move |_window, cx| {
                    cx.new(|_| NameTooltip {
                        label: tooltip.clone(),
                    })
                    .into()
                }),
        )
        .on_click(
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.select_project(&id, window, cx)
            }),
        )
        .into_any_element()
}

/// Pinned footer entry for the archived projects.
///
/// A disclosure chevron on the left expands an inline archived list above the
/// row; the rest of the row keeps the old chip behaviour and switches the whole
/// tree into the archived view (`ProjectsState::show_archived`).
fn archived_toggle_row(
    state: &ProjectsState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let open = state.archived_open;
    div()
        .id("projects-archived-toggle")
        .test_support()
        .border_t_1()
        .border_color(t.divider)
        .py(px(6.))
        .child(
            div()
                .mx(px(6.))
                .h(px(26.))
                .flex()
                .items_center()
                .rounded(px(4.))
                // Discloses the inline archived list; the same 12 px glyph and
                // faint colour as the tree's own chevrons.
                .child(
                    div()
                        .id("projects-archived-chevron")
                        .test_support()
                        .h_full()
                        .pl(px(TREE_BASE_INSET))
                        .pr(px(4.))
                        .flex()
                        .items_center()
                        .rounded(px(3.))
                        .cursor_pointer()
                        .hover(|style| style.bg(t.pressed))
                        .child(icon(
                            if open {
                                Icon::ChevronDown
                            } else {
                                Icon::ChevronRight
                            },
                            12.,
                            t.text_faint,
                        ))
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.toggle_archived_disclosure(cx)
                        })),
                )
                .child(
                    // A header for the disclosure, not a control: only the
                    // chevron expands and collapses the list, so the label,
                    // icon and count take no click handling and no hover wash.
                    div()
                        .id("projects-archived-row")
                        .test_support()
                        .flex_1()
                        .h_full()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .pl(px(2.))
                        .pr(px(8.))
                        .child(icon(Icon::Archive, 14., t.text_faint))
                        .child(
                            div()
                                .flex_1()
                                .text(Text::Small)
                                .text_color(t.text_muted)
                                .child("Archived"),
                        )
                        .when(!state.archived.is_empty(), |this| {
                            this.child(
                                div()
                                    .flex_none()
                                    .text(Text::Micro)
                                    .text_color(t.text_ghost)
                                    .child(state.archived.len().to_string()),
                            )
                        }),
                ),
        )
        .into_any_element()
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
                        .filter(|child| node_visible(child, &state.filter, state))
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
                if state.docs_open.contains(&project.id)
                    && let Some(entries) = state.doc_trees.get(&project.id)
                {
                    for row in tree_doc_rows(&project.id, entries, depth + 1, state, t, cx) {
                        column = column.child(row);
                    }
                }
            }
        }
    }
    column.into_any_element()
}

/// 2 px accent marker pinned to the left edge of a selected tree row.
fn selected_marker(t: &Tokens) -> AnyElement {
    div()
        .absolute()
        .left(px(0.))
        .top(px(6.))
        .bottom(px(6.))
        .w(px(2.))
        .rounded(px(2.))
        .bg(t.accent)
        .into_any_element()
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
        .relative()
        .mx(px(6.))
        .h(px(26.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(4.))
        // 6 px pill margin + 4 px base = the header's own 10 px text inset,
        // so the tree's leading glyph lines up with "PROJECTS". The 12 px
        // chevron slot and the per-depth step below stay as they were.
        .pl(px(TREE_BASE_INSET + depth as f32 * TREE_INDENT))
        .pr(px(8.))
        .cursor_pointer()
        .when(selected, |this| this.bg(t.surface_2))
        .when(selected, |this| this.child(selected_marker(t)))
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
        .child(icon(Icon::Folder, 14., t.text_muted))
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
    // The row is labelled with the identifier, like the egui app; the full
    // name lives in the tooltip. An identifier-less project falls back to its
    // name so the row never renders empty.
    let label = if project.identifier.is_empty() {
        project.name.clone()
    } else {
        project.identifier.clone()
    };
    let drag_label = label.clone();
    // Whether the disclosure chevron is useful: a project with no documents
    // keeps the plain spacer instead of a control that would do nothing.
    let has_docs = state
        .doc_trees
        .get(&project.id)
        .is_some_and(|entries| !entries.is_empty());
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
        .relative()
        .mx(px(6.))
        .h(px(26.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(4.))
        // Same base inset and step as folder rows: a project's icon lands on
        // the folder icon's x at the same depth (the 12 px slot stands in for
        // the folder's chevron), and the identifier stays right-aligned.
        .pl(px(TREE_BASE_INSET + depth as f32 * TREE_INDENT))
        .pr(px(8.))
        .cursor_pointer()
        .when(selected, |this| this.bg(t.surface_2))
        .when(selected, |this| this.child(selected_marker(t)))
        .when(is_dragged, |this| this.opacity(0.3))
        .hover(|style| style.bg(t.hover))
        // The slot the row used to reserve for a folder-style chevron is a
        // real disclosure control here when the project has documents; with
        // none, the plain spacer keeps every row's icon on the same x.
        .child(if has_docs {
            let toggle_id = project.id.clone();
            let expanded = state.docs_open.contains(&project.id);
            div()
                .id(format!("project-docs-{}", project.id))
                .test_support()
                .w(px(12.))
                .h(px(12.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(2.))
                .cursor_pointer()
                .hover(move |style| style.bg(t.hover))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.toggle_project_docs(&toggle_id, cx)
                }))
                .child(icon(
                    if expanded {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    12.,
                    t.text_faint,
                ))
                .into_any_element()
        } else {
            div().w(px(12.)).into_any_element()
        })
        .child(icon(Icon::FolderColumns, 14., t.text_muted))
        .child(
            // The tooltip belongs to the label, not the row: it triggers on
            // the identifier's own bounds, so hovering the disclosure chevron
            // or the project icon does not pop the full name. The label is
            // content-sized (shrinking only when the text is too long), which
            // keeps the trigger region off the row's trailing padding.
            div()
                .id(format!("project-name-{}", project.id))
                .test_support()
                .flex_shrink(1.)
                .min_w(px(0.))
                .truncate()
                .text(Text::Monospace)
                .text_color(t.text)
                .child(label.clone())
                .tooltip(move |_window, cx| {
                    cx.new(|_| NameTooltip {
                        label: tooltip.clone(),
                    })
                    .into()
                }),
        )
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
                        label: drag_label.clone(),
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

/// A document row under an expanded project in the tree.
///
/// Mirrors the Overview document list — folders nest behind their own
/// chevron, Markdown files open a tab, other files are marked external — but
/// uses the tree's inset and step so it lines up with folder rows at the same
/// depth. While filtering, only rows whose subtree contains a hit survive and
/// folders open themselves.
fn tree_doc_rows(
    project_id: &str,
    entries: &[FsEntry],
    depth: usize,
    state: &ProjectsState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> Vec<AnyElement> {
    let filter = state.filter.trim().to_lowercase();
    let project_dir = state.root.as_ref().map(|root| root.join(project_id));
    let mut rows = Vec::new();
    for entry in entries {
        if !filter.is_empty() && !doc_entry_matches(entry, &filter) {
            continue;
        }
        let key = format!("{project_id}:{}", entry.path.display());
        let open = !filter.is_empty() || state.doc_folders_open.contains(&key);
        let rel = project_dir
            .as_ref()
            .and_then(|dir| entry.path.strip_prefix(dir).ok())
            .map(Path::to_path_buf);
        let name = div()
            .flex_1()
            .truncate()
            .text(Text::Small)
            .text_color(t.text)
            .child(entry.name.clone());
        let mut row = div()
            .id(format!("project-tree-doc-{key}"))
            .test_support()
            .mx(px(6.))
            .h(px(24.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(4.))
            .pl(px(TREE_BASE_INSET + depth as f32 * TREE_INDENT))
            .pr(px(8.))
            .cursor_pointer()
            .hover(|style| style.bg(t.hover));
        if entry.is_dir {
            let toggle = key.clone();
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
                .child(icon(Icon::Folder, 14., t.text_muted))
                .child(name)
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.toggle_project_doc_folder(toggle.clone(), cx)
                }));
        } else if let Some(rel) = rel.filter(|rel| is_text_doc(rel)) {
            let open_doc = rel.clone();
            row = row
                .child(div().w(px(12.)))
                .child(icon(Icon::FileText, 14., t.text_muted))
                .child(name)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_project_doc(open_doc.clone(), window, cx)
                }));
        } else {
            row = row
                .child(div().w(px(12.)))
                .child(icon(Icon::FileText, 14., t.text_muted))
                .child(name)
                .child(
                    div()
                        .text(Text::Micro)
                        .text_color(t.text_faint)
                        .child("external"),
                );
        }
        rows.push(row.into_any_element());
        if entry.is_dir
            && open
            && let Some(children) = &entry.children
        {
            rows.extend(tree_doc_rows(project_id, children, depth + 1, state, t, cx));
        }
    }
    rows
}

/// A document entry survives the filter when its own name matches or any
/// descendant does, so folders stay visible around a hit.
fn doc_entry_matches(entry: &FsEntry, needle: &str) -> bool {
    entry.name.to_lowercase().contains(needle)
        || entry.children.as_deref().is_some_and(|children| {
            children
                .iter()
                .any(|child| doc_entry_matches(child, needle))
        })
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
        .relative()
        .mx(px(6.))
        .h(px(26.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(4.))
        .pl(px(8.))
        .pr(px(8.))
        .cursor_pointer()
        .when(selected, |this| this.bg(t.surface_2))
        .when(selected, |this| this.child(selected_marker(t)))
        .hover(|style| style.bg(t.hover))
        .child(icon(Icon::FolderColumns, 14., t.text_faint))
        .child(
            div()
                .flex_1()
                .truncate()
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(project.name.clone()),
        )
        .child(
            div()
                .flex_none()
                .text(Text::Micro)
                .text_color(t.text_ghost)
                .child(project.identifier.clone()),
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

fn count_projects(nodes: &[ProjectNode]) -> usize {
    nodes
        .iter()
        .map(|node| match node {
            ProjectNode::Project(_) => 1,
            ProjectNode::Folder { children, .. } => count_projects(children),
        })
        .sum()
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

/// A tree-header action with a tooltip and a disabled state.
///
/// The box and padding match [`tree_action`], so adding an action never
/// changes the header's height or the 19 px rhythm. Disabled actions keep the
/// same box and glyph size; only the colour dims and the hover/click handlers
/// are dropped, so the row never reflows when a project is selected.
fn header_action(
    id: impl Into<gpui_kit::ElementId>,
    glyph: Icon,
    label: &'static str,
    enabled: bool,
    t: &Tokens,
    listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let el = div()
        .id(id)
        .test_support()
        .p(px(3.))
        .rounded(px(4.))
        .tooltip(move |_window, cx| {
            cx.new(|_| NameTooltip {
                label: label.to_string(),
            })
            .into()
        });
    let el = if enabled {
        el.cursor_pointer()
            .hover(|style| style.bg(t.hover))
            .on_click(listener)
            .child(icon(glyph, 13., t.text_muted))
    } else {
        el.child(icon(glyph, 13., t.text_ghost))
    };
    el.into_any_element()
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

/// Small floating label used as a row/tab tooltip.
pub(crate) struct NameTooltip {
    pub(crate) label: String,
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
    drag: tasks::BoardDrag<'_>,
    t: &Tokens,
    window: &Window,
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

    let mut strip = crate::views::tab_strip(t);
    // Pinned pages and open documents share the one segment row; the pinned
    // pair never shrinks or scrolls, the documents scroll inside their own
    // lane once the row runs out of width.
    let mut pinned: Vec<AnyElement> = Vec::new();
    let mut documents: Vec<AnyElement> = Vec::new();
    for (index, tab) in state.tabs.iter().enumerate() {
        let active = state.active == index;
        let (label, closable) = match tab {
            ProjectTab::Overview => ("Overview".to_string(), false),
            ProjectTab::Tasks => ("Tasks".to_string(), false),
            ProjectTab::Doc(doc) => (doc.name.clone(), true),
        };
        let mut item = crate::views::pane_tab(format!("project-tab-{index}"), active, t)
            .test_support()
            .cursor_pointer()
            .hover(|style| style.bg(t.hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.activate_project_tab(index, cx)
            }));
        if matches!(tab, ProjectTab::Doc(_)) {
            item = item.child(icon(Icon::FileText, 12., t.text_faint));
        }
        item = item.child(
            div()
                .text(Text::Small)
                .text_color(if active { t.text } else { t.text_faint })
                .child(label),
        );
        if let ProjectTab::Doc(doc) = tab {
            if doc.dirty {
                item = item.child(div().size(px(3.)).rounded(px(2.)).bg(t.accent));
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
                        .child(icon(Icon::X, 10., t.text_faint)),
                );
            }
        }
        if index < 2 {
            pinned.push(item.into_any_element());
        } else {
            documents.push(item.into_any_element());
        }
    }
    strip = strip.child(
        div()
            .flex()
            .items_stretch()
            .flex_shrink_0()
            .children(pinned),
    );
    if !documents.is_empty() {
        strip = strip.child(
            div()
                .id("project-tab-documents")
                .test_support()
                .flex()
                .items_stretch()
                .flex_shrink(1.)
                .min_w(px(0.))
                .overflow_x_scroll()
                .children(documents),
        );
    }
    strip = strip.child(div().flex_1());
    body = body.child(strip);

    match state.active_tab() {
        Some(ProjectTab::Overview) => body
            .child(overview_panel(state, t, window, cx))
            .into_any_element(),
        Some(ProjectTab::Tasks) => {
            let content = match (state.detail.as_ref(), board) {
                (Some(_), Some(board)) => div()
                    .id("project-task-board")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    // Project boards create cards through the card modal, so
                    // the board surface never shows an add row here.
                    .child(tasks::board_view(
                        board,
                        drag,
                        tasks::AddCard {
                            allowed: false,
                            form: None,
                        },
                        "",
                        None,
                        t,
                        cx,
                    ))
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
        Some(ProjectTab::Doc(_)) => body
            .child(doc_body(state, t, window, cx))
            .into_any_element(),
        None => body.into_any_element(),
    }
}

fn overview_panel(
    state: &ProjectsState,
    t: &Tokens,
    window: &Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let Some(project) = &state.detail else {
        return div().into_any_element();
    };
    let archived = find_project(&state.tree, &project.id).is_none();

    // One measure for the whole page: the panel caps at 880 px and every block
    // below fills its 26 px-inset content box, so their right edges line up.
    let mut panel = div()
        .id("project-overview")
        .test_support()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(26.))
        .pt(px(22.))
        .pb(px(16.))
        .flex()
        .flex_col()
        .gap(px(15.))
        .max_w(px(880.));

    // Title row: a plain label that turns into the inline editor on
    // double-click. There is no edit button; the field autosaves while typing
    // and commits on Enter or blur, while Esc restores the previous name.
    let editing_title = state
        .title_edit
        .as_ref()
        .is_some_and(|edit| edit.project_id == project.id);
    let mut title_row = div().flex().items_center().gap(px(9.));
    if editing_title {
        let input = state.title_edit.as_ref().map(|edit| edit.input.clone());
        if let Some(input) = input {
            title_row = title_row
                .child(div().flex_1().child(crate::views::field_input(&input, t)))
                .capture_key_down(cx.listener(|this, event, window, cx| {
                    this.handle_project_title_key(event, window, cx)
                }));
        }
    } else {
        let id = project.id.clone();
        title_row = title_row.child(
            div()
                .id("project-title-name")
                .test_support()
                .flex_1()
                .text(Text::Display)
                .text_color(t.text)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.project_title_clicked(&id, window, cx)
                }))
                .child(project.name.clone()),
        );
    }
    // Archived projects keep their Restore action here; Archive and Delete
    // live at the bottom of the page.
    if archived {
        title_row = title_row.child(action_button(
            "project-restore",
            "Restore",
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| this.restore_selected_project(cx)),
        ));
    }
    // Title block: the title owns its row and the identifier group sits
    // directly beneath it at the same left edge.
    let mut title_block = div()
        .id("project-title-block")
        .test_support()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(title_row);
    // The Overview scrolls as one page, so a flexed field inside it would
    // collapse. Size the description from the window instead: it grows on tall
    // windows, shrinks on short ones, and the page keeps its scroll and pinned
    // Archive/Delete pair.
    let desc_h = (f32::from(window.viewport_size().height) * 0.32).clamp(180., 420.);
    if let Some(editor) = &state.detail_editor {
        // Identifier: a normal field sized for about 24 monospace characters.
        // It only writes through the Save button, which appears while the
        // value differs from the persisted one. The validation message sits on
        // its own line below the field, not inside the row.
        let mut identifier_row = div()
            .id("project-identifier")
            .test_support()
            .flex()
            .items_center()
            .gap(px(12.))
            .capture_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.cancel_project_identifier(window, cx);
                    }
                }),
            )
            .child(
                div()
                    .w(px(92.))
                    .text(Text::Small)
                    .text_color(t.text_faint)
                    .child("Identifier"),
            )
            .child(
                div()
                    .w(px(210.))
                    .child(crate::views::field_input(&editor.identifier, t)),
            );
        if editor.identifier_dirty {
            identifier_row = identifier_row.child(action_button(
                "project-identifier-save",
                "Save",
                t,
                cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.save_project_identifier(window, cx)
                }),
            ));
        }
        title_block = title_block.child(identifier_row);
        if let Some(error) = &editor.identifier_error {
            title_block = title_block.child(
                div()
                    .id("project-identifier-error")
                    .test_support()
                    // Aligns under the field: caption width plus the row gap.
                    .pl(px(92. + 12.))
                    .text(Text::Small)
                    .text_color(t.danger)
                    .child(error.clone()),
            );
        }
    }
    panel = panel.child(title_block);

    // Status: one three-option segmented control, the same control language as
    // the Edit | Preview switch.
    let mut status_row = div().flex().items_center().gap(px(12.)).child(
        div()
            .w(px(92.))
            .text(Text::Small)
            .text_color(t.text_faint)
            .child("Status"),
    );
    let mut status_group = div()
        .flex()
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(7.))
        .bg(t.hover);
    for (key, label) in PROJECT_STATUSES {
        status_group = status_group.child(crate::views::segmented_option(
            format!("project-status-{key}"),
            label,
            project.status == key,
            false,
            t,
            cx.listener(move |this, _: &ClickEvent, _window, cx| this.set_project_status(key, cx)),
        ));
    }
    status_row = status_row.child(status_group);
    panel = panel.child(status_row);

    panel = panel.child(folders_list(state, t, cx));

    if let Some(summary) = &state.summary {
        let cells = [
            (summary.todo, "TO DO"),
            (summary.in_progress, "IN PROGRESS"),
            (summary.done, "DONE"),
            (summary.total, "TOTAL"),
            (summary.file_count as i64, "FILES"),
        ];
        let mut strip = div()
            .id("project-summary")
            .test_support()
            .flex()
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.card)
            .overflow_hidden();
        for (index, (value, label)) in cells.into_iter().enumerate() {
            let mut cell = div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(13.))
                .pt(px(9.))
                .pb(px(10.));
            if index < cells.len() - 1 {
                cell = cell.border_r_1().border_color(t.border);
            }
            strip = strip.child(
                cell.child(
                    div()
                        .text(Text::Metric)
                        .text_color(t.text)
                        .child(value.to_string()),
                )
                .child(
                    div()
                        .text(Text::Label)
                        .text_color(t.text_faint)
                        .child(label),
                ),
            );
        }
        panel = panel.child(strip);
    }

    if let Some(editor) = &state.detail_editor {
        // The description autosaves (debounced, flushed on blur and on leaving
        // the project or section) and has no vim mode and no Save button: the
        // field is the only surface. The compact Edit | Preview switch is
        // right-aligned on the row above the field, so its trailing edge lines
        // up with the field's.
        let preview = editor.preview;
        let description_text = editor.description.read(cx).value().to_string();
        panel = panel
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .w(px(92.))
                            .text(Text::Small)
                            .text_color(t.text_faint)
                            .child("Description"),
                    )
                    .child(div().flex_1())
                    .child(crate::views::mode_switch_compact(
                        "project-description-toggle",
                        preview,
                        t,
                        cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.set_project_description_preview(false, cx)
                        }),
                        cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.set_project_description_preview(true, cx)
                        }),
                    )),
            )
            .child(
                div()
                    .id("project-description")
                    .test_support()
                    .h(px(desc_h))
                    .when(preview, |this| {
                        this.child(
                            div()
                                .id("project-description-preview")
                                .test_support()
                                .w_full()
                                .h(px(desc_h))
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
                            crate::views::field_textarea(&editor.description, t).h(px(desc_h)),
                        )
                    }),
            );
    }

    panel = panel.child(documents_panel(state, project, archived, t, cx));

    // Archive and Delete sit at the bottom of the page. The spacer above them
    // absorbs the leftover height on short pages so the pair never floats
    // mid-page; it collapses to nothing once the content overflows.
    panel = panel.child(div().flex_1().min_h(px(12.)));
    let mut actions = div()
        .id("project-actions")
        .test_support()
        .flex()
        .items_center()
        .gap(px(8.));
    if !archived {
        actions = actions.child(action_button(
            "project-archive",
            "Archive",
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| this.archive_selected_project(cx)),
        ));
    }
    actions = actions.child(action_button_danger(
        "project-delete",
        "Delete",
        t,
        cx.listener(|this, _: &ClickEvent, _window, cx| this.ask_delete_selected_project(cx)),
    ));
    panel = panel.child(actions);

    if let Some(confirm) = &state.confirm {
        panel = panel.child(confirm_bar(confirm, t, cx));
    }
    panel.into_any_element()
}

/// Folder references attached to the selected project.
///
/// Each row shows one path (middle-ellipsized so long paths never wrap), a
/// remove button, and an "Add folder" action that opens a native picker.
/// Removing a row only drops the reference; the folder on disk is untouched.
fn folders_list(state: &ProjectsState, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let mut column = div().flex().items_start().gap(px(12.)).child(
        div()
            .w(px(92.))
            .pt(px(4.))
            .text(Text::Small)
            .text_color(t.text_faint)
            .child("Folders"),
    );
    let mut rows = div()
        .id("project-folders")
        .test_support()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .gap(px(4.));
    if state.paths.is_empty() {
        // One actionable line rather than a bare button under a blank slot.
        rows = rows.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .min_h(px(26.))
                .child(
                    div()
                        .text(Text::Small)
                        .text_color(t.text_faint)
                        .child("No folders yet"),
                )
                .child(action_button(
                    "project-folder-add",
                    "Add folder",
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.add_project_folder(window, cx)
                    }),
                )),
        );
    } else {
        for path in &state.paths {
            let path_id = path.id.clone();
            rows = rows.child(
                div()
                    .id(format!("project-folder-{}", path.id))
                    .test_support()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(t.radius))
                    .bg(t.card)
                    .child(icon(Icon::Folder, 12., t.text_faint))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text(Text::Monospace)
                            .text_color(t.text_muted)
                            .child(ellipsize_middle(&path.path, 64)),
                    )
                    .tooltip({
                        let full = path.path.clone();
                        move |_window, cx| {
                            cx.new(|_| NameTooltip {
                                label: full.clone(),
                            })
                            .into()
                        }
                    })
                    .child(
                        div()
                            .id(format!("project-folder-remove-{}", path.id))
                            .p(px(2.))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|style| style.bg(t.pressed))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                this.remove_project_folder(&path_id, cx)
                            }))
                            .child(icon(Icon::X, 11., t.text_faint)),
                    ),
            );
        }
        rows = rows.child(action_button(
            "project-folder-add",
            "Add folder",
            t,
            cx.listener(|this, _: &ClickEvent, window, cx| this.add_project_folder(window, cx)),
        ));
    }
    column = column.child(rows);
    column.into_any_element()
}

/// Shorten a path for display by removing characters from the middle, keeping
/// the beginning and end readable. Full paths are available in the tooltip.
pub(crate) fn ellipsize_middle(path: &str, max: usize) -> String {
    let count = path.chars().count();
    if count <= max {
        return path.to_string();
    }
    let keep = (max.saturating_sub(1)) / 2;
    let head: String = path.chars().take(keep).collect();
    let tail: String = path
        .chars()
        .skip(count.saturating_sub(max.saturating_sub(1) - keep))
        .collect();
    format!("{head}…{tail}")
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

/// Destructive variant of [`action_button`]: same box, danger text.
fn action_button_danger(
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
        .child(div().text(Text::Small).text_color(t.danger).child(label))
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
    archived: bool,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let editable = !archived;
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
                // Same order as the tree header: folder, file, upload.
                .child(header_action(
                    "project-doc-new-folder",
                    Icon::FolderPlus,
                    "New folder",
                    editable,
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.begin_project_doc_draft(DocDraftKind::Folder, window, cx)
                    }),
                ))
                .child(header_action(
                    "project-doc-new-file",
                    Icon::FilePlus,
                    "New file",
                    editable,
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.begin_project_doc_draft(DocDraftKind::File, window, cx)
                    }),
                ))
                .child(header_action(
                    "project-doc-upload",
                    Icon::Upload,
                    "Upload",
                    editable,
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.upload_project_documents(window, cx)
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
                .child(crate::views::field_input(&draft.input, t)),
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
            .h(px(26.))
            .flex()
            .items_center()
            .gap(px(7.))
            .pl(px(8.0 + depth as f32 * 14.0))
            .pr(px(8.))
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
                    .min_w(px(0.))
                    .overflow_hidden()
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
            row = row.child(doc_tag("MD", t)).on_click(cx.listener(
                move |this, _: &ClickEvent, window, cx| {
                    this.open_project_doc(open.clone(), window, cx)
                },
            ));
        } else {
            row = row.child(doc_tag("EXTERNAL", t));
        }
        rows.push(row.into_any_element());
        if is_dir && let Some(children) = &entry.children {
            rows.extend(doc_rows(children, depth + 1, state, _project, t, cx));
        }
    }
    rows
}

/// Trailing tag on a document row, right-aligned after the flexible name.
fn doc_tag(label: &'static str, t: &Tokens) -> AnyElement {
    div()
        .flex_none()
        .text(Text::Micro)
        .text_color(t.text_ghost)
        .child(label)
        .into_any_element()
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

fn doc_body(
    state: &ProjectsState,
    t: &Tokens,
    window: &Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
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
    let tab_preview = tab.preview;
    body = body.child(
        div()
            .h(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(14.))
            .border_b_1()
            .border_color(t.divider)
            .child(crate::views::path_label(
                "project-doc-path",
                tab.rel.display().to_string(),
                t,
                cx,
            ))
            .child(div().flex_1())
            .child(crate::views::vim_indicator("project-doc-vim", &tab.vim, t))
            .child(crate::views::mode_switch(
                "project-doc-preview",
                tab.preview,
                t,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if tab_preview {
                        this.toggle_project_doc_preview(window, cx);
                    }
                }),
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    if !tab_preview {
                        this.toggle_project_doc_preview(window, cx);
                    }
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
        let lines = tab.editor.read(cx).value().lines().count();
        let (mono_family, mono_size) = {
            let theme = cx.theme();
            (theme.mono_font_family.clone(), theme.mono_font_size)
        };
        body = body
            .child(
                div()
                    .id("project-doc-editor")
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
                                    // Multi-line inputs auto-size to their
                                    // content; pin the editor to the wrapper.
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
                                    )
                                    .absolute()
                                    .inset_0(),
                                )
                            })
                            .child(crate::views::gutter_rule(lines, t, window)),
                    ),
            )
            .capture_key_down(cx.listener(|this, event, window, cx| {
                this.handle_project_doc_key(event, window, cx)
            }));
    }
    body = body.child(doc_status_bar(tab, t, cx));
    body.into_any_element()
}

/// The project document's bottom status bar, matching Notes and Documents.
fn doc_status_bar(tab: &ProjectDocTab, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
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
        "project-doc-status",
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
