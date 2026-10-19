//! Documents view — left file tree, right multi-tab PDF reader.
//!
//! PDFs are read-only files under the core-managed root
//! (`kandrium_core::documents`); rendering and text extraction happen on a
//! worker thread per open document (`crate::pdf`). Highlights and sticky
//! notes are stored in a sidecar JSON next to each PDF and auto-saved after
//! a short debounce, like Notes.
//!
//! All annotation geometry is in *page view space* (points, top-left origin,
//! y down — hayro's render space); screen mapping is `× scale + page origin`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use egui::{Color32, Pos2, Rect, Vec2};
use kandrium_core::documents::{
    self, AnnotationDoc, AnnotationKind, DocAnnotation, DocEntry, new_annotation,
};

use crate::pdf::{self, PageText, PdfWorker};
use crate::theme::{self, Press, Tokens};
use crate::widgets::tabs::TAB_H;

const AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const TREE_DEFAULT_W: f32 = 240.0;
const ROW_H: f32 = 26.0;
const INDENT: f32 = 14.0;
/// Margin between pages and around the page area.
const PAGE_MARGIN: f32 = 8.0;
/// Right gutter so the floating scrollbar never overlaps page content.
const VIEWER_RIGHT_INSET: f32 = 7.0;
/// Zoom step and clamp (multiplier over fit-width).
const ZOOM_STEP: f32 = 1.25;
const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 4.0;
/// Keep at most this many page textures per document (LRU-evicted).
const TEXTURE_CACHE_CAP: usize = 24;
/// Sticky-note anchor size in points.
const NOTE_ANCHOR: f32 = 20.0;
/// Highlight fill opacity (0–255).
const HIGHLIGHT_ALPHA: u8 = 90;

/// Highlighter colors offered in the popover (PdfCraft's default set).
pub const NOTE_COLORS: [&str; 4] = ["#f6d448", "#a5d6a7", "#8ab4f8", "#f6a6c1"];

/// Which pointer tool is active in the viewer.
#[derive(Clone, Copy, PartialEq)]
pub enum Tool {
    /// Plain select (drag = selection visual only, v1).
    Select,
    Highlight,
    Note,
}

/// LRU stamp for the texture cache.
static FRAME_STAMP: AtomicU64 = AtomicU64::new(0);

fn frame_now() -> u64 {
    FRAME_STAMP.fetch_add(1, Ordering::Relaxed)
}

/// Parse `#rrggbb` into RGB, tolerating garbage (falls back to yellow).
fn hex_rgb(hex: &str) -> (u8, u8, u8) {
    let t = hex.trim().trim_start_matches('#');
    if let Ok(v) = u32::from_str_radix(t, 16)
        && t.len() == 6
    {
        return (
            ((v >> 16) & 0xff) as u8,
            ((v >> 8) & 0xff) as u8,
            (v & 0xff) as u8,
        );
    }
    (0xf6, 0xd4, 0x48)
}

fn color_with_alpha(hex: &str, alpha: u8) -> Color32 {
    let (r, g, b) = hex_rgb(hex);
    Color32::from_rgba_unmultiplied(r, g, b, alpha)
}

/// A drag-in-progress selection, in page view space (points).
#[derive(Debug, Clone, Copy)]
struct Selection {
    page: usize,
    start: [f32; 2],
    current: [f32; 2],
}

impl Selection {
    fn rect(&self) -> [f32; 4] {
        [
            self.start[0].min(self.current[0]),
            self.start[1].min(self.current[1]),
            self.start[0].max(self.current[0]),
            self.start[1].max(self.current[1]),
        ]
    }
}

/// One open document tab (UI side).
pub struct OpenDoc {
    path: PathBuf,
    worker: Option<PdfWorker>,
    events: std::sync::mpsc::Receiver<pdf::Event>,
    page_count: usize,
    /// Load/render error (sticky once shown).
    error: Option<String>,
    /// Zoom multiplier over fit-width (1.0 = fit width).
    zoom: f32,
    /// Rendered page textures keyed by `(page, scale in ‰)`.
    textures: HashMap<(usize, u32), egui::TextureHandle>,
    texture_stamps: HashMap<(usize, u32), u64>,
    /// Natural page size in points (from the first render).
    page_size: Option<(f32, f32)>,
    page_texts: HashMap<usize, PageText>,
    text_requested: HashSet<usize>,
    /// Renders already requested, keyed `(page, scale ‰)`.
    pending: HashSet<(usize, u32)>,
    annotations: Vec<DocAnnotation>,
    /// Unsaved annotation changes (debounced save).
    dirty_since: Option<Instant>,
    /// Selection drag in progress.
    selection: Option<Selection>,
    /// Annotation id currently showing the edit popover.
    popover: Option<String>,
    /// Toolbar "next page" request, consumed by the page loop.
    scroll_to_page: Option<usize>,
    /// Page currently on screen (for the toolbar label).
    visible_page: usize,
}

/// Screen point → page view space (points), given the page's screen rect
/// and render scale.
fn screen_to_page(page_rect: Rect, scale: f32, pos: Pos2) -> [f32; 2] {
    [
        (pos.x - page_rect.left()) / scale,
        (pos.y - page_rect.top()) / scale,
    ]
}

/// Page view space rect → screen rect.
fn page_to_screen(page_rect: Rect, scale: f32, r: &[f32; 4]) -> Rect {
    Rect::from_min_max(
        Pos2::new(
            page_rect.left() + r[0] * scale,
            page_rect.top() + r[1] * scale,
        ),
        Pos2::new(
            page_rect.left() + r[2] * scale,
            page_rect.top() + r[3] * scale,
        ),
    )
}

/// Page view space point → screen point.
fn point_to_screen(page_rect: Rect, scale: f32, p: [f32; 2]) -> Pos2 {
    Pos2::new(
        page_rect.left() + p[0] * scale,
        page_rect.top() + p[1] * scale,
    )
}

/// Small anchor quad for a note placed at `p` (points, y down).
fn note_anchor_quad(p: [f32; 2]) -> [[f32; 2]; 4] {
    [
        p,
        [p[0] + NOTE_ANCHOR, p[1]],
        [p[0] + NOTE_ANCHOR, p[1] + NOTE_ANCHOR],
        [p[0], p[1] + NOTE_ANCHOR],
    ]
}

pub struct DocumentsState {
    /// egui context, captured each frame (worker repaint + texture loads).
    ui_ctx: Option<egui::Context>,
    root: PathBuf,
    tree: Vec<DocEntry>,
    expanded: HashSet<PathBuf>,
    selected_dir: PathBuf,
    renaming: Option<(PathBuf, String)>,
    open: Vec<OpenDoc>,
    active: Option<usize>,
    tool: Tool,
    error: Option<String>,
}

#[derive(Default)]
pub struct DocumentsActions {
    /// A tree row asked to delete this path; app routes it to the
    /// confirm modal.
    pub request_delete: Option<PathBuf>,
}

impl DocumentsState {
    pub fn new() -> Self {
        match documents::ensure_root() {
            Ok(root) => {
                let mut state = Self {
                    ui_ctx: None,
                    selected_dir: root.clone(),
                    root,
                    tree: Vec::new(),
                    expanded: HashSet::new(),
                    renaming: None,
                    open: Vec::new(),
                    active: None,
                    tool: Tool::Select,
                    error: None,
                };
                state.rescan();
                state
            }
            Err(e) => Self {
                ui_ctx: None,
                root: PathBuf::new(),
                selected_dir: PathBuf::new(),
                tree: Vec::new(),
                expanded: HashSet::new(),
                renaming: None,
                open: Vec::new(),
                active: None,
                tool: Tool::Select,
                error: Some(format!("Cannot open documents folder: {e}")),
            },
        }
    }

    /// Documents root (for delete/rename guards in core calls).
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn rescan(&mut self) {
        match documents::scan_tree(&self.root) {
            Ok(tree) => {
                self.tree = tree;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("Cannot read documents folder: {e}")),
        }
    }

    /// Open a PDF in a tab (or activate its existing tab). The worker reads
    /// and parses the file off-thread; annotations load lazily from the
    /// sidecar once that open is confirmed.
    pub fn open_file(&mut self, path: PathBuf) {
        if let Some(idx) = self.open.iter().position(|d| d.path == path) {
            self.active = Some(idx);
            return;
        }
        let repaint = self.ui_ctx.clone().unwrap_or_default();
        let (worker, events) = PdfWorker::open(path.clone(), repaint);
        self.open.push(OpenDoc {
            path,
            worker: Some(worker),
            events,
            page_count: 0,
            error: None,
            zoom: 1.0,
            textures: HashMap::new(),
            texture_stamps: HashMap::new(),
            page_size: None,
            page_texts: HashMap::new(),
            text_requested: HashSet::new(),
            pending: HashSet::new(),
            annotations: Vec::new(),
            dirty_since: None,
            selection: None,
            popover: None,
            scroll_to_page: None,
            visible_page: 0,
        });
        let idx = self.open.len() - 1;
        self.active = Some(idx);
        self.load_annotations(idx);
    }

    fn load_annotations(&mut self, idx: usize) {
        let Some(path) = self.open.get(idx).map(|d| d.path.clone()) else {
            return;
        };
        match documents::load_annotations(&path, &self.root) {
            Ok(doc) => {
                if let Some(d) = self.open.get_mut(idx) {
                    d.annotations = doc.annotations;
                }
            }
            Err(e) => self.error = Some(format!("Cannot read annotations: {e}")),
        }
    }

    fn flush(&mut self, idx: usize) {
        let Some(doc) = self.open.get_mut(idx) else {
            return;
        };
        if doc.dirty_since.is_none() {
            return;
        }
        let path = doc.path.clone();
        let annotations = AnnotationDoc {
            annotations: doc.annotations.clone(),
            ..AnnotationDoc::default()
        };
        match documents::save_annotations(&path, &self.root, &annotations) {
            Ok(()) => {
                if let Some(doc) = self.open.get_mut(idx) {
                    doc.dirty_since = None;
                }
            }
            Err(e) => {
                if let Some(doc) = self.open.get_mut(idx) {
                    doc.dirty_since = None;
                }
                self.error = Some(format!("Cannot save annotations: {e}"));
            }
        }
    }

    /// Flush every dirty tab (section switch, app exit).
    pub fn flush_all(&mut self) {
        for idx in 0..self.open.len() {
            self.flush(idx);
        }
    }

    /// Flush tabs whose debounce elapsed. Returns true while any tab is
    /// still dirty so the caller can keep scheduling repaints.
    pub fn tick_autosave(&mut self) -> bool {
        let due: Vec<usize> = self
            .open
            .iter()
            .enumerate()
            .filter_map(|(idx, d)| {
                d.dirty_since
                    .filter(|since| Instant::now().duration_since(*since) >= AUTOSAVE_DEBOUNCE)
                    .map(|_| idx)
            })
            .collect();
        for idx in due {
            self.flush(idx);
        }
        self.open.iter().any(|d| d.dirty_since.is_some())
    }

    fn close_tab(&mut self, idx: usize) {
        self.flush(idx);
        self.remove_tab(idx);
    }

    /// Drop a tab and keep the active index valid, without flushing. Used
    /// when the backing file is gone, where a save would only leave an
    /// orphan sidecar (or fail) behind.
    fn remove_tab(&mut self, idx: usize) {
        self.open.remove(idx);
        self.active = match self.active {
            _ if self.open.is_empty() => None,
            Some(a) if a >= idx => Some(a.saturating_sub(1).min(self.open.len() - 1)),
            other => other,
        };
    }

    /// Persisted session: open tabs as root-relative paths + active index.
    pub fn rel_paths(&self) -> Vec<String> {
        self.open
            .iter()
            .filter_map(|d| {
                d.path
                    .strip_prefix(&self.root)
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
            })
            .collect()
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    /// Restore tabs from a previous session; vanished files are dropped.
    pub fn open_relative(&mut self, rel: &[String], active: Option<usize>) {
        for r in rel {
            let path = self.root.join(r);
            if path.is_file() {
                self.open_file(path);
            }
        }
        self.active = active
            .filter(|&i| i < self.open.len())
            .or(if self.open.is_empty() { None } else { Some(0) });
    }

    /// After a confirmed delete: drop tabs living under `path` and rescan.
    /// The paths are already deleted, so the tabs are discarded without
    /// flushing.
    pub fn on_deleted(&mut self, path: &Path) {
        let doomed: Vec<usize> = self
            .open
            .iter()
            .enumerate()
            .filter_map(|(idx, d)| d.path.starts_with(path).then_some(idx))
            .collect();
        for idx in doomed.into_iter().rev() {
            self.remove_tab(idx);
        }
        self.rescan();
    }

    /// After a rename: repoint open tabs that lived under the old path.
    fn on_renamed(&mut self, old: &Path, new: &Path) {
        for doc in &mut self.open {
            if doc.path == old {
                doc.path = new.to_path_buf();
            } else if let Ok(suffix) = doc.path.strip_prefix(old) {
                doc.path = new.join(suffix);
            }
        }
        if self.selected_dir.starts_with(old) {
            self.selected_dir = new.to_path_buf();
        }
        self.expanded = self
            .expanded
            .iter()
            .map(|p| {
                if let Ok(suffix) = p.strip_prefix(old) {
                    new.join(suffix)
                } else {
                    p.clone()
                }
            })
            .collect();
        self.rescan();
    }

    /// Drain worker events for every open document; called once per frame
    /// before drawing so textures/text are fresh.
    fn pump_events(&mut self, ctx: &egui::Context) {
        for idx in 0..self.open.len() {
            while let Ok(event) = self.open[idx].events.try_recv() {
                self.apply_event(idx, event, ctx);
            }
        }
    }

    fn apply_event(&mut self, idx: usize, event: pdf::Event, ctx: &egui::Context) {
        let Some(doc) = self.open.get_mut(idx) else {
            return;
        };
        match event {
            pdf::Event::Opened { page_count } => {
                doc.page_count = page_count;
            }
            pdf::Event::OpenFailed { message } => {
                doc.error = Some(message);
                doc.worker = None;
            }
            pdf::Event::Rendered {
                page,
                scale,
                width,
                height,
                pixels,
            } => {
                doc.pending.remove(&(page, permille(scale)));
                if doc.page_size.is_none() && scale > 0.0 && width > 0 {
                    doc.page_size = Some((width as f32 / scale, height as f32 / scale));
                }
                let name = format!("doc-page-{}-{page}", doc.path.display());
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [width as usize, height as usize],
                    &pixels,
                );
                let key = (page, permille(scale));
                let handle = ctx.load_texture(&name, image, Default::default());
                doc.textures.insert(key, handle);
                doc.texture_stamps.insert(key, frame_now());

                // LRU-evict when over cap.
                while doc.textures.len() > TEXTURE_CACHE_CAP {
                    let oldest = doc
                        .texture_stamps
                        .iter()
                        .min_by_key(|(_, stamp)| **stamp)
                        .map(|(k, _)| *k);
                    match oldest {
                        Some(k) => {
                            doc.textures.remove(&k);
                            doc.texture_stamps.remove(&k);
                        }
                        None => break,
                    }
                }
            }
            pdf::Event::Text { page, text } => {
                doc.page_texts.insert(page, text);
            }
            pdf::Event::Failed { page, message } => {
                let _ = page;
                if doc.error.is_none() {
                    doc.error = Some(message);
                }
            }
        }
    }
}

fn permille(scale: f32) -> u32 {
    (scale * 1000.0).round().max(1.0) as u32
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

pub fn show(ui: &mut egui::Ui, state: &mut DocumentsState, dark: bool) -> DocumentsActions {
    let mut actions = DocumentsActions::default();
    let t = Tokens::get(ui.ctx());
    let _ = dark;

    state.ui_ctx = Some(ui.ctx().clone());
    state.pump_events(ui.ctx());

    egui::Panel::left("documents-tree")
        .resizable(true)
        .default_size(TREE_DEFAULT_W)
        .min_size(160.0)
        .max_size(480.0)
        .frame(egui::Frame::new().fill(t.panel))
        .show(ui, |ui| {
            tree_panel(ui, state, &mut actions);
        });

    viewer_panel(ui, state);
    actions
}

// -- left panel: toolbar + tree ---------------------------------------------

fn tree_panel(ui: &mut egui::Ui, state: &mut DocumentsState, actions: &mut DocumentsActions) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("PDFs")
                .text_style(theme::small_strong())
                .color(Tokens::get(ui.ctx()).text_muted),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::icon_button(ui, "rotate-cw", 26.0, false, "Refresh").clicked() {
                state.rescan();
            }
        });
    });
    ui.add_space(4.0);

    if let Some(err) = state.error.clone() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.colored_label(ui.visuals().error_fg_color, err);
        });
    }

    egui::ScrollArea::vertical()
        .id_salt("documents-tree-scroll")
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            // Clone for display: rename/delete handlers trigger rescan(),
            // which rewrites `state.tree` mid-render.
            let entries = state.tree.clone();
            tree_entries(ui, state, &entries, 0, actions);
        });
}

fn tree_entries(
    ui: &mut egui::Ui,
    state: &mut DocumentsState,
    entries: &[DocEntry],
    depth: usize,
    actions: &mut DocumentsActions,
) {
    for entry in entries {
        tree_row(ui, state, entry, depth, actions);
        if entry.is_dir
            && state.expanded.contains(&entry.path)
            && let Some(children) = &entry.children
        {
            tree_entries(ui, state, children, depth + 1, actions);
        }
    }
}

fn tree_row(
    ui: &mut egui::Ui,
    state: &mut DocumentsState,
    entry: &DocEntry,
    depth: usize,
    actions: &mut DocumentsActions,
) {
    let t = Tokens::get(ui.ctx());
    ui.push_id(("doc-row", entry.path.as_os_str()), |ui| {
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ROW_H),
            egui::Sense::click(),
        );
        let press = Press::track(ui, &resp);
        let is_active = state
            .active
            .is_some_and(|a| state.open.get(a).is_some_and(|d| d.path == entry.path));
        press.wash(ui, rect, egui::CornerRadius::same(t.radius), is_active);

        let mut x = rect.left() + 8.0 + depth as f32 * INDENT;
        let cy = rect.center().y + press.offset().y;

        if entry.is_dir {
            let chevron = if state.expanded.contains(&entry.path) {
                "chevron-down"
            } else {
                "chevron-right"
            };
            crate::icons::paint(
                ui,
                Rect::from_center_size(egui::pos2(x + 6.0, cy), Vec2::splat(12.0)),
                chevron,
                12.0,
                t.text_faint,
            );
        }
        x += 14.0;

        let icon = if entry.is_dir { "folder" } else { "file" };
        crate::icons::paint(
            ui,
            Rect::from_center_size(egui::pos2(x + 7.0, cy), Vec2::splat(14.0)),
            icon,
            14.0,
            t.icon,
        );
        x += 18.0;

        // Inline rename swaps the label for a text field.
        let renaming_this = state
            .renaming
            .as_ref()
            .is_some_and(|(p, _)| p == &entry.path);
        if renaming_this {
            let (path, mut name) = state.renaming.take().unwrap();
            let resp = ui.put(
                Rect::from_min_size(
                    egui::pos2(x + 4.0, rect.top() + 3.0),
                    egui::vec2(160.0, ROW_H - 6.0),
                ),
                egui::TextEdit::singleline(&mut name).desired_width(160.0),
            );
            resp.request_focus();
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if (enter || resp.lost_focus()) && !escape {
                let trimmed = name.trim();
                if !trimmed.is_empty() && trimmed != entry.name {
                    match documents::rename_path(&path, &state.root, trimmed) {
                        Ok(new_path) => state.on_renamed(&path, &new_path),
                        Err(e) => state.error = Some(format!("Cannot rename: {e}")),
                    }
                }
            } else if !escape {
                state.renaming = Some((path, name));
            }
        } else {
            let galley = ui.painter().layout_no_wrap(
                entry.name.clone(),
                theme::font_id(ui, egui::TextStyle::Small),
                if is_active { t.accent_text } else { t.text },
            );
            ui.painter().galley(
                egui::pos2(x + 4.0, cy - galley.size().y * 0.5),
                galley,
                t.text,
            );
        }

        if resp.clicked() {
            if entry.is_dir {
                state.selected_dir = entry.path.clone();
                if !state.expanded.insert(entry.path.clone()) {
                    state.expanded.remove(&entry.path);
                }
            } else {
                state.open_file(entry.path.clone());
            }
        }

        resp.context_menu(|ui| {
            if !entry.is_dir && theme::menu_item(ui, "eye", "Open", false).clicked() {
                ui.close();
                state.open_file(entry.path.clone());
            }
            if theme::menu_item(ui, "pencil", "Rename", false).clicked() {
                ui.close();
                state.renaming = Some((entry.path.clone(), entry.name.clone()));
            }
            if theme::menu_item(ui, "trash-2", "Delete", true).clicked() {
                ui.close();
                actions.request_delete = Some(entry.path.clone());
            }
        });
    });
}

// -- right panel: tabs + toolbar + page area ---------------------------------

fn viewer_panel(ui: &mut egui::Ui, state: &mut DocumentsState) {
    if state.open.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.weak("Select a PDF to open");
        });
        return;
    }

    tab_strip(ui, state);
    ui.add_space(6.0);

    if state.open.is_empty() {
        return;
    }
    let idx = state.active.unwrap_or(0).min(state.open.len() - 1);
    state.active = Some(idx);

    viewer(ui, state, idx);
}

fn tab_strip(ui: &mut egui::Ui, state: &mut DocumentsState) {
    ui.horizontal(|ui| {
        let strip_w = ui.available_width().max(60.0);
        ui.allocate_ui(egui::vec2(strip_w, TAB_H), |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("documents-tabs-scroll")
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let mut close_idx: Option<usize> = None;
                        for (idx, doc) in state.open.iter().enumerate() {
                            let name = doc
                                .path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "document".to_string());
                            let resp = crate::widgets::tabs::tab_widget(
                                ui,
                                &name,
                                state.active == Some(idx),
                                doc.dirty_since.is_some(),
                                ("doc-tab", doc.path.as_os_str()),
                            );
                            if resp.clicked() {
                                state.active = Some(idx);
                            }
                            if resp.middle_clicked() || resp.close_clicked {
                                close_idx = Some(idx);
                            }
                        }
                        if let Some(idx) = close_idx {
                            state.close_tab(idx);
                        }
                    });
                });
        });
    });
}

/// Toolbar: page nav, zoom, and the tool toggles.
fn viewer_toolbar(ui: &mut egui::Ui, state: &mut DocumentsState, idx: usize) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        let page_count = state.open[idx].page_count;
        let page = state.open[idx]
            .visible_page
            .min(page_count.saturating_sub(1));

        // Page navigation (continuous scroll; buttons jump). No-ops until the
        // document reports its page count.
        if theme::icon_button(ui, "chevron-up", 26.0, false, "Previous page").clicked()
            && page_count > 0
        {
            state.open[idx].scroll_to_page = Some(page.saturating_sub(1));
        }
        if theme::icon_button(ui, "chevron-down", 26.0, false, "Next page").clicked()
            && page_count > 0
        {
            state.open[idx].scroll_to_page = Some((page + 1).min(page_count - 1));
        }
        ui.label(
            egui::RichText::new(if page_count > 0 {
                format!("{} / {}", page + 1, page_count)
            } else {
                "…".to_string()
            })
            .text_style(egui::TextStyle::Small)
            .color(t.text_muted),
        );

        ui.separator();

        // Zoom controls.
        if theme::icon_button(ui, "minus", 26.0, false, "Zoom out").clicked() {
            let z = state.open[idx].zoom / ZOOM_STEP;
            state.open[idx].zoom = z.clamp(ZOOM_MIN, ZOOM_MAX);
        }
        let pct = format!("{}%", (state.open[idx].zoom * 100.0).round() as i32);
        let pct_resp = ui
            .label(
                egui::RichText::new(pct)
                    .text_style(egui::TextStyle::Small)
                    .color(t.text_muted),
            )
            .interact(egui::Sense::click());
        if pct_resp.clicked() {
            state.open[idx].zoom = 1.0;
        }
        if theme::icon_button(ui, "plus", 26.0, false, "Zoom in").clicked() {
            let z = state.open[idx].zoom * ZOOM_STEP;
            state.open[idx].zoom = z.clamp(ZOOM_MIN, ZOOM_MAX);
        }

        ui.separator();

        // Tools.
        let tool = state.tool;
        if theme::icon_button(ui, "square", 26.0, tool == Tool::Select, "Select").clicked() {
            state.tool = Tool::Select;
        }
        if theme::icon_button(
            ui,
            "highlighter",
            26.0,
            tool == Tool::Highlight,
            "Highlight",
        )
        .clicked()
        {
            state.tool = Tool::Highlight;
        }
        if theme::icon_button(ui, "sticky-note", 26.0, tool == Tool::Note, "Sticky note").clicked()
        {
            state.tool = Tool::Note;
        }
    });
    ui.add_space(2.0);
}

/// Render the active document's pages, selection, annotations, and popover.
fn viewer(ui: &mut egui::Ui, state: &mut DocumentsState, idx: usize) {
    let t = Tokens::get(ui.ctx());
    viewer_toolbar(ui, state, idx);

    // Load-error / not-yet-parsed states.
    if let Some(err) = state.open[idx].error.clone() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.colored_label(ui.visuals().error_fg_color, err);
        });
        return;
    }
    if state.open[idx].page_count == 0 {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.weak("Loading…");
        });
        ui.ctx().request_repaint();
        return;
    }

    // Page geometry: fit-width scale from the natural page size and the
    // current zoom multiplier.
    let avail_w = (ui.available_width() - VIEWER_RIGHT_INSET - PAGE_MARGIN * 2.0).max(80.0);
    let (pw, ph) = state.open[idx].page_size.unwrap_or((612.0, 792.0));
    let fit = avail_w / pw;
    let base_scale = fit.max(0.01);

    let mut annotations_changed = false;
    let mut popover_anchor: Option<Pos2> = None;

    egui::ScrollArea::vertical()
        .id_salt(("doc-pages", state.open[idx].path.as_os_str()))
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            for page in 0..state.open[idx].page_count {
                let zoom = state.open[idx].zoom;
                let scale = base_scale * zoom;
                let page_w = pw * scale;
                let page_h = ph * scale;

                let (rect, resp) = ui.allocate_exact_size(
                    Vec2::new(
                        (page_w + PAGE_MARGIN * 2.0).max(ui.available_width()),
                        page_h + PAGE_MARGIN * 2.0,
                    ),
                    egui::Sense::click_and_drag(),
                );
                let page_rect = Rect::from_min_size(
                    Pos2::new(rect.left() + PAGE_MARGIN, rect.top() + PAGE_MARGIN),
                    Vec2::new(page_w, page_h),
                );

                // Track the on-screen page for the toolbar label.
                if ui.clip_rect().intersects(rect) {
                    state.open[idx].visible_page = page;
                }

                // Page background + hairline border.
                ui.painter().rect_filled(page_rect, 2.0, Color32::WHITE);
                ui.painter().rect_stroke(
                    page_rect,
                    2.0,
                    egui::Stroke::new(1.0, t.border),
                    egui::StrokeKind::Inside,
                );

                // Request render + text extraction for visible pages (±1).
                let key = (page, permille(scale));
                let visible = ui
                    .clip_rect()
                    .intersects(rect.expand2(Vec2::new(0.0, page_h * 0.75)));
                if visible {
                    if !state.open[idx].textures.contains_key(&key)
                        && state.open[idx].pending.insert(key)
                        && let Some(worker) = &state.open[idx].worker
                    {
                        worker.send(pdf::Request::Render { page, scale });
                    }
                    if !state.open[idx].page_texts.contains_key(&page)
                        && state.open[idx].text_requested.insert(page)
                        && let Some(worker) = &state.open[idx].worker
                    {
                        worker.send(pdf::Request::Text { page });
                    }
                }

                // Paint the texture when ready, placeholder otherwise.
                let tex_id = state.open[idx].textures.get(&key).map(|h| h.id());
                if let Some(tex_id) = tex_id {
                    state.open[idx].texture_stamps.insert(key, frame_now());
                    ui.painter().image(
                        tex_id,
                        page_rect,
                        Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    ui.painter().text(
                        page_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Rendering…",
                        egui::TextStyle::Small.resolve(ui.style()),
                        t.text_faint,
                    );
                }

                // Disjoint field borrows for the interaction + overlay work.
                let doc = &mut state.open[idx];
                let OpenDoc {
                    page_texts,
                    annotations,
                    selection,
                    popover,
                    ..
                } = doc;
                let page_text = page_texts.get(&page);

                // Live selection overlay while dragging.
                if let Some(sel) = *selection
                    && sel.page == page
                {
                    let screen = page_to_screen(page_rect, scale, &sel.rect());
                    ui.painter()
                        .rect_filled(screen, 0.0, t.accent.gamma_multiply(0.25));
                }

                // Annotation overlays (highlights + note anchors).
                draw_annotations(
                    ui,
                    page_rect,
                    scale,
                    annotations,
                    page,
                    popover,
                    &mut popover_anchor,
                );

                // Pointer interactions.
                if page_text.is_some() {
                    let tool = state.tool;
                    match tool {
                        Tool::Select | Tool::Highlight => {
                            if resp.drag_started()
                                && let Some(pos) = resp.interact_pointer_pos()
                            {
                                *selection = Some(Selection {
                                    page,
                                    start: screen_to_page(page_rect, scale, pos),
                                    current: screen_to_page(page_rect, scale, pos),
                                });
                            }
                            if resp.dragged()
                                && let Some(pos) = ui.input(|i| i.pointer.latest_pos())
                                && let Some(sel) = selection.as_mut()
                                && sel.page == page
                            {
                                sel.current = screen_to_page(page_rect, scale, pos);
                            }
                            if resp.drag_stopped()
                                && let Some(sel) = selection.take()
                            {
                                // Only the highlight tool turns a drag into an
                                // annotation; the select tool is visual-only.
                                if tool == Tool::Highlight && sel.page == page {
                                    let (quads, memo) = page_text
                                        .map(|t| {
                                            (
                                                t.selection_quads(&sel.rect()),
                                                t.selection_text(&sel.rect()),
                                            )
                                        })
                                        .unwrap_or_default();
                                    annotations_changed |=
                                        finalize_highlight(annotations, sel, quads, memo);
                                } else if sel.page != page {
                                    *selection = Some(sel);
                                }
                            }
                        }
                        Tool::Note => {
                            if resp.clicked()
                                && let Some(pos) = resp.interact_pointer_pos()
                                && page_rect.contains(pos)
                            {
                                let mut ann = new_annotation(
                                    AnnotationKind::Note,
                                    page,
                                    vec![],
                                    NOTE_COLORS[3],
                                );
                                ann.quads =
                                    vec![note_anchor_quad(screen_to_page(page_rect, scale, pos))];
                                let id = ann.id.clone();
                                annotations.push(ann);
                                annotations_changed = true;
                                *popover = Some(id);
                            }
                        }
                    }
                }

                // Scroll-to-page request from the toolbar.
                if state.open[idx].scroll_to_page == Some(page) {
                    ui.scroll_to_rect(rect, Some(egui::Align::Min));
                    state.open[idx].scroll_to_page = None;
                }
            }
            ui.add_space(PAGE_MARGIN);
        });

    // Note/highlight edit popover (drawn above the scroll area).
    if let Some(pop_id) = state.open[idx].popover.clone() {
        let known = state.open[idx].annotations.iter().any(|a| a.id == pop_id);
        if known {
            if let Some(anchor) = popover_anchor {
                let doc = &mut state.open[idx];
                let OpenDoc {
                    annotations,
                    popover,
                    ..
                } = doc;
                annotations_changed |=
                    draw_popover(ui, anchor, pop_id.as_str(), annotations, popover);
            }
        } else {
            state.open[idx].popover = None; // stale id — close
        }
    }

    if annotations_changed {
        state.open[idx].dirty_since = Some(Instant::now());
    }
    // Open the popover on a freshly created highlight.
    PENDING_POPOVER.with(|p| {
        if let Some(id) = p.borrow_mut().take() {
            state.open[idx].popover = Some(id);
        }
    });
}

/// Turn a finished highlight-tool drag into a Highlight annotation over the
/// intersected glyphs (one quad per line), with the selected text as its
/// memo. Returns true when created.
fn finalize_highlight(
    annotations: &mut Vec<DocAnnotation>,
    sel: Selection,
    quads: Vec<[[f32; 2]; 4]>,
    memo: String,
) -> bool {
    // Ignore tiny drags (plain clicks).
    let r = sel.rect();
    if quads.is_empty() || ((r[2] - r[0]) < 2.0 && (r[3] - r[1]) < 2.0) {
        return false;
    }
    let mut ann = new_annotation(AnnotationKind::Highlight, sel.page, quads, NOTE_COLORS[0]);
    ann.text = memo;
    let id = ann.id.clone();
    annotations.push(ann);
    PENDING_POPOVER.with(|p| p.borrow_mut().replace(id));
    true
}

thread_local! {
    /// One-shot hand-off from `finalize_highlight` (inside the page loop) to
    /// the popover bookkeeping afterwards: the id of a freshly created
    /// annotation whose popover should open.
    static PENDING_POPOVER: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Paint annotations for one page and register click interactions. Sets
/// `popover_anchor` when the open popover belongs to this page.
fn draw_annotations(
    ui: &mut egui::Ui,
    page_rect: Rect,
    scale: f32,
    annotations: &[DocAnnotation],
    page: usize,
    popover: &mut Option<String>,
    popover_anchor: &mut Option<Pos2>,
) {
    for ann in annotations.iter().filter(|a| a.page == page) {
        // Highlight quads: translucent fills. Note: small card behind an icon.
        for quad in &ann.quads {
            let screen: Vec<Pos2> = quad
                .iter()
                .map(|p| point_to_screen(page_rect, scale, *p))
                .collect();
            match ann.kind {
                AnnotationKind::Highlight => {
                    ui.painter().add(egui::Shape::convex_polygon(
                        screen,
                        color_with_alpha(&ann.color, HIGHLIGHT_ALPHA),
                        egui::Stroke::NONE,
                    ));
                }
                AnnotationKind::Note => {
                    let rect = Rect::from_points(&screen);
                    let t = Tokens::get(ui.ctx());
                    ui.painter()
                        .rect_filled(rect, 3.0, color_with_alpha(&ann.color, 220));
                    ui.painter().rect_stroke(
                        rect,
                        3.0,
                        egui::Stroke::new(1.0, t.border),
                        egui::StrokeKind::Inside,
                    );
                    crate::icons::paint(
                        ui,
                        Rect::from_center_size(rect.center(), Vec2::splat(12.0)),
                        "sticky-note",
                        12.0,
                        t.text,
                    );
                }
            }
        }

        // Interaction: bbox click opens/closes the popover.
        let mut bbox: Option<Rect> = None;
        for quad in &ann.quads {
            let screen: Vec<Pos2> = quad
                .iter()
                .map(|p| point_to_screen(page_rect, scale, *p))
                .collect();
            let rect = Rect::from_points(&screen);
            bbox = Some(match bbox {
                Some(b) => b.union(rect),
                None => rect,
            });
        }
        if let Some(bbox) = bbox {
            let resp = ui.interact(
                bbox.expand2(Vec2::splat(2.0)),
                egui::Id::new(("doc-ann", ann.id.as_str())),
                egui::Sense::click(),
            );
            if resp.clicked()
                && let Some(pos) = resp.interact_pointer_pos()
            {
                // Hit-test per quad, not against the annotation's union box,
                // so the gaps between merged line boxes stay click-through.
                let hit = ann.quads.iter().any(|q| {
                    let screen: Vec<Pos2> = q
                        .iter()
                        .map(|p| point_to_screen(page_rect, scale, *p))
                        .collect();
                    Rect::from_points(&screen).contains(pos)
                });
                if hit {
                    let toggle = popover.as_ref() == Some(&ann.id);
                    *popover = if toggle { None } else { Some(ann.id.clone()) };
                }
            }
            if popover.as_deref() == Some(ann.id.as_str()) {
                *popover_anchor = Some(bbox.left_top());
            }
        }
    }
}

/// The small editor popover for a note/highlight: memo text, color swatches,
/// delete. Returns true when anything changed.
fn draw_popover(
    ui: &mut egui::Ui,
    anchor: Pos2,
    id: &str,
    annotations: &mut Vec<DocAnnotation>,
    popover: &mut Option<String>,
) -> bool {
    let t = Tokens::get(ui.ctx());
    let mut changed = false;
    // Work by index: every access is a short borrow, so the delete branch
    // can remove the entry without fighting the editor's borrow.
    let pos = annotations.iter().position(|a| a.id == id);
    egui::Area::new(egui::Id::new(("doc-popover", id)))
        .fixed_pos(anchor)
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            let Some(pos) = pos else {
                return;
            };
            egui::Frame::new()
                .fill(t.card)
                .stroke(egui::Stroke::new(1.0, t.border))
                .corner_radius(egui::CornerRadius::same(t.radius))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_min_width(220.0);
                    let editor = egui::TextEdit::multiline(&mut annotations[pos].text)
                        .id_salt(("doc-popover-editor", id))
                        .desired_width(204.0)
                        .font(egui::TextStyle::Small)
                        .frame(egui::Frame::new());
                    if ui.add(editor).changed() {
                        annotations[pos].modified_at = now_unix();
                        changed = true;
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        for color in NOTE_COLORS {
                            let (rect, resp) =
                                ui.allocate_exact_size(Vec2::splat(16.0), egui::Sense::click());
                            let selected = annotations[pos].color.eq_ignore_ascii_case(color);
                            let (r, g, b) = hex_rgb(color);
                            let fill = if selected {
                                Color32::from_rgb(r, g, b)
                            } else {
                                color_with_alpha(color, 140)
                            };
                            ui.painter().rect_filled(rect, 4.0, fill);
                            if selected {
                                ui.painter().rect_stroke(
                                    rect,
                                    4.0,
                                    egui::Stroke::new(2.0, t.accent),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if resp.clicked() {
                                annotations[pos].color = color.to_string();
                                annotations[pos].modified_at = now_unix();
                                changed = true;
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::icon_button(ui, "trash-2", 24.0, false, "Delete").clicked() {
                                annotations.retain(|a| a.id != id);
                                *popover = None;
                                changed = true;
                            }
                            if theme::icon_button(ui, "x", 24.0, false, "Close").clicked() {
                                *popover = None;
                            }
                        });
                    });
                });
        });
    changed
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn shapes_text(output: &egui::FullOutput) -> String {
        output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) => Some(t.galley.job.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn test_state(root: &Path) -> DocumentsState {
        let mut state = DocumentsState {
            ui_ctx: None,
            root: root.to_path_buf(),
            selected_dir: root.to_path_buf(),
            tree: Vec::new(),
            expanded: HashSet::new(),
            renaming: None,
            open: Vec::new(),
            active: None,
            tool: Tool::Select,
            error: None,
        };
        state.rescan();
        state
    }

    #[test]
    fn screen_page_space_round_trip() {
        let page_rect = Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(400.0, 500.0));
        let scale = 2.0;
        let p = screen_to_page(page_rect, scale, egui::pos2(200.0, 150.0));
        assert!((p[0] - 50.0).abs() < 1e-4 && (p[1] - 50.0).abs() < 1e-4);
        let r = [10.0, 20.0, 110.0, 34.0];
        let screen = page_to_screen(page_rect, scale, &r);
        assert!((screen.left() - 120.0).abs() < 1e-4);
        assert!((screen.top() - 90.0).abs() < 1e-4);
        assert!((screen.width() - 200.0).abs() < 1e-4);
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

    /// Render the documents view with one tab open (a worker-less doc) and
    /// scan for egui duplicate-ID warnings.
    #[test]
    fn no_duplicate_id_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("documents");
        std::fs::create_dir_all(&root).unwrap();
        // Minimal fake PDF file; the worker is not started (worker: None).
        std::fs::write(root.join("a.pdf"), b"%PDF-fake").unwrap();
        std::fs::write(root.join("b.pdf"), b"%PDF-fake").unwrap();

        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, true);

        let mut state = test_state(&root);
        state.open_file(root.join("a.pdf"));
        // Worker-less stub so no thread is involved in the test.
        state.open[0].worker = None;
        state.open[0].page_count = 2;
        state.open[0].page_size = Some((612.0, 792.0));
        state.open_file(root.join("b.pdf"));
        state.open[1].worker = None;
        state.open[1].page_count = 2;
        state.open[1].page_size = Some((612.0, 792.0));
        state.active = Some(0);

        for _ in 0..2 {
            let mut output = ctx.run_ui(Default::default(), |ctx| {
                let mut ui = egui::Ui::new(
                    ctx.clone(),
                    egui::Id::new("root"),
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(1000.0, 700.0),
                    )),
                );
                show(&mut ui, &mut state, true);
            });
            let text = shapes_text(&output);
            assert!(
                !text.contains("use of"),
                "duplicate-ID warning visible:\n{text}"
            );
            output.textures_delta.clear();
        }
    }

    /// Tab bookkeeping: open/activate/close through the state API.
    #[test]
    fn tab_lifecycle() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("documents");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.pdf"), b"x").unwrap();
        std::fs::write(root.join("b.pdf"), b"x").unwrap();

        let mut state = test_state(&root);
        state.open_file(root.join("a.pdf"));
        state.open_file(root.join("b.pdf"));
        assert_eq!(state.open.len(), 2);
        assert_eq!(state.active, Some(1));

        // Re-open activates the existing tab.
        state.open_file(root.join("a.pdf"));
        assert_eq!(state.open.len(), 2);
        assert_eq!(state.active, Some(0));

        state.close_tab(0);
        assert_eq!(state.open.len(), 1);
        assert_eq!(state.active, Some(0));
        assert_eq!(state.open[0].path, root.join("b.pdf"));
    }
}
