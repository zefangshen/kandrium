//! Column widget for the real board view: header + menu,
//! card list, add-card form.
//!
//! Pure rendering + hit-rect collection; the view layer owns drag/drop
//! decisions and service calls.

use egui::{CornerRadius, Label, Layout, Rect, TextWrapMode, Ui};

use kandrium_core::Column;

use crate::anim;
use crate::icons;
use crate::theme::{self, Tokens};
use crate::widgets::card::{CardMenuAction, card_chip, card_context_menu};

/// Everything the view needs to know from rendering one column.
pub struct ColumnActions {
    /// Card id that was clicked (open editor).
    pub open_card: Option<String>,
    /// Card id whose hover pencil was pressed (open inline title editor).
    pub edit_card: Option<String>,
    /// Card id whose inline title editor submitted (Enter/blur).
    pub edit_commit: Option<String>,
    /// Card id whose inline title editor was canceled (Esc).
    pub edit_cancel: Option<String>,
    /// Card id chosen from the right-click "Archive" menu item.
    pub archive_card: Option<String>,
    /// Card id chosen from the right-click "Delete" menu item.
    pub delete_card: Option<String>,
    /// Ordered card rects collected for the column's drop zone.
    pub card_rects: Vec<(String, Rect)>,
    /// Whole-column rect (drop target for cross-column card moves).
    pub body_rect: Rect,
    /// Header rect (drop target for column reordering).
    pub header_rect: Rect,
    pub archive_all: bool,
    pub delete: bool,
    /// "Add card" clicked (open the inline form).
    pub add_clicked: bool,
    /// Inline form submitted (Add button or Enter); view reads the draft text.
    pub add_submitted: bool,
    /// Inline form canceled.
    pub cancel_add: bool,
}

impl Default for ColumnActions {
    fn default() -> Self {
        ColumnActions {
            open_card: None,
            edit_card: None,
            edit_commit: None,
            edit_cancel: None,
            archive_card: None,
            delete_card: None,
            card_rects: Vec::new(),
            body_rect: Rect::NOTHING,
            header_rect: Rect::NOTHING,
            archive_all: false,
            delete: false,
            add_clicked: false,
            add_submitted: false,
            cancel_add: false,
        }
    }
}

/// Render one board column.
///
/// The caller must place this in a finite-width child ui (e.g. via
/// [`egui::Ui::allocate_ui`]); the outer horizontal scroll area otherwise
/// provides infinite available width, which collapses the inner scroll area.
///
/// * `dragging_card_id` — the card being dragged (hidden from its source).
/// * `add_form` — Some(text) while the inline add-card form is open on this column.
/// * `max_col_height` — hard ceiling for the column frame; card list scrolls
///   when content would exceed this (captured from the viewport before the
///   infinite-height horizontal placer, so it stays a real finite value).
/// * `gap_idx` — insertion index of the open drag placeholder, if the
///   pointer is hovering this column.
/// * `gap_height` — animated height of the placeholder slot.
/// * `edit_card` — card whose title is being edited inline, if it lives here.
/// * `edit_draft` — live text of that inline title editor (mutated in place).
#[allow(clippy::too_many_arguments)]
pub fn show_column(
    ui: &mut Ui,
    column: &Column,
    dragging_card_id: Option<&str>,
    add_form: Option<&mut String>,
    max_col_height: f32,
    gap_idx: Option<usize>,
    gap_height: f32,
    edit_card: Option<&str>,
    edit_draft: &mut String,
) -> ColumnActions {
    let mut actions = ColumnActions::default();
    let t = Tokens::get(ui.ctx());

    let frame_response = egui::Frame::new()
        .fill(t.card)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(8))
        // Right margin is 1px (not 8) so the card-list ScrollArea's outer
        // rect — and with it the scrollbar — hugs the column's right border
        // (~2px gap, Trello-style). Header items, direct-laid cards and the
        // add-card row keep the classic 8px right inset via
        // `widgets::right_inset` / a header spacer.
        .inner_margin(egui::Margin {
            left: 8,
            right: 1,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            // Claim the full content width even though most content is
            // right-inset; otherwise the frame shrink-wraps under 280px.
            ui.set_min_width(ui.available_width());
            // --- Header: title + menu ---
            let (header_rect, _) = ui
                .horizontal(|ui| {
                    ui.add(
                        Label::new(egui::RichText::new(&column.title).text_style(theme::strong()))
                            .wrap_mode(TextWrapMode::Truncate),
                    );
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        // Align the menu button with the cards' right edge
                        // (the frame's right margin is only 1px now).
                        ui.add_space(crate::widgets::CONTENT_RIGHT_INSET);
                        menu_button(ui, &mut actions);
                        ui.add_space(2.0);
                        ui.add(
                            Label::new(
                                egui::RichText::new(
                                    (column.cards.len() + column.mirrored.len()).to_string(),
                                )
                                .text_style(theme::small_strong())
                                .color(t.text_faint),
                            )
                            .wrap_mode(TextWrapMode::Extend),
                        );
                    });
                    (ui.min_rect(), ())
                })
                .inner;
            actions.header_rect = header_rect;

            ui.add_space(4.0);

            // --- Card list ---
            let body_top = ui.cursor().top();
            let mut card_rects = Vec::new();

            // Compute how much vertical space is available for cards: the
            // passed-in max_col_height minus what's already been used
            // (header + spacing) and what remains below (add button/form).
            let used_so_far = ui.cursor().top() - ui.min_rect().top();
            let max_body =
                (max_col_height - used_so_far - add_form_height(add_form.is_some())).max(0.0);

            // Dynamic height: render cards directly when they fit, wrap in
            // a ScrollArea with max_height when they would overflow. Cards
            // with wrapped titles, due dates, or labels can be taller than
            // the baseline; 108px per card covers all common variants at
            // the app's 1.2x font scale while still triggering the scroll
            // threshold sensibly.
            let estimated = (column.cards.len() + column.mirrored.len()) as f32 * 108.0;
            if estimated <= max_body {
                // Direct layout — cards grow/shrink column naturally. Inset
                // so the cards' right edge matches the classic 8px margin
                // (the ScrollArea branch below runs 7px further right).
                crate::widgets::right_inset(ui, |ui| {
                    let mut rendered = 0usize;
                    for card in &column.cards {
                        if Some(card.id.as_str()) == dragging_card_id {
                            continue;
                        }
                        if gap_idx == Some(rendered) {
                            anim::gap_slot(ui, gap_height);
                        }
                        let editing = edit_card == Some(card.id.as_str());
                        let chip = card_chip(
                            ui,
                            card,
                            None,
                            anim::stagger_order(gap_idx, rendered),
                            if editing {
                                Some(&mut *edit_draft)
                            } else {
                                None
                            },
                            true,
                        );
                        if chip.response.clicked() {
                            actions.open_card = Some(card.id.clone());
                        }
                        if chip.edit_clicked {
                            actions.edit_card = Some(card.id.clone());
                        }
                        if chip.edit_commit {
                            actions.edit_commit = Some(card.id.clone());
                        }
                        if chip.edit_cancel {
                            actions.edit_cancel = Some(card.id.clone());
                        }
                        match card_context_menu(&chip.response) {
                            Some(CardMenuAction::Archive) => {
                                actions.archive_card = Some(card.id.clone());
                            }
                            Some(CardMenuAction::Delete) => {
                                actions.delete_card = Some(card.id.clone());
                            }
                            None => {}
                        }
                        card_rects.push((card.id.clone(), chip.response.rect));
                        rendered += 1;
                    }
                    if gap_idx == Some(rendered) {
                        anim::gap_slot(ui, gap_height);
                    }
                    mirrored_cards(ui, column, edit_card, edit_draft, &mut actions);
                });
            } else {
                egui::ScrollArea::vertical()
                    .max_height(max_body)
                    // Constant 7px right gutter (the bar reserves no layout
                    // space globally): cards stay 264px wide with symmetric
                    // 8px margins whether or not the bar is shown, and the
                    // 3px floating bar paints inside the gutter when needed.
                    .content_margin(egui::Margin {
                        left: 0,
                        right: crate::widgets::CONTENT_RIGHT_INSET as i8,
                        top: 0,
                        bottom: 0,
                    })
                    // auto_shrink[1] = true: shrink to content height so the
                    // column never has blank space below the cards. When
                    // content exceeds max_body it caps and scrolls instead.
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let mut rendered = 0usize;
                        for card in &column.cards {
                            if Some(card.id.as_str()) == dragging_card_id {
                                continue;
                            }
                            if gap_idx == Some(rendered) {
                                anim::gap_slot(ui, gap_height);
                            }
                            let editing = edit_card == Some(card.id.as_str());
                            let chip = card_chip(
                                ui,
                                card,
                                None,
                                anim::stagger_order(gap_idx, rendered),
                                if editing {
                                    Some(&mut *edit_draft)
                                } else {
                                    None
                                },
                                true,
                            );
                            if chip.response.clicked() {
                                actions.open_card = Some(card.id.clone());
                            }
                            if chip.edit_clicked {
                                actions.edit_card = Some(card.id.clone());
                            }
                            if chip.edit_commit {
                                actions.edit_commit = Some(card.id.clone());
                            }
                            if chip.edit_cancel {
                                actions.edit_cancel = Some(card.id.clone());
                            }
                            match card_context_menu(&chip.response) {
                                Some(CardMenuAction::Archive) => {
                                    actions.archive_card = Some(card.id.clone());
                                }
                                Some(CardMenuAction::Delete) => {
                                    actions.delete_card = Some(card.id.clone());
                                }
                                None => {}
                            }
                            card_rects.push((card.id.clone(), chip.response.rect));
                            rendered += 1;
                        }
                        if gap_idx == Some(rendered) {
                            anim::gap_slot(ui, gap_height);
                        }
                        mirrored_cards(ui, column, edit_card, edit_draft, &mut actions);
                    });
            }
            let body_bottom = ui.cursor().top();
            // Empty columns collapse body_rect to 0 without this minimum —
            // which also collapses the drop zone, making card drops impossible.
            // Reserve just enough room for a "drop here" hint above the add
            // button; populated columns still report their actual height.
            let min_body = 30.0;
            let body_bottom = body_bottom.max(body_top + min_body);
            actions.card_rects = card_rects;
            actions.body_rect = Rect::from_min_max(
                egui::pos2(ui.min_rect().left(), body_top),
                // Follow the cards' right edge, not the scrollbar lane:
                // min_rect now spans the full-width header/ScrollArea.
                egui::pos2(
                    ui.max_rect().right() - crate::widgets::CONTENT_RIGHT_INSET,
                    body_bottom,
                ),
            );

            // --- Add card form / button ---
            crate::widgets::right_inset(ui, |ui| {
                if let Some(text) = add_form {
                    ui.add_space(2.0);
                    let edit_id = egui::Id::new(("add-card", &column.id));
                    // Focus the input as soon as the form opens so typing works
                    // immediately; only grab focus when nothing else holds it.
                    ui.ctx().memory_mut(|m| {
                        if m.focused().is_none() {
                            m.request_focus(edit_id);
                        }
                    });
                    // Input framed like a card chip so the draft visually reads
                    // as the card it will become.
                    let edit = egui::TextEdit::singleline(text)
                        .hint_text("Enter a title…")
                        .desired_width(ui.available_width())
                        .min_size(egui::vec2(ui.available_width(), 40.0))
                        .frame(
                            egui::Frame::new()
                                .fill(t.field)
                                .stroke(egui::Stroke::new(1.0, t.border))
                                .corner_radius(CornerRadius::same(t.radius))
                                .inner_margin(egui::Margin::same(8)),
                        )
                        .id(edit_id);
                    let edit_response = ui.add(edit);
                    if edit_response.lost_focus() {
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            actions.add_submitted = true;
                        } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            actions.cancel_add = true;
                        }
                    }
                    let row_response = ui
                        .horizontal(|ui| {
                            if theme::pill_button(ui, "Add card", true).clicked() {
                                actions.add_submitted = true;
                            }
                            if icons::button(ui, "x", 32.0, false, "Cancel").clicked() {
                                actions.cancel_add = true;
                            }
                        })
                        .response;
                    // A press anywhere outside the form cancels it; the draft
                    // text survives (app.rs stashes it per column).
                    let form_rect = edit_response.rect.union(row_response.rect);
                    let pressed_outside = ui.input(|i| {
                        i.pointer.any_pressed()
                            && i.pointer
                                .press_origin()
                                .is_some_and(|p| !form_rect.contains(p))
                    });
                    if pressed_outside {
                        actions.cancel_add = true;
                    }
                } else {
                    if theme::ghost_button(ui, Some("plus"), "Add card").clicked() {
                        actions.add_clicked = true;
                    }
                }
            });
        });

    let _ = frame_response;
    actions
}

/// Render project cards filed to this area board after the column's own
/// cards. Mirrored cards can be opened, renamed inline and archived, but they
/// are not drag sources and never become drop anchors: their position is
/// owned by their project board.
fn mirrored_cards(
    ui: &mut Ui,
    column: &Column,
    edit_card: Option<&str>,
    edit_draft: &mut String,
    actions: &mut ColumnActions,
) {
    if column.mirrored.is_empty() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    ui.add_space(4.0);
    let (rule, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rule, egui::CornerRadius::ZERO, t.divider);
    ui.add_space(4.0);

    for card in &column.mirrored {
        let editing = edit_card == Some(card.id.as_str());
        let chip = card_chip(
            ui,
            card,
            None,
            0,
            if editing {
                Some(&mut *edit_draft)
            } else {
                None
            },
            false,
        );
        if chip.response.clicked() {
            actions.open_card = Some(card.id.clone());
        }
        if chip.edit_clicked {
            actions.edit_card = Some(card.id.clone());
        }
        if chip.edit_commit {
            actions.edit_commit = Some(card.id.clone());
        }
        if chip.edit_cancel {
            actions.edit_cancel = Some(card.id.clone());
        }
        match card_context_menu(&chip.response) {
            Some(CardMenuAction::Archive) => {
                actions.archive_card = Some(card.id.clone());
            }
            Some(CardMenuAction::Delete) => {
                actions.delete_card = Some(card.id.clone());
            }
            None => {}
        }
    }
}

fn add_form_height(open: bool) -> f32 {
    // Closed: 30px ghost button + spacing. Open: 40px card-like input +
    // 2px gap + 30px button row + spacing.
    if open { 90.0 } else { 32.0 }
}

fn menu_button(ui: &mut Ui, actions: &mut ColumnActions) {
    let resp = icons::button(ui, "ellipsis", 26.0, false, "Column menu");
    let _ = egui::Popup::menu(&resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            if theme::menu_item(ui, "archive", "Archive all cards", false).clicked() {
                actions.archive_all = true;
            }
            if theme::menu_item(ui, "trash-2", "Delete column", true).clicked() {
                actions.delete = true;
            }
        });
}
