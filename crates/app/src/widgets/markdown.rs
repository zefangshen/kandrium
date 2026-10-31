//! Shared markdown editor/preview body used by the Notes view and the
//! Projects document tabs.
//!
//! Source editing is framed and scrolls internally (egui 0.36 multiline has
//! no built-in scroll); preview renders the in-memory buffer, so toggling
//! never touches disk.

use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use crate::theme::{self, Tokens};
use crate::views::notes_highlight::MarkdownHighlighter;

/// How a markdown tab renders its buffer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NoteMode {
    Edit,
    Preview,
}

/// Render `text` as a framed source editor or a rendered preview, filling the
/// remaining height of `ui`. `id` must be unique per open document. Returns
/// true when the buffer changed.
pub fn body(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &mut String,
    mode: NoteMode,
    highlighter: &MarkdownHighlighter,
    cache: &mut CommonMarkCache,
) -> bool {
    match mode {
        NoteMode::Edit => {
            let avail = ui.available_size();
            let outer_h = avail.y.max(80.0);
            let t = Tokens::get(ui.ctx());
            let mut changed = false;
            // Pinned widget id so the vim layer can load the same
            // `TextEditState` from outside the scroll area.
            let editor_id = ui.make_persistent_id(("markdown-editor", id));
            egui::Frame::new()
                .fill(t.field)
                .stroke(egui::Stroke::new(1.0, t.border))
                .corner_radius(egui::CornerRadius::same(t.radius))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    let inner_h = outer_h - 18.0 - crate::widgets::vim::BAR_H; // margins + stroke + strip
                    egui::ScrollArea::vertical()
                        .id_salt(id.with("scroll"))
                        .max_height(inner_h)
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            let mut layouter =
                                |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
                                    let mut job = highlighter.layout_job(ui, buf.as_str());
                                    job.wrap.max_width = wrap;
                                    ui.fonts_mut(|f| f.layout_job(job))
                                };
                            if crate::widgets::vim::pre_edit(ui.ctx(), editor_id, text) {
                                changed = true;
                            }
                            let output = egui::TextEdit::multiline(text)
                                .id(editor_id)
                                .frame(egui::Frame::new())
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace)
                                .layouter(&mut layouter)
                                .min_size(egui::vec2(ui.available_width(), inner_h))
                                .show(ui);
                            if output.response.response.changed() {
                                changed = true;
                            }
                            crate::widgets::vim::paint(ui, editor_id, &output);
                        });
                    crate::widgets::vim::status_bar(ui, editor_id);
                });
            changed
        }
        NoteMode::Preview => {
            egui::ScrollArea::vertical()
                .id_salt(id.with("preview"))
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    CommonMarkViewer::new().show(ui, cache, text.as_str());
                });
            false
        }
    }
}

/// Eye/pencil toggle for the active tab. Returns true when the mode changed.
pub fn mode_toggle(ui: &mut egui::Ui, mode: &mut NoteMode) -> bool {
    let (icon, tip) = match mode {
        NoteMode::Edit => ("eye", "Preview"),
        NoteMode::Preview => ("pencil", "Edit"),
    };
    if theme::icon_button(ui, icon, 26.0, false, tip).clicked() {
        *mode = match mode {
            NoteMode::Edit => NoteMode::Preview,
            NoteMode::Preview => NoteMode::Edit,
        };
        true
    } else {
        false
    }
}
