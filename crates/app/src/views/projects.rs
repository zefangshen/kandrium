//! Projects view — left DB-backed tree (folders + projects), right project
//! overview.
//!
//! Projects live in SQLite (`kandrium_core::projects`), each owning a private
//! board and an on-disk document folder under the projects root. The right
//! pane is an overview (metadata, description, documents), not an editor:
//! files are opened/revealed externally.
//!
//! The `Arc<Mutex<Connection>>` is passed in (not a live guard) so the lock is
//! taken only inside action handlers, never held across a frame — the watcher
//! thread polls `PRAGMA data_version` on its own connection.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::Rect;
use egui_commonmark::CommonMarkCache;
use kandrium_core::fsutil::FsEntry;
use kandrium_core::projects::{
    self, PROJECT_STATUSES, Project, ProjectChoice, ProjectNode, ProjectSummary, TEMPLATES,
    project_dir,
};
use rusqlite::Connection;

use crate::theme::{self, Press, Tokens};
use crate::views::notes_highlight::MarkdownHighlighter;
use crate::widgets;
use crate::widgets::markdown::{self, NoteMode};
use crate::widgets::tabs::TAB_H;

const AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const TREE_DEFAULT_W: f32 = 240.0;
/// Breathing room between the tree panel and the detail pane; matches the
/// page's own 12 px content inset.
pub const PANE_GAP: f32 = 12.0;
const ROW_H: f32 = 26.0;
const INDENT: f32 = 14.0;
const DESC_FIELD_HEIGHT: f32 = 120.0;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One open inline-rename session (folder or project).
struct Rename {
    is_folder: bool,
    id: String,
    draft: String,
}

/// Overview title inline editor. `project_id` ties the draft to a project so
/// switching selection drops it; `focus` requests keyboard focus once.
struct TitleEdit {
    project_id: String,
    draft: String,
    focus: bool,
}

/// Which kind of entry the Documents tree's inline create row will make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocCreateKind {
    File,
    Folder,
}

/// Inline "new file/folder" draft in the Documents tree.
struct DocCreate {
    kind: DocCreateKind,
    /// Absolute folder the entry is created in (inside the project directory).
    parent: PathBuf,
    name: String,
    /// Request keyboard focus on the next frame.
    focus: bool,
}

/// Which surface the detail pane shows. The app draws the Board itself (it
/// owns the kanban pipeline); the view renders the other two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailKind {
    Overview,
    Board,
    Doc,
}

/// One open document tab: project-relative path plus its live buffer.
struct DocTab {
    rel: PathBuf,
    text: String,
    mode: NoteMode,
    dirty_since: Option<Instant>,
}

enum DetailTab {
    /// Pinned first tab: the project overview.
    Overview,
    Board,
    Doc(DocTab),
}

/// Tab strip + document buffers for the selected project's detail pane.
struct DetailTabs {
    tabs: Vec<DetailTab>,
    active: usize,
    /// Project the tabs belong to; a selection change resets them.
    project_id: Option<String>,
    highlighter: Option<MarkdownHighlighter>,
    highlighter_dark: bool,
    preview_cache: CommonMarkCache,
    error: Option<String>,
}

impl DetailTabs {
    fn new() -> Self {
        Self {
            tabs: vec![DetailTab::Overview, DetailTab::Board],
            active: 0,
            project_id: None,
            highlighter: None,
            highlighter_dark: true,
            preview_cache: CommonMarkCache::default(),
            error: None,
        }
    }

    fn active_kind(&self) -> DetailKind {
        match self.tabs.get(self.active) {
            Some(DetailTab::Board) => DetailKind::Board,
            Some(DetailTab::Doc(_)) => DetailKind::Doc,
            _ => DetailKind::Overview,
        }
    }

    fn active_doc_index(&self) -> Option<usize> {
        matches!(self.tabs.get(self.active), Some(DetailTab::Doc(_))).then_some(self.active)
    }

    /// Keep the syntax highlighter bound to the current theme.
    fn sync_theme(&mut self, dark: bool) {
        if self.highlighter.is_none() || self.highlighter_dark != dark {
            self.highlighter = Some(MarkdownHighlighter::new(dark));
            self.highlighter_dark = dark;
        }
    }

    /// Flush pending writes, then reset for a (possibly different) project.
    /// Document tabs belong to their project and are dropped; the pinned
    /// Overview/Tasks tabs are always rebuilt, and an active Tasks tab carries
    /// over so browsing projects stays on the board.
    fn enter_project(&mut self, root: &Path, id: Option<&str>) -> bool {
        if self.project_id.as_deref() == id {
            return true;
        }
        if !self.flush_all(root) {
            return false;
        }
        self.project_id = id.map(str::to_string);
        let keep_tasks = id.is_some() && self.active_kind() == DetailKind::Board;
        self.tabs = vec![DetailTab::Overview, DetailTab::Board];
        self.active = if keep_tasks { 1 } else { 0 };
        self.error = None;
        true
    }

    /// Focus the tab for `rel`, reading the file into a new tab if needed.
    fn open_doc(&mut self, root: &Path, rel: &Path) {
        if let Some(idx) = self
            .tabs
            .iter()
            .position(|t| matches!(t, DetailTab::Doc(d) if d.rel == rel))
        {
            self.active = idx;
            return;
        }
        let Some(id) = self.project_id.clone() else {
            return;
        };
        match projects::read_doc(root, &id, rel) {
            Ok(text) => {
                self.tabs.push(DetailTab::Doc(DocTab {
                    rel: rel.to_path_buf(),
                    text,
                    mode: NoteMode::Edit,
                    dirty_since: None,
                }));
                self.active = self.tabs.len() - 1;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("Cannot open document: {e}")),
        }
    }

    /// Close a document tab; the pinned Overview and Tasks tabs ignore this.
    fn close(&mut self, root: &Path, idx: usize) {
        if idx < 2 || !matches!(self.tabs.get(idx), Some(DetailTab::Doc(_))) {
            return;
        }
        if !self.flush_doc(root, idx) {
            return;
        }
        self.tabs.remove(idx);
        if self.active > idx {
            self.active -= 1;
        } else if self.active == idx {
            self.active = idx.saturating_sub(1);
        }
    }

    /// Write one dirty document tab back to disk. Returns false when a save
    /// failed and the tab must stay open/dirty for a retry.
    fn flush_doc(&mut self, root: &Path, idx: usize) -> bool {
        let Some(id) = self.project_id.clone() else {
            return true;
        };
        let Some(DetailTab::Doc(doc)) = self.tabs.get_mut(idx) else {
            return true;
        };
        if doc.dirty_since.is_none() {
            return true;
        }
        match projects::write_doc(root, &id, &doc.rel, &doc.text) {
            Ok(()) => {
                doc.dirty_since = None;
                true
            }
            Err(e) => {
                // Keep the buffer dirty and throttle the next autosave
                // attempt instead of silently dropping the edit.
                doc.dirty_since = Some(Instant::now());
                self.error = Some(format!("Cannot save document: {e}"));
                false
            }
        }
    }

    fn flush_all(&mut self, root: &Path) -> bool {
        let mut ok = true;
        for idx in 0..self.tabs.len() {
            ok &= self.flush_doc(root, idx);
        }
        ok
    }

    /// Flush documents whose debounce elapsed. Returns true while any tab
    /// stays dirty so the caller can keep scheduling repaints.
    fn tick_autosave(&mut self, root: &Path) -> bool {
        for idx in 0..self.tabs.len() {
            let due = match &self.tabs[idx] {
                DetailTab::Doc(doc) => doc
                    .dirty_since
                    .is_some_and(|t| t.elapsed() >= AUTOSAVE_DEBOUNCE),
                _ => false,
            };
            if due {
                self.flush_doc(root, idx);
            }
        }
        self.tabs
            .iter()
            .any(|t| matches!(t, DetailTab::Doc(doc) if doc.dirty_since.is_some()))
    }
}

pub struct ProjectsState {
    root: PathBuf,
    tree: Vec<ProjectNode>,
    expanded: HashSet<String>,
    /// Project ids whose documents are shown inline in the left tree
    /// (persisted across restarts).
    docs_expanded: HashSet<String>,
    /// Cached document trees for the left tree, keyed by project id.
    tree_docs: HashMap<String, Vec<FsEntry>>,
    /// Expanded document folders in the left tree (session-only).
    tree_doc_expanded: HashSet<PathBuf>,
    /// Selected project id (right pane).
    selected: Option<String>,
    /// Folder new items are created into (`None` = tree root).
    selected_folder: Option<String>,
    renaming: Option<Rename>,
    filter: String,
    files: Vec<FsEntry>,
    summary: Option<ProjectSummary>,
    detail: Option<Project>,
    /// Detail pane tabs (overview / board / documents).
    tabs: DetailTabs,
    /// Left tree panel visibility (persisted across restarts).
    pub tree_open: bool,
    /// Overview Documents section body visibility (persisted across restarts).
    pub docs_open: bool,
    /// Expanded folders in the overview Documents tree (absolute paths).
    doc_expanded: HashSet<PathBuf>,
    /// Folder new documents are created into (absolute; project root default).
    doc_selected_dir: PathBuf,
    /// Inline create draft, if the tree is asking for a name.
    doc_create: Option<DocCreate>,
    /// Project the Documents tree expansion/selection belongs to; a selection
    /// change resets them.
    doc_tree_project: Option<String>,
    /// Pending debounced description save.
    desc_dirty: Option<Instant>,
    error: Option<String>,
    /// Overview title inline editor (double-click to rename).
    title_edit: Option<TitleEdit>,
    /// Live buffer for the Overview identifier field. `identifier_for` ties it
    /// to a project id so switching selection re-seeds it.
    identifier_draft: String,
    identifier_for: Option<String>,
    identifier_error: Option<String>,
}

/// A tree row asked to delete something; the app routes it to the confirm
/// modal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeleteKind {
    Project { id: String, name: String },
    Folder { id: String, name: String },
}

#[derive(Default)]
pub struct ProjectsActions {
    pub request_delete: Option<DeleteKind>,
    /// Open this project's board in the Tasks section.
    pub open_board: Option<String>,
}

/// Deferred DB mutation collected during rendering and applied after the
/// panels are drawn (the DB lock is never held across widget code).
enum Mutation {
    Refresh,
    CreateFolder {
        parent: Option<String>,
        name: String,
    },
    CreateProject {
        folder: Option<String>,
        name: String,
        template: &'static str,
    },
    RenameFolder {
        id: String,
        name: String,
    },
    RenameProject {
        id: String,
        name: String,
    },
    SetStatus {
        id: String,
        status: String,
    },
    SetDescription {
        id: String,
        description: String,
    },
    SetIdentifier {
        id: String,
        identifier: String,
    },
}

impl ProjectsState {
    pub fn new() -> Self {
        match projects::ensure_root() {
            Ok(root) => Self::empty(root, None),
            Err(e) => Self::empty(
                projects::resolve_projects_root(),
                Some(format!("Cannot open projects folder: {e}")),
            ),
        }
    }

    fn empty(root: PathBuf, error: Option<String>) -> Self {
        ProjectsState {
            root,
            tree: Vec::new(),
            expanded: HashSet::new(),
            docs_expanded: HashSet::new(),
            tree_docs: HashMap::new(),
            tree_doc_expanded: HashSet::new(),
            selected: None,
            selected_folder: None,
            renaming: None,
            filter: String::new(),
            files: Vec::new(),
            summary: None,
            detail: None,
            tabs: DetailTabs::new(),
            tree_open: true,
            docs_open: true,
            doc_expanded: HashSet::new(),
            doc_selected_dir: PathBuf::new(),
            doc_create: None,
            doc_tree_project: None,
            desc_dirty: None,
            error,
            title_edit: None,
            identifier_draft: String::new(),
            identifier_for: None,
            identifier_error: None,
        }
    }

    /// Projects root (for delete/create guards and reveal-in-Finder).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Selected project id (persisted session).
    pub fn selected_id(&self) -> Option<String> {
        self.selected.clone()
    }

    /// Board owned by the selected project, if any. Falls back to the tree
    /// while the detail row is reloading after a selection change.
    pub fn selected_board_id(&self) -> Option<String> {
        let id = self.selected.as_ref()?;
        if let Some(detail) = self.detail.as_ref().filter(|d| &d.id == id) {
            return Some(detail.board_id.clone());
        }
        projects::find_project(&self.tree, id).map(|p| p.board_id.clone())
    }

    /// Flattened project list for the card modal's project picker.
    pub fn project_choices(&self) -> Vec<ProjectChoice> {
        fn walk(nodes: &[ProjectNode], out: &mut Vec<ProjectChoice>) {
            for node in nodes {
                match node {
                    ProjectNode::Folder { children, .. } => walk(children, out),
                    ProjectNode::Project(project) => out.push(ProjectChoice {
                        id: project.id.clone(),
                        name: project.name.clone(),
                        identifier: project.identifier.clone(),
                    }),
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.tree, &mut out);
        out
    }

    /// True when the detail pane is showing the embedded board, i.e. the app
    /// should load/render the selected project's board.
    pub fn detail_tab_is_board(&self) -> bool {
        self.detail.is_some() && self.tabs.active_kind() == DetailKind::Board
    }

    /// Expanded folder ids (persisted session).
    pub fn expanded_ids(&self) -> Vec<String> {
        self.expanded.iter().cloned().collect()
    }

    /// Project ids whose documents are expanded in the left tree (persisted).
    pub fn docs_expanded_ids(&self) -> Vec<String> {
        self.docs_expanded.iter().cloned().collect()
    }

    /// Restore a previous session's selection + expansion.
    pub fn restore_session(
        &mut self,
        selected: Option<String>,
        expanded: Vec<String>,
        docs_expanded: Vec<String>,
    ) {
        self.selected = selected;
        self.expanded = expanded.into_iter().collect();
        self.docs_expanded = docs_expanded.into_iter().collect();
    }

    /// Re-read the tree plus the selected project's files/summary/detail.
    /// Detail is left untouched while a description save is pending so an
    /// in-flight edit is never clobbered.
    pub fn reload(&mut self, db: &Connection) {
        match projects::list_tree(db) {
            Ok(tree) => {
                self.tree = tree;
                self.error = None;
            }
            Err(e) => {
                self.error = Some(format!("Cannot load projects: {e}"));
                return;
            }
        }

        // Refresh the inline document trees shown in the left panel. Entries
        // for projects that no longer exist are pruned.
        self.docs_expanded
            .retain(|id| projects::find_project(&self.tree, id).is_some());
        let expanded = self.docs_expanded.clone();
        self.tree_docs.retain(|id, _| expanded.contains(id));
        for id in &expanded {
            self.load_project_docs(id);
        }

        let Some(id) = self.selected.clone() else {
            if !self.tabs.enter_project(&self.root, None) {
                self.selected = self.tabs.project_id.clone();
                return;
            }
            self.detail = None;
            self.files.clear();
            self.summary = None;
            self.clear_doc_tree();
            return;
        };
        if projects::find_project(&self.tree, &id).is_none() {
            self.selected = None;
            // The project vanished; drop the tab strip rather than retrying a
            // save against a folder that no longer exists.
            self.tabs.enter_project(&self.root, None);
            self.detail = None;
            self.files.clear();
            self.summary = None;
            self.clear_doc_tree();
            return;
        }

        // Selection changed: flush the outgoing project's documents and reset
        // the tab strip before loading the new project. If a document cannot
        // be saved, stay on the old project so its buffer is not discarded.
        if !self.tabs.enter_project(&self.root, Some(&id)) {
            self.selected = self.tabs.project_id.clone();
            return;
        }

        // The Documents tree's expansion/target belong to one project.
        let dir = doc_dir(&self.root, &id);
        if self.doc_tree_project.as_deref() != Some(id.as_str()) {
            self.doc_tree_project = Some(id.clone());
            self.doc_expanded.clear();
            self.doc_create = None;
            self.doc_selected_dir = dir.clone();
        } else if !self.doc_selected_dir.starts_with(&dir) || !self.doc_selected_dir.is_dir() {
            self.doc_selected_dir = dir.clone();
        }
        // Drop a stale create row if its folder vanished under us.
        if let Some(draft) = &self.doc_create
            && !draft.parent.is_dir()
        {
            self.doc_create = None;
        }

        match projects::project_files(&self.root, &id) {
            Ok(files) => self.files = files,
            Err(e) => {
                self.files.clear();
                self.error = Some(format!("Cannot read project files: {e}"));
            }
        }
        match projects::project_summary(db, &self.root, &id) {
            Ok(summary) => self.summary = Some(summary),
            Err(e) => {
                self.summary = None;
                self.error = Some(format!("Cannot load project summary: {e}"));
            }
        }
        if self.desc_dirty.is_none() {
            match projects::get_project(db, &id) {
                Ok(project) => self.detail = Some(project),
                Err(e) => self.error = Some(format!("Cannot load project: {e}")),
            }
        }
    }

    /// Reset the Documents tree when no project is selected (or it vanished).
    fn clear_doc_tree(&mut self) {
        self.doc_tree_project = None;
        self.doc_expanded.clear();
        self.doc_create = None;
        self.doc_selected_dir.clear();
    }

    /// Re-scan the selected project's document folder (refresh or after a
    /// create).
    fn reload_files(&mut self) {
        let Some(id) = self.selected.clone() else {
            self.files.clear();
            return;
        };
        match projects::project_files(&self.root, &id) {
            Ok(files) => {
                if self.docs_expanded.contains(&id) {
                    self.tree_docs.insert(id.clone(), files.clone());
                }
                self.files = files;
            }
            Err(e) => self.error = Some(format!("Cannot read project files: {e}")),
        }
    }

    /// Scan one project's documents into the left-tree cache.
    fn load_project_docs(&mut self, id: &str) {
        match projects::project_files(&self.root, id) {
            Ok(files) => {
                self.tree_docs.insert(id.to_string(), files);
            }
            Err(e) => {
                self.tree_docs.insert(id.to_string(), Vec::new());
                self.error = Some(format!("Cannot read project files: {e}"));
            }
        }
    }

    /// Expand/collapse a project's inline document tree in the left panel.
    fn toggle_project_docs(&mut self, id: &str) {
        if self.docs_expanded.remove(id) {
            self.tree_docs.remove(id);
        } else {
            self.docs_expanded.insert(id.to_string());
            if projects::find_project(&self.tree, id).is_some() {
                self.load_project_docs(id);
            }
        }
    }

    /// Open the Documents tree's inline create row in the selected folder
    /// (project root when nothing is selected).
    fn begin_doc_create(&mut self, kind: DocCreateKind) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let dir = doc_dir(&self.root, &id);
        let parent = if self.doc_selected_dir.starts_with(&dir) && self.doc_selected_dir.is_dir() {
            self.doc_selected_dir.clone()
        } else {
            dir
        };
        self.begin_doc_create_in(kind, parent);
    }

    /// Open the inline create row inside `parent` (an existing project folder).
    fn begin_doc_create_in(&mut self, kind: DocCreateKind, parent: PathBuf) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let dir = doc_dir(&self.root, &id);
        if !parent.starts_with(&dir) || !parent.is_dir() {
            return;
        }
        if parent != dir {
            self.doc_expanded.insert(parent.clone());
        }
        self.doc_selected_dir = parent.clone();
        self.doc_create = Some(DocCreate {
            kind,
            parent,
            name: match kind {
                DocCreateKind::File => "untitled.md".to_string(),
                DocCreateKind::Folder => "New Folder".to_string(),
            },
            focus: true,
        });
    }

    /// Commit the inline create row. A new folder is selected + expanded; a
    /// new text file opens in a tab. Failures keep the row open.
    fn commit_doc_create(&mut self) {
        let Some(draft) = self.doc_create.take() else {
            return;
        };
        let Some(id) = self.selected.clone() else {
            return;
        };
        let root = self.root.clone();
        let dir = doc_dir(&root, &id);
        let parent_rel = draft.parent.strip_prefix(&dir).unwrap_or(Path::new(""));
        let outcome = match draft.kind {
            DocCreateKind::File => projects::create_doc_file(&root, &id, parent_rel, &draft.name),
            DocCreateKind::Folder => {
                projects::create_doc_folder(&root, &id, parent_rel, &draft.name)
            }
        };
        match outcome {
            Ok(path) => {
                self.doc_selected_dir = draft.parent.clone();
                self.doc_expanded.insert(draft.parent.clone());
                self.reload_files();
                match draft.kind {
                    DocCreateKind::File => {
                        if let Ok(rel) = path.strip_prefix(&dir) {
                            self.tabs.open_doc(&root, rel);
                        }
                    }
                    DocCreateKind::Folder => {
                        self.doc_expanded.insert(path.clone());
                        self.doc_selected_dir = path;
                    }
                }
                self.error = None;
            }
            Err(e) => {
                let what = match draft.kind {
                    DocCreateKind::File => "file",
                    DocCreateKind::Folder => "folder",
                };
                self.error = Some(format!("Cannot create {what}: {e}"));
                // Keep the row open so the name can be corrected.
                self.doc_create = Some(DocCreate {
                    focus: true,
                    ..draft
                });
            }
        }
    }

    /// Flush the description and any documents whose debounce elapsed.
    /// Returns true while a save is still pending so the caller can keep
    /// scheduling repaints.
    pub fn tick_autosave(&mut self, db: &Connection) -> bool {
        if let Some(since) = self.desc_dirty
            && since.elapsed() >= AUTOSAVE_DEBOUNCE
        {
            self.flush_description(db);
        }
        let docs_pending = self.tabs.tick_autosave(&self.root);
        self.desc_dirty.is_some() || docs_pending
    }

    /// Flush any pending description/document writes (section switch, exit).
    pub fn flush_all(&mut self, db: &Connection) {
        if self.desc_dirty.is_some() {
            self.flush_description(db);
        }
        self.tabs.flush_all(&self.root);
    }

    fn flush_description(&mut self, db: &Connection) {
        let Some(id) = self.selected.clone() else {
            self.desc_dirty = None;
            return;
        };
        let Some(description) = self.detail.as_ref().map(|d| d.description.clone()) else {
            self.desc_dirty = None;
            return;
        };
        match projects::update_project(db, &id, Some(&description), None) {
            Ok(_) => self.desc_dirty = None,
            Err(e) => {
                // Keep it dirty so autosave retries, but throttle the next
                // attempt instead of hammering a failing database.
                self.desc_dirty = Some(Instant::now());
                self.error = Some(format!("Cannot save description: {e}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

/// Draw the left panel (toolbar, filter, tree). The app draws the detail pane
/// inside [`PANE_GAP`] after this returns.
pub fn show_tree(
    ui: &mut egui::Ui,
    state: &mut ProjectsState,
    db: &Arc<Mutex<Connection>>,
) -> ProjectsActions {
    let mut actions = ProjectsActions::default();
    let mut mutations: Vec<Mutation> = Vec::new();
    let t = Tokens::get(ui.ctx());

    if state.tree_open {
        egui::Panel::left("projects-tree")
            .resizable(true)
            .default_size(TREE_DEFAULT_W)
            .min_size(160.0)
            .max_size(480.0)
            .frame(egui::Frame::new().fill(t.panel))
            .show(ui, |ui| {
                tree_panel(ui, state, &mut actions, &mut mutations);
            });
    } else {
        egui::Panel::left("projects-tree-rail")
            .resizable(false)
            .default_size(widgets::RAIL_W)
            .min_size(widgets::RAIL_W)
            .max_size(widgets::RAIL_W)
            .frame(egui::Frame::new().fill(t.panel))
            .show(ui, |ui| {
                if widgets::expand_button(ui) {
                    state.tree_open = true;
                }
            });
    }

    if !mutations.is_empty() {
        apply_mutations(state, db, mutations);
    }
    actions
}

// -- right pane: tab strip + body -------------------------------------------

/// Tab strip for the selected project: pinned Overview and Tasks tabs plus
/// closable document tabs. Returns the kind the app should render below it,
/// or `None` when no project is selected (the empty state).
pub fn detail_tab_strip(
    ui: &mut egui::Ui,
    state: &mut ProjectsState,
    dark: bool,
) -> Option<DetailKind> {
    state.detail.as_ref()?;
    state.tabs.sync_theme(dark);

    let mut focus: Option<usize> = None;
    let mut close: Option<usize> = None;
    ui.horizontal(|ui| {
        let toggle_w = 34.0;
        let strip_w = (ui.available_width() - toggle_w).max(60.0);
        ui.allocate_ui(egui::vec2(strip_w, TAB_H), |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("project-tabs-scroll")
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Pinned tabs: no close buttons.
                        for (idx, label) in [(0, "Overview"), (1, "Tasks")] {
                            if theme::tab_button(
                                ui,
                                label,
                                state.tabs.active == idx,
                                egui::TextStyle::Small,
                                theme::small_strong(),
                                TAB_H,
                            )
                            .clicked()
                            {
                                focus = Some(idx);
                            }
                        }
                        for (idx, tab) in state.tabs.tabs.iter().enumerate().skip(2) {
                            let DetailTab::Doc(doc) = tab else {
                                continue;
                            };
                            let resp = crate::widgets::tabs::tab_widget(
                                ui,
                                &doc_label(&doc.rel),
                                state.tabs.active == idx,
                                doc.dirty_since.is_some(),
                                ("project-tab", idx),
                            );
                            if resp.clicked() {
                                focus = Some(idx);
                            }
                            if resp.middle_clicked() || resp.close_clicked {
                                close = Some(idx);
                            }
                        }
                    });
                });
        });

        // Edit/preview toggle while a document tab is active.
        if let Some(DetailTab::Doc(doc)) = state.tabs.tabs.get_mut(state.tabs.active) {
            markdown::mode_toggle(ui, &mut doc.mode);
        }
    });
    ui.add_space(6.0);

    if let Some(idx) = focus {
        state.tabs.active = idx;
    }
    if let Some(idx) = close {
        state.tabs.close(&state.root, idx);
    }
    Some(state.tabs.active_kind())
}

/// Body under the tab strip. The Tasks tab is drawn by the app (it owns the
/// kanban pipeline), so this only handles the overview, documents and the
/// no-selection empty state.
pub fn detail_body(
    ui: &mut egui::Ui,
    state: &mut ProjectsState,
    db: &Arc<Mutex<Connection>>,
    dark: bool,
) {
    let Some(detail) = state.detail.clone() else {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            if state.tree.is_empty() {
                ui.weak("No projects yet — create one from the toolbar");
            } else {
                ui.weak("Select a project");
            }
        });
        return;
    };
    state.tabs.sync_theme(dark);
    if state.tabs.active_doc_index().is_some() {
        doc_panel(ui, state);
        return;
    }
    let mut mutations: Vec<Mutation> = Vec::new();
    overview_panel(ui, state, &detail, &mut mutations);
    if !mutations.is_empty() {
        apply_mutations(state, db, mutations);
    }
}

// -- left panel: toolbar + filter + tree ------------------------------------

fn tree_panel(
    ui: &mut egui::Ui,
    state: &mut ProjectsState,
    actions: &mut ProjectsActions,
    mutations: &mut Vec<Mutation>,
) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        if theme::icon_button(ui, "folder-plus", 26.0, false, "New folder").clicked() {
            mutations.push(Mutation::CreateFolder {
                parent: state.selected_folder.clone(),
                name: "New Folder".to_string(),
            });
        }
        new_project_menu(ui, state, mutations);
        if theme::icon_button(ui, "rotate-cw", 26.0, false, "Refresh").clicked() {
            mutations.push(Mutation::Refresh);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::collapse_button(ui) {
                state.tree_open = false;
            }
        });
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        let w = (ui.available_width() - 4.0).max(80.0);
        theme::search_field(ui, &mut state.filter, "Filter projects", w);
    });
    ui.add_space(4.0);

    if let Some(err) = state.error.clone() {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.colored_label(ui.visuals().error_fg_color, err);
        });
        ui.add_space(4.0);
    }

    egui::ScrollArea::vertical()
        .id_salt("projects-tree-scroll")
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            let tree = state.tree.clone();
            let needle = state.filter.trim().to_lowercase();
            if needle.is_empty() {
                tree_entries(ui, state, &tree, 0, None, actions, mutations);
            } else {
                let mut matches: Vec<&Project> = Vec::new();
                collect_matching(&tree, &needle, &mut matches);
                if matches.is_empty() {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.weak("No matching projects");
                    });
                }
                for project in matches {
                    entity_row(
                        ui,
                        state,
                        &project.id,
                        &project.name,
                        false,
                        Some(project),
                        project.folder_id.as_deref(),
                        0,
                        false,
                        actions,
                        mutations,
                    );
                }
            }
        });
}

/// "New project" button: a popup menu of the built-in templates. The project
/// is created with a default name and immediately opened for inline rename.
fn new_project_menu(ui: &mut egui::Ui, state: &mut ProjectsState, mutations: &mut Vec<Mutation>) {
    let resp = theme::icon_button(ui, "file-plus", 26.0, false, "New project");
    let parent = state.selected_folder.clone();
    let _ = egui::Popup::menu(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            for tpl in TEMPLATES.iter() {
                if theme::menu_item(ui, "briefcase", tpl.label, false).clicked() {
                    mutations.push(Mutation::CreateProject {
                        folder: parent.clone(),
                        name: "New Project".to_string(),
                        template: tpl.id,
                    });
                }
            }
        });
}

fn tree_entries(
    ui: &mut egui::Ui,
    state: &mut ProjectsState,
    nodes: &[ProjectNode],
    depth: usize,
    parent: Option<&str>,
    actions: &mut ProjectsActions,
    mutations: &mut Vec<Mutation>,
) {
    for node in nodes {
        match node {
            ProjectNode::Folder { id, name, children } => {
                entity_row(
                    ui, state, id, name, true, None, None, depth, true, actions, mutations,
                );
                if state.expanded.contains(id) {
                    tree_entries(ui, state, children, depth + 1, Some(id), actions, mutations);
                }
            }
            ProjectNode::Project(project) => {
                entity_row(
                    ui,
                    state,
                    &project.id,
                    &project.name,
                    false,
                    Some(project),
                    parent,
                    depth,
                    true,
                    actions,
                    mutations,
                );
                if state.docs_expanded.contains(&project.id) {
                    let entries = state
                        .tree_docs
                        .get(&project.id)
                        .cloned()
                        .unwrap_or_default();
                    tree_doc_entries(
                        ui,
                        state,
                        &project.id,
                        parent,
                        &entries,
                        depth + 1,
                        mutations,
                    );
                }
            }
        }
    }
}

fn collect_matching<'a>(nodes: &'a [ProjectNode], needle: &str, out: &mut Vec<&'a Project>) {
    for node in nodes {
        match node {
            ProjectNode::Folder { children, .. } => collect_matching(children, needle, out),
            ProjectNode::Project(p) => {
                if p.name.to_lowercase().contains(needle)
                    || p.identifier.to_lowercase().contains(needle)
                {
                    out.push(p);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn entity_row(
    ui: &mut egui::Ui,
    state: &mut ProjectsState,
    id: &str,
    label: &str,
    is_folder: bool,
    project: Option<&Project>,
    parent: Option<&str>,
    depth: usize,
    show_docs_toggle: bool,
    actions: &mut ProjectsActions,
    mutations: &mut Vec<Mutation>,
) {
    let t = Tokens::get(ui.ctx());
    ui.push_id(("project-row", is_folder, id), |ui| {
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ROW_H),
            egui::Sense::click(),
        );
        // Project rows are labelled with their short identifier; keep the full
        // name available as a tooltip.
        let resp = if project.is_some() {
            resp.on_hover_text(label)
        } else {
            resp
        };
        let press = Press::track(ui, &resp);
        let is_selected = !is_folder && state.selected.as_deref() == Some(id);
        press.wash(ui, rect, egui::CornerRadius::same(t.radius), is_selected);

        // Folders show their name; projects show their identifier.
        let display = match project {
            Some(project) if !project.identifier.is_empty() => project.identifier.as_str(),
            _ => label,
        };

        let mut x = rect.left() + 8.0 + depth as f32 * INDENT;
        let cy = rect.center().y + press.offset().y;

        let docs_open = !is_folder && state.docs_expanded.contains(id);
        let docs_toggle = is_folder || (project.is_some() && show_docs_toggle);
        let mut chevron_clicked = false;
        if docs_toggle {
            let expanded_here = if is_folder {
                state.expanded.contains(id)
            } else {
                docs_open
            };
            let chevron = if expanded_here {
                "chevron-down"
            } else {
                "chevron-right"
            };
            let chevron_rect =
                Rect::from_center_size(egui::pos2(x + 6.0, cy), egui::Vec2::splat(12.0));
            crate::icons::paint(ui, chevron_rect, chevron, 12.0, t.text_faint);
            if project.is_some() {
                // Projects have their own disclosure target: clicking the
                // chevron toggles documents without changing the selection.
                let arrow = ui
                    .interact(
                        chevron_rect.expand(3.0),
                        ui.id().with(("project-docs-chevron", id)),
                        egui::Sense::click(),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(if docs_open {
                        "Hide documents"
                    } else {
                        "Show documents"
                    });
                if arrow.clicked() {
                    chevron_clicked = true;
                    state.toggle_project_docs(id);
                }
            }
        }
        x += 14.0;

        if is_folder {
            let icon_tint = if is_selected { t.accent_text } else { t.icon };
            crate::icons::paint(
                ui,
                Rect::from_center_size(egui::pos2(x + 7.0, cy), egui::Vec2::splat(14.0)),
                "folder",
                14.0,
                icon_tint,
            );
            x += 18.0;
        } else {
            // No project icon: keep the identifier close to the chevron.
            x += 6.0;
        }

        let renaming_this = state
            .renaming
            .as_ref()
            .is_some_and(|r| r.is_folder == is_folder && r.id == id);
        if renaming_this {
            let mut rename = state.renaming.take().unwrap();
            let resp = ui.put(
                Rect::from_min_size(
                    egui::pos2(x + 4.0, rect.top() + 3.0),
                    egui::vec2(160.0, ROW_H - 6.0),
                ),
                egui::TextEdit::singleline(&mut rename.draft).desired_width(160.0),
            );
            resp.request_focus();
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if (enter || resp.lost_focus()) && !escape {
                let trimmed = rename.draft.trim().to_string();
                if !trimmed.is_empty() && trimmed != label {
                    if is_folder {
                        mutations.push(Mutation::RenameFolder {
                            id: id.to_string(),
                            name: trimmed,
                        });
                    } else {
                        mutations.push(Mutation::RenameProject {
                            id: id.to_string(),
                            name: trimmed,
                        });
                    }
                }
            } else if !escape {
                state.renaming = Some(rename);
            }
        } else {
            let galley = ui.painter().layout_no_wrap(
                display.to_string(),
                theme::font_id(ui, egui::TextStyle::Small),
                if is_selected { t.accent_text } else { t.text },
            );
            ui.painter().galley(
                egui::pos2(x + 4.0, cy - galley.size().y * 0.5),
                galley,
                t.text,
            );
        }

        if resp.clicked() && !chevron_clicked {
            if is_folder {
                state.selected_folder = Some(id.to_string());
                if !state.expanded.insert(id.to_string()) {
                    state.expanded.remove(id);
                }
            } else if state.selected.as_deref() != Some(id) {
                // Flush the outgoing project's pending description first.
                if state.desc_dirty.is_some()
                    && let Some(prev) = &state.detail
                {
                    mutations.push(Mutation::SetDescription {
                        id: prev.id.clone(),
                        description: prev.description.clone(),
                    });
                }
                state.selected = Some(id.to_string());
                state.selected_folder = parent.map(str::to_string);
                state.detail = None;
                state.desc_dirty = None;
                mutations.push(Mutation::Refresh);
            }
        }

        resp.context_menu(|ui| {
            if is_folder {
                if theme::menu_item(ui, "file-plus", "New project here", false).clicked() {
                    ui.close();
                    mutations.push(Mutation::CreateProject {
                        folder: Some(id.to_string()),
                        name: "New Project".to_string(),
                        template: "blank",
                    });
                }
                if theme::menu_item(ui, "folder-plus", "New folder here", false).clicked() {
                    ui.close();
                    state.expanded.insert(id.to_string());
                    mutations.push(Mutation::CreateFolder {
                        parent: Some(id.to_string()),
                        name: "New Folder".to_string(),
                    });
                }
            } else if let Some(project) = project {
                let docs_open = state.docs_expanded.contains(id);
                if show_docs_toggle {
                    let docs_label = if docs_open {
                        "Hide documents"
                    } else {
                        "Show documents"
                    };
                    let docs_icon = if docs_open {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    };
                    if theme::menu_item(ui, docs_icon, docs_label, false).clicked() {
                        ui.close();
                        state.toggle_project_docs(id);
                    }
                }
                if theme::menu_item(ui, "square-kanban", "Open board", false).clicked() {
                    ui.close();
                    actions.open_board = Some(project.board_id.clone());
                }
                if theme::menu_item(ui, "folder-open", "Reveal in Finder", false).clicked() {
                    ui.close();
                    reveal_in_finder(&project_dir(&state.root, &project.id), true);
                }
            }
            if theme::menu_item(ui, "pencil", "Rename", false).clicked() {
                ui.close();
                state.renaming = Some(Rename {
                    is_folder,
                    id: id.to_string(),
                    draft: label.to_string(),
                });
            }
            if theme::menu_item(ui, "trash-2", "Delete", true).clicked() {
                ui.close();
                let name = label.to_string();
                actions.request_delete = Some(if is_folder {
                    DeleteKind::Folder {
                        id: id.to_string(),
                        name,
                    }
                } else {
                    DeleteKind::Project {
                        id: id.to_string(),
                        name,
                    }
                });
            }
        });
    });
}

// -- right panel: overview --------------------------------------------------

fn tree_doc_entries(
    ui: &mut egui::Ui,
    state: &mut ProjectsState,
    project_id: &str,
    parent: Option<&str>,
    entries: &[FsEntry],
    depth: usize,
    mutations: &mut Vec<Mutation>,
) {
    if entries.is_empty() {
        tree_doc_empty(ui, depth);
        return;
    }
    for entry in entries {
        tree_doc_row(ui, state, project_id, parent, entry, depth, mutations);
        if entry.is_dir
            && state.tree_doc_expanded.contains(&entry.path)
            && let Some(children) = &entry.children
        {
            tree_doc_entries(
                ui,
                state,
                project_id,
                parent,
                children,
                depth + 1,
                mutations,
            );
        }
    }
}

fn tree_doc_empty(ui: &mut egui::Ui, depth: usize) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.add_space(8.0 + depth as f32 * INDENT);
        ui.label(
            egui::RichText::new("No documents")
                .text_style(egui::TextStyle::Small)
                .color(t.text_faint),
        );
    });
}

fn tree_doc_row(
    ui: &mut egui::Ui,
    state: &mut ProjectsState,
    project_id: &str,
    parent: Option<&str>,
    entry: &FsEntry,
    depth: usize,
    mutations: &mut Vec<Mutation>,
) {
    let t = Tokens::get(ui.ctx());
    ui.push_id(
        ("project-tree-doc", project_id, entry.path.as_os_str()),
        |ui| {
            let editable = !entry.is_dir && is_text_doc(&entry.name);
            let sense = if entry.is_dir || editable {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            };
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), sense);
            let press = Press::track(ui, &resp);
            press.wash(ui, rect, egui::CornerRadius::same(t.radius), false);

            let mut x = rect.left() + 8.0 + depth as f32 * INDENT;
            let cy = rect.center().y + press.offset().y;

            if entry.is_dir {
                let expanded = state.tree_doc_expanded.contains(&entry.path);
                let chevron = if expanded {
                    "chevron-down"
                } else {
                    "chevron-right"
                };
                crate::icons::paint(
                    ui,
                    Rect::from_center_size(egui::pos2(x + 6.0, cy), egui::Vec2::splat(12.0)),
                    chevron,
                    12.0,
                    t.text_faint,
                );
            }
            x += 14.0;

            let icon = if entry.is_dir { "folder" } else { "file" };
            crate::icons::paint(
                ui,
                Rect::from_center_size(egui::pos2(x + 7.0, cy), egui::Vec2::splat(14.0)),
                icon,
                14.0,
                t.icon,
            );
            x += 18.0;

            let color = if editable { t.text } else { t.text_muted };
            let galley = ui.painter().layout_no_wrap(
                entry.name.clone(),
                theme::font_id(ui, egui::TextStyle::Small),
                color,
            );
            ui.painter().with_clip_rect(rect).galley(
                egui::pos2(x + 4.0, cy - galley.size().y * 0.5),
                galley,
                color,
            );

            let resp = if entry.is_dir {
                let hint = if state.tree_doc_expanded.contains(&entry.path) {
                    "Collapse folder"
                } else {
                    "Expand folder"
                };
                resp.on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(hint)
            } else if editable {
                resp.on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text("Open in a project tab")
            } else {
                resp.on_hover_text("Not a markdown or text document")
            };

            if resp.clicked() {
                if entry.is_dir {
                    if !state.tree_doc_expanded.insert(entry.path.clone()) {
                        state.tree_doc_expanded.remove(&entry.path);
                    }
                } else if editable {
                    open_doc_from_tree(state, project_id, parent, &entry.path, mutations);
                }
            }

            if editable {
                resp.context_menu(|ui| {
                    if theme::menu_item(ui, "eye", "Open", false).clicked() {
                        ui.close();
                        open_doc_from_tree(state, project_id, parent, &entry.path, mutations);
                    }
                });
            }
        },
    );
}

/// Open a text document from the left tree. Selecting another project first
/// resets its tabs, opens the document, and asks for a refresh so the detail
/// pane catches up without dropping the new tab.
fn open_doc_from_tree(
    state: &mut ProjectsState,
    project_id: &str,
    parent: Option<&str>,
    path: &Path,
    mutations: &mut Vec<Mutation>,
) {
    let root = state.root.clone();
    let dir = doc_dir(&root, project_id);
    let Ok(rel) = path.strip_prefix(&dir) else {
        return;
    };
    let switching = state.selected.as_deref() != Some(project_id);
    if switching {
        if state.desc_dirty.is_some()
            && let Some(prev) = &state.detail
        {
            mutations.push(Mutation::SetDescription {
                id: prev.id.clone(),
                description: prev.description.clone(),
            });
        }
        // Flush and reset the outgoing project's tab strip before changing
        // the selection. A failed save keeps the old project (and buffer).
        if !state.tabs.enter_project(&root, Some(project_id)) {
            return;
        }
        state.selected = Some(project_id.to_string());
        state.selected_folder = parent.map(str::to_string);
        state.detail = None;
        state.desc_dirty = None;
    }
    // `enter_project` no-ops when the tab strip already belongs to this
    // project, so re-selecting the same project keeps its open tabs.
    state.tabs.enter_project(&root, Some(project_id));
    state.tabs.open_doc(&root, rel);
    if switching {
        // Load the new project's detail/summary without resetting the tab
        // strip (its project id now matches).
        mutations.push(Mutation::Refresh);
    }
}

// -- right panel: overview --------------------------------------------------

fn overview_panel(
    ui: &mut egui::Ui,
    state: &mut ProjectsState,
    detail: &Project,
    mutations: &mut Vec<Mutation>,
) {
    let t = Tokens::get(ui.ctx());
    let summary = state.summary.clone().unwrap_or_default();

    egui::ScrollArea::vertical()
        .id_salt("project-overview-scroll")
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            ui.add_space(4.0);

            title_field(ui, state, detail, mutations);
            ui.add_space(6.0);
            identifier_field(ui, state, detail, mutations);
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                status_picker(ui, &detail.status, &detail.id, mutations);
            });
            ui.add_space(12.0);

            ui.label(
                egui::RichText::new("Description")
                    .text_style(theme::small_strong())
                    .color(t.text_muted),
            );
            ui.add_space(4.0);
            let mut description = detail.description.clone();
            if description_editor(ui, &mut description) {
                if let Some(d) = state.detail.as_mut() {
                    d.description = description;
                }
                state.desc_dirty = Some(Instant::now());
            }
            ui.add_space(14.0);

            documents_section(ui, state, detail);
            ui.add_space(14.0);

            metadata(ui, detail, &summary);
            ui.add_space(8.0);
        });
}

/// Project title with inline rename: double-click the label to edit, Enter or
/// focus loss commits, Esc cancels. Empty names are ignored (same as the tree
/// rename), so the old name survives.
fn title_field(
    ui: &mut egui::Ui,
    state: &mut ProjectsState,
    detail: &Project,
    mutations: &mut Vec<Mutation>,
) {
    if let Some(mut edit) = state.title_edit.take() {
        // An edit belongs to one project; drop it if the selection moved on.
        if edit.project_id == detail.id {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut edit.draft)
                    .font(theme::strong())
                    .desired_width(f32::INFINITY)
                    .hint_text("Project name"),
            );
            if edit.focus {
                resp.request_focus();
                edit.focus = false;
            }
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if escape {
                return;
            }
            if enter || resp.lost_focus() {
                let trimmed = edit.draft.trim().to_string();
                if !trimmed.is_empty() && trimmed != detail.name {
                    mutations.push(Mutation::RenameProject {
                        id: detail.id.clone(),
                        name: trimmed,
                    });
                }
                return;
            }
            state.title_edit = Some(edit);
            return;
        }
    }

    let resp = ui
        .add(
            egui::Label::new(egui::RichText::new(&detail.name).text_style(theme::strong()))
                .wrap_mode(egui::TextWrapMode::Truncate)
                .selectable(false)
                .sense(egui::Sense::click()),
        )
        .on_hover_text("Double-click to rename");
    if resp.double_clicked() {
        state.title_edit = Some(TitleEdit {
            project_id: detail.id.clone(),
            draft: detail.name.clone(),
            focus: true,
        });
    }
}

/// Short project identifier editor. Commits on Enter or focus loss; a unique
/// constraint failure is shown inline instead of the generic tree banner.
fn identifier_field(
    ui: &mut egui::Ui,
    state: &mut ProjectsState,
    detail: &Project,
    mutations: &mut Vec<Mutation>,
) {
    let t = Tokens::get(ui.ctx());
    if state.identifier_for.as_deref() != Some(detail.id.as_str()) {
        state.identifier_for = Some(detail.id.clone());
        state.identifier_draft = detail.identifier.clone();
        state.identifier_error = None;
    }
    // Size the box for 12 monospace characters: identifiers stop at 10, but
    // the extra room keeps the field from clipping the text and the caret.
    let mono = theme::font_id(ui, egui::TextStyle::Monospace);
    let char_width = ui.ctx().fonts_mut(|f| f.glyph_width(&mono, '0'));
    let field_width = char_width * 12.0 + 14.0;

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Identifier")
                .text_style(theme::small_strong())
                .color(t.text_muted),
        );
        ui.add_space(6.0);
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.identifier_draft)
                .font(egui::TextStyle::Monospace)
                .desired_width(field_width)
                .char_limit(10)
                .hint_text("prj"),
        );
        // Keep the buffer inside the accepted alphabet while typing so the
        // user doesn't see an error after every uppercase keystroke.
        let cleaned: String = state
            .identifier_draft
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(10)
            .flat_map(|c| c.to_lowercase())
            .collect();
        if cleaned != state.identifier_draft {
            state.identifier_draft = cleaned;
        }
        if resp.changed() {
            state.identifier_error = None;
        }

        let enter = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if enter || resp.lost_focus() {
            let draft = state.identifier_draft.trim().to_string();
            if draft.is_empty() {
                state.identifier_error = Some("Identifier required".to_string());
                state.identifier_for = None;
            } else if draft != detail.identifier {
                mutations.push(Mutation::SetIdentifier {
                    id: detail.id.clone(),
                    identifier: draft,
                });
            }
        }
    });
    if let Some(err) = &state.identifier_error {
        ui.add_space(2.0);
        ui.colored_label(ui.visuals().error_fg_color, err);
    }
}

fn status_picker(ui: &mut egui::Ui, current: &str, id: &str, mutations: &mut Vec<Mutation>) {
    let segments: Vec<theme::Segment> = PROJECT_STATUSES
        .into_iter()
        .map(|(key, label)| theme::Segment {
            label,
            icon: match key {
                "active" => "square-kanban",
                "on_hold" => "minus",
                _ => "check",
            },
        })
        .collect();
    let selected = PROJECT_STATUSES
        .iter()
        .position(|(key, _)| *key == current)
        .unwrap_or(0);
    if let Some(idx) = theme::segmented_control(ui, &segments, selected)
        && PROJECT_STATUSES[idx].0 != current
    {
        mutations.push(Mutation::SetStatus {
            id: id.to_string(),
            status: PROJECT_STATUSES[idx].0.to_string(),
        });
    }
}

/// Fixed-height description field: outer bordered frame + inner scrolling
/// frameless multiline editor (egui 0.36 multiline has no internal scroll).
/// Returns true when the text changed.
fn description_editor(ui: &mut egui::Ui, text: &mut String) -> bool {
    let t = Tokens::get(ui.ctx());
    let viewport = DESC_FIELD_HEIGHT - 18.0; // inner margins + stroke
    let mut changed = false;
    egui::Frame::new()
        .fill(t.field)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(egui::CornerRadius::same(t.radius))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.scope(|ui| {
                ui.spacing_mut().scroll.floating_allocated_width = 7.0;
                egui::ScrollArea::vertical()
                    .id_salt("project-description-scroll")
                    .max_height(viewport)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        let resp = ui.add(
                            egui::TextEdit::multiline(text)
                                .hint_text("Add a description…")
                                .frame(egui::Frame::new())
                                .desired_width(f32::INFINITY)
                                .min_size(egui::vec2(ui.available_width(), viewport)),
                        );
                        if resp.changed() {
                            changed = true;
                        }
                    });
            });
        });
    changed
}

fn metadata(ui: &mut egui::Ui, detail: &Project, summary: &ProjectSummary) {
    let t = Tokens::get(ui.ctx());
    let rows = [
        (
            "calendar".to_string(),
            format!("Created {}", relative_time(detail.created_at)),
        ),
        (
            "rotate-cw".to_string(),
            format!("Updated {}", relative_time(detail.updated_at)),
        ),
        (
            "square-kanban".to_string(),
            format!(
                "{} task{}  ·  {} to do  ·  {} in progress  ·  {} done",
                summary.total,
                if summary.total == 1 { "" } else { "s" },
                summary.todo,
                summary.in_progress,
                summary.done
            ),
        ),
        (
            "file".to_string(),
            format!(
                "{} document{}",
                summary.file_count,
                if summary.file_count == 1 { "" } else { "s" }
            ),
        ),
    ];
    for (icon, text) in rows {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(14.0), egui::Sense::hover());
            crate::icons::paint(ui, rect, &icon, 14.0, t.text_faint);
            ui.label(
                egui::RichText::new(text)
                    .text_style(egui::TextStyle::Small)
                    .color(t.text_muted),
            );
        });
    }
}

fn documents_section(ui: &mut egui::Ui, state: &mut ProjectsState, detail: &Project) {
    let t = Tokens::get(ui.ctx());
    let root = state.root.clone();
    let dir = doc_dir(&root, &detail.id);

    ui.horizontal(|ui| {
        let (chevron, hint) = if state.docs_open {
            ("chevron-down", "Collapse documents")
        } else {
            ("chevron-right", "Expand documents")
        };
        if theme::icon_button(ui, chevron, 22.0, false, hint).clicked() {
            state.docs_open = !state.docs_open;
        }
        ui.label(
            egui::RichText::new("Documents")
                .text_style(theme::small_strong())
                .color(t.text_muted),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // One reveal action for the whole project folder; rows don't get
            // their own Finder buttons.
            if theme::icon_button(
                ui,
                "folder-open",
                24.0,
                false,
                "Reveal project folder in Finder",
            )
            .clicked()
            {
                reveal_in_finder(&dir, true);
            }
            if theme::icon_button(ui, "rotate-cw", 24.0, false, "Refresh").clicked() {
                state.reload_files();
            }
            if theme::icon_button(ui, "folder-plus", 24.0, false, "New folder").clicked() {
                state.begin_doc_create(DocCreateKind::Folder);
            }
            if theme::icon_button(ui, "file-plus", 24.0, false, "New file").clicked() {
                state.begin_doc_create(DocCreateKind::File);
            }
        });
    });

    if !state.docs_open {
        return;
    }
    ui.add_space(4.0);

    if state.files.is_empty() && state.doc_create.is_none() {
        ui.weak("No documents yet — create a file or add files to the project folder.");
        return;
    }

    // Clone for display: row actions may rescan the folder mid-render.
    let entries = state.files.clone();
    doc_entries(ui, state, &entries, &dir, 0);
}

fn doc_entries(
    ui: &mut egui::Ui,
    state: &mut ProjectsState,
    entries: &[FsEntry],
    dir: &Path,
    depth: usize,
) {
    // The inline create row sits at the top of its target folder.
    doc_create_row(ui, state, dir, depth);
    for entry in entries {
        doc_row(ui, state, entry, depth);
        if entry.is_dir
            && state.doc_expanded.contains(&entry.path)
            && let Some(children) = &entry.children
        {
            doc_entries(ui, state, children, &entry.path, depth + 1);
        }
    }
}

fn doc_row(ui: &mut egui::Ui, state: &mut ProjectsState, entry: &FsEntry, depth: usize) {
    let t = Tokens::get(ui.ctx());
    ui.push_id(("project-doc-row", entry.path.as_os_str()), |ui| {
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ROW_H),
            egui::Sense::click(),
        );
        let press = Press::track(ui, &resp);
        let is_selected = entry.is_dir && state.doc_selected_dir == entry.path;
        press.wash(ui, rect, egui::CornerRadius::same(t.radius), is_selected);

        let mut x = rect.left() + 4.0 + depth as f32 * INDENT;
        let cy = rect.center().y + press.offset().y;

        if entry.is_dir {
            let chevron = if state.doc_expanded.contains(&entry.path) {
                "chevron-down"
            } else {
                "chevron-right"
            };
            crate::icons::paint(
                ui,
                Rect::from_center_size(egui::pos2(x + 6.0, cy), egui::Vec2::splat(12.0)),
                chevron,
                12.0,
                t.text_faint,
            );
        }
        x += 16.0;

        let icon = if entry.is_dir { "folder" } else { "file" };
        crate::icons::paint(
            ui,
            Rect::from_center_size(egui::pos2(x + 7.0, cy), egui::Vec2::splat(14.0)),
            icon,
            14.0,
            t.icon,
        );
        x += 18.0;

        let editable = !entry.is_dir && is_text_doc(&entry.name);
        let color = if is_selected { t.accent_text } else { t.text };
        let galley = ui.painter().layout_no_wrap(
            entry.name.clone(),
            theme::font_id(ui, egui::TextStyle::Small),
            color,
        );
        ui.painter().with_clip_rect(rect).galley(
            egui::pos2(x + 4.0, cy - galley.size().y * 0.5),
            galley,
            color,
        );

        let resp = if editable {
            resp.on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Open in a project tab")
        } else {
            resp
        };

        if resp.clicked() {
            if entry.is_dir {
                state.doc_selected_dir = entry.path.clone();
                if !state.doc_expanded.insert(entry.path.clone()) {
                    state.doc_expanded.remove(&entry.path);
                    // Hiding the folder hides its inline create row too.
                    if state
                        .doc_create
                        .as_ref()
                        .is_some_and(|d| d.parent.starts_with(&entry.path))
                    {
                        state.doc_create = None;
                    }
                }
            } else if editable {
                let root = state.root.clone();
                if let Some(id) = state.selected.clone() {
                    let project = doc_dir(&root, &id);
                    if let Ok(rel) = entry.path.strip_prefix(&project) {
                        state.tabs.open_doc(&root, rel);
                    }
                }
            }
        }

        resp.context_menu(|ui| {
            if entry.is_dir {
                if theme::menu_item(ui, "file-plus", "New file here", false).clicked() {
                    ui.close();
                    state.begin_doc_create_in(DocCreateKind::File, entry.path.clone());
                }
                if theme::menu_item(ui, "folder-plus", "New folder here", false).clicked() {
                    ui.close();
                    state.begin_doc_create_in(DocCreateKind::Folder, entry.path.clone());
                }
            } else if editable && theme::menu_item(ui, "eye", "Open", false).clicked() {
                ui.close();
                let root = state.root.clone();
                if let Some(id) = state.selected.clone() {
                    let project = doc_dir(&root, &id);
                    if let Ok(rel) = entry.path.strip_prefix(&project) {
                        state.tabs.open_doc(&root, rel);
                    }
                }
            }
        });
    });
}

/// Inline "new file/folder" row: focused name field, Enter commits, Esc
/// cancels, focus loss commits (so clicking elsewhere keeps the item).
fn doc_create_row(ui: &mut egui::Ui, state: &mut ProjectsState, dir: &Path, depth: usize) {
    let Some(mut draft) = state.doc_create.take() else {
        return;
    };
    if draft.parent != dir {
        state.doc_create = Some(draft);
        return;
    }
    let t = Tokens::get(ui.ctx());

    ui.horizontal(|ui| {
        ui.add_space(4.0 + depth as f32 * INDENT + 16.0);
        let icon = match draft.kind {
            DocCreateKind::File => "file",
            DocCreateKind::Folder => "folder",
        };
        let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(14.0), egui::Sense::hover());
        crate::icons::paint(ui, rect, icon, 14.0, t.icon);
        ui.add_space(4.0);

        let width = (ui.available_width() - 8.0).max(120.0);
        let resp = ui.add(
            egui::TextEdit::singleline(&mut draft.name)
                .hint_text(match draft.kind {
                    DocCreateKind::File => "name.md",
                    DocCreateKind::Folder => "Folder name",
                })
                .desired_width(width)
                .id(egui::Id::new(("project-doc-create", &draft.parent))),
        );
        let requested_now = draft.focus;
        if draft.focus {
            resp.request_focus();
            draft.focus = false;
        }
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if escape {
            // Cancel: dropping the draft closes the row.
        } else if (enter || resp.lost_focus()) && !requested_now {
            state.doc_create = Some(draft);
            state.commit_doc_create();
        } else {
            state.doc_create = Some(draft);
        }
    });
}

// -- right pane: document editor -------------------------------------------

fn doc_panel(ui: &mut egui::Ui, state: &mut ProjectsState) {
    let Some(idx) = state.tabs.active_doc_index() else {
        return;
    };
    let DetailTabs {
        tabs,
        highlighter,
        preview_cache,
        error,
        ..
    } = &mut state.tabs;
    let Some(DetailTab::Doc(doc)) = tabs.get_mut(idx) else {
        return;
    };
    let hl = highlighter
        .as_ref()
        .expect("highlighter set by detail_body");

    ui.add(
        egui::Label::new(egui::RichText::new(doc_label(&doc.rel)).text_style(theme::strong()))
            .wrap_mode(egui::TextWrapMode::Truncate),
    );
    if let Some(err) = error.as_deref() {
        ui.colored_label(ui.visuals().error_fg_color, err);
    }
    ui.add_space(6.0);

    let id = egui::Id::new(("project-doc", doc.rel.as_os_str()));
    if markdown::body(ui, id, &mut doc.text, doc.mode, hl, preview_cache) {
        doc.dirty_since = Some(Instant::now());
    }
}

/// Tab label for a document: its file name.
fn doc_label(rel: &Path) -> String {
    rel.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| rel.to_string_lossy().to_string())
}

/// Documents the pane can open in a tab (plain-text sources only).
fn is_text_doc(name: &str) -> bool {
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some(ext) => matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown" | "txt"),
        None => false,
    }
}

// -- helpers ----------------------------------------------------------------

/// Canonical project document folder. `scan_tree` emits canonical paths, so
/// the tree's expansion/target state must use the same form to compare.
fn doc_dir(root: &Path, id: &str) -> PathBuf {
    let dir = project_dir(root, id);
    dir.canonicalize().unwrap_or(dir)
}

/// Reveal a file (`open -R`) or open a folder (`open`) in the Finder. The app
/// is macOS-only, so no cross-platform shim.
fn reveal_in_finder(path: &Path, is_dir: bool) {
    let mut cmd = std::process::Command::new("open");
    if !is_dir {
        cmd.arg("-R");
    }
    let _ = cmd.arg(path).spawn();
}

/// Relative time from a unix-seconds timestamp (pure arithmetic; no `chrono`).
fn relative_time(unix: i64) -> String {
    if unix <= 0 {
        return "—".to_string();
    }
    let secs = now_unix().saturating_sub(unix);
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else if secs < 2_592_000 {
        format!("{}d ago", secs / 86_400)
    } else if secs < 31_536_000 {
        format!("{}mo ago", secs / 2_592_000)
    } else {
        format!("{}y ago", secs / 31_536_000)
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn apply_mutations(
    state: &mut ProjectsState,
    db: &Arc<Mutex<Connection>>,
    mutations: Vec<Mutation>,
) {
    let guard = match db.lock() {
        Ok(guard) => guard,
        Err(_) => {
            state.error = Some("Database is locked".to_string());
            return;
        }
    };

    let mut error: Option<String> = None;
    let mut identifier_error: Option<String> = None;
    for mutation in mutations {
        let outcome: kandrium_core::Result<()> = (|| {
            match mutation {
                Mutation::Refresh => {}
                Mutation::CreateFolder { parent, name } => {
                    let folder = projects::create_folder(&guard, &name, parent.as_deref())?;
                    if let Some(parent) = parent {
                        state.expanded.insert(parent);
                    }
                    state.expanded.insert(folder.id.clone());
                    state.selected_folder = Some(folder.id.clone());
                    state.renaming = Some(Rename {
                        is_folder: true,
                        id: folder.id,
                        draft: name,
                    });
                }
                Mutation::CreateProject {
                    folder,
                    name,
                    template,
                } => {
                    let project = projects::create_project(
                        &guard,
                        &state.root,
                        &name,
                        folder.as_deref(),
                        template,
                    )?;
                    if let Some(folder) = folder {
                        state.expanded.insert(folder);
                    }
                    state.selected = Some(project.id.clone());
                    state.selected_folder = project.folder_id.clone();
                    state.renaming = Some(Rename {
                        is_folder: false,
                        id: project.id,
                        draft: name,
                    });
                }
                Mutation::RenameFolder { id, name } => {
                    projects::rename_folder(&guard, &id, &name)?;
                }
                Mutation::RenameProject { id, name } => {
                    let updated = projects::rename_project(&guard, &id, &name)?;
                    // `reload` skips the detail while a description save is
                    // pending, so apply the rename fields directly. Only name
                    // and identifier change: the unsaved description draft
                    // must survive.
                    if let Some(detail) = state.detail.as_mut()
                        && detail.id == id
                    {
                        detail.name = updated.name;
                        detail.identifier = updated.identifier;
                    }
                    // An auto-derived identifier may have changed: re-seed the
                    // Overview identifier field from the fresh detail.
                    state.identifier_for = None;
                    state.identifier_error = None;
                }
                Mutation::SetStatus { id, status } => {
                    projects::update_project(&guard, &id, None, Some(&status))?;
                }
                Mutation::SetDescription { id, description } => {
                    projects::update_project(&guard, &id, Some(&description), None)?;
                }
                Mutation::SetIdentifier { id, identifier } => {
                    match projects::set_identifier(&guard, &id, &identifier) {
                        Ok(updated) => {
                            if let Some(detail) = state.detail.as_mut()
                                && detail.id == id
                            {
                                detail.identifier = updated.identifier;
                            }
                            state.identifier_for = None;
                            state.identifier_error = None;
                        }
                        Err(e) => {
                            identifier_error = Some(e.0.clone());
                            state.identifier_for = None;
                            return Err(e);
                        }
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = outcome {
            error = Some(e.0);
            break;
        }
    }

    state.reload(&guard);
    drop(guard);
    if let Some(e) = identifier_error {
        state.identifier_error = Some(e);
    } else if let Some(e) = error {
        state.error = Some(e);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn shapes_text(output: &egui::FullOutput) -> String {
        output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) => Some(t.galley.job.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn relative_time_buckets() {
        let now = now_unix();
        assert_eq!(relative_time(0), "—");
        assert_eq!(relative_time(now), "just now");
        assert_eq!(relative_time(now - 90), "1m ago");
        assert_eq!(relative_time(now - 7_200), "2h ago");
        assert_eq!(relative_time(now - 172_800), "2d ago");
    }

    /// The Overview Documents section is a collapsible tree that can create
    /// nested folders and markdown files.
    #[test]
    fn documents_tree_create_and_collapse() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = kandrium_core::open_db(&tmp.path().join("t.db")).unwrap();
        kandrium_core::run_schema(&mut conn).unwrap();
        let root = tmp.path().join("projects");
        std::fs::create_dir_all(&root).unwrap();
        let project = kandrium_core::create_project(&conn, &root, "Docs", None, "blank").unwrap();
        let db = Arc::new(Mutex::new(conn));

        let mut state = ProjectsState::empty(root.clone(), None);
        state.selected = Some(project.id.clone());
        {
            let guard = db.lock().unwrap();
            state.reload(&guard);
        }
        let dir = doc_dir(&root, &project.id);

        // New folder becomes the target and is expanded.
        state.begin_doc_create_in(DocCreateKind::Folder, dir.clone());
        state.doc_create.as_mut().unwrap().name = "Notes".into();
        state.commit_doc_create();
        assert!(dir.join("Notes").is_dir());
        assert!(state.doc_expanded.contains(&dir.join("Notes")));
        assert_eq!(state.doc_selected_dir, dir.join("Notes"));

        // New file lands inside the selected folder and opens in a tab.
        state.begin_doc_create(DocCreateKind::File);
        state.doc_create.as_mut().unwrap().name = "plan.md".into();
        state.commit_doc_create();
        assert!(dir.join("Notes/plan.md").is_file());
        assert!(
            state.files.iter().any(|e| e.name == "Notes"
                && e.children
                    .as_ref()
                    .is_some_and(|c| c.iter().any(|f| f.name == "plan.md"))),
            "nested file must appear in the rescan: {:?}",
            state.files
        );
        assert!(
            state.tabs.active_doc_index().is_some(),
            "a new file opens in a document tab"
        );
        // A non-text doc only shows as a tree row (it cannot open in a tab).
        std::fs::write(dir.join("Notes/brief.pdf"), b"%PDF").unwrap();
        state.reload_files();
        // Back to the Overview so the tree renders.
        state.tabs.active = 0;

        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, true);

        for open in [true, false] {
            state.docs_open = open;
            let mut output = ctx.run_ui(Default::default(), |ctx| {
                let mut ui = egui::Ui::new(
                    ctx.clone(),
                    egui::Id::new("root"),
                    egui::UiBuilder::new().max_rect(Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(1000.0, 700.0),
                    )),
                );
                let _ = show_tree(&mut ui, &mut state, &db);
                egui::Frame::new().show(&mut ui, |ui| {
                    let kind = detail_tab_strip(ui, &mut state, true);
                    if kind != Some(DetailKind::Board) {
                        detail_body(ui, &mut state, &db, true);
                    }
                });
            });
            let text = shapes_text(&output);
            assert!(text.contains("Documents"), "header missing:\n{text}");
            if open {
                assert!(text.contains("Notes"), "folder row missing:\n{text}");
                assert!(text.contains("plan.md"), "nested file row missing:\n{text}");
                assert!(
                    text.contains("brief.pdf"),
                    "nested non-text row missing:\n{text}"
                );
            } else {
                assert!(
                    !text.contains("Notes") && !text.contains("brief.pdf"),
                    "collapsed section must hide the tree:\n{text}"
                );
            }
            assert!(!text.contains("use of"), "duplicate-ID warning:\n{text}");
            output.textures_delta.clear();
        }
    }

    /// Project rows in the left tree expand their documents via the chevron;
    /// expansion never changes the selected project.
    #[test]
    fn project_tree_documents_expand_and_collapse() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = kandrium_core::open_db(&tmp.path().join("t.db")).unwrap();
        kandrium_core::run_schema(&mut conn).unwrap();
        let root = tmp.path().join("projects");
        std::fs::create_dir_all(&root).unwrap();
        let project = kandrium_core::create_project(&conn, &root, "Docs", None, "blank").unwrap();
        let dir = doc_dir(&root, &project.id);
        std::fs::create_dir(dir.join("Notes")).unwrap();
        std::fs::write(dir.join("Notes/plan.md"), "# Plan\n").unwrap();
        std::fs::write(dir.join("Notes/brief.pdf"), b"%PDF").unwrap();

        let db = Arc::new(Mutex::new(conn));
        let mut state = ProjectsState::empty(root.clone(), None);
        state.selected = Some(project.id.clone());
        {
            let guard = db.lock().unwrap();
            state.reload(&guard);
        }

        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, true);

        fn render(
            ctx: &egui::Context,
            state: &mut ProjectsState,
            db: &Arc<Mutex<Connection>>,
        ) -> String {
            let mut output = ctx.run_ui(Default::default(), |ctx| {
                let mut ui = egui::Ui::new(
                    ctx.clone(),
                    egui::Id::new("root"),
                    egui::UiBuilder::new().max_rect(Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(1000.0, 700.0),
                    )),
                );
                let _ = show_tree(&mut ui, state, db);
            });
            let text = shapes_text(&output);
            output.textures_delta.clear();
            text
        }

        // Collapsed by default: the project row exists but its docs don't.
        let text = render(&ctx, &mut state, &db);
        assert!(
            text.contains(&project.identifier),
            "project row missing:\n{text}"
        );
        assert!(
            !text.contains("Notes"),
            "docs leaked while collapsed:\n{text}"
        );
        assert!(!text.contains("plan.md"));

        // Expanding the project loads the document tree without selecting.
        let selected_before = state.selected.clone();
        state.toggle_project_docs(&project.id);
        assert_eq!(state.selected, selected_before);
        assert!(state.tree_docs.contains_key(&project.id));
        assert_eq!(state.docs_expanded_ids(), vec![project.id.clone()]);

        // Top-level folder rows show; nested files stay behind their folder.
        let text = render(&ctx, &mut state, &db);
        assert!(
            text.contains("Notes"),
            "document folder row missing:\n{text}"
        );
        assert!(
            !text.contains("plan.md"),
            "nested file leaked from a collapsed folder:\n{text}"
        );

        // Expanding the document folder reveals its files.
        state.tree_doc_expanded.insert(dir.join("Notes"));
        let text = render(&ctx, &mut state, &db);
        assert!(
            text.contains("plan.md"),
            "text document row missing:\n{text}"
        );
        assert!(
            text.contains("brief.pdf"),
            "non-text document row missing:\n{text}"
        );
        assert!(!text.contains("use of"), "duplicate-ID warning:\n{text}");

        // Collapsing the project hides the whole document subtree.
        state.toggle_project_docs(&project.id);
        assert!(state.tree_docs.is_empty());
        let text = render(&ctx, &mut state, &db);
        assert!(
            !text.contains("Notes") && !text.contains("plan.md"),
            "collapsed project must hide its documents:\n{text}"
        );
        assert!(!text.contains("use of"), "duplicate-ID warning:\n{text}");
    }

    /// Opening a document from the tree selects its project, opens the tab,
    /// and keeps that tab through the follow-up reload.
    #[test]
    fn opening_tree_doc_switches_project_and_keeps_tab() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = kandrium_core::open_db(&tmp.path().join("t.db")).unwrap();
        kandrium_core::run_schema(&mut conn).unwrap();
        let root = tmp.path().join("projects");
        std::fs::create_dir_all(&root).unwrap();
        let alpha = kandrium_core::create_project(&conn, &root, "Alpha", None, "blank").unwrap();
        let beta = kandrium_core::create_project(&conn, &root, "Beta", None, "blank").unwrap();
        projects::write_doc(&root, &beta.id, Path::new("notes.md"), "# Beta\n").unwrap();

        let db = Arc::new(Mutex::new(conn));
        let mut state = ProjectsState::empty(root.clone(), None);
        state.selected = Some(alpha.id.clone());
        {
            let guard = db.lock().unwrap();
            state.reload(&guard);
        }
        assert_eq!(state.selected.as_deref(), Some(alpha.id.as_str()));

        let path = doc_dir(&root, &beta.id).join("notes.md");
        let mut mutations = Vec::new();
        open_doc_from_tree(&mut state, &beta.id, None, &path, &mut mutations);
        assert_eq!(state.selected.as_deref(), Some(beta.id.as_str()));
        assert!(
            state.tabs.active_doc_index().is_some(),
            "document tab must open immediately"
        );
        assert!(matches!(mutations.as_slice(), [Mutation::Refresh]));

        apply_mutations(&mut state, &db, mutations);
        assert_eq!(
            state.detail.as_ref().map(|d| d.id.as_str()),
            Some(beta.id.as_str())
        );
        let Some(DetailTab::Doc(doc)) = state.tabs.tabs.get(state.tabs.active) else {
            panic!("follow-up reload dropped the opened document tab");
        };
        assert_eq!(doc.rel, Path::new("notes.md"));
    }

    /// Render the projects view over a temp DB with a folder + project and scan
    /// for egui duplicate-ID warnings.
    #[test]
    fn no_duplicate_id_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = kandrium_core::open_db(&tmp.path().join("t.db")).unwrap();
        kandrium_core::run_schema(&mut conn).unwrap();
        let root = tmp.path().join("projects");
        std::fs::create_dir_all(&root).unwrap();
        let folder = kandrium_core::create_folder(&conn, "Work", None).unwrap();
        let project =
            kandrium_core::create_project(&conn, &root, "Alpha", Some(&folder.id), "software")
                .unwrap();

        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, true);

        let db = Arc::new(Mutex::new(conn));
        let mut state = ProjectsState::empty(root.clone(), None);
        state.expanded.insert(folder.id.clone());
        state.selected = Some(project.id.clone());
        {
            let guard = db.lock().unwrap();
            state.reload(&guard);
        }
        // Render with the Tasks tab and a document tab open.
        state.tabs.active = 1;
        state.tabs.open_doc(&root, Path::new("README.md"));

        let mut saw_tabs = false;
        // Expanded tree and collapsed rail must both render cleanly.
        for tree_open in [true, false] {
            state.tree_open = tree_open;
            for _ in 0..2 {
                let mut output = ctx.run_ui(Default::default(), |ctx| {
                    let mut ui = egui::Ui::new(
                        ctx.clone(),
                        egui::Id::new("root"),
                        egui::UiBuilder::new().max_rect(Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(1000.0, 700.0),
                        )),
                    );
                    let actions = show_tree(&mut ui, &mut state, &db);
                    assert!(actions.request_delete.is_none());
                    assert!(actions.open_board.is_none());
                    egui::Frame::new()
                        .inner_margin(egui::Margin {
                            left: PANE_GAP as i8,
                            right: 0,
                            top: 0,
                            bottom: 0,
                        })
                        .show(&mut ui, |ui| {
                            let kind = detail_tab_strip(ui, &mut state, true);
                            assert_eq!(kind, Some(state.tabs.active_kind()));
                            if kind != Some(DetailKind::Board) {
                                detail_body(ui, &mut state, &db, true);
                            }
                        });
                });
                let text = shapes_text(&output);
                assert!(
                    !text.contains("use of"),
                    "duplicate-ID warning visible:\n{text}"
                );
                saw_tabs |= text.contains("Overview")
                    && text.contains("Tasks")
                    && text.contains("README.md");
                output.textures_delta.clear();
            }
        }
        assert!(saw_tabs, "tab strip labels were not painted");
    }

    /// The tree labels projects with their short identifier, and the Overview
    /// exposes the identifier field.
    #[test]
    fn tree_labels_use_the_project_identifier() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = kandrium_core::open_db(&tmp.path().join("t.db")).unwrap();
        kandrium_core::run_schema(&mut conn).unwrap();
        let root = tmp.path().join("projects");
        std::fs::create_dir_all(&root).unwrap();
        let project =
            kandrium_core::create_project(&conn, &root, "Kandrium", None, "blank").unwrap();
        assert_eq!(project.identifier, "kan");

        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let db = Arc::new(Mutex::new(conn));
        let mut state = ProjectsState::empty(root.clone(), None);
        state.selected = Some(project.id.clone());
        {
            let guard = db.lock().unwrap();
            state.reload(&guard);
        }

        let mut output = ctx.run_ui(Default::default(), |ctx| {
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::Id::new("root"),
                egui::UiBuilder::new().max_rect(Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1000.0, 700.0),
                )),
            );
            let _ = show_tree(&mut ui, &mut state, &db);
            egui::Frame::new().show(&mut ui, |ui| {
                let _ = detail_tab_strip(ui, &mut state, false);
                detail_body(ui, &mut state, &db, false);
            });
        });
        let text = shapes_text(&output);
        assert!(
            text.contains("kan"),
            "tree should show the identifier:\n{text}"
        );
        assert!(
            text.contains("Identifier"),
            "overview should show the identifier field:\n{text}"
        );
        assert!(!text.contains("use of"), "duplicate-ID warning:\n{text}");
        output.textures_delta.clear();
    }

    /// Pinned Overview/Tasks tabs plus document tabs: dedupe on reopen,
    /// flush on close, drop documents on a project switch.
    #[test]
    fn project_tabs_open_close_and_reset() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = kandrium_core::open_db(&tmp.path().join("t.db")).unwrap();
        kandrium_core::run_schema(&mut conn).unwrap();
        let root = tmp.path().join("projects");
        std::fs::create_dir_all(&root).unwrap();
        let first = kandrium_core::create_project(&conn, &root, "First", None, "software").unwrap();
        let second = kandrium_core::create_project(&conn, &root, "Second", None, "blank").unwrap();

        let mut state = ProjectsState::empty(root.clone(), None);
        state.selected = Some(first.id.clone());
        state.reload(&conn);
        assert_eq!(state.tabs.active_kind(), DetailKind::Overview);

        // The Tasks tab is pinned from the start and ignores close requests.
        assert_eq!(state.tabs.tabs.len(), 2);
        assert!(matches!(state.tabs.tabs[1], DetailTab::Board));
        state.tabs.close(&root, 1);
        assert_eq!(state.tabs.tabs.len(), 2);

        // Focusing Tasks is a plain tab switch; the pinned set never grows.
        state.tabs.active = 1;
        assert_eq!(state.tabs.active_kind(), DetailKind::Board);
        assert_eq!(state.tabs.tabs.len(), 2);

        let readme = Path::new("README.md");
        state.tabs.open_doc(&root, readme);
        assert_eq!(state.tabs.active_kind(), DetailKind::Doc);
        let doc_idx = state.tabs.active;
        state.tabs.open_doc(&root, readme);
        assert_eq!(state.tabs.active, doc_idx);
        assert_eq!(state.tabs.tabs.len(), 3);

        // Edits flush on close.
        if let DetailTab::Doc(doc) = &mut state.tabs.tabs[doc_idx] {
            doc.text = "# Changed\n".to_string();
            doc.dirty_since = Some(Instant::now());
        }
        state.tabs.close(&root, doc_idx);
        assert_eq!(state.tabs.tabs.len(), 2);
        assert_eq!(state.tabs.active_kind(), DetailKind::Board);
        assert_eq!(
            projects::read_doc(&root, &first.id, readme).unwrap(),
            "# Changed\n"
        );

        // A project switch drops document tabs but keeps the Tasks tab.
        state.tabs.open_doc(&root, readme);
        state.tabs.active = 1;
        state.selected = Some(second.id.clone());
        state.reload(&conn);
        assert_eq!(state.tabs.tabs.len(), 2);
        assert_eq!(state.tabs.active_kind(), DetailKind::Board);

        // No selection resets to the pinned overview.
        state.selected = None;
        state.reload(&conn);
        assert_eq!(state.tabs.active_kind(), DetailKind::Overview);
    }

    /// A document edit is debounced and written into the project folder.
    #[test]
    fn document_autosave_writes_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = kandrium_core::open_db(&tmp.path().join("t.db")).unwrap();
        kandrium_core::run_schema(&mut conn).unwrap();
        let root = tmp.path().join("projects");
        std::fs::create_dir_all(&root).unwrap();
        let project =
            kandrium_core::create_project(&conn, &root, "Docs", None, "software").unwrap();

        let mut state = ProjectsState::empty(root.clone(), None);
        state.selected = Some(project.id.clone());
        state.reload(&conn);
        state.tabs.open_doc(&root, Path::new("notes.md"));
        if let DetailTab::Doc(doc) = &mut state.tabs.tabs[state.tabs.active] {
            doc.text = "# Saved\n".to_string();
            doc.dirty_since = Some(Instant::now() - AUTOSAVE_DEBOUNCE - Duration::from_millis(10));
        }
        assert!(!state.tick_autosave(&conn));
        assert_eq!(
            projects::read_doc(&root, &project.id, Path::new("notes.md")).unwrap(),
            "# Saved\n"
        );
    }

    /// A description edit is debounced: `tick_autosave` writes it back to the
    /// DB once the debounce elapses.
    #[test]
    fn description_autosave_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = kandrium_core::open_db(&tmp.path().join("t.db")).unwrap();
        kandrium_core::run_schema(&mut conn).unwrap();
        let root = tmp.path().join("projects");
        std::fs::create_dir_all(&root).unwrap();
        let project = kandrium_core::create_project(&conn, &root, "Alpha", None, "blank").unwrap();

        let mut state = ProjectsState::empty(root, None);
        {
            let guard = conn;
            state.selected = Some(project.id.clone());
            state.reload(&guard);
            state.detail.as_mut().unwrap().description = "hello".to_string();
            state.desc_dirty = Some(Instant::now() - AUTOSAVE_DEBOUNCE - Duration::from_millis(10));
            // The debounce elapsed, so this tick flushes and reports clean.
            assert!(!state.tick_autosave(&guard));
            assert!(state.desc_dirty.is_none());
            let saved = kandrium_core::get_project(&guard, &project.id).unwrap();
            assert_eq!(saved.description, "hello");
        }
    }

    // -- Overview title inline rename ---------------------------------------

    fn overview_fixture(name: &str) -> (tempfile::TempDir, Arc<Mutex<Connection>>, ProjectsState) {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = kandrium_core::open_db(&tmp.path().join("t.db")).unwrap();
        kandrium_core::run_schema(&mut conn).unwrap();
        let root = tmp.path().join("projects");
        std::fs::create_dir_all(&root).unwrap();
        let project = kandrium_core::create_project(&conn, &root, name, None, "blank").unwrap();
        let db = Arc::new(Mutex::new(conn));
        let mut state = ProjectsState::empty(root, None);
        state.selected = Some(project.id);
        {
            let guard = db.lock().unwrap();
            state.reload(&guard);
        }
        (tmp, db, state)
    }

    fn render_overview(
        ctx: &egui::Context,
        state: &mut ProjectsState,
        db: &Arc<Mutex<Connection>>,
        input: egui::RawInput,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(input, |ctx| {
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::Id::new("root"),
                egui::UiBuilder::new().max_rect(Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1000.0, 700.0),
                )),
            );
            let _ = show_tree(&mut ui, state, db);
            egui::Frame::new().show(&mut ui, |ui| {
                let _ = detail_tab_strip(ui, state, false);
                detail_body(ui, state, db, false);
            });
        });
        output.textures_delta.clear();
        output
    }

    fn painted_rect(output: &egui::FullOutput, text: &str) -> Option<Rect> {
        output.shapes.iter().find_map(|s| match &s.shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text == text => {
                let size = t.galley.size();
                let left = match t.galley.job.halign {
                    egui::Align::LEFT => t.pos.x,
                    egui::Align::Center => t.pos.x - size.x / 2.0,
                    egui::Align::RIGHT => t.pos.x - size.x,
                };
                Some(Rect::from_min_size(egui::pos2(left, t.pos.y), size))
            }
            _ => None,
        })
    }

    #[test]
    fn identifier_field_lowercases_and_accepts_ten_chars() {
        let (_tmp, db, mut state) = overview_fixture("Alpha");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);

        // Pin the draft to the selected project so the field does not re-seed
        // from the detail, then feed it mixed case and an 11th character.
        state.identifier_for = state.selected.clone();
        state.identifier_draft = "AbC-123456X".into();

        let output = render_overview(&ctx, &mut state, &db, Default::default());

        assert_eq!(state.identifier_draft, "abc-123456");
        assert!(
            !shapes_text(&output).contains("/10"),
            "the character counter should be gone"
        );
    }

    fn click_events(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn double_click_title_starts_inline_rename() {
        let (_tmp, db, mut state) = overview_fixture("Alpha");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);

        // Warm up the panel layout, then locate the title.
        render_overview(&ctx, &mut state, &db, Default::default());
        let output = render_overview(&ctx, &mut state, &db, Default::default());
        let title = painted_rect(&output, "Alpha").expect("title should be painted");
        let pos = title.center();

        // Two clicks inside the double-click window.
        let mut first = click_events(pos);
        first.insert(0, egui::Event::PointerMoved(pos));
        render_overview(
            &ctx,
            &mut state,
            &db,
            egui::RawInput {
                time: Some(0.1),
                events: first,
                ..Default::default()
            },
        );
        render_overview(
            &ctx,
            &mut state,
            &db,
            egui::RawInput {
                time: Some(0.2),
                events: click_events(pos),
                ..Default::default()
            },
        );

        let edit = state
            .title_edit
            .as_ref()
            .expect("double-click should start editing");
        assert_eq!(edit.project_id, state.selected.clone().unwrap());
        assert_eq!(edit.draft, "Alpha");
    }

    #[test]
    fn title_enter_renames_and_keeps_description_draft() {
        let (_tmp, db, mut state) = overview_fixture("Alpha");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);

        let project_id = state.selected.clone().unwrap();
        state.detail.as_mut().unwrap().description = "unsaved".to_string();
        state.desc_dirty = Some(Instant::now());
        state.title_edit = Some(TitleEdit {
            project_id: project_id.clone(),
            draft: "Renamed".into(),
            focus: false,
        });

        render_overview(
            &ctx,
            &mut state,
            &db,
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
        );

        assert!(state.title_edit.is_none());
        let detail = state.detail.as_ref().unwrap();
        assert_eq!(detail.name, "Renamed");
        assert_eq!(detail.description, "unsaved");
        assert_eq!(detail.identifier, "ren");
        assert!(state.identifier_for.is_none());

        let guard = db.lock().unwrap();
        let saved = kandrium_core::get_project(&guard, &project_id).unwrap();
        assert_eq!(saved.name, "Renamed");
        assert_eq!(saved.identifier, "ren");
    }

    #[test]
    fn title_escape_cancels_rename() {
        let (_tmp, db, mut state) = overview_fixture("Alpha");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);

        let project_id = state.selected.clone().unwrap();
        state.title_edit = Some(TitleEdit {
            project_id: project_id.clone(),
            draft: "Nope".into(),
            focus: false,
        });

        render_overview(
            &ctx,
            &mut state,
            &db,
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
        );

        assert!(state.title_edit.is_none());
        assert_eq!(state.detail.as_ref().unwrap().name, "Alpha");
        let guard = db.lock().unwrap();
        assert_eq!(
            kandrium_core::get_project(&guard, &project_id)
                .unwrap()
                .name,
            "Alpha"
        );
    }
}
