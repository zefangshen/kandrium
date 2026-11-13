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

/// Notes-half share of the Wiki tree split, in permille.
pub const DEFAULT_SPLIT_PERMILLE: u16 = 500;
/// Smallest share a half may take while both are expanded.
pub const MIN_SPLIT_PERMILLE: u16 = 150;
/// Largest share a half may take while both are expanded.
pub const MAX_SPLIT_PERMILLE: u16 = 850;

/// The collapsible trees the shell remembers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreePanel {
    Wiki,
    Projects,
}

impl TreePanel {
    fn open_key(self) -> &'static str {
        match self {
            Self::Wiki => "wiki_tree_open",
            Self::Projects => "projects_tree_open",
        }
    }

    fn width_key(self) -> &'static str {
        match self {
            Self::Wiki => "wiki_tree_width",
            Self::Projects => "projects_tree_width",
        }
    }

    /// Keys written by builds that had one tree per section. They seed the
    /// merged Wiki column, documents first so its width wins when both exist.
    fn legacy_open_keys(self) -> &'static [&'static str] {
        match self {
            Self::Wiki => &["documents_tree_open", "notes_tree_open"],
            Self::Projects => &[],
        }
    }

    fn legacy_width_keys(self) -> &'static [&'static str] {
        match self {
            Self::Wiki => &["documents_tree_width", "notes_tree_width"],
            Self::Projects => &[],
        }
    }
}

/// Which half of the Wiki tree column a collapse flag belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WikiHalf {
    Notes,
    Documents,
}

impl WikiHalf {
    fn collapsed_key(self) -> &'static str {
        match self {
            Self::Notes => "wiki_notes_collapsed",
            Self::Documents => "wiki_documents_collapsed",
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

    /// Last active section. A saved id this build no longer knows falls back to
    /// Tasks, except the retired Notes/Documents ids, which become Wiki.
    pub fn section(&self) -> Section {
        let Some(key) = self.values.get("section").map(String::as_str) else {
            return Section::Tasks;
        };
        Section::from_key(key).unwrap_or(match key {
            "notes" | "documents" => Section::Wiki,
            _ => Section::Tasks,
        })
    }

    pub fn set_section(&mut self, section: Section) {
        self.values
            .insert("section".into(), section.id().to_string());
    }

    /// Clock id shown in the titlebar. Missing, blank or unparsable values
    /// fall back to the first saved city at resolve time.
    pub fn titlebar_clock(&self) -> Option<&str> {
        self.values
            .get("titlebar_clock")
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    pub fn set_titlebar_clock(&mut self, clock_id: Option<&str>) {
        match clock_id {
            Some(id) if !id.trim().is_empty() => {
                self.values.insert("titlebar_clock".into(), id.to_string());
            }
            _ => {
                self.values.remove("titlebar_clock");
            }
        }
    }

    pub fn tree_width(&self, panel: TreePanel) -> f32 {
        let width = self
            .values
            .get(panel.width_key())
            .or_else(|| {
                panel
                    .legacy_width_keys()
                    .iter()
                    .find_map(|key| self.values.get(*key))
            })
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
        let key = panel.open_key();
        if self.values.contains_key(key) {
            return self.bool_at(key, true);
        }
        panel
            .legacy_open_keys()
            .iter()
            .find(|legacy| self.values.contains_key(**legacy))
            .is_none_or(|legacy| self.bool_at(legacy, true))
    }

    pub fn set_tree_open(&mut self, panel: TreePanel, open: bool) {
        self.set_bool(panel.open_key(), open);
    }

    /// Notes-half share of the Wiki tree split, 0.15–0.85.
    pub fn wiki_split(&self) -> f32 {
        let permille = self
            .values
            .get("wiki_split")
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(DEFAULT_SPLIT_PERMILLE);
        clamp_split_permille(permille) as f32 / 1000.0
    }

    /// Persist the split as an integer permille so the key-sorted file stays
    /// byte-stable.
    pub fn set_wiki_split(&mut self, share: f32) {
        let permille = if share.is_finite() {
            (share * 1000.0).round() as i64
        } else {
            DEFAULT_SPLIT_PERMILLE as i64
        };
        let permille = clamp_split_permille(permille.clamp(0, u16::MAX as i64) as u16);
        self.values
            .insert("wiki_split".into(), permille.to_string());
    }

    /// Whether one Wiki tree half is collapsed to its header.
    pub fn wiki_half_collapsed(&self, half: WikiHalf) -> bool {
        self.bool_at(half.collapsed_key(), false)
    }

    pub fn set_wiki_half_collapsed(&mut self, half: WikiHalf, collapsed: bool) {
        self.set_bool(half.collapsed_key(), collapsed);
    }

    /// Whether the Tasks section last showed the Archived page. The board
    /// selection itself is not persisted, so this is the only Tasks view state
    /// that survives a restart.
    pub fn tasks_archive(&self) -> bool {
        self.bool_at("tasks_archive", false)
    }

    pub fn set_tasks_archive(&mut self, on: bool) {
        self.set_bool("tasks_archive", on);
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

/// Clamp the stored split share so neither half can be squeezed away.
pub fn clamp_split_permille(permille: u16) -> u16 {
    permille.clamp(MIN_SPLIT_PERMILLE, MAX_SPLIT_PERMILLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_reads_defaults() {
        let settings = Settings::load(Path::new("/nonexistent/gpui-settings.conf"));
        assert!(settings.vim_mode());
        assert_eq!(settings.section(), Section::Tasks);
        assert_eq!(settings.tree_width(TreePanel::Wiki), DEFAULT_TREE_W);
        assert!(settings.tree_open(TreePanel::Wiki));
        assert_eq!(settings.tree_width(TreePanel::Projects), DEFAULT_TREE_W);
        assert_eq!(settings.wiki_split(), 0.5);
        assert!(!settings.wiki_half_collapsed(WikiHalf::Notes));
        assert!(!settings.wiki_half_collapsed(WikiHalf::Documents));
    }

    #[test]
    fn retired_section_ids_resolve_to_wiki() {
        for key in ["notes", "documents", "wiki"] {
            let settings = Settings::parse(&format!("section={key}\n"));
            assert_eq!(settings.section(), Section::Wiki, "section={key}");
        }
        let settings = Settings::parse("section=elsewhere\n");
        assert_eq!(settings.section(), Section::Tasks);
    }

    #[test]
    fn round_trip_keeps_unknown_keys() {
        let text = "# comment\nfuture_flag=abc\nvim_mode=0\nnotes_tree_width=300\n";
        let mut settings = Settings::parse(text);
        assert!(!settings.vim_mode());
        settings.set_section(Section::Wiki);
        let again = Settings::parse(&settings.to_text());
        assert_eq!(again.section(), Section::Wiki);
        assert!(!again.vim_mode());
        assert_eq!(again.tree_width(TreePanel::Wiki), 300.0);
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
        assert_eq!(settings.tree_width(TreePanel::Wiki), MAX_TREE_W);
        assert_eq!(settings.tree_width(TreePanel::Projects), MIN_TREE_W);
    }

    #[test]
    fn documents_width_wins_the_legacy_fallback() {
        let settings = Settings::parse("notes_tree_width=200\ndocuments_tree_width=310\n");
        assert_eq!(settings.tree_width(TreePanel::Wiki), 310.0);
        let only_notes = Settings::parse("notes_tree_width=200\n");
        assert_eq!(only_notes.tree_width(TreePanel::Wiki), 200.0);
    }

    #[test]
    fn legacy_tree_open_is_read_when_the_wiki_key_is_missing() {
        let settings = Settings::parse("documents_tree_open=0\n");
        assert!(!settings.tree_open(TreePanel::Wiki));
        let explicit = Settings::parse("documents_tree_open=0\nwiki_tree_open=1\n");
        assert!(explicit.tree_open(TreePanel::Wiki));
    }

    #[test]
    fn split_clamps_and_round_trips() {
        let settings = Settings::parse("wiki_split=0\n");
        assert_eq!(settings.wiki_split(), MIN_SPLIT_PERMILLE as f32 / 1000.0);
        let settings = Settings::parse("wiki_split=9999\n");
        assert_eq!(settings.wiki_split(), MAX_SPLIT_PERMILLE as f32 / 1000.0);
        let settings = Settings::parse("wiki_split=garbage\n");
        assert_eq!(settings.wiki_split(), 0.5);

        let mut settings = Settings::default();
        settings.set_wiki_split(0.63);
        assert_eq!(settings.to_text(), "wiki_split=630\n");
    }

    #[test]
    fn collapsed_flags_parse_and_persist() {
        let settings = Settings::parse("wiki_notes_collapsed=1\nwiki_documents_collapsed=true\n");
        assert!(settings.wiki_half_collapsed(WikiHalf::Notes));
        assert!(settings.wiki_half_collapsed(WikiHalf::Documents));
        let settings = Settings::parse("wiki_notes_collapsed=garbage\n");
        assert!(!settings.wiki_half_collapsed(WikiHalf::Notes));

        let mut settings = Settings::default();
        settings.set_wiki_half_collapsed(WikiHalf::Notes, true);
        assert!(settings.wiki_half_collapsed(WikiHalf::Notes));
        settings.set_wiki_half_collapsed(WikiHalf::Notes, false);
        assert!(!settings.wiki_half_collapsed(WikiHalf::Notes));
    }

    #[test]
    fn save_writes_sorted_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let mut settings = Settings::default();
        settings.set_vim_mode(false);
        settings.set_section(Section::Wiki);
        settings.set_tree_open(TreePanel::Wiki, false);
        settings.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text, "section=wiki\nvim_mode=0\nwiki_tree_open=0\n",
            "keys must be deterministic for a clean diff"
        );
    }

    #[test]
    fn titlebar_clock_round_trips_and_clears() {
        let mut settings = Settings::default();
        assert_eq!(settings.titlebar_clock(), None);
        settings.set_titlebar_clock(Some("clock_tokyo"));
        assert_eq!(settings.titlebar_clock(), Some("clock_tokyo"));
        let again = Settings::parse(&settings.to_text());
        assert_eq!(again.titlebar_clock(), Some("clock_tokyo"));
        settings.set_titlebar_clock(None);
        assert_eq!(settings.titlebar_clock(), None);
        assert!(!settings.to_text().contains("titlebar_clock"));
    }

    #[test]
    fn blank_titlebar_clock_reads_as_unset() {
        let settings = Settings::parse("titlebar_clock=   \n");
        assert_eq!(settings.titlebar_clock(), None);
        let mut settings = settings;
        settings.set_titlebar_clock(Some("  "));
        assert!(!settings.to_text().contains("titlebar_clock"));
    }
}
