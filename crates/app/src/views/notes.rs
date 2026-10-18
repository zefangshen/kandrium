//! Notes view — left file tree, right multi-tab markdown editor/preview.
//!
//! Notes are plain `.md` files under the core-managed root
//! (`kandrium_core::notes`). Edits auto-save after a short debounce; tabs
//! show a dirty dot until flushed. Preview renders the in-memory buffer, so
//! toggling never needs to touch disk.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::Rect;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use kandrium_core::notes::{self, NoteEntry};

use crate::theme::{self, Press, Tokens};
use crate::views::notes_highlight::MarkdownHighlighter;

const AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const TREE_DEFAULT_W: f32 = 240.0;
const ROW_H: f32 = 26.0;
const TAB_H: f32 = 30.0;
const INDENT: f32 = 14.0;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum NoteMode {
    Edit,
    Preview,
}

/// One open editor tab.
pub struct OpenNote {
    pub path: PathBuf,
    pub text: String,
    pub dirty_since: Option<Instant>,
    pub mode: NoteMode,
}

pub struct NotesState {
    root: PathBuf,
    tree: Vec<NoteEntry>,
    expanded: HashSet<PathBuf>,
    /// Directory that "new file / new folder" buttons create into.
    selected_dir: PathBuf,
    /// Inline-rename session: the path being renamed and the draft name.
    renaming: Option<(PathBuf, String)>,
    open: Vec<OpenNote>,
    active: Option<usize>,
    highlighter: Option<MarkdownHighlighter>,
    highlighter_dark: bool,
    preview_cache: CommonMarkCache,
    error: Option<String>,
}

#[derive(Default)]
pub struct NotesActions {
    /// A tree row asked to delete this path; app routes it to the
    /// confirm modal.
    pub request_delete: Option<PathBuf>,
}

impl NotesState {
    pub fn new() -> Self {
        match notes::ensure_root() {
            Ok(root) => {
                let mut state = Self {
                    selected_dir: root.clone(),
                    root,
                    tree: Vec::new(),
                    expanded: HashSet::new(),
                    renaming: None,
                    open: Vec::new(),
                    active: None,
                    highlighter: None,
                    highlighter_dark: true,
                    preview_cache: CommonMarkCache::default(),
                    error: None,
                };
                state.rescan();
                state
            }
            Err(e) => Self {
                root: PathBuf::new(),
                selected_dir: PathBuf::new(),
                tree: Vec::new(),
                expanded: HashSet::new(),
                renaming: None,
                open: Vec::new(),
                active: None,
                highlighter: None,
                highlighter_dark: true,
                preview_cache: CommonMarkCache::default(),
                error: Some(format!("Cannot open notes folder: {e}")),
            },
        }
    }

    /// Rebuild the highlighter when the app theme changes.
    fn set_dark(&mut self, dark: bool) {
        if self.highlighter.is_none() || self.highlighter_dark != dark {
            self.highlighter = Some(MarkdownHighlighter::new(dark));
            self.highlighter_dark = dark;
        }
    }

    /// Notes root (for delete/rename guards in core calls).
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn rescan(&mut self) {
        match notes::scan_tree(&self.root) {
            Ok(tree) => {
                self.tree = tree;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("Cannot read notes folder: {e}")),
        }
    }

    /// Open a file in a tab (or activate its existing tab).
    pub fn open_file(&mut self, path: PathBuf) {
        if let Some(idx) = self.open.iter().position(|n| n.path == path) {
            self.active = Some(idx);
            return;
        }
        match notes::read_note(&path, &self.root) {
            Ok(text) => {
                self.open.push(OpenNote {
                    path,
                    text,
                    dirty_since: None,
                    mode: NoteMode::Edit,
                });
                self.active = Some(self.open.len() - 1);
            }
            Err(e) => self.error = Some(format!("Cannot open note: {e}")),
        }
    }

    fn flush(&mut self, idx: usize) {
        let Some(note) = self.open.get_mut(idx) else {
            return;
        };
        if note.dirty_since.is_none() {
            return;
        }
        match notes::write_note(&note.path.clone(), &self.root, &note.text) {
            Ok(()) => note.dirty_since = None,
            Err(e) => {
                // Clear the debounce so a persistently failing write (e.g. a
                // read-only root) is not retried on every autosave tick. The
                // text stays in the tab; the next edit retries the save.
                note.dirty_since = None;
                self.error = Some(format!("Cannot save note: {e}"));
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
        let now = Instant::now();
        let due: Vec<usize> = self
            .open
            .iter()
            .enumerate()
            .filter_map(|(idx, n)| {
                n.dirty_since
                    .filter(|since| now.duration_since(*since) >= AUTOSAVE_DEBOUNCE)
                    .map(|_| idx)
            })
            .collect();
        for idx in due {
            self.flush(idx);
        }
        self.open.iter().any(|n| n.dirty_since.is_some())
    }

    fn close_tab(&mut self, idx: usize) {
        self.flush(idx);
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
            .filter_map(|n| {
                n.path
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
    pub fn on_deleted(&mut self, path: &Path) {
        let doomed: Vec<usize> = self
            .open
            .iter()
            .enumerate()
            .filter_map(|(idx, n)| n.path.starts_with(path).then_some(idx))
            .collect();
        for idx in doomed.into_iter().rev() {
            // Already deleted on disk — flush would recreate the file.
            self.open[idx].dirty_since = None;
            self.close_tab(idx);
        }
        self.rescan();
    }

    /// After a rename: repoint open tabs that lived under the old path.
    fn on_renamed(&mut self, old: &Path, new: &Path) {
        for note in &mut self.open {
            if note.path == old {
                note.path = new.to_path_buf();
            } else if let Ok(suffix) = note.path.strip_prefix(old) {
                note.path = new.join(suffix);
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
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

pub fn show(ui: &mut egui::Ui, state: &mut NotesState, dark: bool) -> NotesActions {
    state.set_dark(dark);
    let mut actions = NotesActions::default();
    let t = Tokens::get(ui.ctx());

    egui::Panel::left("notes-tree")
        .resizable(true)
        .default_size(TREE_DEFAULT_W)
        .min_size(160.0)
        .max_size(480.0)
        .frame(egui::Frame::new().fill(t.panel))
        .show(ui, |ui| {
            tree_panel(ui, state, &mut actions);
        });

    editor_panel(ui, state);
    actions
}

// -- left panel: toolbar + tree ---------------------------------------------

fn tree_panel(ui: &mut egui::Ui, state: &mut NotesState, actions: &mut NotesActions) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        if theme::icon_button(ui, "file-plus", 26.0, false, "New note").clicked() {
            match notes::create_note(&state.root, &state.selected_dir) {
                Ok(path) => {
                    state.rescan();
                    state.open_file(path);
                }
                Err(e) => state.error = Some(format!("Cannot create note: {e}")),
            }
        }
        if theme::icon_button(ui, "folder-plus", 26.0, false, "New folder").clicked() {
            match notes::create_folder(&state.root, &state.selected_dir) {
                Ok(_) => state.rescan(),
                Err(e) => state.error = Some(format!("Cannot create folder: {e}")),
            }
        }
        if theme::icon_button(ui, "rotate-cw", 26.0, false, "Refresh").clicked() {
            state.rescan();
        }
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
        .id_salt("notes-tree-scroll")
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
    state: &mut NotesState,
    entries: &[NoteEntry],
    depth: usize,
    actions: &mut NotesActions,
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
    state: &mut NotesState,
    entry: &NoteEntry,
    depth: usize,
    actions: &mut NotesActions,
) {
    let t = Tokens::get(ui.ctx());
    ui.push_id(("note-row", entry.path.as_os_str()), |ui| {
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ROW_H),
            egui::Sense::click(),
        );
        let press = Press::track(ui, &resp);
        let is_active = state
            .active
            .is_some_and(|a| state.open.get(a).is_some_and(|n| n.path == entry.path));
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
                Rect::from_center_size(egui::pos2(x + 6.0, cy), egui::Vec2::splat(12.0)),
                chevron,
                12.0,
                t.text_faint,
            );
            x += 14.0;
        } else {
            x += 14.0;
        }

        let icon = if entry.is_dir { "folder" } else { "file" };
        crate::icons::paint(
            ui,
            Rect::from_center_size(egui::pos2(x + 7.0, cy), egui::Vec2::splat(14.0)),
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
                egui::Rect::from_min_size(
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
                    match notes::rename_path(&path, &state.root, trimmed) {
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
                state.selected_dir = entry
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| state.root.clone());
                state.open_file(entry.path.clone());
            }
        }

        resp.context_menu(|ui| {
            if entry.is_dir {
                if theme::menu_item(ui, "file-plus", "New note here", false).clicked() {
                    ui.close();
                    match notes::create_note(&state.root, &entry.path) {
                        Ok(path) => {
                            state.expanded.insert(entry.path.clone());
                            state.rescan();
                            state.open_file(path);
                        }
                        Err(e) => state.error = Some(format!("Cannot create note: {e}")),
                    }
                }
                if theme::menu_item(ui, "folder-plus", "New folder here", false).clicked() {
                    ui.close();
                    match notes::create_folder(&state.root, &entry.path) {
                        Ok(_) => {
                            state.expanded.insert(entry.path.clone());
                            state.rescan();
                        }
                        Err(e) => state.error = Some(format!("Cannot create folder: {e}")),
                    }
                }
            } else if theme::menu_item(ui, "eye", "Open", false).clicked() {
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

// -- right panel: tab strip + editor/preview --------------------------------

fn editor_panel(ui: &mut egui::Ui, state: &mut NotesState) {
    if state.open.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.weak("Select a note or create a new one");
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

    let NotesState {
        open,
        highlighter,
        preview_cache,
        ..
    } = state;
    let note = &mut open[idx];

    match note.mode {
        NoteMode::Edit => {
            let avail = ui.available_size();
            let outer_h = avail.y.max(80.0);
            let t = Tokens::get(ui.ctx());
            egui::Frame::new()
                .fill(t.field)
                .stroke(egui::Stroke::new(1.0, t.border))
                .corner_radius(egui::CornerRadius::same(t.radius))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    let inner_h = outer_h - 18.0; // margins + stroke
                    egui::ScrollArea::vertical()
                        .id_salt(("note-editor-scroll", note.path.as_os_str()))
                        .max_height(inner_h)
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            let hl = highlighter.as_ref().expect("highlighter set in show()");
                            let mut layouter =
                                |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                                    let mut job = hl.layout_job(ui, buf.as_str());
                                    job.wrap.max_width = wrap;
                                    ui.fonts_mut(|f| f.layout_job(job))
                                };
                            let resp = ui.add(
                                egui::TextEdit::multiline(&mut note.text)
                                    .id_salt(("note-editor", note.path.as_os_str()))
                                    .frame(egui::Frame::new())
                                    .desired_width(f32::INFINITY)
                                    .font(egui::TextStyle::Monospace)
                                    .layouter(&mut layouter)
                                    .min_size(egui::vec2(ui.available_width(), inner_h)),
                            );
                            if resp.changed() {
                                note.dirty_since = Some(Instant::now());
                            }
                        });
                });
        }
        NoteMode::Preview => {
            egui::ScrollArea::vertical()
                .id_salt(("note-preview-scroll", note.path.as_os_str()))
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    CommonMarkViewer::new().show(ui, preview_cache, &note.text);
                });
        }
    }
}

fn tab_strip(ui: &mut egui::Ui, state: &mut NotesState) {
    ui.horizontal(|ui| {
        let toggle_w = 34.0;
        let strip_w = (ui.available_width() - toggle_w).max(60.0);
        ui.allocate_ui(egui::vec2(strip_w, TAB_H), |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("notes-tabs-scroll")
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let mut close_idx: Option<usize> = None;
                        for (idx, note) in state.open.iter().enumerate() {
                            let name = note
                                .path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "untitled".to_string());
                            let resp = tab_widget(
                                ui,
                                &name,
                                state.active == Some(idx),
                                note.dirty_since.is_some(),
                                ("note-tab", note.path.as_os_str()),
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

        // Edit/preview toggle for the active tab.
        if let Some(idx) = state.active {
            let mode = state.open[idx].mode;
            let (icon, tip) = match mode {
                NoteMode::Edit => ("eye", "Preview"),
                NoteMode::Preview => ("pencil", "Edit"),
            };
            if theme::icon_button(ui, icon, 26.0, false, tip).clicked() {
                state.open[idx].mode = match mode {
                    NoteMode::Edit => NoteMode::Preview,
                    NoteMode::Preview => NoteMode::Edit,
                };
            }
        }
    });
}

struct TabResponse {
    inner: egui::Response,
    close_clicked: bool,
}

impl TabResponse {
    fn clicked(&self) -> bool {
        self.inner.clicked() && !self.close_clicked
    }
    fn middle_clicked(&self) -> bool {
        self.inner.middle_clicked()
    }
}

/// One tab: name, optional dirty dot, close ×. Active tabs get the accent
/// underline and stronger text, mirroring `theme::tab_button`.
fn tab_widget(
    ui: &mut egui::Ui,
    name: &str,
    active: bool,
    dirty: bool,
    id_salt: impl egui::AsIdSalt,
) -> TabResponse {
    let t = Tokens::get(ui.ctx());
    ui.push_id(id_salt, |ui| {
        let text_style = if active {
            theme::small_strong()
        } else {
            egui::TextStyle::Small
        };
        let text_color = if active { t.accent_text } else { t.text_muted };
        let galley = ui.painter().layout_no_wrap(
            name.to_string(),
            theme::font_id(ui, text_style),
            text_color,
        );

        let close_w = 18.0;
        let dirty_w = if dirty { 10.0 } else { 0.0 };
        let width = 10.0 + galley.size().x + dirty_w + close_w + 8.0;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, TAB_H), egui::Sense::click());
        let press = Press::track(ui, &resp);
        press.wash(ui, rect, egui::CornerRadius::same(t.radius), false);

        let cy = rect.center().y + press.offset().y;
        let text_w = galley.size().x;
        let tx = rect.left() + 10.0;
        ui.painter().galley(
            egui::pos2(tx, cy - galley.size().y * 0.5),
            galley,
            text_color,
        );

        let mut x = tx + text_w;
        if dirty {
            ui.painter()
                .circle_filled(egui::pos2(x + 6.0, cy), 2.5, t.accent);
            x += dirty_w;
        }

        // Close button.
        let close_rect = egui::Rect::from_min_size(
            egui::pos2(x + 2.0, rect.top() + (TAB_H - 16.0) * 0.5),
            egui::vec2(16.0, 16.0),
        );
        let close_resp = ui.interact(close_rect, ui.id().with("close"), egui::Sense::click());
        let close_hover = ui.ctx().animate_bool_with_time_and_easing(
            close_resp.id.with("hover"),
            close_resp.hovered(),
            0.12,
            egui::emath::easing::quadratic_out,
        );
        if close_hover > 0.0 {
            ui.painter().rect_filled(
                close_rect,
                egui::CornerRadius::same(4),
                t.hover.gamma_multiply(close_hover),
            );
        }
        crate::icons::paint(
            ui,
            Rect::from_center_size(close_rect.center(), egui::Vec2::splat(11.0)),
            "x",
            11.0,
            if close_resp.hovered() {
                t.icon
            } else {
                t.text_faint
            },
        );

        // Active underline accent.
        if active {
            let y = rect.bottom() - 1.5;
            ui.painter().rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left() + 6.0, y - 2.0),
                    egui::pos2(rect.right() - 6.0, y),
                ),
                egui::CornerRadius::same(1),
                t.accent,
            );
        }

        TabResponse {
            close_clicked: close_resp.clicked(),
            inner: resp,
        }
    })
    .inner
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

    /// Render the notes view over a temp root with two notes and scan for
    /// egui duplicate-ID warnings (they surface as on-screen text).
    #[test]
    fn no_duplicate_id_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("notes");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.md"), "# A").unwrap();
        std::fs::write(root.join("b.md"), "# B").unwrap();

        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, true);

        let mut state = NotesState {
            root: root.clone(),
            selected_dir: root.clone(),
            tree: Vec::new(),
            expanded: HashSet::new(),
            renaming: None,
            open: Vec::new(),
            active: None,
            highlighter: None,
            highlighter_dark: true,
            preview_cache: CommonMarkCache::default(),
            error: None,
        };
        state.rescan();
        state.open_file(root.join("a.md"));
        state.open_file(root.join("b.md"));

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
}
