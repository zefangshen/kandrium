//! Widget modules for the Kandrium UI.

pub mod card;
pub mod card_modal;
pub mod column;
pub mod confirm;
pub mod markdown;
pub mod tabs;

/// Width of the collapsed left-panel ("sidebar") rail. Panel collapse helpers
/// below keep the three tree pages (Projects, Notes, Documents) consistent.
pub const RAIL_W: f32 = 30.0;

/// Chevron for a left panel header that collapses the tree. Returns true when
/// clicked. Call it inside a right-to-left layout so it hugs the panel edge.
pub fn collapse_button(ui: &mut egui::Ui) -> bool {
    crate::theme::icon_button(ui, "chevron-left", 26.0, false, "Collapse sidebar").clicked()
}

/// Expand affordance for the collapsed rail. The whole bar is a click target
/// (only the chevron would be a 26 px sliver), with the chevron painted on
/// top. Returns true when the rail was clicked.
pub fn expand_button(ui: &mut egui::Ui) -> bool {
    let t = crate::theme::Tokens::get(ui.ctx());
    // Registered before the chevron, so the button keeps priority on its own
    // pixels while every other pixel of the rail expands too.
    let rail = ui.max_rect();
    let resp = ui
        .interact(rail, ui.id().with("rail-expand"), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Expand sidebar");
    if resp.hovered() {
        ui.painter()
            .rect_filled(rail, egui::CornerRadius::ZERO, t.hover);
    }

    let mut clicked = resp.clicked();
    ui.add_space(6.0);
    ui.vertical_centered(|ui| {
        clicked |=
            crate::theme::icon_button(ui, "chevron-right", 26.0, false, "Expand sidebar").clicked();
    });
    clicked
}

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
