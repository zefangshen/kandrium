//! Wiki: the merged Notes, Wiki and Sources section.
//!
//! The tree column splits vertically — personal notes on top, compiled wiki
//! pages in the middle, source documents below — with two drag seams, one
//! shared tab row and one detail column serving all three kinds. Each half
//! keeps its own tree panel (header actions, inline drafts, context menus)
//! from [`super::notes`], [`super::documents`] or this module; this module
//! owns the split, the collapse flags, the merged tab order and the detail
//! dispatcher.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, CursorStyle, DragMoveEvent, IntoElement, Render,
    TestSupportExt, Window, div, px,
};
use kandrium_core::notes::NoteEntry;
use kandrium_core::{LintFinding, LintSeverity, WikiStatus};

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::settings::{self, WikiHalf};
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::documents::{self, DocumentsState};
use crate::views::notes::{self, NotesState};
use crate::views::placeholder;

/// A half may not shrink below this while it is expanded.
pub const MIN_HALF_H: f32 = 120.0;
/// Half header height: the same 38 px row the tree panels use.
pub const HEADER_H: f32 = 38.0;
/// The wiki status toolbar above the halves.
pub const TOOLBAR_H: f32 = 34.0;
/// Height the 5 px vertical splitter occupies in the column layout: its
/// `-2 px` side margins overlap the two halves.
const SPLITTER_H: f32 = 1.0;
/// The titlebar the section body sits below, subtracted from the window height
/// when the split is converted to pixels.
const TITLEBAR_H: f32 = 38.0;
/// Smallest share a half may take, and the smallest gap the seams keep.
const MIN_SPLIT: f32 = settings::MIN_SPLIT_PERMILLE as f32 / 1000.0;
/// Largest share a seam may take.
const MAX_SPLIT: f32 = settings::MAX_SPLIT_PERMILLE as f32 / 1000.0;

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

/// The compiled-page half: the wiki root's Markdown tree.
#[derive(Default)]
pub struct WikiPagesState {
    pub root: Option<PathBuf>,
    pub entries: Vec<NoteEntry>,
    pub expanded: HashSet<PathBuf>,
    pub selected: Option<PathBuf>,
    pub error: Option<String>,
}

/// Wiki section state: the three tree states plus the merged tab order.
pub struct WikiState {
    /// Personal notes (`KANDRIUM_NOTES`).
    pub notes: NotesState,
    /// Compiled wiki pages (`KANDRIUM_WIKI`).
    pub pages: WikiPagesState,
    /// Source documents, PDFs plus Markdown.
    pub documents: DocumentsState,
    /// Display order of the merged tab row, oldest first.
    pub order: Vec<TabKey>,
    /// The tab the detail column renders.
    pub active: Option<TabKey>,
    /// Notes-half share of the tree split (0.15–0.85).
    pub split: f32,
    /// Cumulative Notes + Wiki share: where the second seam sits.
    pub pages_split: f32,
    pub notes_collapsed: bool,
    pub pages_collapsed: bool,
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
            pages: WikiPagesState::default(),
            documents: DocumentsState::new(),
            order: Vec::new(),
            active: None,
            split: settings::DEFAULT_SPLIT_PERMILLE as f32 / 1000.0,
            pages_split: settings::DEFAULT_SPLIT_PAGES_PERMILLE as f32 / 1000.0,
            notes_collapsed: false,
            pages_collapsed: false,
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

    /// Keep the two seams ordered after values arrive from settings or a drag.
    pub fn settle_splits(&mut self) {
        self.pages_split = self.pages_split.clamp(MIN_SPLIT, MAX_SPLIT);
        self.split = self
            .split
            .clamp(MIN_SPLIT, (self.pages_split - MIN_SPLIT).max(MIN_SPLIT));
    }

    /// Pixel heights of the Notes, Wiki and Sources halves for the column
    /// height `total`. Collapsed halves take `HEADER_H`; the rest share the
    /// remainder by the stored shares, falling back to equal parts whenever a
    /// weight is not positive.
    pub fn half_heights(&self, total: f32) -> (f32, f32, f32) {
        let collapsed = [
            self.notes_collapsed,
            self.pages_collapsed,
            self.docs_collapsed,
        ];
        let seams = usize::from(!self.notes_collapsed && !self.pages_collapsed)
            + usize::from(!self.pages_collapsed && !self.docs_collapsed);
        let collapsed_count = collapsed.iter().filter(|flag| **flag).count();
        let expanded_count = 3 - collapsed_count;
        let available =
            (total - HEADER_H * collapsed_count as f32 - SPLITTER_H * seams as f32).max(0.0);

        let weights = [
            if self.notes_collapsed {
                0.0
            } else {
                self.split.max(0.0)
            },
            if self.pages_collapsed {
                0.0
            } else {
                (self.pages_split - self.split).max(0.0)
            },
            if self.docs_collapsed {
                0.0
            } else {
                (1.0 - self.pages_split).max(0.0)
            },
        ];
        let ordered = weights
            .iter()
            .zip(collapsed)
            .all(|(weight, is_collapsed)| is_collapsed || *weight > 0.0);
        let shares = if ordered || expanded_count == 0 {
            weights
        } else {
            [
                if self.notes_collapsed { 0.0 } else { 1.0 },
                if self.pages_collapsed { 0.0 } else { 1.0 },
                if self.docs_collapsed { 0.0 } else { 1.0 },
            ]
        };
        let sum: f32 = shares.iter().sum();
        let min = MIN_HALF_H.min(available / expanded_count.max(1) as f32);
        let max = (available - min * expanded_count.saturating_sub(1) as f32).max(min);
        let mut heights = [0.0f32; 3];
        for (index, is_collapsed) in collapsed.iter().enumerate() {
            heights[index] = if *is_collapsed {
                HEADER_H
            } else if sum > 0.0 {
                (available * shares[index] / sum).clamp(min, max)
            } else if expanded_count > 0 {
                available / expanded_count as f32
            } else {
                0.0
            };
        }
        (heights[0], heights[1], heights[2])
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

/// The three stacked halves with a draggable seam between each pair, or the
/// whole-tree rail.
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
    let (notes_h, pages_h, sources_h) = state.half_heights(total);
    let notes_seam = !state.notes_collapsed && !state.pages_collapsed;
    let pages_seam = !state.pages_collapsed && !state.docs_collapsed;

    let mut column = div()
        .id("wiki-tree")
        .test_support()
        .w(px(state.notes.tree_w))
        .h_full()
        .flex()
        .flex_col()
        .child(notes_half(state, notes_h, t, cx));

    if notes_seam {
        column = column.child(seam("wiki-splitter", WikiSplitDrag, t));
    }

    column = column.child(pages_half(state, pages_h, t, cx));

    if pages_seam {
        column = column.child(seam("wiki-pages-splitter", WikiPagesSplitDrag, t));
    }

    column
        .child(documents_half(state, sources_h, t, cx))
        .on_drag_move::<WikiSplitDrag>(cx.listener(
            |this, event: &DragMoveEvent<WikiSplitDrag>, _window, cx| {
                if let Some(share) = seam_share(event) {
                    this.set_wiki_split(share, cx);
                }
            },
        ))
        .on_drop::<WikiSplitDrag>(
            cx.listener(|this, _drag: &WikiSplitDrag, _window, cx| this.finish_wiki_split(cx)),
        )
        .on_drag_move::<WikiPagesSplitDrag>(cx.listener(
            |this, event: &DragMoveEvent<WikiPagesSplitDrag>, _window, cx| {
                if let Some(share) = seam_share(event) {
                    this.set_wiki_pages_split(share, cx);
                }
            },
        ))
        .on_drop::<WikiPagesSplitDrag>(
            cx.listener(|this, _drag: &WikiPagesSplitDrag, _window, cx| this.finish_wiki_split(cx)),
        )
        .into_any_element()
}

/// Pointer position over the column, as a full-column share.
fn seam_share<D>(event: &DragMoveEvent<D>) -> Option<f32> {
    let height = f32::from(event.bounds.size.height);
    (height > 0.0)
        .then(|| (f32::from(event.event.position.y) - f32::from(event.bounds.origin.y)) / height)
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
        .child(notes::tree_header(t, cx, "NOTES", Some(toggle), collapsed));
    if !collapsed {
        half = half.child(notes::tree_body(&state.notes, t, cx));
    }
    half.into_any_element()
}

fn pages_half(
    state: &WikiState,
    height: f32,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let collapsed = state.pages_collapsed;
    let toggle = half_toggle("wiki-pages", collapsed, WikiHalf::Wiki, t, cx);
    let mut half = div()
        .id("wiki-pages-tree")
        .test_support()
        .h(px(height))
        .flex_none()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(pages_header(t, toggle, collapsed));
    if !collapsed {
        half = half.child(pages_body(&state.pages, t, cx));
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
    let toggle = half_toggle("wiki-documents", collapsed, WikiHalf::Sources, t, cx);
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

/// The Wiki half's 38 px header: just the collapse toggle and its name. Pages
/// are written through the wiki pipeline, not created from this tree.
fn pages_header(t: &Tokens, toggle: AnyElement, separator: bool) -> AnyElement {
    div()
        .h(px(HEADER_H))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(12.))
        .when(separator, |this| this.border_b_1().border_color(t.divider))
        .child(toggle)
        .child(
            div()
                .flex_1()
                .truncate()
                .text(Text::Label)
                .text_color(t.text_muted)
                .child("WIKI"),
        )
        .into_any_element()
}

/// The page tree: folders and Markdown pages, with no inline drafts.
fn pages_body(state: &WikiPagesState, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let mut rows = div()
        .id("wiki-pages-rows")
        .test_support()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll();
    if let Some(error) = &state.error {
        rows = rows.child(
            div()
                .id("wiki-pages-error")
                .test_support()
                .mx(px(8.))
                .my(px(6.))
                .p(px(8.))
                .rounded(px(t.radius))
                .bg(gpui_kit::rgba(0x5c1d1dcc))
                .text(Text::Small)
                .text_color(t.text)
                .child(error.clone()),
        );
    }
    if state.entries.is_empty() && state.error.is_none() {
        rows = rows.child(
            div()
                .px(px(12.))
                .py(px(10.))
                .text(Text::Small)
                .text_color(t.text_faint)
                .child("No wiki pages yet."),
        );
    }
    for entry in &state.entries {
        rows = rows.child(page_row(entry, 0, state, t, cx));
    }
    div()
        .id("wiki-pages-body")
        .test_support()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(rows)
        .into_any_element()
}

fn page_row(
    entry: &NoteEntry,
    depth: usize,
    state: &WikiPagesState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let path = entry.path.clone();
    let selected = state.selected.as_deref() == Some(entry.path.as_path());
    let open = state.expanded.contains(&entry.path);
    let mut row = div()
        .id(format!("wiki-page-row-{}", entry.path.display()))
        .test_support()
        .mx(px(6.))
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(26.))
        .rounded(px(4.))
        .pl(px(8.0 + depth as f32 * 12.0))
        .pr(px(8.))
        .cursor_pointer()
        .when(selected, |this| this.bg(t.surface_2))
        .hover(|style| style.bg(t.hover));

    if entry.is_dir {
        let toggle = path.clone();
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
            .child(icon(Icon::Folder, 13., t.text_muted))
            .child(
                div()
                    .flex_1()
                    .text(Text::Small)
                    .text_color(t.text)
                    .child(entry.name.clone()),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.toggle_wiki_page_folder(toggle.clone(), cx)
            }));
    } else {
        row = row
            .child(div().w(px(12.)))
            .child(icon(Icon::FileText, 13., t.text_muted))
            .child(
                div()
                    .flex_1()
                    .text(Text::Small)
                    .text_color(t.text)
                    .child(entry.name.clone()),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_note(path.clone(), window, cx)
            }));
    }

    let mut column = div().flex().flex_col().child(row);
    if entry.is_dir && open {
        for child in entry.children.iter().flatten() {
            column = column.child(page_row(child, depth + 1, state, t, cx));
        }
    }
    column.into_any_element()
}

/// A 5 px grab lane drawing the same 1 px rule as the plain divider.
fn seam<D>(id: &'static str, drag: D, t: &Tokens) -> AnyElement
where
    D: Clone + 'static,
{
    div()
        .id(id)
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
        .on_drag(drag, |_drag, _offset, _window, cx| {
            cx.new(|_| SplitDragGhost)
        })
        .child(div().h(px(1.)).w_full().bg(t.divider))
        .into_any_element()
}

/// Payload for the Notes ↔ Wiki seam drag.
#[derive(Clone, Copy)]
pub struct WikiSplitDrag;

/// Payload for the Wiki ↔ Sources seam drag.
#[derive(Clone, Copy)]
pub struct WikiPagesSplitDrag;

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
                "Pick a note, wiki page or source in the trees, or use the new and upload buttons above.",
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
        WikiPanel::Status => ("Knowledge status", "wiki-panel-status"),
        WikiPanel::Lint => ("Lint findings", "wiki-panel-lint"),
        WikiPanel::Setup => ("Compile setup", "wiki-panel-setup"),
    };
    let mut body = div()
        .id("wiki-panel-body")
        .flex()
        .flex_col()
        .gap(px(6.))
        .flex_1()
        .min_h_0()
        .overflow_y_scroll();
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
                    body = body.child(kv_row("Notes root", &root.display().to_string(), t));
                }
                if let Some(root) = &state.pages.root {
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
                )
                // The same bytes `get_wiki_instructions` serves, compiled into
                // this build, so the panel cannot drift from the server.
                .child(section_label("Instructions", t))
                .child(
                    div()
                        .id("wiki-setup-instructions")
                        .test_support()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .children(kandrium_core::WIKI_INSTRUCTIONS.lines().map(|line| {
                            div()
                                .text(Text::Micro)
                                .text_color(t.text_muted)
                                .child(if line.is_empty() {
                                    " ".to_string()
                                } else {
                                    line.to_string()
                                })
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

    fn state(
        split: f32,
        pages_split: f32,
        notes_collapsed: bool,
        pages_collapsed: bool,
        docs_collapsed: bool,
    ) -> WikiState {
        WikiState {
            split,
            pages_split,
            notes_collapsed,
            pages_collapsed,
            docs_collapsed,
            ..WikiState::default()
        }
    }

    #[test]
    fn default_split_gives_each_half_a_third() {
        let (notes, pages, sources) = WikiState::default().half_heights(600.0);
        let expected = (600.0 - 2.0 * SPLITTER_H) / 3.0;
        assert!((notes - expected).abs() < 0.5, "notes {notes}");
        assert!((pages - expected).abs() < 0.5, "pages {pages}");
        assert!((sources - expected).abs() < 0.5, "sources {sources}");
    }

    #[test]
    fn half_heights_clamp_tiny_shares_to_the_minimum() {
        let (notes, pages, sources) = state(0.15, 0.85, false, false, false).half_heights(600.0);
        assert_eq!(notes, MIN_HALF_H);
        assert_eq!(sources, MIN_HALF_H);
        assert_eq!(
            pages,
            600.0 - 2.0 * SPLITTER_H - 2.0 * MIN_HALF_H,
            "the middle half takes the room the clamped halves leave"
        );
    }

    #[test]
    fn collapsed_halves_take_only_their_header() {
        let (notes, pages, sources) = state(0.333, 0.667, true, false, false).half_heights(600.0);
        assert_eq!(notes, HEADER_H);
        let expanded = 600.0 - HEADER_H - SPLITTER_H;
        assert!((pages - expanded / 2.0).abs() < 0.5, "pages {pages}");
        assert!((sources - expanded / 2.0).abs() < 0.5, "sources {sources}");

        let (notes, pages, sources) = state(0.333, 0.667, false, true, false).half_heights(600.0);
        assert_eq!(pages, HEADER_H);
        // A collapsed middle half removes both seams, so the outer halves
        // share the whole remainder.
        let expanded = 600.0 - HEADER_H;
        assert!((notes - expanded / 2.0).abs() < 0.5, "notes {notes}");
        assert!((sources - expanded / 2.0).abs() < 0.5, "sources {sources}");
    }

    #[test]
    fn settle_splits_keeps_the_seams_ordered() {
        let mut state = state(0.9, 0.2, false, false, false);
        state.settle_splits();
        assert_eq!(state.split, MIN_SPLIT);
        assert_eq!(state.pages_split, 0.2);
    }
}
