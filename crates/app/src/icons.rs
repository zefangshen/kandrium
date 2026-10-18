//! Lucide SVG icons (ISC license), embedded and tinted at runtime.

use std::collections::HashMap;
use std::sync::OnceLock;

use egui::{Color32, Image, ImageSource, Rect, Response, Sense, Stroke, Vec2, vec2};

use crate::theme::{self, Tokens};

static ICONS: &[(&str, &[u8])] = &[
    ("archive", include_bytes!("../assets/icons/archive.svg")),
    ("calendar", include_bytes!("../assets/icons/calendar.svg")),
    ("check", include_bytes!("../assets/icons/check.svg")),
    (
        "chevron-down",
        include_bytes!("../assets/icons/chevron-down.svg"),
    ),
    (
        "chevron-right",
        include_bytes!("../assets/icons/chevron-right.svg"),
    ),
    ("ellipsis", include_bytes!("../assets/icons/ellipsis.svg")),
    ("eye", include_bytes!("../assets/icons/eye.svg")),
    ("file", include_bytes!("../assets/icons/file.svg")),
    ("file-plus", include_bytes!("../assets/icons/file-plus.svg")),
    ("folder", include_bytes!("../assets/icons/folder.svg")),
    (
        "folder-plus",
        include_bytes!("../assets/icons/folder-plus.svg"),
    ),
    ("moon", include_bytes!("../assets/icons/moon.svg")),
    ("pencil", include_bytes!("../assets/icons/pencil.svg")),
    ("plus", include_bytes!("../assets/icons/plus.svg")),
    (
        "rotate-ccw",
        include_bytes!("../assets/icons/rotate-ccw.svg"),
    ),
    ("rotate-cw", include_bytes!("../assets/icons/rotate-cw.svg")),
    ("save", include_bytes!("../assets/icons/save.svg")),
    ("search", include_bytes!("../assets/icons/search.svg")),
    ("square", include_bytes!("../assets/icons/square.svg")),
    (
        "square-kanban",
        include_bytes!("../assets/icons/square-kanban.svg"),
    ),
    (
        "sticky-note",
        include_bytes!("../assets/icons/sticky-note.svg"),
    ),
    ("sun", include_bytes!("../assets/icons/sun.svg")),
    ("trash-2", include_bytes!("../assets/icons/trash-2.svg")),
    ("x", include_bytes!("../assets/icons/x.svg")),
];

fn sources() -> &'static HashMap<&'static str, ImageSource<'static>> {
    static MAP: OnceLock<HashMap<&'static str, ImageSource<'static>>> = OnceLock::new();
    MAP.get_or_init(|| {
        ICONS
            .iter()
            .map(|(name, bytes)| {
                let s = String::from_utf8_lossy(bytes)
                    .replace("currentColor", "#ffffff")
                    .replace("stroke-width=\"2\"", "stroke-width=\"1.75\"");
                let uri: std::borrow::Cow<'static, str> =
                    format!("bytes://icons/{name}.svg").into();
                (
                    *name,
                    ImageSource::Bytes {
                        uri,
                        bytes: egui::load::Bytes::from(s.into_bytes()),
                    },
                )
            })
            .collect()
    })
}

fn source(name: &str) -> ImageSource<'static> {
    sources()
        .get(name)
        .cloned()
        .unwrap_or_else(|| sources()["square"].clone())
}

pub fn image(name: &str, size: f32, tint: Color32) -> Image<'static> {
    Image::new(source(name))
        .fit_to_exact_size(Vec2::splat(size))
        .tint(tint)
}

pub fn paint(ui: &egui::Ui, rect: Rect, name: &str, size: f32, tint: Color32) {
    image(name, size, tint).paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(size)));
}

/// Square icon button with an explicit icon tint (e.g. error red for
/// destructive actions). Same wash behavior as [`button`].
pub fn button_tinted(
    ui: &mut egui::Ui,
    name: &str,
    box_size: f32,
    tint: Color32,
    tooltip: &str,
) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(box_size, box_size), Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            if tooltip.is_empty() { name } else { tooltip },
        )
    });
    let press = theme::Press::track(ui, &resp);
    let fill = press.wash_color(&t, false);
    if fill.a() > 0 {
        ui.painter().rect_filled(rect, t.radius, fill);
    }
    let size = (box_size * 0.5).round() * (1.0 - 0.08 * press.down);
    paint(ui, rect.translate(press.offset()), name, size, tint);
    let resp = theme::hand(resp);
    if tooltip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tooltip)
    }
}

/// Square icon button: transparent until hovered; `selected` gets the accent treatment.
pub fn button(
    ui: &mut egui::Ui,
    name: &str,
    box_size: f32,
    selected: bool,
    tooltip: &str,
) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(box_size, box_size), Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            if tooltip.is_empty() { name } else { tooltip },
        )
    });
    let press = theme::Press::track(ui, &resp);
    let fill = press.wash_color(&t, selected);
    if fill.a() > 0 {
        ui.painter().rect_filled(rect, t.radius, fill);
    }
    if selected {
        ui.painter().rect_stroke(
            rect.shrink(0.5),
            t.radius,
            Stroke::new(1.0, t.accent.gamma_multiply(0.45)),
            egui::StrokeKind::Inside,
        );
    }
    let size = (box_size * 0.5).round() * (1.0 - 0.08 * press.down);
    paint(
        ui,
        rect.translate(press.offset()),
        name,
        size,
        if selected { t.accent_text } else { t.icon },
    );
    let resp = theme::hand(resp);
    if tooltip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tooltip)
    }
}
