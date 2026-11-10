//! Documents: a tree over the documents root, PDF and Markdown tabs, and the
//! per-tab session that survives a restart.
//!
//! Every disk operation goes through `kandrium_core::documents`, so the root
//! (`KANDRIUM_DOCS`), the path guards, the `-2` import names and the atomic
//! annotation sidecars are the same ones the egui app uses. This module owns
//! only the tree, the tab list and the small session file that stores which
//! tabs were open, keyed by root-relative paths.

use std::path::{Path, PathBuf};

/// Tree panel width when expanded, matching the other tree sections.
pub const TREE_W: f32 = 240.0;
/// Collapsed rail width.
pub const RAIL_W: f32 = 30.0;
/// Tree row height.
pub const ROW_H: f32 = 26.0;
/// Indentation per folder level.
pub const INDENT: f32 = 14.0;

/// Which pointer tool the PDF viewer has active. Selection stays visual-only,
/// exactly like the egui app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Select,
    Highlight,
    Note,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Highlight => "Highlight",
            Tool::Note => "Note",
        }
    }
}

/// Session text: `tree_open`, active tab index, then one root-relative path
/// per line. Plain text keeps the gpui shell free of a settings dependency
/// while still persisting exactly what matters.
pub fn format_session(tree_open: bool, active: Option<usize>, rels: &[String]) -> String {
    let mut out = format!("tree_open={}\n", if tree_open { 1 } else { 0 });
    out.push_str(&format!(
        "active={}\n",
        active.map(|index| index.to_string()).unwrap_or_default()
    ));
    for rel in rels {
        out.push_str(rel);
        out.push('\n');
    }
    out
}

/// Parse [`format_session`] output. Only the two leading keyed lines are
/// interpreted; everything else is a path, so a file name containing `=`
/// survives. Unknown/missing keys fall back to `(tree open, no active tab)`.
pub fn parse_session(text: &str) -> (bool, Option<usize>, Vec<String>) {
    let mut tree_open = true;
    let mut active = None;
    let mut rels = Vec::new();
    let mut keyed = 0;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if keyed < 2 {
            if let Some(value) = line.strip_prefix("tree_open=") {
                tree_open = value != "0";
                keyed += 1;
                continue;
            }
            if let Some(value) = line.strip_prefix("active=") {
                active = value.parse::<usize>().ok();
                keyed += 1;
                continue;
            }
        }
        if !line.trim().is_empty() {
            rels.push(line.to_string());
        }
    }
    (tree_open, active, rels)
}

/// Store `path` relative to the documents root, with `/` separators.
pub fn rel_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let text = rel.to_string_lossy().replace('\\', "/");
    if text.is_empty() { None } else { Some(text) }
}

/// Resolve a session path against the documents root.
pub fn abs_path(root: &Path, rel: &str) -> PathBuf {
    root.join(rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trips() {
        let rels = vec!["papers/report.pdf".to_string(), "notes.md".to_string()];
        let text = format_session(false, Some(1), &rels);
        assert_eq!(parse_session(&text), (false, Some(1), rels));
    }

    #[test]
    fn session_defaults_and_garbage() {
        // Missing keys: tree open, no active tab.
        assert_eq!(
            parse_session("notes.md\n"),
            (true, None, vec!["notes.md".to_string()])
        );
        // A non-numeric active index is ignored, and a path with `=` after the
        // keyed header is preserved verbatim.
        let (open, active, rels) = parse_session("tree_open=1\nactive=oops\nx=y.md\n");
        assert!(open);
        assert_eq!(active, None);
        assert_eq!(rels, vec!["x=y.md".to_string()]);
        // Blank lines never become tabs.
        assert!(parse_session("\n\n").2.is_empty());
    }

    #[test]
    fn relative_paths_are_root_based() {
        let root = Path::new("/docs");
        let path = root.join("papers/report.pdf");
        assert_eq!(rel_path(root, &path).as_deref(), Some("papers/report.pdf"));
        assert_eq!(abs_path(root, "papers/report.pdf"), path);
        assert_eq!(rel_path(root, root), None);
        assert_eq!(rel_path(root, Path::new("/elsewhere/a.md")), None);
    }

    #[test]
    fn tool_default_is_select() {
        assert_eq!(Tool::default(), Tool::Select);
        assert_eq!(Tool::Highlight.label(), "Highlight");
    }
}
