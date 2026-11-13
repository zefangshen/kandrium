//! The application shell: a title bar with the five-section segmented control
//! and one body per section.
//!
//! Tasks, Clocks, Notes and Documents read and write the real data; Projects
//! is the last section still on the placeholder.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::TestSupportExt;
use gpui_kit::component::input::{EditorState, InputEvent, InputState, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{
    Animation, AnimationExt, AnyElement, App, Bounds, ClickEvent, Context, DragMoveEvent, Entity,
    FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseUpEvent,
    Pixels, Point, Render, RenderImage, ScrollHandle, Window, div, px, rgba,
};
use jiff::Timestamp;
use jiff::civil::Date;
use kandrium_core::documents::{AnnotationDoc, AnnotationKind, import_file, new_annotation};
use kandrium_core::{
    ALL_BOARD_ID, ArchivedCard, Board, BoardWithColumns, Card, CardStatus, CreateCardBody, Person,
    ProjectChoice, Travel, day_end_seconds, day_start_seconds, project_dir,
};
use pdf_render::{Event as PdfEvent, PdfWorker};
use smallvec::smallvec;

use crate::anim::{SNAP_BACK, ease_snap, eased};
use crate::assets::{Icon, icon};
use crate::settings::{self, TreePanel};
use crate::store::Store;
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::archive;
use crate::views::card_modal::{self, EditSession, ModalContext};
use crate::views::clocks::{
    self, ClockDrag, ClockState, ConfirmKind, ConfirmState, DragState, FormKind, FormState,
    InspectorState, PickerState,
};
use crate::views::documents::{self, DocDraft, DocDraftKind, DocKind, DocTab, DocumentsState};
use crate::views::documents_pdf;
use crate::views::projects::{
    self, DetailEditor, DocDraft as ProjectDocDraft, DocDraftKind as ProjectDocDraftKind,
    ProjectConfirm, ProjectConfirmKind, ProjectDocTab, ProjectDraft, ProjectDraftKind, ProjectTab,
    ProjectsState, TitleEdit as ProjectTitleEdit, folder_name, project_folder, sibling_projects,
};
use crate::views::tasks::{
    COLUMN_W, CardMetrics, DropTarget, GAP_FALLBACK, SLOT_H, card_body, title_row,
};
use crate::views::{
    notes::{self, DraftKind, NameDraft, NoteTab, NotesState},
    placeholder, tasks,
};
use crate::vim;

/// The five top-level sections. [`Section::ALL`] fixes the segmented
/// control's order, which deliberately differs from the egui app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Tasks,
    Clocks,
    Notes,
    Documents,
    Projects,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Tasks,
        Section::Projects,
        Section::Notes,
        Section::Documents,
        Section::Clocks,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Tasks => "Tasks",
            Section::Clocks => "Clocks",
            Section::Notes => "Notes",
            Section::Documents => "Documents",
            Section::Projects => "Projects",
        }
    }

    /// Stable element-id suffix, also used as the tab's test path.
    pub fn id(self) -> &'static str {
        match self {
            Section::Tasks => "tasks",
            Section::Clocks => "clocks",
            Section::Notes => "notes",
            Section::Documents => "documents",
            Section::Projects => "projects",
        }
    }

    /// Inverse of [`Section::id`], used by the settings store. `None` for a key
    /// this build does not know (a file written by a newer build).
    pub fn from_key(key: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|section| section.id() == key)
    }

    pub fn icon(self) -> Icon {
        match self {
            Section::Tasks => Icon::SquareKanban,
            Section::Clocks => Icon::Globe,
            Section::Notes => Icon::StickyNote,
            Section::Documents => Icon::FileText,
            Section::Projects => Icon::FolderKanban,
        }
    }
}

impl std::fmt::Display for Section {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The inline "+ Add card" draft open on one column of a normal Tasks board.
pub(crate) struct AddCardForm {
    pub column_id: String,
    pub input: Entity<InputState>,
}

/// The inline title editor open on one card row.
pub(crate) struct CardTitleEdit {
    pub card_id: String,
    /// Title the card had when the editor opened; Esc and empty drafts keep it.
    pub original: String,
    pub input: Entity<InputState>,
}

/// The card right-click menu, anchored at the pointer.
pub(crate) struct CardMenu {
    pub card_id: String,
    pub position: Point<Pixels>,
}

/// A pending destructive card action, shown as a confirmation dialog.
pub(crate) struct CardConfirm {
    pub card_id: String,
    pub title: &'static str,
    pub message: &'static str,
    /// Delete runs from the Archived page rather than a board row.
    pub from_archive: bool,
}

/// The shell-owned ghost that flies to the destination after release.
///
/// gpui tears down its drag view on mouse-up, so the settle and snap-back
/// flight is an overlay animated by the shell instead. It carries the source
/// card's snapshot, not just its title: the overlay paints the same body as
/// the resting card, so the meta row never blinks out between the drop and
/// the settle.
struct DragFlight {
    card_id: String,
    card: Card,
    /// Release pointer, in window coordinates.
    from: Point<Pixels>,
    /// Destination insertion row, measured by the board.
    to: Bounds<Pixels>,
    token: u64,
}

/// Root view. Owns the database handles and the data each section renders.
pub struct AppShell {
    section: Section,
    tokens: Tokens,
    use_24h: bool,
    store: Store,
    boards: Vec<Board>,
    active_board_id: Option<String>,
    board: Option<BoardWithColumns>,
    board_error: Option<String>,
    drop_target: Option<DropTarget>,
    /// The target the gap occupied before the latest change. The renderer
    /// derives each displaced card's glide offset from the pair.
    previous_target: Option<DropTarget>,
    dragging_card: Option<String>,
    drop_generation: u64,
    /// Row height of the dragged card, and of the gap replacing it.
    drag_gap_height: f32,
    /// Where inside the card the drag was picked up, so the shell's flight
    /// ghost starts exactly where gpui's drag view was.
    drag_grab: Point<Pixels>,
    /// The source card's rect and insertion point, for a snap-back.
    drag_origin: Option<Bounds<Pixels>>,
    drag_origin_target: Option<DropTarget>,
    drag_flight: Option<DragFlight>,
    drag_flight_token: u64,
    /// Card that just landed, faded in once so the handoff from the ghost
    /// does not pop.
    settled_card: Option<(String, u64)>,
    /// Row rects measured by the board.
    drag_metrics: CardMetrics,
    editing: Option<EditSession>,
    /// Open inline title editor on one card row, and the row under the
    /// pointer whose hover pencil is showing.
    title_edit: Option<CardTitleEdit>,
    hovered_card: Option<String>,
    /// The board strip's card search field, kept for the session so opening a
    /// card and closing it again preserves the query.
    board_search: Option<Entity<InputState>>,
    /// Per-column card-list scroll handles, kept for the session so an offset
    /// survives reloads, section switches and card round trips.
    column_scrolls: HashMap<String, ScrollHandle>,
    /// Pointer position during a card drag, written by the columns and read
    /// while rendering to keep edge-scrolling while the pointer rests.
    drag_pointer: Rc<Cell<Option<Point<Pixels>>>>,
    /// Query of the last board render; a change resets the scrolled lists.
    board_query: String,
    /// Open card right-click menu and the delete confirmation.
    card_menu: Option<CardMenu>,
    card_menu_focus: Option<FocusHandle>,
    card_confirm: Option<CardConfirm>,
    card_confirm_focus: Option<FocusHandle>,
    /// Archived tab selection and the page rows.
    showing_archive: bool,
    archive: Option<archive::ArchiveState>,
    archive_cards: Option<Vec<ArchivedCard>>,
    archive_error: Option<String>,
    /// Window bounds of the archive board-filter trigger, published by the
    /// page and consumed by the root-level filter menu overlay.
    archive_filter_anchor: Rc<RefCell<Option<Bounds<Pixels>>>>,
    archive_filter_focus: Option<FocusHandle>,
    /// Open "+ Add card" draft and the per-column stashed text.
    add_card: Option<AddCardForm>,
    add_card_drafts: HashMap<String, String>,
    project_choices: Vec<ProjectChoice>,
    pub(crate) clocks: ClockState,
    /// Clock id shown in the titlebar; the persisted user pick.
    titlebar_clock: Option<String>,
    /// Whether the titlebar city menu is open.
    titlebar_clock_menu: bool,
    /// Bumped to retarget or stop the minute tick.
    clock_tick: u64,
    /// Generation of the tick loop currently allowed to run.
    clock_tick_running: Option<u64>,
    notes: NotesState,
    documents: DocumentsState,
    projects: ProjectsState,
    /// Board currently loaded into `board` for a project's Tasks tab. Kept
    /// separate from `active_board_id` so the Tasks section is unaffected.
    project_board_shown: Option<String>,
    /// Persisted UI preferences (section, vim, tree widths and collapse flags).
    settings: settings::Settings,
    settings_path: PathBuf,
    /// The user's vim preference, shared by every editor.
    vim_mode: bool,
    /// External-change watcher; present only while a database-backed section
    /// is foregrounded.
    watcher: Option<crate::watcher::Watcher>,
    watcher_generation: u64,
    watcher_running: Option<u64>,
    /// An external change arrived mid-drag and is applied on a later frame.
    pending_sync: bool,
}

/// Native window drag for the titlebar row: a single press hands the window to
/// the system move loop, a double click is the standard macOS zoom.
fn titlebar_drag(event: &MouseDownEvent, window: &mut Window, _cx: &mut App) {
    if event.click_count == 2 {
        window.zoom_window();
    } else {
        window.start_window_move();
    }
}

/// `Wed 11 Nov 8:13 pm` in the given zone: three-letter weekday, day without
/// a leading zero, three-letter month, lowercase 12-hour time.
fn format_titlebar_clock(now: Timestamp, zone: &jiff::tz::TimeZone) -> String {
    let zoned = now.to_zoned(zone.clone());
    format!(
        "{} {} {} {}",
        zoned.strftime("%a"),
        zoned.strftime("%-d"),
        zoned.strftime("%b"),
        zoned.strftime("%-I:%M %P"),
    )
}

/// `Thu 12 Nov 3:28 am` for a menu row: weekday, day, month, then the time.
/// The clock itself drops the date; rows show both so cities in far zones read
/// unambiguously. The time follows the app's 12/24-hour preference.
fn format_clock_menu_meta(now: Timestamp, zone: &jiff::tz::TimeZone, use_24h: bool) -> String {
    let zoned = now.to_zoned(zone.clone());
    let time = if use_24h {
        zoned.strftime("%H:%M").to_string()
    } else {
        zoned.strftime("%-I:%M %P").to_string()
    };
    format!(
        "{} {} {} {time}",
        zoned.strftime("%a"),
        zoned.strftime("%-d"),
        zoned.strftime("%b"),
    )
}

impl AppShell {
    /// Open the default database and load the first screen.
    pub fn new() -> Self {
        Self::with_store(Store::open())
    }

    /// Build the shell over a specific store (the headless tests use this).
    pub fn with_store(store: Store) -> Self {
        let settings_path = store
            .path
            .parent()
            .map(|parent| parent.join(settings::FILE_NAME))
            .unwrap_or_else(|| PathBuf::from(settings::FILE_NAME));
        let settings = settings::Settings::load(&settings_path);

        let mut notes = NotesState::new();
        notes.tree_open = settings.tree_open(TreePanel::Notes);
        notes.tree_w = settings.tree_width(TreePanel::Notes);
        let mut documents = DocumentsState::new();
        documents.tree_open = settings.tree_open(TreePanel::Documents);
        documents.tree_w = settings.tree_width(TreePanel::Documents);
        let mut projects = ProjectsState::new();
        projects.tree_open = settings.tree_open(TreePanel::Projects);
        projects.tree_w = settings.tree_width(TreePanel::Projects);
        let section = settings.section();
        let vim_mode = settings.vim_mode();
        let titlebar_clock = settings.titlebar_clock().map(str::to_string);

        let mut shell = Self {
            section,
            tokens: Tokens::dark(),
            use_24h: true,
            store,
            boards: Vec::new(),
            active_board_id: None,
            board: None,
            board_error: None,
            drop_target: None,
            previous_target: None,
            dragging_card: None,
            drop_generation: 0,
            drag_gap_height: GAP_FALLBACK,
            drag_grab: Point::default(),
            drag_origin: None,
            drag_origin_target: None,
            drag_flight: None,
            drag_flight_token: 0,
            settled_card: None,
            drag_metrics: Rc::new(RefCell::new(HashMap::new())),
            editing: None,
            title_edit: None,
            hovered_card: None,
            board_search: None,
            column_scrolls: HashMap::new(),
            drag_pointer: Rc::new(Cell::new(None)),
            board_query: String::new(),
            card_menu: None,
            card_menu_focus: None,
            card_confirm: None,
            card_confirm_focus: None,
            showing_archive: settings.tasks_archive(),
            archive: None,
            archive_cards: None,
            archive_error: None,
            archive_filter_anchor: Rc::new(RefCell::new(None)),
            archive_filter_focus: None,
            add_card: None,
            add_card_drafts: HashMap::new(),
            project_choices: Vec::new(),
            clocks: ClockState::new(),
            titlebar_clock,
            titlebar_clock_menu: false,
            clock_tick: 0,
            clock_tick_running: None,
            notes,
            documents,
            projects,
            project_board_shown: None,
            settings,
            settings_path,
            vim_mode,
            watcher: None,
            watcher_generation: 0,
            watcher_running: None,
            pending_sync: false,
        };
        shell.load_tasks();
        shell.load_clocks();
        if shell.showing_archive {
            shell.load_archived();
        }
        match section {
            Section::Notes => shell.load_notes(),
            Section::Documents => shell.load_documents(),
            Section::Projects => shell.load_projects(),
            _ => {}
        }
        shell
    }

    fn settings_path(&self) -> PathBuf {
        self.settings_path.clone()
    }

    /// Copy the live preferences into the settings map and write it. Called
    /// only from user actions, never per frame.
    fn persist_settings(&mut self) {
        self.settings.set_vim_mode(self.vim_mode);
        self.settings.set_section(self.section);
        self.settings
            .set_tree_open(TreePanel::Notes, self.notes.tree_open);
        self.settings
            .set_tree_width(TreePanel::Notes, self.notes.tree_w);
        self.settings
            .set_tree_open(TreePanel::Documents, self.documents.tree_open);
        self.settings
            .set_tree_width(TreePanel::Documents, self.documents.tree_w);
        self.settings
            .set_tree_open(TreePanel::Projects, self.projects.tree_open);
        self.settings
            .set_tree_width(TreePanel::Projects, self.projects.tree_w);
        self.settings
            .set_titlebar_clock(self.titlebar_clock.as_deref());
        self.settings.set_tasks_archive(self.showing_archive);
        let path = self.settings_path();
        if let Err(error) = self.settings.save(&path) {
            kandrium_core::logging::warn!("could not write settings {}: {error}", path.display());
        }
    }

    /// Flip the vim preference and apply it to every open editor immediately.
    pub(crate) fn toggle_vim_mode(&mut self, cx: &mut Context<Self>) {
        self.vim_mode = !self.vim_mode;
        let enabled = self.vim_mode;
        if let Some(session) = self.editing.as_mut() {
            let description = session.description.clone();
            session.vim.set_enabled(enabled, &description, cx);
        }
        for tab in &mut self.notes.tabs {
            let editor = tab.editor.clone();
            tab.vim.set_enabled(enabled, &editor, cx);
        }
        for tab in &mut self.documents.tabs {
            if let (Some(vim), Some(editor)) = (tab.vim.as_mut(), tab.editor.clone()) {
                vim.set_enabled(enabled, &editor, cx);
            }
        }
        for tab in &mut self.projects.tabs {
            if let ProjectTab::Doc(doc) = tab {
                let editor = doc.editor.clone();
                doc.vim.set_enabled(enabled, &editor, cx);
            }
        }
        self.persist_settings();
        cx.notify();
    }

    /// Live width while the tree splitter is dragged.
    pub(crate) fn set_notes_tree_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let width = settings::clamp_tree_width(width);
        if (self.notes.tree_w - width).abs() > 0.5 {
            self.notes.tree_w = width;
            cx.notify();
        }
    }

    /// Live width while the tree splitter is dragged.
    pub(crate) fn set_documents_tree_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let width = settings::clamp_tree_width(width);
        if (self.documents.tree_w - width).abs() > 0.5 {
            self.documents.tree_w = width;
            cx.notify();
        }
    }

    /// Live width while the project tree splitter is dragged.
    pub(crate) fn set_projects_tree_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let width = settings::clamp_tree_width(width);
        if (self.projects.tree_w - width).abs() > 0.5 {
            self.projects.tree_w = width;
            cx.notify();
        }
    }

    pub(crate) fn toggle_projects_tree(&mut self, cx: &mut Context<Self>) {
        self.projects.tree_open = !self.projects.tree_open;
        self.persist_settings();
        cx.notify();
    }

    /// Persist after a splitter drag ends.
    pub(crate) fn finish_tree_resize(&mut self, cx: &mut Context<Self>) {
        self.persist_settings();
        cx.notify();
    }

    /// Currently selected section.
    pub fn section(&self) -> Section {
        self.section
    }

    /// Whether vim editing is on for every editor.
    pub fn vim_mode(&self) -> bool {
        self.vim_mode
    }

    fn select(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        // Leaving a section unmounts its fields, but gpui keeps the focus
        // handle alive, so the caret would silently reappear when the section
        // comes back. Drop focus with the section it belonged to.
        window.blur(cx);
        if self.section == Section::Notes {
            self.flush_active_note(cx);
        }
        if self.section == Section::Documents {
            self.flush_documents(cx);
            self.save_documents_session();
        }
        if self.section == Section::Projects {
            self.flush_project_fields(window, cx);
            self.save_project_description(cx);
            self.flush_project_docs(cx);
            self.project_board_shown = None;
        }
        self.clear_drag_state();
        self.section = section;
        match section {
            Section::Tasks => self.load_tasks(),
            Section::Clocks => self.load_clocks(),
            Section::Notes => self.load_notes(),
            Section::Documents => self.load_documents(),
            Section::Projects => self.load_projects(),
        }
        if section != Section::Clocks {
            self.invalidate_clock_tick();
        }
        self.persist_settings();
        cx.notify();
    }

    fn load_tasks(&mut self) {
        match self.store.boards() {
            Ok(boards) => {
                self.boards = boards;
                let target = self
                    .active_board_id
                    .clone()
                    .or_else(|| self.boards.first().map(|board| board.id.clone()));
                let Some(id) = target else {
                    self.board = None;
                    self.active_board_id = None;
                    self.board_error = None;
                    return;
                };
                if let Err(error) = self.load_board(&id) {
                    // The held board disappeared (deleted or archived): fall
                    // back to the first visible one before surfacing an error.
                    let fallback = self
                        .boards
                        .first()
                        .map(|board| board.id.clone())
                        .filter(|first| first != &id);
                    match fallback {
                        Some(first) if self.load_board(&first).is_ok() => {}
                        _ => {
                            self.board = None;
                            self.board_error = Some(error);
                        }
                    }
                }
            }
            Err(e) => {
                self.boards.clear();
                self.board = None;
                self.board_error = Some(e);
            }
        }
    }

    /// Load one board by id, keeping the previous board on failure. The virtual
    /// All board is loaded through `Store::all_board` and stored in the same
    /// shape, so the surface renders both identically.
    fn load_board(&mut self, id: &str) -> Result<(), String> {
        let loaded = if id == ALL_BOARD_ID {
            self.store.all_board()
        } else {
            self.store.board(id)
        };
        match loaded {
            Ok(board) => {
                self.active_board_id = Some(board.id.clone());
                self.board = Some(board);
                self.board_error = None;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Switch the Tasks tab to `id` (and off the Archived page).
    pub(crate) fn open_board(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        // A board switch swaps the whole surface under the strip, so no field
        // from the old board — the card search included — may keep the caret.
        window.blur(cx);
        let from_archive = self.showing_archive;
        if from_archive {
            self.showing_archive = false;
            self.persist_settings();
        }
        if self.active_board_id.as_deref() == Some(id) {
            if from_archive {
                cx.notify();
            }
            return;
        }
        self.clear_drag_state();
        if let Err(error) = self.load_board(id) {
            self.board_error = Some(error);
        }
        cx.notify();
    }

    /// Open the inline "+ Add card" draft on `column_id`, restoring any text
    /// stashed for that column.
    pub(crate) fn begin_add_card(
        &mut self,
        column_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .add_card
            .as_ref()
            .is_some_and(|form| form.column_id == column_id)
        {
            return;
        }
        // Switching columns mid-draft stashes the outgoing text.
        self.cancel_add_card(cx);
        let draft = self.add_card_drafts.remove(column_id).unwrap_or_default();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Enter a title…")
                .default_value(draft)
        });
        input.update(cx, |state, cx| state.focus(window, cx));
        self.add_card = Some(AddCardForm {
            column_id: column_id.to_string(),
            input,
        });
        cx.notify();
    }

    /// Close the draft, keeping its text for the next open on that column.
    pub(crate) fn cancel_add_card(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.add_card.take() else {
            return;
        };
        let text = form.input.read(cx).value().trim().to_string();
        if !text.is_empty() {
            self.add_card_drafts.insert(form.column_id, text);
        }
        cx.notify();
    }

    /// Enter in the draft: create the card in its column and close the form.
    pub(crate) fn submit_add_card(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.add_card.as_ref() else {
            return;
        };
        let title = form.input.read(cx).value().trim().to_string();
        let column_id = form.column_id.clone();
        if title.is_empty() {
            // An empty draft keeps the form open, like egui.
            return;
        }
        match self.store.create_card(&column_id, &title) {
            Ok(()) => {
                self.add_card = None;
                self.add_card_drafts.remove(&column_id);
                self.board_error = None;
                self.reload_active_board();
            }
            Err(error) => self.board_error = Some(error),
        }
        cx.notify();
    }

    /// Keys routed to the open add-card draft before the input sees them.
    pub(crate) fn handle_add_card_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                self.cancel_add_card(cx);
            }
            "enter" => {
                cx.stop_propagation();
                self.submit_add_card(cx);
            }
            _ => {}
        }
    }

    /// Show the hover pencil on the card row under the pointer.
    pub(crate) fn set_card_hover(&mut self, card_id: Option<String>, cx: &mut Context<Self>) {
        if self.hovered_card == card_id {
            return;
        }
        self.hovered_card = card_id;
        cx.notify();
    }

    /// Replace one card's title with an inline field, seeded with the current
    /// title and the caret at the end.
    pub(crate) fn begin_card_title_edit(
        &mut self,
        card_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .title_edit
            .as_ref()
            .is_some_and(|edit| edit.card_id == card_id)
        {
            return;
        }
        let Some(title) = self
            .board
            .as_ref()
            .into_iter()
            .flat_map(|board| board.columns.iter())
            .flat_map(|column| column.ordered_cards())
            .find(|card| card.id == card_id)
            .map(|card| card.title.clone())
        else {
            return;
        };
        // One inline draft at a time: the add-card form and the title editor
        // share the board's press-away handling.
        self.cancel_add_card(cx);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Card title"));
        input.update(cx, |state, cx| {
            // `set_value` leaves the caret at the end of a single-line field.
            state.set_value(title.clone(), window, cx);
            state.focus(window, cx);
        });
        self.title_edit = Some(CardTitleEdit {
            card_id: card_id.to_string(),
            original: title,
            input,
        });
        cx.notify();
    }

    /// Commit the inline title editor: Enter, a press outside the field, or a
    /// board/section switch. An empty or whitespace-only draft is rejected and
    /// the card keeps its title.
    pub(crate) fn commit_card_title_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.title_edit.take() else {
            return;
        };
        let title = edit.input.read(cx).value().trim().to_string();
        if !title.is_empty() && title != edit.original {
            match self.store.rename_card(&edit.card_id, &title) {
                Ok(()) => self.patch_card_title(&edit.card_id, &title),
                Err(error) => self.board_error = Some(error),
            }
        }
        cx.notify();
    }

    /// Esc in the inline editor: drop the draft, leaving the title unchanged.
    pub(crate) fn cancel_card_title_edit(&mut self, cx: &mut Context<Self>) {
        if self.title_edit.take().is_some() {
            cx.notify();
        }
    }

    /// Keys routed to the inline editor before the input sees them.
    pub(crate) fn handle_card_title_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                self.cancel_card_title_edit(cx);
            }
            "enter" => {
                cx.stop_propagation();
                self.commit_card_title_edit(cx);
            }
            _ => {}
        }
    }

    /// Patch the loaded board in place after a rename, so the row updates
    /// without a rebuild that would drop the scroll offset and the open state.
    fn patch_card_title(&mut self, card_id: &str, title: &str) {
        let Some(board) = self.board.as_mut() else {
            return;
        };
        for column in &mut board.columns {
            for card in column.cards.iter_mut().chain(column.mirrored.iter_mut()) {
                if card.id == card_id {
                    card.title = title.to_string();
                }
            }
        }
    }

    // -- board search -------------------------------------------------------

    /// The board strip's search field, created once per session.
    fn ensure_board_search(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(input) = &self.board_search {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search cards…"));
        // Filtering happens in the board render, so every keystroke must
        // repaint the shell.
        cx.observe(&input, |_: &mut AppShell, _, cx| cx.notify())
            .detach();
        self.board_search = Some(input.clone());
        input
    }

    /// Clear the search from the clear button or Esc.
    pub(crate) fn clear_board_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.board_search.clone() {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        cx.notify();
    }

    // -- column scrolling ---------------------------------------------------

    /// Give every column of the loaded board a persistent scroll handle, and
    /// restart the lists at the top whenever the filter changes what they
    /// hold — a stale offset would point past the new content.
    fn prepare_column_scrolls(&mut self, query: &str) {
        let ids: Vec<String> = match &self.board {
            Some(board) => board
                .columns
                .iter()
                .map(|column| column.id.clone())
                .collect(),
            None => return,
        };
        for id in ids {
            self.column_scrolls.entry(id).or_default();
        }
        if self.board_query != query {
            self.board_query = query.to_string();
            for scroll in self.column_scrolls.values() {
                scroll.set_offset(Point::default());
            }
        }
    }

    /// While a card is dragged near the top or bottom of a column list, step
    /// that list so cards out of view come into reach. Runs on every board
    /// render; the animation frame it requests keeps stepping while the
    /// pointer rests at the edge, and it stops as soon as the drag ends.
    fn column_edge_scroll(&mut self, window: &mut Window) {
        if self.dragging_card.is_none() {
            return;
        }
        let Some(pointer) = self.drag_pointer.get() else {
            return;
        };
        const EDGE: f32 = 28.0;
        const STEP: f32 = 14.0;
        let mut stepped = false;
        for scroll in self.column_scrolls.values() {
            let bounds = scroll.bounds();
            let max = scroll.max_offset().y;
            if max <= px(0.) || bounds.size.height <= px(0.) {
                continue;
            }
            // The bar's gutter is part of the column, so a pointer just past
            // the list's right edge still counts.
            if pointer.x < bounds.left() || pointer.x > bounds.right() + px(12.) {
                continue;
            }
            let (top, bottom) = (bounds.top() + px(EDGE), bounds.bottom() - px(EDGE));
            let delta = if pointer.y < top {
                px(-STEP)
            } else if pointer.y > bottom {
                px(STEP)
            } else {
                continue;
            };
            let offset = scroll.offset();
            let y = (offset.y + delta).clamp(px(0.), max);
            if y != offset.y {
                scroll.set_offset(Point { x: offset.x, y });
                stepped = true;
            }
        }
        if stepped {
            window.request_animation_frame();
        }
    }

    // -- card menu / archive ------------------------------------------------

    /// Open the right-click menu at the pointer, clamped inside the window.
    pub(crate) fn open_card_menu(
        &mut self,
        card_id: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // One overlay at a time: the inline editor and the add-card draft close
        // before the menu opens.
        self.commit_card_title_edit(cx);
        self.cancel_add_card(cx);
        self.card_confirm = None;
        let viewport = window.viewport_size();
        let max_x = (viewport.width - px(168.)).max(px(8.));
        let max_y = (viewport.height - px(84.)).max(px(8.));
        let position = Point {
            x: position.x.max(px(8.)).min(max_x),
            y: position.y.max(px(8.)).min(max_y),
        };
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.card_menu_focus = Some(focus);
        self.card_menu = Some(CardMenu {
            card_id: card_id.to_string(),
            position,
        });
        cx.notify();
    }

    pub(crate) fn close_card_menu(&mut self, cx: &mut Context<Self>) {
        if self.card_menu.take().is_some() {
            self.card_menu_focus = None;
            cx.notify();
        }
    }

    /// Archive one card. Mirrors carry the real card id, so the write and the
    /// row removal always target the card, never a copy.
    pub(crate) fn archive_card(&mut self, card_id: &str, cx: &mut Context<Self>) {
        self.card_menu = None;
        self.card_menu_focus = None;
        match self.store.set_card_archived(card_id, true) {
            Ok(()) => {
                self.remove_card_from_board(card_id);
                self.archive_cards = None;
                self.board_error = None;
            }
            Err(error) => self.board_error = Some(error),
        }
        cx.notify();
    }

    /// Delete on a board row: ask first, exactly like egui.
    pub(crate) fn request_delete_card(&mut self, card_id: &str, cx: &mut Context<Self>) {
        self.card_menu = None;
        self.card_menu_focus = None;
        self.card_confirm = Some(CardConfirm {
            card_id: card_id.to_string(),
            title: "Delete card",
            message: "Delete this card permanently?",
            from_archive: false,
        });
        cx.notify();
    }

    /// Delete on the Archived page: egui names the permanence explicitly.
    pub(crate) fn request_delete_archived_card(&mut self, card_id: &str, cx: &mut Context<Self>) {
        self.card_confirm = Some(CardConfirm {
            card_id: card_id.to_string(),
            title: "Delete archived card",
            message: "Permanently delete this archived card? This cannot be undone.",
            from_archive: true,
        });
        cx.notify();
    }

    pub(crate) fn cancel_card_confirm(&mut self, cx: &mut Context<Self>) {
        if self.card_confirm.take().is_some() {
            self.card_confirm_focus = None;
            cx.notify();
        }
    }

    pub(crate) fn confirm_card_delete(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.card_confirm.take() else {
            return;
        };
        self.card_confirm_focus = None;
        match self.store.delete_card(&confirm.card_id) {
            Ok(()) => {
                if confirm.from_archive {
                    self.load_archived();
                } else {
                    self.remove_card_from_board(&confirm.card_id);
                }
                self.board_error = None;
            }
            Err(error) => self.board_error = Some(error),
        }
        cx.notify();
    }

    /// Drop one card from the loaded board without a reload, so the scroll
    /// offset and the rest of the rows stay put.
    fn remove_card_from_board(&mut self, card_id: &str) {
        if self
            .title_edit
            .as_ref()
            .is_some_and(|edit| edit.card_id == card_id)
        {
            self.title_edit = None;
        }
        if self.dragging_card.as_deref() == Some(card_id) {
            self.clear_drag_state();
        }
        if self
            .editing
            .as_ref()
            .is_some_and(|session| session.card_id == card_id)
        {
            self.editing = None;
        }
        if let Some(board) = self.board.as_mut() {
            for column in &mut board.columns {
                column.cards.retain(|card| card.id != card_id);
                column.mirrored.retain(|card| card.id != card_id);
            }
        }
    }

    // -- Archived page ------------------------------------------------------

    fn load_archived(&mut self) {
        match self.store.archived_cards() {
            Ok(cards) => {
                self.archive_cards = Some(cards);
                self.archive_error = None;
            }
            Err(error) => {
                self.archive_cards = Some(Vec::new());
                self.archive_error = Some(error);
            }
        }
    }

    fn ensure_archive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.archive.is_none() {
            self.archive = Some(archive::ArchiveState::new(window, cx));
        }
        if self.archive_cards.is_none() {
            self.load_archived();
        }
    }

    /// Show the Archived page instead of a board.
    pub(crate) fn open_archive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.showing_archive {
            return;
        }
        // The board strip and its search unmount here; leaving focus on them
        // would hand the caret back when a board is reopened.
        window.blur(cx);
        self.commit_card_title_edit(cx);
        self.cancel_add_card(cx);
        self.clear_drag_state();
        self.card_menu = None;
        self.showing_archive = true;
        self.ensure_archive(window, cx);
        self.persist_settings();
        cx.notify();
    }

    /// Open or close the board filter. The menu floats at the root, so opening
    /// it never moves the table or its scroll offset.
    pub(crate) fn toggle_archive_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.archive.as_mut() else {
            return;
        };
        state.filter_open = !state.filter_open;
        if state.filter_open {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            self.archive_filter_focus = Some(focus);
        } else {
            self.archive_filter_focus = None;
        }
        cx.notify();
    }

    /// Dismiss the menu without touching the selection (Esc / outside press).
    pub(crate) fn close_archive_filter(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.archive.as_mut() {
            state.filter_open = false;
        }
        self.archive_filter_focus = None;
        cx.notify();
    }

    pub(crate) fn set_archive_filter(&mut self, board_id: &str, cx: &mut Context<Self>) {
        if let Some(state) = self.archive.as_mut() {
            state.board_filter = board_id.to_string();
            state.filter_open = false;
        }
        self.archive_filter_focus = None;
        cx.notify();
    }

    pub(crate) fn toggle_archive_sort(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.archive.as_mut() {
            state.sort_newest = !state.sort_newest;
        }
        cx.notify();
    }

    /// Restore one archived card; the board is refreshed in the background so
    /// switching back shows it in place.
    pub(crate) fn restore_archived_card(&mut self, card_id: &str, cx: &mut Context<Self>) {
        match self.store.set_card_archived(card_id, false) {
            Ok(()) => {
                self.load_archived();
                self.reload_active_board();
                self.board_error = None;
            }
            Err(error) => self.archive_error = Some(error),
        }
        cx.notify();
    }

    /// Enter the drag session at the card's own slot.
    ///
    /// The dragged card leaves the flow and a same-height gap takes its row,
    /// so pickup itself never shifts anything. `grab` is gpui's grab offset
    /// inside the card, reused by the release flight.
    pub(crate) fn begin_card_drag(
        &mut self,
        card_id: &str,
        column_id: &str,
        before: Option<String>,
        grab: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.dragging_card.as_deref() == Some(card_id) {
            return;
        }
        // The inline editor owns that row's pointer; never start a drag under it.
        if self
            .title_edit
            .as_ref()
            .is_some_and(|edit| edit.card_id == card_id)
        {
            return;
        }
        let bounds = self.drag_metrics.borrow().get(card_id).copied();
        self.drag_gap_height = bounds
            .map(|bounds| bounds.size.height.as_f32())
            .filter(|height| *height > 0.0)
            .unwrap_or(GAP_FALLBACK);
        // The card's row starts with a 6 px slot; the flight lands on the body.
        self.drag_origin = bounds.map(|bounds| {
            Bounds::new(
                bounds.origin
                    + Point {
                        x: px(0.0),
                        y: px(SLOT_H),
                    },
                bounds.size,
            )
        });
        self.drag_grab = grab;
        self.drop_generation = self.drop_generation.wrapping_add(1);
        let target = DropTarget {
            column_id: column_id.to_string(),
            before,
            generation: self.drop_generation,
        };
        self.drop_target = Some(target.clone());
        self.drag_origin_target = Some(target.clone());
        // At pickup the gap replaces the card's row exactly, so no card moves:
        // seeding the previous target with the same slot makes the offset
        // table return zero for every sibling.
        self.previous_target = Some(target);
        self.drag_flight = None;
        self.settled_card = None;
        self.dragging_card = Some(card_id.to_string());
        cx.notify();
    }

    /// Whether the pointer is inside a measured row (a card or the gap).
    ///
    /// Rows register their own `on_drag_move`, but the column's fallback runs
    /// first because it is an ancestor. Without this check every pointer move
    /// first moved the gap to the column's end and then back to the hovered
    /// slot, so the whole column reflowed twice per move.
    pub(crate) fn pointer_over_row(&self, position: Point<Pixels>) -> bool {
        self.drag_metrics
            .borrow()
            .values()
            .any(|bounds| bounds.contains(&position))
    }

    /// Record where a drop would land. The previous target is kept so the
    /// renderer can glide the cards the change displaces.
    pub(crate) fn set_drop_target(
        &mut self,
        column_id: &str,
        before: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let same = self
            .drop_target
            .as_ref()
            .is_some_and(|target| target.column_id == column_id && target.before == before);
        if same {
            return;
        }
        self.previous_target = self.drop_target.take();
        self.drop_generation = self.drop_generation.wrapping_add(1);
        self.drop_target = Some(DropTarget {
            column_id: column_id.to_string(),
            before,
            generation: self.drop_generation,
        });
        cx.notify();
    }

    /// Release outside every drop zone, or a release that never produced a
    /// drop: park the gap back at the source slot and fly the ghost there.
    pub(crate) fn cancel_card_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drag_flight.is_some() {
            // A drop handler already started the settle flight.
            return;
        }
        let Some(card_id) = self.dragging_card.clone() else {
            return;
        };
        let card = self.card_snapshot(&card_id);
        let origin = self.drag_origin;
        self.restore_origin_gap();
        self.start_drag_flight(&card_id, card, window, origin, cx);
    }

    /// Commit a drop. The target recorded by `on_drag_move` decides the
    /// insertion point; a drop on a column without one appends.
    pub(crate) fn drop_card(
        &mut self,
        card_id: &str,
        column_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self
            .drop_target
            .clone()
            .filter(|target| target.column_id == column_id)
            .and_then(|target| target.before)
            .filter(|before| before != card_id);
        let origin_target = self.drag_origin_target.clone();
        let same_slot = origin_target
            .as_ref()
            .is_some_and(|target| target.column_id == column_id && target.before == before);
        // Snapshot the card before the move: the flight overlay renders this
        // card body, and the reload below replaces the board it came from.
        let card = self.card_snapshot(card_id);
        let origin = self.drag_origin;

        if same_slot {
            // Dropped on its own slot: a cancel, never a move.
            self.restore_origin_gap();
            self.start_drag_flight(card_id, card, window, origin, cx);
            return;
        }

        // All-board columns are synthetic `all:<status>` ids: a drop there
        // changes the card's status on its home board (core handles the move).
        let result = match column_id
            .strip_prefix("all:")
            .and_then(CardStatus::from_key)
        {
            Some(status) => self.store.move_all_card(card_id, status, before.as_deref()),
            None => self.store.move_card(card_id, column_id, before.as_deref()),
        };
        match result {
            Ok(()) => {
                self.board_error = None;
                // The board on screen may be a project's, so reload whatever
                // the section is displaying rather than the Tasks board.
                self.reload_displayed_board();
                // Keep the gap at the destination until the ghost lands, so
                // the real card appears exactly where it was released.
                self.drop_generation = self.drop_generation.wrapping_add(1);
                let target = DropTarget {
                    column_id: column_id.to_string(),
                    before,
                    generation: self.drop_generation,
                };
                self.previous_target = self.drop_target.take();
                self.drop_target = Some(target);
                let destination = self
                    .drag_metrics
                    .borrow()
                    .get(&format!("gap:{card_id}"))
                    .copied()
                    .or(origin);
                self.start_drag_flight(card_id, card, window, destination, cx);
            }
            Err(error) => {
                self.board_error = Some(error);
                self.restore_origin_gap();
                self.start_drag_flight(card_id, card, window, origin, cx);
            }
        }
    }

    /// The card on the displayed board, for the flight overlay.
    ///
    /// The overlay paints the same body as the resting card, so it needs the
    /// card's meta label, due date and labels — not just its title.
    fn card_snapshot(&self, card_id: &str) -> Option<Card> {
        self.board
            .as_ref()
            .into_iter()
            .flat_map(|board| board.columns.iter())
            .flat_map(|column| column.ordered_cards())
            .find(|card| card.id == card_id)
            .cloned()
    }

    /// Move the insertion gap back to the card's own slot.
    fn restore_origin_gap(&mut self) {
        let Some(origin) = self.drag_origin_target.clone() else {
            return;
        };
        self.previous_target = self.drop_target.take();
        self.drop_generation = self.drop_generation.wrapping_add(1);
        self.drop_target = Some(DropTarget {
            generation: self.drop_generation,
            ..origin
        });
    }

    /// Fly the released card to `to`. With reduced motion the drag simply
    /// ends: no flight, no wait.
    fn start_drag_flight(
        &mut self,
        card_id: &str,
        card: Option<Card>,
        window: &mut Window,
        to: Option<Bounds<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let Some(to) = to else {
            self.finish_drag_state(card_id, cx);
            return;
        };
        if cx.reduce_motion() {
            self.finish_drag_state(card_id, cx);
            return;
        }
        // Without a snapshot there is no body to paint, so end the drag at
        // once instead of flying an empty shell.
        let Some(card) = card else {
            self.finish_drag_state(card_id, cx);
            return;
        };
        self.drag_flight_token = self.drag_flight_token.wrapping_add(1);
        let token = self.drag_flight_token;
        let from = window.mouse_position() - self.drag_grab;
        self.drag_flight = Some(DragFlight {
            card_id: card_id.to_string(),
            card,
            from,
            to,
            token,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SNAP_BACK).await;
            this.update(cx, |this, cx| this.finish_drag_flight(token, cx))
                .ok();
        })
        .detach();
    }

    /// Drop every piece of drag state without touching the settle marker.
    fn clear_drag_state(&mut self) {
        self.drop_target = None;
        self.previous_target = None;
        self.dragging_card = None;
        self.drag_flight = None;
    }

    /// End the current drag without a flight: the card reappears at once.
    fn finish_drag_state(&mut self, card_id: &str, cx: &mut Context<Self>) {
        self.drag_flight_token = self.drag_flight_token.wrapping_add(1);
        self.settled_card = Some((card_id.to_string(), self.drag_flight_token));
        self.clear_drag_state();
        cx.notify();
    }

    fn finish_drag_flight(&mut self, token: u64, cx: &mut Context<Self>) {
        let card_id = match self.drag_flight.as_ref() {
            Some(flight) if flight.token == token => flight.card_id.clone(),
            _ => return,
        };
        self.finish_drag_state(&card_id, cx);
    }

    /// The shell-owned ghost that flies from the release point to the slot.
    ///
    /// gpui removes its drag view on mouse-up, so the settle and snap-back
    /// flight is re-created here and animated with the snap-back curve. The
    /// overlay paints the source card's full body — meta label, due date and
    /// label dots included — because that is what was on screen under the
    /// pointer a frame earlier; a title-only shell would blink the meta row
    /// out between the drop and the settle.
    fn drag_flight_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let flight = self.drag_flight.as_ref()?;
        let t = self.tokens;
        let reduced = cx.reduce_motion();
        let from = flight.from;
        let to = flight.to.origin;
        let token = flight.token;
        let card = &flight.card;
        let body = card_body(
            card,
            card.meta_label.as_deref(),
            title_row(card.title.clone(), None, &t),
            false,
            &t,
        );
        let from_x = from.x.as_f32();
        let from_y = from.y.as_f32();
        let to_x = to.x.as_f32();
        let to_y = to.y.as_f32();
        Some(
            div()
                .id("drag-flight")
                .test_support()
                .absolute()
                .w(px(COLUMN_W - 16.0))
                .child(body)
                .with_animation(
                    format!("drag-flight-{token}"),
                    Animation::new(SNAP_BACK).with_easing(ease_snap),
                    move |el, delta| {
                        let progress = eased(reduced, delta, ease_snap);
                        el.left(px(from_x + (to_x - from_x) * progress))
                            .top(px(from_y + (to_y - from_y) * progress))
                            .opacity(0.97 + 0.03 * progress)
                    },
                )
                .into_any_element(),
        )
    }

    /// Open the card editor for `card_id`, resolving the card from the board
    /// currently on screen (mirrors included).
    pub(crate) fn open_card(&mut self, card_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        // The row being edited has no click handler; this is the belt-and-braces
        // guard against a modal opening under the field.
        if self
            .title_edit
            .as_ref()
            .is_some_and(|edit| edit.card_id == card_id)
        {
            return;
        }
        let card = {
            let Some(board) = &self.board else {
                return;
            };
            board
                .columns
                .iter()
                .flat_map(|column| column.ordered_cards())
                .find(|card| card.id == card_id)
                .cloned()
        };
        let Some(card) = card else {
            return;
        };
        // `list_boards` hides project boards, so a card whose home board is
        // missing there is a project card (including area-board mirrors).
        let is_project_card = !self.boards.iter().any(|board| board.id == card.board_id);
        self.project_choices = match self.store.projects() {
            Ok(choices) => choices,
            Err(error) => {
                self.board_error = Some(error);
                Vec::new()
            }
        };

        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(card.title.clone())
                .placeholder("Card title")
        });
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .default_value(card.description.clone())
                .placeholder("Add a more detailed description…")
        });
        let due = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(card.due_date.clone())
                .placeholder("YYYY-MM-DD")
        });
        title.update(cx, |state, cx| state.focus(window, cx));
        // The calendar opens on the stored date's month, or the current month.
        let due_month = card
            .due_date
            .trim()
            .parse::<Date>()
            .unwrap_or_else(|_| jiff::Zoned::now().date())
            .first_of_month();
        // Vim starts in Normal mode: the description only accepts plain typing
        // after `i`, so it starts read-only unless the preference is off.
        let mut vim = crate::vim::VimSession::new();
        vim.set_enabled(self.vim_mode, &description, cx);
        let focus = cx.focus_handle();

        self.editing = Some(EditSession {
            card_id: card.id,
            is_project_card,
            home_board_id: card.board_id,
            title,
            description,
            due,
            due_menu: false,
            due_month,
            due_anchor: Rc::new(RefCell::new(None)),
            due_viewport: Rc::new(RefCell::new(None)),
            focus,
            vim,
            labels: card.labels,
            project_id: card.project_id,
            project_menu: false,
            project_anchor: Rc::new(RefCell::new(None)),
            area_board_id: card.area_board_id,
            preview: false,
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn close_card(&mut self, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            cx.notify();
        }
    }

    /// Persist the open editor. A blank title keeps the modal open with an
    /// error, matching the egui modal. Returns whether the card was written.
    fn persist_editing(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(mut session) = self.editing.take() else {
            return false;
        };
        let title = session.title.read(cx).value().trim().to_string();
        if title.is_empty() {
            session.error = Some("A title is required".into());
            self.editing = Some(session);
            cx.notify();
            return false;
        }
        let body = CreateCardBody {
            title,
            description: Some(session.description.read(cx).value().to_string()),
            due_date: Some(session.due.read(cx).value().to_string()),
            labels: Some(session.labels.clone()),
        };
        // Project cards pick an area board; normal cards pick a project.
        // Passing `None` for the other link leaves it untouched.
        let result = if session.is_project_card {
            self.store.save_card(
                &session.card_id,
                &body,
                None,
                Some(session.area_board_id.as_deref()),
            )
        } else {
            self.store.save_card(
                &session.card_id,
                &body,
                Some(session.project_id.as_deref()),
                None,
            )
        };
        match result {
            Ok(()) => {
                self.board_error = None;
                self.reload_active_board();
            }
            Err(error) => {
                session.error = Some(error);
                self.editing = Some(session);
                cx.notify();
                return false;
            }
        }
        self.editing = Some(session);
        cx.notify();
        true
    }

    /// Save and close (`Esc`, the Save button, `:q`, `:wq`, `:x`).
    pub(crate) fn save_editing(&mut self, cx: &mut Context<Self>) {
        if self.persist_editing(cx) {
            self.editing = None;
            cx.notify();
        }
    }

    /// Save and keep the editor open (`:w`).
    pub(crate) fn save_editing_and_keep_open(&mut self, cx: &mut Context<Self>) {
        self.persist_editing(cx);
    }

    /// Persist the editor, then move the card to `board_id` and close. Edits
    /// are written first so a move never discards what the user typed; an
    /// error keeps the modal open.
    pub(crate) fn move_editing_card(&mut self, board_id: String, cx: &mut Context<Self>) {
        let Some(card_id) = self.editing.as_ref().map(|session| session.card_id.clone()) else {
            return;
        };
        if !self.persist_editing(cx) {
            return;
        }
        match self.store.move_card_to_board(&card_id, &board_id) {
            Ok(()) => {
                self.editing = None;
                self.reload_displayed_board();
            }
            Err(error) => {
                if let Some(session) = self.editing.as_mut() {
                    session.error = Some(error);
                }
            }
        }
        cx.notify();
    }

    /// Keys routed to the open card modal before the textarea sees them.
    ///
    /// `Esc` in Normal mode (or outside the description) closes the editor;
    /// otherwise the description's vim layer owns the key and consumes it.
    pub(crate) fn handle_card_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.editing.as_ref() else {
            return;
        };
        // A popup inside the editor takes Escape first: it closes the popup,
        // not the whole modal.
        if event.keystroke.key == "escape" && (session.project_menu || session.due_menu) {
            cx.stop_propagation();
            self.close_editing_menu(cx);
            return;
        }
        let description_focused = session
            .description
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let escape = event.keystroke.key == "escape";
        if !description_focused || session.preview {
            if escape {
                cx.stop_propagation();
                self.close_card(cx);
            }
            return;
        }

        let description = session.description.clone();
        let outcome = match self.editing.as_mut() {
            Some(session) => session.vim.handle_key(event, &description, window, cx),
            None => return,
        };
        if outcome.handled {
            cx.stop_propagation();
            cx.notify();
        }
        match outcome.ex {
            Some(vim::ExCommand::Write) => self.save_editing_and_keep_open(cx),
            Some(vim::ExCommand::Close) => self.save_editing(cx),
            None => {
                if escape && !outcome.handled {
                    cx.stop_propagation();
                    self.close_card(cx);
                }
            }
        }
    }

    pub(crate) fn toggle_editing_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.editing.as_mut() {
            session.preview = !session.preview;
            cx.notify();
        }
    }

    pub(crate) fn toggle_editing_label(&mut self, label: i64, cx: &mut Context<Self>) {
        if let Some(session) = self.editing.as_mut() {
            if let Some(index) = session.labels.iter().position(|value| *value == label) {
                session.labels.remove(index);
            } else {
                session.labels.push(label);
            }
            cx.notify();
        }
    }

    /// Apply a project pick from the modal's dropdown. The link is written
    /// immediately through `kandrium_core::set_card_project`, then the open
    /// board is reloaded so its rows and `meta_label` catch up without the
    /// editor closing. An empty id clears the link.
    pub(crate) fn set_editing_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        let Some(card_id) = self.editing.as_ref().map(|session| session.card_id.clone()) else {
            return;
        };
        let next = (!project_id.is_empty()).then_some(project_id);
        let result = self.store.set_card_project(&card_id, next.as_deref());
        if let Some(session) = self.editing.as_mut() {
            session.project_menu = false;
            match &result {
                Ok(()) => {
                    session.project_id = next;
                    session.error = None;
                }
                Err(error) => session.error = Some(error.clone()),
            }
        }
        if result.is_ok() {
            self.reload_active_board();
        }
        cx.notify();
    }

    /// Toggle the modal's project dropdown.
    pub(crate) fn toggle_editing_project_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.editing.as_mut() {
            session.project_menu = !session.project_menu;
            cx.notify();
        }
    }

    /// Close any open modal popup (project dropdown, calendar) without
    /// touching values.
    pub(crate) fn close_editing_menu(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.editing.as_mut() else {
            return;
        };
        if session.project_menu || session.due_menu {
            session.project_menu = false;
            session.due_menu = false;
            cx.notify();
        }
    }

    /// Open or close the due-date calendar. Opening seeds the displayed month
    /// from whatever is typed into the field (falling back to today).
    pub(crate) fn toggle_editing_due_menu(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.editing.as_mut() else {
            return;
        };
        if session.due_menu {
            session.due_menu = false;
            cx.notify();
            return;
        }
        let today = jiff::Zoned::now().date();
        let typed = session.due.read(cx).value().trim().to_string();
        session.due_month = typed.parse::<Date>().unwrap_or(today).first_of_month();
        session.due_menu = true;
        cx.notify();
    }

    /// Move the calendar to another month (arrows and Today).
    pub(crate) fn set_editing_due_month(&mut self, month: Date, cx: &mut Context<Self>) {
        if let Some(session) = self.editing.as_mut() {
            session.due_month = month.first_of_month();
            cx.notify();
        }
    }

    /// Pick a day in the calendar: write it into the field and persist it
    /// through the card update path.
    pub(crate) fn pick_editing_due(
        &mut self,
        date: Date,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.editing.as_mut() else {
            return;
        };
        let due = session.due.clone();
        due.update(cx, |state, cx| {
            state.set_value(date.to_string(), window, cx)
        });
        session.due_month = date.first_of_month();
        session.due_menu = false;
        self.persist_editing(cx);
        cx.notify();
    }

    /// Clear the due date from the calendar and persist it.
    pub(crate) fn clear_editing_due(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.editing.as_mut() else {
            return;
        };
        let due = session.due.clone();
        due.update(cx, |state, cx| state.set_value("", window, cx));
        session.due_menu = false;
        self.persist_editing(cx);
        cx.notify();
    }

    /// Set the area board of a project card. An empty id unfiles it.
    pub(crate) fn set_editing_area(&mut self, area_id: String, cx: &mut Context<Self>) {
        if let Some(session) = self.editing.as_mut() {
            session.area_board_id = (!area_id.is_empty()).then_some(area_id);
            cx.notify();
        }
    }

    fn reload_active_board(&mut self) {
        self.reload_displayed_board();
    }

    /// Reload whichever board the current section is showing: a project's
    /// board while its Tasks tab is open, otherwise the Tasks board.
    fn reload_displayed_board(&mut self) {
        let target = self
            .project_board_shown
            .clone()
            .or_else(|| self.active_board_id.clone());
        let Some(id) = target else {
            return;
        };
        let result = if self.project_board_shown.is_some() {
            self.load_project_board(&id)
        } else {
            self.load_board(&id)
        };
        if let Err(error) = result {
            self.board_error = Some(error);
        }
    }

    /// Load a project's board into the shared surface without touching
    /// `active_board_id`; the Tasks section keeps its own board selection.
    fn load_project_board(&mut self, id: &str) -> Result<(), String> {
        self.clear_drag_state();
        match self.store.board(id) {
            Ok(board) => {
                self.project_board_shown = Some(board.id.clone());
                self.board = Some(board);
                self.board_error = None;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Point the shared board surface at the project Tasks tab when it is
    /// active, and release it when it is not.
    fn sync_project_board(&mut self) {
        match self.projects.project_board_id().map(str::to_string) {
            Some(want) if self.project_board_shown.as_deref() == Some(want.as_str()) => {}
            Some(want) => {
                if let Err(error) = self.load_project_board(&want) {
                    self.board_error = Some(error);
                }
            }
            None => self.project_board_shown = None,
        }
    }

    // -----------------------------------------------------------------------
    // Projects
    // -----------------------------------------------------------------------

    /// Reload the tree and archived list, keeping a valid selection. Project
    /// tabs are rebuilt only when the selected project actually changes.
    fn load_projects(&mut self) {
        let root = match self.store.projects_root() {
            Ok(root) => root,
            Err(error) => {
                self.projects.error = Some(error.clone());
                return;
            }
        };
        // `read_dir` reports canonical child paths. Canonicalize the root once
        // so `rel_path` (used by every doc read/write) strips successfully even
        // when the configured root runs through a symlink (`/tmp`, `/var`).
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let tree = match self.store.project_tree() {
            Ok(tree) => tree,
            Err(error) => {
                self.projects.error = Some(error.clone());
                return;
            }
        };
        let archived = match self.store.archived_projects() {
            Ok(archived) => archived,
            Err(error) => {
                self.projects.error = Some(error.clone());
                return;
            }
        };
        self.projects.root = Some(root);
        self.projects.tree = tree;
        self.projects.archived = archived;
        self.projects.error = None;
        // The tree can expand a project's documents in place, and a project
        // with no documents shows no chevron. One scan per project is enough
        // to answer both, refreshed whenever the project list reloads.
        self.projects.doc_trees.clear();
        if let Some(root) = self.projects.root.clone() {
            let ids = projects::project_ids(&self.projects.tree);
            for id in ids {
                let entries = self.store.project_files(&root, &id).unwrap_or_default();
                self.projects.doc_trees.insert(id, entries);
            }
        }

        let selected_valid = self.projects.selected.as_deref().is_some_and(|id| {
            kandrium_core::projects::find_project(&self.projects.tree, id).is_some()
                || self
                    .projects
                    .archived
                    .iter()
                    .any(|project| project.id == id)
        });
        if !selected_valid {
            self.projects.selected = if self.projects.show_archived {
                self.projects
                    .archived
                    .first()
                    .map(|project| project.id.clone())
            } else {
                projects::first_project(&self.projects.tree).map(|project| project.id.clone())
            };
        }

        if self.projects.tabs_project != self.projects.selected {
            let keep_tasks = self.projects.active_is_tasks() && self.projects.selected.is_some();
            self.projects.tabs = vec![ProjectTab::Overview, ProjectTab::Tasks];
            self.projects.active = if keep_tasks { 1 } else { 0 };
            self.projects.tabs_project = self.projects.selected.clone();
            self.projects.detail_editor = None;
            self.projects.title_edit = None;
            self.projects.doc_draft = None;
            self.projects.doc_folder = PathBuf::new();
        }
        self.reload_project_detail();
    }

    /// Read the selected project, its summary and its document tree.
    fn reload_project_detail(&mut self) {
        let (Some(root), Some(id)) = (self.projects.root.clone(), self.projects.selected.clone())
        else {
            self.projects.detail = None;
            self.projects.summary = None;
            self.projects.files.clear();
            self.projects.paths.clear();
            return;
        };
        match self.store.project(&id) {
            Ok(project) => {
                self.projects.summary = self.store.project_summary(&root, &id).ok();
                self.projects.files = self.store.project_files(&root, &id).unwrap_or_default();
                self.projects.paths = self.store.project_paths(&id).unwrap_or_default();
                // Keep the tree's in-place document rows in step with edits
                // made through the Overview.
                self.projects
                    .doc_trees
                    .insert(id.clone(), self.projects.files.clone());
                self.projects.detail = Some(project);
            }
            Err(error) => {
                self.projects.error = Some(error);
                self.projects.detail = None;
                self.projects.summary = None;
                self.projects.files.clear();
                self.projects.paths.clear();
            }
        }
    }

    /// Create the Overview inputs once the selected project is known. Called
    /// from the render path, which is the first place with a window.
    fn ensure_project_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.projects.detail.clone() else {
            self.projects.detail_editor = None;
            return;
        };
        if self
            .projects
            .detail_editor
            .as_ref()
            .is_some_and(|editor| editor.project_id == project.id)
        {
            return;
        }
        let identifier =
            cx.new(|cx| InputState::new(window, cx).default_value(project.identifier.clone()));
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .default_value(project.description.clone())
                .placeholder("Add a description… Markdown is supported")
        });
        // The identifier only writes on Save; track dirtiness so the button
        // can appear and a duplicate can be reported inline.
        cx.subscribe_in(
            &identifier,
            window,
            |this: &mut AppShell, input, event: &InputEvent, _window, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let value = input.read(cx).value().trim().to_string();
                if let Some(editor) = this.projects.detail_editor.as_mut() {
                    editor.identifier_dirty = value != editor.saved_identifier;
                    editor.identifier_error = None;
                }
                cx.notify();
            },
        )
        .detach();
        // The description autosaves: a debounced timer per keystroke, plus a
        // flush on blur. Vim is off for this field.
        cx.subscribe_in(
            &description,
            window,
            |this: &mut AppShell, _input, event: &InputEvent, _window, cx| match event {
                InputEvent::Change => this.schedule_project_description_autosave(cx),
                InputEvent::Blur => this.save_project_description(cx),
                _ => {}
            },
        )
        .detach();
        let saved_identifier = project.identifier.clone();
        self.projects.detail_editor = Some(DetailEditor {
            project_id: project.id.clone(),
            identifier,
            description,
            saved_description: project.description,
            preview: false,
            description_generation: 0,
            saved_identifier,
            identifier_dirty: false,
            identifier_error: None,
        });
    }

    fn ensure_project_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.projects.filter_input.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search projects…"));
        cx.subscribe_in(
            &input,
            window,
            |this: &mut AppShell, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.projects.filter = input.read(cx).value().to_string();
                    cx.notify();
                }
            },
        )
        .detach();
        self.projects.filter_input = Some(input);
    }

    pub(crate) fn select_project(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.projects.selected.as_deref() == Some(id) {
            return;
        }
        self.flush_project_fields(window, cx);
        self.save_project_description(cx);
        self.flush_project_docs(cx);
        self.projects.selected = Some(id.to_string());
        self.projects.selected_folder = project_folder(&self.projects.tree, id).or_else(|| {
            self.projects
                .archived
                .iter()
                .find(|p| p.id == id)
                .and_then(|p| p.folder_id.clone())
        });
        let keep_tasks = self.projects.active_is_tasks();
        self.projects.tabs = vec![ProjectTab::Overview, ProjectTab::Tasks];
        self.projects.active = if keep_tasks { 1 } else { 0 };
        self.projects.tabs_project = Some(id.to_string());
        self.projects.detail_editor = None;
        self.projects.title_edit = None;
        self.projects.doc_draft = None;
        self.projects.doc_folder = PathBuf::new();
        self.projects.confirm = None;
        self.reload_project_detail();
        cx.notify();
    }

    pub(crate) fn toggle_project_folder(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.projects.expanded.remove(&id) {
            self.projects.expanded.insert(id.clone());
        }
        self.projects.selected_folder = Some(id);
        cx.notify();
    }

    /// Expand or collapse a project's documents in the tree. Session state:
    /// the document cache is already loaded with the project list.
    pub(crate) fn toggle_project_docs(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.projects.docs_open.remove(id) {
            self.projects.docs_open.insert(id.to_string());
        }
        cx.notify();
    }

    /// Expand or collapse one document folder under an open project.
    pub(crate) fn toggle_project_doc_folder(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.projects.doc_folders_open.remove(&key) {
            self.projects.doc_folders_open.insert(key);
        }
        cx.notify();
    }

    /// Expands or collapses the footer's inline archived list without leaving
    /// the active tree. Archiving or restoring a project reloads the list
    /// already, but re-reading on open keeps it fresh after external commits.
    pub(crate) fn toggle_archived_disclosure(&mut self, cx: &mut Context<Self>) {
        self.projects.archived_open = !self.projects.archived_open;
        if self.projects.archived_open
            && let Ok(archived) = self.store.archived_projects()
        {
            self.projects.archived = archived;
        }
        cx.notify();
    }

    pub(crate) fn begin_project_draft(
        &mut self,
        kind: ProjectDraftKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.projects.draft.is_some() {
            return;
        }
        let placeholder = match kind {
            ProjectDraftKind::Project => "Project name",
            ProjectDraftKind::Folder => "Folder name",
            ProjectDraftKind::RenameFolder => "New name",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |state, cx| state.focus(window, cx));
        self.projects.draft = Some(ProjectDraft {
            kind,
            folder: self.projects.selected_folder.clone(),
            input,
        });
        self.projects.error = None;
        cx.notify();
    }

    pub(crate) fn begin_project_folder_rename(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = folder_name(&self.projects.tree, id) else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        input.update(cx, |state, cx| state.focus(window, cx));
        self.projects.draft = Some(ProjectDraft {
            kind: ProjectDraftKind::RenameFolder,
            folder: Some(id.to_string()),
            input,
        });
        self.projects.error = None;
        cx.notify();
    }

    pub(crate) fn handle_project_draft_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                self.projects.draft = None;
                cx.notify();
            }
            "enter" => {
                cx.stop_propagation();
                self.commit_project_draft(window, cx);
            }
            _ => {}
        }
    }

    fn commit_project_draft(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &self.projects.draft else {
            return;
        };
        let kind = draft.kind;
        let folder = draft.folder.clone();
        let name = draft.input.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.projects.error = Some("A name is required".into());
            cx.notify();
            return;
        }
        let result: Result<Option<String>, String> = match kind {
            ProjectDraftKind::Project => {
                let Some(root) = self.projects.root.clone() else {
                    self.projects.error = Some("The projects root is unavailable".into());
                    cx.notify();
                    return;
                };
                self.store
                    .create_project(&root, &name, folder.as_deref())
                    .map(|project| Some(project.id))
            }
            ProjectDraftKind::Folder => self
                .store
                .create_project_folder(&name, folder.as_deref())
                .map(|folder| {
                    self.projects.expanded.insert(folder.id.clone());
                    self.projects.selected_folder = Some(folder.id.clone());
                    None
                }),
            ProjectDraftKind::RenameFolder => {
                let Some(id) = folder.as_deref() else {
                    return;
                };
                self.store.rename_project_folder(id, &name).map(|_| None)
            }
        };
        match result {
            Ok(selected) => {
                if let Some(id) = selected {
                    self.projects.selected = Some(id);
                }
                self.projects.draft = None;
                self.projects.error = None;
                self.load_projects();
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    pub(crate) fn begin_project_drag(&mut self, id: &str, cx: &mut Context<Self>) {
        let from_folder = project_folder(&self.projects.tree, id);
        self.projects.drag = Some(crate::views::projects::ProjectDragState {
            id: id.to_string(),
            from_folder,
            hover: None,
        });
        cx.notify();
    }

    pub(crate) fn hover_project_drop(&mut self, target: &str, after: bool, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.projects.drag else {
            return;
        };
        let hover = Some((target.to_string(), after));
        if drag.hover != hover {
            drag.hover = hover;
            cx.notify();
        }
    }

    pub(crate) fn drop_project(
        &mut self,
        drag: &crate::views::projects::ProjectDrag,
        cx: &mut Context<Self>,
    ) {
        let hover = self
            .projects
            .drag
            .as_ref()
            .and_then(|session| session.hover.clone());
        self.projects.drag = None;
        let Some((target, after)) = hover else {
            cx.notify();
            return;
        };
        let siblings = sibling_projects(&self.projects.tree, drag.from_folder.as_deref());
        if !siblings.contains(&target) || target == drag.id {
            cx.notify();
            return;
        }
        let mut order: Vec<String> = siblings.into_iter().filter(|id| id != &drag.id).collect();
        let index = projects::insert_index(&order, &target, after);
        order.insert(index, drag.id.clone());
        if let Err(error) = self.store.reorder_project(&drag.id, index) {
            self.projects.error = Some(error);
        } else {
            self.load_projects();
        }
        cx.notify();
    }

    pub(crate) fn begin_project_title_edit(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.detail.as_ref() else {
            return;
        };
        if project.id != id {
            return;
        }
        let name = project.name.clone();
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |state, cx| {
            // `set_value` leaves the caret at the end of a single-line field.
            state.set_value(name.clone(), window, cx);
            state.focus(window, cx);
        });
        cx.subscribe_in(
            &input,
            window,
            |this: &mut AppShell, _input, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    this.commit_project_title(window, cx)
                }
                InputEvent::Change => this.schedule_project_title_autosave(cx),
                _ => {}
            },
        )
        .detach();
        self.projects.title_edit = Some(ProjectTitleEdit {
            project_id: id.to_string(),
            input,
            original: name.clone(),
            saved: name,
            generation: 0,
        });
        cx.notify();
    }

    /// Debounce the title's autosave: every keystroke bumps the generation and
    /// schedules a write; a newer keystroke makes the older timer a no-op.
    fn schedule_project_title_autosave(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.projects.title_edit.as_mut() else {
            return;
        };
        edit.generation = edit.generation.wrapping_add(1);
        let generation = edit.generation;
        let project_id = edit.project_id.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            this.update(cx, |this, cx| {
                this.autosave_project_title(&project_id, generation, cx)
            })
            .ok();
        })
        .detach();
    }

    /// Write the title in place without closing the field. Whitespace-only
    /// names are rejected and stay in the editor.
    fn autosave_project_title(
        &mut self,
        project_id: &str,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.projects.title_edit.as_ref() else {
            return;
        };
        if edit.project_id != project_id || edit.generation != generation {
            return;
        }
        let name = edit.input.read(cx).value().trim().to_string();
        if name.is_empty() || name == edit.saved {
            return;
        }
        match self.store.rename_project(project_id, &name) {
            Ok(_) => {
                if let Some(edit) = self.projects.title_edit.as_mut() {
                    edit.saved = name;
                }
                self.projects.error = None;
                // The rename may regenerate the identifier, and the board
                // title follows the name, so refresh both.
                self.projects.detail_editor = None;
                self.project_board_shown = None;
                self.load_projects();
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    pub(crate) fn project_title_clicked(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = std::time::Instant::now();
        let double = self
            .projects
            .last_title_click
            .as_ref()
            .is_some_and(|(last_id, at)| {
                last_id == id && now.duration_since(*at) < Duration::from_millis(400)
            });
        if double {
            self.projects.last_title_click = None;
            self.begin_project_title_edit(id, window, cx);
        } else {
            self.projects.last_title_click = Some((id.to_string(), now));
        }
    }

    pub(crate) fn handle_project_title_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                self.cancel_project_title(cx);
            }
            "enter" => {
                cx.stop_propagation();
                self.commit_project_title(window, cx);
            }
            _ => {}
        }
    }

    pub(crate) fn commit_project_title(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.projects.title_edit.as_ref() else {
            return;
        };
        let name = edit.input.read(cx).value().trim().to_string();
        if name.is_empty() {
            // Keep the field open: an empty name is rejected, not saved.
            self.projects.error = Some("A project name is required".into());
            cx.notify();
            return;
        }
        let project_id = edit.project_id.clone();
        if name == edit.saved {
            self.projects.title_edit = None;
            cx.notify();
            return;
        }
        match self.store.rename_project(&project_id, &name) {
            Ok(_) => {
                self.projects.title_edit = None;
                self.projects.detail_editor = None;
                self.projects.error = None;
                // The board title follows the rename; force the Tasks tab to
                // reload so its header is not stale.
                self.project_board_shown = None;
                self.load_projects();
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    /// Esc closes the editor and puts the previous name back, including when
    /// a debounced autosave already wrote the new one.
    pub(crate) fn cancel_project_title(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.projects.title_edit.take() else {
            return;
        };
        if edit.saved != edit.original
            && self
                .store
                .rename_project(&edit.project_id, &edit.original)
                .is_ok()
        {
            self.projects.detail_editor = None;
            self.project_board_shown = None;
            self.load_projects();
        }
        cx.notify();
    }

    /// Save the identifier only when explicitly asked. Validates the core
    /// charset, then checks the other projects case-insensitively so the
    /// inline error can name the conflict instead of just quoting SQLite.
    pub(crate) fn save_project_identifier(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.try_save_project_identifier(window, cx) {
            if let Some(editor) = self.projects.detail_editor.as_mut() {
                editor.identifier_error = Some(error);
                editor
                    .identifier
                    .update(cx, |state, cx| state.focus(window, cx));
            }
            cx.notify();
        }
    }

    /// Esc in the identifier field: restore the persisted value.
    pub(crate) fn cancel_project_identifier(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.projects.detail_editor.as_mut() else {
            return;
        };
        let saved = editor.saved_identifier.clone();
        editor.identifier_error = None;
        editor.identifier_dirty = false;
        editor
            .identifier
            .update(cx, |state, cx| state.set_value(saved, window, cx));
        cx.notify();
    }

    fn try_save_project_identifier(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> std::result::Result<(), String> {
        let Some(editor) = self.projects.detail_editor.as_ref() else {
            return Ok(());
        };
        let Some(project) = self.projects.detail.as_ref() else {
            return Ok(());
        };
        let raw = editor.identifier.read(cx).value();
        let identifier = kandrium_core::validate_identifier(&raw).map_err(|e| e.to_string())?;
        let project_id = project.id.clone();
        if identifier == project.identifier {
            if let Some(editor) = self.projects.detail_editor.as_mut() {
                editor.identifier_dirty = false;
                editor.identifier_error = None;
            }
            return Ok(());
        }
        if let Some(taken) = self.identifier_owner(&identifier, &project_id) {
            return Err(format!(
                "Identifier \"{identifier}\" is already used by \"{taken}\""
            ));
        }
        self.store
            .set_project_identifier(&project_id, &identifier)
            .map_err(|e| e.to_string())?;
        if let Some(editor) = self.projects.detail_editor.as_mut() {
            editor.saved_identifier = identifier.clone();
            editor.identifier_dirty = false;
            editor.identifier_error = None;
            editor.identifier.update(cx, |state, cx| {
                state.set_value(identifier.clone(), window, cx);
            });
        }
        self.projects.error = None;
        // The tree labels projects with the identifier; refresh it.
        self.load_projects();
        cx.notify();
        Ok(())
    }

    /// Name of a different project that already owns this identifier,
    /// compared case-insensitively across the tree and the archive.
    fn identifier_owner(&self, identifier: &str, project_id: &str) -> Option<String> {
        let taken = |project: &&kandrium_core::Project| {
            project.id != project_id && project.identifier.eq_ignore_ascii_case(identifier)
        };
        crate::views::projects::flat_projects(&self.projects.tree)
            .into_iter()
            .map(|(project, _)| project)
            .find(taken)
            .map(|project| project.name.clone())
            .or_else(|| {
                self.projects
                    .archived
                    .iter()
                    .find(taken)
                    .map(|project| project.name.clone())
            })
    }

    /// Persist a valid identifier edit; used when the project is about to
    /// stop being displayed so a valid edit is not silently dropped on a
    /// switch (matching egui's commit-on-blur). Returns `false` when the value
    /// was rejected; the message is left inline.
    fn flush_project_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Err(error) = self.try_save_project_identifier(window, cx) {
            if let Some(editor) = self.projects.detail_editor.as_mut() {
                editor.identifier_error = Some(error);
            }
            return false;
        }
        true
    }

    pub(crate) fn set_project_status(&mut self, status: &str, cx: &mut Context<Self>) {
        let Some(project) = &self.projects.detail else {
            return;
        };
        let id = project.id.clone();
        match self.store.update_project(&id, None, Some(status)) {
            Ok(_) => {
                self.projects.error = None;
                self.reload_project_detail();
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    pub(crate) fn save_project_description(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.projects.detail_editor else {
            return;
        };
        let Some(project) = &self.projects.detail else {
            return;
        };
        let description = editor.description.read(cx).value().to_string();
        if description == editor.saved_description {
            return;
        }
        let id = project.id.clone();
        match self.store.update_project(&id, Some(&description), None) {
            Ok(_) => {
                if let Some(editor) = self.projects.detail_editor.as_mut() {
                    editor.saved_description = description;
                }
                self.projects.error = None;
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    /// Switch the Overview description between its editor and the Markdown
    /// preview. The textarea stays mounted in edit mode only, but the text
    /// lives on the entity, so toggling back returns the same content.
    pub(crate) fn set_project_description_preview(
        &mut self,
        preview: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.projects.detail_editor.as_mut() else {
            return;
        };
        if editor.preview == preview {
            return;
        }
        editor.preview = preview;
        cx.notify();
    }

    pub(crate) fn archive_selected_project(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.projects.detail else {
            return;
        };
        let id = project.id.clone();
        match self.store.archive_project(&id) {
            Ok(()) => {
                self.projects.show_archived = false;
                self.projects.selected = None;
                self.load_projects();
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    pub(crate) fn restore_selected_project(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.projects.detail else {
            return;
        };
        let id = project.id.clone();
        match self.store.unarchive_project(&id) {
            Ok(()) => {
                self.projects.show_archived = false;
                self.projects.selected = Some(id);
                self.load_projects();
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    pub(crate) fn ask_delete_selected_project(&mut self, cx: &mut Context<Self>) {
        let Some(project) = &self.projects.detail else {
            return;
        };
        self.projects.confirm = Some(ProjectConfirm {
            kind: ProjectConfirmKind::DeleteProject,
            id: project.id.clone(),
            name: project.name.clone(),
        });
        cx.notify();
    }

    pub(crate) fn ask_delete_folder(&mut self, id: &str, name: &str, cx: &mut Context<Self>) {
        self.projects.confirm = Some(ProjectConfirm {
            kind: ProjectConfirmKind::DeleteFolder,
            id: id.to_string(),
            name: name.to_string(),
        });
        cx.notify();
    }

    pub(crate) fn cancel_project_confirm(&mut self, cx: &mut Context<Self>) {
        if self.projects.confirm.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn confirm_project_delete(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.projects.confirm.take() else {
            return;
        };
        let Some(root) = self.projects.root.clone() else {
            return;
        };
        let result = match confirm.kind {
            ProjectConfirmKind::DeleteProject => {
                if self.projects.selected.as_deref() == Some(confirm.id.as_str()) {
                    self.projects.selected = None;
                }
                self.store.delete_project(&root, &confirm.id)
            }
            ProjectConfirmKind::DeleteFolder => {
                self.projects.selected_folder = None;
                self.store.delete_project_folder(&root, &confirm.id)
            }
        };
        match result {
            Ok(()) => {
                self.projects.error = None;
                self.load_projects();
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    pub(crate) fn activate_project_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.projects.tabs.len() || self.projects.active == index {
            return;
        }
        if let Some(doc) = self.projects.active_doc_index() {
            self.save_project_doc(doc, cx);
        }
        self.projects.active = index;
        cx.notify();
    }

    // ---- Project documents -------------------------------------------------

    pub(crate) fn open_project_doc(
        &mut self,
        rel: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.projects.tab_index(&rel) {
            self.projects.active = index;
            if let Some(ProjectTab::Doc(doc)) = self.projects.tabs.get(index) {
                let editor = doc.editor.clone();
                editor.update(cx, |state, cx| state.focus(window, cx));
            }
            cx.notify();
            return;
        }
        let (Some(root), Some(id)) = (self.projects.root.clone(), self.projects.selected.clone())
        else {
            return;
        };
        match self.store.read_project_doc(&root, &id, &rel) {
            Ok(text) => {
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .default_value(text.clone())
                        .placeholder("Start writing… Markdown is supported")
                        .language("markdown")
                });
                let mut session = crate::vim::VimSession::new();
                session.set_enabled(self.vim_mode, &editor, cx);
                cx.observe(&editor, |this: &mut AppShell, editor, cx| {
                    this.project_doc_edited(&editor, cx)
                })
                .detach();
                let name = rel
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.projects.tabs.push(ProjectTab::Doc(ProjectDocTab {
                    rel,
                    name,
                    editor: editor.clone(),
                    vim: session,
                    preview: false,
                    saved: text,
                    dirty: false,
                    generation: 0,
                }));
                self.projects.active = self.projects.tabs.len() - 1;
                self.projects.error = None;
                editor.update(cx, |state, cx| state.focus(window, cx));
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    pub(crate) fn close_project_doc(&mut self, index: usize, cx: &mut Context<Self>) {
        let closable: Vec<bool> = self
            .projects
            .tabs
            .iter()
            .map(ProjectTab::is_closable)
            .collect();
        let Some(active) = projects::close_index(&closable, self.projects.active, index) else {
            return;
        };
        self.save_project_doc(index, cx);
        self.projects.tabs.remove(index);
        self.projects.active = active;
        cx.notify();
    }

    fn project_doc_edited(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        let Some(index) = self
            .projects
            .tabs
            .iter()
            .position(|tab| matches!(tab, ProjectTab::Doc(doc) if doc.editor == *editor))
        else {
            return;
        };
        let value = editor.read(cx).value().to_string();
        let ProjectTab::Doc(tab) = &mut self.projects.tabs[index] else {
            return;
        };
        if value == tab.saved {
            if tab.dirty {
                tab.dirty = false;
                cx.notify();
            }
            return;
        }
        tab.dirty = true;
        tab.generation = tab.generation.wrapping_add(1);
        let generation = tab.generation;
        let rel = tab.rel.clone();
        cx.notify();
        let selected = self.projects.selected.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::views::projects::AUTOSAVE)
                .await;
            this.update(cx, |this, cx| {
                if this.projects.selected == selected {
                    this.autosave_project_doc(&rel, generation, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn autosave_project_doc(&mut self, rel: &Path, generation: u64, cx: &mut Context<Self>) {
        let Some(index) = self.projects.tab_index(rel) else {
            return;
        };
        if let Some(ProjectTab::Doc(tab)) = self.projects.tabs.get(index)
            && tab.generation != generation
        {
            return;
        }
        self.save_project_doc(index, cx);
    }

    fn save_project_doc(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(ProjectTab::Doc(tab)) = self.projects.tabs.get(index) else {
            return;
        };
        if !tab.dirty {
            return;
        }
        let (Some(root), Some(id)) = (self.projects.root.clone(), self.projects.selected.clone())
        else {
            return;
        };
        let rel = tab.rel.clone();
        let value = tab.editor.read(cx).value().to_string();
        match self.store.write_project_doc(&root, &id, &rel, &value) {
            Ok(()) => {
                if let Some(ProjectTab::Doc(tab)) = self.projects.tabs.get_mut(index) {
                    tab.saved = value;
                    tab.dirty = false;
                }
                self.projects.error = None;
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    /// Write every dirty project document; called on project and section
    /// switches so a tab close can never lose an edit.
    pub(crate) fn flush_project_docs(&mut self, cx: &mut Context<Self>) {
        for index in 0..self.projects.tabs.len() {
            self.save_project_doc(index, cx);
        }
    }

    pub(crate) fn toggle_project_doc_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.projects.active_doc_index() else {
            return;
        };
        let Some(ProjectTab::Doc(tab)) = self.projects.tabs.get_mut(index) else {
            return;
        };
        tab.preview = !tab.preview;
        let editor = tab.editor.clone();
        editor.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn handle_project_doc_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.projects.active_doc_index() else {
            return;
        };
        let Some(ProjectTab::Doc(tab)) = self.projects.tabs.get(index) else {
            return;
        };
        if tab.preview {
            return;
        }
        let editor = tab.editor.clone();
        if !editor.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        let outcome = match self.projects.tabs.get_mut(index) {
            Some(ProjectTab::Doc(tab)) => tab.vim.handle_key(event, &editor, window, cx),
            _ => return,
        };
        if outcome.handled {
            cx.stop_propagation();
            cx.notify();
        }
        match outcome.ex {
            Some(vim::ExCommand::Write) => self.save_project_doc(index, cx),
            Some(vim::ExCommand::Close) => self.close_project_doc(index, cx),
            None => {
                if event.keystroke.key == "escape" && !outcome.handled {
                    cx.stop_propagation();
                    self.close_project_doc(index, cx);
                }
            }
        }
    }

    /// Debounce the description autosave. Every keystroke bumps the
    /// generation and schedules a write; a newer keystroke makes the older
    /// timer a no-op, so typing is never interrupted by a reload.
    fn schedule_project_description_autosave(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.projects.detail_editor.as_mut() else {
            return;
        };
        editor.description_generation = editor.description_generation.wrapping_add(1);
        let generation = editor.description_generation;
        let project_id = editor.project_id.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            this.update(cx, |this, cx| {
                this.autosave_project_description(&project_id, generation, cx)
            })
            .ok();
        })
        .detach();
    }

    fn autosave_project_description(
        &mut self,
        project_id: &str,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.projects.detail_editor.as_ref() else {
            return;
        };
        if editor.project_id != project_id || editor.description_generation != generation {
            return;
        }
        self.save_project_description(cx);
    }

    pub(crate) fn begin_project_doc_draft(
        &mut self,
        kind: ProjectDocDraftKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let folder = self.projects.doc_folder.clone();
        self.begin_project_doc_draft_in(folder, kind, window, cx);
    }

    pub(crate) fn begin_project_doc_draft_in(
        &mut self,
        folder: PathBuf,
        kind: ProjectDocDraftKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.projects.doc_draft.is_some() {
            return;
        }
        let placeholder = match kind {
            ProjectDocDraftKind::File => "name.md",
            ProjectDocDraftKind::Folder => "Folder name",
        };
        // The draft field lives in the Overview's document list, so surface
        // that tab — the header actions start these drafts from the tree.
        self.projects.active = 0;
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |state, cx| state.focus(window, cx));
        self.projects.doc_draft = Some(ProjectDocDraft {
            kind,
            folder,
            input,
        });
        self.projects.error = None;
        cx.notify();
    }

    pub(crate) fn handle_project_doc_draft_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                self.projects.doc_draft = None;
                cx.notify();
            }
            "enter" => {
                cx.stop_propagation();
                self.commit_project_doc_draft(window, cx);
            }
            _ => {}
        }
    }

    fn commit_project_doc_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &self.projects.doc_draft else {
            return;
        };
        let kind = draft.kind;
        let folder = draft.folder.clone();
        let name = draft.input.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.projects.error = Some("A name is required".into());
            cx.notify();
            return;
        }
        let (Some(root), Some(id)) = (self.projects.root.clone(), self.projects.selected.clone())
        else {
            return;
        };
        let result = match kind {
            ProjectDocDraftKind::File => self
                .store
                .create_project_doc(&root, &id, &folder, &name)
                .map(Some),
            ProjectDocDraftKind::Folder => self
                .store
                .create_project_doc_folder(&root, &id, &folder, &name)
                .map(|_| None),
        };
        match result {
            Ok(created) => {
                self.projects.doc_draft = None;
                self.projects.doc_folder = folder;
                self.projects.error = None;
                self.reload_project_detail();
                if let Some(created) = created
                    && let Some(rel) = self.projects.rel_path(&created)
                {
                    self.open_project_doc(rel, window, cx);
                }
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    /// Copy picked files into the selected project's current document folder.
    ///
    /// Mirrors the Documents section's upload: the same PDF/Markdown filter,
    /// the same collision handling (a second `report.pdf` lands as
    /// `report-2.pdf`) and the same per-file error reporting. The project
    /// directory is the guard root, so nothing can be written outside it.
    pub(crate) fn upload_project_documents(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(root), Some(id)) = (self.projects.root.clone(), self.projects.selected.clone())
        else {
            return;
        };
        let Some(files) = rfd::FileDialog::new()
            .add_filter("Documents", &["pdf", "md", "markdown"])
            .pick_files()
        else {
            return;
        };
        if files.is_empty() {
            return;
        }
        let project_root = project_dir(&root, &id);
        let requested = project_root.join(&self.projects.doc_folder);
        // A stale document folder falls back to the project root instead of
        // failing the whole upload.
        let dest = if requested.is_dir() {
            requested
        } else {
            project_root.clone()
        };
        let mut errors = Vec::new();
        for source in &files {
            match import_file(source, &dest, &project_root) {
                Ok(_) => {}
                Err(error) => errors.push(format!("{}: {error}", source.display())),
            }
        }
        self.projects.error = if errors.is_empty() {
            None
        } else {
            Some(errors.join("\n"))
        };
        self.projects.active = 0;
        self.reload_project_detail();
        cx.notify();
    }

    /// Attach a folder to the selected project through a native picker. Only
    /// the reference is stored; nothing is copied or created on disk.
    pub(crate) fn add_project_folder(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_id) = self.projects.selected.clone() else {
            return;
        };
        let Some(dir) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let path = dir.display().to_string();
        match self.store.add_project_path(&project_id, &path) {
            Ok(_) => {
                self.projects.paths = self.store.project_paths(&project_id).unwrap_or_default();
                self.projects.error = None;
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    /// Drop a folder reference from the selected project. The folder on disk
    /// is untouched.
    pub(crate) fn remove_project_folder(&mut self, path_id: &str, cx: &mut Context<Self>) {
        let Some(project_id) = self.projects.selected.clone() else {
            return;
        };
        match self.store.remove_project_path(path_id) {
            Ok(()) => {
                self.projects.paths = self.store.project_paths(&project_id).unwrap_or_default();
                self.projects.error = None;
            }
            Err(error) => self.projects.error = Some(error),
        }
        cx.notify();
    }

    /// Read one consistent snapshot of cities, people and trips. Rendering and
    /// the minute tick work from this cache, so a tick never touches SQLite.
    fn load_clocks(&mut self) {
        match self.store.clock_data() {
            Ok(data) => {
                self.clocks.load(data);
                self.clocks.error = None;
            }
            Err(error) => {
                self.clocks.entries.clear();
                self.clocks.travels.clear();
                self.clocks.error = Some(error);
            }
        }
        self.invalidate_clock_tick();
    }

    fn clock_person(&self, person_id: &str) -> Option<&Person> {
        self.clocks
            .entries
            .iter()
            .flat_map(|entry| entry.people.iter())
            .find(|person| person.id == person_id)
    }

    fn clock_travel(&self, travel_id: &str) -> Option<&Travel> {
        self.clocks
            .travels
            .iter()
            .find(|travel| travel.id == travel_id)
    }

    /// Stop the running tick loop. The loop notices the new generation and
    /// exits; the next render starts a fresh one.
    fn invalidate_clock_tick(&mut self) {
        self.clock_tick = self.clock_tick.wrapping_add(1);
        self.clock_tick_running = None;
    }

    /// Keep one tick loop alive while the shell is on screen. The titlebar
    /// clock is visible in every section, so the loop never gates on the
    /// foreground section. It waits for the nearer of the next minute and the
    /// nearest travel boundary, then refreshes `now` from the cached rows only.
    fn ensure_clock_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.clock_tick;
        if self.clock_tick_running == Some(generation) {
            return;
        }
        self.clock_tick_running = Some(generation);
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let wait = this.update_in(cx, |this, _window, cx| {
                    if this.clock_tick_running != Some(generation) {
                        return None;
                    }
                    this.clocks.set_now(Timestamp::now());
                    cx.notify();
                    Some(this.clocks.next_wait())
                });
                let Ok(Some(wait)) = wait else {
                    break;
                };
                cx.background_executor()
                    .timer(wait.max(Duration::from_millis(50)))
                    .await;
            }
        })
        .detach();
    }

    /// Whether the foreground section reads the database and therefore needs
    /// the external-change watcher. Notes and Documents are filesystem-backed.
    fn wants_watcher(&self) -> bool {
        matches!(
            self.section,
            Section::Tasks | Section::Clocks | Section::Projects
        )
    }

    /// Stop the watcher thread and retire its pump loop.
    fn stop_watcher(&mut self) {
        if self.watcher.is_some() {
            // `Drop` sets the stop flag; the thread exits within one poll.
            self.watcher = None;
        }
        if self.watcher_running.is_some() {
            self.watcher_generation = self.watcher_generation.wrapping_add(1);
            self.watcher_running = None;
        }
    }

    /// Keep one watcher alive while a database-backed section is on screen.
    /// The pump loop only drains the receiver; the thread owns the connection.
    fn ensure_watcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.wants_watcher() {
            self.stop_watcher();
            return;
        }
        if self.watcher.is_none() {
            self.watcher = Some(crate::watcher::Watcher::spawn(self.store.path.clone()));
        }
        let generation = self.watcher_generation;
        if self.watcher_running == Some(generation) {
            return;
        }
        self.watcher_running = Some(generation);
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let alive = this.update_in(cx, |this, _window, cx| {
                    if this.watcher_generation != generation || !this.wants_watcher() {
                        return None;
                    }
                    let changed = this
                        .watcher
                        .as_ref()
                        .is_some_and(crate::watcher::Watcher::drain);
                    if changed {
                        this.apply_external_change(cx);
                    }
                    Some(())
                });
                let Ok(Some(())) = alive else {
                    break;
                };
                cx.background_executor()
                    .timer(crate::watcher::POLL_INTERVAL)
                    .await;
            }
        })
        .detach();
    }

    /// Refresh only what the foreground section shows. In-flight user state
    /// (`editing`, note/document tabs, an active drag) is never rebuilt: a
    /// drag defers the reload to the next frame, and an open modal keeps its
    /// buffer while the board behind it updates.
    fn apply_external_change(&mut self, cx: &mut Context<Self>) {
        if self.dragging_card.is_some() || self.drop_target.is_some() {
            self.pending_sync = true;
            return;
        }
        if self.section == Section::Projects && self.projects.is_busy() {
            self.pending_sync = true;
            return;
        }
        // The titlebar clock lists saved cities in every section, so the
        // snapshot is always refreshed, not only while Clocks is open.
        self.load_clocks();
        match self.section {
            Section::Tasks => self.load_tasks(),
            Section::Clocks => {}
            Section::Projects => {
                self.load_projects();
                // The tree/detail reload above does not touch the board
                // surface. Refresh an open project board through the same
                // displayed-board path as a drop; this only replaces
                // `self.board`, so drafts, tabs and the detail editor stay.
                if self.project_board_shown.is_some() {
                    self.reload_displayed_board();
                }
            }
            Section::Notes | Section::Documents => return,
        }
        cx.notify();
    }

    /// Times are formatted at render time, so the toggle needs no reload.
    pub(crate) fn toggle_clock_units(&mut self, cx: &mut Context<Self>) {
        self.use_24h = !self.use_24h;
        cx.notify();
    }

    pub(crate) fn open_clock_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clocks.ensure_zones();
        let input = cx
            .new(|cx| InputState::new(window, cx).placeholder("Search city, region or time zone"));
        input.update(cx, |state, cx| state.focus(window, cx));
        self.clocks.picker = Some(PickerState {
            input,
            selected: 0,
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn close_clock_picker(&mut self, cx: &mut Context<Self>) {
        if self.clocks.picker.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn clear_clock_picker_search(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = &self.clocks.picker else {
            return;
        };
        let input = picker.input.clone();
        input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        if let Some(picker) = &mut self.clocks.picker {
            picker.selected = 0;
            picker.error = None;
        }
        cx.notify();
    }

    pub(crate) fn handle_picker_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = &self.clocks.picker else {
            return;
        };
        let query = picker.input.read(cx).value().to_string();
        let rows = clocks::picker_rows(&self.clocks.zones, &query);
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                self.close_clock_picker(cx);
            }
            "down" | "up" => {
                cx.stop_propagation();
                let from = self
                    .clocks
                    .picker
                    .as_ref()
                    .map(|picker| picker.selected)
                    .unwrap_or(0);
                let next = clocks::next_selectable(
                    &self.clocks.entries,
                    &rows,
                    from,
                    event.keystroke.key == "down",
                );
                if let Some(picker) = &mut self.clocks.picker {
                    picker.selected = next;
                }
                cx.notify();
            }
            "enter" => {
                cx.stop_propagation();
                let selected = self
                    .clocks
                    .picker
                    .as_ref()
                    .map(|picker| picker.selected)
                    .unwrap_or(0);
                if let Some(zone) = rows.get(selected).cloned()
                    && !clocks::zone_is_added(&self.clocks.entries, &zone)
                {
                    self.pick_zone(&zone, cx);
                }
            }
            _ => {}
        }
    }

    /// Add a zone and keep the picker open, matching the egui picker.
    pub(crate) fn pick_zone(&mut self, zone: &str, cx: &mut Context<Self>) {
        match self.store.add_clock(zone) {
            Ok(()) => {
                self.load_clocks();
                if let Some(picker) = &mut self.clocks.picker {
                    picker.selected = 0;
                    picker.error = None;
                }
            }
            Err(error) => {
                if let Some(picker) = &mut self.clocks.picker {
                    picker.error = Some(error);
                } else {
                    self.clocks.error = Some(error);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn clear_clock_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.clocks.search_input.clone() {
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
                state.focus(window, cx);
            });
        }
        self.clocks.search.clear();
        self.clocks.clear_drag();
        cx.notify();
    }

    pub(crate) fn start_clock_drag(&mut self, from: usize, clock_id: &str, cx: &mut Context<Self>) {
        let same = self
            .clocks
            .drag
            .as_ref()
            .is_some_and(|drag| drag.clock_id == clock_id);
        if same {
            return;
        }
        self.clocks.drag = Some(DragState {
            clock_id: clock_id.to_string(),
            from,
        });
        self.clocks.hover = Some(from);
        cx.notify();
    }

    /// Turn the pointer position into a static insertion slot. The geometry
    /// comes from the grid container's own bounds, so the preview reflow never
    /// moves the targets.
    pub(crate) fn hover_clock(&mut self, event: &DragMoveEvent<ClockDrag>, cx: &mut Context<Self>) {
        if self.clocks.drag.is_none() || !self.clocks.search.trim().is_empty() {
            return;
        }
        let grid = clocks::StaticGrid::new(
            f32::from(event.bounds.origin.x),
            f32::from(event.bounds.origin.y),
            f32::from(event.bounds.size.width),
            self.clocks.entries.len(),
        );
        let hover = grid.slot_at(
            f32::from(event.event.position.x),
            f32::from(event.event.position.y),
        );
        if hover != self.clocks.hover {
            self.clocks.hover = hover;
            cx.notify();
        }
    }

    pub(crate) fn drop_clock(&mut self, drag: &ClockDrag, cx: &mut Context<Self>) {
        let hover = self.clocks.hover;
        self.clocks.clear_drag();
        let Some(hover) = hover else {
            cx.notify();
            return;
        };
        if hover == drag.from {
            cx.notify();
            return;
        }
        let order = clocks::reordered(&self.clocks.order(), drag.from, hover);
        match self.store.reorder_clocks(&order) {
            Ok(()) => self.load_clocks(),
            Err(error) => self.clocks.error = Some(error),
        }
        cx.notify();
    }

    pub(crate) fn ask_remove_clock(&mut self, clock_id: &str, city: &str, cx: &mut Context<Self>) {
        self.clocks.confirm = Some(ConfirmState {
            kind: ConfirmKind::RemoveClock {
                clock_id: clock_id.to_string(),
            },
            title: format!("Remove {city}?"),
            message: "The city leaves your clocks. Trip history is kept.".to_string(),
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn open_inspector(
        &mut self,
        clock_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.clocks.entry(clock_id).is_none() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Add a person"));
        input.update(cx, |state, cx| state.focus(window, cx));
        self.clocks.inspector = Some(InspectorState {
            clock_id: clock_id.to_string(),
            add: input,
            rename: None,
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn close_inspector(&mut self, cx: &mut Context<Self>) {
        if self.clocks.inspector.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn handle_inspector_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(inspector) = &self.clocks.inspector else {
            return;
        };
        let renaming = inspector.rename.is_some();
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                if renaming {
                    self.cancel_person_rename(cx);
                } else {
                    self.close_inspector(cx);
                }
            }
            "enter" if renaming => {
                cx.stop_propagation();
                self.commit_person_rename(cx);
            }
            _ => {}
        }
    }

    pub(crate) fn add_person_from_inspector(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(inspector) = &self.clocks.inspector else {
            return;
        };
        let clock_id = inspector.clock_id.clone();
        let name = inspector.add.read(cx).value().trim().to_string();
        if name.is_empty() {
            if let Some(inspector) = &mut self.clocks.inspector {
                inspector.error = Some("A name is required".to_string());
            }
            cx.notify();
            return;
        }
        match self.store.add_person(&clock_id, &name) {
            Ok(()) => {
                self.load_clocks();
                if let Some(inspector) = &self.clocks.inspector {
                    let input = inspector.add.clone();
                    input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                        state.focus(window, cx);
                    });
                    if let Some(inspector) = &mut self.clocks.inspector {
                        inspector.error = None;
                    }
                }
            }
            Err(error) => {
                if let Some(inspector) = &mut self.clocks.inspector {
                    inspector.error = Some(error);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn begin_person_rename(
        &mut self,
        person_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(person) = self.clock_person(person_id).cloned() else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(person.name));
        input.update(cx, |state, cx| state.focus(window, cx));
        if let Some(inspector) = &mut self.clocks.inspector {
            inspector.rename = Some((person_id.to_string(), input));
        }
        cx.notify();
    }

    fn commit_person_rename(&mut self, cx: &mut Context<Self>) {
        let Some(inspector) = &self.clocks.inspector else {
            return;
        };
        let Some((person_id, input)) = &inspector.rename else {
            return;
        };
        let person_id = person_id.clone();
        let name = input.read(cx).value().trim().to_string();
        if name.is_empty() {
            if let Some(inspector) = &mut self.clocks.inspector {
                inspector.error = Some("A name is required".to_string());
            }
            cx.notify();
            return;
        }
        match self.store.rename_person(&person_id, &name) {
            Ok(()) => {
                if let Some(inspector) = &mut self.clocks.inspector {
                    inspector.rename = None;
                    inspector.error = None;
                }
                self.load_clocks();
            }
            Err(error) => {
                if let Some(inspector) = &mut self.clocks.inspector {
                    inspector.error = Some(error);
                }
            }
        }
        cx.notify();
    }

    fn cancel_person_rename(&mut self, cx: &mut Context<Self>) {
        if let Some(inspector) = &mut self.clocks.inspector {
            inspector.rename = None;
        }
        cx.notify();
    }

    pub(crate) fn ask_remove_person(&mut self, person_id: &str, cx: &mut Context<Self>) {
        let Some(person) = self.clock_person(person_id) else {
            return;
        };
        let name = person.name.clone();
        self.clocks.confirm = Some(ConfirmState {
            kind: ConfirmKind::RemovePerson {
                person_id: person_id.to_string(),
            },
            title: format!("Remove {name}?"),
            message: "Their trips are removed with them.".to_string(),
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn ask_remove_travel(&mut self, travel_id: &str, cx: &mut Context<Self>) {
        self.clocks.confirm = Some(ConfirmState {
            kind: ConfirmKind::RemoveTravel {
                travel_id: travel_id.to_string(),
            },
            title: "Remove this trip?".to_string(),
            message: "The trip is deleted from the history.".to_string(),
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn cancel_clock_confirm(&mut self, cx: &mut Context<Self>) {
        if self.clocks.confirm.take().is_some() {
            cx.notify();
        }
    }

    /// Run the confirmed removal. A core rejection (a city with residents, for
    /// example) keeps the dialog open and shows the exact message.
    pub(crate) fn confirm_clock_action(&mut self, cx: &mut Context<Self>) {
        let Some(confirm) = self.clocks.confirm.clone() else {
            return;
        };
        let result = match &confirm.kind {
            ConfirmKind::RemoveClock { clock_id } => self.store.remove_clock(clock_id),
            ConfirmKind::RemovePerson { person_id } => self.store.remove_person(person_id),
            ConfirmKind::RemoveTravel { travel_id } => self.store.remove_travel(travel_id),
        };
        match result {
            Ok(()) => {
                self.clocks.confirm = None;
                if matches!(confirm.kind, ConfirmKind::RemoveClock { .. }) {
                    // The inspector (if it was open) belongs to the removed city.
                    self.clocks.inspector = None;
                }
                self.load_clocks();
            }
            Err(error) => {
                if let Some(confirm) = &mut self.clocks.confirm {
                    confirm.error = Some(error);
                }
            }
        }
        cx.notify();
    }

    fn form_error(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.clocks.form {
            form.error = Some(message.into());
        }
        cx.notify();
    }

    fn finish_clock_form(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        match result {
            Ok(()) => {
                self.clocks.form = None;
                self.load_clocks();
            }
            Err(error) => {
                if let Some(form) = &mut self.clocks.form {
                    form.error = Some(error);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn open_travel_form(
        &mut self,
        person_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(person) = self.clock_person(person_id).cloned() else {
            return;
        };
        let Some(home) = self.clocks.entry(&person.home_clock_id) else {
            return;
        };
        let home_city = home.city.clone();
        let current_city = self
            .clocks
            .city_people(&person.home_clock_id)
            .residents
            .into_iter()
            .find(|status| status.person.id == person.id)
            .and_then(|status| status.away_to);
        let today = jiff::Zoned::now().date().to_string();
        let start = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(today.clone())
                .placeholder("YYYY-MM-DD")
        });
        let end = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(today)
                .placeholder("YYYY-MM-DD")
        });
        start.update(cx, |state, cx| state.focus(window, cx));
        self.clocks.form = Some(FormState {
            kind: FormKind::Assign,
            travel_id: None,
            person_id: person.id,
            person_name: person.name,
            home_clock_id: person.home_clock_id,
            home_city,
            current_city,
            destination: None,
            start,
            end,
            open_ended: false,
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn open_extend_form(
        &mut self,
        travel_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(travel) = self.clock_travel(travel_id).cloned() else {
            return;
        };
        let Some(person) = self.clock_person(&travel.person_id).cloned() else {
            return;
        };
        let Some(home) = self.clocks.entry(&person.home_clock_id) else {
            return;
        };
        let zone = travel
            .destination_clock_id
            .as_deref()
            .and_then(|id| self.clocks.entry(id))
            .and_then(|entry| entry.zone.clone());
        let start_date = zone
            .clone()
            .and_then(|zone| {
                Timestamp::from_second(travel.starts_at)
                    .ok()
                    .map(|instant| instant.to_zoned(zone).date())
            })
            .unwrap_or_else(|| jiff::Zoned::now().date());
        let end_date = travel
            .ends_at
            .and_then(|seconds| {
                let zone = zone.clone()?;
                Timestamp::from_second(seconds)
                    .ok()
                    .map(|instant| instant.to_zoned(zone).date())
            })
            .unwrap_or_else(|| start_date.tomorrow().unwrap_or(start_date));
        let start = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(start_date.to_string())
                .placeholder("YYYY-MM-DD")
        });
        let end = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(end_date.to_string())
                .placeholder("YYYY-MM-DD")
        });
        end.update(cx, |state, cx| state.focus(window, cx));
        self.clocks.form = Some(FormState {
            kind: FormKind::Extend,
            travel_id: Some(travel.id.clone()),
            person_id: person.id,
            person_name: person.name,
            home_clock_id: person.home_clock_id,
            home_city: home.city.clone(),
            current_city: travel
                .destination_clock_id
                .as_deref()
                .and_then(|id| self.clocks.entry(id))
                .map(|entry| entry.city.clone()),
            destination: travel.destination_clock_id.clone(),
            start,
            end,
            open_ended: travel.ends_at.is_none(),
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn open_relocate_form(
        &mut self,
        person_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(person) = self.clock_person(person_id).cloned() else {
            return;
        };
        let Some(home) = self.clocks.entry(&person.home_clock_id) else {
            return;
        };
        let today = jiff::Zoned::now().date().to_string();
        let start = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(today.clone())
                .placeholder("YYYY-MM-DD")
        });
        let end = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(today)
                .placeholder("YYYY-MM-DD")
        });
        self.clocks.form = Some(FormState {
            kind: FormKind::Relocate,
            travel_id: None,
            person_id: person.id,
            person_name: person.name,
            home_clock_id: person.home_clock_id,
            home_city: home.city.clone(),
            current_city: None,
            destination: None,
            start,
            end,
            open_ended: false,
            error: None,
        });
        cx.notify();
    }

    pub(crate) fn close_clock_form(&mut self, cx: &mut Context<Self>) {
        if self.clocks.form.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn set_form_destination(&mut self, clock_id: &str, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.clocks.form {
            form.destination = Some(clock_id.to_string());
            form.error = None;
        }
        cx.notify();
    }

    pub(crate) fn toggle_form_open_ended(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.clocks.form {
            form.open_ended = !form.open_ended;
        }
        cx.notify();
    }

    pub(crate) fn handle_form_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.close_clock_form(cx);
        }
    }

    /// Write the form. Trip windows are whole days in the destination zone.
    pub(crate) fn submit_clock_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &self.clocks.form else {
            return;
        };
        let kind = form.kind;
        let start_raw = form.start.read(cx).value().trim().to_string();
        let end_raw = form.end.read(cx).value().trim().to_string();
        let destination = form.destination.clone();
        let travel_id = form.travel_id.clone();
        let person_id = form.person_id.clone();
        let open_ended = form.open_ended;

        if kind == FormKind::Relocate {
            let Some(destination_id) = destination else {
                self.form_error("Pick the new home city", cx);
                return;
            };
            let result = self.store.relocate_person(&person_id, &destination_id);
            self.finish_clock_form(result, cx);
            return;
        }

        let Some(destination_id) = destination else {
            self.form_error("Pick a destination city", cx);
            return;
        };
        let Some(zone) = self
            .clocks
            .entry(&destination_id)
            .and_then(|entry| entry.zone.clone())
        else {
            self.form_error("That city has no time zone", cx);
            return;
        };
        let start = match start_raw.parse::<Date>() {
            Ok(date) => date,
            Err(_) => {
                self.form_error("The start date must be YYYY-MM-DD", cx);
                return;
            }
        };
        let ends_at = if open_ended {
            None
        } else {
            match end_raw.parse::<Date>() {
                Ok(date) => match day_end_seconds(date, &zone) {
                    Ok(seconds) => Some(seconds),
                    Err(error) => {
                        self.form_error(error.to_string(), cx);
                        return;
                    }
                },
                Err(_) => {
                    self.form_error("The end date must be YYYY-MM-DD", cx);
                    return;
                }
            }
        };
        let starts_at = match day_start_seconds(start, &zone) {
            Ok(seconds) => seconds,
            Err(error) => {
                self.form_error(error.to_string(), cx);
                return;
            }
        };
        let result = match kind {
            FormKind::Assign => {
                self.store
                    .start_travel(&person_id, &destination_id, starts_at, ends_at)
            }
            _ => match &travel_id {
                Some(id) => self.store.extend_travel(id, ends_at),
                None => Err("No trip to extend".to_string()),
            },
        };
        self.finish_clock_form(result, cx);
    }

    /// Finish the running trip now (`End now`, `Send home`).
    pub(crate) fn end_travel_now(&mut self, travel_id: &str, cx: &mut Context<Self>) {
        match self.store.end_travel(travel_id) {
            Ok(()) => self.load_clocks(),
            Err(error) => self.clocks.error = Some(error),
        }
        cx.notify();
    }

    pub(crate) fn send_home(&mut self, travel_id: &str, cx: &mut Context<Self>) {
        self.end_travel_now(travel_id, cx);
    }

    // -----------------------------------------------------------------------
    // Notes
    // -----------------------------------------------------------------------

    /// Resolve the notes root and rescan it. Tabs and the expanded set survive
    /// because they are keyed by path.
    fn load_notes(&mut self) {
        let root = match self.store.notes_root() {
            Ok(root) => root,
            Err(error) => {
                self.notes.root = None;
                self.notes.entries.clear();
                self.notes.error = Some(error.to_string());
                return;
            }
        };
        match self.store.notes_tree(&root) {
            Ok(entries) => {
                self.notes.root = Some(root);
                self.notes.entries = entries;
                self.notes.error = None;
            }
            Err(error) => {
                self.notes.root = Some(root);
                self.notes.entries.clear();
                self.notes.error = Some(error.to_string());
            }
        }
    }

    pub(crate) fn toggle_notes_tree(&mut self, cx: &mut Context<Self>) {
        self.notes.tree_open = !self.notes.tree_open;
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn toggle_notes_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.notes.expanded.remove(&path) {
            self.notes.expanded.insert(path.clone());
        }
        self.notes.selected = Some(path);
        cx.notify();
    }

    /// Open a note in a tab, reusing the tab when the path is already open.
    pub(crate) fn open_note(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.notes.selected = Some(path.clone());
        if let Some(index) = self.notes.tab_index(&path) {
            self.notes.active = Some(index);
            let editor = self.notes.tabs[index].editor.clone();
            editor.update(cx, |state, cx| state.focus(window, cx));
            cx.notify();
            return;
        }
        let Some(root) = self.notes.root.clone() else {
            self.notes.error = Some("The notes folder is unavailable".into());
            cx.notify();
            return;
        };
        match self.store.read_note(&path, &root) {
            Ok(text) => {
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .default_value(text.clone())
                        .placeholder("Start writing… Markdown is supported")
                        .language("markdown")
                });
                let mut session = crate::vim::VimSession::new();
                session.set_enabled(self.vim_mode, &editor, cx);
                cx.observe(&editor, |this: &mut AppShell, editor, cx| {
                    this.note_edited(&editor, cx)
                })
                .detach();
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.notes.tabs.push(NoteTab {
                    path,
                    name,
                    editor: editor.clone(),
                    vim: session,
                    preview: true,
                    saved: text,
                    dirty: false,
                    generation: 0,
                });
                self.notes.active = Some(self.notes.tabs.len() - 1);
                self.notes.error = None;
                if self.notes.tabs.last().is_some_and(|tab| !tab.preview) {
                    editor.update(cx, |state, cx| state.focus(window, cx));
                }
            }
            Err(error) => self.notes.error = Some(error.to_string()),
        }
        cx.notify();
    }

    pub(crate) fn activate_note_tab(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.notes.tab_index(&path) else {
            return;
        };
        self.notes.active = Some(index);
        let editor = self.notes.tabs[index].editor.clone();
        if !self.notes.tabs[index].preview {
            editor.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    pub(crate) fn close_note_tab_by_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.save_note(&path, cx);
        let Some(index) = self.notes.tab_index(&path) else {
            return;
        };
        self.notes.tabs.remove(index);
        self.notes.active = match (self.notes.tabs.is_empty(), self.notes.active) {
            (true, _) => None,
            (false, Some(active)) if active > index => Some(active - 1),
            (false, Some(active)) if active == index => Some(index.min(self.notes.tabs.len() - 1)),
            (false, other) => other,
        };
        cx.notify();
    }

    /// A textarea notification. Only a value that differs from the last write
    /// marks the tab dirty and schedules the debounce.
    fn note_edited(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        let Some(index) = self.notes.tabs.iter().position(|tab| tab.editor == *editor) else {
            return;
        };
        let value = editor.read(cx).value().to_string();
        let tab = &mut self.notes.tabs[index];
        if value == tab.saved {
            if tab.dirty {
                tab.dirty = false;
                cx.notify();
            }
            return;
        }
        tab.dirty = true;
        tab.generation = tab.generation.wrapping_add(1);
        let generation = tab.generation;
        let path = tab.path.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(notes::AUTOSAVE).await;
            this.update(cx, |this, cx| this.autosave_note(&path, generation, cx))
                .ok();
        })
        .detach();
    }

    /// A debounce that already lost its race (a newer edit bumped the
    /// generation) bows out.
    fn autosave_note(&mut self, path: &Path, generation: u64, cx: &mut Context<Self>) {
        let Some(index) = self.notes.tab_index(path) else {
            return;
        };
        if self.notes.tabs[index].generation != generation {
            return;
        }
        self.save_note(path, cx);
    }

    /// Write a dirty tab through core's atomic write.
    fn save_note(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(index) = self.notes.tab_index(path) else {
            return;
        };
        if !self.notes.tabs[index].dirty {
            return;
        }
        let Some(root) = self.notes.root.clone() else {
            return;
        };
        let value = self.notes.tabs[index].editor.read(cx).value().to_string();
        match self.store.write_note(path, &root, &value) {
            Ok(()) => {
                let tab = &mut self.notes.tabs[index];
                tab.saved = value;
                tab.dirty = false;
                self.notes.error = None;
            }
            Err(error) => self.notes.error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn save_active_note(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.notes.active_tab().map(|tab| tab.path.clone()) else {
            return;
        };
        self.save_note(&path, cx);
    }

    fn close_active_note(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.notes.active_tab().map(|tab| tab.path.clone()) else {
            return;
        };
        self.close_note_tab_by_path(path, cx);
    }

    /// Save the open note when the section is left.
    fn flush_active_note(&mut self, cx: &mut Context<Self>) {
        self.save_active_note(cx);
    }

    pub(crate) fn toggle_note_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.notes.active else {
            return;
        };
        let Some(tab) = self.notes.tabs.get_mut(index) else {
            return;
        };
        tab.preview = !tab.preview;
        let editor = tab.editor.clone();
        editor.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// Start the inline name field for a new note or folder.
    pub(crate) fn begin_note_draft(
        &mut self,
        kind: DraftKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.notes.draft.is_some() {
            return;
        }
        let Some(root) = self.notes.root.clone() else {
            self.notes.error = Some("The notes folder is unavailable".into());
            cx.notify();
            return;
        };
        let target = self.notes.selected_dir(&root);
        let placeholder = match kind {
            DraftKind::Note => "Note name",
            DraftKind::Folder => "Folder name",
            DraftKind::Rename => "New name",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |state, cx| state.focus(window, cx));
        self.notes.draft = Some(NameDraft {
            kind,
            target,
            input,
        });
        self.notes.error = None;
        cx.notify();
    }

    pub(crate) fn begin_note_rename(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        input.update(cx, |state, cx| state.focus(window, cx));
        self.notes.draft = Some(NameDraft {
            kind: DraftKind::Rename,
            target: path,
            input,
        });
        self.notes.error = None;
        cx.notify();
    }

    pub(crate) fn handle_note_draft_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                self.cancel_note_draft(cx);
            }
            "enter" => {
                cx.stop_propagation();
                self.commit_note_draft(window, cx);
            }
            _ => {}
        }
    }

    fn commit_note_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &self.notes.draft else {
            return;
        };
        let kind = draft.kind;
        let target = draft.target.clone();
        let name = draft.input.read(cx).value().trim().to_string();
        let Some(root) = self.notes.root.clone() else {
            self.notes.error = Some("The notes folder is unavailable".into());
            cx.notify();
            return;
        };
        if name.is_empty() {
            self.notes.error = Some("A name is required".into());
            cx.notify();
            return;
        }
        let result = match kind {
            DraftKind::Note => self.store.create_note(&root, &target, &name),
            DraftKind::Folder => self.store.create_folder(&root, &target, &name),
            DraftKind::Rename => self.store.rename_note(&target, &root, &name),
        };
        match result {
            Ok(created) => {
                if kind == DraftKind::Rename {
                    self.remap_note_paths(&target, &created);
                } else {
                    self.notes.expanded.insert(target);
                    if kind == DraftKind::Note {
                        self.open_note(created.clone(), window, cx);
                    }
                }
                self.notes.selected = Some(created);
                self.notes.draft = None;
                self.notes.error = None;
                self.load_notes();
            }
            Err(error) => self.notes.error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// Follow a rename (file or folder) through open tabs, the selection and
    /// the expanded set.
    fn remap_note_paths(&mut self, from: &Path, to: &Path) {
        for tab in &mut self.notes.tabs {
            if let Some(path) = remap_path(&tab.path, from, to) {
                tab.path = path;
                tab.name = tab
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
            }
        }
        if let Some(selected) = &self.notes.selected
            && let Some(path) = remap_path(selected, from, to)
        {
            self.notes.selected = Some(path);
        }
        let expanded: Vec<PathBuf> = self
            .notes
            .expanded
            .iter()
            .filter_map(|path| remap_path(path, from, to))
            .collect();
        self.notes.expanded = expanded.into_iter().collect();
    }

    fn cancel_note_draft(&mut self, cx: &mut Context<Self>) {
        if self.notes.draft.take().is_some() {
            self.notes.error = None;
            cx.notify();
        }
    }

    pub(crate) fn ask_delete_note(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.notes.confirm = Some(path);
        cx.notify();
    }

    pub(crate) fn cancel_delete_note(&mut self, cx: &mut Context<Self>) {
        if self.notes.confirm.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn confirm_delete_note(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(root) = self.notes.root.clone() else {
            return;
        };
        match self.store.delete_note(&path, &root) {
            Ok(()) => {
                let active_path = self.notes.active_tab().map(|tab| tab.path.clone());
                self.notes.tabs.retain(|tab| !tab.path.starts_with(&path));
                self.notes.active = active_path
                    .and_then(|path| self.notes.tabs.iter().position(|tab| tab.path == path));
                if self.notes.active.is_none() && !self.notes.tabs.is_empty() {
                    self.notes.active = Some(self.notes.tabs.len() - 1);
                }
                if self
                    .notes
                    .selected
                    .as_deref()
                    .is_some_and(|selected| selected.starts_with(&path))
                {
                    self.notes.selected = None;
                }
                self.notes.confirm = None;
                self.notes.error = None;
                self.load_notes();
            }
            Err(error) => self.notes.error = Some(error.to_string()),
        }
        cx.notify();
    }

    /// Keys routed to the active note before the textarea sees them.
    pub(crate) fn handle_notes_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.notes.active else {
            return;
        };
        let Some(tab) = self.notes.tabs.get(index) else {
            return;
        };
        if tab.preview {
            return;
        }
        let editor = tab.editor.clone();
        if !editor.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        let outcome = self.notes.tabs[index]
            .vim
            .handle_key(event, &editor, window, cx);
        if outcome.handled {
            cx.stop_propagation();
            cx.notify();
        }
        match outcome.ex {
            Some(vim::ExCommand::Write) => self.save_active_note(cx),
            Some(vim::ExCommand::Close) => self.close_active_note(cx),
            None => {
                if event.keystroke.key == "escape" && !outcome.handled {
                    cx.stop_propagation();
                    self.close_active_note(cx);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Documents
    // -----------------------------------------------------------------------

    /// Where the open-tab session lives: next to the database, so a
    /// `KANDRIUM_DB` override isolates it automatically.
    fn documents_session_path(&self) -> PathBuf {
        self.store
            .path
            .parent()
            .map(|parent| parent.join("gpui-documents.session"))
            .unwrap_or_else(|| PathBuf::from("gpui-documents.session"))
    }

    /// Resolve the documents root, rescan it and read the session file once.
    fn load_documents(&mut self) {
        let root = match self.store.documents_root() {
            Ok(root) => root,
            Err(error) => {
                self.documents.root = None;
                self.documents.entries.clear();
                self.documents.error = Some(error);
                return;
            }
        };
        match self.store.documents_tree(&root) {
            Ok(entries) => {
                self.documents.root = Some(root.clone());
                self.documents.entries = entries;
                self.documents.error = None;
            }
            Err(error) => {
                self.documents.root = Some(root.clone());
                self.documents.entries.clear();
                self.documents.error = Some(error);
            }
        }
        if !self.documents.session_loaded {
            self.documents.session_loaded = true;
            if let Ok(text) = std::fs::read_to_string(self.documents_session_path()) {
                self.documents.restore = Some(documents::parse_session(&text));
            }
        }
    }

    /// Restore the tabs once a window exists. Missing files are dropped.
    fn restore_documents_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((tree_open, active, rels)) = self.documents.restore.take() else {
            return;
        };
        self.documents.tree_open = tree_open;
        let Some(root) = self.documents.root.clone() else {
            return;
        };
        for rel in rels {
            let path = documents::abs_path(&root, &rel);
            if path.is_file() {
                self.open_document(path, window, cx);
            }
        }
        if let Some(index) = active {
            self.documents.active = if self.documents.tabs.is_empty() {
                None
            } else {
                Some(index.min(self.documents.tabs.len() - 1))
            };
        }
        cx.notify();
    }

    /// Persist the tab list as root-relative paths plus the tree flag.
    fn save_documents_session(&self) {
        let Some(root) = self.documents.root.clone() else {
            return;
        };
        let rels: Vec<String> = self
            .documents
            .tabs
            .iter()
            .filter_map(|tab| documents::rel_path(&root, &tab.path))
            .collect();
        let text =
            documents::format_session(self.documents.tree_open, self.documents.active, &rels);
        let _ = kandrium_core::fsutil::write_atomic(&self.documents_session_path(), text);
    }

    pub(crate) fn toggle_documents_tree(&mut self, cx: &mut Context<Self>) {
        self.documents.tree_open = !self.documents.tree_open;
        self.save_documents_session();
        self.persist_settings();
        cx.notify();
    }

    pub(crate) fn toggle_doc_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.documents.expanded.remove(&path) {
            self.documents.expanded.insert(path.clone());
        }
        self.documents.selected = Some(path);
        cx.notify();
    }

    /// Open a document in a tab, reusing the tab when the path is already open.
    pub(crate) fn open_document(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.documents.selected = Some(path.clone());
        if let Some(index) = self.documents.tab_index(&path) {
            self.documents.active = Some(index);
            if let Some(editor) = self.documents.tabs[index].editor.clone() {
                editor.update(cx, |state, cx| state.focus(window, cx));
            }
            cx.notify();
            return;
        }
        let Some(root) = self.documents.root.clone() else {
            self.documents.error = Some("The documents folder is unavailable".into());
            cx.notify();
            return;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        if kandrium_core::documents::is_pdf(&path) {
            match self.store.load_annotations(&path, &root) {
                Ok(doc) => {
                    let (worker, events) = PdfWorker::open(path.clone(), || {});
                    self.documents.tabs.push(DocTab::pdf(
                        path,
                        name,
                        worker,
                        events,
                        doc.annotations,
                    ));
                    self.documents.active = Some(self.documents.tabs.len() - 1);
                    self.documents.error = None;
                }
                Err(error) => {
                    self.documents.error = Some(error);
                    cx.notify();
                    return;
                }
            }
        } else if kandrium_core::documents::is_markdown(&path) {
            match self.store.read_document(&path, &root) {
                Ok(text) => {
                    let editor = cx.new(|cx| {
                        EditorState::new(window, cx)
                            .default_value(text.clone())
                            .placeholder("Start writing… Markdown is supported")
                            .language("markdown")
                    });
                    cx.observe(&editor, |this: &mut AppShell, editor, cx| {
                        this.doc_tab_edited(&editor, cx)
                    })
                    .detach();
                    self.documents
                        .tabs
                        .push(DocTab::markdown(path, name, text, editor.clone()));
                    let index = self.documents.tabs.len() - 1;
                    if let Some(vim) = self.documents.tabs[index].vim.as_mut() {
                        vim.set_enabled(self.vim_mode, &editor, cx);
                    }
                    self.documents.active = Some(index);
                    self.documents.error = None;
                    editor.update(cx, |state, cx| state.focus(window, cx));
                }
                Err(error) => self.documents.error = Some(error),
            }
        } else {
            self.documents.error = Some("Unsupported document type".into());
        }
        self.save_documents_session();
        cx.notify();
    }

    pub(crate) fn activate_doc_tab(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.documents.tab_index(&path) else {
            return;
        };
        self.documents.active = Some(index);
        if let Some(editor) = self.documents.tabs[index].editor.clone() {
            editor.update(cx, |state, cx| state.focus(window, cx));
        }
        self.save_documents_session();
        cx.notify();
    }

    pub(crate) fn close_doc_tab_by_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.save_document_tab(&path, cx);
        let Some(index) = self.documents.tab_index(&path) else {
            return;
        };
        // A failed save keeps the tab (and its dirty buffer) open so the user
        // can retry instead of losing the edit.
        if self.documents.tabs[index].has_unsaved() {
            cx.notify();
            return;
        }
        self.documents.tabs.remove(index);
        self.documents.active = match (self.documents.tabs.is_empty(), self.documents.active) {
            (true, _) => None,
            (false, Some(active)) if active > index => Some(active - 1),
            (false, Some(active)) if active == index => {
                Some(index.min(self.documents.tabs.len() - 1))
            }
            (false, other) => other,
        };
        self.save_documents_session();
        cx.notify();
    }

    pub(crate) fn toggle_doc_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.documents.active else {
            return;
        };
        let Some(tab) = self.documents.tabs.get_mut(index) else {
            return;
        };
        tab.preview = !tab.preview;
        let editor = tab.editor.clone();
        if let Some(editor) = editor {
            editor.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    pub(crate) fn begin_doc_draft(
        &mut self,
        kind: DocDraftKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.documents.root.clone() else {
            self.documents.error = Some("The documents folder is unavailable".into());
            cx.notify();
            return;
        };
        let target = self.documents.selected_dir(&root);
        self.begin_doc_draft_in(kind, target, window, cx);
    }

    pub(crate) fn begin_doc_draft_in(
        &mut self,
        kind: DocDraftKind,
        target: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.documents.draft.is_some() {
            return;
        }
        let placeholder = match kind {
            DocDraftKind::File => "name.md",
            DocDraftKind::Folder => "Folder name",
            DocDraftKind::Rename => "New name",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |state, cx| state.focus(window, cx));
        cx.subscribe_in(
            &input,
            window,
            |this: &mut AppShell, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.commit_doc_draft(window, cx);
                }
            },
        )
        .detach();
        self.documents.draft = Some(DocDraft {
            kind,
            target,
            input,
        });
        self.documents.error = None;
        cx.notify();
    }

    pub(crate) fn begin_doc_rename(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name.clone()));
        let end = name.len();
        input.update(cx, |state, cx| {
            state.focus(window, cx);
            // Renaming starts with the whole name selected, so typing replaces
            // it and Enter on an untouched field keeps the current name.
            state.set_selected_range(0..end, cx);
        });
        cx.subscribe_in(
            &input,
            window,
            |this: &mut AppShell, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.commit_doc_draft(window, cx);
                }
            },
        )
        .detach();
        self.documents.draft = Some(DocDraft {
            kind: DocDraftKind::Rename,
            target: path,
            input,
        });
        self.documents.error = None;
        cx.notify();
    }

    pub(crate) fn handle_doc_draft_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                self.cancel_doc_draft(cx);
            }
            "enter" => {
                cx.stop_propagation();
                self.commit_doc_draft(window, cx);
            }
            _ => {}
        }
    }

    fn commit_doc_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &self.documents.draft else {
            return;
        };
        let kind = draft.kind;
        let target = draft.target.clone();
        let name = draft.input.read(cx).value().trim().to_string();
        let Some(root) = self.documents.root.clone() else {
            self.documents.error = Some("The documents folder is unavailable".into());
            cx.notify();
            return;
        };
        if name.is_empty() {
            self.documents.error = Some("A name is required".into());
            cx.notify();
            return;
        }
        let result = match kind {
            DocDraftKind::File => self.store.create_markdown(&target, &root, &name),
            DocDraftKind::Folder => self.store.create_doc_folder(&target, &root, &name),
            DocDraftKind::Rename => self.store.rename_document(&target, &root, &name),
        };
        match result {
            Ok(created) => {
                if kind == DocDraftKind::Rename {
                    self.remap_doc_paths(&target, &created);
                } else {
                    self.documents.expanded.insert(target);
                    if kind == DocDraftKind::File {
                        self.open_document(created.clone(), window, cx);
                    }
                }
                self.documents.selected = Some(created);
                self.documents.draft = None;
                self.documents.error = None;
                self.load_documents();
                self.save_documents_session();
            }
            Err(error) => self.documents.error = Some(error),
        }
        cx.notify();
    }

    fn cancel_doc_draft(&mut self, cx: &mut Context<Self>) {
        if self.documents.draft.take().is_some() {
            self.documents.error = None;
            cx.notify();
        }
    }

    /// Follow a rename (file or folder) through open tabs, the selection and
    /// the expanded set.
    fn remap_doc_paths(&mut self, from: &Path, to: &Path) {
        for tab in &mut self.documents.tabs {
            if let Some(path) = remap_path(&tab.path, from, to) {
                tab.path = path;
                tab.name = tab
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
            }
        }
        if let Some(selected) = &self.documents.selected
            && let Some(path) = remap_path(selected, from, to)
        {
            self.documents.selected = Some(path);
        }
        let expanded: Vec<PathBuf> = self
            .documents
            .expanded
            .iter()
            .filter_map(|path| remap_path(path, from, to))
            .collect();
        self.documents.expanded = expanded.into_iter().collect();
    }

    pub(crate) fn ask_delete_document(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.documents.confirm = Some(path);
        cx.notify();
    }

    pub(crate) fn cancel_delete_document(&mut self, cx: &mut Context<Self>) {
        if self.documents.confirm.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn confirm_delete_document(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(root) = self.documents.root.clone() else {
            return;
        };
        match self.store.delete_document(&path, &root) {
            Ok(()) => {
                let active_path = self.documents.active_tab().map(|tab| tab.path.clone());
                // Drop the tabs without flushing: the backing file is gone, so
                // a save would only leave an orphan sidecar behind.
                self.documents
                    .tabs
                    .retain(|tab| !tab.path.starts_with(&path));
                self.documents.active = active_path
                    .and_then(|path| self.documents.tabs.iter().position(|tab| tab.path == path));
                if self.documents.active.is_none() && !self.documents.tabs.is_empty() {
                    self.documents.active = Some(self.documents.tabs.len() - 1);
                }
                if self
                    .documents
                    .selected
                    .as_deref()
                    .is_some_and(|selected| selected.starts_with(&path))
                {
                    self.documents.selected = None;
                }
                self.documents.confirm = None;
                self.documents.error = None;
                self.load_documents();
                self.save_documents_session();
            }
            Err(error) => self.documents.error = Some(error),
        }
        cx.notify();
    }

    /// Native multi-select upload into the selected folder. Each file lands
    /// under the `-2` unique-name rule; failures are reported per file.
    pub(crate) fn pick_and_import_documents(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.documents.root.clone() else {
            self.documents.error = Some("The documents folder is unavailable".into());
            cx.notify();
            return;
        };
        let Some(files) = rfd::FileDialog::new()
            .add_filter("Documents", &["pdf", "md", "markdown"])
            .pick_files()
        else {
            return;
        };
        if files.is_empty() {
            return;
        }
        let dest = self.documents.selected_dir(&root);
        match self.store.import_documents(&files, &dest, &root) {
            Ok(results) => {
                let errors: Vec<String> = results
                    .iter()
                    .filter_map(|(source, error)| {
                        error
                            .as_ref()
                            .map(|message| format!("{}: {message}", source.display()))
                    })
                    .collect();
                self.documents.expanded.insert(dest.clone());
                self.documents.selected = Some(dest);
                self.load_documents();
                self.documents.error = if errors.is_empty() {
                    None
                } else {
                    Some(errors.join("\n"))
                };
            }
            Err(error) => self.documents.error = Some(error),
        }
        cx.notify();
    }

    /// Keys routed to the active Markdown tab before the textarea sees them.
    pub(crate) fn handle_documents_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.documents.active else {
            return;
        };
        let Some(tab) = self.documents.tabs.get(index) else {
            return;
        };
        if tab.preview || tab.kind != DocKind::Markdown {
            return;
        }
        let Some(editor) = tab.editor.clone() else {
            return;
        };
        if !editor.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        let outcome = {
            let Some(vim) = self.documents.tabs[index].vim.as_mut() else {
                return;
            };
            vim.handle_key(event, &editor, window, cx)
        };
        if outcome.handled {
            cx.stop_propagation();
            cx.notify();
        }
        match outcome.ex {
            Some(vim::ExCommand::Write) => self.save_active_document(cx),
            Some(vim::ExCommand::Close) => self.close_active_document(cx),
            None => {
                if event.keystroke.key == "escape" && !outcome.handled {
                    cx.stop_propagation();
                    self.close_active_document(cx);
                }
            }
        }
    }

    fn save_active_document(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.documents.active_tab().map(|tab| tab.path.clone()) else {
            return;
        };
        self.save_document_tab(&path, cx);
    }

    fn close_active_document(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.documents.active_tab().map(|tab| tab.path.clone()) else {
            return;
        };
        self.close_doc_tab_by_path(path, cx);
    }

    /// Save every tab whose buffer changed, then the session.
    fn flush_documents(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self
            .documents
            .tabs
            .iter()
            .map(|tab| tab.path.clone())
            .collect();
        for path in paths {
            self.save_document_tab(&path, cx);
        }
    }

    /// A textarea notification. Only a value that differs from the last write
    /// marks the tab dirty and schedules the debounce.
    fn doc_tab_edited(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        let Some(index) = self
            .documents
            .tabs
            .iter()
            .position(|tab| tab.editor.as_ref() == Some(editor))
        else {
            return;
        };
        let value = editor.read(cx).value().to_string();
        let tab = &mut self.documents.tabs[index];
        if value == tab.saved {
            if tab.dirty {
                tab.dirty = false;
                cx.notify();
            }
            return;
        }
        // Textarea notifications also fire for caret and selection moves; only
        // a changed buffer restarts the debounce, or an idle edit would never
        // settle into a write.
        if tab.dirty && value == tab.last_value {
            return;
        }
        tab.last_value = value;
        tab.dirty = true;
        tab.generation = tab.generation.wrapping_add(1);
        let generation = tab.generation;
        let path = tab.path.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(documents::AUTOSAVE).await;
            this.update(cx, |this, cx| this.autosave_document(&path, generation, cx))
                .ok();
        })
        .detach();
    }

    /// A debounce that already lost its race (a newer edit bumped the
    /// generation) bows out.
    fn autosave_document(&mut self, path: &Path, generation: u64, cx: &mut Context<Self>) {
        let Some(index) = self.documents.tab_index(path) else {
            return;
        };
        if self.documents.tabs[index].generation != generation {
            return;
        }
        self.save_document_tab(path, cx);
    }

    /// Write a dirty tab through core: Markdown text or the annotation
    /// sidecar. Errors keep the tab dirty so the edit is never dropped.
    fn save_document_tab(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(index) = self.documents.tab_index(path) else {
            return;
        };
        match self.documents.tabs[index].kind {
            DocKind::Markdown => {
                if !self.documents.tabs[index].dirty {
                    return;
                }
                let Some(root) = self.documents.root.clone() else {
                    return;
                };
                let Some(editor) = self.documents.tabs[index].editor.clone() else {
                    return;
                };
                let value = editor.read(cx).value().to_string();
                match self.store.write_document(path, &root, &value) {
                    Ok(()) => {
                        let tab = &mut self.documents.tabs[index];
                        tab.saved = value;
                        tab.dirty = false;
                        self.documents.error = None;
                    }
                    Err(error) => self.documents.error = Some(error),
                }
                cx.notify();
            }
            DocKind::Pdf => self.save_annotations_tab(path, cx),
        }
    }

    fn save_annotations_tab(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(index) = self.documents.tab_index(path) else {
            return;
        };
        if !self.documents.tabs[index].ann_dirty {
            return;
        }
        let Some(root) = self.documents.root.clone() else {
            return;
        };
        let doc = AnnotationDoc {
            annotations: self.documents.tabs[index].annotations.clone(),
            ..AnnotationDoc::default()
        };
        match self.store.save_annotations(path, &root, &doc) {
            Ok(()) => {
                self.documents.tabs[index].ann_dirty = false;
                self.documents.error = None;
            }
            Err(error) => self.documents.error = Some(error),
        }
        cx.notify();
    }

    /// Mark the annotation sidecar dirty and debounce its write.
    fn mark_annotations_dirty(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(index) = self.documents.tab_index(path) else {
            return;
        };
        let tab = &mut self.documents.tabs[index];
        tab.ann_dirty = true;
        tab.ann_generation = tab.ann_generation.wrapping_add(1);
        let generation = tab.ann_generation;
        let path = path.to_path_buf();
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(documents::ANNOTATION_AUTOSAVE)
                .await;
            this.update(cx, |this, cx| {
                this.autosave_annotations(&path, generation, cx)
            })
            .ok();
        })
        .detach();
    }

    fn autosave_annotations(&mut self, path: &Path, generation: u64, cx: &mut Context<Self>) {
        let Some(index) = self.documents.tab_index(path) else {
            return;
        };
        if self.documents.tabs[index].ann_generation != generation {
            return;
        }
        self.save_annotations_tab(path, cx);
    }

    fn open_doc_popover(
        &mut self,
        path: &Path,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.documents.tab_index(path) else {
            return;
        };
        let text = self.documents.tabs[index]
            .annotations
            .iter()
            .find(|ann| ann.id == id)
            .map(|ann| ann.text.clone())
            .unwrap_or_default();
        let editor = cx.new(|cx| TextareaState::new(window, cx).default_value(text));
        editor.update(cx, |state, cx| state.focus(window, cx));
        cx.observe(&editor, |this: &mut AppShell, editor, cx| {
            this.doc_popover_edited(&editor, cx)
        })
        .detach();
        let tab = &mut self.documents.tabs[index];
        tab.popover = Some(id.to_string());
        tab.popover_editor = Some(editor);
        cx.notify();
    }

    pub(crate) fn open_annotation(
        &mut self,
        path: PathBuf,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.documents.tab_index(&path) else {
            return;
        };
        if self.documents.tabs[index].popover.as_deref() == Some(id.as_str()) {
            self.close_doc_popover(path, cx);
            return;
        }
        self.open_doc_popover(&path, &id, window, cx);
    }

    pub(crate) fn close_doc_popover(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(index) = self.documents.tab_index(&path) else {
            return;
        };
        let tab = &mut self.documents.tabs[index];
        tab.popover = None;
        tab.popover_editor = None;
        cx.notify();
    }

    fn doc_popover_edited(&mut self, editor: &Entity<TextareaState>, cx: &mut Context<Self>) {
        let Some(index) = self
            .documents
            .tabs
            .iter()
            .position(|tab| tab.popover_editor.as_ref() == Some(editor))
        else {
            return;
        };
        let Some(id) = self.documents.tabs[index].popover.clone() else {
            return;
        };
        let value = editor.read(cx).value().to_string();
        let changed = match self.documents.tabs[index]
            .annotations
            .iter_mut()
            .find(|ann| ann.id == id)
        {
            Some(ann) if ann.text != value => {
                ann.text = value;
                ann.modified_at = now_unix();
                true
            }
            _ => false,
        };
        if changed {
            let path = self.documents.tabs[index].path.clone();
            self.mark_annotations_dirty(&path, cx);
        }
    }

    pub(crate) fn set_annotation_color(
        &mut self,
        path: PathBuf,
        id: String,
        color: String,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.documents.tab_index(&path) else {
            return;
        };
        let changed = match self.documents.tabs[index]
            .annotations
            .iter_mut()
            .find(|ann| ann.id == id)
        {
            Some(ann) if ann.color != color => {
                ann.color = color;
                ann.modified_at = now_unix();
                true
            }
            _ => false,
        };
        if changed {
            self.mark_annotations_dirty(&path, cx);
        }
    }

    pub(crate) fn delete_annotation(&mut self, path: PathBuf, id: String, cx: &mut Context<Self>) {
        let Some(index) = self.documents.tab_index(&path) else {
            return;
        };
        let before = self.documents.tabs[index].annotations.len();
        let tab = &mut self.documents.tabs[index];
        tab.annotations.retain(|ann| ann.id != id);
        let removed = tab.annotations.len() != before;
        if tab.popover.as_deref() == Some(id.as_str()) {
            tab.popover = None;
            tab.popover_editor = None;
        }
        if removed {
            self.mark_annotations_dirty(&path, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn zoom_doc_by(&mut self, path: PathBuf, delta: f32, cx: &mut Context<Self>) {
        let Some(index) = self.documents.tab_index(&path) else {
            return;
        };
        let factor = if delta >= 0.0 {
            documents::ZOOM_STEP
        } else {
            1.0 / documents::ZOOM_STEP
        };
        let zoom = documents_pdf::clamp_zoom(self.documents.tabs[index].zoom * factor);
        self.documents.tabs[index].zoom = zoom;
        cx.notify();
    }

    pub(crate) fn reset_doc_zoom(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(index) = self.documents.tab_index(&path) else {
            return;
        };
        self.documents.tabs[index].zoom = 1.0;
        cx.notify();
    }

    pub(crate) fn jump_doc_page(&mut self, path: PathBuf, delta: i32, cx: &mut Context<Self>) {
        let Some(index) = self.documents.tab_index(&path) else {
            return;
        };
        let count = self.documents.tabs[index].page_count;
        if count == 0 {
            return;
        }
        let visible = self.documents.tabs[index].visible_page;
        let target = (visible as i32 + delta).clamp(0, count as i32 - 1) as usize;
        self.documents.tabs[index].jump_to = Some(target);
        cx.notify();
    }

    pub(crate) fn set_doc_tool(&mut self, tool: documents::Tool, cx: &mut Context<Self>) {
        if self.documents.tool != tool {
            self.documents.tool = tool;
            cx.notify();
        }
    }

    pub(crate) fn doc_selection_down(
        &mut self,
        path: PathBuf,
        page: usize,
        origin: Option<Point<Pixels>>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(origin) = origin else {
            return;
        };
        let Some(index) = self.documents.tab_index(&path) else {
            return;
        };
        if !self.documents.tabs[index].page_texts.contains_key(&page) {
            return;
        }
        let scale = self.documents.tabs[index].last_scale;
        let start = documents::screen_to_page(origin, scale, position);
        match self.documents.tool {
            documents::Tool::Note => {
                let mut ann = new_annotation(
                    AnnotationKind::Note,
                    page,
                    Vec::new(),
                    documents::NOTE_COLORS[3],
                );
                ann.quads = vec![documents::note_anchor_quad(start)];
                let id = ann.id.clone();
                self.documents.tabs[index].annotations.push(ann);
                self.open_doc_popover(&path, &id, window, cx);
                self.mark_annotations_dirty(&path, cx);
            }
            _ => {
                self.documents.tabs[index].selection = Some(documents::Selection {
                    page,
                    start,
                    current: start,
                });
                cx.notify();
            }
        }
    }

    pub(crate) fn doc_selection_move(
        &mut self,
        path: PathBuf,
        page: usize,
        origin: Option<Point<Pixels>>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(origin) = origin else {
            return;
        };
        let Some(index) = self.documents.tab_index(&path) else {
            return;
        };
        let scale = self.documents.tabs[index].last_scale;
        if let Some(sel) = &mut self.documents.tabs[index].selection
            && sel.page == page
        {
            sel.current = documents::screen_to_page(origin, scale, position);
            cx.notify();
        }
    }

    pub(crate) fn doc_selection_up(
        &mut self,
        path: PathBuf,
        page: usize,
        origin: Option<Point<Pixels>>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(origin) = origin else {
            return;
        };
        let Some(index) = self.documents.tab_index(&path) else {
            return;
        };
        let scale = self.documents.tabs[index].last_scale;
        let Some(mut sel) = self.documents.tabs[index].selection.take() else {
            return;
        };
        if sel.page == page {
            sel.current = documents::screen_to_page(origin, scale, position);
        }
        if self.documents.tool != documents::Tool::Highlight || sel.page != page {
            cx.notify();
            return;
        }
        let rect = sel.rect();
        let (quads, memo) = self.documents.tabs[index]
            .page_texts
            .get(&page)
            .map(|text| (text.selection_quads(&rect), text.selection_text(&rect)))
            .unwrap_or_default();
        // Ignore tiny drags (plain clicks).
        if quads.is_empty() || ((rect[2] - rect[0]) < 2.0 && (rect[3] - rect[1]) < 2.0) {
            cx.notify();
            return;
        }
        let mut ann = new_annotation(
            AnnotationKind::Highlight,
            page,
            quads,
            documents::NOTE_COLORS[0],
        );
        ann.text = memo;
        let id = ann.id.clone();
        self.documents.tabs[index].annotations.push(ann);
        self.open_doc_popover(&path, &id, window, cx);
        self.mark_annotations_dirty(&path, cx);
    }

    /// Drain worker events for every open PDF tab; keeps painting while a
    /// parse or render is outstanding (gpui has no cross-thread wake).
    pub(crate) fn pump_documents(&mut self, cx: &mut Context<Self>) {
        let mut busy = false;
        for tab in &mut self.documents.tabs {
            if let Some(rx) = tab.events.take() {
                loop {
                    match rx.try_recv() {
                        Ok(event) => apply_document_event(tab, event),
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            tab.events = Some(rx);
                            break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                    }
                }
            }
            if tab.kind == DocKind::Pdf {
                if tab.page_count == 0 && tab.error.is_none() {
                    busy = true;
                }
                if !tab.pending.is_empty() {
                    busy = true;
                }
            }
        }
        if busy {
            cx.notify();
        }
    }

    fn titlebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.tokens;
        // The native titlebar is transparent and the content starts at the
        // window's top edge, so this row *is* the titlebar: the macOS traffic
        // lights float over the left inset (68 px) and the sections sit beside
        // them. `app_owns_titlebar_drag` hands the move to us, so ONLY these
        // two empty stretches drag the window (and zoom on double click).
        // Nothing attached to the section control or the clock may carry a
        // drag handler, or a press on a chip would move the window.
        div()
            .id("titlebar")
            .test_support()
            .w_full()
            .h(px(38.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(10.))
            .bg(t.titlebar)
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .id("titlebar-drag-left")
                    .test_support()
                    .h_full()
                    .w(px(68.))
                    .flex_shrink_0()
                    .bg(t.titlebar)
                    .on_mouse_down(MouseButton::Left, titlebar_drag),
            )
            .child(self.section_control(window, cx))
            .child(
                div()
                    .id("titlebar-drag-right")
                    .test_support()
                    .h_full()
                    .flex_1()
                    .bg(t.titlebar)
                    .on_mouse_down(MouseButton::Left, titlebar_drag),
            )
            .when_some(self.titlebar_clock(cx), |el, clock| el.child(clock))
    }

    /// The clock of the city picked for the titlebar, if any cities are saved.
    /// Resolution never writes settings: a deleted city falls back to the
    /// first saved one until the user picks again.
    fn resolved_titlebar_clock(&self) -> Option<&clocks::ClockEntry> {
        self.titlebar_clock
            .as_deref()
            .and_then(|id| self.clocks.entry(id))
            .or_else(|| self.clocks.entries.first())
    }

    /// `Wed 11 Nov 8:13 pm City` — one mono run in the titlebar. Mono keeps
    /// the numerals from shifting as the minute changes.
    fn titlebar_clock_label(&self, now: Timestamp, zone: Option<&jiff::tz::TimeZone>) -> String {
        zone.map(|zone| format_titlebar_clock(now, zone))
            .unwrap_or_else(|| "--:--".to_string())
    }

    /// Date, 12-hour time and city name in one run, in the mockup's 10 px mono
    /// ghost type; a click opens the city menu. Deliberately carries no drag
    /// handler: the titlebar row is app-owned.
    fn titlebar_clock(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.tokens;
        let entry = self.resolved_titlebar_clock()?;
        let label = format!(
            "{} {}",
            self.titlebar_clock_label(self.clocks.now, entry.zone.as_ref()),
            entry.city
        );
        let tokens = t;
        Some(
            div()
                .id("titlebar-clock")
                .test_support()
                .aria_label(label.clone())
                .h(px(26.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .rounded(px(5.))
                .cursor_pointer()
                .hover(move |style| style.bg(tokens.hover))
                .child(
                    div()
                        .id("titlebar-clock-text")
                        .test_support()
                        .text(Text::Micro)
                        .text_color(t.text_ghost)
                        .whitespace_nowrap()
                        .child(label),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                    this.titlebar_clock_menu = !this.titlebar_clock_menu;
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// The city list under the titlebar clock. Rendered as an overlay on
    /// `app-root` (like the card modal) so it paints above the section body;
    /// a transparent backdrop turns any outside press into a dismissal.
    fn titlebar_clock_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = self.tokens;
        let selected = self.resolved_titlebar_clock().map(|entry| entry.id.clone());
        let now = self.clocks.now;
        let use_24h = self.use_24h;
        let entries = &self.clocks.entries;
        let row_count = entries.len();
        if row_count == 0 {
            return None;
        }
        Some(
            div()
                .id("titlebar-clock-menu-layer")
                .absolute()
                .inset_0()
                .child(
                    div()
                        .id("titlebar-clock-backdrop")
                        .test_support()
                        .absolute()
                        .inset_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                this.titlebar_clock_menu = false;
                                cx.notify();
                            }),
                        ),
                )
                .child(
                    div()
                        .id("titlebar-clock-menu")
                        .test_support()
                        .absolute()
                        .top(px(40.))
                        .right(px(8.))
                        .w(px(240.))
                        .p(px(4.))
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .rounded(px(t.radius_lg))
                        .bg(t.card)
                        .border_1()
                        .border_color(t.border)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .children(entries.iter().map(|entry| {
                            let id = entry.id.clone();
                            let is_selected = selected.as_deref() == Some(entry.id.as_str());
                            let meta = entry
                                .zone
                                .as_ref()
                                .map(|zone| format_clock_menu_meta(now, zone, use_24h))
                                .unwrap_or_else(|| "--".into());
                            let tokens = t;
                            div()
                                .id(format!("titlebar-clock-item-{}", entry.id))
                                .test_support()
                                .h(px(26.))
                                .px(px(8.))
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .rounded(px(4.))
                                .cursor_pointer()
                                .hover(move |style| style.bg(tokens.hover))
                                .child(
                                    div()
                                        .w(px(6.))
                                        .h(px(14.))
                                        .rounded_full()
                                        .bg(if is_selected { t.accent } else { t.titlebar }),
                                )
                                .child(
                                    div()
                                        .id(format!("titlebar-clock-city-{}", entry.id))
                                        .test_support()
                                        .flex_1()
                                        .text(Text::Micro)
                                        .text_color(if is_selected { t.text } else { t.text_muted })
                                        .truncate()
                                        .child(entry.city.clone()),
                                )
                                .child(
                                    div()
                                        .id(format!("titlebar-clock-meta-{}", entry.id))
                                        .test_support()
                                        .text(Text::Micro)
                                        .text_color(t.text_faint)
                                        .whitespace_nowrap()
                                        .child(meta),
                                )
                                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                    this.select_titlebar_clock(&id, cx)
                                }))
                        }))
                        .into_any_element(),
                )
                .into_any_element(),
        )
    }

    /// Point the titlebar at one saved city and persist the choice.
    pub(crate) fn select_titlebar_clock(&mut self, clock_id: &str, cx: &mut Context<Self>) {
        if self.clocks.entry(clock_id).is_none() {
            return;
        }
        self.titlebar_clock = Some(clock_id.to_string());
        self.titlebar_clock_menu = false;
        self.persist_settings();
        cx.notify();
    }

    /// The mockup's nav: text-only chips on the titlebar surface, 26 px tall,
    /// 5 px radius, 1 px apart. The active one sits on `surface2`; hover is a
    /// faint wash and full-strength text. Below 980 px the mockup tightens the
    /// horizontal padding to 8 px, which covers the 940 px minimum window.
    fn section_control(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.tokens;
        let pad = if window.bounds().size.width <= px(980.) {
            px(8.)
        } else {
            px(10.)
        };
        div()
            .id("section-control")
            .test_support()
            .flex()
            .items_center()
            .gap(px(1.))
            .children(Section::ALL.into_iter().map(|section| {
                let selected = section == self.section;
                div()
                    .id(format!("nav-{}", section.id()))
                    .test_support()
                    .flex()
                    .items_center()
                    .h(px(26.))
                    .px(pad)
                    .rounded(px(5.))
                    .cursor_pointer()
                    .text(Text::Button)
                    .text_color(if selected { t.text } else { t.text_muted })
                    .when(selected, |el| el.bg(t.surface_2))
                    .hover(move |style| style.bg(t.hover).text_color(t.text))
                    .child(section.label())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select(section, window, cx);
                    }))
            }))
    }

    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.tokens;
        match self.section {
            Section::Tasks => self.tasks_body(window, cx),
            Section::Clocks => clocks::clocks_view(&mut self.clocks, self.use_24h, &t, window, cx)
                .into_any_element(),
            Section::Notes => notes::notes_view(&self.notes, &t, window, cx).into_any_element(),
            Section::Documents => {
                self.pump_documents(cx);
                self.restore_documents_session(window, cx);
                let element = documents::documents_view(&mut self.documents, &t, window, cx);
                if self.documents.needs_repaint {
                    cx.notify();
                }
                element.into_any_element()
            }
            Section::Projects => {
                self.ensure_project_filter(window, cx);
                self.ensure_project_editor(window, cx);
                self.sync_project_board();
                self.prepare_column_scrolls("");
                self.column_edge_scroll(window);
                projects::projects_view(
                    &self.projects,
                    self.board
                        .as_ref()
                        .filter(|_| self.project_board_shown.is_some()),
                    tasks::BoardDrag {
                        target: self.drop_target.as_ref(),
                        previous: self.previous_target.as_ref(),
                        dragging: self.dragging_card.as_deref(),
                        gap_height: self.drag_gap_height,
                        metrics: &self.drag_metrics,
                        settled: self
                            .settled_card
                            .as_ref()
                            .map(|(id, token)| (id.as_str(), *token)),
                        editing: self
                            .title_edit
                            .as_ref()
                            .map(|edit| (edit.card_id.as_str(), &edit.input)),
                        hovered: self.hovered_card.as_deref(),
                        scrolls: &self.column_scrolls,
                        pointer: self.drag_pointer.clone(),
                    },
                    &t,
                    window,
                    cx,
                )
                .into_any_element()
            }
        }
    }

    fn tasks_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.tokens;
        let search = self.ensure_board_search(window, cx);
        let query = search.read(cx).value().to_string();
        self.prepare_column_scrolls(&query);
        self.column_edge_scroll(window);
        if self.showing_archive {
            self.ensure_archive(window, cx);
            let tabs = tasks::task_tabs(&self.boards, None);
            let state = self.archive.as_ref().expect("archive state");
            let cards = self.archive_cards.as_deref().unwrap_or(&[]);
            let page = archive::archive_view(
                cards,
                &self.boards,
                state,
                &self.archive_filter_anchor,
                &t,
                cx,
            );
            let error = self.archive_error.clone();
            return div()
                .id("tasks-body")
                .size_full()
                .flex()
                .flex_col()
                .child(tasks::tabs_strip(
                    &tabs,
                    None,
                    0,
                    true,
                    tasks::Search {
                        input: &search,
                        query: "",
                    },
                    &t,
                    cx,
                ))
                .when_some(error, |el, error| {
                    el.child(
                        div()
                            .px(px(16.))
                            .py(px(6.))
                            .text(Text::Body)
                            .text_color(t.danger)
                            .child(error),
                    )
                })
                .child(div().flex_1().min_h_0().child(page))
                .into_any_element();
        }
        if let Some(error) = &self.board_error {
            return placeholder::message(
                "tasks-error",
                Icon::FolderKanban,
                "Can't read the board",
                error.clone(),
                &t,
            )
            .into_any_element();
        }
        let Some(board) = &self.board else {
            return placeholder::message(
                "tasks-empty",
                Icon::FolderKanban,
                "No boards yet",
                "The Tasks section will render the first board as soon as the database has one.",
                &t,
            )
            .into_any_element();
        };
        // The active board is kept as a tab even when `list_boards` hides it
        // (project boards): the active-project-board rule.
        let active = Board {
            id: board.id.clone(),
            title: board.title.clone(),
        };
        let tabs = tasks::task_tabs(&self.boards, Some(&active));
        let active_cards: usize = board
            .columns
            .iter()
            .map(|column| column.ordered_cards().len())
            .sum();
        // "+ Add card" belongs to real boards only: the synthetic All board has
        // no column to write to, and a project board creates cards in its modal.
        let allow_add = self.boards.iter().any(|candidate| candidate.id == board.id);
        let add_form = self
            .add_card
            .as_ref()
            .map(|form| (form.column_id.as_str(), &form.input));
        div()
            .id("tasks-body")
            .size_full()
            .flex()
            .flex_col()
            .child(tasks::tabs_strip(
                &tabs,
                self.active_board_id.as_deref(),
                active_cards,
                false,
                tasks::Search {
                    input: &search,
                    query: &query,
                },
                &t,
                cx,
            ))
            .child(
                div().flex_1().min_h_0().child(tasks::board_view(
                    board,
                    tasks::BoardDrag {
                        target: self.drop_target.as_ref(),
                        previous: self.previous_target.as_ref(),
                        dragging: self.dragging_card.as_deref(),
                        gap_height: self.drag_gap_height,
                        metrics: &self.drag_metrics,
                        settled: self
                            .settled_card
                            .as_ref()
                            .map(|(id, token)| (id.as_str(), *token)),
                        editing: self
                            .title_edit
                            .as_ref()
                            .map(|edit| (edit.card_id.as_str(), &edit.input)),
                        hovered: self.hovered_card.as_deref(),
                        scrolls: &self.column_scrolls,
                        pointer: self.drag_pointer.clone(),
                    },
                    tasks::AddCard {
                        allowed: allow_add,
                        form: add_form,
                    },
                    &query,
                    self.title_edit.as_ref().map(|edit| edit.card_id.as_str()),
                    &t,
                    cx,
                )),
            )
            .into_any_element()
    }
}

impl Default for AppShell {
    fn default() -> Self {
        Self::new()
    }
}

/// Rewrite a path that lives under `from` so it lives under `to` instead.
/// Returns `None` for unrelated paths.
fn remap_path(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(rest)
    })
}

/// Apply one PDF worker event to a tab. Page size comes from the first
/// render (`page 0` at scale 1), so later zoomed renders only fill the cache.
fn apply_document_event(tab: &mut DocTab, event: PdfEvent) {
    match event {
        PdfEvent::Opened { page_count } => {
            tab.page_count = page_count;
            tab.error = None;
        }
        PdfEvent::OpenFailed { message } => {
            tab.error = Some(message);
            tab.page_count = 0;
            tab.worker = None;
        }
        PdfEvent::Rendered {
            page,
            scale,
            width,
            height,
            mut pixels,
        } => {
            let key = (page, documents::permille(scale));
            tab.pending.remove(&key);
            if width == 0 || height == 0 {
                return;
            }
            if tab.page_size.is_none() && scale > 0.0 {
                tab.page_size = Some((width as f32 / scale, height as f32 / scale));
            }
            // gpui's RenderImage is BGRA on little-endian; the worker emits
            // straight RGBA8.
            for px in pixels.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
            let Some(buffer) = image::RgbaImage::from_raw(width, height, pixels) else {
                return;
            };
            let rendered = RenderImage::new(smallvec![image::Frame::new(buffer)]);
            tab.stamps.insert(key, tab.next_stamp);
            tab.next_stamp = tab.next_stamp.wrapping_add(1);
            tab.images.insert(key, std::sync::Arc::new(rendered));
            while tab.images.len() > documents::TEXTURE_CACHE_CAP {
                let Some(oldest) = tab
                    .stamps
                    .iter()
                    .min_by_key(|(_, stamp)| **stamp)
                    .map(|(key, _)| *key)
                else {
                    break;
                };
                tab.images.remove(&oldest);
                tab.stamps.remove(&oldest);
            }
        }
        PdfEvent::Text { page, text } => {
            tab.page_texts.insert(page, text);
        }
        PdfEvent::Failed { page, message } => {
            // The failure does not carry a scale, so drop every in-flight
            // request for the page: the spinner must stop and a retry must not
            // be mistaken for a render that is already running.
            tab.pending
                .retain(|(pending_page, _)| *pending_page != page);
            if tab.error.is_none() {
                tab.error = Some(message);
            }
        }
    }
}

/// Unix seconds, matching the sidecar timestamps the egui app writes.
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

impl AppShell {
    /// The archive board-filter menu, floated at the root over the table.
    fn archive_filter_layer(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.showing_archive {
            return None;
        }
        let state = self.archive.as_ref()?;
        if !state.filter_open {
            return None;
        }
        let anchor = (*self.archive_filter_anchor.borrow())?;
        let viewport = window.viewport_size();
        let panel = archive::filter_menu_panel(
            anchor,
            &self.boards,
            &state.board_filter,
            viewport.width,
            viewport.height,
            &self.tokens,
            cx,
        );
        let focus = self.archive_filter_focus.clone();
        Some(
            div()
                .id("archive-board-filter-layer")
                .absolute()
                .inset_0()
                .when_some(focus, |el, focus| el.track_focus(&focus))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.close_archive_filter(cx);
                    }
                }))
                .child(
                    div()
                        .id("archive-board-filter-catcher")
                        .test_support()
                        .absolute()
                        .inset_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                this.close_archive_filter(cx)
                            }),
                        ),
                )
                .child(panel)
                .into_any_element(),
        )
    }

    /// The card right-click menu: a press-catching backdrop plus the two
    /// entries, anchored at the clamped pointer position.
    fn card_menu_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.card_menu.as_ref()?;
        let t = self.tokens;
        let position = menu.position;
        let menu_card_id = menu.card_id.clone();
        let focus = self.card_menu_focus.clone();
        Some(
            div()
                .id("card-menu-layer")
                .absolute()
                .inset_0()
                .when_some(focus, |el, focus| el.track_focus(&focus))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.close_card_menu(cx);
                    }
                }))
                .child(
                    div()
                        .id("card-menu-backdrop")
                        .test_support()
                        .absolute()
                        .inset_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                this.close_card_menu(cx)
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                this.close_card_menu(cx)
                            }),
                        ),
                )
                .child(
                    div()
                        .id("card-menu")
                        .test_support()
                        .absolute()
                        .left(position.x)
                        .top(position.y)
                        .w(px(160.))
                        .p(px(4.))
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .rounded(px(t.radius_lg))
                        .bg(t.card)
                        .border_1()
                        .border_color(t.border)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(menu_item(
                            "card-menu-archive",
                            Icon::Archive,
                            "Archive",
                            t.text,
                            &t,
                            {
                                let card_id = menu_card_id.clone();
                                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                    this.archive_card(&card_id, cx)
                                })
                            },
                        ))
                        .child(menu_item(
                            "card-menu-delete",
                            Icon::Trash2,
                            "Delete",
                            t.danger,
                            &t,
                            {
                                let card_id = menu_card_id;
                                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                    this.request_delete_card(&card_id, cx)
                                })
                            },
                        )),
                )
                .into_any_element(),
        )
    }

    /// The confirmation dialog: a dim backdrop, Esc to cancel, and Cancel /
    /// Delete buttons.
    fn card_confirm_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let confirm = self.card_confirm.as_ref()?;
        let t = self.tokens;
        let title = confirm.title;
        let message = confirm.message;
        let focus = self.card_confirm_focus.clone();
        let panel = div()
            .id("card-confirm")
            .test_support()
            .w(px(360.))
            .p(px(18.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .rounded(px(t.radius_lg))
            .bg(t.card)
            .border_1()
            .border_color(t.border)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text(Text::Heading).text_color(t.text).child(title))
            .child(
                div()
                    .text(Text::Body)
                    .text_color(t.text_muted)
                    .child(message),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(dialog_button(
                        "card-confirm-cancel",
                        "Cancel",
                        t.text,
                        t.field,
                        cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.cancel_card_confirm(cx)
                        }),
                    ))
                    .child(dialog_button(
                        "card-confirm-delete",
                        "Delete",
                        t.accent_text,
                        t.danger,
                        cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.confirm_card_delete(cx)
                        }),
                    )),
            );
        Some(
            div()
                .id("card-confirm-layer")
                .absolute()
                .inset_0()
                .when_some(focus, |el, focus| el.track_focus(&focus))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.cancel_card_confirm(cx);
                    }
                }))
                .child(
                    div()
                        .id("card-confirm-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(rgba(0x00000059))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                this.cancel_card_confirm(cx)
                            }),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(panel),
                )
                .into_any_element(),
        )
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // One clock reading per frame; the tick only refreshes this cache. The
        // titlebar clock renders in every section, so the loop always runs.
        self.clocks.set_now(Timestamp::now());
        self.ensure_clock_tick(window, cx);
        // An external change that arrived mid-drag lands once the drop has
        // finished, so the board never rearranges under the pointer.
        if self.pending_sync
            && self.dragging_card.is_none()
            && self.drop_target.is_none()
            && !(self.section == Section::Projects && self.projects.is_busy())
        {
            self.pending_sync = false;
            self.apply_external_change(cx);
        }
        self.ensure_watcher(window, cx);
        let t = self.tokens;
        let overlay = self.editing.as_ref().map(|session| {
            card_modal::modal(
                ModalContext {
                    session,
                    area_boards: &self.boards,
                    projects: &self.project_choices,
                    tokens: &t,
                },
                cx,
            )
        });
        let clock_menu = if self.titlebar_clock_menu {
            self.titlebar_clock_menu(cx)
        } else {
            None
        };
        let flight = self.drag_flight_overlay(cx);
        // The confirmation takes focus so Esc reaches its layer.
        if self.card_confirm.is_some() && self.card_confirm_focus.is_none() {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            self.card_confirm_focus = Some(focus);
        }
        let card_menu = self.card_menu_layer(cx);
        let card_confirm = self.card_confirm_layer(cx);
        let archive_filter = self.archive_filter_layer(window, cx);
        div()
            .id("app-root")
            .relative()
            .size_full()
            .child(
                div()
                    .id("app-shell")
                    .test_support()
                    .size_full()
                    .flex()
                    .flex_col()
                    .bg(t.pasteboard)
                    .text(Text::Body)
                    .text_color(t.text)
                    .child(self.titlebar(window, cx))
                    .child(
                        div()
                            .id("section-body")
                            .test_support()
                            .flex_1()
                            .min_h_0()
                            .bg(t.pasteboard)
                            .child(self.body(window, cx)),
                    ),
            )
            .children(overlay)
            .children(clock_menu)
            .children(flight)
            .children(archive_filter)
            .children(card_menu)
            .children(card_confirm)
            // A press anywhere outside the inline title field commits it, the
            // same way egui's blur does; the field stops propagation for its
            // own presses.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                    this.commit_card_title_edit(cx)
                }),
            )
            // A release outside the board still ends the drag. Drop handlers
            // stop propagation, so this only runs when nothing accepted the
            // drop — the refused-drop snap-back.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| this.cancel_card_drag(window, cx)),
            )
    }
}

/// One row in a small context menu.
fn menu_item(
    id: &'static str,
    glyph: Icon,
    label: &'static str,
    color: gpui_kit::Rgba,
    t: &Tokens,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let tokens = *t;
    div()
        .id(id)
        .test_support()
        .h(px(26.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(4.))
        .cursor_pointer()
        .hover(move |style| style.bg(tokens.hover))
        .text(Text::Body)
        .text_color(color)
        .child(icon(glyph, 13., color))
        .child(label)
        .on_click(handler)
        .into_any_element()
}

/// One button in the delete confirmation.
fn dialog_button(
    id: &'static str,
    label: &'static str,
    color: gpui_kit::Rgba,
    background: gpui_kit::Rgba,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .h(px(28.))
        .px(px(12.))
        .flex()
        .items_center()
        .rounded(px(5.))
        .bg(background)
        .cursor_pointer()
        .text(Text::Button)
        .text_color(color)
        .child(label)
        .on_click(handler)
        .into_any_element()
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use gpui_kit::test::TestWindowExt;

    fn seeded_db(dir: &Path) -> PathBuf {
        let path = dir.join("kandrium.db");
        let mut db = kandrium_core::open_db(&path).expect("open temp db");
        kandrium_core::bootstrap(&mut db, &path).expect("bootstrap temp db");
        path
    }

    /// A stored file is applied at boot: section, vim preference and the tree
    /// widths/flags land on the state the views render from.
    #[test]
    fn with_store_applies_saved_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = seeded_db(dir.path());
        std::fs::write(
            dir.path().join(settings::FILE_NAME),
            "section=clocks\nvim_mode=0\nnotes_tree_width=310\n\
             documents_tree_open=0\nprojects_tree_width=200\n",
        )
        .unwrap();

        let shell = AppShell::with_store(Store::open_path(path));
        assert_eq!(shell.section(), Section::Clocks);
        assert!(!shell.vim_mode());
        assert_eq!(shell.notes.tree_w, 310.0);
        assert!(!shell.documents.tree_open);
        assert_eq!(shell.settings.tree_width(TreePanel::Projects), 200.0);
    }

    /// No file means the defaults egui ships: Tasks, vim on, both trees open at
    /// the design-system width.
    #[test]
    fn with_store_defaults_without_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = seeded_db(dir.path());
        let shell = AppShell::with_store(Store::open_path(path));
        assert_eq!(shell.section(), Section::Tasks);
        assert!(shell.vim_mode());
        assert_eq!(shell.notes.tree_w, settings::DEFAULT_TREE_W);
        assert!(shell.documents.tree_open);
    }

    fn first_card_id(shell: &AppShell) -> String {
        shell.board.as_ref().expect("board").columns[0].ordered_cards()[0]
            .id
            .clone()
    }

    /// A commit from another connection surfaces through the watcher refresh,
    /// and an open modal keeps its buffer across that refresh.
    #[gpui::test]
    fn external_change_refreshes_tasks_without_closing_the_modal(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = seeded_db(dir.path());
        cx.update(gpui_kit::init);
        let boot = path.clone();
        let (view, cx) =
            cx.add_window_view(move |_window, _cx| AppShell::with_store(Store::open_path(boot)));
        cx.update(|window, cx| window.render_frame(cx));

        let card_id = cx.update(|_window, app| first_card_id(view.read(app)));
        cx.update(|window, app| {
            view.update(app, |shell, cx| shell.open_card(&card_id, window, cx))
        });
        cx.update(|window, cx| window.render_frame(cx));

        let db = kandrium_core::open_db(&path).expect("second connection");
        kandrium_core::rename_card(&db, &card_id, "Renamed elsewhere").expect("external rename");

        cx.update(|_window, app| view.update(app, |shell, cx| shell.apply_external_change(cx)));
        cx.update(|window, cx| window.render_frame(cx));

        cx.update(|_window, app| {
            view.read_with(app, |shell, _app| {
                let board = shell.board.as_ref().expect("board");
                let card = board
                    .columns
                    .iter()
                    .flat_map(|column| column.ordered_cards())
                    .find(|card| card.id == card_id)
                    .expect("card");
                assert_eq!(card.title, "Renamed elsewhere", "the board must reload");
                assert!(
                    shell.editing.is_some(),
                    "an open modal must survive an external refresh"
                );
            })
        });
    }

    /// A change that arrives while a card is dragged is deferred, not lost.
    #[gpui::test]
    fn external_change_defers_during_a_drag(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = seeded_db(dir.path());
        cx.update(gpui_kit::init);
        let boot = path.clone();
        let (view, cx) =
            cx.add_window_view(move |_window, _cx| AppShell::with_store(Store::open_path(boot)));
        cx.update(|window, cx| window.render_frame(cx));

        let card_id = cx.update(|_window, app| first_card_id(view.read(app)));
        cx.update(|_window, app| {
            view.update(app, |shell, cx| {
                shell.begin_card_drag(&card_id, "col-test", None, Point::default(), cx)
            })
        });

        let db = kandrium_core::open_db(&path).expect("second connection");
        kandrium_core::rename_card(&db, &card_id, "Renamed elsewhere").expect("external rename");
        cx.update(|_window, app| view.update(app, |shell, cx| shell.apply_external_change(cx)));

        cx.update(|_window, app| {
            view.read_with(app, |shell, _app| {
                assert!(shell.pending_sync, "the reload must be deferred");
                let title = shell
                    .board
                    .as_ref()
                    .unwrap()
                    .columns
                    .iter()
                    .flat_map(|column| column.ordered_cards())
                    .find(|card| card.id == card_id)
                    .unwrap()
                    .title
                    .clone();
                assert_ne!(
                    title, "Renamed elsewhere",
                    "the board must not move mid-drag"
                );
            })
        });

        cx.update(|window, cx| window.render_frame(cx));
        cx.update(|_window, app| {
            view.read_with(app, |shell, _app| {
                assert!(shell.pending_sync, "still dragging, still deferred");
            })
        });
    }

    /// A failed render must clear every in-flight request for that page: the
    /// spinner stops and a retry is not mistaken for an operation already
    /// running. Requests for other pages stay untouched.
    #[test]
    fn pdf_failure_clears_the_failed_page_only() {
        let path = PathBuf::from("missing.pdf");
        let (worker, events) = pdf_render::PdfWorker::open(path.clone(), || {});
        let mut tab = DocTab::pdf(
            path.clone(),
            "missing.pdf".to_string(),
            worker,
            events,
            Vec::new(),
        );
        tab.pending.insert((3, 500));
        tab.pending.insert((3, 1000));
        tab.pending.insert((4, 1000));

        apply_document_event(
            &mut tab,
            pdf_render::Event::Failed {
                page: 3,
                message: "boom".to_string(),
            },
        );

        assert!(
            !tab.pending.contains(&(3, 500)) && !tab.pending.contains(&(3, 1000)),
            "both scales of the failed page must leave the in-flight set"
        );
        assert!(
            tab.pending.contains(&(4, 1000)),
            "another page's request must keep running"
        );
        assert_eq!(tab.pending.len(), 1);
        assert_eq!(tab.error.as_deref(), Some("boom"));
    }

    /// The titlebar clock uses one exact run: weekday, day, month, lowercase
    /// 12-hour time — no leading zeros, no seconds, no 24-hour form.
    #[test]
    fn titlebar_clock_format_matches_the_spec() {
        let utc = jiff::tz::TimeZone::get("UTC").expect("UTC zone");
        let noon: Timestamp = "2026-11-11T20:13:00Z".parse().unwrap();
        assert_eq!(format_titlebar_clock(noon, &utc), "Wed 11 Nov 8:13 pm");
        let midnight: Timestamp = "2026-01-05T00:05:00Z".parse().unwrap();
        assert_eq!(
            format_titlebar_clock(midnight, &utc),
            "Mon 5 Jan 12:05 am",
            "single-digit day and hour keep no leading zero"
        );
    }

    /// Menu rows run `weekday day month time`, with the time following the
    /// app's 12/24-hour preference.
    #[test]
    fn clock_menu_meta_matches_the_row_spec() {
        let utc = jiff::tz::TimeZone::get("UTC").expect("UTC zone");
        let afternoon: Timestamp = "2026-11-12T15:28:00Z".parse().unwrap();
        assert_eq!(
            format_clock_menu_meta(afternoon, &utc, false),
            "Thu 12 Nov 3:28 pm"
        );
        assert_eq!(
            format_clock_menu_meta(afternoon, &utc, true),
            "Thu 12 Nov 15:28"
        );
        let morning: Timestamp = "2026-11-12T03:28:00Z".parse().unwrap();
        assert_eq!(
            format_clock_menu_meta(morning, &utc, false),
            "Thu 12 Nov 3:28 am"
        );
    }

    /// The column and the source insertion point (`before`) of a card.
    fn drag_source(shell: &AppShell, card_id: &str) -> (String, Option<String>) {
        let board = shell.board.as_ref().expect("board");
        for column in &board.columns {
            let cards = column.ordered_cards();
            if let Some(index) = cards.iter().position(|card| card.id == card_id) {
                return (
                    column.id.clone(),
                    cards.get(index + 1).map(|card| card.id.clone()),
                );
            }
        }
        panic!("card {card_id} not found");
    }

    fn card_order(shell: &AppShell) -> Vec<String> {
        shell
            .board
            .as_ref()
            .expect("board")
            .columns
            .iter()
            .flat_map(|column| column.ordered_cards())
            .map(|card| card.id.clone())
            .collect()
    }

    /// Pickup leaves the flow in exactly the same shape: the card disappears
    /// but a same-height gap takes its row, so nothing shifts.
    #[gpui::test]
    fn a_drag_opens_a_same_height_gap_at_the_source(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = seeded_db(dir.path());
        cx.update(gpui_kit::init);
        let boot = path.clone();
        let (view, cx) =
            cx.add_window_view(move |_window, _cx| AppShell::with_store(Store::open_path(boot)));
        cx.update(|window, cx| window.render_frame(cx));

        let card_id = cx.update(|_window, app| first_card_id(view.read(app)));
        let (column_id, before) = cx
            .update(|_window, app| view.read_with(app, |shell, _app| drag_source(shell, &card_id)));

        cx.update(|_window, app| {
            view.update(app, |shell, cx| {
                shell.begin_card_drag(&card_id, &column_id, before.clone(), Point::default(), cx);
                assert_eq!(shell.dragging_card.as_deref(), Some(card_id.as_str()));
                let target = shell.drop_target.as_ref().expect("source target");
                assert_eq!(target.column_id, column_id);
                assert_eq!(target.before, before);
                assert!(
                    shell.drag_gap_height > 0.0,
                    "the gap must take the card's measured row height"
                );
            })
        });
    }

    /// A cancelled drag keeps the card hidden and flies the ghost back to the
    /// source slot; the card only reappears when the flight lands.
    #[gpui::test]
    fn a_cancelled_drag_flies_back_before_the_card_returns(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = seeded_db(dir.path());
        cx.update(gpui_kit::init);
        let boot = path.clone();
        let (view, cx) =
            cx.add_window_view(move |_window, _cx| AppShell::with_store(Store::open_path(boot)));
        cx.update(|window, cx| window.render_frame(cx));

        let card_id = cx.update(|_window, app| first_card_id(view.read(app)));
        // Give the card a meta row, so the flight has something to lose: a
        // title-only overlay would drop the due date and dots for the length
        // of the snap-back.
        let title = cx.update(|_window, app| {
            view.read_with(app, |shell, _app| {
                shell
                    .card_snapshot(&card_id)
                    .expect("card on the board")
                    .title
            })
        });
        cx.update(|_window, app| {
            view.update(app, |shell, cx| {
                let body = CreateCardBody {
                    title,
                    due_date: Some("2026-01-02".into()),
                    labels: Some(vec![0]),
                    ..Default::default()
                };
                shell
                    .store
                    .save_card(&card_id, &body, None, None)
                    .expect("store the card's meta");
                shell.reload_displayed_board();
                cx.notify();
            })
        });
        let (column_id, before) = cx
            .update(|_window, app| view.read_with(app, |shell, _app| drag_source(shell, &card_id)));
        cx.update(|_window, app| {
            view.update(app, |shell, cx| {
                shell.begin_card_drag(&card_id, &column_id, before.clone(), Point::default(), cx);
            })
        });
        cx.update(|window, app| {
            view.update(app, |shell, cx| shell.cancel_card_drag(window, cx));
        });

        let token = cx.update(|_window, app| {
            view.read_with(app, |shell, _app| {
                assert!(
                    shell.dragging_card.is_some(),
                    "the card stays hidden until touchdown"
                );
                let flight = shell.drag_flight.as_ref().expect("snap-back flight");
                let target = shell
                    .drop_target
                    .as_ref()
                    .expect("gap parked at the origin");
                assert_eq!(target.column_id, column_id);
                // The overlay paints the card it came from, meta row included.
                assert_eq!(flight.card.id, card_id);
                assert_eq!(flight.card.due_date, "2026-01-02");
                assert_eq!(flight.card.labels, vec![0]);
                flight.token
            })
        });
        // The next frame really does paint the flying card: the real row is
        // hidden while the flight is in the air, so this overlay is the only
        // place the meta row can come from.
        cx.update(|window, cx| window.render_frame(cx));
        cx.update(|window, _cx| {
            assert!(
                window.try_find("drag-flight").is_some(),
                "the flying card stays on screen during the snap-back"
            );
        });
        cx.update(|_window, app| view.update(app, |shell, cx| shell.finish_drag_flight(token, cx)));
        cx.update(|_window, app| {
            view.read_with(app, |shell, _app| {
                assert!(shell.dragging_card.is_none());
                assert!(shell.drag_flight.is_none());
                assert_eq!(
                    shell.settled_card.as_ref().map(|(id, _)| id.as_str()),
                    Some(card_id.as_str()),
                    "the landed card gets the settle fade token"
                );
            })
        });
    }

    /// A drop on the card's own slot is a cancel: no write, no reorder.
    #[gpui::test]
    fn a_drop_on_its_own_slot_does_not_move_the_card(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = seeded_db(dir.path());
        cx.update(gpui_kit::init);
        let boot = path.clone();
        let (view, cx) =
            cx.add_window_view(move |_window, _cx| AppShell::with_store(Store::open_path(boot)));
        cx.update(|window, cx| window.render_frame(cx));

        let card_id = cx.update(|_window, app| first_card_id(view.read(app)));
        let (column_id, before) = cx
            .update(|_window, app| view.read_with(app, |shell, _app| drag_source(shell, &card_id)));
        let order = cx.update(|_window, app| view.read_with(app, |shell, _app| card_order(shell)));

        cx.update(|_window, app| {
            view.update(app, |shell, cx| {
                shell.begin_card_drag(&card_id, &column_id, before.clone(), Point::default(), cx);
            })
        });
        cx.update(|window, app| {
            view.update(app, |shell, cx| {
                shell.drop_card(&card_id, &column_id, window, cx)
            });
        });

        let token = cx.update(|_window, app| {
            view.read_with(app, |shell, _app| {
                assert_eq!(card_order(shell), order, "the card must not move");
                shell.drag_flight.as_ref().map(|flight| flight.token)
            })
        });
        if let Some(token) = token {
            cx.update(|_window, app| {
                view.update(app, |shell, cx| shell.finish_drag_flight(token, cx))
            });
        }
        cx.update(|_window, app| {
            view.read_with(app, |shell, _app| {
                assert!(shell.dragging_card.is_none());
                assert!(shell.drop_target.is_none());
            })
        });
    }

    /// Reduced motion teleports: the drag ends at once and no flight exists.
    #[gpui::test]
    fn reduced_motion_cancels_without_a_flight(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = seeded_db(dir.path());
        cx.update(gpui_kit::init);
        let boot = path.clone();
        let (view, cx) =
            cx.add_window_view(move |_window, _cx| AppShell::with_store(Store::open_path(boot)));
        cx.update(|window, cx| window.render_frame(cx));
        cx.update(|_window, app| app.set_reduce_motion(true));

        let card_id = cx.update(|_window, app| first_card_id(view.read(app)));
        let (column_id, before) = cx
            .update(|_window, app| view.read_with(app, |shell, _app| drag_source(shell, &card_id)));
        cx.update(|_window, app| {
            view.update(app, |shell, cx| {
                shell.begin_card_drag(&card_id, &column_id, before.clone(), Point::default(), cx);
            })
        });
        cx.update(|window, app| {
            view.update(app, |shell, cx| shell.cancel_card_drag(window, cx));
        });
        cx.update(|_window, app| {
            view.read_with(app, |shell, _app| {
                assert!(shell.dragging_card.is_none());
                assert!(shell.drag_flight.is_none(), "reduced motion must not fly");
                assert!(shell.drop_target.is_none());
            })
        });
    }
}
