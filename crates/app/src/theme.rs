//! Design tokens: color palette, fonts, spacing, Press-wash helpers.
//!
//! Ported from PdfCraft's `ui-egui/src/theme.rs` — neutral chrome, white
//! panels, one blue accent. The existing Trello label palette is unchanged.

use egui::{
    Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Rect,
    Response, Sense, Stroke, TextStyle, Vec2, Visuals,
};
use std::sync::Arc;

/// Trello-style label palette (8 colors, index = stored label id).
pub const LABEL_COLORS: [(&str, Color32); 8] = [
    ("Green", Color32::from_rgb(0x61, 0xbd, 0x4f)),
    ("Yellow", Color32::from_rgb(0xf5, 0xcd, 0x28)),
    ("Orange", Color32::from_rgb(0xff, 0x9f, 0x1a)),
    ("Red", Color32::from_rgb(0xeb, 0x5a, 0x46)),
    ("Purple", Color32::from_rgb(0xc3, 0x77, 0xe0)),
    ("Blue", Color32::from_rgb(0x00, 0x79, 0xbf)),
    ("Sky", Color32::from_rgb(0x00, 0xc2, 0xe8)),
    ("Black", Color32::from_rgb(0x17, 0x2b, 0x4d)),
];

/// Color for a label index, if in range.
pub fn label_color(index: i64) -> Option<Color32> {
    LABEL_COLORS.get(index as usize).map(|(_, c)| *c)
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// Design tokens for the app. Stored per-frame in `Context` temp memory.
#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub dark: bool,
    /// Title bar / toolbar background.
    pub titlebar: Color32,
    /// Panels and column cards.
    pub panel: Color32,
    /// Background behind columns (the "canvas").
    pub pasteboard: Color32,
    /// Card / dialog background.
    pub card: Color32,
    pub border: Color32,
    pub divider: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,
    pub icon: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub selected: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    pub accent_soft: Color32,
    /// Text-field background.
    pub field: Color32,
    pub radius: u8,
}

impl Tokens {
    pub fn for_kind(dark: bool) -> Self {
        if dark {
            Self {
                dark: true,
                titlebar: Color32::from_rgb(0x1B, 0x1B, 0x1E),
                panel: Color32::from_rgb(0x26, 0x26, 0x2A),
                pasteboard: Color32::from_rgb(0x19, 0x19, 0x1C),
                card: Color32::from_rgb(0x2E, 0x2E, 0x33),
                border: Color32::from_rgb(0x3C, 0x3C, 0x43),
                divider: Color32::from_rgb(0x33, 0x33, 0x39),
                text: Color32::from_rgb(0xEC, 0xEC, 0xEF),
                text_muted: Color32::from_rgb(0xAE, 0xAE, 0xB6),
                text_faint: Color32::from_rgb(0x97, 0x97, 0x9E),
                icon: Color32::from_rgb(0xD4, 0xD4, 0xDA),
                hover: Color32::from_rgb(0x34, 0x34, 0x3A),
                pressed: Color32::from_rgb(0x3E, 0x3E, 0x45),
                selected: Color32::from_rgb(0x23, 0x3A, 0x63),
                accent: Color32::from_rgb(0x4B, 0x8B, 0xF5),
                accent_text: Color32::from_rgb(0x8C, 0xB6, 0xFA),
                accent_soft: Color32::from_rgb(0x24, 0x36, 0x57),
                field: Color32::from_rgb(0x1E, 0x1E, 0x22),
                radius: 6,
            }
        } else {
            Self {
                dark: false,
                titlebar: Color32::from_rgb(0xE9, 0xE9, 0xEB),
                panel: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                pasteboard: Color32::from_rgb(0xF1, 0xF1, 0xF3),
                card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                border: Color32::from_rgb(0xDA, 0xDA, 0xDE),
                divider: Color32::from_rgb(0xE8, 0xE8, 0xEB),
                text: Color32::from_rgb(0x22, 0x22, 0x26),
                text_muted: Color32::from_rgb(0x5E, 0x5E, 0x66),
                text_faint: Color32::from_rgb(0x6B, 0x6B, 0x73),
                icon: Color32::from_rgb(0x44, 0x44, 0x4B),
                hover: Color32::from_rgb(0xF0, 0xF0, 0xF3),
                pressed: Color32::from_rgb(0xE4, 0xE4, 0xE9),
                selected: Color32::from_rgb(0xE6, 0xEE, 0xFD),
                accent: Color32::from_rgb(0x1B, 0x63, 0xE0),
                accent_text: Color32::from_rgb(0x17, 0x55, 0xC4),
                accent_soft: Color32::from_rgb(0xE3, 0xEC, 0xFD),
                field: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                radius: 6,
            }
        }
    }

    pub fn get(ctx: &Context) -> Self {
        ctx.data(|d| d.get_temp::<Tokens>(egui::Id::new("kandrium-tokens")))
            .unwrap_or_else(|| Self::for_kind(false))
    }

    pub fn set(&self, ctx: &Context) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("kandrium-tokens"), *self));
    }
}

/// Pointing hand, the cursor for a control that clicks.
pub fn hand(resp: egui::Response) -> egui::Response {
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

pub const FONT_REGULAR: &str = "Inter-Regular";
pub const FONT_MEDIUM: &str = "Inter-Medium";
pub const FONT_SEMIBOLD: &str = "Inter-SemiBold";
pub const FONT_MONO: &str = "JetBrainsMono-Regular";

/// Install Inter + JetBrains Mono, plus named medium/semibold families.
pub fn install_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();

    let mut insert = |name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.to_string(), Arc::new(FontData::from_static(bytes)));
    };

    insert(
        FONT_REGULAR,
        include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    );
    insert(
        FONT_MEDIUM,
        include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    );
    insert(
        FONT_SEMIBOLD,
        include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    );
    insert(
        FONT_MONO,
        include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    );

    // Proportional = Inter regular, Monospace = JetBrains Mono.
    if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
        family.insert(0, FONT_REGULAR.to_string());
    }
    if let Some(family) = fonts.families.get_mut(&FontFamily::Monospace) {
        family.insert(0, FONT_MONO.to_string());
    }

    // Named families for medium / semibold variants.
    fonts.families.insert(
        FontFamily::Name("medium".into()),
        vec![FONT_MEDIUM.to_string(), FONT_REGULAR.to_string()],
    );
    fonts.families.insert(
        FontFamily::Name("semibold".into()),
        vec![FONT_SEMIBOLD.to_string(), FONT_REGULAR.to_string()],
    );

    ctx.set_fonts(fonts);
}

// ---------------------------------------------------------------------------
// Text styles
// ---------------------------------------------------------------------------
//
// Every font size in the app comes from the `text_styles` table installed in
// [`apply_visuals`]. Widgets pick a [`TextStyle`] (standard or one of the
// named styles below) and resolve it against the current style — sizes are
// never passed around at call sites.

/// Semibold 16: column titles, active board tabs.
pub fn strong() -> TextStyle {
    TextStyle::Name("strong".into())
}

/// Semibold 13: uppercase section titles, table column headers.
pub fn small_strong() -> TextStyle {
    TextStyle::Name("small-strong".into())
}

/// Resolve a [`TextStyle`] to a concrete [`FontId`] using the ui's current
/// style, for painter-drawn text (`layout_no_wrap`).
pub fn font_id(ui: &egui::Ui, style: TextStyle) -> FontId {
    style.resolve(ui.style())
}

// ---------------------------------------------------------------------------
// Press wash
// ---------------------------------------------------------------------------

/// Hover/press state for chrome controls. `hover` eases a wash fill in;
/// `down` shifts the control content 1px while pressed.
pub struct Press {
    pub hover: f32,
    pub down: f32,
}

impl Press {
    pub fn track(ui: &egui::Ui, response: &egui::Response) -> Self {
        Self {
            hover: ui.ctx().animate_bool_with_time_and_easing(
                response.id.with("hover"),
                response.hovered(),
                0.12,
                egui::emath::easing::quadratic_out,
            ),
            down: ui.ctx().animate_bool_with_time_and_easing(
                response.id.with("down"),
                response.is_pointer_button_down_on(),
                0.06,
                egui::emath::easing::quadratic_out,
            ),
        }
    }

    /// Vertical press offset (1px down at full press).
    pub fn offset(&self) -> Vec2 {
        Vec2::new(0.0, self.down.round())
    }

    /// Ease fill between rest → hover → pressed.
    pub fn fill(&self, rest: Color32, hover: Color32, pressed: Color32) -> Color32 {
        let h = hover.gamma_multiply(self.hover);
        let p = pressed.gamma_multiply(self.down);
        let base = rest.lerp_to_gamma(h, self.hover);
        base.lerp_to_gamma(p, self.down)
    }

    /// The color [`Self::wash`] would paint (used by icon buttons that draw
    /// their own rect).
    pub fn wash_color(&self, t: &Tokens, selected: bool) -> Color32 {
        if selected {
            self.fill(t.accent_soft, t.accent_soft, t.selected)
        } else {
            self.fill(Color32::TRANSPARENT, t.hover, t.pressed)
        }
    }

    /// Paint a rounded wash behind a control.
    pub fn wash(&self, ui: &egui::Ui, rect: Rect, radius: CornerRadius, selected: bool) {
        let t = Tokens::get(ui.ctx());
        let fill = self.wash_color(&t, selected);
        if fill.a() > 0 {
            ui.painter().rect_filled(rect, radius, fill);
        }
        if selected {
            ui.painter().rect_stroke(
                rect.shrink(0.5),
                radius,
                Stroke::new(1.0, t.accent.gamma_multiply(0.45)),
                egui::StrokeKind::Inside,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Style application
// ---------------------------------------------------------------------------

/// Apply the full style (visuals + spacing + scrollbars + text styles).
pub fn apply_visuals(ctx: &Context, dark: bool) {
    let t = Tokens::for_kind(dark);
    t.set(ctx);

    let mut visuals = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    // Fills
    visuals.panel_fill = t.panel;
    visuals.window_fill = t.card;
    visuals.window_stroke = Stroke::new(1.0, t.border);
    visuals.extreme_bg_color = t.field;
    visuals.faint_bg_color = t.pasteboard;

    // Selection + hyperlinks
    visuals.selection.bg_fill = t.selected;
    visuals.selection.stroke = Stroke::new(1.0, t.accent);
    visuals.hyperlink_color = t.accent_text;

    // Override text color: default text reads from tokens.
    visuals.override_text_color = Some(t.text);

    // Radii
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.menu_corner_radius = CornerRadius::same(8);

    // Shadows
    visuals.window_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 110 } else { 38 }),
    };
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 90 } else { 30 }),
    };

    // Widgets
    let w = &mut visuals.widgets;
    w.noninteractive.corner_radius = CornerRadius::same(6);
    w.inactive.corner_radius = CornerRadius::same(6);
    w.hovered.corner_radius = CornerRadius::same(6);
    w.active.corner_radius = CornerRadius::same(6);
    w.open.corner_radius = CornerRadius::same(6);

    w.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.text);

    w.inactive.bg_fill = t.hover;
    w.inactive.weak_bg_fill = t.hover;
    w.inactive.bg_stroke = Stroke::new(1.0, t.border);
    w.inactive.fg_stroke = Stroke::new(1.0, t.text);

    w.hovered.bg_fill = t.hover;
    w.hovered.weak_bg_fill = t.hover;
    w.hovered.bg_stroke = Stroke::new(1.0, t.border);
    w.hovered.fg_stroke = Stroke::new(1.0, t.text);

    w.active.bg_fill = t.pressed;
    w.active.weak_bg_fill = t.pressed;
    w.active.bg_stroke = Stroke::new(1.0, t.border);
    w.active.fg_stroke = Stroke::new(1.0, t.text);

    w.open.bg_fill = t.hover;
    w.open.weak_bg_fill = t.hover;
    w.open.bg_stroke = Stroke::new(1.0, t.border);
    w.open.fg_stroke = Stroke::new(1.0, t.text);

    // Cursors
    visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);

    // Text rendering: crisper at 100–150% scaling.
    visuals.text_options.subpixel_binning = false;
    if !dark {
        visuals.text_options.color_transfer_function =
            egui::epaint::FontColorTransferFunction::Gamma(0.75);
    }

    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        // Spacing
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.spacing.menu_margin = Margin::same(6);
        style.spacing.interact_size.y = 22.0;
        style.spacing.indent = 20.0;

        // Floating scrollbars. With floating_allocated_width 0 the bar
        // reserves NO layout space, so showing/hiding it can never shift
        // content — callers that must keep content clear of the bar give
        // their ScrollArea a constant right/bottom content gutter
        // (`content_margin`) or locally restore a 7px lane via
        // `ui.spacing_mut().scroll`. The bar is a constant 3px (no hover
        // expansion) and the handle stays visible at 0.35 opacity whenever
        // scrolling is possible, brightening on hover (0.6 over the area,
        // 1.0 over the bar); no track background.
        style.spacing.scroll = egui::style::ScrollStyle {
            floating: true,
            bar_width: 3.0,
            floating_width: 3.0,
            bar_inner_margin: 6.0,
            bar_outer_margin: 1.0,
            floating_allocated_width: 0.0,
            dormant_handle_opacity: 0.35,
            dormant_background_opacity: 0.0,
            active_background_opacity: 0.0,
            interact_background_opacity: 0.0,
            ..style.spacing.scroll
        };

        // Text styles — the single source of truth for app font sizes.
        // Standard styles: Small 13 (regular), Body 16 (regular),
        // Button 16 (medium), Heading 20 (semibold), Monospace 16.
        // Named styles: "strong" = semibold 16, "small-strong" = semibold 13.
        let medium_family = FontFamily::Name("medium".into());
        let semibold_family = FontFamily::Name("semibold".into());
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(13.0)),
            (TextStyle::Body, FontId::proportional(16.0)),
            (TextStyle::Button, FontId::new(16.0, medium_family)),
            (
                TextStyle::Heading,
                FontId::new(20.0, semibold_family.clone()),
            ),
            (TextStyle::Monospace, FontId::monospace(16.0)),
            (strong(), FontId::new(16.0, semibold_family)),
            (
                small_strong(),
                FontId::new(13.0, FontFamily::Name("semibold".into())),
            ),
        ]
        .into();
    });
}

/// Tighten the spacing for a `Modal` dialog body.
pub fn dialog_frame(ctx: &Context) -> egui::Frame {
    let t = Tokens::get(ctx);
    egui::Frame::new()
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(20, 16))
        .shadow(egui::Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(if t.dark { 110 } else { 38 }),
        })
}

/// Board tab: text with accent underline when active, hover wash otherwise.
/// Used for the 13 px board-tab row (Small/small-strong at 24 px); the
/// top-level section row is a [`segmented_control`] instead. Text styles
/// come from the `text_styles` table (see `apply_visuals`).
pub fn tab_button(
    ui: &mut egui::Ui,
    label: &str,
    active: bool,
    inactive_style: TextStyle,
    active_style: TextStyle,
    height: f32,
) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let text_style = if active { active_style } else { inactive_style };
    let color = if active { t.accent_text } else { t.text_muted };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id(ui, text_style), color);

    let hpad = if height < 30.0 { 8.0 } else { 12.0 };
    let width = galley.size().x + hpad * 2.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::click());
    let press = Press::track(ui, &resp);

    if !active {
        press.wash(ui, rect, CornerRadius::same(t.radius), false);
    }

    ui.painter().galley(
        egui::pos2(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5 + press.offset().y,
        ),
        galley,
        color,
    );

    if active {
        let y = rect.bottom() - 2.0;
        // Underline extends 2 px past the text on each side.
        let inset = hpad - 2.0;
        ui.painter().rect_filled(
            Rect::from_min_max(
                egui::pos2(rect.left() + inset, y - 2.0),
                egui::pos2(rect.right() - inset, y),
            ),
            1.0,
            t.accent,
        );
    }

    resp
}

/// A segment of a [`segmented_control`].
pub struct Segment<'a> {
    pub label: &'a str,
    pub icon: &'a str,
}

/// macOS-style segmented control: rounded inset container; the selected
/// segment gets a soft accent fill with accent-colored icon + label.
/// Returns the clicked segment index, if any.
pub fn segmented_control(
    ui: &mut egui::Ui,
    segments: &[Segment<'_>],
    selected: usize,
) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    const SEG_H: f32 = 26.0;
    const ICON: f32 = 14.0;
    const GAP: f32 = 6.0;
    const HPAD: f32 = 12.0;

    // Resolve galleys up front: semibold when selected, regular otherwise.
    // The text color is baked into the galley; keep it alongside for the
    // painter's fallback-color argument.
    let galleys: Vec<_> = segments
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            let style = if i == selected {
                small_strong()
            } else {
                TextStyle::Small
            };
            let color = if i == selected {
                t.accent_text
            } else {
                t.text_muted
            };
            let galley =
                ui.painter()
                    .layout_no_wrap(seg.label.to_string(), font_id(ui, style), color);
            (galley, color)
        })
        .collect();

    let frame = egui::Frame::new()
        .fill(t.pasteboard)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(t.radius))
        .inner_margin(Margin::same(2));

    let mut clicked = None;
    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (i, seg) in segments.iter().enumerate() {
                let (galley, color) = &galleys[i];
                let width = HPAD + ICON + GAP + galley.size().x + HPAD;
                let (rect, resp) =
                    ui.allocate_exact_size(Vec2::new(width, SEG_H), egui::Sense::click());
                let press = Press::track(ui, &resp);

                if i == selected {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(t.radius), t.accent_soft);
                } else {
                    press.wash(ui, rect, CornerRadius::same(t.radius), false);
                }

                // Centered icon + label, shifted 1px down while pressed.
                let content_w = ICON + GAP + galley.size().x;
                let mut x = rect.center().x - content_w * 0.5;
                let y_off = press.offset().y;
                let icon_rect = Rect::from_center_size(
                    egui::pos2(x + ICON * 0.5, rect.center().y + y_off),
                    Vec2::splat(ICON),
                );
                let icon_color = if i == selected {
                    t.accent_text
                } else {
                    t.text_faint
                };
                crate::icons::paint(ui, icon_rect, seg.icon, ICON, icon_color);
                x += ICON + GAP;
                ui.painter().galley(
                    egui::pos2(x, rect.center().y - galley.size().y * 0.5 + y_off),
                    galley.clone(),
                    *color,
                );

                if hand(resp).clicked() {
                    clicked = Some(i);
                }
            }
        });
    });
    clicked
}

/// Rounded search field with a magnifying-glass icon prefix and an accent
/// focus ring. The horizontal text inset (8px) matches the card chip's
/// inner margin so the typed text lines up with card titles.
pub fn search_field(ui: &mut egui::Ui, text: &mut String, hint: &str, width: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let frame = egui::Frame::new()
        .fill(t.field)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(t.radius))
        .inner_margin(Margin::symmetric(8, 5));

    let mut focused = false;
    let resp = frame
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let icon_size = 14.0;
                let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(icon_size), Sense::hover());
                crate::icons::paint(ui, icon_rect, "search", icon_size, t.text_faint);
                ui.add_space(6.0);
                // Frame inner margin (8*2) + icon (14) + gap (6) = 36px of chrome.
                let text_w = (width - 36.0).max(0.0);
                let r = ui.add(
                    egui::TextEdit::singleline(text)
                        .hint_text(hint)
                        .desired_width(text_w)
                        .frame(egui::Frame::new())
                        .margin(Vec2::ZERO),
                );
                focused = r.has_focus();
            });
        })
        .response;

    if focused {
        ui.painter().rect_stroke(
            resp.rect.shrink(0.5),
            CornerRadius::same(t.radius),
            Stroke::new(1.5, t.accent),
            egui::StrokeKind::Inside,
        );
    }

    resp
}

/// Ghost button: transparent until hovered, 30px tall, icon + label.
pub fn ghost_button(ui: &mut egui::Ui, icon: Option<&str>, label: &str) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), font_id(ui, TextStyle::Body), t.text);

    let icon_size = 16.0;
    let icon_gap = 6.0;
    let text_w = galley.size().x;
    let total_w = if icon.is_some() {
        icon_size + icon_gap + text_w
    } else {
        text_w
    } + 20.0;
    let height = 30.0;

    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, height), egui::Sense::click());
    let press = Press::track(ui, &resp);
    press.wash(ui, rect, CornerRadius::same(t.radius), false);

    let mut x = rect.left() + 10.0;
    let cy = rect.center().y + press.offset().y;

    if let Some(name) = icon {
        let icon_rect =
            Rect::from_center_size(egui::pos2(x + icon_size * 0.5, cy), Vec2::splat(icon_size));
        crate::icons::paint(ui, icon_rect, name, icon_size, t.icon);
        x += icon_size + icon_gap;
    }

    ui.painter()
        .galley(egui::pos2(x, cy - galley.size().y * 0.5), galley, t.text);

    resp
}

/// Pill button: 30px tall, radius 15. Primary = accent fill + white text.
/// Danger = red fill + white text (used for destructive actions).
pub fn pill_button(ui: &mut egui::Ui, label: &str, primary: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        font_id(ui, TextStyle::Button),
        if primary { Color32::WHITE } else { t.text },
    );

    let width = galley.size().x + 32.0;
    let height = 30.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::click());
    let press = Press::track(ui, &resp);

    if primary {
        let fill = press.fill(t.accent, t.accent, t.accent);
        ui.painter().rect_filled(rect, CornerRadius::same(15), fill);
    } else {
        press.wash(ui, rect, CornerRadius::same(15), false);
        ui.painter().rect_stroke(
            rect.shrink(0.5),
            CornerRadius::same(15),
            Stroke::new(1.0, t.border),
            egui::StrokeKind::Inside,
        );
    }

    ui.painter().galley(
        egui::pos2(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5 + press.offset().y,
        ),
        galley,
        if primary { Color32::WHITE } else { t.text },
    );

    resp
}

/// Destructive pill button: error-red fill + white text.
pub fn danger_pill_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let red = ui.visuals().error_fg_color;
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        font_id(ui, TextStyle::Button),
        Color32::WHITE,
    );
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(galley.size().x + 32.0, 30.0),
        egui::Sense::click(),
    );
    let press = Press::track(ui, &resp);
    let fill = press.fill(red, red.gamma_multiply(0.92), red.gamma_multiply(0.85));
    ui.painter().rect_filled(rect, CornerRadius::same(15), fill);
    ui.painter().galley(
        egui::pos2(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5 + press.offset().y,
        ),
        galley,
        Color32::WHITE,
    );
    resp
}

/// Icon button: square, transparent until hovered, selected gets accent stroke.
pub fn icon_button(
    ui: &mut egui::Ui,
    name: &str,
    box_size: f32,
    selected: bool,
    tooltip: &str,
) -> egui::Response {
    crate::icons::button(ui, name, box_size, selected, tooltip)
}

/// Popup-menu row: ghost wash, optional danger tint for destructive items.
pub fn menu_item(ui: &mut egui::Ui, icon: &str, label: &str, danger: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let color = if danger {
        ui.visuals().error_fg_color
    } else {
        t.text
    };
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), font_id(ui, TextStyle::Body), color);

    let icon_size = 14.0;
    // Natural content width only — do NOT stretch to available_width, which
    // inside a popup Area is the whole screen and would blow up the menu.
    // The popup's justified layout expands narrower rows to the widest one.
    let width = icon_size + 6.0 + galley.size().x + 24.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 30.0), egui::Sense::click());
    let press = Press::track(ui, &resp);
    press.wash(ui, rect, CornerRadius::same(t.radius), false);

    let cy = rect.center().y + press.offset().y;
    let icon_tint = if danger { color } else { t.icon };
    crate::icons::paint(
        ui,
        Rect::from_center_size(
            egui::pos2(rect.left() + 10.0 + icon_size * 0.5, cy),
            Vec2::splat(icon_size),
        ),
        icon,
        icon_size,
        icon_tint,
    );
    ui.painter().galley(
        egui::pos2(
            rect.left() + 10.0 + icon_size + 6.0,
            cy - galley.size().y * 0.5,
        ),
        galley,
        color,
    );

    resp
}

/// Toast: bottom-center floating card, inverted colors, radius 8.
pub fn toast(ctx: &Context, msg: &str) {
    let t = Tokens::get(ctx);
    let fill = if t.dark {
        Color32::from_rgb(0xF5, 0xF5, 0xF5)
    } else {
        Color32::from_rgb(0x22, 0x22, 0x26)
    };
    let text = if t.dark {
        Color32::from_rgb(0x22, 0x22, 0x26)
    } else {
        Color32::from_rgb(0xFF, 0xFF, 0xFF)
    };

    egui::Area::new(egui::Id::new("kandrium-toast"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -24.0))
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(fill)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(14, 8))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(msg)
                            .text_style(TextStyle::Button)
                            .color(text),
                    );
                });
        });
}
