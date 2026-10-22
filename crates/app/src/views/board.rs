//! Real board view: horizontal scroll of columns, drag-and-drop for cards
//! and columns.

use egui::Ui;

use kandrium_core::BoardWithColumns;

use crate::anim;
use crate::dnd::{self, ColKey, DropZone};
use crate::widgets::column::show_column;

/// Extra breathing room between the bottom edge of a capped (scrolling) column
/// and the window's bottom edge, on top of the CentralPanel's 8px inner margin.
/// 15 = 8px visual gutter + 7px lane the horizontal scrollbar reserves when
/// active (the lane is set locally on this ui since the global style reserves
/// none); keeps a capped column's bottom border clear of the h-bar clip.
const BOTTOM_GUTTER: f32 = 15.0;

/// What the user did on the board view this frame.
#[derive(Default)]
pub struct BoardActions {
    pub open_card: Option<String>,
    /// Pencil pressed on a card (open the inline title editor).
    pub edit_card: Option<String>,
    /// Inline title editor submitted (Enter/blur); text lives in the app draft.
    pub edit_commit: Option<String>,
    /// Inline title editor canceled (Esc).
    pub edit_cancel: Option<String>,
    /// Drop performed: (card_id, target_col_id, before_card_id).
    pub card_drop: Option<(String, String, Option<String>)>,
    /// Column reorder: (col_id, before_col_id).
    pub column_drop: Option<(String, Option<String>)>,
    pub archive_all: Option<String>,
    pub delete_column: Option<String>,
    /// Right-click "Archive" on a card chip.
    pub archive_card: Option<String>,
    /// Right-click "Delete" on a card chip.
    pub delete_card: Option<String>,
    pub create_card: Option<(String, String)>,
    pub open_add_form: Option<String>,
    pub cancel_add: Option<String>,
}

struct ColumnLayout {
    id: String,
    body: egui::Rect,
    cards: Vec<(String, egui::Rect)>,
}

/// Render the board. Returns this frame's actions.
///
/// The add-card draft is mutated in place; the submit action tells the view
/// to read + consume it. `edit_card` names the card whose title is being
/// edited inline and `edit_draft` holds its live text (mutated in place).
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    board: &BoardWithColumns,
    drag: Option<&dnd::Drag>,
    zones: &mut Vec<DropZone>,
    add_form_col: Option<&str>,
    add_draft: &mut Option<String>,
    hover: Option<(ColKey, usize)>,
    edit_card: Option<&str>,
    edit_draft: &mut String,
) -> BoardActions {
    let mut out = BoardActions::default();
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
    // ScrollArea / horizontal_top placer. Inside the placer, the nested
    // vertical UIs receive a corrupt available_height (not INFINITY, but
    // also not the real viewport). This caps tall columns so their card
    // list scrolls instead of growing past the window bottom. The gutter
    // reserves extra space below a capped column so its border doesn't
    // sit flush against the window's bottom edge.
    let max_col_height = ui.available_height() - BOTTOM_GUTTER;

    // Reserve the 7px bar lane locally for the board h-scroll (the global
    // style reserves none) — matches the BOTTOM_GUTTER math below.
    ui.spacing_mut().scroll.floating_allocated_width = 7.0;
    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Reset for descendants: the 7px lane is for THIS h-scroll only;
            // nested column v-scrolls supply their own constant content
            // gutter (style Arc is copy-on-write per Ui, so this doesn't
            // touch the outer ui's style used to paint the h-bar).
            ui.spacing_mut().scroll.floating_allocated_width = 0.0;
            ui.spacing_mut().item_spacing.x = col_gap;
            ui.horizontal_top(|ui| {
                let mut layouts: Vec<ColumnLayout> = Vec::new();

                for column in &board.columns {
                    let is_add_target = add_form_col == Some(column.id.as_str());

                    // Insertion gap only for the hovered column.
                    let gap_idx = hover.as_ref().and_then(|(key, idx)| match key {
                        ColKey::Column(col_id) if col_id == &column.id => Some(*idx),
                        _ => None,
                    });

                    let actions = ui
                        .allocate_ui_with_layout(
                            // Finite WIDTH (280px) mandatory — outer horizontal
                            // ScrollArea gives infinite available width, so
                            // each column needs a constrained width or the
                            // inner label wraps one char per line. Height is
                            // f32::INFINITY so each column shrink-wraps its
                            // content (header + cards + add button). Tall
                            // columns self-cap via an inner ScrollArea using
                            // max_col_height so cards never exceed the window.
                            egui::vec2(col_width, f32::INFINITY),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                // Namespace all persistent widget IDs (the
                                // vertical ScrollArea, menu/buttons) per column;
                                // otherwise identical paths clash across columns.
                                ui.push_id(("board-column", column.id.as_str()), |ui| {
                                    show_column(
                                        ui,
                                        column,
                                        dragging_card.as_deref(),
                                        if is_add_target {
                                            add_draft.as_mut()
                                        } else {
                                            None
                                        },
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

                    if actions.add_clicked && add_form_col != Some(column.id.as_str()) {
                        out.open_add_form = Some(column.id.clone());
                    }
                    if actions.add_submitted
                        && let Some(text) = add_draft.as_ref()
                    {
                        let text = text.trim().to_string();
                        if !text.is_empty() {
                            out.create_card = Some((column.id.clone(), text));
                        }
                    }
                    if actions.cancel_add {
                        out.cancel_add = Some(column.id.clone());
                    }
                    if actions.archive_all {
                        out.archive_all = Some(column.id.clone());
                    }
                    if actions.delete {
                        out.delete_column = Some(column.id.clone());
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

                    layouts.push(ColumnLayout {
                        id: column.id.clone(),
                        body: actions.body_rect,
                        cards: actions.card_rects,
                    });
                }

                // Register card drop zones after all columns are laid out.
                for layout in &layouts {
                    let mut cards = layout.cards.clone();
                    if let Some(d) = drag {
                        cards.retain(|(id, _)| id != &d.card_id);
                    }
                    zones.push(DropZone {
                        key: ColKey::Column(layout.id.clone()),
                        rect: layout.body,
                        card_rects: cards,
                    });
                }

                // Drag feedback is the animated insertion gap rendered
                // in-flow by each column (driven by the pre-computed hover).
            });
        });

    out
}

#[cfg(test)]
mod layout_tests {
    use kandrium_core::{BoardWithColumns, Card, Column};

    fn board_with_cards() -> BoardWithColumns {
        let mk = |col_id: &str| Column {
            id: col_id.to_string(),
            title: format!("Column {}", col_id),
            cards: vec![
                Card {
                    id: format!("c-{}", col_id),
                    column_id: col_id.to_string(),
                    title: "Setup the board with a fairly long card title".to_string(),
                    description: String::new(),
                    due_date: String::new(),
                    labels: vec![],
                    ..Default::default()
                },
                // Short title must not shrink the chip to text width.
                Card {
                    id: format!("c-{}-short", col_id),
                    column_id: col_id.to_string(),
                    title: "Hi".to_string(),
                    description: String::new(),
                    due_date: String::new(),
                    labels: vec![],
                    ..Default::default()
                },
            ],
            mirrored: vec![],
        };
        BoardWithColumns {
            id: "b".into(),
            title: "B".into(),
            columns: vec![mk("1"), mk("2"), mk("3")],
        }
    }

    #[test]
    fn columns_get_full_finite_width_and_positions() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let board = board_with_cards();
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
            let mut add_draft = None;
            super::show(
                &mut ui,
                &board,
                None,
                &mut zones,
                None,
                &mut add_draft,
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

        assert_eq!(zones.len(), 3, "one drop zone per column");
        for (i, zone) in zones.iter().enumerate() {
            assert!(
                (zone.rect.width() - 264.0).abs() < 4.0,
                "zone {} width = {} (expected ~264 = 280 - margins)",
                i,
                zone.rect.width()
            );
            // Frame inner margin (8px) + 292px stride (280 width + 12 gap).
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
                // Every chip stretches to the column content width regardless
                // of title length: 264 zone − 8px frame margin each side
                // (minus the 1px bg stroke). A short title used to shrink to
                // ~12px, leaving the right side empty.
                let expected_card_width = zone.rect.width() - 18.0;
                assert!(
                    (card_rect.width() - expected_card_width).abs() < 4.0,
                    "card {j} width = {} (expected ~{} = full column width minus margins)",
                    card_rect.width(),
                    expected_card_width
                );
                // Margins stay uniform on both sides: 8px inner margin + 1px stroke.
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
    fn column_height_scales_with_card_count_and_caps_at_max() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);

        let mk = |title: &str, count: usize| Column {
            id: title.to_string(),
            title: title.to_string(),
            cards: (0..count)
                .map(|i| Card {
                    id: format!("{title}-c{i}"),
                    column_id: title.to_string(),
                    title: format!("Card {i}"),
                    description: String::new(),
                    due_date: String::new(),
                    labels: vec![],
                    ..Default::default()
                })
                .collect(),
            mirrored: vec![],
        };
        let board = BoardWithColumns {
            id: "b".into(),
            title: "B".into(),
            columns: vec![mk("empty", 0), mk("short", 2), mk("tall", 30)],
        };
        let mut zones = Vec::new();
        let mut add_draft = None;
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
                None,
                &mut zones,
                None,
                &mut add_draft,
                None,
                None,
                &mut String::new(),
            );
        });
        output.textures_delta.clear();

        let empty_h = zones[0].rect.height();
        let short_h = zones[1].rect.height();
        let tall_h = zones[2].rect.height();

        // Empty column still has a drop zone (min 30px above add button).
        assert!(
            empty_h >= 30.0,
            "empty body must retain min drop-zone height, got {empty_h}"
        );

        // 2 cards grow naturally (ScrollArea NOT triggered).
        assert!(
            short_h > empty_h,
            "2-card column ({short_h}) must be taller than empty ({empty_h})"
        );
        assert!(
            short_h < 600.0,
            "2-card column ({short_h}) should NOT trigger ScrollArea cap"
        );

        // 30 cards exceed the max → ScrollArea caps at max_body.
        assert!(
            tall_h > short_h,
            "tall column ({tall_h}) must be taller than short ({short_h})"
        );
        assert!(
            (tall_h - short_h).abs() > 100.0,
            "tall ({tall_h}) vs short ({short_h}) delta should be visible"
        );
        assert!(
            (500.0..=700.0).contains(&tall_h),
            "tall column ({tall_h}) should be capped near viewport"
        );
    }

    #[test]
    fn drag_gap_reserves_a_slot_only_in_the_hovered_column() {
        use crate::anim;
        use crate::dnd::{self, ColKey};

        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let board = board_with_cards(); // 2 cards in each of columns "1".."3"
        let dragged = board.columns[1].cards[0].clone(); // "c-2"

        let render = |ctx: &egui::Context,
                      board: &BoardWithColumns,
                      drag: Option<&dnd::Drag>,
                      hover: Option<(ColKey, usize)>,
                      now: f64| {
            let mut zones = Vec::new();
            let mut add_draft = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    // Tests pass timestamps well after GAP_DURATION once the
                    // drag has started, so the placeholder is fully open.
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
                        drag,
                        &mut zones,
                        None,
                        &mut add_draft,
                        hover.clone(),
                        None,
                        &mut String::new(),
                    );
                },
            );
            output.textures_delta.clear();
            zones
        };

        // Frame 1: pristine render populates the rig with card rects.
        let base = render(&ctx, &board, None, None, 0.0);
        let dragged_height = base[1]
            .card_rects
            .iter()
            .find(|(id, _)| id == &dragged.id)
            .map(|(_, r)| r.height())
            .expect("dragged card rect recorded");
        let col1_base = base[0].rect.height();
        let col2_base = base[1].rect.height();

        // Pickup the first card of column "2".
        anim::CardRig::begin_drag(
            &ctx,
            dragged.clone(),
            None,
            base[1].card_rects[0].1.center(),
        );
        let drag = dnd::Drag {
            card_id: dragged.id.clone(),
        };

        // Frame 2: dragging with no hover yet — the source card is gone and
        // no gap is reserved anywhere.
        let lifted = render(&ctx, &board, Some(&drag), None, 10.0);
        assert_eq!(lifted[1].card_rects.len(), 1, "dragged card hidden");
        assert!(
            !lifted[1].card_rects.iter().any(|(id, _)| id == &dragged.id),
            "dragged card must not be collected in card_rects"
        );
        let removed_stride = lifted[1].rect.height();
        assert!(
            removed_stride < col2_base,
            "column must shrink when its card is lifted: {removed_stride} >= {col2_base}"
        );

        // Frame 3: gap opens at the END of column "2" (index 1).
        let hover = Some((ColKey::Column("2".to_string()), 1));
        let gapped = render(&ctx, &board, Some(&drag), hover.clone(), 20.0);

        // The gap reserves one full chip stride: the dragged card's measured
        // height plus the chip Frame chrome on each side — 8px inner margin
        // + 1px stroke + the style's outer item spacing.
        let gap_height = anim::CardRig::gap_height(&ctx);
        assert!(
            (gap_height - dragged_height).abs() < 0.5,
            "fully open gap = {gap_height}, expected card height {dragged_height}"
        );
        let chip_chrome =
            2.0 * (8.0 + 1.0 + ctx.style_of(egui::Theme::Light).spacing.item_spacing.y);
        let restored = gapped[1].rect.height() - removed_stride;
        assert!(
            (restored - (gap_height + chip_chrome)).abs() < 4.0,
            "gap reserve = {restored}, expected card stride ~{}",
            gap_height + chip_chrome
        );
        assert!(
            (gapped[1].rect.height() - col2_base).abs() < 4.0,
            "gapped column height must match the resting two-card layout"
        );
        // Other columns are untouched by the gap.
        assert!(
            (gapped[0].rect.height() - col1_base).abs() < 4.0,
            "gap must not reserve space in non-hovered columns"
        );
        // Gap never pollutes drop-zone geometry.
        assert_eq!(gapped[1].card_rects.len(), 1);
        for zone in &gapped {
            assert!(
                (zone.rect.width() - 264.0).abs() < 4.0,
                "zone width changed: {}",
                zone.rect.width()
            );
        }

        // No duplicate widget IDs while rendering a drag.
        let mut zones = Vec::new();
        let mut add_draft = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(30.0),
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
                    &board,
                    Some(&drag),
                    &mut zones,
                    None,
                    &mut add_draft,
                    hover.clone(),
                    None,
                    &mut String::new(),
                );
            },
        );
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
    }

    #[test]
    fn settle_flight_clears_ghost_for_cancel_and_valid_drop() {
        use crate::anim;
        use crate::dnd::{self, ColKey};
        use egui::Event;

        // Mirrors the rig calls `App::process_drag` makes, minus real pointer
        // hardware. `cancel` releases outside every zone (snap-back); the
        // other session releases over column "2" and swaps to a post-move
        // board fixture (fly-in).
        fn run_session(cancel: bool) {
            let ctx = egui::Context::default();
            crate::theme::install_fonts(&ctx);
            crate::theme::apply_visuals(&ctx, false);
            let board = board_with_cards();

            let frame = |ctx: &egui::Context,
                         board: &BoardWithColumns,
                         drag: Option<&dnd::Drag>,
                         hover: Option<(ColKey, usize)>,
                         now: f64,
                         pointer: Option<egui::Pos2>| {
                let mut zones = Vec::new();
                let mut add_draft = None;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        time: Some(now),
                        events: pointer
                            .map(|p| vec![Event::PointerMoved(p)])
                            .unwrap_or_default(),
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
                            drag,
                            &mut zones,
                            None,
                            &mut add_draft,
                            hover.clone(),
                            None,
                            &mut String::new(),
                        );
                        // Same call site as `App::content`: advances/finishes
                        // the ghost lifecycle.
                        anim::CardRig::draw_ghost(ctx);
                    },
                );
                output.textures_delta.clear();
                zones
            };

            // Frame 0: resting layout + rig rects.
            let base = frame(&ctx, &board, None, None, 0.0, None);
            let start = base[0].card_rects[0].1.center();
            let card = board.columns[0].cards[0].clone();
            anim::CardRig::begin_drag(&ctx, card.clone(), None, start);
            let drag = dnd::Drag {
                card_id: card.id.clone(),
            };
            // Mirror `App::process_drag`: the source slot is the snap-back
            // placeholder position for cancelled drops.
            let source_gap = dnd::hit_test(&base, start).map(|(zi, ii)| (base[zi].key.clone(), ii));

            // Release target: outside every zone (right gutter) or between
            // the two cards of column "2".
            let release_pos = if cancel {
                egui::pos2(950.0, 350.0)
            } else {
                let a = base[1].card_rects[0].1.center_bottom();
                let b = base[1].card_rects[1].1.center_top();
                egui::pos2((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
            };

            // Float phase: ~0.3s of frames following the pointer. Hover is
            // resolved against last frame's zones, exactly like the app.
            let mut last_zones = base;
            let steps = 12;
            for s in 1..=steps {
                let now = f64::from(s) * 0.025;
                let t = s as f32 / steps as f32;
                let pointer = egui::pos2(
                    start.x + (release_pos.x - start.x) * t,
                    start.y + (release_pos.y - start.y) * t,
                );
                let hover = dnd::hit_test(&last_zones, pointer)
                    .map(|(zi, ii)| (last_zones[zi].key.clone(), ii));
                last_zones = frame(&ctx, &board, Some(&drag), hover, now, Some(pointer));
                assert!(
                    anim::CardRig::needs_repaint(&ctx),
                    "frames must be pumped while dragging (step {s})"
                );
            }

            // Button up: freeze the ghost and start the flight. Data only
            // changes when the release lands on a zone.
            anim::CardRig::begin_settle(&ctx, Some(release_pos));
            let drop = dnd::hit_test(&last_zones, release_pos);
            assert_eq!(cancel, drop.is_none(), "hit-test expectation mismatch");

            // Mirror `App::process_drag`: park the placeholder at the drop
            // target (or back at the source slot for a cancel).
            let settle_hover = drop
                .map(|(zi, ii)| (last_zones[zi].key.clone(), ii))
                .or(source_gap);

            let post_board;
            let render_board: &BoardWithColumns = if cancel {
                &board
            } else {
                let (_, insert_idx) = drop.unwrap();
                let mut moved = board_with_cards();
                let lifted = moved.columns[0].cards.remove(0);
                let idx = insert_idx.min(moved.columns[1].cards.len());
                moved.columns[1].cards.insert(idx, lifted);
                post_board = moved;
                &post_board
            };

            // Settle frame: the placeholder is still visible (fully open)
            // and the real card is STILL hidden — it must not render in the
            // drop zone while the ghost flies.
            assert!(anim::CardRig::is_settling(&ctx));
            let settle = frame(
                &ctx,
                render_board,
                Some(&drag),
                settle_hover.clone(),
                0.35,
                None,
            );
            assert!(
                anim::CardRig::needs_repaint(&ctx),
                "ghost flight must keep repainting"
            );
            let gap_now = anim::CardRig::gap_height(&ctx);
            assert!(
                gap_now > 0.0,
                "placeholder stays open during the flight: {gap_now}"
            );
            if cancel {
                assert_eq!(
                    settle[0].card_rects.len(),
                    1,
                    "real card stays hidden mid-flight (placeholder stands in)"
                );
            } else {
                assert_eq!(settle[0].card_rects.len(), 1);
                assert_eq!(
                    settle[1].card_rects.len(),
                    2,
                    "moved card hidden in the drop zone; placeholder reserves its slot"
                );
            }

            // After SETTLE_DURATION the ghost merges: placeholder removed and
            // the real card re-enters, in the same frame's layout.
            let landed = frame(&ctx, render_board, None, None, 1.0, None);
            assert!(
                !anim::CardRig::is_settling(&ctx),
                "ghost must be gone after the settle flight"
            );
            if cancel {
                assert_eq!(landed[0].card_rects.len(), 2, "card returns to source");
                assert!(
                    (landed[0].rect.height() - settle[0].rect.height()).abs() < 4.0,
                    "placeholder hands its slot to the real card with no layout jump"
                );
            } else {
                assert_eq!(landed[0].card_rects.len(), 1);
                assert_eq!(landed[1].card_rects.len(), 3, "card lands in destination");
                assert!(
                    (landed[1].rect.height() - settle[1].rect.height()).abs() < 4.0,
                    "placeholder hands its slot to the real card with no layout jump"
                );
            }

            // Reflow tracks may still be running right at touchdown; past
            // SHIFT_DURATION the rig must be fully idle.
            frame(&ctx, render_board, None, None, 2.0, None);
            assert!(
                !anim::CardRig::needs_repaint(&ctx),
                "rig must be idle once the flight and reflow are done"
            );
        }

        run_session(true); // cancel → snap-back to source
        run_session(false); // valid drop → fly into destination
    }
}
