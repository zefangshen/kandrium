//! Application shell: data flow, header tabs, status banner, drag-and-drop
//! wiring, modals, and persistence.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use eframe::egui;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use kandrium_core::{
    AllBoard, ArchiveAllResult, ArchivedCard, Board, BoardWithColumns, Card, CardStatus,
    CreateCardBody, ProjectChoice,
};

use crate::anim;
use crate::dnd::{self, ColKey, Drag, DropZone};
use crate::theme;
use crate::views::{all_board, archive, board, documents, notes, projects};
use crate::watcher;
use crate::widgets::{card_modal, confirm};

const STATUS_TRANSIENT: Duration = Duration::from_secs(2);

/// Horizontal side margin of the toolbar panel. `header()` extends the
/// board-tabs hairline by this amount on each side so the divider spans
/// the full window width past the panel's content rect.
const TOOLBAR_H_MARGIN: i8 = 10;

/// Fallback vertical center (px from the window top) of the macOS
/// traffic-light buttons, used until `traffic_light_metrics()` measures the
/// live window. Measured: 14 px buttons 9 px from the window top.
const TRAFFIC_LIGHT_CENTER_Y: f32 = 16.0;

/// Fallback x (window coordinates) where title-bar content starts: the
/// standard traffic lights end at x = 69 (zoom-button right edge) and the
/// app name starts a 14 px gap after. Replaced by the measured
/// `traffic_light_metrics()` value once the window is up.
const TRAFFIC_LIGHT_INSET: f32 = 83.0;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Which view is displayed.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Board,
    Archived,
}

/// Top-level app section (header row 1).
#[derive(Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
enum Section {
    #[default]
    Tasks,
    Projects,
    Notes,
    Documents,
}

/// The currently loaded board (real or virtual).
enum BoardData {
    All(AllBoard),
    Real(Box<BoardWithColumns>),
}

/// Status banner entry.
struct StatusMsg {
    msg: String,
    is_error: bool,
    at: Instant,
}

/// An open card editor session.
struct EditSession {
    state: card_modal::CardEditState,
    /// Always false today: only non-archived cards open in the editor.
    archived: bool,
    /// Board the card lives on (for the "Move to board" picker).
    home_board_id: String,
    /// True when the card lives on a project board: the modal then offers the
    /// area picker instead of the project tag picker.
    is_project_card: bool,
    /// Normal boards a project card can be mirrored onto.
    area_targets: Vec<Board>,
    /// All projects, for tagging a normal-board card.
    project_targets: Vec<ProjectChoice>,
}

/// Persisted UI state.
#[derive(Serialize, Deserialize, Clone)]
struct SavedState {
    active_board_id: String,
    dark_mode: bool,
    #[serde(default)]
    section: Section,
    /// Open note tabs (root-relative paths) + active tab index.
    #[serde(default)]
    notes_open: Vec<String>,
    #[serde(default)]
    notes_active: Option<usize>,
    /// Open document tabs (root-relative paths) + active tab index.
    #[serde(default)]
    docs_open: Vec<String>,
    #[serde(default)]
    docs_active: Option<usize>,
    /// Projects section: last selected project + expanded folder ids.
    #[serde(default)]
    projects_selected: Option<String>,
    #[serde(default)]
    projects_expanded: Vec<String>,
    /// Project ids whose inline document trees are expanded in the project
    /// tree (persisted).
    #[serde(default)]
    projects_docs_expanded: Vec<String>,
    /// Left tree panel visibility per section (default: expanded).
    #[serde(default = "default_true")]
    notes_tree_open: bool,
    #[serde(default = "default_true")]
    docs_tree_open: bool,
    #[serde(default = "default_true")]
    projects_tree_open: bool,
    /// Overview Documents section body visibility.
    #[serde(default = "default_true")]
    projects_docs_open: bool,
}

/// Serde default for booleans that must start `true` in older saved sessions.
fn default_true() -> bool {
    true
}

impl Default for SavedState {
    fn default() -> Self {
        SavedState {
            active_board_id: kandrium_core::ALL_BOARD_ID.to_string(),
            dark_mode: true,
            section: Section::default(),
            notes_open: Vec::new(),
            notes_active: None,
            docs_open: Vec::new(),
            docs_active: None,
            projects_selected: None,
            projects_expanded: Vec::new(),
            projects_docs_expanded: Vec::new(),
            notes_tree_open: true,
            docs_tree_open: true,
            projects_tree_open: true,
            projects_docs_open: true,
        }
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct KandriumApp {
    db: Arc<Mutex<Connection>>,
    watcher_rx: mpsc::Receiver<()>,
    /// Set when an external change was seen (drives the "Synced" hint).
    synced_at: Option<Instant>,
    /// External change arrived mid-drag and was deferred; applied on the first
    /// frame after the drag ends (whether the drop landed or was cancelled).
    pending_sync: bool,

    saved: SavedState,
    view: View,

    boards: Vec<Board>,
    board: Option<BoardData>,
    /// Id of the board currently in `board` (drives context switches between
    /// the Tasks board and a project's embedded board).
    board_loaded_id: Option<String>,
    status: Option<StatusMsg>,

    // Archive view
    archive_state: archive::ArchiveState,
    archived: Option<Vec<ArchivedCard>>,

    // Notes section
    notes_state: notes::NotesState,

    // Projects section
    projects_state: projects::ProjectsState,

    // Documents section
    documents_state: documents::DocumentsState,

    // Drag and drop
    drag: Option<Drag>,
    zones: Vec<DropZone>,
    /// Drop-target gap kept open while the release flight (settle) runs.
    /// The placeholder stands in for the real card until the ghost lands.
    settle_gap: Option<(ColKey, usize)>,
    /// Slot the card was picked up from; the snap-back placeholder position
    /// when a drop lands outside every zone.
    source_gap: Option<(ColKey, usize)>,

    // Modals
    editing: Option<EditSession>,
    confirm: Option<confirm::ConfirmRequest>,

    // Inline editors (board view)
    add_col: Option<String>,
    add_draft: Option<String>,
    /// Stashed add-card drafts per column id; restored when the column's
    /// form reopens. Non-empty text only.
    draft_stash: HashMap<String, String>,

    // Inline add-card form (All board): one bucket at a time.
    /// Bucket whose inline add-card form is open (None = closed).
    all_add_status: Option<CardStatus>,
    /// Live title draft for that form.
    all_add_draft: Option<String>,
    /// Board the new card lands on. Session-only; seeded to "Career" each
    /// launch (falling back to the first visible board).
    all_add_board: Option<String>,
    /// Stashed All-board drafts per bucket; restored when the form reopens.
    all_draft_stash: HashMap<CardStatus, String>,

    /// Card whose title is being edited inline on the board (None = none).
    edit_card: Option<String>,
    /// Live text of the inline title editor (only meaningful while
    /// `edit_card` is set).
    edit_draft: String,

    // Dock-icon retry state for executables without a bundled .icns
    dock_icon_pending: bool,
    dock_icon_tries: u32,
}

impl KandriumApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        theme::install_fonts(&cc.egui_ctx);

        let saved: SavedState = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();
        theme::apply_visuals(&cc.egui_ctx, saved.dark_mode);

        let db = open_db_or_die();

        let (tx, rx) = mpsc::channel();
        watcher::spawn(tx, cc.egui_ctx.clone());

        let mut app = KandriumApp {
            db,
            watcher_rx: rx,
            synced_at: None,
            pending_sync: false,
            saved,
            view: View::Board,
            boards: Vec::new(),
            board: None,
            board_loaded_id: None,
            status: Some(StatusMsg {
                msg: "Loading...".to_string(),
                is_error: false,
                at: Instant::now(),
            }),
            archive_state: archive::ArchiveState::default(),
            archived: None,
            notes_state: notes::NotesState::new(),
            projects_state: projects::ProjectsState::new(),
            documents_state: documents::DocumentsState::new(),
            drag: None,
            zones: Vec::new(),
            settle_gap: None,
            source_gap: None,
            editing: None,
            confirm: None,
            add_col: None,
            add_draft: None,
            draft_stash: HashMap::new(),
            all_add_status: None,
            all_add_draft: None,
            all_add_board: None,
            all_draft_stash: HashMap::new(),
            edit_card: None,
            edit_draft: String::new(),
            dock_icon_pending: true,
            dock_icon_tries: 0,
        };
        // Restore the Projects section selection before the first load so the
        // overview pane comes back populated.
        let projects_selected = app.saved.projects_selected.clone();
        let projects_expanded = app.saved.projects_expanded.clone();
        let projects_docs_expanded = app.saved.projects_docs_expanded.clone();
        app.projects_state.restore_session(
            projects_selected,
            projects_expanded,
            projects_docs_expanded,
        );
        // Load once up front. Every other caller (`refresh`, `run`, tab
        // switches, external-sync events) runs only after the user acts, so
        // without this the first frame paints with `board: None` and the app
        // never leaves the "Loading..." state.
        app.reload_silent();
        // Restore last session's note tabs (vanished files are dropped).
        let notes_open = app.saved.notes_open.clone();
        let notes_active = app.saved.notes_active;
        app.notes_state.open_relative(&notes_open, notes_active);
        // Same for the Documents section.
        let docs_open = app.saved.docs_open.clone();
        let docs_active = app.saved.docs_active;
        app.documents_state.open_relative(&docs_open, docs_active);
        // Restore the per-section tree panel visibility.
        app.notes_state.tree_open = app.saved.notes_tree_open;
        app.documents_state.tree_open = app.saved.docs_tree_open;
        app.projects_state.tree_open = app.saved.projects_tree_open;
        app.projects_state.docs_open = app.saved.projects_docs_open;
        app
    }

    // -- data loading -------------------------------------------------------

    fn set_status(&mut self, msg: impl Into<String>, is_error: bool) {
        self.status = Some(StatusMsg {
            msg: msg.into(),
            is_error,
            at: Instant::now(),
        });
    }

    fn clear_status_if_loading(&mut self) {
        if let Some(s) = &self.status
            && s.msg == "Loading..."
            && !s.is_error
        {
            self.status = None;
        }
    }

    /// Load boards + the active board (or the archive list). Shows no
    /// "Loading..." flash — used after mutations and external syncs.
    fn reload_silent(&mut self) {
        // A board change hides the card whose title is being edited inline.
        // Save that draft before the old board data is replaced; the cards
        // themselves still exist in SQLite.
        let target_board_id = self.displayed_board_id();
        if self.board_loaded_id.as_deref() != Some(target_board_id.as_str()) {
            self.finish_active_inline_edit();
        }

        let db = self.db.clone();
        let Ok(db) = db.lock() else {
            self.set_status("Failed to load: database is locked", true);
            return;
        };

        match kandrium_core::list_boards(&db) {
            Ok(res) => self.boards = res.boards,
            Err(e) => self.set_status(format!("Failed to load: {}", e.0), true),
        }

        match self.view {
            View::Board => {
                let id = self.displayed_board_id();
                let project_board = self.saved.section == Section::Projects
                    && self.projects_state.detail_tab_is_board();
                let result = if id == kandrium_core::ALL_BOARD_ID {
                    kandrium_core::get_all_board(&db).map(BoardData::All)
                } else {
                    match kandrium_core::get_board(&db, &id) {
                        Ok(b) => Ok(BoardData::Real(Box::new(b))),
                        // The saved Tasks board vanished: fall back to the
                        // All board instead of leaving a dead tab active. A
                        // project board has no fallback — its tab shows the
                        // load error.
                        Err(e) if e.0 == "Board not found" && !project_board => {
                            self.saved.active_board_id = kandrium_core::ALL_BOARD_ID.to_string();
                            kandrium_core::get_all_board(&db).map(BoardData::All)
                        }
                        Err(e) => Err(e),
                    }
                };
                match result {
                    Ok(data) => {
                        self.board = Some(data);
                        self.board_loaded_id = Some(id);
                        self.clear_status_if_loading();
                    }
                    Err(e) => {
                        if project_board {
                            self.board = None;
                            // Remember the attempt so the mismatch check does
                            // not re-query every frame; an external change or
                            // tab switch retries.
                            self.board_loaded_id = Some(id);
                        }
                        self.set_status(format!("Failed to load board: {}", e.0), true);
                    }
                }
            }
            View::Archived => match kandrium_core::list_archived_cards(&db) {
                Ok(res) => {
                    self.archived = Some(res.cards);
                    self.clear_status_if_loading();
                }
                Err(e) => self.set_status(format!("Failed to load: {}", e.0), true),
            },
        }

        // Keep the Projects tree in sync with kanban/external changes too.
        self.projects_state.reload(&db);

        drop(db);
        self.reconcile_editing();
    }

    /// Full reload with the "Loading..." status (tab switch, manual refresh).
    fn refresh(&mut self) {
        self.set_status("Loading...", false);
        self.reload_silent();
    }

    /// Board the content area renders: the selected project's board while its
    /// detail Board tab is open, otherwise the saved Tasks board.
    fn displayed_board_id(&self) -> String {
        if self.saved.section == Section::Projects
            && self.projects_state.detail_tab_is_board()
            && let Some(id) = self.projects_state.selected_board_id()
        {
            return id;
        }
        self.saved.active_board_id.clone()
    }

    /// Close the card editor when its card disappeared (removed or archived
    /// externally), and drop any inline title edit whose card is gone.
    fn reconcile_editing(&mut self) {
        // Drop an inline title edit whose card is gone (removed or archived
        // externally).
        if let Some(id) = self.edit_card.clone()
            && self.find_card(&id).is_none()
        {
            self.edit_card = None;
        }
        let Some(editing) = &self.editing else { return };
        let card_id = &editing.state.card_id;
        let still_there = self.find_card(card_id).is_some();
        if !still_there {
            self.editing = None;
            self.set_status("This card was removed or archived externally", false);
        }
    }

    // -- helpers ------------------------------------------------------------

    /// All visible card ids (drag candidates).
    fn visible_card_ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(data) = &self.board {
            match data {
                BoardData::Real(b) => {
                    for col in &b.columns {
                        for card in &col.cards {
                            out.push(card.id.clone());
                        }
                    }
                }
                BoardData::All(b) => {
                    for col in &b.columns {
                        for all_card in &col.cards {
                            out.push(all_card.card.id.clone());
                        }
                    }
                }
            }
        }
        out
    }

    fn find_card(&self, card_id: &str) -> Option<(Card, String)> {
        let data = self.board.as_ref()?;
        match data {
            BoardData::Real(b) => b.columns.iter().find_map(|col| {
                col.cards
                    .iter()
                    .chain(col.mirrored.iter())
                    .find(|c| c.id == card_id)
                    .map(|c| (c.clone(), c.board_id.clone()))
            }),
            BoardData::All(b) => b.columns.iter().find_map(|col| {
                col.cards
                    .iter()
                    .find(|c| c.card.id == card_id)
                    .map(|c| (c.card.clone(), c.board_id.clone()))
            }),
        }
    }

    fn open_editor(&mut self, card_id: &str) {
        if let Some((card, home_board_id)) = self.find_card(card_id) {
            // Cards on project boards (including their mirrors on area boards)
            // are owned by a project; cards on normal boards can be tagged.
            let is_project_card = !self.boards.iter().any(|b| b.id == home_board_id);
            self.editing = Some(EditSession {
                state: card_modal::CardEditState::from_card(&card),
                archived: false,
                home_board_id,
                is_project_card,
                area_targets: self.boards.clone(),
                project_targets: self.projects_state.project_choices(),
            });
        }
    }

    /// Open the inline title editor on a card, seeding the draft with the
    /// current title.
    fn begin_inline_edit(&mut self, card_id: &str) {
        if let Some((card, _)) = self.find_card(card_id) {
            self.edit_card = Some(card_id.to_string());
            self.edit_draft = card.title;
        }
    }

    /// Commit the inline title editor: save the trimmed draft, or warn on an
    /// empty title (the editor closes either way).
    fn commit_inline_edit(&mut self, card_id: &str) {
        if self.finish_inline_edit(card_id) {
            self.reload_silent();
        }
    }

    /// Write the active inline title edit (if any) without reloading the board.
    /// Returns true when `card_id` was the active editor.
    fn finish_inline_edit(&mut self, card_id: &str) -> bool {
        if self.edit_card.as_deref() != Some(card_id) {
            return false;
        }
        let title = self.edit_draft.trim().to_string();
        self.edit_card = None;
        if title.is_empty() {
            self.set_status("Card title can't be empty", true);
            return true;
        }
        let db = self.db.clone();
        let outcome = match db.lock() {
            Ok(db) => kandrium_core::rename_card(&db, card_id, &title)
                .map(|_| ())
                .map_err(|e| format!("Save failed: {}", e.0)),
            Err(_) => Err("Save failed: database is locked".to_string()),
        };
        if let Err(msg) = outcome {
            self.set_status(msg, true);
        }
        true
    }

    /// Finish an active inline edit before a context switch that will reload
    /// the board itself.
    fn finish_active_inline_edit(&mut self) {
        if let Some(card_id) = self.edit_card.clone() {
            self.finish_inline_edit(&card_id);
        }
    }

    /// Finish an active inline edit and reload the currently displayed board.
    fn commit_active_inline_edit(&mut self) {
        if self.edit_card.is_some() {
            self.finish_active_inline_edit();
            self.reload_silent();
        }
    }

    /// Cancel the inline title editor (Esc), leaving the title unchanged.
    fn cancel_inline_edit(&mut self, card_id: &str) {
        if self.edit_card.as_deref() == Some(card_id) {
            self.edit_card = None;
        }
    }

    // -- mutations ----------------------------------------------------------

    fn run(
        &mut self,
        err_prefix: &str,
        f: impl FnOnce(&Connection) -> kandrium_core::Result<()>,
    ) -> bool {
        let db = self.db.clone();
        let outcome = match db.lock() {
            Ok(db) => f(&db).map_err(|e| format!("{}: {}", err_prefix, e.0)),
            Err(_) => Err(format!("{}: database is locked", err_prefix)),
        };
        let ok = match outcome {
            Ok(()) => true,
            Err(msg) => {
                self.set_status(msg, true);
                false
            }
        };
        self.reload_silent();
        ok
    }

    fn run_confirmed(&mut self, action: &confirm::ConfirmAction) {
        match action.clone() {
            confirm::ConfirmAction::DeleteColumn(id) => {
                self.run("Delete column failed", |db| {
                    kandrium_core::delete_column(db, &id).map(|_| ())
                });
            }
            confirm::ConfirmAction::DeleteCard(id) => {
                self.editing = None;
                self.run("Delete failed", |db| {
                    kandrium_core::delete_card(db, &id).map(|_| ())
                });
            }
            confirm::ConfirmAction::ArchiveAll(col_id) => {
                let mut count: Option<ArchiveAllResult> = None;
                self.run("Archive all failed", |db| {
                    count = Some(kandrium_core::archive_all_cards(db, &col_id)?);
                    Ok(())
                });
                if let Some(res) = count {
                    let n = res.archived_count;
                    self.set_status(
                        format!("Archived {} card{}", n, if n == 1 { "" } else { "s" }),
                        false,
                    );
                }
            }
            confirm::ConfirmAction::DeleteNotePath(path) => {
                match kandrium_core::notes::delete_path(&path, self.notes_state.root()) {
                    Ok(()) => {
                        self.notes_state.on_deleted(&path);
                        self.set_status("Deleted", false);
                    }
                    Err(e) => self.set_status(format!("Delete failed: {}", e.0), true),
                }
            }
            confirm::ConfirmAction::DeleteDocPath(path) => {
                match kandrium_core::documents::delete_path(&path, self.documents_state.root()) {
                    Ok(()) => {
                        self.documents_state.on_deleted(&path);
                        self.set_status("Deleted", false);
                    }
                    Err(e) => self.set_status(format!("Delete failed: {}", e.0), true),
                }
            }
            confirm::ConfirmAction::DeleteProject(id) => {
                let root = self.projects_state.root().to_path_buf();
                let db = self.db.clone();
                let result = match db.lock() {
                    Ok(db) => kandrium_core::projects::delete_project(&db, &root, &id),
                    Err(_) => Err(kandrium_core::KandriumError::new("database is locked")),
                };
                match result {
                    Ok(()) => self.set_status("Deleted", false),
                    Err(e) => self.set_status(format!("Delete failed: {}", e.0), true),
                }
                self.reload_silent();
            }
            confirm::ConfirmAction::DeleteProjectFolder(id) => {
                let root = self.projects_state.root().to_path_buf();
                let db = self.db.clone();
                let result = match db.lock() {
                    Ok(db) => kandrium_core::projects::delete_folder(&db, &root, &id),
                    Err(_) => Err(kandrium_core::KandriumError::new("database is locked")),
                };
                match result {
                    Ok(()) => self.set_status("Deleted", false),
                    Err(e) => self.set_status(format!("Delete failed: {}", e.0), true),
                }
                self.reload_silent();
            }
        }
    }

    // -- header -------------------------------------------------------------

    fn header(&mut self, ui: &mut egui::Ui) {
        let t = theme::Tokens::get(ui.ctx());
        ui.vertical(|ui| {
            // Row 0: macOS title-bar band. The window uses a fullsize
            // content view (`main.rs`), so this row is painted over the OS
            // title-bar strip. The traffic-light geometry is measured from
            // the live NSWindow (`traffic_light_metrics()`), so the app name
            // and the window controls (theme toggle, refresh, transient sync
            // hint) sit on the same center line as the buttons; standard
            // 28 px title-bar metrics apply until the measurement lands.
            let (center_y, title_x) = crate::traffic_light_metrics()
                .unwrap_or((TRAFFIC_LIGHT_CENTER_Y, TRAFFIC_LIGHT_INSET));
            let band_h = (2.0 * center_y).round().max(22.0);
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), band_h),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add_space(title_x - ui.cursor().left());
                    ui.label(egui::RichText::new("Kandrium").text_style(theme::strong()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Theme toggle
                        let icon = if self.saved.dark_mode { "sun" } else { "moon" };
                        if theme::icon_button(ui, icon, band_h, false, "Toggle theme").clicked() {
                            self.saved.dark_mode = !self.saved.dark_mode;
                            theme::apply_visuals(ui.ctx(), self.saved.dark_mode);
                        }

                        // Manual refresh
                        if theme::icon_button(ui, "rotate-cw", band_h, false, "Refresh").clicked() {
                            self.refresh();
                        }

                        // Synced hint
                        if let Some(t) = &self.synced_at
                            && t.elapsed() < STATUS_TRANSIENT
                        {
                            let r = ui.horizontal(|ui| {
                                let (icon_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(12.0, 12.0),
                                    egui::Sense::hover(),
                                );
                                crate::icons::paint(
                                    ui,
                                    icon_rect,
                                    "check",
                                    12.0,
                                    ui.visuals().weak_text_color(),
                                );
                                ui.label(
                                    egui::RichText::new("Synced")
                                        .small()
                                        .color(ui.visuals().weak_text_color()),
                                );
                            });
                            let _ = r;
                        }
                    });
                },
            );

            // Hairline between the title-bar band and the section row,
            // always visible, spanning the full toolbar width (content rect
            // plus `TOOLBAR_H_MARGIN` on each side).
            ui.add_space(2.0);
            let y = ui.cursor().top() + 0.5;
            let r = ui.max_rect();
            let bleed = f32::from(TOOLBAR_H_MARGIN);
            ui.painter().hline(
                r.left() - bleed..=r.right() + bleed,
                y,
                egui::Stroke::new(1.0, t.divider),
            );
            ui.add_space(5.0);

            // Row 1: top-level sections.
            ui.horizontal(|ui| {
                const SECTIONS: [Section; 4] = [
                    Section::Tasks,
                    Section::Projects,
                    Section::Notes,
                    Section::Documents,
                ];
                let segments = [
                    theme::Segment {
                        label: "Tasks",
                        icon: "square-kanban",
                    },
                    theme::Segment {
                        label: "Projects",
                        icon: "briefcase",
                    },
                    theme::Segment {
                        label: "Notes",
                        icon: "sticky-note",
                    },
                    theme::Segment {
                        label: "Documents",
                        icon: "folder",
                    },
                ];
                let selected = SECTIONS
                    .iter()
                    .position(|s| *s == self.saved.section)
                    .unwrap_or(0);
                if let Some(idx) = theme::segmented_control(ui, &segments, selected) {
                    self.switch_section(SECTIONS[idx], ui.ctx());
                }
            });

            // Row 2: board tabs (Tasks section only), smaller than row 1.
            if self.saved.section == Section::Tasks {
                ui.add_space(2.0);
                // Hairline between the section row and the board tabs,
                // spanning the full toolbar width (content rect plus
                // `TOOLBAR_H_MARGIN` on each side).
                let y = ui.cursor().top() + 0.5;
                let r = ui.max_rect();
                let bleed = f32::from(TOOLBAR_H_MARGIN);
                ui.painter().hline(
                    r.left() - bleed..=r.right() + bleed,
                    y,
                    egui::Stroke::new(1.0, t.divider),
                );
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    // Tabs: virtual All board first, then real boards.
                    let mut tabs: Vec<(String, String)> = std::iter::once((
                        kandrium_core::ALL_BOARD_ID.to_string(),
                        "All".to_string(),
                    ))
                    .chain(self.boards.iter().map(|b| (b.id.clone(), b.title.clone())))
                    .collect();

                    // A project-owned board is not in `self.boards` (project
                    // boards are hidden from the Tasks tab list); keep the
                    // currently open one visible so its tab stays active.
                    if self.view == View::Board
                        && !tabs.iter().any(|(id, _)| *id == self.saved.active_board_id)
                        && let Some(BoardData::Real(b)) = &self.board
                    {
                        tabs.push((b.id.clone(), b.title.clone()));
                    }

                    for (id, title) in &tabs {
                        let active = self.view == View::Board && *id == self.saved.active_board_id;
                        if theme::tab_button(
                            ui,
                            title,
                            active,
                            egui::TextStyle::Small,
                            theme::small_strong(),
                            24.0,
                        )
                        .clicked()
                        {
                            self.switch_board(id.clone(), ui.ctx());
                        }
                    }

                    // Archived view toggle
                    if theme::tab_button(
                        ui,
                        "Archived",
                        self.view == View::Archived,
                        egui::TextStyle::Small,
                        theme::small_strong(),
                        24.0,
                    )
                    .clicked()
                    {
                        self.finish_active_inline_edit();
                        self.view = View::Archived;
                        self.clear_inline_editors();
                        self.drag = None;
                        self.settle_gap = None;
                        self.source_gap = None;
                        anim::CardRig::clear_drag(ui.ctx());
                        self.refresh();
                    }
                });
            }
        });
    }

    fn switch_section(&mut self, section: Section, ctx: &egui::Context) {
        if self.saved.section == section {
            return;
        }
        self.finish_active_inline_edit();
        // Leaving Notes/Documents: flush dirty tabs so nothing is lost.
        if self.saved.section == Section::Notes {
            self.notes_state.flush_all();
        }
        if self.saved.section == Section::Documents {
            self.documents_state.flush_all();
        }
        if self.saved.section == Section::Projects {
            let db = self.db.clone();
            if let Ok(db) = db.lock() {
                self.projects_state.flush_all(&db);
            }
        }
        self.saved.section = section;
        self.clear_inline_editors();
        self.drag = None;
        self.settle_gap = None;
        self.source_gap = None;
        // Close modals belonging to the kanban UI being hidden.
        self.editing = None;
        self.confirm = None;
        anim::CardRig::clear_drag(ctx);
        // The displayed board depends on the section (project Board tab vs
        // the Tasks board), so reload it for the new context.
        self.reload_silent();
    }

    fn switch_board(&mut self, id: String, ctx: &egui::Context) {
        if self.view == View::Board && self.saved.active_board_id == id {
            return;
        }
        self.finish_active_inline_edit();
        self.saved.active_board_id = id;
        self.view = View::Board;
        self.clear_inline_editors();
        self.drag = None;
        self.settle_gap = None;
        self.source_gap = None;
        anim::CardRig::clear_drag(ctx);
        self.refresh();
    }

    fn clear_inline_editors(&mut self) {
        // Stash any open add-card draft so it survives the view switch.
        if let (Some(col_id), Some(draft)) = (self.add_col.take(), self.add_draft.take()) {
            let trimmed = draft.trim().to_string();
            if !trimmed.is_empty() {
                self.draft_stash.insert(col_id, trimmed);
            }
        }
        // Same for the All board's inline form.
        if let (Some(status), Some(draft)) = (self.all_add_status.take(), self.all_add_draft.take())
        {
            let trimmed = draft.trim().to_string();
            if !trimmed.is_empty() {
                self.all_draft_stash.insert(status, trimmed);
            }
        }
    }

    // -- content ------------------------------------------------------------

    fn content(&mut self, ui: &mut egui::Ui) {
        // Notes and Documents live outside the kanban UI entirely.
        match self.saved.section {
            Section::Projects => {
                let dark = self.saved.dark_mode;
                let db = self.db.clone();
                let actions = projects::show_tree(ui, &mut self.projects_state, &db);
                if let Some(kind) = actions.request_delete {
                    let (title, message) = match &kind {
                        projects::DeleteKind::Project { name, .. } => (
                            "Delete project",
                            format!(
                                "Permanently delete \"{name}\", its tasks and its documents? \
                                 This cannot be undone."
                            ),
                        ),
                        projects::DeleteKind::Folder { name, .. } => (
                            "Delete folder",
                            format!(
                                "Permanently delete \"{name}\" and every project inside it — \
                                 including their boards, tasks and documents? \
                                 This cannot be undone."
                            ),
                        ),
                    };
                    self.confirm = Some(confirm::ConfirmRequest {
                        title: title.to_string(),
                        message,
                        confirm_label: "Delete".to_string(),
                        action: match kind {
                            projects::DeleteKind::Project { id, .. } => {
                                confirm::ConfirmAction::DeleteProject(id)
                            }
                            projects::DeleteKind::Folder { id, .. } => {
                                confirm::ConfirmAction::DeleteProjectFolder(id)
                            }
                        },
                    });
                }
                if let Some(board_id) = actions.open_board {
                    self.switch_section(Section::Tasks, ui.ctx());
                    self.switch_board(board_id, ui.ctx());
                    return;
                }

                // Reserve a small gutter on the detail's left so it does not
                // sit flush against the tree panel (the page already insets
                // it by 12 px on the right). No fill: the pasteboard shows
                // through as the gap.
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: projects::PANE_GAP as i8,
                        right: 0,
                        top: 0,
                        bottom: 0,
                    })
                    .show(ui, |ui| {
                        let kind = projects::detail_tab_strip(ui, &mut self.projects_state, dark);
                        if kind == Some(projects::DetailKind::Board) {
                            // `content_board` bails on the archive view; the
                            // project board always draws the board.
                            self.view = View::Board;
                            // The strip can switch tabs mid-frame (e.g. the
                            // overview's Open board pill), so compare against
                            // the board actually held in memory.
                            let id = self.displayed_board_id();
                            if self.board_loaded_id.as_deref() != Some(id.as_str()) {
                                self.reload_silent();
                            }
                            self.content_board(ui);
                        } else {
                            // Leaving the project board hides the inline title
                            // editor; save it rather than dropping the draft.
                            self.commit_active_inline_edit();
                            projects::detail_body(ui, &mut self.projects_state, &db, dark);
                        }
                    });
                return;
            }
            Section::Notes => {
                let dark = self.saved.dark_mode;
                let actions = notes::show(ui, &mut self.notes_state, dark);
                if let Some(path) = actions.request_delete {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "this item".to_string());
                    // Folder deletes are recursive (core uses remove_dir_all),
                    // so the copy must say more than the single-file variant.
                    let (title, message) = if path.is_dir() {
                        (
                            "Delete folder",
                            format!(
                                "Permanently delete \"{name}\" and every note inside it? \
                                 This cannot be undone."
                            ),
                        )
                    } else {
                        (
                            "Delete note",
                            format!("Permanently delete \"{name}\"? This cannot be undone."),
                        )
                    };
                    self.confirm = Some(confirm::ConfirmRequest {
                        title: title.to_string(),
                        message,
                        confirm_label: "Delete".to_string(),
                        action: confirm::ConfirmAction::DeleteNotePath(path),
                    });
                }
                return;
            }
            Section::Documents => {
                let dark = self.saved.dark_mode;
                let actions = documents::show(ui, &mut self.documents_state, dark);
                if let Some(path) = actions.request_delete {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "this item".to_string());
                    let (title, message) = if path.is_dir() {
                        (
                            "Delete folder",
                            format!(
                                "Permanently delete \"{name}\" and every PDF inside it? \
                                 This cannot be undone."
                            ),
                        )
                    } else {
                        (
                            "Delete PDF",
                            format!(
                                "Permanently delete \"{name}\" and its annotations? \
                                 This cannot be undone."
                            ),
                        )
                    };
                    self.confirm = Some(confirm::ConfirmRequest {
                        title: title.to_string(),
                        message,
                        confirm_label: "Delete".to_string(),
                        action: confirm::ConfirmAction::DeleteDocPath(path),
                    });
                }
                return;
            }
            Section::Tasks => {}
        }

        if self.view == View::Archived {
            self.content_archived(ui);
            return;
        }

        self.content_board(ui);
    }

    /// Board body shared by the Tasks section and the project Board tab.
    fn content_board(&mut self, ui: &mut egui::Ui) {
        let drag = self.drag.clone();

        // Drop the settle placeholder once the release flight has finished;
        // only then may the real card re-enter the layout.
        if self.settle_gap.is_some() && !anim::CardRig::is_settling(ui.ctx()) {
            self.settle_gap = None;
            self.source_gap = None;
        }

        // Resolve the insertion gap against LAST frame's zones (they are
        // recollected during this frame's render). The one-frame lag is
        // imperceptible at 60fps and avoids needing layout before it exists.
        // While the release flight runs, the placeholder stays parked at the
        // drop target as a transparent indicator.
        let hover = if drag.is_some() {
            ui.ctx()
                .input(|i| i.pointer.latest_pos())
                .and_then(|pos| dnd::hit_test(&self.zones, pos))
                .map(|(zone_idx, insert_idx)| (self.zones[zone_idx].key.clone(), insert_idx))
        } else {
            self.settle_gap.clone()
        };

        self.zones.clear();

        // Views treat the settling card exactly like a held one: the real
        // card stays hidden (the placeholder stands in for it) until the
        // ghost has landed.
        let settle_drag = anim::CardRig::settling_card_id(ui.ctx()).map(|card_id| Drag { card_id });
        let view_drag = drag.as_ref().or(settle_drag.as_ref());

        // If the load failed, show the error instead of a spinner that never
        // resolves (`board` stays `None` on failure).
        let load_error = self
            .status
            .as_ref()
            .filter(|s| s.is_error)
            .map(|s| s.msg.clone());

        match &mut self.board {
            None => {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| match &load_error {
                    Some(msg) => {
                        ui.colored_label(ui.visuals().error_fg_color, msg);
                    }
                    None => {
                        ui.weak("Loading...");
                    }
                });
            }
            Some(BoardData::Real(data)) => {
                let add_col = self.add_col.clone();
                let edit_card = self.edit_card.clone();
                let actions = {
                    let add_draft = &mut self.add_draft;
                    board::show(
                        ui,
                        data,
                        view_drag,
                        &mut self.zones,
                        add_col.as_deref(),
                        add_draft,
                        hover,
                        edit_card.as_deref(),
                        &mut self.edit_draft,
                    )
                };
                self.handle_board_actions(actions);
            }
            Some(BoardData::All(data)) => {
                let edit_card = self.edit_card.clone();
                let actions = all_board::show(
                    ui,
                    data,
                    &self.boards,
                    view_drag,
                    &mut self.zones,
                    self.all_add_status,
                    self.all_add_board.as_deref(),
                    &mut self.all_add_draft,
                    hover,
                    edit_card.as_deref(),
                    &mut self.edit_draft,
                );
                self.handle_all_board_actions(actions);
            }
        }

        // Lift/fly-back ghost above everything, then pump frames while any
        // animation is running. egui otherwise repaints on demand only.
        anim::CardRig::draw_ghost(ui.ctx());
        if anim::CardRig::needs_repaint(ui.ctx()) {
            ui.ctx().request_repaint();
        }

        self.process_drag(ui.ctx());
    }

    fn content_archived(&mut self, ui: &mut egui::Ui) {
        let Some(cards) = self.archived.clone() else {
            let load_error = self
                .status
                .as_ref()
                .filter(|s| s.is_error)
                .map(|s| s.msg.clone());
            ui.add_space(24.0);
            ui.vertical_centered(|ui| match &load_error {
                Some(msg) => {
                    ui.colored_label(ui.visuals().error_fg_color, msg);
                }
                None => {
                    ui.weak("Loading archived cards...");
                }
            });
            return;
        };
        let actions = archive::show(ui, &cards, &self.boards, &mut self.archive_state);
        if let Some(id) = actions.restore {
            self.run("Restore failed", |db| {
                kandrium_core::set_card_archived(db, &id, false).map(|_| ())
            });
        }
        if let Some(id) = actions.request_delete {
            self.confirm = Some(confirm::ConfirmRequest {
                title: "Delete archived card".to_string(),
                message: "Permanently delete this archived card? This cannot be undone."
                    .to_string(),
                confirm_label: "Delete".to_string(),
                action: confirm::ConfirmAction::DeleteCard(id),
            });
        }
    }

    // -- action routing -----------------------------------------------------

    fn handle_board_actions(&mut self, actions: board::BoardActions) {
        if let Some(card_id) = actions.open_card {
            self.open_editor(&card_id);
        }
        if let Some(card_id) = actions.edit_commit {
            self.commit_inline_edit(&card_id);
        }
        if let Some(card_id) = actions.edit_cancel {
            self.cancel_inline_edit(&card_id);
        }
        if let Some(card_id) = actions.edit_card {
            self.begin_inline_edit(&card_id);
        }
        if let Some((card_id, to_col, before)) = actions.card_drop {
            self.drag = None;
            self.run("Move failed", |db| {
                kandrium_core::move_card(db, &card_id, &to_col, before.as_deref()).map(|_| ())
            });
        }
        if let Some((col_id, before_col)) = actions.column_drop {
            self.run("Move column failed", |db| {
                kandrium_core::move_column(db, &col_id, before_col.as_deref()).map(|_| ())
            });
        }
        if let Some(col_id) = actions.archive_all {
            self.confirm = Some(confirm::ConfirmRequest {
                title: "Archive all cards".to_string(),
                message:
                    "Archive all cards in this column? They can be restored from the Archived view."
                        .to_string(),
                confirm_label: "Archive all".to_string(),
                action: confirm::ConfirmAction::ArchiveAll(col_id),
            });
        }
        if let Some(col_id) = actions.delete_column {
            self.confirm = Some(confirm::ConfirmRequest {
                title: "Delete column".to_string(),
                message: "Delete this column and all its cards?".to_string(),
                confirm_label: "Delete".to_string(),
                action: confirm::ConfirmAction::DeleteColumn(col_id),
            });
        }
        if let Some(card_id) = actions.archive_card {
            self.run("Archive failed", |db| {
                kandrium_core::set_card_archived(db, &card_id, true).map(|_| ())
            });
        }
        if let Some(card_id) = actions.delete_card {
            self.confirm = Some(confirm::ConfirmRequest {
                title: "Delete card".to_string(),
                message: "Delete this card permanently?".to_string(),
                confirm_label: "Delete".to_string(),
                action: confirm::ConfirmAction::DeleteCard(card_id),
            });
        }
        if let Some(col_id) = actions.open_add_form {
            // Switching columns mid-draft: stash the outgoing column's text.
            if let (Some(prev_col), Some(prev_draft)) = (self.add_col.take(), self.add_draft.take())
            {
                let trimmed = prev_draft.trim().to_string();
                if !trimmed.is_empty() && prev_col != col_id {
                    self.draft_stash.insert(prev_col, trimmed);
                }
            }
            self.add_col = Some(col_id.clone());
            // Restore a previously stashed draft for this column, if any.
            self.add_draft = Some(self.draft_stash.remove(&col_id).unwrap_or_default());
        }
        if let Some((col_id, title)) = actions.create_card {
            let created = self.run("Add card failed", |db| {
                kandrium_core::create_card(
                    db,
                    &col_id,
                    &CreateCardBody {
                        title,
                        description: Some(String::new()),
                        due_date: None,
                        labels: None,
                    },
                )
                .map(|_| ())
            });
            if created {
                self.add_col = None;
                self.add_draft = None;
                self.draft_stash.remove(&col_id);
            }
        }
        if let Some(col_id) = actions.cancel_add
            && self.add_col.as_deref() == Some(col_id.as_str())
        {
            // Stash the draft so reopening the form restores it.
            if let Some(draft) = self.add_draft.take() {
                let trimmed = draft.trim().to_string();
                if !trimmed.is_empty() {
                    self.draft_stash.insert(col_id, trimmed);
                }
            }
            self.add_col = None;
        }
    }

    fn handle_all_board_actions(&mut self, actions: all_board::AllBoardActions) {
        if let Some(card_id) = actions.open_card {
            self.open_editor(&card_id);
        }
        if let Some(card_id) = actions.edit_commit {
            self.commit_inline_edit(&card_id);
        }
        if let Some(card_id) = actions.edit_cancel {
            self.cancel_inline_edit(&card_id);
        }
        if let Some(card_id) = actions.edit_card {
            self.begin_inline_edit(&card_id);
        }
        if let Some((card_id, to_status, before)) = actions.card_drop {
            self.drag = None;
            self.run("Move failed", |db| {
                kandrium_core::move_all_card(db, &card_id, to_status, before.as_deref()).map(|_| ())
            });
        }
        if actions.archive_all_done {
            let col_id = format!("{}done", kandrium_core::ALL_COLUMN_PREFIX);
            self.confirm = Some(confirm::ConfirmRequest {
                title: "Archive all cards".to_string(),
                message: "Archive all cards in Done? They can be restored from the Archived view."
                    .to_string(),
                confirm_label: "Archive all".to_string(),
                action: confirm::ConfirmAction::ArchiveAll(col_id),
            });
        }
        if let Some(card_id) = actions.archive_card {
            self.run("Archive failed", |db| {
                kandrium_core::set_card_archived(db, &card_id, true).map(|_| ())
            });
        }
        if let Some(card_id) = actions.delete_card {
            self.confirm = Some(confirm::ConfirmRequest {
                title: "Delete card".to_string(),
                message: "Delete this card permanently?".to_string(),
                confirm_label: "Delete".to_string(),
                action: confirm::ConfirmAction::DeleteCard(card_id),
            });
        }
        if let Some(status) = actions.open_add_form {
            self.open_all_add_form(status);
        }
        if let Some(board_id) = actions.choose_board {
            self.all_add_board = Some(board_id);
        }
        if let Some((status, board_id, title)) = actions.create_card {
            let created = self.run("Add card failed", |db| {
                kandrium_core::create_card_in_status(
                    db,
                    &board_id,
                    status,
                    &CreateCardBody {
                        title,
                        description: Some(String::new()),
                        due_date: None,
                        labels: None,
                    },
                )
                .map(|_| ())
            });
            if created {
                self.all_add_status = None;
                self.all_add_draft = None;
                self.all_draft_stash.remove(&status);
            }
        }
        if let Some(status) = actions.cancel_add
            && self.all_add_status == Some(status)
        {
            // Stash the draft so reopening the form restores it.
            if let Some(draft) = self.all_add_draft.take() {
                let trimmed = draft.trim().to_string();
                if !trimmed.is_empty() {
                    self.all_draft_stash.insert(status, trimmed);
                }
            }
            self.all_add_status = None;
        }
    }

    /// Open the All board's inline add-card form for `status`, stashing any
    /// draft from another bucket and seeding the target board (Career by
    /// default, else the first visible board).
    fn open_all_add_form(&mut self, status: CardStatus) {
        if self.all_add_status == Some(status) {
            return;
        }
        if let (Some(prev), Some(draft)) = (self.all_add_status.take(), self.all_add_draft.take()) {
            let trimmed = draft.trim().to_string();
            if !trimmed.is_empty() {
                self.all_draft_stash.insert(prev, trimmed);
            }
        }
        self.all_add_status = Some(status);
        self.all_add_draft = Some(self.all_draft_stash.remove(&status).unwrap_or_default());

        let still_valid = self
            .all_add_board
            .as_ref()
            .is_some_and(|id| self.boards.iter().any(|b| &b.id == id));
        if !still_valid {
            self.all_add_board = self
                .boards
                .iter()
                .find(|b| b.title.eq_ignore_ascii_case("career"))
                .or_else(|| self.boards.first())
                .map(|b| b.id.clone());
        }
    }

    // -- drag and drop ------------------------------------------------------

    /// Detect drag starts and stops from the per-card widget responses.
    fn process_drag(&mut self, ctx: &egui::Context) {
        if self.drag.is_none() {
            for card_id in self.visible_card_ids() {
                if let Some(resp) = ctx.read_response(dnd::card_widget_id(&card_id))
                    && resp.dragged()
                {
                    // Snapshot the card for the floating ghost before any
                    // layout change happens.
                    let pointer = ctx.input(|i| i.pointer.latest_pos());
                    if let (Some((card, board_title)), Some(pointer)) =
                        (self.drag_snapshot(&card_id), pointer)
                    {
                        anim::CardRig::begin_drag(ctx, card, board_title, pointer);
                        // Remember the source slot: on a cancelled drop
                        // the placeholder parks here while the ghost
                        // flies back (Trello-style snap-back).
                        self.source_gap =
                            dnd::hit_test(&self.zones, pointer).map(|(zone_idx, insert_idx)| {
                                (self.zones[zone_idx].key.clone(), insert_idx)
                            });
                    }
                    self.drag = Some(Drag { card_id });
                    return;
                }
            }
        }

        if self.drag.is_some() {
            let (released, pos) = ctx.input(|i| (i.pointer.any_released(), i.pointer.latest_pos()));
            if released {
                let drag = self.drag.take();
                // Begin the flight first so it freezes the rect the ghost is
                // flying from. With no hit (or no pointer) the data stays put
                // and the ghost animates back to its source — the snap-back.
                anim::CardRig::begin_settle(ctx, pos);
                // Park the placeholder at the drop target (or, for a cancel,
                // back at the source slot) until the flight completes — the
                // real card only re-enters the layout afterwards.
                self.settle_gap = if anim::CardRig::is_settling(ctx) {
                    pos.and_then(|p| dnd::hit_test(&self.zones, p))
                        .map(|(zone_idx, insert_idx)| {
                            (self.zones[zone_idx].key.clone(), insert_idx)
                        })
                        .or_else(|| self.source_gap.clone())
                } else {
                    None
                };
                if let (Some(drag), Some(pos)) = (drag, pos)
                    && let Some((zone_idx, insert_idx)) = dnd::hit_test(&self.zones, pos)
                {
                    self.execute_drop_at(&drag, zone_idx, insert_idx);
                }
            }
        }
    }

    /// Card content + board badge title for the drag ghost snapshot.
    fn drag_snapshot(&self, card_id: &str) -> Option<(Card, Option<String>)> {
        let data = self.board.as_ref()?;
        match data {
            BoardData::Real(b) => b.columns.iter().find_map(|col| {
                col.cards
                    .iter()
                    .find(|c| c.id == card_id)
                    .map(|c| (c.clone(), None))
            }),
            BoardData::All(b) => b.columns.iter().find_map(|col| {
                col.cards
                    .iter()
                    .find(|c| c.card.id == card_id)
                    .map(|c| (c.card.clone(), Some(c.board_title.clone())))
            }),
        }
    }

    fn execute_drop_at(&mut self, drag: &Drag, zone_idx: usize, insert_idx: usize) {
        let before = self.zones[zone_idx]
            .before_card_at(insert_idx)
            .map(str::to_string);
        let key = self.zones[zone_idx].key.clone();
        let card_id = drag.card_id.clone();

        match key {
            ColKey::Column(to_col) => {
                self.run("Move failed", |db| {
                    kandrium_core::move_card(db, &card_id, &to_col, before.as_deref()).map(|_| ())
                });
            }
            ColKey::Bucket(to_status) => {
                self.run("Move failed", |db| {
                    kandrium_core::move_all_card(db, &card_id, to_status, before.as_deref())
                        .map(|_| ())
                });
            }
        }
    }

    // -- modals -------------------------------------------------------------

    fn modals(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // Confirm dialog (may sit on top of the editor).
        if let Some(request) = self.confirm.clone() {
            match confirm::show(&ctx, &request, "kandrium-confirm-modal") {
                confirm::ConfirmOutcome::None => {}
                confirm::ConfirmOutcome::Confirmed => {
                    self.confirm = None;
                    self.run_confirmed(&request.action);
                }
                confirm::ConfirmOutcome::Cancelled => {
                    self.confirm = None;
                }
            }
        }

        // Card editor.
        if let Some(editing) = self.editing.as_mut() {
            let archived = editing.archived;
            let card_id = editing.state.card_id.clone();
            let move_targets: Vec<Board> = self
                .boards
                .iter()
                .filter(|b| b.id != editing.home_board_id)
                .cloned()
                .collect();

            match card_modal::show(
                &ctx,
                &mut editing.state,
                archived,
                &move_targets,
                &editing.area_targets,
                &editing.project_targets,
                editing.is_project_card,
            ) {
                card_modal::CardModalOutcome::None => {}
                card_modal::CardModalOutcome::Closed => self.editing = None,
                card_modal::CardModalOutcome::Action(actions) => {
                    if actions.save {
                        let body = editing.state.to_body();
                        let project_id = editing.state.project_id.clone();
                        let area_board_id = if editing.state.area_enabled {
                            editing.state.area_board_id.clone()
                        } else {
                            None
                        };
                        let is_project_card = editing.is_project_card;
                        self.editing = None;
                        self.run("Save failed", |db| {
                            kandrium_core::update_card(db, &card_id, &body)?;
                            if is_project_card {
                                kandrium_core::set_card_area(
                                    db,
                                    &card_id,
                                    area_board_id.as_deref(),
                                )?;
                            } else {
                                kandrium_core::set_card_project(
                                    db,
                                    &card_id,
                                    project_id.as_deref(),
                                )?;
                            }
                            Ok(())
                        });
                    }
                    if actions.toggle_archive {
                        self.editing = None;
                        self.run("Archive failed", |db| {
                            kandrium_core::set_card_archived(db, &card_id, !archived).map(|_| ())
                        });
                    }
                    if actions.delete {
                        self.confirm = Some(confirm::ConfirmRequest {
                            title: "Delete card".to_string(),
                            message: "Delete this card permanently?".to_string(),
                            confirm_label: "Delete".to_string(),
                            action: confirm::ConfirmAction::DeleteCard(card_id.clone()),
                        });
                    }
                    if let Some(target) = actions.move_to_board {
                        self.editing = None;
                        let title = self
                            .boards
                            .iter()
                            .find(|b| b.id == target)
                            .map(|b| b.title.clone())
                            .unwrap_or(target.clone());
                        self.run("Move to board failed", |db| {
                            kandrium_core::move_card_to_board(db, &card_id, &target).map(|_| ())
                        });
                        self.set_status(format!("Moved card to \"{}\"", title), false);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// eframe integration
// ---------------------------------------------------------------------------

impl eframe::App for KandriumApp {
    /// Drain watcher events and expire transient banners before painting.
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Bare `cargo run` binaries and the minimal dev `.app` carry no
        // bundle .icns; apply the dock icon from the first frame on. egui
        // repaints on demand, so request repaints until the icon is
        // confirmed applied (~2s budget).
        if self.dock_icon_pending {
            _ctx.request_repaint();
            self.dock_icon_tries += 1;
            let applied = crate::set_dock_icon();
            if applied || self.dock_icon_tries >= 120 {
                self.dock_icon_pending = false;
                if !applied {
                    eprintln!(
                        "[kandrium] dock icon not applied after {} frames",
                        self.dock_icon_tries
                    );
                }
            }
        }

        if self.watcher_rx.try_recv().is_ok() {
            if self.drag.is_none() {
                self.reload_silent();
                self.synced_at = Some(Instant::now());
            } else {
                // Mid-drag: a reload would yank the board out from under the
                // gesture. Defer instead of dropping the event — a cancelled
                // drag never reaches the drop handler's reload.
                self.pending_sync = true;
            }
        }
        // The drag ended (drop landed or cancelled): apply any deferred sync.
        if self.pending_sync && self.drag.is_none() {
            self.pending_sync = false;
            self.reload_silent();
            self.synced_at = Some(Instant::now());
        }

        // Expire transient non-error banners.
        if let Some(s) = &self.status
            && !s.is_error
            && s.msg != "Loading..."
            && s.at.elapsed() > STATUS_TRANSIENT
        {
            self.status = None;
        }
        if let Some(t) = &self.synced_at
            && t.elapsed() > STATUS_TRANSIENT
        {
            self.synced_at = None;
        }

        // Notes/Documents auto-save: flush tabs whose debounce elapsed. egui
        // repaints on demand, so keep scheduling frames while anything stays
        // dirty.
        if self.saved.section == Section::Notes && self.notes_state.tick_autosave() {
            _ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.saved.section == Section::Documents && self.documents_state.tick_autosave() {
            _ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.saved.section == Section::Projects {
            let db = self.db.clone();
            if let Ok(db) = db.lock()
                && self.projects_state.tick_autosave(&db)
            {
                _ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let t = theme::Tokens::get(&ctx);

        // Toolbar: titlebar fill; `header()` paints the hairline under the
        // section row. No top margin: the first header row is the macOS
        // title-bar band, which must start at the window edge to line up
        // with the traffic-light buttons.
        egui::Panel::top("kandrium-toolbar")
            .frame(
                egui::Frame::new()
                    .fill(t.titlebar)
                    .inner_margin(egui::Margin {
                        left: TOOLBAR_H_MARGIN,
                        right: TOOLBAR_H_MARGIN,
                        top: 0,
                        bottom: 8,
                    }),
            )
            .show(ui, |ui| {
                self.header(ui);
            });

        // Main content on the pasteboard background, inset from the
        // toolbar and the window edges.
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(t.pasteboard)
                    .inner_margin(egui::Margin::symmetric(12, 10)),
            )
            .show(ui, |ui| {
                self.content(ui);
                self.modals(ui);
            });

        // Floating toast for transient status / errors.
        if let Some(s) = &self.status {
            let expired = !s.is_error && s.msg != "Loading..." && s.at.elapsed() > STATUS_TRANSIENT;
            if !expired {
                theme::toast(&ctx, &s.msg);
            }
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // Flush notes + documents and remember the tab sessions for next
        // launch.
        self.notes_state.flush_all();
        self.saved.notes_open = self.notes_state.rel_paths();
        self.saved.notes_active = self.notes_state.active_index();
        self.documents_state.flush_all();
        self.saved.docs_open = self.documents_state.rel_paths();
        self.saved.docs_active = self.documents_state.active_index();
        // Flush any pending project description and remember the tree session.
        let db = self.db.clone();
        if let Ok(db) = db.lock() {
            self.projects_state.flush_all(&db);
        }
        self.saved.projects_selected = self.projects_state.selected_id();
        self.saved.projects_expanded = self.projects_state.expanded_ids();
        self.saved.projects_docs_expanded = self.projects_state.docs_expanded_ids();
        self.saved.notes_tree_open = self.notes_state.tree_open;
        self.saved.docs_tree_open = self.documents_state.tree_open;
        self.saved.projects_tree_open = self.projects_state.tree_open;
        self.saved.projects_docs_open = self.projects_state.docs_open;
        eframe::set_value(storage, eframe::APP_KEY, &self.saved);
    }
}

// ---------------------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------------------

fn open_db_or_die() -> Arc<Mutex<Connection>> {
    let path = kandrium_core::resolve_db_path();
    match kandrium_core::migrate_legacy_db() {
        Ok(Some(from)) => eprintln!(
            "[kandrium-app] migrated database from {} to {}",
            from.display(),
            path.display()
        ),
        Ok(None) => {}
        Err(e) => eprintln!("[kandrium-app] legacy database migration failed: {e}"),
    }
    if let Some(dir) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match kandrium_core::open_db(&path) {
        Ok(mut db) => {
            if let Err(e) = kandrium_core::bootstrap(&mut db) {
                eprintln!("[kandrium-app] bootstrap failed: {}", e.0);
            }
            Arc::new(Mutex::new(db))
        }
        Err(e) => {
            eprintln!(
                "[kandrium-app] cannot open database at {}: {}",
                path.display(),
                e
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The title-bar band layout must center its content on the traffic
    /// lights' measured center line, with the title starting at the measured
    /// `title_x`. Headless regression for the macOS fullsize-content-view
    /// title bar (traffic lights at center_y=16 on current macOS).
    #[test]
    fn title_band_centers_content_on_traffic_light_line() {
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        theme::apply_visuals(&ctx, false);

        // Measured on current macOS: lights centered 16 px from the window
        // top (buttons 14 px tall, 9 px from the top), title starting 14 px
        // right of the zoom button (right edge x=69).
        let center_y = 16.0_f32;
        let title_x = 83.0_f32;
        let band_h = (2.0 * center_y).round().max(22.0);

        let mut out = ctx.run_ui(egui::RawInput::default(), |ctx| {
            // Toolbar content rect: panel side margin, top at the window edge.
            let content = egui::Rect::from_min_size(
                egui::pos2(f32::from(TOOLBAR_H_MARGIN), 0.0),
                egui::vec2(600.0, 300.0),
            );
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::Id::new("band"),
                egui::UiBuilder::new().max_rect(content),
            );
            ui.vertical(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), band_h),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add_space(title_x - ui.cursor().left());
                        ui.label(egui::RichText::new("Kandrium").text_style(theme::strong()));
                    },
                );
            });
        });

        let mut title_rect = None;
        for clipped in &out.shapes {
            if let egui::Shape::Text(text) = &clipped.shape
                && text.galley.text() == "Kandrium"
            {
                title_rect = Some(egui::Rect::from_min_size(text.pos, text.galley.size()));
            }
        }
        out.textures_delta.clear();

        let r = title_rect.expect("Kandrium label should be painted in the band");
        assert!(
            (r.center().y - center_y).abs() <= 1.0,
            "title center y {} should match traffic-light center {center_y} \
             (band height {band_h}, rect {r:?})",
            r.center().y
        );
        assert!(
            (r.left() - title_x).abs() <= 1.0,
            "title left {} should start at measured title_x {title_x}",
            r.left()
        );
    }
}
