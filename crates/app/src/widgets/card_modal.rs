//! Card editor modal — port of the renderer's CardModal behavior.

use egui::{Context, Modal};

use kandrium_core::{Board, Card, CreateCardBody};

use crate::theme::{self, Tokens};

/// Editable snapshot of a card while the modal is open.
#[derive(Clone, Debug)]
pub struct CardEditState {
    pub card_id: String,
    pub title: String,
    pub description: String,
    pub due_date: String,
    pub labels: Vec<i64>,
}

impl CardEditState {
    pub fn from_card(card: &Card) -> Self {
        CardEditState {
            card_id: card.id.clone(),
            title: card.title.clone(),
            description: card.description.clone(),
            due_date: card.due_date.clone(),
            labels: card.labels.clone(),
        }
    }

    pub fn to_body(&self) -> CreateCardBody {
        CreateCardBody {
            title: self.title.clone(),
            description: Some(self.description.clone()),
            due_date: Some(self.due_date.clone()),
            labels: Some(self.labels.clone()),
        }
    }
}

pub struct ModalActions {
    /// User pressed Save (values already edited in-place in the state).
    pub save: bool,
    /// User pressed Archive/Unarchive.
    pub toggle_archive: bool,
    /// User pressed Delete (needs confirmation).
    pub delete: bool,
    /// User picked a board to move the card to.
    pub move_to_board: Option<String>,
}

/// What happened this frame.
pub enum CardModalOutcome {
    /// Still open, nothing requested.
    None,
    /// Closed via backdrop click / Esc.
    Closed,
    /// A button was pressed (at most one action).
    Action(ModalActions),
}

/// Show the card editor. `archived` controls the archive button label.
/// `move_targets` are the boards the card can be moved to (empty hides the
/// picker).
pub fn show(
    ctx: &Context,
    state: &mut CardEditState,
    archived: bool,
    move_targets: &[Board],
) -> CardModalOutcome {
    let mut actions = ModalActions {
        save: false,
        toggle_archive: false,
        delete: false,
        move_to_board: None,
    };

    let modal = Modal::new(egui::Id::new("kandrium-card-modal")).frame(theme::dialog_frame(ctx));
    let response = modal.show(ctx, |ui| {
        ui.set_min_width(420.0);
        ui.heading("Edit card");
        ui.add_space(8.0);

        let t = Tokens::get(ui.ctx());
        let edit_frame = || {
            egui::Frame::new()
                .fill(t.field)
                .stroke(egui::Stroke::new(1.0, t.border))
                .corner_radius(egui::CornerRadius::same(t.radius))
                .inner_margin(egui::Margin::same(8))
        };

        ui.label("Title");
        ui.add(
            egui::TextEdit::singleline(&mut state.title)
                .hint_text("Card title")
                .desired_width(f32::INFINITY)
                .min_size(egui::vec2(ui.available_width(), 40.0))
                .frame(edit_frame()),
        );
        ui.add_space(6.0);

        ui.label("Description");
        description_editor(ui, &mut state.description);
        ui.add_space(6.0);

        ui.label("Due date");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.due_date)
                    .hint_text("YYYY-MM-DD")
                    .desired_width(140.0)
                    .frame(edit_frame()),
            );
            // Calendar picker: seed from the current text (or today when
            // empty/unparseable); a pick writes YYYY-MM-DD back.
            let mut picked = state
                .due_date
                .parse::<jiff::civil::Date>()
                .unwrap_or_else(|_| jiff::Zoned::now().date());
            let before = picked;
            ui.add(
                egui_extras::DatePickerButton::new(&mut picked)
                    .id_salt("due-date-picker")
                    .format("%Y-%m-%d"),
            );
            if picked != before {
                state.due_date = picked.to_string();
            }
        });
        ui.add_space(8.0);

        ui.label("Labels");
        ui.horizontal_wrapped(|ui| {
            for (i, (_, color)) in theme::LABEL_COLORS.iter().enumerate() {
                let index = i as i64;
                let selected = state.labels.contains(&index);
                let (rect, response) =
                    ui.allocate_exact_size(egui::vec2(26.0, 16.0), egui::Sense::click());
                ui.painter().rect_filled(rect, 4.0, *color);
                if selected {
                    ui.painter().rect_stroke(
                        rect.shrink(2.0),
                        2.0,
                        egui::Stroke::new(1.5, t.text),
                        egui::StrokeKind::Inside,
                    );
                }
                if response.clicked() {
                    if selected {
                        state.labels.retain(|l| *l != index);
                    } else {
                        state.labels.push(index);
                    }
                }
            }
        });

        if !move_targets.is_empty() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Move to board");
                egui::ComboBox::from_id_salt("move-to-board-combo")
                    .selected_text("Choose…")
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for b in move_targets {
                            if ui.selectable_label(true, &b.title).clicked() {
                                actions.move_to_board = Some(b.id.clone());
                                ui.close();
                            }
                        }
                    });
            });
        }

        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::pill_button(ui, "Save", true).clicked() {
                actions.save = true;
            }
            let archive_label = if archived { "Unarchive" } else { "Archive" };
            if theme::pill_button(ui, archive_label, false).clicked() {
                actions.toggle_archive = true;
            }
            if theme::danger_pill_button(ui, "Delete").clicked() {
                actions.delete = true;
            }
        });
    });

    // Closed via backdrop click / Esc — treat as save-less dismiss.
    if response.should_close() {
        return CardModalOutcome::Closed;
    }
    if actions.save || actions.toggle_archive || actions.delete || actions.move_to_board.is_some() {
        CardModalOutcome::Action(actions)
    } else {
        CardModalOutcome::None
    }
}

/// Total height of the description field, border included.
const DESC_FIELD_HEIGHT: f32 = 140.0;
/// Inner margin of the field frame (matches `edit_frame` in `show`).
const DESC_FIELD_MARGIN: f32 = 8.0;
/// Frame stroke width (matches `edit_frame` in `show`); adds to the outer rect.
const DESC_FIELD_STROKE: f32 = 1.0;
/// Scrollable viewport height inside the field box.
const DESC_VIEWPORT_HEIGHT: f32 = DESC_FIELD_HEIGHT - 2.0 * (DESC_FIELD_MARGIN + DESC_FIELD_STROKE);

/// Fixed-height description field: the bordered box stays at exactly
/// [`DESC_FIELD_HEIGHT`] and overflowing text scrolls inside an inner
/// ScrollArea. egui 0.36's multiline TextEdit has no internal scrolling — its
/// height grows with the galley and `add_sized` does not clamp it — so sizing
/// the TextEdit itself would let it overflow and grow the modal.
///
/// Returns the outer frame's response (used by tests to assert the height).
fn description_editor(ui: &mut egui::Ui, text: &mut String) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    egui::Frame::new()
        .fill(t.field)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(egui::CornerRadius::same(t.radius))
        .inner_margin(egui::Margin::same(DESC_FIELD_MARGIN as i8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            // The global style reserves no bar lane; locally restore the 7px
            // lane so long descriptions don't run under the scrollbar.
            ui.scope(|ui| {
                ui.spacing_mut().scroll.floating_allocated_width = 7.0;
                egui::ScrollArea::vertical()
                    .id_salt("card-description-scroll")
                    .max_height(DESC_VIEWPORT_HEIGHT)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .hint_text("Add a more detailed description…")
                                .frame(egui::Frame::new())
                                .desired_width(f32::INFINITY)
                                .min_size(egui::vec2(ui.available_width(), DESC_VIEWPORT_HEIGHT)),
                        );
                    });
            });
        })
        .response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render the description editor in a 500x800 viewport and return the
    /// outer frame height after egui's layout has settled (second pass).
    fn rendered_height(text: &str) -> f32 {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let mut text = text.to_string();
        let mut height = 0.0;
        for _ in 0..2 {
            let mut output = ctx.run_ui(Default::default(), |ctx| {
                let mut ui = egui::Ui::new(
                    ctx.clone(),
                    egui::Id::new("root"),
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(500.0, 800.0),
                    )),
                );
                height = description_editor(&mut ui, &mut text).rect.height();
            });
            output.textures_delta.clear();
        }
        height
    }

    #[test]
    fn description_field_height_is_fixed_regardless_of_content() {
        let short = rendered_height("hi");
        let long = rendered_height(&"line\n".repeat(200));
        assert!(
            (short - DESC_FIELD_HEIGHT).abs() <= 1.0,
            "short text: expected ~{DESC_FIELD_HEIGHT}px, got {short}"
        );
        assert!(
            (long - DESC_FIELD_HEIGHT).abs() <= 1.0,
            "long text: expected ~{DESC_FIELD_HEIGHT}px, got {long}"
        );
    }
}
