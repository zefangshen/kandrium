//! The application shell: a title bar with the five-section segmented control
//! and one body per section.
//!
//! Tasks, Clocks, Notes and Documents read and write the real data; Projects
//! is the last section still on the placeholder.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::TestSupportExt;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, DragMoveEvent, Entity, Focusable, IntoElement, KeyDownEvent,
    Pixels, Point, Render, RenderImage, Window, div, px,
};
use jiff::Timestamp;
use jiff::civil::Date;
use kandrium_core::documents::{AnnotationDoc, AnnotationKind, new_annotation};
use kandrium_core::{
    ALL_BOARD_ID, Board, BoardWithColumns, CardStatus, CreateCardBody, Person, ProjectChoice,
    Travel, day_end_seconds, day_start_seconds,
};
use pdf_render::{Event as PdfEvent, PdfWorker};
use smallvec::smallvec;

use crate::assets::{Icon, icon};
use crate::settings::{self, TreePanel};
use crate::store::Store;
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::card_modal::{self, EditSession, ModalContext};
use crate::views::clocks::{
    self, ClockDrag, ClockState, ConfirmKind, ConfirmState, DragState, FormKind, FormState,
    InspectorState, PickerState,
};
use crate::views::documents::{self, DocDraft, DocDraftKind, DocKind, DocTab, DocumentsState};
use crate::views::documents_pdf;
use crate::views::tasks::DropTarget;
use crate::views::{
    notes::{self, DraftKind, NameDraft, NoteTab, NotesState},
    placeholder, tasks,
};
use crate::vim;

/// The five top-level sections, in the same order as the egui app.
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
        Section::Clocks,
        Section::Notes,
        Section::Documents,
        Section::Projects,
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
            Section::Tasks => Icon::FolderKanban,
            Section::Clocks => Icon::Globe,
            Section::Notes => Icon::StickyNote,
            Section::Documents => Icon::FileText,
            Section::Projects => Icon::Folder,
        }
    }
}

impl std::fmt::Display for Section {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
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
    dragging_card: Option<String>,
    drop_generation: u64,
    editing: Option<EditSession>,
    project_choices: Vec<ProjectChoice>,
    pub(crate) clocks: ClockState,
    /// Bumped to retarget or stop the minute tick.
    clock_tick: u64,
    /// Generation of the tick loop currently allowed to run.
    clock_tick_running: Option<u64>,
    notes: NotesState,
    documents: DocumentsState,
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
        let section = settings.section();
        let vim_mode = settings.vim_mode();

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
            dragging_card: None,
            drop_generation: 0,
            editing: None,
            project_choices: Vec::new(),
            clocks: ClockState::new(),
            clock_tick: 0,
            clock_tick_running: None,
            notes,
            documents,
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
        match section {
            Section::Notes => shell.load_notes(),
            Section::Documents => shell.load_documents(),
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

    fn select(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        if self.section == Section::Notes {
            self.flush_active_note(cx);
        }
        if self.section == Section::Documents {
            self.flush_documents(cx);
            self.save_documents_session();
        }
        self.section = section;
        match section {
            Section::Tasks => self.load_tasks(),
            Section::Clocks => self.load_clocks(),
            Section::Notes => self.load_notes(),
            Section::Documents => self.load_documents(),
            Section::Projects => {}
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

    /// Switch the Tasks tab to `id`.
    pub(crate) fn open_board(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.active_board_id.as_deref() == Some(id) {
            return;
        }
        self.drop_target = None;
        if let Err(error) = self.load_board(id) {
            self.board_error = Some(error);
        }
        cx.notify();
    }

    /// Record where a drop would land. No-op when the target has not changed,
    /// so a drag does not repaint on every mouse move.
    pub(crate) fn begin_card_drag(&mut self, card_id: &str, cx: &mut Context<Self>) {
        if self.dragging_card.as_deref() != Some(card_id) {
            self.dragging_card = Some(card_id.to_string());
            cx.notify();
        }
    }

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
        self.drop_generation += 1;
        self.drop_target = Some(DropTarget {
            column_id: column_id.to_string(),
            before,
            generation: self.drop_generation,
        });
        cx.notify();
    }

    pub(crate) fn clear_drop_target(&mut self, cx: &mut Context<Self>) {
        let had_target = self.drop_target.take().is_some();
        let had_drag = self.dragging_card.take().is_some();
        if had_target || had_drag {
            cx.notify();
        }
    }

    /// Commit a drop. The target recorded by `on_drag_move` decides the
    /// insertion point; a drop on a column without one appends.
    pub(crate) fn drop_card(&mut self, card_id: &str, column_id: &str, cx: &mut Context<Self>) {
        let before = self
            .drop_target
            .clone()
            .filter(|target| target.column_id == column_id)
            .and_then(|target| target.before)
            .filter(|before| before != card_id);
        self.drop_target = None;
        self.dragging_card = None;
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
                if let Some(id) = self.active_board_id.clone()
                    && let Err(error) = self.load_board(&id)
                {
                    self.board_error = Some(error);
                }
            }
            Err(error) => self.board_error = Some(error),
        }
        cx.notify();
    }

    /// Open the card editor for `card_id`, resolving the card from the board
    /// currently on screen (mirrors included).
    pub(crate) fn open_card(&mut self, card_id: &str, window: &mut Window, cx: &mut Context<Self>) {
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
        let description =
            cx.new(|cx| TextareaState::new(window, cx).default_value(card.description.clone()));
        let due = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(card.due_date.clone())
                .placeholder("YYYY-MM-DD")
        });
        title.update(cx, |state, cx| state.focus(window, cx));
        // Vim starts in Normal mode: the description only accepts plain typing
        // after `i`, so it starts read-only unless the preference is off.
        let mut vim = crate::vim::VimSession::new();
        vim.set_enabled(self.vim_mode, &description, cx);
        let focus = cx.focus_handle();

        self.editing = Some(EditSession {
            card_id: card.id,
            is_project_card,
            title,
            description,
            due,
            focus,
            vim,
            labels: card.labels,
            project_id: card.project_id,
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

    /// Set the project tag of a normal card. An empty id clears it.
    pub(crate) fn set_editing_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        if let Some(session) = self.editing.as_mut() {
            session.project_id = (!project_id.is_empty()).then_some(project_id);
            cx.notify();
        }
    }

    /// Set the area board of a project card. An empty id unfiles it.
    pub(crate) fn set_editing_area(&mut self, area_id: String, cx: &mut Context<Self>) {
        if let Some(session) = self.editing.as_mut() {
            session.area_board_id = (!area_id.is_empty()).then_some(area_id);
            cx.notify();
        }
    }

    fn reload_active_board(&mut self) {
        if let Some(id) = self.active_board_id.clone()
            && let Err(error) = self.load_board(&id)
        {
            self.board_error = Some(error);
        }
    }

    // -----------------------------------------------------------------------
    // Clocks
    // -----------------------------------------------------------------------

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

    /// Keep one tick loop alive while Clocks is on screen. It waits for the
    /// nearer of the next minute and the nearest travel boundary, then
    /// refreshes `now` from the cached rows only.
    fn ensure_clock_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.clock_tick;
        if self.clock_tick_running == Some(generation) {
            return;
        }
        self.clock_tick_running = Some(generation);
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let wait = this.update_in(cx, |this, _window, cx| {
                    if this.clock_tick_running != Some(generation)
                        || this.section != Section::Clocks
                    {
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
        matches!(self.section, Section::Tasks | Section::Clocks)
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
        match self.section {
            Section::Tasks => self.load_tasks(),
            Section::Clocks => self.load_clocks(),
            Section::Notes | Section::Documents | Section::Projects => return,
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
                let editor =
                    cx.new(|cx| TextareaState::new(window, cx).default_value(text.clone()));
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
                    preview: false,
                    saved: text,
                    dirty: false,
                    generation: 0,
                });
                self.notes.active = Some(self.notes.tabs.len() - 1);
                self.notes.error = None;
                editor.update(cx, |state, cx| state.focus(window, cx));
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
        editor.update(cx, |state, cx| state.focus(window, cx));
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
    fn note_edited(&mut self, editor: &Entity<TextareaState>, cx: &mut Context<Self>) {
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
                    let editor =
                        cx.new(|cx| TextareaState::new(window, cx).default_value(text.clone()));
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
    fn doc_tab_edited(&mut self, editor: &Entity<TextareaState>, cx: &mut Context<Self>) {
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

    fn titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.tokens;
        div()
            .id("titlebar")
            .test_support()
            .w_full()
            .h(px(48.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(16.))
            .px(px(14.))
            .bg(t.titlebar)
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon(Icon::FolderKanban, 16., t.accent))
                    .child(
                        div()
                            .text(Text::Strong)
                            .text_color(t.text)
                            .child("Kandrium"),
                    ),
            )
            .child(self.section_control(cx))
            .child(div().flex_1())
            .child(
                div()
                    .text(Text::Small)
                    .text_color(t.text_faint)
                    .child("gpui preview"),
            )
    }

    fn section_control(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.tokens;
        div()
            .id("section-control")
            .test_support()
            .flex()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(t.radius))
            .bg(t.field)
            .border_1()
            .border_color(t.border)
            .children(Section::ALL.into_iter().map(|section| {
                let selected = section == self.section;
                div()
                    .id(format!("nav-{}", section.id()))
                    .test_support()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(26.))
                    .px(px(10.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text(Text::Button)
                    .text_color(if selected {
                        t.accent_text
                    } else {
                        t.text_muted
                    })
                    .when(selected, |el| el.bg(t.accent_soft))
                    .child(icon(
                        section.icon(),
                        14.,
                        if selected {
                            t.accent_text
                        } else {
                            t.text_muted
                        },
                    ))
                    .child(section.label())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.select(section, cx);
                    }))
            }))
    }

    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = self.tokens;
        match self.section {
            Section::Tasks => self.tasks_body(cx),
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
            Section::Projects => placeholder::message(
                "projects-placeholder",
                Icon::Folder,
                "Projects",
                "Not ported yet. The project tree, overview and documents land in a later stage.",
                &t,
            )
            .into_any_element(),
        }
    }

    fn tasks_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.tokens;
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
        div()
            .id("tasks-body")
            .size_full()
            .flex()
            .flex_col()
            .child(tasks::tabs_strip(
                &tabs,
                self.active_board_id.as_deref(),
                &t,
                cx,
            ))
            .child(div().flex_1().min_h_0().child(tasks::board_view(
                board,
                self.drop_target.as_ref(),
                self.dragging_card.as_deref(),
                &t,
                cx,
            )))
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
        PdfEvent::Failed { message, .. } => {
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

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // One clock reading per frame; the tick only refreshes this cache.
        if self.section == Section::Clocks {
            self.clocks.set_now(Timestamp::now());
            self.ensure_clock_tick(window, cx);
        } else if self.clock_tick_running.is_some() {
            self.invalidate_clock_tick();
        }
        // An external change that arrived mid-drag lands once the drop has
        // finished, so the board never rearranges under the pointer.
        if self.pending_sync && self.dragging_card.is_none() && self.drop_target.is_none() {
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
                    .child(self.titlebar(cx))
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
    }
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
        let (view, mut cx) =
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
        let (view, mut cx) =
            cx.add_window_view(move |_window, _cx| AppShell::with_store(Store::open_path(boot)));
        cx.update(|window, cx| window.render_frame(cx));

        let card_id = cx.update(|_window, app| first_card_id(view.read(app)));
        cx.update(|_window, app| view.update(app, |shell, cx| shell.begin_card_drag(&card_id, cx)));

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
}
