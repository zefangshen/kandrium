//! Card chip widget: title, due date, label dots.

use egui::{CornerRadius, Label, Sense, TextWrapMode, Ui, Vec2};

use kandrium_core::Card;

use crate::anim;
use crate::dnd;
use crate::icons;
use crate::theme::{self, Tokens};

/// Paint the card's contents (title, board badge, meta row). Shared by the
/// in-flow chip and the floating drag ghost so the two never diverge.
pub(crate) fn card_body(ui: &mut Ui, card: &Card, board_title: Option<&str>) {
    // A Frame shrink-wraps its content, so a short title would make
    // the chip only as wide as the text. Pin the content to the full
    // available width so every card stretches to the column's right
    // edge (margins are defined by the Frame and stay unchanged).
    ui.set_min_width(ui.available_width());

    let t = Tokens::get(ui.ctx());

    // Title
    ui.add(
        Label::new(egui::RichText::new(&card.title).text_style(egui::TextStyle::Button))
            .wrap_mode(TextWrapMode::Wrap),
    );

    // Board badge (All board view)
    if let Some(board_title) = board_title {
        ui.label(
            egui::RichText::new(board_title)
                .text_style(egui::TextStyle::Small)
                .color(t.text_muted),
        );
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

/// Render the card chip. Returns an interactive response (click + drag)
/// covering the whole chip.
///
/// `stagger_order` is the card's distance (in slots) below the open insertion
/// gap, used to stagger the FLIP reorder motion; pass 0 when not dragging.
pub fn card_chip(
    ui: &mut Ui,
    card: &Card,
    board_title: Option<&str>,
    stagger_order: u32,
) -> egui::Response {
    let id = dnd::card_widget_id(&card.id);
    let t = Tokens::get(ui.ctx());

    // Mark the shape range emitted by this chip so the FLIP translation can
    // move the painted card without moving its interaction rect. `Noop`
    // shapes are free placeholders in the layer's paint list.
    let start = ui.painter().add(egui::Shape::Noop);

    let response = egui::Frame::new()
        .fill(t.card)
        .stroke(egui::Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(t.radius))
        .inner_margin(egui::Margin::same(8))
        .outer_margin(egui::Margin::symmetric(0, 3))
        .show(ui, |ui| {
            card_body(ui, card, board_title);

            // Overlay interaction on the full content area.
            let rect = ui.min_rect();
            ui.interact(rect, id, Sense::click_and_drag())
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

    response
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
        .inner_margin(egui::Margin::same(8))
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
