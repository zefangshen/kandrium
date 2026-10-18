//! syntect-backed markdown highlighting for the notes editor.
//!
//! Feeds a `TextEdit::layouter` closure: markdown source goes in, a colored
//! [`LayoutJob`] comes out. The syntax set and theme set are loaded once
//! process-wide; the theme choice (dark/light) is per highlighter so the
//! editor follows the app theme.

use std::sync::OnceLock;

use egui::TextStyle;
use egui::text::LayoutJob;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::theme;

fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    static TS: OnceLock<ThemeSet> = OnceLock::new();
    TS.get_or_init(ThemeSet::load_defaults)
}

/// Markdown syntax highlighter bound to one app theme (dark or light).
pub struct MarkdownHighlighter {
    syntax: &'static SyntaxReference,
    theme: &'static Theme,
}

impl MarkdownHighlighter {
    pub fn new(dark: bool) -> Self {
        let ss = syntax_set();
        let syntax = ss
            .find_syntax_by_extension("md")
            .unwrap_or_else(|| ss.find_syntax_plain_text());
        let theme_name = if dark {
            "base16-ocean.dark"
        } else {
            "InspiredGitHub"
        };
        let theme = theme_set()
            .themes
            .get(theme_name)
            .expect("built-in syntect theme missing");
        Self { syntax, theme }
    }

    /// Highlight `text` and return a job for `TextEdit::layouter`.
    /// Colors come from the syntect theme; the font is the app Monospace
    /// text style so editor metrics stay consistent with the design system.
    pub fn layout_job(&self, ui: &egui::Ui, text: &str) -> LayoutJob {
        let font_id = theme::font_id(ui, TextStyle::Monospace);
        let fallback = theme::Tokens::get(ui.ctx()).text;
        let mut highlighter = HighlightLines::new(self.syntax, self.theme);
        let mut job = LayoutJob::default();

        for line in text.split_inclusive('\n') {
            let ranges = highlighter
                .highlight_line(line, syntax_set())
                .unwrap_or_default();
            for (style, span) in ranges {
                let color = if style.foreground.a == 0 {
                    fallback
                } else {
                    egui::Color32::from_rgba_unmultiplied(
                        style.foreground.r,
                        style.foreground.g,
                        style.foreground.b,
                        style.foreground.a,
                    )
                };
                let format = egui::text::TextFormat {
                    font_id: font_id.clone(),
                    color,
                    italics: style.font_style.contains(FontStyle::ITALIC),
                    ..Default::default()
                };
                job.append(span, 0.0, format);
            }
        }
        job
    }
}
