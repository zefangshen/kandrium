//! Virtual All board view: three fixed status buckets aggregating cards from
//! every real board. Columns are read-only; cards move between buckets and
//! (via the card modal) between boards.

use egui::{CornerRadius, Label, Rect, TextWrapMode, Ui};

use kandrium_core::{AllBoard, AllBoardColumn, CardStatus};

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
    /// Drop performed: (card_id, target_status, before_card_id).
    pub card_drop: Option<(String, CardStatus, Option<String>)>,
    /// "Archive all" requested on the Done bucket.
    pub archive_all_done: bool,
    /// Right-click "Archive" on a card chip.
    pub archive_card: Option<String>,
    /// Right-click "Delete" on a card chip.
    pub delete_card: Option<String>,
}

/// Collected layout of one bucket, for drop-zone registration.
struct BucketLayout {
    status: CardStatus,
    body: Rect,
    cards: Vec<(String, Rect)>,
}

/// Render the All board. Returns this frame's actions.
pub fn show(
    ui: &mut Ui,
    board: &AllBoard,
    drag: Option<&dnd::Drag>,
    zones: &mut Vec<DropZone>,
    hover: Option<(ColKey, usize)>,
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
                                        dragging_card.as_deref(),
                                        max_col_height,
                                        gap_idx,
                                        gap_height,
                                    )
                                })
                                .inner
                            },
                        )
                        .inner;

                    if actions.archive_all {
                        out.archive_all_done = true;
                    }
                    if let Some(card) = actions.open_card {
                        out.open_card = Some(card);
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
    archive_card: Option<String>,
    delete_card: Option<String>,
    card_rects: Vec<(String, Rect)>,
    body_rect: Rect,
    archive_all: bool,
}

/// Render one status bucket (read-only column with a count and, on Done,
/// an "Archive all" menu).
fn show_bucket(
    ui: &mut Ui,
    column: &AllBoardColumn,
    dragging_card_id: Option<&str>,
    max_col_height: f32,
    gap_idx: Option<usize>,
    gap_height: f32,
) -> BucketActions {
    let mut actions = BucketActions {
        open_card: None,
        archive_card: None,
        delete_card: None,
        card_rects: Vec::new(),
        body_rect: Rect::NOTHING,
        archive_all: false,
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
            let max_body = (max_col_height - used_so_far).max(0.0);
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
                        );
                        if response.clicked() {
                            actions.open_card = Some(all_card.card.id.clone());
                        }
                        match card_context_menu(&response) {
                            Some(CardMenuAction::Archive) => {
                                actions.archive_card = Some(all_card.card.id.clone());
                            }
                            Some(CardMenuAction::Delete) => {
                                actions.delete_card = Some(all_card.card.id.clone());
                            }
                            None => {}
                        }
                        card_rects.push((all_card.card.id.clone(), response.rect));
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
                            );
                            if response.clicked() {
                                actions.open_card = Some(all_card.card.id.clone());
                            }
                            match card_context_menu(&response) {
                                Some(CardMenuAction::Archive) => {
                                    actions.archive_card = Some(all_card.card.id.clone());
                                }
                                Some(CardMenuAction::Delete) => {
                                    actions.delete_card = Some(all_card.card.id.clone());
                                }
                                None => {}
                            }
                            card_rects.push((all_card.card.id.clone(), response.rect));
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
        });

    actions
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
            super::show(&mut ui, &board, None, &mut zones, None);
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
                    super::show(&mut ui, board, drag, &mut zones, hover.clone());
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
