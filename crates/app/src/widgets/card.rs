//! Card chip widget: title, due date, label dots.

use egui::{CornerRadius, Label, Rect, Sense, TextWrapMode, Ui, Vec2};

use kandrium_core::Card;

use crate::anim;
use crate::dnd;
use crate::icons;
use crate::theme::{self, Tokens};

/// Square box reserved in the title row's top-right for the hover pencil
/// button. Kept constant so the title never reflows when the pencil appears.
const CARD_PENCIL_GUTTER: f32 = 26.0;
/// Pencil button box (its icon paints at half this size).
const PENCIL_BOX: f32 = 22.0;
/// Card frame inner padding. The hover target is the content rect expanded by
/// this — i.e. the painted card, not just the text band.
const CARD_PAD: f32 = 8.0;

/// Paint the card's contents (title, board badge, meta row). Shared by the
/// in-flow chip and the floating drag ghost so the two never diverge.
pub(crate) fn card_body(ui: &mut Ui, card: &Card, board_title: Option<&str>) {
    // A Frame shrink-wraps its content, so a short title would make
    // the chip only as wide as the text. Pin the content to the full
    // available width so every card stretches to the column's right
    // edge (margins are defined by the Frame and stay unchanged).
    ui.set_min_width(ui.available_width());

    // Title — narrowed by the pencil gutter so wrapped text never sits under
    // the hover pencil button (the gutter is reserved even while not hovered,
    // so the title does not reflow when the pencil appears).
    ui.allocate_ui_with_layout(
        egui::vec2((ui.available_width() - CARD_PENCIL_GUTTER).max(0.0), 0.0),
        egui::Layout::top_down(egui::Align::LEFT),
        |ui| {
            ui.add(
                Label::new(egui::RichText::new(&card.title).text_style(egui::TextStyle::Button))
                    .wrap_mode(TextWrapMode::Wrap),
            );
        },
    );

    card_meta(ui, card, board_title);
}

/// Board badge + meta row (due date and label dots). Shared by the resting
/// chip and the inline title editor.
fn card_meta(ui: &mut Ui, card: &Card, board_title: Option<&str>) {
    let t = Tokens::get(ui.ctx());

    // Second-row context label on real boards, in the same slot the All board
    // uses for the board name. `get_board` resolves it per board: the area
    // title on a project board, the project identifier elsewhere. On the All
    // board the two share one row: board name left, identifier right-aligned.
    match board_title {
        None => {
            if let Some(label) = &card.meta_label {
                ui.add(
                    Label::new(
                        egui::RichText::new(label)
                            .text_style(egui::TextStyle::Small)
                            .color(t.text_muted),
                    )
                    .wrap_mode(TextWrapMode::Extend),
                );
            }
        }
        Some(board_title) => {
            ui.horizontal(|ui| {
                ui.add(
                    Label::new(
                        egui::RichText::new(board_title)
                            .text_style(egui::TextStyle::Small)
                            .color(t.text_muted),
                    )
                    .wrap_mode(TextWrapMode::Extend),
                );
                if let Some(identifier) = &card.project_identifier {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(
                            Label::new(
                                egui::RichText::new(identifier)
                                    .text_style(egui::TextStyle::Small)
                                    .color(t.text_muted),
                            )
                            .wrap_mode(TextWrapMode::Extend),
                        );
                    });
                }
            });
        }
    }

    // Meta row: due date + label dots
    if !card.due_date.is_empty() || !card.labels.is_empty() {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if !card.due_date.is_empty() {
                let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(10.5, 10.5), Sense::hover());
                icons::paint(ui, icon_rect, "calendar", 10.5, t.text_muted);
                ui.label(
                    egui::RichText::new(&card.due_date)
                        .small()
                        .color(t.text_muted),
                );
            }
            for label in &card.labels {
                if let Some(color) = theme::label_color(*label) {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 6.0), Sense::hover());
                    ui.painter().rect_filled(rect, 3.0, color);
                }
            }
        });
    }
}

/// In-place title editor: same footprint as the resting chip's title row, but
/// the title text becomes a single-line field. Returns `(commit, cancel)`:
/// Enter or blur commits, Esc cancels.
fn card_body_editing(
    ui: &mut Ui,
    card: &Card,
    board_title: Option<&str>,
    draft: &mut String,
) -> (bool, bool) {
    ui.set_min_width(ui.available_width());

    let t = Tokens::get(ui.ctx());
    let edit_id = egui::Id::new(("card-title-edit", card.id.as_str()));
    // Grab focus as soon as the editor opens so typing works immediately.
    ui.ctx().memory_mut(|m| {
        if m.focused().is_none() {
            m.request_focus(edit_id);
        }
    });

    let edit = egui::TextEdit::singleline(draft)
        .hint_text("Card title")
        .desired_width(ui.available_width())
        .min_size(egui::vec2(ui.available_width(), 40.0))
        .frame(
            egui::Frame::new()
                .fill(t.field)
                .stroke(egui::Stroke::new(1.0, t.border))
                .corner_radius(CornerRadius::same(t.radius))
                .inner_margin(egui::Margin::same(CARD_PAD as i8)),
        )
        .id(edit_id);
    let response = ui.add(edit);

    let mut commit = false;
    let mut cancel = false;
    if response.lost_focus() {
        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            commit = true;
        } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        } else {
            // Blur (e.g. clicking another card) saves, Trello-style.
            commit = true;
        }
    }

    card_meta(ui, card, board_title);
    (commit, cancel)
}

/// Result of rendering one card chip.
pub struct CardChip {
    /// Chip click + drag response; carries the content rect for drop zones.
    pub response: egui::Response,
    /// Pencil button pressed — the view should open the inline title editor.
    pub edit_clicked: bool,
    /// The inline editor was submitted (Enter or blur).
    pub edit_commit: bool,
    /// The inline editor was canceled (Esc).
    pub edit_cancel: bool,
}

/// Render the card chip. Returns the interactive response (click + drag)
/// covering the whole chip plus any inline-title-edit signals.
///
/// `stagger_order` is the card's distance (in slots) below the open insertion
/// gap, used to stagger the FLIP reorder motion; pass 0 when not dragging.
///
/// `editing` is `Some(draft)` for the one card whose title is being edited
/// in place; the chip's click/drag is disabled for that card so the editor
/// owns interaction.
///
/// `draggable` is false for mirrored project cards on area boards: they are
/// clickable/editable but their position is owned by the project board.
pub fn card_chip(
    ui: &mut Ui,
    card: &Card,
    board_title: Option<&str>,
    stagger_order: u32,
    editing: Option<&mut String>,
    draggable: bool,
) -> CardChip {
    let id = dnd::card_widget_id(&card.id);
    let t = Tokens::get(ui.ctx());

    // Mark the shape range emitted by this chip so the FLIP translation can
    // move the painted card without moving its interaction rect. `Noop`
    // shapes are free placeholders in the layer's paint list.
    let start = ui.painter().add(egui::Shape::Noop);

    let (response, edit_clicked, edit_commit, edit_cancel) = egui::Frame::new()
        .fill(t.card)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(t.radius))
        .inner_margin(egui::Margin::same(CARD_PAD as i8))
        .outer_margin(egui::Margin::symmetric(0, 3))
        .show(ui, |ui| {
            if let Some(draft) = editing {
                // Inline title editor: no card click/drag — a hover-only
                // placeholder id keeps the content rect for drop zones without
                // stealing clicks from the TextEdit or starting a drag.
                let (commit, cancel) = card_body_editing(ui, card, board_title, draft);
                let rect = ui.min_rect();
                let resp = ui.interact(rect, id.with("editing"), Sense::hover());
                (resp, false, commit, cancel)
            } else {
                card_body(ui, card, board_title);

                // Overlay interaction on the full content area.
                let rect = ui.min_rect();
                let sense = if draggable {
                    Sense::click_and_drag()
                } else {
                    Sense::click()
                };
                let chip = ui.interact(rect, id, sense);

                // Hover pencil, drawn AFTER the chip interact so it is on top
                // for clicks (egui tie-breaks to the last-registered widget)
                // while the chip still wins drags (pencil senses click only).
                // Hover target = the painted card (content + padding), not just
                // the text band, so the pencil does not appear/disappear while
                // the pointer is over the card's padding.
                //
                // Registered with `Ui::interact` rather than allocated: a
                // one-line title row is only ~19.4 px tall (Inter at
                // TextStyle::Button) while the pencil box is 22 px, so
                // allocating it would grow the card — and reflow every card
                // below it — for as long as the pointer rests on the card.
                let mut edit_clicked = false;
                if ui.rect_contains_pointer(rect.expand(CARD_PAD))
                    && ui.ctx().dragged_id().is_none()
                {
                    let pencil_rect = Rect::from_min_size(
                        egui::pos2(rect.right() - PENCIL_BOX, rect.top()),
                        Vec2::splat(PENCIL_BOX),
                    );
                    let pencil_id = egui::Id::new(("card-pencil", card.id.as_str()));
                    let pencil = ui.interact(pencil_rect, pencil_id, Sense::click());
                    let pencil = icons::paint_button(
                        ui,
                        pencil,
                        pencil_rect,
                        PENCIL_BOX,
                        "pencil",
                        t.icon,
                        false,
                        "Edit title",
                    );
                    if pencil.clicked() {
                        edit_clicked = true;
                    }
                }
                (chip, edit_clicked, false, false)
            }
        })
        .inner;

    let end = ui.painter().add(egui::Shape::Noop);

    // Register the laid-out rect and get the paint delta for the glide.
    let delta = anim::CardRig::card_laid_out(ui.ctx(), &card.id, response.rect, stagger_order);
    if delta != Vec2::ZERO {
        // Translate only the shapes between our two markers. Hit-testing and
        // clicks keep using the real layout rect (`response.rect`).
        let layer_id = ui.layer_id();
        ui.ctx().graphics_mut(|g| {
            if let Some(list) = g.get_mut(layer_id) {
                // The start marker is a `Noop`, so including it in the range
                // is harmless (transforming nothing leaves nothing).
                list.transform_range(
                    start,
                    end,
                    egui::epaint::emath::TSTransform::from_translation(delta),
                );
            }
        });
    }

    CardChip {
        response,
        edit_clicked,
        edit_commit,
        edit_cancel,
    }
}

/// Action chosen from a card's right-click context menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardMenuAction {
    Archive,
    Delete,
}

/// Attach the right-click context menu (Archive / Delete) to a card chip
/// response. Returns the chosen action, if any. The popup closes on click
/// via its default `CloseOnClick` behavior; `ui.close()` keeps that explicit.
pub fn card_context_menu(response: &egui::Response) -> Option<CardMenuAction> {
    let mut action = None;
    response.context_menu(|ui| {
        if theme::menu_item(ui, "archive", "Archive", false).clicked() {
            action = Some(CardMenuAction::Archive);
            ui.close();
        }
        if theme::menu_item(ui, "trash-2", "Delete", true).clicked() {
            action = Some(CardMenuAction::Delete);
            ui.close();
        }
    });
    action
}

/// Render the floating drag ghost with elevation derived from `lift`
/// (0 = resting in slot, 1 = fully picked up). Sized/positioned by the
/// caller's foreground [`egui::Area`]; never interactive.
pub(crate) fn ghost_card(ui: &mut Ui, card: &Card, board_title: Option<&str>, lift: f32) {
    let t = Tokens::get(ui.ctx());
    let offset =
        anim::SHADOW_OFFSET_REST + (anim::SHADOW_OFFSET_LIFT - anim::SHADOW_OFFSET_REST) * lift;
    let blur = anim::SHADOW_BLUR_REST + (anim::SHADOW_BLUR_LIFT - anim::SHADOW_BLUR_REST) * lift;
    let alpha =
        anim::SHADOW_ALPHA_REST + (anim::SHADOW_ALPHA_LIFT - anim::SHADOW_ALPHA_REST) * lift;

    egui::Frame::new()
        .fill(t.card)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(t.radius))
        .inner_margin(egui::Margin::same(CARD_PAD as i8))
        .shadow(egui::epaint::Shadow {
            // Integer pixel fields: rounding the interpolated values keeps
            // the diffuse shadow smooth enough at 60fps.
            offset: [0, offset.round() as i8],
            blur: blur.round() as u8,
            spread: 0,
            color: egui::Color32::from_black_alpha((alpha * 255.0).round() as u8),
        })
        .show(ui, |ui| {
            // Subtle translucency while fully lifted; readable at all times.
            ui.multiply_opacity(anim::ghost_opacity(lift));
            card_body(ui, card, board_title);
        });
}

#[cfg(test)]
mod hover_layout_tests {
    use super::*;
    use kandrium_core::Card;

    /// A one-line, no-meta card: its content rect used to be shorter than the
    /// 22 px hover pencil, which made the card grow while hovered.
    fn card() -> Card {
        Card {
            id: "hover-layout".into(),
            column_id: "c".into(),
            title: "One line".into(),
            description: String::new(),
            due_date: String::new(),
            labels: vec![],
            ..Default::default()
        }
    }

    struct Probe {
        /// Chip content rect (what the pencil is anchored to).
        content: Rect,
        /// Laid-out card including the frame's padding and outer margin.
        frame: Rect,
        /// The pencil was clicked this pass.
        edit_clicked: bool,
    }

    /// Run one frame on `ctx` with the given pointer events.
    fn frame(ctx: &egui::Context, events: Vec<egui::Event>) -> Probe {
        let mut probe = Probe {
            content: Rect::NOTHING,
            frame: Rect::NOTHING,
            edit_clicked: false,
        };
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(320.0, 240.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                ui.set_width(280.0);
                let chip = card_chip(ui, &card(), None, 0, None, true);
                probe.content = chip.response.rect;
                probe.frame = ui.min_rect();
                probe.edit_clicked = chip.edit_clicked;
            },
        );
        output.textures_delta.clear();
        probe
    }

    /// Text shapes (text + screen rect) painted by one card render, plus the
    /// chip's content rect.
    fn text_probe(card: &Card, board_title: Option<&str>) -> (Vec<(String, Rect)>, Rect) {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        let mut content = Rect::NOTHING;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(360.0, 300.0),
                )),
                ..Default::default()
            },
            |ui| {
                ui.set_width(280.0);
                let chip = card_chip(ui, card, board_title, 0, None, true);
                content = chip.response.rect;
            },
        );
        let rects = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(t) => {
                    // Right-to-left labels anchor the galley at its right edge,
                    // so `pos` is not always the rect's top-left.
                    let size = t.galley.size();
                    let left = match t.galley.job.halign {
                        egui::Align::LEFT => t.pos.x,
                        egui::Align::Center => t.pos.x - size.x / 2.0,
                        egui::Align::RIGHT => t.pos.x - size.x,
                    };
                    Some((
                        t.galley.job.text.clone(),
                        Rect::from_min_size(egui::pos2(left, t.pos.y), size),
                    ))
                }
                _ => None,
            })
            .collect();
        output.textures_delta.clear();
        (rects, content)
    }

    fn find_text<'a>(rects: &'a [(String, Rect)], text: &str) -> &'a Rect {
        &rects
            .iter()
            .find(|(t, _)| t == text)
            .unwrap_or_else(|| panic!("no text {text:?} in {rects:?}"))
            .1
    }

    #[test]
    fn meta_label_sits_below_title_on_real_boards() {
        let mut card = card();
        card.project_identifier = Some("kan".into());
        card.meta_label = Some("Career".into());
        let (rects, _) = text_probe(&card, None);
        let title = find_text(&rects, "One line");
        let label = find_text(&rects, "Career");
        assert!(
            label.top() >= title.bottom() - 1.0,
            "meta label is not below the title: title={title:?} label={label:?}"
        );
        assert!(
            !rects.iter().any(|(t, _)| t == "kan"),
            "the project identifier must not render on a project board"
        );
    }

    #[test]
    fn real_board_without_meta_label_shows_no_identifier() {
        let mut card = card();
        card.project_identifier = Some("kan".into());
        let (rects, _) = text_probe(&card, None);
        assert!(
            !rects.iter().any(|(t, _)| t == "kan"),
            "unfiled project cards have no second-row label"
        );
    }

    #[test]
    fn all_board_shows_identifier_right_of_board_name() {
        let mut card = card();
        card.project_identifier = Some("kan".into());
        let (rects, content) = text_probe(&card, Some("Career"));
        let title = find_text(&rects, "One line");
        let board = find_text(&rects, "Career");
        let ident = find_text(&rects, "kan");
        assert!(
            ident.top() >= title.bottom() - 1.0,
            "identifier should be in the meta row: title={title:?} ident={ident:?}"
        );
        assert!(
            (board.center().y - ident.center().y).abs() < 2.0,
            "identifier should share the board-name row: board={board:?} ident={ident:?}"
        );
        assert!(
            ident.left() > board.right(),
            "identifier should be to the right of the board name: board={board:?} ident={ident:?}"
        );
        assert!(
            (ident.right() - content.right()).abs() < 1.5,
            "identifier should be right-aligned: board={board:?} ident={ident:?} content={content:?}"
        );
    }

    fn click_events(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn hover_pencil_does_not_change_card_layout() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);

        // Pointer well outside the card.
        let resting = frame(
            &ctx,
            vec![egui::Event::PointerMoved(egui::pos2(310.0, 200.0))],
        );
        assert!(resting.frame.is_positive(), "card should lay out");

        // Centre of the hover pencil: it is anchored to the content's top-right.
        let pencil_center = egui::pos2(
            resting.content.right() - PENCIL_BOX / 2.0,
            resting.content.top() + PENCIL_BOX / 2.0,
        );

        let hovered = frame(&ctx, vec![egui::Event::PointerMoved(pencil_center)]);
        assert!(
            (hovered.frame.min - resting.frame.min).length() < 0.01
                && (hovered.frame.max - resting.frame.max).length() < 0.01,
            "hover changed the card layout: {:?} -> {:?}",
            resting.frame,
            hovered.frame
        );

        // Clicking the pencil proves the hover overlay really is live, so the
        // layout comparison above is not vacuous.
        let clicked = frame(&ctx, click_events(pencil_center));
        assert!(clicked.edit_clicked, "pencil click should open the editor");
        assert!(
            (clicked.frame.min - resting.frame.min).length() < 0.01
                && (clicked.frame.max - resting.frame.max).length() < 0.01,
            "clicking the pencil changed the card layout: {:?} -> {:?}",
            resting.frame,
            clicked.frame
        );
    }
}
