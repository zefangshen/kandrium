//! Virtual All board view: three fixed status buckets aggregating cards from
//! every real board. Columns are read-only apart from the add-card row; cards
//! move between buckets and (via the card modal) between boards.

use egui::{CornerRadius, Label, Rect, TextWrapMode, Ui};

use kandrium_core::{AllBoard, AllBoardColumn, Board, CardStatus};

use crate::anim;
use crate::dnd::{self, ColKey, DropZone};
use crate::icons;
use crate::theme::{self, Tokens};
use crate::widgets::card::{CardMenuAction, card_chip, card_context_menu};

/// Extra breathing room between the bottom edge of a capped (scrolling) column
/// and the window's bottom edge, on top of the CentralPanel's 8px inner margin.
/// 15 = 8px visual gutter + 7px lane the horizontal scrollbar reserves when
/// active (the lane is set locally on this ui since the global style reserves
/// none); keeps a capped column's bottom border clear of the h-bar clip.
const BOTTOM_GUTTER: f32 = 15.0;

/// What the user did on the All board this frame.
#[derive(Default)]
pub struct AllBoardActions {
    /// Card id that was clicked (open editor).
    pub open_card: Option<String>,
    /// Pencil pressed on a card (open the inline title editor).
    pub edit_card: Option<String>,
    /// Inline title editor submitted (Enter/blur); text lives in the app draft.
    pub edit_commit: Option<String>,
    /// Inline title editor canceled (Esc).
    pub edit_cancel: Option<String>,
    /// Drop performed: (card_id, target_status, before_card_id).
    pub card_drop: Option<(String, CardStatus, Option<String>)>,
    /// "Archive all" requested on the Done bucket.
    pub archive_all_done: bool,
    /// Right-click "Archive" on a card chip.
    pub archive_card: Option<String>,
    /// Right-click "Delete" on a card chip.
    pub delete_card: Option<String>,
    /// "Add card" clicked on a bucket (opens the inline form for that status).
    pub open_add_form: Option<CardStatus>,
    /// Inline form submitted: (bucket status, target board id, title).
    pub create_card: Option<(CardStatus, String, String)>,
    /// Inline form canceled (Esc / click outside / X).
    pub cancel_add: Option<CardStatus>,
    /// Board picked in the inline form's dropdown.
    pub choose_board: Option<String>,
}

/// Collected layout of one bucket, for drop-zone registration.
struct BucketLayout {
    status: CardStatus,
    body: Rect,
    cards: Vec<(String, Rect)>,
}

/// Render the All board. Returns this frame's actions.
///
/// `add_status` names the bucket whose inline add-card form is open and
/// `add_draft` holds its live title text (mutated in place). `add_board` is the
/// board id the new card will land on.
///
/// `edit_card` names the card whose title is being edited inline and
/// `edit_draft` holds its live text (mutated in place).
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    board: &AllBoard,
    boards: &[Board],
    drag: Option<&dnd::Drag>,
    zones: &mut Vec<DropZone>,
    add_status: Option<CardStatus>,
    add_board: Option<&str>,
    add_draft: &mut Option<String>,
    hover: Option<(ColKey, usize)>,
    edit_card: Option<&str>,
    edit_draft: &mut String,
) -> AllBoardActions {
    let mut out = AllBoardActions::default();
    let dragging_card = drag.map(|d| d.card_id.clone());

    let col_width = 280.0;
    let col_gap = 12.0;

    // The gap opens only while a button is held; its height eases in.
    let gap_height = if drag.is_some() {
        anim::CardRig::gap_height(ui.ctx())
    } else {
        0.0
    };

    // Real viewport ceiling — MUST be captured BEFORE the horizontal
    // ScrollArea / horizontal_top placer (see board.rs::show for details).
    // The gutter reserves extra space below a capped bucket so its border
    // doesn't sit flush against the window's bottom edge.
    let max_col_height = ui.available_height() - BOTTOM_GUTTER;

    // Reserve the 7px bar lane locally for the board h-scroll (the global
    // style reserves none) — matches the BOTTOM_GUTTER math below.
    ui.spacing_mut().scroll.floating_allocated_width = 7.0;
    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Reset for descendants: the 7px lane is for THIS h-scroll only
            // (see board.rs).
            ui.spacing_mut().scroll.floating_allocated_width = 0.0;
            ui.spacing_mut().item_spacing.x = col_gap;
            ui.horizontal_top(|ui| {
                let mut bucket_layouts: Vec<BucketLayout> = Vec::new();

                for column in &board.columns {
                    let is_add_target = add_status == Some(column.status);

                    // Insertion gap only for the hovered bucket.
                    let gap_idx = hover.as_ref().and_then(|(key, idx)| match key {
                        ColKey::Bucket(status) if *status == column.status => Some(*idx),
                        _ => None,
                    });

                    let actions = ui
                        .allocate_ui_with_layout(
                            egui::vec2(col_width, f32::INFINITY),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                // Namespace persistent widget IDs per bucket
                                // (the vertical ScrollArea shares one default
                                // path otherwise → duplicate-ID warnings).
                                ui.push_id(("all-bucket", column.id.as_str()), |ui| {
                                    show_bucket(
                                        ui,
                                        column,
                                        boards,
                                        dragging_card.as_deref(),
                                        if is_add_target {
                                            add_draft.as_mut()
                                        } else {
                                            None
                                        },
                                        add_board,
                                        max_col_height,
                                        gap_idx,
                                        gap_height,
                                        edit_card,
                                        &mut *edit_draft,
                                    )
                                })
                                .inner
                            },
                        )
                        .inner;

                    if actions.archive_all {
                        out.archive_all_done = true;
                    }
                    if actions.add_clicked {
                        out.open_add_form = Some(column.status);
                    }
                    if actions.add_submitted {
                        let title = add_draft
                            .as_ref()
                            .map(|t| t.trim().to_string())
                            .unwrap_or_default();
                        if !title.is_empty()
                            && let Some(board_id) = add_board
                        {
                            out.create_card = Some((column.status, board_id.to_string(), title));
                        }
                    }
                    if actions.cancel_add {
                        out.cancel_add = Some(column.status);
                    }
                    if let Some(board_id) = actions.choose_board {
                        out.choose_board = Some(board_id);
                    }
                    if let Some(card) = actions.open_card {
                        out.open_card = Some(card);
                    }
                    if let Some(card) = actions.edit_card {
                        out.edit_card = Some(card);
                    }
                    if let Some(card) = actions.edit_commit {
                        out.edit_commit = Some(card);
                    }
                    if let Some(card) = actions.edit_cancel {
                        out.edit_cancel = Some(card);
                    }
                    if let Some(card) = actions.archive_card {
                        out.archive_card = Some(card);
                    }
                    if let Some(card) = actions.delete_card {
                        out.delete_card = Some(card);
                    }

                    bucket_layouts.push(BucketLayout {
                        status: column.status,
                        body: actions.body_rect,
                        cards: actions.card_rects,
                    });
                }

                // Register card drop zones after all buckets are laid out.
                for layout in &bucket_layouts {
                    let mut cards = layout.cards.clone();
                    if let Some(d) = drag {
                        cards.retain(|(id, _)| id != &d.card_id);
                    }
                    zones.push(DropZone {
                        key: ColKey::Bucket(layout.status),
                        rect: layout.body,
                        card_rects: cards,
                    });
                }

                // Drag feedback is the animated insertion gap rendered
                // in-flow by each bucket (driven by the pre-computed hover).
            });
        });

    out
}

struct BucketActions {
    open_card: Option<String>,
    edit_card: Option<String>,
    edit_commit: Option<String>,
    edit_cancel: Option<String>,
    archive_card: Option<String>,
    delete_card: Option<String>,
    card_rects: Vec<(String, Rect)>,
    body_rect: Rect,
    archive_all: bool,
    add_clicked: bool,
    add_submitted: bool,
    cancel_add: bool,
    choose_board: Option<String>,
}

/// Render one status bucket (read-only column with a count, an "Add card" row
/// and, on Done, an "Archive all" menu).
#[allow(clippy::too_many_arguments)]
fn show_bucket(
    ui: &mut Ui,
    column: &AllBoardColumn,
    boards: &[Board],
    dragging_card_id: Option<&str>,
    add_form: Option<&mut String>,
    add_board: Option<&str>,
    max_col_height: f32,
    gap_idx: Option<usize>,
    gap_height: f32,
    edit_card: Option<&str>,
    edit_draft: &mut String,
) -> BucketActions {
    let mut actions = BucketActions {
        open_card: None,
        edit_card: None,
        edit_commit: None,
        edit_cancel: None,
        archive_card: None,
        delete_card: None,
        card_rects: Vec::new(),
        body_rect: Rect::NOTHING,
        archive_all: false,
        add_clicked: false,
        add_submitted: false,
        cancel_add: false,
        choose_board: None,
    };
    let t = Tokens::get(ui.ctx());

    egui::Frame::new()
        .fill(t.card)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(8))
        // Right margin is 1px (not 8) so the card-list ScrollArea's outer
        // rect — and with it the scrollbar — hugs the bucket's right border
        // (~2px gap, Trello-style). Header items and direct-laid cards keep
        // the classic 8px right inset via a spacer / `widgets::right_inset`.
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
            // --- Header: title + count + (Done) menu ---
            ui.horizontal(|ui| {
                ui.add(
                    Label::new(egui::RichText::new(&column.title).text_style(theme::strong()))
                        .wrap_mode(TextWrapMode::Truncate),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Align the menu/count with the cards' right edge (the
                    // frame's right margin is only 1px now).
                    ui.add_space(crate::widgets::CONTENT_RIGHT_INSET);
                    if column.status == CardStatus::Done {
                        let resp = icons::button(ui, "ellipsis", 26.0, false, "Bucket menu");
                        let _ = egui::Popup::menu(&resp)
                            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                            .show(|ui| {
                                if theme::menu_item(ui, "archive", "Archive all cards", false)
                                    .clicked()
                                {
                                    actions.archive_all = true;
                                }
                            });
                    }
                    ui.label(
                        egui::RichText::new(column.cards.len().to_string())
                            .text_style(theme::small_strong())
                            .color(t.text_faint),
                    );
                });
            });

            ui.add_space(4.0);

            // --- Card list (dynamic height, same as column.rs) ---
            let body_top = ui.cursor().top();
            let mut card_rects = Vec::new();

            let used_so_far = ui.cursor().top() - ui.min_rect().top();
            // Subtract the add-card row (or open form) so a bucket with many
            // cards caps its list instead of pushing the row past the window.
            let max_body =
                (max_col_height - used_so_far - add_form_height(add_form.is_some())).max(0.0);
            // Each card ~95px tall (frame + margin + possible board badge);
            // switch to scroll above threshold.
            let estimated = column.cards.len() as f32 * 95.0;
            if estimated <= max_body {
                // Direct layout. Inset so the cards' right edge matches the
                // classic 8px margin (the ScrollArea branch below runs 7px
                // further right).
                crate::widgets::right_inset(ui, |ui| {
                    let mut rendered = 0usize;
                    for all_card in &column.cards {
                        if Some(all_card.card.id.as_str()) == dragging_card_id {
                            continue;
                        }
                        if gap_idx == Some(rendered) {
                            anim::gap_slot(ui, gap_height);
                        }
                        let response = card_chip(
                            ui,
                            &all_card.card,
                            Some(&all_card.board_title),
                            anim::stagger_order(gap_idx, rendered),
                            if edit_card == Some(all_card.card.id.as_str()) {
                                Some(&mut *edit_draft)
                            } else {
                                None
                            },
                            true,
                        );
                        if response.response.clicked() {
                            actions.open_card = Some(all_card.card.id.clone());
                        }
                        if response.edit_clicked {
                            actions.edit_card = Some(all_card.card.id.clone());
                        }
                        if response.edit_commit {
                            actions.edit_commit = Some(all_card.card.id.clone());
                        }
                        if response.edit_cancel {
                            actions.edit_cancel = Some(all_card.card.id.clone());
                        }
                        match card_context_menu(&response.response) {
                            Some(CardMenuAction::Archive) => {
                                actions.archive_card = Some(all_card.card.id.clone());
                            }
                            Some(CardMenuAction::Delete) => {
                                actions.delete_card = Some(all_card.card.id.clone());
                            }
                            None => {}
                        }
                        card_rects.push((all_card.card.id.clone(), response.response.rect));
                        rendered += 1;
                    }
                    if gap_idx == Some(rendered) {
                        anim::gap_slot(ui, gap_height);
                    }
                });
            } else {
                egui::ScrollArea::vertical()
                    .max_height(max_body)
                    // Constant 7px right gutter so cards stay 264px wide
                    // with symmetric 8px margins whether or not the bar is
                    // shown (see column.rs).
                    .content_margin(egui::Margin {
                        left: 0,
                        right: crate::widgets::CONTENT_RIGHT_INSET as i8,
                        top: 0,
                        bottom: 0,
                    })
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let mut rendered = 0usize;
                        for all_card in &column.cards {
                            if Some(all_card.card.id.as_str()) == dragging_card_id {
                                continue;
                            }
                            if gap_idx == Some(rendered) {
                                anim::gap_slot(ui, gap_height);
                            }
                            let response = card_chip(
                                ui,
                                &all_card.card,
                                Some(&all_card.board_title),
                                anim::stagger_order(gap_idx, rendered),
                                if edit_card == Some(all_card.card.id.as_str()) {
                                    Some(&mut *edit_draft)
                                } else {
                                    None
                                },
                                true,
                            );
                            if response.response.clicked() {
                                actions.open_card = Some(all_card.card.id.clone());
                            }
                            if response.edit_clicked {
                                actions.edit_card = Some(all_card.card.id.clone());
                            }
                            if response.edit_commit {
                                actions.edit_commit = Some(all_card.card.id.clone());
                            }
                            if response.edit_cancel {
                                actions.edit_cancel = Some(all_card.card.id.clone());
                            }
                            match card_context_menu(&response.response) {
                                Some(CardMenuAction::Archive) => {
                                    actions.archive_card = Some(all_card.card.id.clone());
                                }
                                Some(CardMenuAction::Delete) => {
                                    actions.delete_card = Some(all_card.card.id.clone());
                                }
                                None => {}
                            }
                            card_rects.push((all_card.card.id.clone(), response.response.rect));
                            rendered += 1;
                        }
                        if gap_idx == Some(rendered) {
                            anim::gap_slot(ui, gap_height);
                        }
                    });
            }
            let body_bottom = ui.cursor().top();
            // Empty buckets need minimum height too (see column.rs).
            let body_bottom = body_bottom.max(body_top + 30.0);
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

            // --- Add card row / inline form ---
            crate::widgets::right_inset(ui, |ui| {
                if let Some(text) = add_form {
                    ui.add_space(2.0);
                    let edit_id = egui::Id::new(("all-add-card", column.id.as_str()));
                    // Focus the input as soon as the form opens so typing works
                    // immediately; only grab focus when nothing else holds it.
                    ui.ctx().memory_mut(|m| {
                        if m.focused().is_none() {
                            m.request_focus(edit_id);
                        }
                    });
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

                    // Which board the new card belongs to. The app seeds this
                    // when the form opens (Career by default).
                    let selected_title = add_board
                        .and_then(|id| boards.iter().find(|b| b.id == id))
                        .map(|b| b.title.as_str())
                        .unwrap_or("Choose…");
                    let board_response = ui
                        .horizontal(|ui| {
                            ui.label("Board");
                            egui::ComboBox::from_id_salt(("all-add-board", column.id.as_str()))
                                .selected_text(selected_title)
                                .width(180.0)
                                .show_ui(ui, |ui| {
                                    for b in boards {
                                        let selected = add_board == Some(b.id.as_str());
                                        if ui.selectable_label(selected, &b.title).clicked() {
                                            actions.choose_board = Some(b.id.clone());
                                            ui.close();
                                        }
                                    }
                                });
                        })
                        .response;

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
                    // text survives (app.rs stashes it per bucket). Skip while
                    // a popup is open so picking a board from the combo (whose
                    // popup lies outside `form_rect`) doesn't close the form.
                    let form_rect = edit_response
                        .rect
                        .union(board_response.rect)
                        .union(row_response.rect);
                    let pressed_outside = ui.input(|i| {
                        i.pointer.any_pressed()
                            && i.pointer
                                .press_origin()
                                .is_some_and(|p| !form_rect.contains(p))
                    }) && !ui.ctx().any_popup_open();
                    if pressed_outside {
                        actions.cancel_add = true;
                    }
                } else if theme::ghost_button(ui, Some("plus"), "Add card").clicked() {
                    actions.add_clicked = true;
                }
            });
        });

    actions
}

/// Vertical space reserved below the card list for the add-card row: a 30px
/// ghost button, or the open form (title input + board picker + buttons).
fn add_form_height(open: bool) -> f32 {
    if open { 124.0 } else { 32.0 }
}

#[cfg(test)]
mod layout_tests {
    use kandrium_core::{AllBoard, AllBoardCard, AllBoardColumn, Card, CardStatus};

    fn all_board() -> AllBoard {
        let mk = |id: &str, status: CardStatus, title: &str| AllBoardColumn {
            id: id.to_string(),
            title: title.to_string(),
            status,
            cards: vec![
                AllBoardCard {
                    card: Card {
                        id: format!("c-{}", id),
                        column_id: "x".into(),
                        title: "Setup the board with a fairly long card title".to_string(),
                        description: String::new(),
                        due_date: String::new(),
                        labels: vec![],
                        ..Default::default()
                    },
                    board_id: "b".into(),
                    board_title: "Career".into(),
                },
                // Short title must not shrink the chip to text width.
                AllBoardCard {
                    card: Card {
                        id: format!("c-{}-short", id),
                        column_id: "x".into(),
                        title: "Hi".to_string(),
                        description: String::new(),
                        due_date: String::new(),
                        labels: vec![],
                        ..Default::default()
                    },
                    board_id: "b".into(),
                    board_title: "Career".into(),
                },
            ],
        };
        AllBoard {
            id: "all".into(),
            title: "All".into(),
            columns: vec![
                mk("todo", CardStatus::Todo, "To Do"),
                mk("prog", CardStatus::InProgress, "In Progress"),
                mk("done", CardStatus::Done, "Done"),
            ],
        }
    }

    #[test]
    fn buckets_get_full_finite_width_and_positions() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let board = all_board();
        let mut zones = Vec::new();

        let mut output = ctx.run_ui(Default::default(), |ctx| {
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::Id::new("root"),
                egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1000.0, 700.0),
                )),
            );
            super::show(
                &mut ui,
                &board,
                &[],
                None,
                &mut zones,
                None,
                None,
                &mut None,
                None,
                None,
                &mut String::new(),
            );
        });
        output.textures_delta.clear();

        let clashes: Vec<String> = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) => {
                    let text = t.galley.text();
                    text.contains("use of").then(|| text.to_string())
                }
                _ => None,
            })
            .collect();
        assert!(
            clashes.is_empty(),
            "duplicate widget IDs painted: {clashes:?}"
        );

        assert_eq!(zones.len(), 3, "one drop zone per bucket");
        for (i, zone) in zones.iter().enumerate() {
            assert!(
                (zone.rect.width() - 264.0).abs() < 4.0,
                "zone {} width = {} (expected ~264 = 280 - margins)",
                i,
                zone.rect.width()
            );
            let expected_left = 8.0 + i as f32 * 292.0;
            assert!(
                (zone.rect.left() - expected_left).abs() < 4.0,
                "zone {} left = {} expected {}",
                i,
                zone.rect.left(),
                expected_left
            );
            for (j, (_, card_rect)) in zone.card_rects.iter().enumerate() {
                assert!(
                    card_rect.width() > 200.0,
                    "card {j} width = {} (would be ~10px with one-char wrapping)",
                    card_rect.width()
                );
                assert!(
                    card_rect.height() < 80.0,
                    "card {j} height = {} (one-char-per-line wraps the title tall)",
                    card_rect.height()
                );
                // Every chip stretches to the bucket content width regardless
                // of title length: 264 zone − 8px frame margin each side
                // (minus the 1px bg stroke). A short title used to shrink to
                // ~12px, leaving the right side empty.
                let expected_card_width = zone.rect.width() - 18.0;
                assert!(
                    (card_rect.width() - expected_card_width).abs() < 4.0,
                    "card {j} width = {} (expected ~{} = full bucket width minus margins)",
                    card_rect.width(),
                    expected_card_width
                );
                // Margins stay uniform on both sides.
                assert!(
                    (card_rect.left() - (zone.rect.left() + 9.0)).abs() < 4.0,
                    "card {j} left margin changed: left = {} zone = {}",
                    card_rect.left(),
                    zone.rect.left()
                );
                assert!(
                    ((zone.rect.right() - 9.0) - card_rect.right()).abs() < 4.0,
                    "card {j} right margin changed: right = {} zone = {}",
                    card_rect.right(),
                    zone.rect.right()
                );
            }
        }
    }

    #[test]
    fn add_form_opens_in_selected_bucket() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let board = all_board();
        let boards = vec![
            kandrium_core::Board {
                id: "board_career".into(),
                title: "Career".into(),
            },
            kandrium_core::Board {
                id: "board_work".into(),
                title: "Work".into(),
            },
        ];
        let mut zones = Vec::new();
        let mut draft = Some(String::new());

        let mut output = ctx.run_ui(Default::default(), |ctx| {
            let mut ui = egui::Ui::new(
                ctx.clone(),
                egui::Id::new("root"),
                egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(1000.0, 700.0),
                )),
            );
            super::show(
                &mut ui,
                &board,
                &boards,
                None,
                &mut zones,
                Some(CardStatus::Todo),
                Some("board_career"),
                &mut draft,
                None,
                None,
                &mut String::new(),
            );
        });
        output.textures_delta.clear();

        let clashes: Vec<String> = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) => {
                    let text = t.galley.text();
                    text.contains("use of").then(|| text.to_string())
                }
                _ => None,
            })
            .collect();
        assert!(
            clashes.is_empty(),
            "duplicate widget IDs painted: {clashes:?}"
        );

        assert_eq!(zones.len(), 3, "one drop zone per bucket");
        for zone in &zones {
            assert!(
                (zone.rect.width() - 264.0).abs() < 4.0,
                "bucket width changed with the add form open: {}",
                zone.rect.width()
            );
        }

        // Exactly one form renders: one "Board" picker label, while all three
        // buckets still show an "Add card" control (two ghosts + one submit).
        let texts: Vec<String> = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts.iter().filter(|t| t.as_str() == "Board").count(),
            1,
            "only the targeted bucket opens the add form: {texts:?}"
        );
        assert_eq!(
            texts.iter().filter(|t| t.as_str() == "Add card").count(),
            3,
            "every bucket keeps an add control: {texts:?}"
        );
    }

    #[test]
    fn drag_gap_reserves_a_slot_only_in_the_hovered_bucket() {
        use crate::anim;
        use crate::dnd::{self, ColKey};

        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let board = all_board(); // 2 cards per bucket
        let dragged = board.columns[1].cards[0].card.clone(); // first of "prog"

        let render = |ctx: &egui::Context,
                      board: &AllBoard,
                      drag: Option<&dnd::Drag>,
                      hover: Option<(ColKey, usize)>,
                      now: f64| {
            let mut zones = Vec::new();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    // Timestamps past GAP_DURATION make the gap fully open.
                    time: Some(now),
                    ..Default::default()
                },
                |ctx| {
                    let mut ui = egui::Ui::new(
                        ctx.clone(),
                        egui::Id::new("root"),
                        egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                            egui::pos2(0.0, 0.0),
                            egui::vec2(1000.0, 700.0),
                        )),
                    );
                    super::show(
                        &mut ui,
                        board,
                        &[],
                        drag,
                        &mut zones,
                        None,
                        None,
                        &mut None,
                        hover.clone(),
                        None,
                        &mut String::new(),
                    );
                },
            );
            output.textures_delta.clear();
            zones
        };

        let base = render(&ctx, &board, None, None, 0.0);
        let dragged_height = base[1]
            .card_rects
            .iter()
            .find(|(id, _)| id == &dragged.id)
            .map(|(_, r)| r.height())
            .expect("dragged card rect recorded");
        let todo_base = base[0].rect.height();
        let prog_base = base[1].rect.height();

        anim::CardRig::begin_drag(
            &ctx,
            dragged.clone(),
            Some("Career".to_string()),
            base[1].card_rects[0].1.center(),
        );
        let drag = dnd::Drag {
            card_id: dragged.id.clone(),
        };

        // Lifted, no hover yet.
        let lifted = render(&ctx, &board, Some(&drag), None, 10.0);
        assert_eq!(lifted[1].card_rects.len(), 1);
        let removed_stride = lifted[1].rect.height();
        assert!(removed_stride < prog_base);

        // Gap opens at the end of the In Progress bucket.
        let hover = Some((ColKey::Bucket(CardStatus::InProgress), 1));
        let gapped = render(&ctx, &board, Some(&drag), hover, 20.0);
        // One chip stride: card height + frame chrome on each side
        // (8px inner + 1px stroke + the style's outer item spacing).
        let gap_height = anim::CardRig::gap_height(&ctx);
        assert!((gap_height - dragged_height).abs() < 0.5);
        let chip_chrome =
            2.0 * (8.0 + 1.0 + ctx.style_of(egui::Theme::Light).spacing.item_spacing.y);
        let restored = gapped[1].rect.height() - removed_stride;
        assert!(
            (restored - (gap_height + chip_chrome)).abs() < 4.0,
            "gap reserve = {restored}, expected card stride ~{}",
            gap_height + chip_chrome
        );
        assert!((gapped[0].rect.height() - todo_base).abs() < 4.0);
        assert_eq!(gapped[1].card_rects.len(), 1);
        assert!(
            gapped.iter().all(|z| (z.rect.width() - 264.0).abs() < 4.0),
            "bucket widths must not change"
        );
    }
}
