//! Documents: a tree over the documents root, PDF and Markdown tabs, and the
//! per-tab session that survives a restart.
//!
//! Every disk operation goes through `kandrium_core::documents`, so the root
//! (`KANDRIUM_DOCS`), the path guards, the `-2` import names and the atomic
//! annotation sidecars are the same ones the MCP server uses. This module owns
//! only the tree, the tab list and the small session file that stores which
//! tabs were open, keyed by root-relative paths. PDF rendering lives next door
//! in [`crate::views::documents_pdf`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use gpui_kit::base::text::markdown;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Editor, EditorState, InputState, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, Context, Entity, Pixels, Point, RenderImage, Rgba,
    ScrollHandle, TestSupportExt, Window, div, point, px, relative, rgba, size,
};

use kandrium_core::documents::{AnnotationKind, DocAnnotation, DocEntry};
use pdf_render::{Event, PageText, PdfWorker};

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::draft::Draft;
use crate::theme::{Text, TextStyled, Tokens};
use crate::vim::VimSession;

/// Tree panel width when expanded, matching the other tree sections.
pub const TREE_W: f32 = 240.0;
/// Collapsed rail width.
pub const RAIL_W: f32 = 30.0;
/// Tree row height.
pub const ROW_H: f32 = 26.0;
/// Indentation per folder level.
pub const INDENT: f32 = 14.0;

/// Space between page blocks in the viewer.
pub const PAGE_MARGIN: f32 = 8.0;
/// Reserved lane for the viewer's scrollbar.
pub const VIEWER_RIGHT_INSET: f32 = 7.0;

pub const ZOOM_STEP: f32 = 1.25;
pub const ZOOM_MIN: f32 = 0.25;
pub const ZOOM_MAX: f32 = 4.0;

/// Rendered-page cache cap (egui keeps the same number of textures).
pub const TEXTURE_CACHE_CAP: usize = 24;

/// Side of a sticky-note anchor in points.
pub const NOTE_ANCHOR: f32 = 20.0;
/// Highlight fill alpha (of 255).
pub const HIGHLIGHT_ALPHA: u8 = 90;

/// Halo colours for notes and highlights, in the same order as egui.
pub const NOTE_COLORS: [&str; 4] = ["#f6d448", "#a5d6a7", "#8ab4f8", "#f6a6c1"];

/// Debounce before changed annotations hit the sidecar.
pub const ANNOTATION_AUTOSAVE: Duration = Duration::from_millis(500);

/// Which pointer tool the viewer has active. Selection stays visual-only,
/// exactly like the egui app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Select,
    Highlight,
    Note,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Highlight => "Highlight",
            Tool::Note => "Note",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Tool::Select => Icon::Square,
            Tool::Highlight => Icon::Highlighter,
            Tool::Note => Icon::StickyNote,
        }
    }
}

/// Which viewer a tab uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind {
    Pdf,
    Markdown,
}

/// A selection drag in page view space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    pub page: usize,
    pub start: [f32; 2],
    pub current: [f32; 2],
}

impl Selection {
    /// Normalized rect `[x0, y0, x1, y1]`.
    pub fn rect(&self) -> [f32; 4] {
        [
            self.start[0].min(self.current[0]),
            self.start[1].min(self.current[1]),
            self.start[0].max(self.current[0]),
            self.start[1].max(self.current[1]),
        ]
    }
}

/// One open document.
pub struct DocTab {
    pub kind: DocKind,
    pub path: PathBuf,
    pub name: String,
    // Markdown
    pub editor: Option<Entity<EditorState>>,
    pub vim: Option<VimSession>,
    pub preview: bool,
    /// Markdown text as last written to disk, plus the debounce generation.
    pub draft: Draft,
    // PDF
    pub worker: Option<PdfWorker>,
    pub events: Option<Receiver<Event>>,
    pub page_count: usize,
    pub error: Option<String>,
    /// Zoom multiplier over fit-width (1.0 = fit width).
    pub zoom: f32,
    /// Natural page size in points, learned from the first render.
    pub page_size: Option<(f32, f32)>,
    pub images: HashMap<(usize, u32), Arc<RenderImage>>,
    pub stamps: HashMap<(usize, u32), u64>,
    pub pending: HashSet<(usize, u32)>,
    pub page_texts: HashMap<usize, PageText>,
    pub text_requested: HashSet<usize>,
    pub annotations: Vec<DocAnnotation>,
    /// Annotation sidecar as last written, plus the debounce generation.
    pub ann: Draft<Vec<DocAnnotation>>,
    pub selection: Option<Selection>,
    pub popover: Option<String>,
    pub popover_editor: Option<Entity<TextareaState>>,
    pub visible_page: usize,
    pub jump_to: Option<usize>,
    /// Page rects recorded during prepaint (window coordinates).
    pub page_bounds: Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>,
    /// Scroll viewport recorded during prepaint (window coordinates).
    pub viewport: Rc<RefCell<Option<Bounds<Pixels>>>>,
    /// Fit-width scale used by the last frame, for pointer math.
    pub last_scale: f32,
    /// Monotonic counter for texture-cache LRU stamps.
    pub next_stamp: u64,
    pub scroll: ScrollHandle,
}

impl DocTab {
    pub(crate) fn markdown(
        path: PathBuf,
        name: String,
        text: String,
        editor: Entity<EditorState>,
    ) -> Self {
        Self {
            kind: DocKind::Markdown,
            path,
            name,
            editor: Some(editor),
            vim: Some(VimSession::new()),
            preview: false,
            draft: Draft::new(text),
            worker: None,
            events: None,
            page_count: 0,
            error: None,
            zoom: 1.0,
            page_size: None,
            images: HashMap::new(),
            stamps: HashMap::new(),
            pending: HashSet::new(),
            page_texts: HashMap::new(),
            text_requested: HashSet::new(),
            annotations: Vec::new(),
            ann: Draft::new(Vec::new()),
            selection: None,
            popover: None,
            popover_editor: None,
            visible_page: 0,
            jump_to: None,
            page_bounds: Rc::new(RefCell::new(HashMap::new())),
            viewport: Rc::new(RefCell::new(None)),
            last_scale: 1.0,
            next_stamp: 0,
            scroll: ScrollHandle::new(),
        }
    }

    pub(crate) fn pdf(
        path: PathBuf,
        name: String,
        worker: PdfWorker,
        events: Receiver<Event>,
        annotations: Vec<DocAnnotation>,
    ) -> Self {
        Self {
            kind: DocKind::Pdf,
            path,
            name,
            editor: None,
            vim: None,
            preview: false,
            draft: Draft::new(String::new()),
            worker: Some(worker),
            events: Some(events),
            page_count: 0,
            error: None,
            zoom: 1.0,
            page_size: None,
            images: HashMap::new(),
            stamps: HashMap::new(),
            pending: HashSet::new(),
            page_texts: HashMap::new(),
            text_requested: HashSet::new(),
            annotations: annotations.clone(),
            ann: Draft::new(annotations),
            selection: None,
            popover: None,
            popover_editor: None,
            visible_page: 0,
            jump_to: None,
            page_bounds: Rc::new(RefCell::new(HashMap::new())),
            viewport: Rc::new(RefCell::new(None)),
            last_scale: 1.0,
            next_stamp: 0,
            scroll: ScrollHandle::new(),
        }
    }

    /// Dirty marker in the tab strip: Markdown edits or unsaved annotations.
    pub fn has_unsaved(&self) -> bool {
        self.draft.is_dirty() || self.ann.is_dirty()
    }
}

/// What the inline name field is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocDraftKind {
    File,
    Folder,
    Rename,
}

/// An inline name field: a create row in the tree header, or a row label
/// swapped for an input while renaming.
pub struct DocDraft {
    pub kind: DocDraftKind,
    /// Directory a new entry goes into, or the path being renamed.
    pub target: PathBuf,
    pub input: Entity<InputState>,
}

/// Documents section state.
pub struct DocumentsState {
    pub root: Option<PathBuf>,
    pub entries: Vec<DocEntry>,
    pub expanded: HashSet<PathBuf>,
    pub selected: Option<PathBuf>,
    pub tabs: Vec<DocTab>,
    pub active: Option<usize>,
    pub tree_open: bool,
    /// Expanded tree width in points; persisted by the shell.
    pub tree_w: f32,
    pub error: Option<String>,
    pub draft: Option<DocDraft>,
    /// Path waiting for a delete confirmation.
    pub confirm: Option<PathBuf>,
    pub tool: Tool,
    /// Session file read on the first load, applied once a window exists.
    pub session_loaded: bool,
    pub restore: Option<(bool, Option<String>, Vec<String>)>,
    /// Set while a render/text request is outstanding; the shell repaints.
    pub needs_repaint: bool,
    /// Wiki compile state per source path, for the tree dots.
    pub source_states: HashMap<PathBuf, kandrium_core::SourceState>,
}

impl Default for DocumentsState {
    fn default() -> Self {
        Self {
            root: None,
            entries: Vec::new(),
            expanded: HashSet::new(),
            selected: None,
            tabs: Vec::new(),
            active: None,
            tree_open: true,
            tree_w: TREE_W,
            error: None,
            draft: None,
            confirm: None,
            tool: Tool::Select,
            session_loaded: false,
            restore: None,
            needs_repaint: false,
            source_states: HashMap::new(),
        }
    }
}

impl DocumentsState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tab_index(&self, path: &Path) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.path == path)
    }

    pub fn active_tab(&self) -> Option<&DocTab> {
        self.active.and_then(|index| self.tabs.get(index))
    }

    /// Where a new file or folder lands: the selected folder, the selected
    /// file's folder, or the root.
    pub fn selected_dir(&self, root: &Path) -> PathBuf {
        match &self.selected {
            Some(path) if path.is_dir() => path.clone(),
            Some(path) => path
                .parent()
                .map(|parent| parent.to_path_buf())
                .unwrap_or_else(|| root.to_path_buf()),
            None => root.to_path_buf(),
        }
    }
}

/// Screen point → page view space (points).
pub fn screen_to_page(origin: Point<Pixels>, scale: f32, pos: Point<Pixels>) -> [f32; 2] {
    [
        (pos.x - origin.x).as_f32() / scale,
        (pos.y - origin.y).as_f32() / scale,
    ]
}

/// Page view space rect → screen bounds.
pub fn page_to_screen(origin: Point<Pixels>, scale: f32, r: &[f32; 4]) -> Bounds<Pixels> {
    Bounds::new(
        origin + point(px(r[0] * scale), px(r[1] * scale)),
        size(px((r[2] - r[0]) * scale), px((r[3] - r[1]) * scale)),
    )
}

/// Scale in per-mille, the texture cache key.
pub fn permille(scale: f32) -> u32 {
    (scale * 1000.0).round().max(1.0) as u32
}

/// Small anchor quad for a note placed at `p` (points, y down).
pub fn note_anchor_quad(p: [f32; 2]) -> [[f32; 2]; 4] {
    [
        p,
        [p[0] + NOTE_ANCHOR, p[1]],
        [p[0] + NOTE_ANCHOR, p[1] + NOTE_ANCHOR],
        [p[0], p[1] + NOTE_ANCHOR],
    ]
}

/// Parse `#rrggbb` (leading `#` optional); falls back to the first note colour.
pub fn hex_rgb(hex: &str) -> (u8, u8, u8) {
    let hex = hex.trim_start_matches('#');
    if hex.len() >= 6
        && let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        )
    {
        return (r, g, b);
    }
    hex_rgb(NOTE_COLORS[0])
}

/// `#rrggbb` at an explicit alpha.
pub fn color_with_alpha(hex: &str, alpha: u8) -> Rgba {
    let (r, g, b) = hex_rgb(hex);
    rgba(((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | alpha as u32)
}

/// Pages worth rendering or keeping around: the visible window plus three
/// quarters of a page either side (egui's pre-render margin).
pub fn visible_pages(top: f32, viewport_h: f32, row_h: f32, count: usize) -> Vec<usize> {
    if row_h <= 0.0 {
        return Vec::new();
    }
    (0..count)
        .filter(|page| {
            let start = *page as f32 * row_h;
            let end = start + row_h;
            end >= top - row_h * 0.75 && start <= top + viewport_h + row_h * 0.75
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// Session text: `tree_open`, the active document's root-relative path, then
/// one root-relative path per line. Plain text keeps the gpui shell free of a
/// settings dependency while still persisting exactly what matters.
///
/// Earlier builds stored a bare tab index in `active=`; [`parse_session`] still
/// accepts that form for one release.
pub fn format_session(tree_open: bool, active: Option<&str>, rels: &[String]) -> String {
    let mut out = format!("tree_open={}\n", if tree_open { 1 } else { 0 });
    out.push_str(&format!("active={}\n", active.unwrap_or_default()));
    for rel in rels {
        out.push_str(rel);
        out.push('\n');
    }
    out
}

/// Parse [`format_session`] output. Only the two leading keyed lines are
/// interpreted; everything else is a path, so a file name containing `=`
/// survives. Unknown/missing keys fall back to `(tree open, no active tab)`.
pub fn parse_session(text: &str) -> (bool, Option<String>, Vec<String>) {
    let mut tree_open = true;
    let mut active = None;
    let mut rels = Vec::new();
    let mut keyed = 0;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if keyed < 2 {
            if let Some(value) = line.strip_prefix("tree_open=") {
                tree_open = value != "0";
                keyed += 1;
                continue;
            }
            if let Some(value) = line.strip_prefix("active=") {
                active = (!value.is_empty()).then(|| value.to_string());
                keyed += 1;
                continue;
            }
        }
        if !line.trim().is_empty() {
            rels.push(line.to_string());
        }
    }
    (tree_open, active, rels)
}

/// Store `path` relative to the documents root, with `/` separators.
pub fn rel_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let text = rel.to_string_lossy().replace('\\', "/");
    if text.is_empty() { None } else { Some(text) }
}

/// Resolve a session path against the documents root.
pub fn abs_path(root: &Path, rel: &str) -> PathBuf {
    root.join(rel)
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

/// The half's 38 px title row: optional leading control (the Wiki half
/// toggle), name, upload, new file and new folder. The whole-tree collapse
/// lives in the notes half's header, so this row carries no rail control. The
/// separator below is only drawn while the half is collapsed.
pub(crate) fn tree_header(
    t: &Tokens,
    cx: &mut Context<AppShell>,
    leading: Option<AnyElement>,
    separator: bool,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(38.))
        .flex_none()
        .px(px(12.))
        .when(separator, |this| this.border_b_1().border_color(t.divider))
        .children(leading)
        .child(
            div()
                .flex_1()
                .truncate()
                .text(Text::Label)
                .text_color(t.text_muted)
                .child("SOURCES"),
        )
        .child(tree_action(
            "documents-upload",
            Icon::Upload,
            t,
            cx.listener(|this, _: &ClickEvent, window, cx| {
                this.pick_and_import_documents(window, cx)
            }),
        ))
        .child(tree_action(
            "documents-new-file",
            Icon::FilePlus,
            t,
            cx.listener(|this, _: &ClickEvent, window, cx| {
                this.begin_doc_draft(DocDraftKind::File, window, cx)
            }),
        ))
        .child(tree_action(
            "documents-new-folder",
            Icon::FolderPlus,
            t,
            cx.listener(|this, _: &ClickEvent, window, cx| {
                this.begin_doc_draft(DocDraftKind::Folder, window, cx)
            }),
        ))
        .into_any_element()
}

/// The inline create row, any error and the tree rows. Scrolls inside the
/// height its parent gives it.
pub(crate) fn tree_body(
    state: &DocumentsState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let mut panel = div()
        .id("documents-tree-body")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0();

    if let Some(draft) = &state.draft
        && draft.kind != DocDraftKind::Rename
    {
        let label = match draft.kind {
            DocDraftKind::File => "New file (Markdown)",
            DocDraftKind::Folder => "New folder",
            DocDraftKind::Rename => "Rename",
        };
        panel = panel.child(
            div()
                .id("documents-draft")
                .test_support()
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(4.))
                .px(px(10.))
                .py(px(8.))
                .border_b_1()
                .border_color(t.divider)
                .capture_key_down(cx.listener(|this, event, window, cx| {
                    this.handle_doc_draft_key(event, window, cx)
                }))
                .child(
                    div()
                        .text(Text::Small)
                        .text_color(t.text_muted)
                        .child(label),
                )
                .child(crate::views::field_input(&draft.input, t)),
        );
    }

    if let Some(error) = &state.error {
        panel = panel.child(
            div()
                .id("documents-error")
                .test_support()
                .flex_none()
                .mx(px(8.))
                .my(px(6.))
                .p(px(8.))
                .rounded(px(t.radius))
                .bg(rgba(0x5c1d1dcc))
                .text(Text::Small)
                .text_color(t.text)
                .child(error.clone()),
        );
    }

    let entries = state.entries.clone();
    let mut rows = div()
        .id("documents-tree-rows")
        .test_support()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll();
    if entries.is_empty() && state.draft.is_none() {
        rows = rows.child(
            div()
                .px(px(12.))
                .py(px(10.))
                .text(Text::Small)
                .text_color(t.text_faint)
                .child("No sources yet. Upload a PDF or Markdown file."),
        );
    }
    for entry in &entries {
        rows = rows.child(tree_row(entry, 0, state, t, cx));
    }

    panel.child(rows).into_any_element()
}

fn tree_action(
    id: impl Into<gpui_kit::ElementId>,
    glyph: Icon,
    t: &Tokens,
    listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .p(px(3.))
        .rounded(px(4.))
        .cursor_pointer()
        .hover(|style| style.bg(t.hover))
        .on_click(listener)
        .child(icon(glyph, 13., t.text_muted))
        .into_any_element()
}

fn tree_row(
    entry: &DocEntry,
    depth: usize,
    state: &DocumentsState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let path = entry.path.clone();
    let selected = state.selected.as_deref() == Some(entry.path.as_path());
    let open = state.expanded.contains(&entry.path);
    let renaming = state
        .draft
        .as_ref()
        .is_some_and(|draft| draft.kind == DocDraftKind::Rename && draft.target == entry.path);

    let mut row = div()
        .id(format!("doc-row-{}", entry.path.display()))
        .test_support()
        .relative()
        .w_full()
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(ROW_H))
        .pl(px(10.0 + depth as f32 * INDENT))
        .pr(px(8.))
        .cursor_pointer()
        .when(selected, |this| this.bg(t.surface_2))
        .hover(|style| style.bg(t.hover));

    if selected {
        row = row.child(
            div()
                .absolute()
                .left(px(0.))
                .top(px(6.))
                .w(px(2.))
                .h(px(14.))
                .rounded(px(1.))
                .bg(t.accent),
        );
    }

    if renaming {
        let input = state
            .draft
            .as_ref()
            .map(|draft| draft.input.clone())
            .expect("renaming implies a draft");
        return row
            .capture_key_down(
                cx.listener(|this, event, window, cx| this.handle_doc_draft_key(event, window, cx)),
            )
            .child(div().w(px(12.)))
            .child(icon(
                if entry.is_dir {
                    Icon::Folder
                } else {
                    doc_icon(&entry.path)
                },
                13.,
                t.text_muted,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(crate::views::field_input(&input, t)),
            )
            .into_any_element();
    }

    if entry.is_dir {
        let toggle = path.clone();
        let rename = path.clone();
        let delete = path.clone();
        let new_file = path.clone();
        let new_folder = path.clone();
        let row_id = entry.path.display().to_string();
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
            .child(tree_action(
                format!("doc-folder-file-{row_id}"),
                Icon::FilePlus,
                t,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.begin_doc_draft_in(DocDraftKind::File, new_file.clone(), window, cx)
                }),
            ))
            .child(tree_action(
                format!("doc-folder-folder-{row_id}"),
                Icon::FolderPlus,
                t,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.begin_doc_draft_in(DocDraftKind::Folder, new_folder.clone(), window, cx)
                }),
            ))
            .child(tree_action(
                format!("doc-folder-rename-{row_id}"),
                Icon::Pencil,
                t,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.begin_doc_rename(rename.clone(), window, cx)
                }),
            ))
            .child(tree_action(
                format!("doc-folder-delete-{row_id}"),
                Icon::Trash2,
                t,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.ask_delete_document(delete.clone(), cx)
                }),
            ))
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.toggle_doc_folder(toggle.clone(), cx)
            }));
    } else {
        let open_path = path.clone();
        let rename = path.clone();
        let delete = path.clone();
        let row_id = entry.path.display().to_string();
        row = row
            .child(div().w(px(12.)))
            .child(icon(doc_icon(&entry.path), 13., t.text_muted))
            .child(
                div()
                    .flex_1()
                    .text(Text::Small)
                    .text_color(t.text)
                    .child(entry.name.clone()),
            )
            .child(source_dot(state.source_states.get(&entry.path).copied(), t))
            .child(tree_action(
                format!("doc-file-rename-{row_id}"),
                Icon::Pencil,
                t,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.begin_doc_rename(rename.clone(), window, cx)
                }),
            ))
            .child(tree_action(
                format!("doc-file-delete-{row_id}"),
                Icon::Trash2,
                t,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.ask_delete_document(delete.clone(), cx)
                }),
            ))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.open_document(open_path.clone(), window, cx)
            }));
    }

    let mut column = div().flex().flex_col().child(row);
    if entry.is_dir && open {
        for child in entry.children.iter().flatten() {
            column = column.child(tree_row(child, depth + 1, state, t, cx));
        }
    }
    column.into_any_element()
}

pub(crate) fn doc_icon(path: &Path) -> Icon {
    if kandrium_core::documents::is_pdf(path) {
        Icon::File
    } else {
        Icon::FileText
    }
}

/// A 6 px compile-state dot in the sources tree.
fn source_dot(state: Option<kandrium_core::SourceState>, t: &Tokens) -> AnyElement {
    use kandrium_core::SourceState;
    let color = match state {
        None => return div().w(px(6.)).flex_none().into_any_element(),
        Some(SourceState::New) => t.text_faint,
        Some(SourceState::Indexed) => t.text_muted,
        Some(SourceState::Compiled) => t.accent,
        Some(SourceState::Stale) => t.text,
        Some(SourceState::Failed) => t.danger,
    };
    div()
        .w(px(6.))
        .h(px(6.))
        .flex_none()
        .rounded(px(3.))
        .bg(color)
        .into_any_element()
}

/// One document's head row, Markdown editor/preview or PDF viewer, and status
/// bar, below the tab row.
pub(crate) fn detail_body(
    state: &mut DocumentsState,
    index: usize,
    t: &Tokens,
    window: &mut Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    if index >= state.tabs.len() {
        return div()
            .id("documents-detail")
            .test_support()
            .flex_1()
            .into_any_element();
    }

    let mut body = div()
        .id(format!(
            "documents-detail-{}",
            state.tabs[index].path.display()
        ))
        .test_support()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .flex_col();

    let markdown = state.tabs[index].kind == DocKind::Markdown;
    let preview = markdown && state.tabs[index].preview;
    let mut head = div()
        .id("documents-head")
        .test_support()
        .h(px(40.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(14.))
        .border_b_1()
        .border_color(t.divider)
        .child(crate::views::path_label(
            "documents-path",
            state.tabs[index].path.display().to_string(),
            t,
            cx,
        ))
        .child(div().flex_1());
    if markdown {
        head = head.child(crate::views::mode_switch(
            "documents-preview",
            preview,
            t,
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                if preview {
                    this.toggle_doc_preview(window, cx);
                }
            }),
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                if !preview {
                    this.toggle_doc_preview(window, cx);
                }
            }),
        ));
    }
    body = body.child(head);

    match state.tabs[index].kind {
        DocKind::Markdown => body = body.child(markdown_body(state, index, t, window, cx)),
        DocKind::Pdf => {
            body = body.child(crate::views::documents_pdf::viewer(state, index, t, cx));
        }
    }

    if let Some(confirm) = &state.confirm {
        body = body.child(delete_confirm(confirm, t, cx));
    }
    body.into_any_element()
}

fn markdown_body(
    state: &mut DocumentsState,
    index: usize,
    t: &Tokens,
    window: &Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let tab = &state.tabs[index];
    if tab.preview {
        let text = tab
            .editor
            .as_ref()
            .map(|editor| editor.read(cx).value().to_string())
            .unwrap_or_default();
        let pane = div()
            .id("doc-preview")
            .test_support()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(30.))
            .pt(px(20.))
            .pb(px(30.))
            .child(markdown(text.as_str()));
        return div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(pane)
            .child(status_bar(tab, t, cx))
            .into_any_element();
    }

    let editor = tab.editor.clone().expect("markdown tab has an editor");
    let lines = editor.read(cx).value().lines().count();
    let readonly = tab.vim.as_ref().is_none_or(|vim| !vim.is_insert());
    let block_cursor = tab.vim.as_ref().is_some_and(|vim| vim.block_cursor());
    let (mono_family, mono_size) = {
        let theme = cx.theme();
        (theme.mono_font_family.clone(), theme.mono_font_size)
    };
    div()
        .id("doc-markdown")
        .test_support()
        .flex()
        .flex_col()
        .flex_1()
        .h_full()
        .min_h_0()
        .child(
            div()
                .id("doc-editor-wrap")
                .test_support()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("doc-editor")
                        .test_support()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .relative()
                                .w_full()
                                .flex_1()
                                .min_h_0()
                                .child(
                                    div().absolute().inset_0().child(
                                        // Multi-line inputs auto-size to their
                                        // content; pin the editor to the wrapper.
                                        Editor::new(&editor)
                                            .h(relative(1.))
                                            .bordered(false)
                                            .readonly(readonly),
                                    ),
                                )
                                .when(block_cursor, |this| {
                                    this.child(
                                        crate::vim::block_cursor_overlay(
                                            editor.clone(),
                                            mono_family,
                                            mono_size,
                                            t.accent.into(),
                                            t.field.into(),
                                        )
                                        .absolute()
                                        .inset_0(),
                                    )
                                })
                                .child(crate::views::gutter_rule(lines, t, window)),
                        ),
                )
                .capture_key_down(cx.listener(|this, event, window, cx| {
                    this.handle_documents_key(event, window, cx)
                })),
        )
        .child(status_bar(tab, t, cx))
        .into_any_element()
}

/// The editor's bottom status bar: vim mode (also the toggle), pending keys,
/// ex line or error, cursor position, word count and save state.
fn status_bar(tab: &DocTab, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let Some(editor) = &tab.editor else {
        return div().into_any_element();
    };
    let preview = tab.preview;
    let position = editor.read(cx).cursor_position();
    let words = editor.read(cx).value().split_whitespace().count();
    let enabled = tab.vim.as_ref().is_some_and(|vim| vim.is_enabled());
    let pending = if preview {
        None
    } else if let Some(line) = tab.vim.as_ref().and_then(|vim| vim.ex_line()) {
        Some((line.to_string(), false))
    } else if let Some(error) = tab.vim.as_ref().and_then(|vim| vim.ex_error()) {
        Some((error.to_string(), true))
    } else {
        let pending = tab
            .vim
            .as_ref()
            .map(|vim| vim.pending())
            .unwrap_or_default();
        (!pending.is_empty()).then_some((pending, false))
    };
    crate::views::editor_status_bar(
        "doc-status",
        crate::views::EditorStatus {
            mode: if preview {
                "PREVIEW"
            } else {
                tab.vim.as_ref().map_or("vim", |vim| vim.mode_label())
            },
            vim: enabled,
            pending,
            line: position.line as usize + 1,
            column: position.character as usize + 1,
            words,
            dirty: tab.has_unsaved(),
            hint: if preview {
                "660 px measure"
            } else if enabled {
                "Esc normal · i insert · :w save · :q close"
            } else {
                "click the badge for vim"
            },
        },
        t,
        cx.listener(|this, _: &ClickEvent, _window, cx| this.toggle_vim_mode(cx)),
    )
}

fn tab_action(
    id: &'static str,
    label: &'static str,
    t: &Tokens,
    listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .px(px(8.))
        .py(px(3.))
        .rounded(px(t.radius))
        .cursor_pointer()
        .hover(|style| style.bg(t.hover))
        .on_click(listener)
        .child(
            div()
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(label),
        )
        .into_any_element()
}

fn delete_confirm(path: &Path, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let target = path.to_path_buf();
    div()
        .id("documents-delete-confirm")
        .test_support()
        .mx(px(16.))
        .mb(px(16.))
        .p(px(12.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(t.radius))
        .bg(t.card)
        .border_1()
        .border_color(t.border)
        .child(
            div()
                .flex_1()
                .text(Text::Small)
                .text_color(t.text)
                .child(format!("Delete \"{name}\"? This cannot be undone.")),
        )
        .child(tab_action(
            "documents-delete-cancel",
            "Cancel",
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| this.cancel_delete_document(cx)),
        ))
        .child(tab_action(
            "documents-delete-confirm-yes",
            "Delete",
            t,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.confirm_delete_document(target.clone(), cx)
            }),
        ))
        .into_any_element()
}

/// Whether a page-text map contains an annotation kind (used by tests).
#[allow(dead_code)]
fn has_kind(annotations: &[DocAnnotation], kind: AnnotationKind) -> bool {
    annotations.iter().any(|ann| ann.kind == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trips() {
        let rels = vec!["papers/report.pdf".to_string(), "notes.md".to_string()];
        let text = format_session(false, Some("notes.md"), &rels);
        assert_eq!(
            parse_session(&text),
            (false, Some("notes.md".to_string()), rels)
        );
    }

    #[test]
    fn session_defaults_and_garbage() {
        // Missing keys: tree open, no active tab.
        assert_eq!(
            parse_session("notes.md\n"),
            (true, None, vec!["notes.md".to_string()])
        );
        // The active entry is a root-relative path, and a path with `=` after
        // the keyed header is preserved verbatim.
        let (open, active, rels) = parse_session("tree_open=1\nactive=papers/a=b.md\nx=y.md\n");
        assert!(open);
        assert_eq!(active.as_deref(), Some("papers/a=b.md"));
        assert_eq!(rels, vec!["x=y.md".to_string()]);
        // A build before the merge stored an index; it still parses as text,
        // and the restore path falls back to it.
        assert_eq!(
            parse_session("tree_open=1\nactive=1\na.md\nb.md\n")
                .1
                .as_deref(),
            Some("1")
        );
        // Blank lines never become tabs.
        assert!(parse_session("\n\n").2.is_empty());
    }

    #[test]
    fn relative_paths_are_root_based() {
        let root = Path::new("/docs");
        let path = root.join("papers/report.pdf");
        assert_eq!(rel_path(root, &path).as_deref(), Some("papers/report.pdf"));
        assert_eq!(abs_path(root, "papers/report.pdf"), path);
        assert_eq!(rel_path(root, root), None);
        assert_eq!(rel_path(root, Path::new("/elsewhere/a.md")), None);
    }

    #[test]
    fn tool_default_is_select() {
        assert_eq!(Tool::default(), Tool::Select);
        assert_eq!(Tool::Highlight.label(), "Highlight");
    }

    #[test]
    fn screen_page_space_round_trip() {
        let origin = point(px(100.0), px(50.0));
        let scale = 2.0;
        let p = screen_to_page(origin, scale, point(px(200.0), px(150.0)));
        assert!((p[0] - 50.0).abs() < 1e-4 && (p[1] - 50.0).abs() < 1e-4);
        let screen = page_to_screen(origin, scale, &[10.0, 20.0, 110.0, 34.0]);
        assert!((screen.origin.x.as_f32() - 120.0).abs() < 1e-4);
        assert!((screen.origin.y.as_f32() - 90.0).abs() < 1e-4);
        assert!((screen.size.width.as_f32() - 200.0).abs() < 1e-4);
        assert!((screen.size.height.as_f32() - 28.0).abs() < 1e-4);
    }

    #[test]
    fn note_anchor_quad_is_square() {
        let quad = note_anchor_quad([10.0, 20.0]);
        assert_eq!(quad[0], [10.0, 20.0]);
        assert_eq!(quad[2], [10.0 + NOTE_ANCHOR, 20.0 + NOTE_ANCHOR]);
    }

    #[test]
    fn hex_rgb_parses_and_falls_back() {
        assert_eq!(hex_rgb("#ff0000"), (0xff, 0x00, 0x00));
        assert_eq!(hex_rgb("f6d448"), (0xf6, 0xd4, 0x48));
        assert_eq!(hex_rgb("nope"), (0xf6, 0xd4, 0x48));
    }

    #[test]
    fn selection_rect_normalizes_corners() {
        let sel = Selection {
            page: 0,
            start: [30.0, 10.0],
            current: [5.0, 40.0],
        };
        assert_eq!(sel.rect(), [5.0, 10.0, 30.0, 40.0]);
    }

    #[test]
    fn visible_page_window_includes_one_page_margin() {
        // Three 100 px rows, viewport over the middle row.
        assert_eq!(visible_pages(90.0, 120.0, 100.0, 3), vec![0, 1, 2]);
        // Viewport on the first page only plus its pre-render margin.
        assert_eq!(visible_pages(0.0, 100.0, 100.0, 4), vec![0, 1]);
        // Beyond the end.
        assert!(visible_pages(10_000.0, 100.0, 100.0, 3).is_empty());
        assert!(visible_pages(0.0, 100.0, 0.0, 3).is_empty());
    }

    #[test]
    fn permille_rounds_up_to_one() {
        assert_eq!(permille(0.0), 1);
        assert_eq!(permille(1.25), 1250);
        assert_eq!(permille(0.9996), 1000);
    }
}
