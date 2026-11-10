//! The application shell: a title bar with the five-section segmented control
//! and one body per section.
//!
//! Stage 1 of the migration: Tasks and Clocks read the real database
//! (read-only); Notes, Documents and Projects state honestly that they have not
//! been ported yet.

use gpui_kit::TestSupportExt;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, ClickEvent, Context, IntoElement, Render, Window, div, px};
use jiff::Timestamp;
use kandrium_core::BoardWithColumns;

use crate::assets::{Icon, icon};
use crate::store::{ClockRow, Store};
use crate::theme::{Text, TextStyled, Tokens};
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

/// Root view. Owns the read-only database handle and the data each section
/// renders.
pub struct AppShell {
    section: Section,
    tokens: Tokens,
    use_24h: bool,
    store: Store,
    board: Option<BoardWithColumns>,
    board_error: Option<String>,
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
            board: None,
            board_error: None,
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
                let keep = self.board.as_ref().map(|b| b.id.clone());
                let target = keep
                    .filter(|id| boards.iter().any(|b| &b.id == id))
                    .or_else(|| boards.first().map(|b| b.id.clone()));
                match target.map(|id| (id.clone(), self.store.board(&id))) {
                    Some((_, Ok(board))) => {
                        self.board = Some(board);
                        self.board_error = None;
                    }
                    Some((_, Err(e))) => {
                        self.board = None;
                        self.board_error = Some(e);
                    }
                    None => {
                        self.board = None;
                        self.board_error = None;
                    }
                }
            }
            Err(e) => {
                self.board = None;
                self.board_error = Some(e);
            }
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
            Section::Tasks => self.tasks_body(),
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

    fn tasks_body(&self) -> AnyElement {
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
        match &self.board {
            Some(board) => tasks::board_view(board, &t).into_any_element(),
            None => placeholder::message(
                "tasks-empty",
                Icon::FolderKanban,
                "No boards yet",
                "The Tasks section will render the first board as soon as the database has one.",
                &t,
            )
            .into_any_element(),
        }
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
            )
    }
}
