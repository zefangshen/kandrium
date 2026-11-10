//! The application shell: a title bar with the five-section segmented control
//! and one body per section.
//!
//! Stage 1 of the migration: Tasks and Clocks read the real database
//! (read-only); Notes, Documents and Projects state honestly that they have not
//! been ported yet.

use gpui_kit::TestSupportExt;
use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, ClickEvent, Context, IntoElement, Render, Window, div, px};
use jiff::Timestamp;
use kandrium_core::{
    ALL_BOARD_ID, Board, BoardWithColumns, CardStatus, CreateCardBody, ProjectChoice,
};

use crate::assets::{Icon, icon};
use crate::store::{ClockRow, Store};
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::card_modal::{self, EditSession, ModalContext};
use crate::views::tasks::DropTarget;
use crate::views::{clocks, placeholder, tasks};

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
    clock_rows: Vec<ClockRow>,
    clock_error: Option<String>,
    now: Timestamp,
}

impl AppShell {
    /// Open the default database and load the first screen.
    pub fn new() -> Self {
        Self::with_store(Store::open())
    }

    /// Build the shell over a specific store (the headless tests use this).
    pub fn with_store(store: Store) -> Self {
        let mut shell = Self {
            section: Section::Tasks,
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
            clock_rows: Vec::new(),
            clock_error: None,
            now: Timestamp::now(),
        };
        shell.load_tasks();
        shell.load_clocks();
        shell
    }

    /// Currently selected section.
    pub fn section(&self) -> Section {
        self.section
    }

    fn select(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        self.section = section;
        match section {
            Section::Tasks => self.load_tasks(),
            Section::Clocks => self.load_clocks(),
            Section::Notes | Section::Documents | Section::Projects => {}
        }
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
        let focus = cx.focus_handle();

        self.editing = Some(EditSession {
            card_id: card.id,
            is_project_card,
            title,
            description,
            due,
            focus,
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
    /// error, matching the egui modal.
    pub(crate) fn save_editing(&mut self, cx: &mut Context<Self>) {
        let Some(mut session) = self.editing.take() else {
            return;
        };
        let title = session.title.read(cx).value().trim().to_string();
        if title.is_empty() {
            session.error = Some("A title is required".into());
            self.editing = Some(session);
            cx.notify();
            return;
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
            }
        }
        cx.notify();
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

    fn load_clocks(&mut self) {
        self.now = Timestamp::now();
        match self.store.clocks(self.now, self.use_24h) {
            Ok(rows) => {
                self.clock_rows = rows;
                self.clock_error = None;
            }
            Err(e) => {
                self.clock_rows.clear();
                self.clock_error = Some(e);
            }
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

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = self.tokens;
        match self.section {
            Section::Tasks => self.tasks_body(cx),
            Section::Clocks => {
                if let Some(error) = &self.clock_error {
                    return placeholder::message(
                        "clocks-error",
                        Icon::Globe,
                        "Can't read the clocks",
                        error.clone(),
                        &t,
                    )
                    .into_any_element();
                }
                let toggle = cx.listener(|this, _: &ClickEvent, _window, cx| {
                    this.use_24h = !this.use_24h;
                    this.load_clocks();
                    cx.notify();
                });
                clocks::clocks_view(&self.clock_rows, self.use_24h, &t, toggle).into_any_element()
            }
            Section::Notes => placeholder::message(
                "notes-placeholder",
                Icon::StickyNote,
                "Notes",
                "Not ported yet. Notes will read your markdown files in a later stage of the \
                 migration; nothing here touches them.",
                &t,
            )
            .into_any_element(),
            Section::Documents => placeholder::message(
                "documents-placeholder",
                Icon::FileText,
                "Documents",
                "Not ported yet. PDFs and markdown documents land in a later stage; nothing here \
                 touches your documents folder.",
                &t,
            )
            .into_any_element(),
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

impl Render for AppShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                            .child(self.body(cx)),
                    ),
            )
            .children(overlay)
    }
}
