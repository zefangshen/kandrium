//! Design tokens and the single text-size table.
//!
//! Ported from the egui app's `theme.rs`. The rule that all font sizes live in
//! one place is structural here: [`Text`] is the only public source of a font
//! size, and [`TextStyled::text`] is the only helper that applies one. Widget
//! code never passes a pixel size.
//!
//! Palette, spacing and radii are plain [`Tokens`] values so a light theme can
//! be added later without touching call sites.

use std::borrow::Cow;

use gpui_kit::{App, DefiniteLength, FontWeight, Rgba, Styled, px, rgb, rgba};

/// The proportional family bundled with the app (Inter).
pub const FONT_SANS: &str = "Inter";
/// The monospace family bundled with the app (JetBrains Mono).
pub const FONT_MONO: &str = "JetBrains Mono";

/// Design tokens for one colour scheme.
#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub dark: bool,
    /// Title bar / toolbar background.
    pub titlebar: Rgba,
    /// Panels and popovers.
    pub panel: Rgba,
    /// Background behind columns (the "canvas").
    pub pasteboard: Rgba,
    /// Column surface / dialog background.
    pub card: Rgba,
    /// Raised surface inside a column: card fill, active tab.
    pub surface_2: Rgba,
    /// Hovered card fill.
    pub surface_3: Rgba,
    pub border: Rgba,
    /// Border of a hovered card.
    pub border_hover: Rgba,
    pub divider: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub text_faint: Rgba,
    /// Placeholder / disabled text.
    pub text_ghost: Rgba,
    pub icon: Rgba,
    pub hover: Rgba,
    pub pressed: Rgba,
    pub selected: Rgba,
    pub accent: Rgba,
    pub accent_text: Rgba,
    pub accent_soft: Rgba,
    /// Accent at ring strength: away borders, underlines.
    pub accent_line: Rgba,
    /// Focus colour for text fields. Translucent so the component's 1 px
    /// border plus 3 px halo read as a quiet tint instead of a bright rim.
    pub focus_ring: Rgba,
    /// Text-field background.
    pub field: Rgba,
    /// Destructive / error text.
    pub danger: Rgba,
    /// Corner radius for controls and cards.
    pub radius: f32,
    /// Corner radius for large surfaces (columns, clock cards).
    pub radius_lg: f32,
}

impl Tokens {
    /// The shipping dark palette.
    pub fn dark() -> Self {
        Self {
            dark: true,
            titlebar: rgb(0x161618),
            panel: rgb(0x1F2023),
            pasteboard: rgb(0x131314),
            card: rgb(0x19191B),
            surface_2: rgb(0x1F2023),
            surface_3: rgb(0x242528),
            border: rgb(0x26262A),
            border_hover: rgb(0x34353A),
            divider: rgb(0x26262A),
            text: rgb(0xE6E6E9),
            text_muted: rgb(0xA0A0A8),
            text_faint: rgb(0x6E6E77),
            text_ghost: rgb(0x4C4C55),
            icon: rgb(0xA0A0A8),
            hover: rgba(0xFFFFFF0B),
            pressed: rgba(0xFFFFFF12),
            selected: rgba(0x4D8DF721),
            accent: rgb(0x4D8DF7),
            accent_text: rgb(0xA6C8FF),
            accent_soft: rgba(0x4D8DF721),
            accent_line: rgba(0x4D8DF78C),
            focus_ring: rgba(0x4D8DF773),
            field: rgb(0x101011),
            danger: rgb(0xE5674F),
            radius: 6.0,
            radius_lg: 8.0,
        }
    }

    /// The light palette.
    pub fn light() -> Self {
        Self {
            dark: false,
            titlebar: rgb(0xEEEEEF),
            panel: rgb(0xFFFFFF),
            pasteboard: rgb(0xF3F3F4),
            card: rgb(0xFBFBFC),
            surface_2: rgb(0xFFFFFF),
            surface_3: rgb(0xF4F4F6),
            border: rgb(0xE3E3E7),
            border_hover: rgb(0xD2D2D8),
            divider: rgb(0xE3E3E7),
            text: rgb(0x1C1C20),
            text_muted: rgb(0x5B5B65),
            text_faint: rgb(0x83838D),
            text_ghost: rgb(0xABABB4),
            icon: rgb(0x5B5B65),
            hover: rgba(0x1414180B),
            pressed: rgba(0x14141813),
            selected: rgba(0x1F63D617),
            accent: rgb(0x1F63D6),
            accent_text: rgb(0x1A53B6),
            accent_soft: rgba(0x1F63D617),
            accent_line: rgba(0x1F63D673),
            focus_ring: rgba(0x1F63D64D),
            field: rgb(0xFFFFFF),
            danger: rgb(0xB3351F),
            radius: 6.0,
            radius_lg: 8.0,
        }
    }
}

/// Trello-style label palette (index = stored label id).
pub const LABEL_COLORS: [(&str, u32); 8] = [
    ("Green", 0x61bd4f),
    ("Yellow", 0xf5cd28),
    ("Orange", 0xff9f1a),
    ("Red", 0xeb5a46),
    ("Purple", 0xc377e0),
    ("Blue", 0x0079bf),
    ("Sky", 0x00c2e8),
    ("Black", 0x172b4d),
];

/// Colour for a label index, if in range.
pub fn label_color(index: i64) -> Option<Rgba> {
    LABEL_COLORS.get(index as usize).map(|(_, hex)| rgb(*hex))
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// Every text style in the app. Sizes live ONLY in [`Text::metrics`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text {
    /// 12 px regular: secondary labels, tree rows.
    Small,
    /// 13 px regular: body copy.
    Body,
    /// 12.5 px medium: buttons.
    Button,
    /// 13 px medium: board card titles.
    CardTitle,
    /// 17 px semibold: section headings.
    Heading,
    /// 22 px semibold: project overview title.
    Display,
    /// 13 px monospace: code and fixed-width values.
    Monospace,
    /// 12.5 px semibold: column titles, active board tabs.
    Strong,
    /// 12 px semibold: field labels, chips.
    SmallStrong,
    /// 30 px monospace: clock numerals.
    ClockTime,
    /// 12.5 px medium: titlebar section control.
    Chrome,
    /// 10 px monospace: counts, identifiers, due dates, offsets.
    Micro,
    /// 10 px semibold: uppercase tree-head labels.
    Label,
    /// 16 px monospace: summary metrics.
    Metric,
    /// 14 px regular: Markdown prose and editor source.
    Reading,
}

/// The concrete metrics for one [`Text`] slot.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub size: f32,
    pub line_height: f32,
    pub weight: FontWeight,
    pub mono: bool,
}

impl Text {
    /// The one table of font sizes.
    pub fn metrics(self) -> Metrics {
        match self {
            // size, line height, weight, monospace
            Text::Small => metrics(12.0, 17.0, FontWeight::NORMAL, false),
            Text::Body => metrics(13.0, 18.0, FontWeight::NORMAL, false),
            Text::Button => metrics(12.5, 17.0, FontWeight::MEDIUM, false),
            Text::CardTitle => metrics(13.0, 17.0, FontWeight::MEDIUM, false),
            Text::Heading => metrics(17.0, 22.0, FontWeight::SEMIBOLD, false),
            Text::Display => metrics(22.0, 28.0, FontWeight::SEMIBOLD, false),
            Text::Monospace => metrics(13.0, 18.0, FontWeight::NORMAL, true),
            Text::Strong => metrics(12.5, 17.0, FontWeight::SEMIBOLD, false),
            Text::SmallStrong => metrics(12.0, 16.0, FontWeight::SEMIBOLD, false),
            Text::ClockTime => metrics(30.0, 35.0, FontWeight::NORMAL, true),
            Text::Chrome => metrics(12.5, 17.0, FontWeight::NORMAL, false),
            Text::Micro => metrics(10.0, 14.0, FontWeight::NORMAL, true),
            Text::Label => metrics(10.0, 14.0, FontWeight::SEMIBOLD, false),
            Text::Metric => metrics(16.0, 20.0, FontWeight::NORMAL, true),
            Text::Reading => metrics(14.0, 23.0, FontWeight::NORMAL, false),
        }
    }

    /// Font size in pixels, sourced from the table.
    pub fn size(self) -> f32 {
        self.metrics().size
    }

    /// Line height in pixels, sourced from the table.
    pub fn line_height(self) -> f32 {
        self.metrics().line_height
    }

    /// Family name to use for this style.
    pub fn family(self) -> &'static str {
        if self.metrics().mono {
            FONT_MONO
        } else {
            FONT_SANS
        }
    }
}

fn metrics(size: f32, line_height: f32, weight: FontWeight, mono: bool) -> Metrics {
    Metrics {
        size,
        line_height,
        weight,
        mono,
    }
}

/// Apply a [`Text`] style. This is the only way font sizes reach an element.
pub trait TextStyled: Styled + Sized {
    fn text(self, style: Text) -> Self {
        let m = style.metrics();
        self.font_family(style.family())
            .text_size(px(m.size))
            .line_height(DefiniteLength::from(px(m.line_height)))
            .font_weight(m.weight)
    }
}

impl<E: Styled> TextStyled for E {}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

/// Install Inter and JetBrains Mono into the text system.
///
/// The fonts are bundled in this crate's `assets/` directory.
pub fn install_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Medium.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/Inter-SemiBold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")),
    ];
    cx.text_system()
        .add_fonts(fonts)
        .expect("bundled fonts must parse");
}

/// Point gpui-component's own widget theme at the dark palette.
///
/// The component crate carries a global theme for its inputs and text areas.
/// Left alone it follows the system appearance, which renders white fields on
/// our dark shell. Component widgets paint their fill from `background` and
/// their border from `input`, so those two matter most.
pub fn install_component_theme(cx: &mut App) {
    use gpui_kit::component::{Theme, ThemeMode};

    Theme::change(ThemeMode::Dark, None, cx);

    let t = Tokens::dark();
    Theme::update(cx, |theme| {
        theme.background = t.field.into();
        theme.foreground = t.text.into();
        theme.border = t.border.into();
        theme.input = t.border.into();
        theme.muted = t.surface_2.into();
        theme.muted_foreground = t.text_muted.into();
        theme.primary = t.accent.into();
        theme.primary_foreground = t.text.into();
        theme.ring = t.focus_ring.into();
        // gpui-component hard-codes its focus band at 3 px (`FOCUS_RING_WIDTH`,
        // `pub(crate)`), so the only theme-level way to thin it is to turn the
        // ring off. Every field keeps its 1 px border, which `focus_style` then
        // tints with `ring` — a single hairline instead of a border plus halo.
        theme.focus_ring = false;
        theme.caret = t.accent.into();
        theme.selection = t.selected.into();
        // The component projects this colour onto the base theme as the
        // scrollbar's *track* fill. Transparent keeps every list bar leaving
        // nothing behind the content but the thumb.
        theme.scrollbar = rgba(0x00000000).into();
        // Thumb colours for the projection. `install_scrollbar_style` below
        // refines the geometry (4 px, inset); these keep the projected colours
        // identical so a later component-theme refresh cannot reintroduce a
        // track-coloured thumb.
        theme.scrollbar_thumb = t.text_ghost.into();
        theme.scrollbar_thumb_hover = t.text_faint.into();
        theme.title_bar = t.titlebar.into();
        theme.title_bar_border = t.border.into();
        // The code-editor layout (Notes, document tabs, project documents)
        // paints its gutter, line numbers and active-line band from the
        // highlight theme. Start from the library's dark palette, then take
        // the chrome colours from our tokens so the editor sits in the same
        // surface family as every field.
        let mut highlight =
            (*gpui_kit::component::highlighter::HighlightTheme::default_dark()).clone();
        // The editor paints its own surface fill; matching the pane's card
        // colour makes the input read as one uninterrupted surface (no frame,
        // no focus edge). The editors themselves are created `bordered(false)`.
        highlight.style.editor_background = Some(t.card.into());
        highlight.style.editor_foreground = Some(t.text.into());
        highlight.style.editor_gutter_background = Some(t.card.into());
        highlight.style.editor_line_number = Some(t.text_ghost.into());
        highlight.style.editor_active_line_number = Some(t.text_muted.into());
        highlight.style.editor_active_line = Some(rgba(0xFFFFFF09).into());
        theme.highlight_theme = std::sync::Arc::new(highlight);
    });

    // One scrollbar treatment for the whole app: the base theme is where every
    // `Scrollbar` resolves its paint, so it is set once here rather than per
    // list. No track at all, a 4 px thumb tucked into the content gutter, and
    // the wide (16 px) track kept purely as the invisible drag target.
    install_scrollbar_style(cx);
}

/// Thin, trackless scrollbars for every scrolled list.
///
/// The painted thumb is 4 px, inset 2 px from the track's edge, so inside a
/// list with an 8 px content gutter it lands centred in that gutter: 2 px of
/// air on both sides, clear of the card edge and the panel edge. The track
/// stays 16 px wide — that width is the grab area, and it is never painted —
/// so the bar remains comfortably draggable while looking like a hairline.
fn install_scrollbar_style(cx: &mut App) {
    use gpui_kit::base::{ScrollbarStyles, Theme};

    let t = Tokens::dark();
    let thumb = |style: gpui_kit::base::ScrollbarThumbStyle, color: Rgba| {
        style.width(px(4.)).inset(px(2.)).radius(px(2.)).bg(color)
    };
    let transparent = rgba(0x00000000).into();
    let styles = ScrollbarStyles::default()
        .track(|style| style.bg(transparent))
        .track_hover(|style| style.bg(transparent))
        .track_active(|style| style.bg(transparent))
        // With the track gone the thumb sits on the column surface, so it
        // takes the ghost/faint text colours: a faint hairline at rest that
        // brightens on hover, like the egui app's 3 px floating bar.
        .thumb(|style| thumb(style, t.text_ghost))
        .thumb_hover(|style| thumb(style, t.text_faint))
        .thumb_active(|style| thumb(style, t.text_faint));

    let base = Theme::global_mut(cx);
    // Keep the projected mode and motion; only the paint changes.
    let scrollbar = base.scrollbar.clone();
    base.scrollbar = scrollbar.with_styles(styles);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_sizes_match_the_design_system() {
        let expected = [
            (Text::Small, 12.0, "Inter"),
            (Text::Body, 13.0, "Inter"),
            (Text::Button, 12.5, "Inter"),
            (Text::CardTitle, 13.0, "Inter"),
            (Text::Heading, 17.0, "Inter"),
            (Text::Display, 22.0, "Inter"),
            (Text::Monospace, 13.0, "JetBrains Mono"),
            (Text::Strong, 12.5, "Inter"),
            (Text::SmallStrong, 12.0, "Inter"),
            (Text::ClockTime, 30.0, "JetBrains Mono"),
            (Text::Chrome, 12.5, "Inter"),
            (Text::Micro, 10.0, "JetBrains Mono"),
            (Text::Label, 10.0, "Inter"),
            (Text::Metric, 16.0, "JetBrains Mono"),
            (Text::Reading, 14.0, "Inter"),
        ];
        for (style, size, family) in expected {
            assert_eq!(style.size(), size, "{style:?} size");
            assert_eq!(style.family(), family, "{style:?} family");
        }
    }

    #[test]
    fn radius_is_six_pixels() {
        assert_eq!(Tokens::dark().radius, 6.0);
        assert_eq!(Tokens::light().radius, 6.0);
    }

    /// The focus edge must stay translucent: it is a single 1 px border, so a
    /// solid accent would read as a hard bright line.
    #[test]
    fn focus_ring_stays_translucent() {
        assert!(Tokens::dark().focus_ring.a < 0.6);
        assert!(Tokens::light().focus_ring.a < 0.5);
    }
}
