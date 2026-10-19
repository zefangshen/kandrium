//! Widget modules for the Kandrium UI.

pub mod card;
pub mod card_modal;
pub mod column;
pub mod confirm;
pub mod tabs;

/// Right inset for Kanban-column content that must NOT run under the
/// scrollbar. Column frames use a 1px right inner margin so the card-list
/// ScrollArea's outer rect — and thus its scrollbar — hugs the column's
/// right border (~2px gap, Trello-style); 7px here restores the classic
/// 8px content inset (1px frame margin + 7px) for everything else.
pub const CONTENT_RIGHT_INSET: f32 = 7.0;

/// Run `add` in a child ui whose right edge is pulled in by
/// [`CONTENT_RIGHT_INSET`], then advance the parent cursor past it
/// (standard item spacing applies, so surrounding geometry is unchanged).
pub fn right_inset(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let full = ui.available_rect_before_wrap();
    let rect = egui::Rect::from_min_max(
        full.min,
        egui::pos2(full.max.x - CONTENT_RIGHT_INSET, full.max.y),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    add(&mut child);
    ui.advance_cursor_after_rect(child.min_rect());
}
