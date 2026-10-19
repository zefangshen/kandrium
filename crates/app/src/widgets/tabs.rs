//! Shared document-tab widget used by the Notes and Documents views: one
//! tab with name, optional dirty dot, close ×, and an accent underline for
//! the active tab (mirroring `theme::tab_button`).

use egui::Rect;

use crate::theme::{self, Press, Tokens};

pub const TAB_H: f32 = 30.0;

pub struct TabResponse {
    inner: egui::Response,
    pub close_clicked: bool,
}

impl TabResponse {
    pub fn clicked(&self) -> bool {
        self.inner.clicked() && !self.close_clicked
    }
    pub fn middle_clicked(&self) -> bool {
        self.inner.middle_clicked()
    }
}

/// One tab: name, optional dirty dot, close ×. Active tabs get the accent
/// underline and stronger text.
pub fn tab_widget(
    ui: &mut egui::Ui,
    name: &str,
    active: bool,
    dirty: bool,
    id_salt: impl egui::AsIdSalt,
) -> TabResponse {
    let t = Tokens::get(ui.ctx());
    ui.push_id(id_salt, |ui| {
        let text_style = if active {
            theme::small_strong()
        } else {
            egui::TextStyle::Small
        };
        let text_color = if active { t.accent_text } else { t.text_muted };
        let galley = ui.painter().layout_no_wrap(
            name.to_string(),
            theme::font_id(ui, text_style),
            text_color,
        );

        let close_w = 18.0;
        let dirty_w = if dirty { 10.0 } else { 0.0 };
        let width = 10.0 + galley.size().x + dirty_w + close_w + 8.0;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, TAB_H), egui::Sense::click());
        let press = Press::track(ui, &resp);
        press.wash(ui, rect, egui::CornerRadius::same(t.radius), false);

        let cy = rect.center().y + press.offset().y;
        let text_w = galley.size().x;
        let tx = rect.left() + 10.0;
        ui.painter().galley(
            egui::pos2(tx, cy - galley.size().y * 0.5),
            galley,
            text_color,
        );

        let mut x = tx + text_w;
        if dirty {
            ui.painter()
                .circle_filled(egui::pos2(x + 6.0, cy), 2.5, t.accent);
            x += dirty_w;
        }

        // Close button.
        let close_rect = egui::Rect::from_min_size(
            egui::pos2(x + 2.0, rect.top() + (TAB_H - 16.0) * 0.5),
            egui::vec2(16.0, 16.0),
        );
        let close_resp = ui.interact(close_rect, ui.id().with("close"), egui::Sense::click());
        let close_hover = ui.ctx().animate_bool_with_time_and_easing(
            close_resp.id.with("hover"),
            close_resp.hovered(),
            0.12,
            egui::emath::easing::quadratic_out,
        );
        if close_hover > 0.0 {
            ui.painter().rect_filled(
                close_rect,
                egui::CornerRadius::same(4),
                t.hover.gamma_multiply(close_hover),
            );
        }
        crate::icons::paint(
            ui,
            Rect::from_center_size(close_rect.center(), egui::Vec2::splat(11.0)),
            "x",
            11.0,
            if close_resp.hovered() {
                t.icon
            } else {
                t.text_faint
            },
        );

        // Active underline accent.
        if active {
            let y = rect.bottom() - 1.5;
            ui.painter().rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left() + 6.0, y - 2.0),
                    egui::pos2(rect.right() - 6.0, y),
                ),
                egui::CornerRadius::same(1),
                t.accent,
            );
        }

        TabResponse {
            close_clicked: close_resp.clicked(),
            inner: resp,
        }
    })
    .inner
}
