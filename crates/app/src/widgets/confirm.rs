//! Reusable confirm dialog.

use egui::{Context, Modal};

use crate::theme;

/// Pending confirmation request: message + action id.
#[derive(Clone, Debug)]
pub struct ConfirmRequest {
    pub title: String,
    pub message: String,
    /// Label of the destructive action button ("Delete", "Archive all", ...).
    pub confirm_label: String,
    /// What to do on confirm — opaque to this widget.
    pub action: ConfirmAction,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConfirmAction {
    DeleteColumn(String),
    DeleteCard(String),
    /// Column id (may be a virtual `all:<status>` bucket id).
    ArchiveAll(String),
    /// Notes page: file or folder to delete (absolute path under the notes
    /// root).
    DeleteNotePath(std::path::PathBuf),
    /// Documents page: file or folder to delete (absolute path under the
    /// documents root).
    DeleteDocPath(std::path::PathBuf),
}

/// What happened this frame.
pub enum ConfirmOutcome {
    /// Still open, nothing requested.
    None,
    /// User clicked Delete.
    Confirmed,
    /// Cancel button, backdrop click, or Esc.
    Cancelled,
}

/// Show the confirm modal.
pub fn show(ctx: &Context, request: &ConfirmRequest, modal_id: &str) -> ConfirmOutcome {
    let mut confirmed = false;

    let modal = Modal::new(egui::Id::new(modal_id)).frame(theme::dialog_frame(ctx));
    let response = modal.show(ctx, |ui| {
        ui.set_min_width(320.0);
        ui.heading(&request.title);
        ui.add_space(8.0);
        ui.label(&request.message);
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::danger_pill_button(ui, &request.confirm_label).clicked() {
                confirmed = true;
            }
            if theme::pill_button(ui, "Cancel", false).clicked() {
                ui.close();
            }
        });
    });

    if confirmed {
        ConfirmOutcome::Confirmed
    } else if response.should_close() {
        ConfirmOutcome::Cancelled
    } else {
        ConfirmOutcome::None
    }
}
