//! LLM wiki page model and roots.
//!
//! Pages live outside SQLite under the wiki root (`KANDRIUM_WIKI`, default
//! `<data_dir>/kandrium/wiki/`). Compiled output uses `sources/`, `entities/`,
//! and `concepts/` plus the generated `index.md`; hand-written pages may live
//! anywhere else under the same root. The `wiki_*` tables are a derived index
//! over those files, never the source of truth.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Environment variable that overrides the wiki page root.
pub const WIKI_ENV: &str = "KANDRIUM_WIKI";
/// Retired environment variable, accepted as a read alias for one release.
pub const LEGACY_NOTES_ENV: &str = "KANDRIUM_NOTES";

/// Resolve the wiki root:
/// 1. `KANDRIUM_WIKI` (explicit override)
/// 2. `KANDRIUM_NOTES` (deprecated read alias)
/// 3. `<data_dir>/kandrium/wiki`
pub fn resolve_wiki_root() -> PathBuf {
    resolve_root_from(
        std::env::var_os(WIKI_ENV),
        std::env::var_os(LEGACY_NOTES_ENV),
        dirs::data_dir(),
    )
}

/// Create the wiki root if missing and return it.
pub fn ensure_wiki_root() -> Result<PathBuf> {
    let root = resolve_wiki_root();
    fs::create_dir_all(&root)?;
    Ok(root)
}

fn resolve_root_from(
    wiki: Option<OsString>,
    legacy: Option<OsString>,
    data_dir: Option<PathBuf>,
) -> PathBuf {
    if let Some(path) = wiki {
        return PathBuf::from(path);
    }
    if let Some(path) = legacy {
        return PathBuf::from(path);
    }
    data_dir
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kandrium")
        .join("wiki")
}

/// Source document format. Sources live under the documents root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Markdown,
    Pdf,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Pdf => "pdf",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "markdown" => Some(Self::Markdown),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }
}

/// Compile state of one source document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceState {
    New,
    Indexed,
    Compiled,
    Stale,
    Failed,
}

impl SourceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Indexed => "indexed",
            Self::Compiled => "compiled",
            Self::Stale => "stale",
            Self::Failed => "failed",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "new" => Some(Self::New),
            "indexed" => Some(Self::Indexed),
            "compiled" => Some(Self::Compiled),
            "stale" => Some(Self::Stale),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Kind of page stored under the wiki root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageKind {
    Source,
    Entity,
    Concept,
    Index,
    Page,
}

impl PageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Entity => "entity",
            Self::Concept => "concept",
            Self::Index => "index",
            Self::Page => "page",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "source" => Some(Self::Source),
            "entity" => Some(Self::Entity),
            "concept" => Some(Self::Concept),
            "index" => Some(Self::Index),
            "page" => Some(Self::Page),
            _ => None,
        }
    }
}

/// Directory reserved for a compiled page kind.
pub fn compiled_dir(kind: PageKind) -> Option<&'static str> {
    match kind {
        PageKind::Source => Some("sources"),
        PageKind::Entity => Some("entities"),
        PageKind::Concept => Some("concepts"),
        PageKind::Index | PageKind::Page => None,
    }
}

/// True when a root-relative page path sits in a compiled folder.
pub fn is_compiled_path(rel_path: &str) -> bool {
    let Some((first, rest)) = rel_path.split_once('/') else {
        return false;
    };
    !rest.is_empty() && matches!(first, "sources" | "entities" | "concepts")
}

/// One source document as indexed in `wiki_sources`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiSource {
    pub rel_path: String,
    pub kind: SourceKind,
    pub hash: String,
    pub size: u64,
    pub mtime: i64,
    pub state: SourceState,
    pub compiled_at: Option<i64>,
    pub error: Option<String>,
    pub page_count: u32,
}

/// One citation anchor from a page to a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageSource {
    pub source_path: String,
    pub source_hash: String,
    pub start_line: u32,
    pub end_line: u32,
    pub start_page: u32,
    pub end_page: u32,
}

/// One page as indexed in `wiki_pages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiPage {
    pub rel_path: String,
    pub kind: PageKind,
    pub title: String,
    pub summary: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    pub reviewed: bool,
    pub content_hash: String,
    pub generated_hash: Option<String>,
    pub updated_at: i64,
    pub sources: Vec<PageSource>,
}

/// Source counts for status surfaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCounts {
    pub new: usize,
    pub indexed: usize,
    pub compiled: usize,
    pub stale: usize,
    pub failed: usize,
}

/// Page counts by kind for status surfaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageCounts {
    pub source: usize,
    pub entity: usize,
    pub concept: usize,
    pub index: usize,
    pub page: usize,
}

/// Aggregate index state. Filled in stage 2.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiStatus {
    pub sources: SourceCounts,
    pub pages: PageCounts,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_env_wins_over_the_legacy_alias() {
        let root = resolve_root_from(
            Some(OsString::from("/tmp/wiki")),
            Some(OsString::from("/tmp/legacy")),
            Some(PathBuf::from("/data")),
        );
        assert_eq!(root, PathBuf::from("/tmp/wiki"));
    }

    #[test]
    fn legacy_alias_is_used_when_wiki_env_is_absent() {
        let root = resolve_root_from(
            None,
            Some(OsString::from("/tmp/legacy")),
            Some(PathBuf::from("/data")),
        );
        assert_eq!(root, PathBuf::from("/tmp/legacy"));
    }

    #[test]
    fn default_root_is_the_wiki_directory() {
        let root = resolve_root_from(None, None, Some(PathBuf::from("/data")));
        assert_eq!(root, PathBuf::from("/data/kandrium/wiki"));
    }

    #[test]
    fn compiled_path_detection_only_matches_the_reserved_folders() {
        assert!(is_compiled_path("sources/paper.md"));
        assert!(is_compiled_path("entities/vaswani.md"));
        assert!(is_compiled_path("concepts/attention.md"));
        assert!(!is_compiled_path("handwritten/note.md"));
        assert!(!is_compiled_path("index.md"));
        assert!(!is_compiled_path("sources"));
    }
}
