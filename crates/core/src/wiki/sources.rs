//! Source discovery and content hashing.
//!
//! Sources are the files under the documents root. This module never edits
//! them: it scans the tree, hashes each file, and keeps `wiki_sources` in
//! step. Pages are not touched here; a changed source only becomes `stale`.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path};
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::documents;
use crate::error::{KandriumError, Result};
use crate::fsutil::{self, FsEntry};

use super::{SourceCounts, SourceKind, SourceState, WikiSource};

/// Result of one source refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDelta {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
}

impl SourceDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }
}

struct ScannedSource {
    rel_path: String,
    kind: SourceKind,
    hash: String,
    size: u64,
    mtime: i64,
}

/// Scan the documents root and reconcile `wiki_sources`.
///
/// New files become `new`, changed files become `stale`, unchanged files keep
/// their state, and missing files lose their row. Pages that cited a removed
/// source are intentionally left alone; lint reports them later.
pub fn refresh_sources(db: &mut Connection, documents_root: &Path) -> Result<SourceDelta> {
    let scanned = scan_documents(documents_root)?;
    let existing = load_existing_hashes(db)?;
    let seen: HashSet<&str> = scanned.iter().map(|s| s.rel_path.as_str()).collect();

    let mut delta = SourceDelta::default();
    let tx = db.transaction()?;
    for source in &scanned {
        match existing.get(&source.rel_path) {
            None => {
                tx.execute(
                    "INSERT INTO wiki_sources
                         (rel_path, kind, hash, size, mtime, state)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'new')",
                    params![
                        source.rel_path,
                        source.kind.as_str(),
                        source.hash,
                        source.size as i64,
                        source.mtime
                    ],
                )?;
                delta.added.push(source.rel_path.clone());
            }
            Some(hash) if *hash == source.hash => {
                tx.execute(
                    "UPDATE wiki_sources
                        SET kind = ?1, size = ?2, mtime = ?3
                      WHERE rel_path = ?4",
                    params![
                        source.kind.as_str(),
                        source.size as i64,
                        source.mtime,
                        source.rel_path
                    ],
                )?;
                delta.unchanged += 1;
            }
            Some(_) => {
                tx.execute(
                    "UPDATE wiki_sources
                        SET kind = ?1, hash = ?2, size = ?3, mtime = ?4,
                            state = 'stale', error = NULL
                      WHERE rel_path = ?5",
                    params![
                        source.kind.as_str(),
                        source.hash,
                        source.size as i64,
                        source.mtime,
                        source.rel_path
                    ],
                )?;
                delta.changed.push(source.rel_path.clone());
            }
        }
    }

    for rel_path in existing.keys() {
        if !seen.contains(rel_path.as_str()) {
            tx.execute("DELETE FROM wiki_sources WHERE rel_path = ?1", [rel_path])?;
            delta.removed.push(rel_path.clone());
        }
    }
    tx.commit()?;

    delta.added.sort();
    delta.changed.sort();
    delta.removed.sort();
    Ok(delta)
}

/// List indexed sources, optionally filtered by state.
pub fn list_sources(db: &Connection, state: Option<SourceState>) -> Result<Vec<WikiSource>> {
    const SELECT: &str = "SELECT s.rel_path, s.kind, s.hash, s.size, s.mtime, s.state,
                                 s.compiled_at, s.error,
                                 (SELECT COUNT(DISTINCT ps.page_path)
                                    FROM wiki_page_sources ps
                                   WHERE ps.source_path = s.rel_path)
                            FROM wiki_sources s";
    let mut out = Vec::new();
    if let Some(state) = state {
        let mut stmt = db.prepare(&format!("{SELECT} WHERE s.state = ?1 ORDER BY s.rel_path"))?;
        let rows = stmt.query_map([state.as_str()], map_source_row)?;
        for row in rows {
            out.push(decode_source(row?)?);
        }
    } else {
        let mut stmt = db.prepare(&format!("{SELECT} ORDER BY s.rel_path"))?;
        let rows = stmt.query_map([], map_source_row)?;
        for row in rows {
            out.push(decode_source(row?)?);
        }
    }
    Ok(out)
}

/// Count sources by state.
pub fn counts(db: &Connection) -> Result<SourceCounts> {
    let mut counts = SourceCounts::default();
    let mut stmt = db.prepare("SELECT state, COUNT(*) FROM wiki_sources GROUP BY state")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (state, count) = row?;
        let count = count.max(0) as usize;
        match SourceState::from_db(&state) {
            Some(SourceState::New) => counts.new = count,
            Some(SourceState::Indexed) => counts.indexed = count,
            Some(SourceState::Compiled) => counts.compiled = count,
            Some(SourceState::Stale) => counts.stale = count,
            Some(SourceState::Failed) => counts.failed = count,
            None => {
                return Err(KandriumError(format!(
                    "unknown wiki source state in database: {state}"
                )));
            }
        }
    }
    Ok(counts)
}

/// SHA-256 of a byte slice, lowercase hex.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    to_hex(&hasher.finalize())
}

/// SHA-256 of a file, lowercase hex.
pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(to_hex(&hasher.finalize()))
}

fn to_hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

/// Source kind from a file extension.
pub fn kind_for_path(path: &Path) -> Option<SourceKind> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("pdf") => Some(SourceKind::Pdf),
        Some("md" | "markdown") => Some(SourceKind::Markdown),
        _ => None,
    }
}

/// Root-relative path with `/` separators.
pub fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let rel = path.strip_prefix(root).map_err(|_| {
        KandriumError(format!(
            "path {} is outside {}",
            path.display(),
            root.display()
        ))
    })?;
    let mut parts = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => {
                return Err(KandriumError(format!(
                    "invalid source path: {}",
                    path.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(KandriumError(format!(
            "empty source path: {}",
            path.display()
        )));
    }
    Ok(parts.join("/"))
}

fn scan_documents(documents_root: &Path) -> Result<Vec<ScannedSource>> {
    let root = fsutil::canonical_root(documents_root)?;
    let tree = fsutil::scan_tree(&root, documents::DOC_EXTENSIONS)?;
    let mut files = Vec::new();
    collect_files(&tree, &mut files);

    let mut out = Vec::with_capacity(files.len());
    for entry in files {
        let Some(kind) = kind_for_path(&entry.path) else {
            continue;
        };
        let metadata = std::fs::metadata(&entry.path)?;
        out.push(ScannedSource {
            rel_path: relative_path(&root, &entry.path)?,
            kind,
            hash: hash_file(&entry.path)?,
            size: metadata.len(),
            mtime: modified_seconds(&metadata),
        });
    }
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(out)
}

fn collect_files<'a>(entries: &'a [FsEntry], out: &mut Vec<&'a FsEntry>) {
    for entry in entries {
        if entry.is_dir {
            if let Some(children) = &entry.children {
                collect_files(children, out);
            }
        } else {
            out.push(entry);
        }
    }
}

fn modified_seconds(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn load_existing_hashes(db: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = db.prepare("SELECT rel_path, hash FROM wiki_sources")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (path, hash) = row?;
        out.insert(path, hash);
    }
    Ok(out)
}

type SourceRow = (
    String,
    String,
    String,
    i64,
    i64,
    String,
    Option<i64>,
    Option<String>,
    i64,
);

fn map_source_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn decode_source(row: SourceRow) -> Result<WikiSource> {
    let (rel_path, kind, hash, size, mtime, state, compiled_at, error, page_count) = row;
    let kind = SourceKind::from_db(&kind)
        .ok_or_else(|| KandriumError(format!("unknown source kind in database: {kind}")))?;
    let state = SourceState::from_db(&state)
        .ok_or_else(|| KandriumError(format!("unknown source state in database: {state}")))?;
    Ok(WikiSource {
        rel_path,
        kind,
        hash,
        size: size.max(0) as u64,
        mtime,
        state,
        compiled_at,
        error,
        page_count: page_count.max(0) as u32,
    })
}

/// Requested span of a source. Zero means "from the start" or "to the end".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRange {
    pub start_line: u32,
    pub end_line: u32,
    pub start_page: u32,
    pub end_page: u32,
}

/// Text returned by [`read_source`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceText {
    pub path: String,
    pub kind: SourceKind,
    pub text: String,
    pub start_line: u32,
    pub end_line: u32,
    pub page_count: u32,
    pub truncated: bool,
    pub next_page: Option<u32>,
}

/// Read a Markdown or PDF source under the documents root.
///
/// Markdown is read by line; PDFs are read by page and capped at
/// `max_pages` so an agent never pulls a whole book in one call.
pub fn read_source(
    documents_root: &Path,
    rel_path: &str,
    range: SourceRange,
    max_pages: usize,
) -> Result<SourceText> {
    validate_rel_path(rel_path)?;
    let path = fsutil::guard(documents_root, &documents_root.join(rel_path))?;
    let kind = kind_for_path(Path::new(rel_path))
        .ok_or_else(|| KandriumError(format!("unsupported source type: {rel_path}")))?;

    match kind {
        SourceKind::Markdown => {
            let text = std::fs::read_to_string(&path)?;
            let lines: Vec<&str> = text.lines().collect();
            let total = lines.len() as u32;
            let start = range.start_line.max(1);
            let end = if range.end_line == 0 {
                total
            } else {
                range.end_line.min(total)
            };
            let text = if total == 0 || start > total {
                String::new()
            } else {
                lines[(start - 1) as usize..end as usize].join("\n")
            };
            Ok(SourceText {
                path: rel_path.to_string(),
                kind,
                text,
                start_line: start,
                end_line: end,
                page_count: 0,
                truncated: false,
                next_page: None,
            })
        }
        SourceKind::Pdf => {
            let count = super::pdf::page_count(documents_root, rel_path)?;
            let max_pages = max_pages.max(1) as u32;
            let start = range.start_page.max(1);
            let end = if start as usize > count {
                start.saturating_sub(1)
            } else {
                let requested = if range.end_page == 0 {
                    count as u32
                } else {
                    range.end_page.min(count as u32)
                };
                requested.min(start + max_pages - 1).min(count as u32)
            };
            let pages = if start as usize > count {
                Vec::new()
            } else {
                super::pdf::extract(
                    documents_root,
                    rel_path,
                    Some(start as usize),
                    Some(end as usize),
                )?
            };
            let truncated = (end as usize) < count;
            Ok(SourceText {
                path: rel_path.to_string(),
                kind,
                text: super::pdf::page_text(&pages),
                start_line: 0,
                end_line: 0,
                page_count: count as u32,
                truncated,
                next_page: truncated.then_some(end + 1),
            })
        }
    }
}

/// Record that a source could not be read (for example a PDF without a text
/// layer). Status only: the document itself is never modified.
pub fn mark_failed(db: &Connection, rel_path: &str, error: &str) -> Result<bool> {
    let updated = db.execute(
        "UPDATE wiki_sources SET state = 'failed', error = ?1 WHERE rel_path = ?2",
        params![error, rel_path],
    )?;
    Ok(updated > 0)
}

fn validate_rel_path(rel_path: &str) -> Result<()> {
    if rel_path.is_empty() || rel_path.starts_with('/') || rel_path.contains('\\') {
        return Err(KandriumError("invalid source path".to_string()));
    }
    if rel_path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == ".." || part.starts_with('.'))
    {
        return Err(KandriumError("invalid source path".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_db(dir: &Path) -> Connection {
        let mut db = crate::db::open_db(&dir.join("test.db")).unwrap();
        crate::db::migrations::run_schema(&mut db).unwrap();
        db
    }

    fn docs(dir: &Path) -> PathBuf {
        let root = dir.join("documents");
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn hash_matches_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.md");
        std::fs::write(&file, "abc").unwrap();
        assert_eq!(
            hash_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn refresh_adds_markdown_and_pdf_sources() {
        let dir = tempfile::tempdir().unwrap();
        let root = docs(dir.path());
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/a.md"), "one").unwrap();
        std::fs::write(root.join("b.PDF"), b"%PDF-1.4\n").unwrap();

        let mut db = test_db(dir.path());
        let delta = refresh_sources(&mut db, &root).unwrap();
        assert_eq!(
            delta.added,
            vec!["b.PDF".to_string(), "nested/a.md".to_string()]
        );
        assert!(delta.changed.is_empty());
        assert!(delta.removed.is_empty());
        assert_eq!(delta.unchanged, 0);

        let sources = list_sources(&db, None).unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].rel_path, "b.PDF");
        assert_eq!(sources[0].kind, SourceKind::Pdf);
        assert_eq!(sources[0].state, SourceState::New);
        assert_eq!(sources[1].kind, SourceKind::Markdown);
        assert_eq!(sources[1].hash.len(), 64);

        let counts = counts(&db).unwrap();
        assert_eq!(counts.new, 2);
        assert_eq!(counts.indexed, 0);
    }

    #[test]
    fn refresh_marks_changed_sources_stale_and_removes_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let root = docs(dir.path());
        std::fs::write(root.join("a.md"), "one").unwrap();
        std::fs::write(root.join("b.md"), "two").unwrap();

        let mut db = test_db(dir.path());
        refresh_sources(&mut db, &root).unwrap();

        std::fs::write(root.join("a.md"), "one changed").unwrap();
        std::fs::remove_file(root.join("b.md")).unwrap();
        let delta = refresh_sources(&mut db, &root).unwrap();
        assert_eq!(delta.changed, vec!["a.md".to_string()]);
        assert_eq!(delta.removed, vec!["b.md".to_string()]);

        let sources = list_sources(&db, None).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].state, SourceState::Stale);

        let stale = list_sources(&db, Some(SourceState::Stale)).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].rel_path, "a.md");
    }

    #[test]
    fn refresh_is_idempotent_for_unchanged_sources() {
        let dir = tempfile::tempdir().unwrap();
        let root = docs(dir.path());
        std::fs::write(root.join("a.md"), "one").unwrap();

        let mut db = test_db(dir.path());
        refresh_sources(&mut db, &root).unwrap();
        let delta = refresh_sources(&mut db, &root).unwrap();
        assert!(delta.is_empty());
        assert_eq!(delta.unchanged, 1);
    }

    #[test]
    fn kind_and_relative_path_helpers_are_strict() {
        assert_eq!(kind_for_path(Path::new("x.PDF")), Some(SourceKind::Pdf));
        assert_eq!(
            kind_for_path(Path::new("x.markdown")),
            Some(SourceKind::Markdown)
        );
        assert_eq!(kind_for_path(Path::new("x.txt")), None);

        let root = Path::new("/docs");
        assert_eq!(
            relative_path(root, Path::new("/docs/a/b.md")).unwrap(),
            "a/b.md"
        );
        assert!(relative_path(root, Path::new("/elsewhere/a.md")).is_err());
    }
}
