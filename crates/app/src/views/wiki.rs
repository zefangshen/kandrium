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
        .id("wiki-view")
        .test_support()
        .size_full()
        .flex()
        .child(tree_column(state, t, window, cx));

    if state.notes.tree_open {
        row = row.child(tree_splitter(t));
    }
    row.on_drag_move::<WikiTreeDrag>(cx.listener(
        |this, event: &DragMoveEvent<WikiTreeDrag>, _window, cx| {
            let width = f32::from(event.event.position.x) - f32::from(event.bounds.origin.x);
            this.set_notes_tree_width(width, cx);
        },
    ))
    .on_drop::<WikiTreeDrag>(cx.listener(|this, _drag: &WikiTreeDrag, _window, cx| {
        this.finish_tree_resize(cx);
    }))
    .child(detail_column(state, t, window, cx))
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

    let total = (f32::from(window.bounds().size.height) - TITLEBAR_H).max(0.0);
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
                    tab.is_some_and(|tab| tab.dirty),
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
