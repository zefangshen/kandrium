//! Application shell: data flow, header tabs, status banner, drag-and-drop
//! wiring, modals, and persistence.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use eframe::egui;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use kandrium_core::{
    AllBoard, ArchiveAllResult, ArchivedCard, Board, BoardWithColumns, Card, CreateCardBody,
};

use crate::anim;
use crate::dnd::{self, ColKey, Drag, DropZone};
use crate::theme;
use crate::views::{all_board, archive, board};
use crate::watcher;
use crate::widgets::{card_modal, confirm};

const STATUS_TRANSIENT: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Which view is displayed.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Board,
    Archived,
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
}

/// Persisted UI state.
#[derive(Serialize, Deserialize, Clone)]
struct SavedState {
    active_board_id: String,
    dark_mode: bool,
}

impl Default for SavedState {
    fn default() -> Self {
        SavedState {
            active_board_id: kandrium_core::ALL_BOARD_ID.to_string(),
            dark_mode: true,
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
    status: Option<StatusMsg>,

    // Archive view
    archive_state: archive::ArchiveState,
    archived: Option<Vec<ArchivedCard>>,

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

    // Dock-icon retry state for bare `cargo run` binaries (no bundle .icns)
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
            status: Some(StatusMsg {
                msg: "Loading...".to_string(),
                is_error: false,
                at: Instant::now(),
            }),
            archive_state: archive::ArchiveState::default(),
            archived: None,
            drag: None,
            zones: Vec::new(),
            settle_gap: None,
            source_gap: None,
            editing: None,
            confirm: None,
            add_col: None,
            add_draft: None,
            draft_stash: HashMap::new(),
            dock_icon_pending: true,
            dock_icon_tries: 0,
        };
        // Load once up front. Every other caller (`refresh`, `run`, tab
        // switches, external-sync events) runs only after the user acts, so
        // without this the first frame paints with `board: None` and the app
        // never leaves the "Loading..." state.
        app.reload_silent();
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
                let id = self.saved.active_board_id.clone();
                let result = if id == kandrium_core::ALL_BOARD_ID {
                    kandrium_core::get_all_board(&db).map(BoardData::All)
                } else {
                    kandrium_core::get_board(&db, &id).map(|b| BoardData::Real(Box::new(b)))
                };
                match result {
                    Ok(data) => {
                        self.board = Some(data);
                        self.clear_status_if_loading();
                    }
                    Err(e) => {
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

        drop(db);
        self.reconcile_editing();
    }

    /// Full reload with the "Loading..." status (tab switch, manual refresh).
    fn refresh(&mut self) {
        self.set_status("Loading...", false);
        self.reload_silent();
    }

    /// Close the card editor when its card disappeared (removed or archived
    /// externally).
    fn reconcile_editing(&mut self) {
        let Some(editing) = &self.editing else { return };
        let card_id = &editing.state.card_id;
        let still_there = match &self.board {
            Some(BoardData::Real(b)) => b
                .columns
                .iter()
                .any(|c| c.cards.iter().any(|card| &card.id == card_id)),
            Some(BoardData::All(b)) => b
                .columns
                .iter()
                .any(|c| c.cards.iter().any(|card| &card.card.id == card_id)),
            None => false,
        };
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
                    .find(|c| c.id == card_id)
                    .map(|c| (c.clone(), b.id.clone()))
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
        if let Some((card, board_id)) = self.find_card(card_id) {
            self.editing = Some(EditSession {
                state: card_modal::CardEditState::from_card(&card),
                archived: false,
                home_board_id: board_id,
            });
        }
    }

    // -- mutations ----------------------------------------------------------

    fn run(&mut self, err_prefix: &str, f: impl FnOnce(&Connection) -> kandrium_core::Result<()>) {
        let db = self.db.clone();
        let outcome = match db.lock() {
            Ok(db) => f(&db).map_err(|e| format!("{}: {}", err_prefix, e.0)),
            Err(_) => Err(format!("{}: database is locked", err_prefix)),
        };
        if let Err(msg) = outcome {
            self.set_status(msg, true);
        }
        self.reload_silent();
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
        }
    }

    // -- header -------------------------------------------------------------

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Tabs: virtual All board first, then real boards.
            let tabs: Vec<(String, String)> =
                std::iter::once((kandrium_core::ALL_BOARD_ID.to_string(), "All".to_string()))
                    .chain(self.boards.iter().map(|b| (b.id.clone(), b.title.clone())))
                    .collect();

            for (id, title) in &tabs {
                let active = self.view == View::Board && *id == self.saved.active_board_id;
                if theme::tab_button(ui, title, active).clicked() {
                    self.switch_board(id.clone(), ui.ctx());
                }
            }

            // Archived view toggle
            if theme::tab_button(ui, "Archived", self.view == View::Archived).clicked() {
                self.view = View::Archived;
                self.clear_inline_editors();
                self.drag = None;
                self.settle_gap = None;
                self.source_gap = None;
                anim::CardRig::clear_drag(ui.ctx());
                self.refresh();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Theme toggle
                let icon = if self.saved.dark_mode { "sun" } else { "moon" };
                if theme::icon_button(ui, icon, 28.0, false, "Toggle theme").clicked() {
                    self.saved.dark_mode = !self.saved.dark_mode;
                    theme::apply_visuals(ui.ctx(), self.saved.dark_mode);
                }

                // Manual refresh
                if theme::icon_button(ui, "rotate-cw", 28.0, false, "Refresh").clicked() {
                    self.refresh();
                }

                // Synced hint
                if let Some(t) = &self.synced_at
                    && t.elapsed() < STATUS_TRANSIENT
                {
                    let r = ui.horizontal(|ui| {
                        let (icon_rect, _) =
                            ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
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
        });
    }

    fn switch_board(&mut self, id: String, ctx: &egui::Context) {
        if self.view == View::Board && self.saved.active_board_id == id {
            return;
        }
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
    }

    // -- content ------------------------------------------------------------

    fn content(&mut self, ui: &mut egui::Ui) {
        if self.view == View::Archived {
            self.content_archived(ui);
            return;
        }

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
                    )
                };
                self.handle_board_actions(actions);
            }
            Some(BoardData::All(data)) => {
                let actions = all_board::show(ui, data, view_drag, &mut self.zones, hover);
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
            self.add_col = None;
            self.add_draft = None;
            self.draft_stash.remove(&col_id);
            self.run("Add card failed", |db| {
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

            match card_modal::show(&ctx, &mut editing.state, archived, &move_targets) {
                card_modal::CardModalOutcome::None => {}
                card_modal::CardModalOutcome::Closed => self.editing = None,
                card_modal::CardModalOutcome::Action(actions) => {
                    if actions.save {
                        let body = editing.state.to_body();
                        self.editing = None;
                        self.run("Save failed", |db| {
                            kandrium_core::update_card(db, &card_id, &body).map(|_| ())
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
        // Bare `cargo run` binaries carry no bundle .icns; apply the dock
        // icon from the first frame on. egui repaints on demand, so request
        // repaints until the icon is confirmed applied (~2s budget).
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
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let t = theme::Tokens::get(&ctx);

        // Toolbar: titlebar fill with a hairline divider at the bottom edge.
        egui::Panel::top("kandrium-toolbar")
            .frame(
                egui::Frame::new()
                    .fill(t.titlebar)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .stroke(egui::Stroke::new(1.0, t.divider)),
            )
            .show(ui, |ui| {
                self.header(ui);
            });

        // Main content on the pasteboard background, inset from the
        // toolbar divider and the window edges.
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
        Ok(db) => {
            if let Err(e) = kandrium_core::bootstrap(&db) {
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
