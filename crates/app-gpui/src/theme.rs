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

use gpui_kit::{App, DefiniteLength, FontWeight, Rgba, Styled, px, rgb};

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
    /// Card / dialog background (columns share it, separated by a border).
    pub card: Rgba,
    pub border: Rgba,
    pub divider: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub text_faint: Rgba,
    pub icon: Rgba,
    pub hover: Rgba,
    pub pressed: Rgba,
    pub selected: Rgba,
    pub accent: Rgba,
    pub accent_text: Rgba,
    pub accent_soft: Rgba,
    /// Text-field background.
    pub field: Rgba,
    /// Destructive / error text.
    pub danger: Rgba,
    /// Corner radius for controls and cards.
    pub radius: f32,
}

impl Tokens {
    /// The shipping dark palette. Mirrors `crates/app/src/theme.rs`.
    pub fn dark() -> Self {
        Self {
            dark: true,
            titlebar: rgb(0x1B1B1E),
            panel: rgb(0x26262A),
            pasteboard: rgb(0x19191C),
            card: rgb(0x2E2E33),
            border: rgb(0x3C3C43),
            divider: rgb(0x333339),
            text: rgb(0xECECEF),
            text_muted: rgb(0xAEAEB6),
            text_faint: rgb(0x97979E),
            icon: rgb(0xD4D4DA),
            hover: rgb(0x34343A),
            pressed: rgb(0x3E3E45),
            selected: rgb(0x233A63),
            accent: rgb(0x4B8BF5),
            accent_text: rgb(0x8CB6FA),
            accent_soft: rgb(0x243657),
            field: rgb(0x1E1E22),
            danger: rgb(0xEB5A46),
            radius: 6.0,
        }
    }

    /// The light palette, for parity with the egui app.
    pub fn light() -> Self {
        Self {
            dark: false,
            titlebar: rgb(0xE9E9EB),
            panel: rgb(0xFFFFFF),
            pasteboard: rgb(0xF1F1F3),
            card: rgb(0xFFFFFF),
            border: rgb(0xDADADE),
            divider: rgb(0xE8E8EB),
            text: rgb(0x222226),
            text_muted: rgb(0x5E5E66),
            text_faint: rgb(0x6B6B73),
            icon: rgb(0x44444B),
            hover: rgb(0xF0F0F3),
            pressed: rgb(0xE4E4E9),
            selected: rgb(0xE6EEFD),
            accent: rgb(0x1B63E0),
            accent_text: rgb(0x1755C4),
            accent_soft: rgb(0xE3ECFD),
            field: rgb(0xFFFFFF),
            danger: rgb(0xB42418),
            radius: 6.0,
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
    /// 13 px regular: secondary labels, table body.
    Small,
    /// 16 px regular: body copy.
    Body,
    /// 16 px medium: buttons and card titles.
    Button,
    /// 20 px semibold: section headings.
    Heading,
    /// 16 px monospace: code and fixed-width values.
    Monospace,
    /// 16 px semibold: column titles, active board tabs.
    Strong,
    /// 13 px semibold: uppercase section titles, table headers.
    SmallStrong,
    /// 20 px monospace: clock numerals.
    ClockTime,
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
            Text::Small => metrics(13.0, 18.0, FontWeight::NORMAL, false),
            Text::Body => metrics(16.0, 22.0, FontWeight::NORMAL, false),
            Text::Button => metrics(16.0, 22.0, FontWeight::MEDIUM, false),
            Text::Heading => metrics(20.0, 26.0, FontWeight::SEMIBOLD, false),
            Text::Monospace => metrics(16.0, 22.0, FontWeight::NORMAL, true),
            Text::Strong => metrics(16.0, 22.0, FontWeight::SEMIBOLD, false),
            Text::SmallStrong => metrics(13.0, 18.0, FontWeight::SEMIBOLD, false),
            Text::ClockTime => metrics(20.0, 26.0, FontWeight::NORMAL, true),
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
/// The font files are reused from the egui app's asset directory; the gpui
/// crate does not keep a second copy.
pub fn install_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../../app/assets/fonts/Inter-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../../app/assets/fonts/Inter-Medium.ttf")),
        Cow::Borrowed(include_bytes!("../../app/assets/fonts/Inter-SemiBold.ttf")),
        Cow::Borrowed(include_bytes!(
            "../../app/assets/fonts/JetBrainsMono-Regular.ttf"
        )),
    ];
    cx.text_system()
        .add_fonts(fonts)
        .expect("bundled fonts must parse");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_sizes_match_the_design_system() {
        let expected = [
            (Text::Small, 13.0, "Inter"),
            (Text::Body, 16.0, "Inter"),
            (Text::Button, 16.0, "Inter"),
            (Text::Heading, 20.0, "Inter"),
            (Text::Monospace, 16.0, "JetBrains Mono"),
            (Text::Strong, 16.0, "Inter"),
            (Text::SmallStrong, 13.0, "Inter"),
            (Text::ClockTime, 20.0, "JetBrains Mono"),
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
}
