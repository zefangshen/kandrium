//! The gpui shell's small settings store.
//!
//! A plain `key=value` file next to the database (`gpui-settings.conf`),
//! following the same precedent as `gpui-documents.session`. The backing map
//! keeps keys this build does not know about, so a newer build's settings
//! survive a round trip through an older one. Missing or unreadable values
//! fall back to defaults instead of failing the boot.

use std::collections::BTreeMap;
use std::path::Path;

use crate::app::Section;

/// File name next to the database.
pub const FILE_NAME: &str = "gpui-settings.conf";

/// Expanded tree width, in points, matching both tree views.
pub const DEFAULT_TREE_W: f32 = 240.0;
/// Narrowest tree the splitter allows.
pub const MIN_TREE_W: f32 = 160.0;
/// Widest tree the splitter allows.
pub const MAX_TREE_W: f32 = 420.0;

/// The three collapsible trees the shell remembers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreePanel {
    Notes,
    Documents,
    Projects,
}

impl TreePanel {
    fn open_key(self) -> &'static str {
        match self {
            Self::Notes => "notes_tree_open",
            Self::Documents => "documents_tree_open",
            Self::Projects => "projects_tree_open",
        }
    }

    fn width_key(self) -> &'static str {
        match self {
            Self::Notes => "notes_tree_width",
            Self::Documents => "documents_tree_width",
            Self::Projects => "projects_tree_width",
        }
    }
}

/// Parsed settings. Unknown keys are preserved verbatim.
#[derive(Default)]
pub struct Settings {
    values: BTreeMap<String, String>,
}

impl Settings {
    /// Read `path`. A missing or unreadable file yields defaults; parsing never
    /// fails.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        Self::parse(&text)
    }

    /// Parse the file format. Blank lines and `#` comments are skipped, and
    /// only the first `=` separates key from value, so a value may contain one.
    pub fn parse(text: &str) -> Self {
        let mut values = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                if !key.is_empty() {
                    values.insert(key.to_string(), value.trim().to_string());
                }
            }
        }
        Self { values }
    }

    /// Render the deterministic file text (keys sorted by the map).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (key, value) in &self.values {
            out.push_str(key);
            out.push('=');
            out.push_str(value);
            out.push('\n');
        }
        out
    }

    /// Write through core's atomic writer.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        kandrium_core::fsutil::write_atomic(path, self.to_text())
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    pub fn vim_mode(&self) -> bool {
        self.bool_at("vim_mode", true)
    }

    pub fn set_vim_mode(&mut self, on: bool) {
        self.set_bool("vim_mode", on);
    }

    /// Last active section; unknown values resolve to Tasks.
    pub fn section(&self) -> Section {
        self.values
            .get("section")
            .and_then(|value| Section::from_key(value))
            .unwrap_or(Section::Tasks)
    }

    pub fn set_section(&mut self, section: Section) {
        self.values
            .insert("section".into(), section.id().to_string());
    }

    pub fn tree_width(&self, panel: TreePanel) -> f32 {
        let width = self
            .values
            .get(panel.width_key())
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite())
            .unwrap_or(DEFAULT_TREE_W);
        clamp_tree_width(width)
    }

    pub fn set_tree_width(&mut self, panel: TreePanel, width: f32) {
        self.values.insert(
            panel.width_key().into(),
            format!("{:.1}", clamp_tree_width(width)),
        );
    }

    pub fn tree_open(&self, panel: TreePanel) -> bool {
        self.bool_at(panel.open_key(), true)
    }

    pub fn set_tree_open(&mut self, panel: TreePanel, open: bool) {
        self.set_bool(panel.open_key(), open);
    }

    fn bool_at(&self, key: &str, default: bool) -> bool {
        match self.values.get(key).map(String::as_str) {
            Some("1" | "true" | "yes" | "on") => true,
            Some("0" | "false" | "no" | "off") => false,
            _ => default,
        }
    }

    fn set_bool(&mut self, key: &str, value: bool) {
        self.values
            .insert(key.into(), if value { "1" } else { "0" }.into());
    }
}

/// Clamp a tree width into the range both views can render.
pub fn clamp_tree_width(width: f32) -> f32 {
    if width.is_finite() {
        width.clamp(MIN_TREE_W, MAX_TREE_W)
    } else {
        DEFAULT_TREE_W
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_reads_defaults() {
        let settings = Settings::load(Path::new("/nonexistent/gpui-settings.conf"));
        assert!(settings.vim_mode());
        assert_eq!(settings.section(), Section::Tasks);
        assert_eq!(settings.tree_width(TreePanel::Notes), DEFAULT_TREE_W);
        assert!(settings.tree_open(TreePanel::Documents));
        assert_eq!(settings.tree_width(TreePanel::Projects), DEFAULT_TREE_W);
    }

    #[test]
    fn round_trip_keeps_unknown_keys() {
        let text = "# comment\nfuture_flag=abc\nvim_mode=0\nnotes_tree_width=300\n";
        let mut settings = Settings::parse(text);
        assert!(!settings.vim_mode());
        settings.set_section(Section::Documents);
        let again = Settings::parse(&settings.to_text());
        assert_eq!(again.section(), Section::Documents);
        assert!(!again.vim_mode());
        assert_eq!(again.tree_width(TreePanel::Notes), 300.0);
        assert_eq!(
            again.values.get("future_flag").map(String::as_str),
            Some("abc"),
            "unknown keys must survive a rewrite"
        );
    }

    #[test]
    fn bad_values_fall_back_and_widths_clamp() {
        let settings = Settings::parse(
            "vim_mode=maybe\nsection=elsewhere\nnotes_tree_width=oops\n\
             documents_tree_width=9000\nprojects_tree_width=10\n",
        );
        assert!(settings.vim_mode());
        assert_eq!(settings.section(), Section::Tasks);
        assert_eq!(settings.tree_width(TreePanel::Notes), DEFAULT_TREE_W);
        assert_eq!(settings.tree_width(TreePanel::Documents), MAX_TREE_W);
        assert_eq!(settings.tree_width(TreePanel::Projects), MIN_TREE_W);
    }

    #[test]
    fn save_writes_sorted_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let mut settings = Settings::default();
        settings.set_vim_mode(false);
        settings.set_section(Section::Notes);
        settings.set_tree_open(TreePanel::Notes, false);
        settings.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text, "notes_tree_open=0\nsection=notes\nvim_mode=0\n",
            "keys must be deterministic for a clean diff"
        );
    }
}
