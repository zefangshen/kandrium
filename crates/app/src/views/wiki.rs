//! Wiki: the merged Notes and Documents section.
//!
//! The tree column splits vertically — notes above, documents below — with one
//! shared tab row and one detail column serving both kinds. Each half keeps its
//! own tree panel (header actions, inline drafts, context menus) from
//! [`super::notes`] and [`super::documents`]; this module owns the split, the
//! collapse flags, the merged tab order and the detail dispatcher.

use std::path::{Path, PathBuf};

use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, CursorStyle, DragMoveEvent, IntoElement, Render,
    TestSupportExt, Window, div, px,
};
use kandrium_core::{LintFinding, LintSeverity, WikiStatus};

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::settings::WikiHalf;
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::documents::{self, DocumentsState};
use crate::views::notes::{self, NotesState};
use crate::views::placeholder;

/// A half may not shrink below this while it is expanded.
pub const MIN_HALF_H: f32 = 120.0;
/// Half header height: the same 38 px row the tree panels use.
pub const HEADER_H: f32 = 38.0;
/// The wiki status toolbar above both halves.
pub const TOOLBAR_H: f32 = 34.0;
/// Height the 5 px vertical splitter occupies in the column layout: its
/// `-2 px` side margins overlap the two halves.
const SPLITTER_H: f32 = 1.0;
/// The titlebar the section body sits below, subtracted from the window height
/// when the split is converted to pixels.
const TITLEBAR_H: f32 = 38.0;

/// Which tree a merged tab belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    Note,
    Doc,
}

/// One tab in the merged row. Keyed by kind and path so it survives the
/// per-kind index invalidation a close causes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabKey {
    pub kind: TabKind,
    pub path: PathBuf,
}

impl TabKey {
    pub fn note(path: PathBuf) -> Self {
        Self {
            kind: TabKind::Note,
            path,
        }
    }

    pub fn document(path: PathBuf) -> Self {
        Self {
            kind: TabKind::Doc,
            path,
        }
    }

    /// `note` / `doc`, used in the merged tab's element id.
    pub fn kind_id(&self) -> &'static str {
        match self.kind {
            TabKind::Note => "note",
            TabKind::Doc => "doc",
        }
    }
}

/// Which non-tab surface the detail column shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WikiPanel {
    Status,
    Lint,
    Setup,
}

/// Wiki section state: both tree states plus the merged tab order.
pub struct WikiState {
    pub notes: NotesState,
    pub documents: DocumentsState,
    /// Display order of the merged tab row, oldest first.
    pub order: Vec<TabKey>,
    /// The tab the detail column renders.
    pub active: Option<TabKey>,
    /// Notes-half share of the tree split (0.15–0.85).
    pub split: f32,
    pub notes_collapsed: bool,
    pub docs_collapsed: bool,
    /// Status, lint, or setup replaces the tab row until a tab is opened.
    pub panel: Option<WikiPanel>,
    pub status: Option<WikiStatus>,
    pub findings: Vec<LintFinding>,
    pub busy: bool,
    pub panel_error: Option<String>,
}

impl Default for WikiState {
    fn default() -> Self {
        Self {
            notes: NotesState::new(),
            documents: DocumentsState::new(),
            order: Vec::new(),
            active: None,
            split: 0.5,
            notes_collapsed: false,
            docs_collapsed: false,
            panel: None,
            status: None,
            findings: Vec::new(),
            busy: false,
            panel_error: None,
        }
    }
}

impl WikiState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the tab's path is still open in its own kind's tab list.
    pub fn has(&self, key: &TabKey) -> bool {
        match key.kind {
            TabKind::Note => self.notes.tab_index(&key.path).is_some(),
            TabKind::Doc => self.documents.tab_index(&key.path).is_some(),
        }
    }

    /// Drop keys whose tabs closed and append newly opened tabs at the right
    /// end, so `order` stays the single authority for the merged row. Known
    /// keys keep their position, which preserves true open order across kinds.
    pub fn reconcile(&mut self) {
        let mut order: Vec<TabKey> = self
            .order
            .iter()
            .filter(|key| self.has(key))
            .cloned()
            .collect();
        for tab in &self.notes.tabs {
            let key = TabKey::note(tab.path.clone());
            if !order.contains(&key) {
                order.push(key);
            }
        }
        for tab in &self.documents.tabs {
            let key = TabKey::document(tab.path.clone());
            if !order.contains(&key) {
                order.push(key);
            }
        }
        self.order = order;
        if self.active.as_ref().is_none_or(|key| !self.has(key)) {
            self.active = self.default_active();
        }
    }

    /// The tab to show when nothing explicitly selected one: the last active
    /// tab of either kind, then the right end of the row.
    fn default_active(&self) -> Option<TabKey> {
        self.notes
            .active_tab()
            .map(|tab| TabKey::note(tab.path.clone()))
            .or_else(|| {
                self.documents
                    .active_tab()
                    .map(|tab| TabKey::document(tab.path.clone()))
            })
            .or_else(|| self.order.last().cloned())
    }

    pub fn set_active(&mut self, key: TabKey) {
        self.active = Some(key);
        self.panel = None;
    }

    /// Forget a closed tab and fall back to its right neighbour, else its left.
    pub fn forget(&mut self, key: &TabKey) {
        let Some(index) = self.order.iter().position(|known| known == key) else {
            return;
        };
        self.order.remove(index);
        if self.active.as_ref() == Some(key) {
            self.active = self.order.get(index).cloned().or_else(|| {
                index
                    .checked_sub(1)
                    .and_then(|left| self.order.get(left))
                    .cloned()
            });
        }
    }

    /// Follow a rename through the merged row.
    pub fn remap_paths(&mut self, from: &Path, to: &Path) {
        for key in &mut self.order {
            if let Some(path) = crate::app::remap_path(&key.path, from, to) {
                key.path = path;
            }
        }
        if let Some(active) = &mut self.active
            && let Some(path) = crate::app::remap_path(&active.path, from, to)
        {
            active.path = path;
        }
    }

    /// Notes-half share after the geometry clamp for the live height.
    pub fn notes_height(&self, available: f32) -> f32 {
        if self.notes_collapsed {
            return HEADER_H;
        }
        if self.docs_collapsed {
            return (available - HEADER_H).max(HEADER_H);
        }
        let min = MIN_HALF_H.min(available / 2.0);
        let max = (available - MIN_HALF_H).max(min);
        (available * self.split).clamp(min, max)
    }
}

/// Render the whole section.
pub fn wiki_view(
    state: &mut WikiState,
    t: &Tokens,
    window: &mut Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    state.reconcile();
    state.documents.needs_repaint = false;
    let mut row = div()
        .id("wiki-body")
        .test_support()
        .flex_1()
        .min_h_0()
        .flex()
        .child(tree_column(state, t, window, cx));

    if state.notes.tree_open {
        row = row.child(tree_splitter(t));
    }
    row = row
        .on_drag_move::<WikiTreeDrag>(cx.listener(
            |this, event: &DragMoveEvent<WikiTreeDrag>, _window, cx| {
                let width = f32::from(event.event.position.x) - f32::from(event.bounds.origin.x);
                this.set_notes_tree_width(width, cx);
            },
        ))
        .on_drop::<WikiTreeDrag>(cx.listener(|this, _drag: &WikiTreeDrag, _window, cx| {
            this.finish_tree_resize(cx);
        }))
        .child(detail_column(state, t, window, cx));

    div()
        .id("wiki-view")
        .test_support()
        .size_full()
        .flex()
        .flex_col()
        .child(toolbar(state, t, cx))
        .child(row)
        .into_any_element()
}

/// Status toolbar: counts on the left, Refresh / Lint / Setup on the right.
fn toolbar(state: &WikiState, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let summary = match &state.status {
        Some(status) => {
            let sources = status.sources.new
                + status.sources.indexed
                + status.sources.compiled
                + status.sources.stale
                + status.sources.failed;
            let pages = status.pages.source
                + status.pages.entity
                + status.pages.concept
                + status.pages.page;
            format!(
                "{sources} sources · {} compiled · {pages} pages",
                status.sources.compiled
            )
        }
        None => "Index unavailable".to_string(),
    };
    let refresh_label = if state.busy {
        "Refreshing…"
    } else {
        "Refresh"
    };

    div()
        .id("wiki-toolbar")
        .test_support()
        .h(px(TOOLBAR_H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(12.))
        .border_b_1()
        .border_color(t.divider)
        .child(
            div()
                .flex_1()
                .truncate()
                .text(Text::Micro)
                .text_color(t.text_muted)
                .child(summary),
        )
        .child(toolbar_button(
            "wiki-refresh",
            refresh_label,
            false,
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| this.refresh_wiki(cx)),
        ))
        .child(toolbar_button(
            "wiki-lint",
            "Lint",
            state.panel == Some(WikiPanel::Lint),
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.open_wiki_panel(WikiPanel::Lint, cx)
            }),
        ))
        .child(toolbar_button(
            "wiki-setup",
            "Setup",
            state.panel == Some(WikiPanel::Setup),
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.open_wiki_panel(WikiPanel::Setup, cx)
            }),
        ))
        .child(toolbar_button(
            "wiki-status",
            "Status",
            state.panel == Some(WikiPanel::Status),
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.open_wiki_panel(WikiPanel::Status, cx)
            }),
        ))
        .into_any_element()
}

fn toolbar_button(
    id: &'static str,
    label: &'static str,
    active: bool,
    t: &Tokens,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .h(px(22.))
        .px(px(8.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .when(active, |this| this.bg(t.selected))
        .cursor_pointer()
        .hover(|style| style.bg(t.hover))
        .on_click(on_click)
        .text(Text::Small)
        .text_color(if active { t.text } else { t.text_muted })
        .child(label)
        .into_any_element()
}

/// A 5 px grab lane drawing the same 1 px rule as the plain divider.
fn tree_splitter(t: &Tokens) -> AnyElement {
    div()
        .id("wiki-tree-splitter")
        .test_support()
        .w(px(5.))
        .mx(px(-2.))
        .h_full()
        .flex()
        .justify_center()
        .flex_none()
        .cursor(CursorStyle::ResizeLeftRight)
        .hover(|style| style.bg(t.hover))
        .on_drag(WikiTreeDrag, |_drag, _offset, _window, cx| {
            cx.new(|_| TreeDragGhost)
        })
        .child(div().w(px(1.)).h_full().bg(t.divider))
        .into_any_element()
}

/// Payload for a tree-splitter drag. Only its type selects the listener.
pub struct WikiTreeDrag;

struct TreeDragGhost;

impl Render for TreeDragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(1.)).h(px(1.))
    }
}

/// The notes/documents column: whole-tree rail, or the two stacked halves with
/// the vertical splitter between them.
fn tree_column(
    state: &WikiState,
    t: &Tokens,
    window: &Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    if !state.notes.tree_open {
        return tree_rail(t, cx);
    }

    let total = (f32::from(window.bounds().size.height) - TITLEBAR_H - TOOLBAR_H).max(0.0);
    let both = !state.notes_collapsed && !state.docs_collapsed;
    let available = (total - if both { SPLITTER_H } else { 0.0 }).max(0.0);
    let notes_h = state.notes_height(available);
    let docs_h = if state.docs_collapsed {
        HEADER_H
    } else {
        (available - notes_h).max(0.0)
    };

    let mut column = div()
        .id("wiki-tree")
        .test_support()
        .w(px(state.notes.tree_w))
        .h_full()
        .flex()
        .flex_col()
        .child(notes_half(state, notes_h, t, cx));

    if both {
        column = column.child(splitter(t));
    }

    column
        .child(documents_half(state, docs_h, t, cx))
        .on_drag_move::<WikiSplitDrag>(cx.listener(
            |this, event: &DragMoveEvent<WikiSplitDrag>, _window, cx| {
                let height = f32::from(event.bounds.size.height);
                if height > 0.0 {
                    let share = (f32::from(event.event.position.y)
                        - f32::from(event.bounds.origin.y))
                        / height;
                    this.set_wiki_split(share, cx);
                }
            },
        ))
        .on_drop::<WikiSplitDrag>(
            cx.listener(|this, _drag: &WikiSplitDrag, _window, cx| this.finish_wiki_split(cx)),
        )
        .into_any_element()
}

fn notes_half(
    state: &WikiState,
    height: f32,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let collapsed = state.notes_collapsed;
    let toggle = half_toggle("wiki-notes", collapsed, WikiHalf::Notes, t, cx);
    let mut half = div()
        .id("wiki-notes-tree")
        .test_support()
        .h(px(height))
        .flex_none()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(notes::tree_header(t, cx, Some(toggle), collapsed));
    if !collapsed {
        half = half.child(notes::tree_body(&state.notes, t, cx));
    }
    half.into_any_element()
}

fn documents_half(
    state: &WikiState,
    height: f32,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let collapsed = state.docs_collapsed;
    let toggle = half_toggle("wiki-documents", collapsed, WikiHalf::Documents, t, cx);
    let mut half = div()
        .id("wiki-documents-tree")
        .test_support()
        .h(px(height))
        .flex_none()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(documents::tree_header(t, cx, Some(toggle), collapsed));
    if !collapsed {
        half = half.child(documents::tree_body(&state.documents, t, cx));
    }
    half.into_any_element()
}

fn splitter(t: &Tokens) -> AnyElement {
    div()
        .id("wiki-splitter")
        .test_support()
        .h(px(5.))
        .my(px(-2.))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .cursor(CursorStyle::ResizeUpDown)
        .hover(|style| style.bg(t.hover))
        .on_drag(WikiSplitDrag, |_drag, _offset, _window, cx| {
            cx.new(|_| SplitDragGhost)
        })
        .child(div().h(px(1.)).w_full().bg(t.divider))
        .into_any_element()
}

/// Payload for the vertical splitter drag.
pub struct WikiSplitDrag;

struct SplitDragGhost;

impl Render for SplitDragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(1.)).h(px(1.))
    }
}

/// One half's collapse/expand chevron, appended to its 38 px header.
fn half_toggle(
    id: &'static str,
    collapsed: bool,
    half: WikiHalf,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    div()
        .id(if collapsed {
            format!("{id}-expand")
        } else {
            format!("{id}-collapse")
        })
        .test_support()
        .p(px(3.))
        .rounded(px(4.))
        .cursor_pointer()
        .hover(|style| style.bg(t.hover))
        .on_click(
            cx.listener(move |this, _: &ClickEvent, _window, cx| this.toggle_wiki_half(half, cx)),
        )
        .child(icon(
            if collapsed {
                Icon::ChevronRight
            } else {
                Icon::ChevronDown
            },
            13.,
            t.text_muted,
        ))
        .into_any_element()
}

/// The whole tree column collapsed to its 30 px rail.
fn tree_rail(t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    div()
        .id("wiki-tree-rail")
        .test_support()
        .w(px(notes::RAIL_W))
        .h_full()
        .flex()
        .flex_col()
        .items_center()
        .pt(px(10.))
        .border_r_1()
        .border_color(t.divider)
        .child(
            div()
                .id("wiki-expand")
                .test_support()
                .p(px(4.))
                .rounded(px(t.radius))
                .cursor_pointer()
                .hover(|style| style.bg(t.hover))
                .on_click(
                    cx.listener(|this, _: &ClickEvent, _window, cx| this.toggle_wiki_tree(cx)),
                )
                .child(icon(Icon::ChevronRight, 14., t.text_muted)),
        )
        .into_any_element()
}

/// One tab row and one detail column shared by both kinds.
fn detail_column(
    state: &mut WikiState,
    t: &Tokens,
    window: &mut Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let mut column = div()
        .id("wiki-content")
        .test_support()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col();

    if let Some(panel) = state.panel {
        return column.child(panel_body(state, panel, t)).into_any_element();
    }

    if state.order.is_empty() {
        return column
            .child(placeholder::message(
                "wiki-empty",
                Icon::Folder,
                "Nothing open",
                "Pick a wiki page or source in either tree, or use the new and upload buttons above.",
                t,
            ))
            .into_any_element();
    }

    column = column.child(tab_row(state, t, cx));

    let Some(key) = state.active.clone() else {
        return column.into_any_element();
    };
    match key.kind {
        TabKind::Note => {
            if let Some(index) = state.notes.tab_index(&key.path) {
                column = column.child(notes::detail_body(&state.notes, index, t, window, cx));
            }
        }
        TabKind::Doc => {
            if let Some(index) = state.documents.tab_index(&key.path) {
                column = column.child(documents::detail_body(
                    &mut state.documents,
                    index,
                    t,
                    window,
                    cx,
                ));
            }
        }
    }
    column.into_any_element()
}

fn tab_row(state: &WikiState, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let mut strip = crate::views::tab_strip(t)
        .id("wiki-tab-strip")
        .test_support();
    for key in &state.order {
        let active = state.active.as_ref() == Some(key);
        let (name, dirty, glyph) = match key.kind {
            TabKind::Note => {
                let tab = state.notes.tabs.iter().find(|tab| tab.path == key.path);
                (
                    tab.map(|tab| tab.name.clone()).unwrap_or_default(),
                    tab.is_some_and(|tab| tab.draft.is_dirty()),
                    Icon::StickyNote,
                )
            }
            TabKind::Doc => {
                let tab = state.documents.tabs.iter().find(|tab| tab.path == key.path);
                (
                    tab.map(|tab| tab.name.clone()).unwrap_or_default(),
                    tab.is_some_and(|tab| tab.has_unsaved()),
                    documents::doc_icon(&key.path),
                )
            }
        };
        let path = key.path.display().to_string();
        let activate = key.clone();
        let close = key.clone();
        strip = strip.child(
            crate::views::pane_tab(format!("wiki-tab-{}-{path}", key.kind_id()), active, t)
                .test_support()
                .cursor_pointer()
                .hover(|style| style.bg(t.hover))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.activate_wiki_tab(activate.clone(), window, cx)
                }))
                .child(icon(glyph, 12., t.text_faint))
                .child(
                    div()
                        .text(Text::Small)
                        .text_color(if active { t.text } else { t.text_faint })
                        .child(name),
                )
                .when(dirty, |this| {
                    this.child(div().size(px(3.)).rounded(px(2.)).bg(t.accent))
                })
                .child(
                    div()
                        .id(format!("wiki-tab-close-{}-{path}", key.kind_id()))
                        .test_support()
                        .p(px(1.))
                        .rounded(px(3.))
                        .hover(|style| style.bg(t.pressed))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            cx.stop_propagation();
                            this.close_wiki_tab(close.clone(), cx)
                        }))
                        .child(icon(Icon::X, 10., t.text_faint)),
                ),
        );
    }
    strip.child(div().flex_1()).into_any_element()
}

fn panel_body(state: &WikiState, panel: WikiPanel, t: &Tokens) -> AnyElement {
    let (title, id) = match panel {
        WikiPanel::Status => ("Wiki status", "wiki-panel-status"),
        WikiPanel::Lint => ("Lint findings", "wiki-panel-lint"),
        WikiPanel::Setup => ("Compile setup", "wiki-panel-setup"),
    };
    let mut body = div().flex().flex_col().gap(px(6.));
    if let Some(error) = &state.panel_error {
        body = body.child(
            div()
                .text(Text::Small)
                .text_color(t.danger)
                .child(error.clone()),
        );
    }

    match panel {
        WikiPanel::Status => {
            if let Some(status) = &state.status {
                body = body
                    .child(section_label("Sources", t))
                    .child(kv_row("New", &status.sources.new.to_string(), t))
                    .child(kv_row("Indexed", &status.sources.indexed.to_string(), t))
                    .child(kv_row("Compiled", &status.sources.compiled.to_string(), t))
                    .child(kv_row("Stale", &status.sources.stale.to_string(), t))
                    .child(kv_row("Failed", &status.sources.failed.to_string(), t))
                    .child(section_label("Pages", t))
                    .child(kv_row("Source pages", &status.pages.source.to_string(), t))
                    .child(kv_row("Entities", &status.pages.entity.to_string(), t))
                    .child(kv_row("Concepts", &status.pages.concept.to_string(), t))
                    .child(kv_row("Hand-written", &status.pages.page.to_string(), t))
                    .child(kv_row("Index", &status.pages.index.to_string(), t));
                if let Some(root) = &state.notes.root {
                    body = body.child(kv_row("Wiki root", &root.display().to_string(), t));
                }
                if let Some(root) = &state.documents.root {
                    body = body.child(kv_row("Sources root", &root.display().to_string(), t));
                }
                body = body.child(kv_row(
                    "Instructions",
                    &format!("v{}", kandrium_core::WIKI_INSTRUCTIONS_VERSION),
                    t,
                ));
            } else {
                body = body.child(body_text(
                    "No index yet. Press Refresh to scan sources and rebuild the page index.",
                    t,
                ));
            }
        }
        WikiPanel::Lint => {
            if state.busy {
                body = body.child(body_text("Linting…", t));
            } else if state.findings.is_empty() {
                body = body.child(body_text("No findings.", t));
            } else {
                for finding in &state.findings {
                    let color = match finding.severity {
                        LintSeverity::Error => t.danger,
                        LintSeverity::Warn => t.accent,
                        LintSeverity::Info => t.text_muted,
                    };
                    body = body.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .py(px(4.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .text(Text::Micro)
                                            .text_color(color)
                                            .child(finding.code.as_str()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .truncate()
                                            .text(Text::Micro)
                                            .text_color(t.text_faint)
                                            .child(finding.path.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .text(Text::Small)
                                    .text_color(t.text_muted)
                                    .child(finding.message.clone()),
                            ),
                    );
                }
            }
        }
        WikiPanel::Setup => {
            body = body
                .child(body_text(
                    "Kandrium ships no model client. Point an MCP client (Codex, Claude, …) at the kandrium-mcp server and call get_wiki_instructions before writing pages.",
                    t,
                ))
                .child(kv_row("MCP command", "kandrium-mcp", t))
                .child(kv_row(
                    "Instructions version",
                    &format!("v{}", kandrium_core::WIKI_INSTRUCTIONS_VERSION),
                    t,
                ))
                .child(kv_row(
                    "Instructions hash",
                    &kandrium_core::instructions_hash(),
                    t,
                ))
                .child(section_label("Tag vocabulary", t))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .children(kandrium_core::TAG_VOCABULARY.iter().map(|tag| {
                            div()
                                .px(px(6.))
                                .py(px(2.))
                                .rounded(px(4.))
                                .bg(t.surface_2)
                                .text(Text::Micro)
                                .text_color(t.text_muted)
                                .child(*tag)
                        })),
                );
        }
    }

    div()
        .id(id)
        .test_support()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(10.))
        .p(px(20.))
        .child(div().text(Text::Heading).text_color(t.text).child(title))
        .child(body)
        .into_any_element()
}

fn section_label(label: &str, t: &Tokens) -> AnyElement {
    div()
        .pt(px(8.))
        .text(Text::Label)
        .text_color(t.text_muted)
        .child(label.to_string())
        .into_any_element()
}

fn kv_row(label: &str, value: &str, t: &Tokens) -> AnyElement {
    div()
        .flex()
        .items_baseline()
        .gap(px(12.))
        .child(
            div()
                .w(px(120.))
                .flex_none()
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text(Text::Small)
                .text_color(t.text)
                .child(value.to_string()),
        )
        .into_any_element()
}

fn body_text(text: &str, t: &Tokens) -> AnyElement {
    div()
        .max_w(px(620.))
        .text(Text::Body)
        .text_color(t.text_muted)
        .child(text.to_string())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(split: f32, notes_collapsed: bool, docs_collapsed: bool) -> WikiState {
        WikiState {
            split,
            notes_collapsed,
            docs_collapsed,
            ..WikiState::default()
        }
    }

    #[test]
    fn split_keeps_both_halves_at_least_the_minimum_height() {
        assert_eq!(state(0.5, false, false).notes_height(600.0), 300.0);
        // A share beyond the available room clamps so neither half collapses.
        assert_eq!(
            state(1.0, false, false).notes_height(600.0),
            600.0 - MIN_HALF_H
        );
        assert_eq!(state(0.0, false, false).notes_height(600.0), MIN_HALF_H);
    }

    #[test]
    fn collapsed_half_takes_only_its_header() {
        assert_eq!(state(0.5, true, false).notes_height(600.0), HEADER_H);
        assert_eq!(
            state(0.5, false, true).notes_height(600.0),
            600.0 - HEADER_H
        );
        assert_eq!(state(0.5, true, true).notes_height(600.0), HEADER_H);
    }
}
