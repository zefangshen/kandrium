//! Card editor modal — port of the renderer's CardModal behavior.

use egui::{Context, Modal};
use egui_commonmark::CommonMarkCache;

use kandrium_core::{Board, Card, CreateCardBody, ProjectChoice};

use crate::theme::{self, Tokens};
use crate::widgets::markdown::{self, NoteMode};
use crate::widgets::vim::{self, ExCommand};

/// Editable snapshot of a card while the modal is open.
#[derive(Clone, Debug)]
pub struct CardEditState {
    pub card_id: String,
    pub title: String,
    pub description: String,
    pub due_date: String,
    pub labels: Vec<i64>,
    /// Project tag (normal boards) or owning project (project boards).
    pub project_id: Option<String>,
    /// Area board a project card is mirrored onto (`None` = project board only).
    /// Kept while the checkbox is off so re-checking restores the same board.
    pub area_board_id: Option<String>,
    /// "Show on Tasks boards" checkbox for project cards.
    pub area_enabled: bool,
}

impl CardEditState {
    pub fn from_card(card: &Card) -> Self {
        CardEditState {
            card_id: card.id.clone(),
            title: card.title.clone(),
            description: card.description.clone(),
            due_date: card.due_date.clone(),
            labels: card.labels.clone(),
            project_id: card.project_id.clone(),
            area_board_id: card.area_board_id.clone(),
            area_enabled: card.area_board_id.is_some(),
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
    /// User ran `:w` in the description editor: save but keep the modal open.
    pub save_keep_open: bool,
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

/// Board/project context for one modal session: which variant to show and
/// the pickers' option lists.
pub struct ModalTargets<'a> {
    /// Boards the card can be moved to (empty hides the picker).
    pub move_targets: &'a [Board],
    /// Normal boards a project card can be mirrored onto.
    pub area_targets: &'a [Board],
    /// All projects, for tagging a normal-board card.
    pub project_targets: &'a [ProjectChoice],
    /// Project-card variant: swaps the project picker for the "Show on Tasks
    /// boards" checkbox + board picker, and hides "Move to board".
    pub is_project_card: bool,
}

/// Edit/preview state of the markdown description field, owned by the app
/// for the duration of a modal session.
#[derive(Debug)]
pub struct DescState {
    pub mode: NoteMode,
    pub cache: CommonMarkCache,
}

impl Default for DescState {
    fn default() -> Self {
        DescState {
            mode: NoteMode::Edit,
            cache: CommonMarkCache::default(),
        }
    }
}

/// Show the card editor. `archived` controls the archive button label.
/// `targets` selects the modal variant and the pickers' option lists
/// (project cards stay on their project board and are mirrored onto one
/// normal board, while normal cards can be tagged with a project and moved
/// between boards). `desc` owns the markdown description field's
/// edit/preview state for the duration of the modal session.
pub fn show(
    ctx: &Context,
    state: &mut CardEditState,
    archived: bool,
    targets: &ModalTargets,
    desc: &mut DescState,
) -> CardModalOutcome {
    let ModalTargets {
        move_targets,
        area_targets,
        project_targets,
        is_project_card,
    } = *targets;
    let mut actions = ModalActions {
        save: false,
        save_keep_open: false,
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

        if is_project_card {
            // Project cards are owned by their project board; the only
            // project-related choice is which area board mirrors them.
            let project_label = state
                .project_id
                .as_ref()
                .and_then(|id| project_targets.iter().find(|p| &p.id == id))
                .map(|p| format!("{} — {}", p.identifier, p.name))
                .unwrap_or_else(|| "—".to_string());
            ui.horizontal(|ui| {
                ui.label("Project");
                ui.label(
                    egui::RichText::new(project_label)
                        .text_style(egui::TextStyle::Small)
                        .color(t.text_muted),
                );
            });
            ui.add_space(4.0);
            let has_targets = !area_targets.is_empty();
            ui.add_enabled_ui(has_targets, |ui| {
                ui.checkbox(&mut state.area_enabled, "Show on Tasks boards");
            });
            if !has_targets {
                state.area_enabled = false;
            }
            if state.area_enabled {
                // Keep a valid board selected so Save can't produce a
                // half-checked mirror.
                if !area_targets
                    .iter()
                    .any(|b| Some(&b.id) == state.area_board_id.as_ref())
                {
                    state.area_board_id = area_targets.first().map(|b| b.id.clone());
                }
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    ui.label("Board");
                    let area_label = state
                        .area_board_id
                        .as_ref()
                        .and_then(|id| area_targets.iter().find(|b| &b.id == id))
                        .map(|b| b.title.clone())
                        .unwrap_or_else(|| "Select a board…".to_string());
                    egui::ComboBox::from_id_salt("card-area-combo")
                        .selected_text(area_label)
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for board in area_targets {
                                let selected =
                                    state.area_board_id.as_deref() == Some(board.id.as_str());
                                if ui.selectable_label(selected, &board.title).clicked() {
                                    state.area_board_id = Some(board.id.clone());
                                    ui.close();
                                }
                            }
                        });
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    ui.label(
                        egui::RichText::new("Mirrors this card onto the selected board.")
                            .text_style(egui::TextStyle::Small)
                            .color(t.text_faint),
                    );
                });
            } else {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    ui.label(
                        egui::RichText::new("Not mirrored onto a Tasks board; hidden from All.")
                            .text_style(egui::TextStyle::Small)
                            .color(t.text_faint),
                    );
                });
            }
            ui.add_space(6.0);
        } else if !project_targets.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Project");
                let project_label = state
                    .project_id
                    .as_ref()
                    .and_then(|id| project_targets.iter().find(|p| &p.id == id))
                    .map(|p| format!("{} — {}", p.identifier, p.name))
                    .unwrap_or_else(|| "None".to_string());
                egui::ComboBox::from_id_salt("card-project-combo")
                    .selected_text(project_label)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(state.project_id.is_none(), "None")
                            .clicked()
                        {
                            state.project_id = None;
                            ui.close();
                        }
                        for project in project_targets {
                            let label = format!("{} — {}", project.identifier, project.name);
                            let selected = state.project_id.as_deref() == Some(project.id.as_str());
                            if ui.selectable_label(selected, label).clicked() {
                                state.project_id = Some(project.id.clone());
                                ui.close();
                            }
                        }
                    });
            });
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new("Shows the project identifier on this card.")
                    .text_style(egui::TextStyle::Small)
                    .color(t.text_faint),
            );
            ui.add_space(6.0);
        }

        ui.horizontal(|ui| {
            ui.label("Description");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                markdown::mode_toggle(ui, &mut desc.mode);
            });
        });
        match desc.mode {
            NoteMode::Edit => {
                let (_, ex) = description_editor(ui, &state.card_id, &mut state.description);
                match ex {
                    Some(ExCommand::Write) => actions.save_keep_open = true,
                    Some(ExCommand::Close) => actions.save = true,
                    None => {}
                }
            }
            NoteMode::Preview => {
                description_preview(ui, &state.description, &mut desc.cache);
            }
        }
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

        if !is_project_card && !move_targets.is_empty() {
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
    if actions.save
        || actions.save_keep_open
        || actions.toggle_archive
        || actions.delete
        || actions.move_to_board.is_some()
    {
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
/// Inner height budget of the field box, shared by the scroll viewport and
/// the vim mode strip below it.
const DESC_VIEWPORT_HEIGHT: f32 = DESC_FIELD_HEIGHT - 2.0 * (DESC_FIELD_MARGIN + DESC_FIELD_STROKE);
/// Pinned widget id of the description editor: Tab handling checks its focus
/// before the `TextEdit` runs, and the vim layer (`pre_edit`/`paint`/
/// `status_bar`) keys its `TextEditState` and per-editor state on it.
const DESC_EDITOR_ID: &str = "kandrium-card-description-editor";
/// Text inserted by Tab in the description field.
const DESC_TAB_INSERT: &str = "  ";

/// Fixed-height description field: the bordered box stays at exactly
/// [`DESC_FIELD_HEIGHT`]; the vim mode strip sits below a ScrollArea that
/// shrinks to keep the total constant (egui 0.36's multiline TextEdit has no
/// internal scrolling — its height grows with the galley and `add_sized`
/// does not clamp it — so sizing the TextEdit itself would overflow and grow
/// the modal). Editing goes through the vim layer: while the editor is
/// focused and vim keys are on, Esc is consumed by vim (Insert → Normal,
/// closing the `:` line) instead of closing the modal.
///
/// Returns the outer frame's response (used by tests to assert the height)
/// and the `:` command completed this frame, if any. `card_id` keys the
/// per-card vim state so editing one card never leaks undo history, registers,
/// or pending operators into the next — mirroring the per-document ids used
/// by `markdown.rs`.
fn description_editor(
    ui: &mut egui::Ui,
    card_id: &str,
    text: &mut String,
) -> (egui::Response, Option<ExCommand>) {
    let editor_id = egui::Id::new((DESC_EDITOR_ID, card_id));
    let t = Tokens::get(ui.ctx());
    let mut ex = None;
    let response = egui::Frame::new()
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
                // Viewport shares DESC_VIEWPORT_HEIGHT with the mode strip:
                // viewport + item spacing + strip == the old full height.
                // The strip's clickable badge expands its allocation to one
                // interact row when that exceeds BAR_H.
                let strip_h = vim::BAR_H.max(ui.spacing().interact_size.y);
                let viewport_h = DESC_VIEWPORT_HEIGHT - strip_h - ui.spacing().item_spacing.y;
                egui::ScrollArea::vertical()
                    .id_salt("card-description-scroll")
                    .max_height(viewport_h)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        // Tab inserts two spaces instead of moving focus to the
                        // due-date field. `lock_focus(true)` keeps the caret in
                        // the widget; rewriting the Tab key event to text makes
                        // it insert spaces rather than a literal '\t'. Only in
                        // plain editing (vim off or Insert mode): in
                        // Normal/Visual mode the raw key goes to `pre_edit`,
                        // which swallows it.
                        if !vim::block_cursor(ui.ctx(), editor_id)
                            && ui.memory(|m| m.has_focus(editor_id))
                        {
                            ui.input_mut(|i| {
                                for event in &mut i.events {
                                    if let egui::Event::Key {
                                        key: egui::Key::Tab,
                                        pressed: true,
                                        modifiers,
                                        ..
                                    } = *event
                                        && !modifiers.command
                                        && !modifiers.ctrl
                                        && !modifiers.alt
                                    {
                                        *event = egui::Event::Text(DESC_TAB_INSERT.to_owned());
                                    }
                                }
                            });
                        }
                        let (_, ex_cmd) = vim::pre_edit(ui.ctx(), editor_id, text);
                        ex = ex_cmd;
                        // Normal/Visual modes draw a vim block caret; hide
                        // egui's line caret for this widget only.
                        let saved_stroke = vim::block_cursor(ui.ctx(), editor_id).then(|| {
                            let stroke = ui.visuals().text_cursor.stroke;
                            ui.visuals_mut().text_cursor.stroke = egui::Stroke::NONE;
                            stroke
                        });
                        let mut editor = egui::TextEdit::multiline(text)
                            .id(editor_id)
                            .hint_text("Add a more detailed description…")
                            .frame(egui::Frame::new())
                            .lock_focus(true)
                            .desired_width(f32::INFINITY)
                            .min_size(egui::vec2(ui.available_width(), viewport_h));
                        // Keep keyboard focus on Esc so the vim layer can
                        // switch back to Normal mode (egui's focus pass would
                        // otherwise defocus at frame start). The vim filter
                        // drops Tab retention, so restore what
                        // `lock_focus(true)` would set: Tab stays with this
                        // widget for the spaces rewrite / vim to consume.
                        if let Some(mut filter) = vim::event_filter(ui.ctx()) {
                            filter.tab = true;
                            editor = editor.event_filter(filter);
                        }
                        let output = editor.show(ui);
                        if let Some(stroke) = saved_stroke {
                            ui.visuals_mut().text_cursor.stroke = stroke;
                        }
                        vim::paint(ui, editor_id, &output);
                    });
            });
            vim::status_bar(ui, editor_id);
        })
        .response;
    (response, ex)
}

/// Rendered-markdown counterpart of [`description_editor`]: the same fixed
/// [`DESC_FIELD_HEIGHT`] frame so toggling never resizes the modal, with the
/// preview scrolling inside its own ScrollArea (independent offset from the
/// editor's).
fn description_preview(
    ui: &mut egui::Ui,
    text: &str,
    cache: &mut CommonMarkCache,
) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    egui::Frame::new()
        .fill(t.field)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(egui::CornerRadius::same(t.radius))
        .inner_margin(egui::Margin::same(DESC_FIELD_MARGIN as i8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt("card-description-preview-scroll")
                .max_height(DESC_VIEWPORT_HEIGHT)
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    if text.trim().is_empty() {
                        ui.label(
                            egui::RichText::new("Nothing to preview.")
                                .text_style(egui::TextStyle::Small)
                                .color(t.text_faint),
                        );
                    } else {
                        egui_commonmark::CommonMarkViewer::new().show(ui, cache, text);
                    }
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
                height = description_editor(&mut ui, "test", &mut text)
                    .0
                    .rect
                    .height();
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

    /// Same as [`rendered_height`] for the markdown preview: the frame must
    /// stay at the same fixed height so toggling never resizes the modal.
    fn rendered_preview_height(text: &str) -> f32 {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let mut cache = CommonMarkCache::default();
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
                height = description_preview(&mut ui, text, &mut cache).rect.height();
            });
            output.textures_delta.clear();
        }
        height
    }

    #[test]
    fn description_preview_height_matches_editor() {
        let short = rendered_preview_height("hi");
        let long = rendered_preview_height(&"line\n".repeat(200));
        assert!(
            (short - DESC_FIELD_HEIGHT).abs() <= 1.0,
            "short text: expected ~{DESC_FIELD_HEIGHT}px, got {short}"
        );
        assert!(
            (long - DESC_FIELD_HEIGHT).abs() <= 1.0,
            "long text: expected ~{DESC_FIELD_HEIGHT}px, got {long}"
        );
    }

    /// Paint the modal for one frame and return every text shape, so the
    /// tests can assert which controls the project/normal variants expose.
    fn modal_text(is_project_card: bool, description: &str, mode: NoteMode) -> String {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let mut state = CardEditState {
            card_id: "c1".into(),
            title: "Task".into(),
            description: description.to_string(),
            due_date: String::new(),
            labels: vec![],
            project_id: Some("p1".into()),
            area_board_id: None,
            // Exercise the checked branch (and the auto-pick of the first
            // board) on the project-card variant.
            area_enabled: is_project_card,
        };
        let area = [Board {
            id: "b1".into(),
            title: "Career".into(),
        }];
        let projects = [ProjectChoice {
            id: "p1".into(),
            name: "Kandrium".into(),
            identifier: "kan".into(),
        }];
        let mut desc = DescState {
            mode,
            cache: CommonMarkCache::default(),
        };
        let targets = ModalTargets {
            move_targets: &area,
            area_targets: &area,
            project_targets: &projects,
            is_project_card,
        };
        let mut text = String::new();
        // A modal fades in over a few frames; collect the settled frame.
        for _ in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 700.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    let _ = show(ctx, &mut state, false, &targets, &mut desc);
                },
            );
            text = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::epaint::Shape::Text(t) => Some(t.galley.job.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            output.textures_delta.clear();
        }
        text
    }

    #[test]
    fn project_modal_offers_show_on_tasks_boards_not_move_to_board() {
        let text = modal_text(true, "", NoteMode::Edit);
        assert!(text.contains("Show on Tasks boards"), "{text}");
        assert!(text.contains("Board"), "{text}");
        assert!(text.contains("Career"), "{text}");
        assert!(!text.contains("Move to board"), "{text}");
    }

    #[test]
    fn normal_modal_keeps_move_to_board() {
        let text = modal_text(false, "", NoteMode::Edit);
        assert!(text.contains("Move to board"), "{text}");
        assert!(!text.contains("Show on Tasks boards"), "{text}");
    }

    #[test]
    fn preview_mode_renders_markdown_not_source() {
        let preview = modal_text(false, "**bold**", NoteMode::Preview);
        assert!(preview.contains("bold"), "{preview}");
        assert!(!preview.contains("**bold**"), "{preview}");
        let edit = modal_text(false, "**bold**", NoteMode::Edit);
        assert!(edit.contains("**bold**"), "{edit}");
    }

    /// Render one frame of the description editor with `events` (same root
    /// layout as [`rendered_height`]) and clear textures to avoid a drop panic.
    fn editor_frame(
        ctx: &egui::Context,
        card_id: &str,
        text: &mut String,
        events: Vec<egui::Event>,
    ) {
        let mut output = ctx.run_ui(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ctx| {
                let mut ui = egui::Ui::new(
                    ctx.clone(),
                    egui::Id::new("root"),
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(500.0, 800.0),
                    )),
                );
                let _ = description_editor(&mut ui, card_id, text);
            },
        );
        output.textures_delta.clear();
    }

    fn editor_id(card_id: &str) -> egui::Id {
        egui::Id::new((DESC_EDITOR_ID, card_id))
    }

    /// Tab in the description keeps focus and inserts two spaces instead of
    /// moving to the due-date field (plain editing: vim keys off).
    #[test]
    fn tab_inserts_two_spaces_in_description_editor() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        crate::widgets::vim::set_enabled(&ctx, false);
        let id = editor_id("tab-test");
        let mut text = String::from("ab");

        // Frame 1: create the widget and its persisted state.
        editor_frame(&ctx, "tab-test", &mut text, Vec::new());
        ctx.memory_mut(|m| m.request_focus(id));
        // Frame 2: the focused editor registers its Tab event filter.
        editor_frame(&ctx, "tab-test", &mut text, Vec::new());

        // Frame 3: Tab is rewritten to text, so the buffer gains two spaces
        // and focus stays on the editor.
        editor_frame(
            &ctx,
            "tab-test",
            &mut text,
            vec![egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
        );

        assert_eq!(text, "ab  ");
        assert!(!text.contains('\t'));
        assert!(ctx.memory(|m| m.has_focus(id)));
    }

    fn tab_event() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// With vim keys on and no Insert session, Tab is a Normal-mode key the
    /// vim layer swallows: no spaces inserted, focus stays.
    #[test]
    fn normal_mode_tab_is_swallowed_with_vim_on() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let id = editor_id("vim-tab-test");
        let mut text = String::from("ab");

        editor_frame(&ctx, "vim-tab-test", &mut text, Vec::new());
        ctx.memory_mut(|m| m.request_focus(id));
        editor_frame(&ctx, "vim-tab-test", &mut text, Vec::new());
        editor_frame(&ctx, "vim-tab-test", &mut text, vec![tab_event()]);

        assert_eq!(text, "ab");
        assert!(ctx.memory(|m| m.has_focus(id)));
    }

    /// In vim Insert mode Tab keeps the plain-editing behavior: the rewrite
    /// turns it into two spaces.
    #[test]
    fn tab_inserts_two_spaces_in_vim_insert_mode() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let id = editor_id("insert-tab-test");
        let mut text = String::from("ab");

        editor_frame(&ctx, "insert-tab-test", &mut text, Vec::new());
        ctx.memory_mut(|m| m.request_focus(id));
        editor_frame(&ctx, "insert-tab-test", &mut text, Vec::new());
        // Normal mode: `A` enters Insert mode at the end of the line without
        // inserting a character.
        editor_frame(
            &ctx,
            "insert-tab-test",
            &mut text,
            vec![egui::Event::Text("A".into())],
        );
        assert_eq!(text, "ab");
        editor_frame(&ctx, "insert-tab-test", &mut text, vec![tab_event()]);

        assert_eq!(text, "ab  ");
        assert!(ctx.memory(|m| m.has_focus(id)));
    }

    /// Fresh headless context with fonts/theme and the vim preference set.
    fn modal_ctx(vim_on: bool) -> egui::Context {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        crate::widgets::vim::set_enabled(&ctx, vim_on);
        ctx
    }

    /// Run the modal for `frames` frames on `ctx`, focusing the description
    /// editor after the first frame and injecting `events` on frame
    /// `events_frame`. Returns the last frame's outcome. Vim state persists
    /// on `ctx` between calls, so modes carry over.
    fn run_modal(
        ctx: &egui::Context,
        events_frame: usize,
        events: Vec<egui::Event>,
        frames: usize,
    ) -> CardModalOutcome {
        let mut state = CardEditState {
            card_id: "c1".into(),
            title: "Task".into(),
            description: "ab".into(),
            due_date: String::new(),
            labels: vec![],
            project_id: None,
            area_board_id: None,
            area_enabled: false,
        };
        let boards = [Board {
            id: "b1".into(),
            title: "Career".into(),
        }];
        let projects = [ProjectChoice {
            id: "p1".into(),
            name: "Kandrium".into(),
            identifier: "kan".into(),
        }];
        let mut desc = DescState::default();
        let targets = ModalTargets {
            move_targets: &boards,
            area_targets: &boards,
            project_targets: &projects,
            is_project_card: false,
        };
        let editor_id = editor_id(&state.card_id);
        let mut outcome = CardModalOutcome::None;
        for i in 0..frames {
            let frame_events = if i == events_frame {
                events.clone()
            } else {
                Vec::new()
            };
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 700.0),
                    )),
                    events: frame_events,
                    ..Default::default()
                },
                |ctx| {
                    outcome = show(ctx, &mut state, false, &targets, &mut desc);
                },
            );
            output.textures_delta.clear();
            if i == 0 {
                ctx.memory_mut(|m| m.request_focus(editor_id));
            }
        }
        outcome
    }

    /// [`run_modal`] on a fresh context; returns the context too so tests can
    /// assert on vim state.
    fn modal_outcome(
        vim_on: bool,
        events_frame: usize,
        events: Vec<egui::Event>,
        frames: usize,
    ) -> (egui::Context, CardModalOutcome) {
        let ctx = modal_ctx(vim_on);
        let outcome = run_modal(&ctx, events_frame, events, frames);
        (ctx, outcome)
    }

    /// Esc while the description editor is focused goes to vim (Insert →
    /// Normal), not to the modal: the modal stays open.
    #[test]
    fn escape_with_vim_on_keeps_modal_open() {
        let esc = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        let editor_id = editor_id("c1");
        // Enter Insert mode first so Esc has a mode transition to perform.
        let (ctx, _) = modal_outcome(true, 2, vec![egui::Event::Text("i".into())], 3);
        assert!(!crate::widgets::vim::block_cursor(&ctx, editor_id));
        let outcome = run_modal(&ctx, 0, vec![esc], 1);
        assert!(matches!(outcome, CardModalOutcome::None));
        assert!(crate::widgets::vim::block_cursor(&ctx, editor_id));
    }

    /// Esc with vim keys off closes the modal, as before.
    #[test]
    fn escape_with_vim_off_closes_modal() {
        let esc = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        let (_, outcome) = modal_outcome(false, 2, vec![esc], 3);
        assert!(matches!(outcome, CardModalOutcome::Closed));
    }

    fn ex_events(cmd: &str) -> Vec<egui::Event> {
        let mut events = vec![egui::Event::Text(":".into())];
        events.extend(cmd.chars().map(|c| egui::Event::Text(c.to_string())));
        events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        });
        events
    }

    /// `:w` saves the card but keeps the modal open.
    #[test]
    fn ex_write_saves_and_keeps_modal_open() {
        let (_, outcome) = modal_outcome(true, 2, ex_events("w"), 3);
        match outcome {
            CardModalOutcome::Action(actions) => {
                assert!(actions.save_keep_open);
                assert!(!actions.save);
            }
            _ => panic!("expected Action with save_keep_open"),
        }
    }

    /// `:wq` saves the card and closes the modal.
    #[test]
    fn ex_wq_saves_and_closes() {
        let (_, outcome) = modal_outcome(true, 2, ex_events("wq"), 3);
        match outcome {
            CardModalOutcome::Action(actions) => {
                assert!(actions.save);
                assert!(!actions.save_keep_open);
            }
            _ => panic!("expected Action with save"),
        }
    }
}
