//! Wiki write pipeline: guarded, atomic, and index-consistent.
//!
//! The agent never writes a file directly. Every write validates the page
//! contract, checks the user-edit flag and the caller's expected hash, writes
//! through `fsutil::write_atomic`, and reindexes the page plus `index.md` in
//! the same call.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::{KandriumError, Result};
use crate::fsutil;

use super::index;
use super::instructions::WIKI_INSTRUCTIONS_VERSION;
use super::pages::{self, validate_page_path};
use super::sources;
use super::{PageKind, WikiPage, page_kind_for_path};

/// What a write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteAction {
    Created,
    Replaced,
    Unchanged,
}

/// Input for [`write_page`].
#[derive(Debug, Clone)]
pub struct WritePageSpec {
    pub rel_path: String,
    pub content: String,
    pub expected_hash: Option<String>,
    pub force: bool,
    pub agent: Option<String>,
    pub instructions_version: u32,
}

/// Result of a page write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageWrite {
    pub path: String,
    pub previous_hash: Option<String>,
    pub content_hash: String,
    pub action: WriteAction,
}

/// Input for [`patch_page`].
#[derive(Debug, Clone)]
pub struct PatchPageSpec {
    pub rel_path: String,
    pub old_text: String,
    pub new_text: String,
    pub expected_hash: Option<String>,
    pub force: bool,
    pub agent: Option<String>,
    pub instructions_version: u32,
}

/// Result of a patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PagePatch {
    pub path: String,
    pub replaced: u32,
    pub previous_hash: Option<String>,
    pub content_hash: String,
}

/// Input for [`delete_page`].
#[derive(Debug, Clone)]
pub struct DeletePageSpec {
    pub rel_path: String,
    pub expected_hash: Option<String>,
    pub force: bool,
    pub instructions_version: u32,
}

/// Counters recorded with a run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStats {
    pub pages_created: u32,
    pub pages_updated: u32,
    pub pages_skipped: u32,
    pub sources_compiled: u32,
    pub error_count: u32,
}

/// Input for [`record_run`].
#[derive(Debug, Clone, Default)]
pub struct RunRecord {
    pub agent: Option<String>,
    pub operation: Option<String>,
    pub instructions_version: Option<u32>,
    pub pages: Vec<String>,
    pub sources: Vec<String>,
    pub stats: RunStats,
    pub note: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<i64>,
}

/// One row of the run ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiRun {
    pub id: String,
    pub operation: String,
    pub agent: String,
    pub instructions_version: u32,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub stats: RunStats,
    pub pages: Vec<String>,
    pub sources: Vec<String>,
    pub note: Option<String>,
    pub error: Option<String>,
}

/// A set of pages that share a title or alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupGroup {
    pub label: String,
    pub pages: Vec<WikiPage>,
}

/// Write one page. Rejects a stale instruction version, a path outside the
/// wiki root, a malformed compiled page, and a reviewed page without `force`.
pub fn write_page(db: &mut Connection, root: &Path, spec: &WritePageSpec) -> Result<PageWrite> {
    ensure_version(spec.instructions_version)?;
    validate_target(&spec.rel_path)?;
    validate_content(&spec.rel_path, &spec.content)?;

    let root = fsutil::canonical_root(root)?;
    let target = fsutil::guard(&root, &root.join(&spec.rel_path))?;
    let existing = read_if_exists(&target)?;
    ensure_editable(db, &spec.rel_path, existing.as_deref(), spec.force)?;

    let previous_hash = existing.as_deref().map(sources::hash_bytes);
    if let Some(expected) = &spec.expected_hash {
        match &previous_hash {
            Some(current) if current == expected => {}
            Some(current) => {
                return Err(KandriumError(format!(
                    "page changed since it was read (current hash {current})"
                )));
            }
            None => return Err(KandriumError::new("page does not exist")),
        }
    }

    let content_hash = sources::hash_bytes(spec.content.as_bytes());
    let action = match &previous_hash {
        None => WriteAction::Created,
        Some(previous) if previous == &content_hash => WriteAction::Unchanged,
        Some(_) => WriteAction::Replaced,
    };

    if action != WriteAction::Unchanged {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fsutil::write_atomic(&target, spec.content.as_bytes())?;
    }

    index::reindex_page(db, &root, &spec.rel_path)?;
    // Remember what the agent wrote: a later user edit changes content_hash
    // while generated_hash stays behind, which is how merge/lint can tell.
    db.execute(
        "UPDATE wiki_pages SET generated_hash = ?1 WHERE rel_path = ?2",
        params![content_hash, spec.rel_path],
    )?;
    let parsed = pages::parse_page(&spec.rel_path, &spec.content);
    mark_compiled(
        db,
        pages::page_sources(&parsed)
            .into_iter()
            .map(|source| source.source_path),
    )?;

    Ok(PageWrite {
        path: spec.rel_path.clone(),
        previous_hash,
        content_hash,
        action,
    })
}

/// Replace text in an existing page, then run the same write checks.
pub fn patch_page(db: &mut Connection, root: &Path, spec: &PatchPageSpec) -> Result<PagePatch> {
    ensure_version(spec.instructions_version)?;
    validate_target(&spec.rel_path)?;
    if spec.old_text.is_empty() {
        return Err(KandriumError::new("oldText must not be empty"));
    }

    let root = fsutil::canonical_root(root)?;
    let target = fsutil::guard(&root, &root.join(&spec.rel_path))?;
    let existing = read_if_exists(&target)?;
    let Some(bytes) = existing else {
        return Err(KandriumError::new("page does not exist"));
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let replaced = text.matches(&spec.old_text).count();
    if replaced == 0 {
        return Err(KandriumError::new("oldText was not found in the page"));
    }
    let content = text.replace(&spec.old_text, &spec.new_text);

    let write = write_page(
        db,
        &root,
        &WritePageSpec {
            rel_path: spec.rel_path.clone(),
            content,
            expected_hash: spec.expected_hash.clone(),
            force: spec.force,
            agent: spec.agent.clone(),
            instructions_version: spec.instructions_version,
        },
    )?;
    Ok(PagePatch {
        path: write.path,
        replaced: replaced as u32,
        previous_hash: write.previous_hash,
        content_hash: write.content_hash,
    })
}

/// Delete a page file and every derived row that points at it.
pub fn delete_page(db: &mut Connection, root: &Path, spec: &DeletePageSpec) -> Result<()> {
    ensure_version(spec.instructions_version)?;
    validate_target(&spec.rel_path)?;

    let root = fsutil::canonical_root(root)?;
    let target = fsutil::guard(&root, &root.join(&spec.rel_path))?;
    let existing = read_if_exists(&target)?;
    let stored = index::get_page(db, &spec.rel_path)?;
    if existing.is_none() && stored.is_none() {
        return Err(KandriumError::new("page does not exist"));
    }
    ensure_editable(db, &spec.rel_path, existing.as_deref(), spec.force)?;

    if let Some(expected) = &spec.expected_hash {
        match existing.as_deref().map(sources::hash_bytes) {
            Some(current) if &current == expected => {}
            Some(current) => {
                return Err(KandriumError(format!(
                    "page changed since it was read (current hash {current})"
                )));
            }
            None => return Err(KandriumError::new("page does not exist")),
        }
    }

    if existing.is_some() {
        fs::remove_file(&target)?;
    }

    let affected: Vec<String> = {
        let mut stmt =
            db.prepare("SELECT DISTINCT source_path FROM wiki_page_sources WHERE page_path = ?1")?;
        let rows = stmt.query_map([&spec.rel_path], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let tx = db.transaction()?;
    tx.execute(
        "DELETE FROM wiki_pages WHERE rel_path = ?1",
        [&spec.rel_path],
    )?;
    tx.execute(
        "DELETE FROM wiki_page_sources WHERE page_path = ?1",
        [&spec.rel_path],
    )?;
    tx.execute(
        "DELETE FROM wiki_links WHERE from_path = ?1 OR to_path = ?1",
        [&spec.rel_path],
    )?;
    tx.execute("DELETE FROM wiki_fts WHERE path = ?1", [&spec.rel_path])?;
    tx.commit()?;

    refresh_source_states(db, &affected)?;
    index::write_index_md(db, &root)?;
    Ok(())
}

/// Append a run to the ledger and mark the compiled sources.
pub fn record_run(db: &mut Connection, record: RunRecord) -> Result<WikiRun> {
    let version = record
        .instructions_version
        .unwrap_or(WIKI_INSTRUCTIONS_VERSION);
    ensure_version(version)?;

    let now = now_unix();
    let run = WikiRun {
        id: crate::db::uid(),
        operation: record.operation.unwrap_or_else(|| "compile".to_string()),
        agent: record.agent.unwrap_or_default(),
        instructions_version: version,
        started_at: record.started_at.unwrap_or(now),
        finished_at: Some(now),
        stats: record.stats,
        pages: record.pages,
        sources: record.sources,
        note: record.note,
        error: record.error,
    };
    let payload = RunPayload {
        stats: run.stats.clone(),
        pages: run.pages.clone(),
        sources: run.sources.clone(),
    };
    let stats_json = serde_json::to_string(&payload)
        .map_err(|error| KandriumError(format!("cannot encode run stats: {error}")))?;

    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO wiki_runs
             (id, operation, agent, instructions_version, started_at, finished_at,
              stats_json, note, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            run.id,
            run.operation,
            run.agent,
            run.instructions_version,
            run.started_at,
            run.finished_at,
            stats_json,
            run.note,
            run.error
        ],
    )?;
    for source in &run.sources {
        mark_source_compiled(&tx, source, now)?;
    }
    tx.commit()?;
    Ok(run)
}

/// Most recent run, if any.
pub fn latest_run(db: &Connection) -> Result<Option<WikiRun>> {
    let row = db
        .query_row(
            "SELECT id, operation, agent, instructions_version, started_at,
                    finished_at, stats_json, note, error
             FROM wiki_runs
             ORDER BY started_at DESC, id DESC
             LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .optional()?;
    let Some((id, operation, agent, version, started_at, finished_at, stats_json, note, error)) =
        row
    else {
        return Ok(None);
    };
    let payload: RunPayload = serde_json::from_str(&stats_json).unwrap_or_default();
    Ok(Some(WikiRun {
        id,
        operation,
        agent,
        instructions_version: version.max(0) as u32,
        started_at,
        finished_at,
        stats: payload.stats,
        pages: payload.pages,
        sources: payload.sources,
        note,
        error,
    }))
}

/// Pages that share a normalized title or alias, for merge decisions.
pub fn dedup_candidates(db: &Connection) -> Result<Vec<DedupGroup>> {
    let pages = index::list_pages(db, None)?;
    let by_path: HashMap<&str, &WikiPage> = pages
        .iter()
        .map(|page| (page.rel_path.as_str(), page))
        .collect();

    let mut owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for page in &pages {
        if page.kind == PageKind::Index {
            continue;
        }
        let mut labels = vec![page.title.clone()];
        labels.extend(page.aliases.iter().cloned());
        for label in labels {
            let key = normalize_label(&label);
            if key.is_empty() {
                continue;
            }
            owners.entry(key).or_default().insert(page.rel_path.clone());
        }
    }

    let mut groups = Vec::new();
    for (label, paths) in owners {
        if paths.len() < 2 {
            continue;
        }
        let pages: Vec<WikiPage> = paths
            .iter()
            .filter_map(|path| by_path.get(path.as_str()).map(|page| (*page).clone()))
            .collect();
        groups.push(DedupGroup { label, pages });
    }
    Ok(groups)
}

fn ensure_version(version: u32) -> Result<()> {
    if version != WIKI_INSTRUCTIONS_VERSION {
        return Err(KandriumError(format!(
            "stale instructions version {version}; call get_wiki_instructions"
        )));
    }
    Ok(())
}

fn validate_target(rel_path: &str) -> Result<()> {
    validate_page_path(rel_path)?;
    if page_kind_for_path(rel_path) == PageKind::Index {
        return Err(KandriumError::new(
            "index.md is generated and cannot be written",
        ));
    }
    Ok(())
}

fn validate_content(rel_path: &str, content: &str) -> Result<()> {
    let (frontmatter, _) = pages::parse_frontmatter(content)
        .map_err(|error| KandriumError(format!("invalid frontmatter: {error}")))?;
    let title = frontmatter.scalar("title").map(str::trim).unwrap_or("");
    if title.is_empty() {
        return Err(KandriumError::new("frontmatter is missing title"));
    }
    if let Some(expected) = compiled_type(page_kind_for_path(rel_path)) {
        let declared = frontmatter.scalar("type").map(str::trim).unwrap_or("");
        if !declared.eq_ignore_ascii_case(expected) {
            return Err(KandriumError(format!(
                "frontmatter type must be \"{expected}\" for this folder"
            )));
        }
    }
    Ok(())
}

fn compiled_type(kind: PageKind) -> Option<&'static str> {
    match kind {
        PageKind::Source => Some("source"),
        PageKind::Entity => Some("entity"),
        PageKind::Concept => Some("concept"),
        PageKind::Index | PageKind::Page => None,
    }
}

fn read_if_exists(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn ensure_editable(
    db: &Connection,
    rel_path: &str,
    existing: Option<&[u8]>,
    force: bool,
) -> Result<()> {
    if force {
        return Ok(());
    }
    let reviewed = match existing {
        Some(bytes) => {
            let text = String::from_utf8_lossy(bytes);
            let parsed = pages::parse_page(rel_path, &text);
            parsed.reviewed().unwrap_or(false) || stored_reviewed(db, rel_path)?
        }
        None => stored_reviewed(db, rel_path)?,
    };
    if reviewed {
        return Err(KandriumError::new(
            "page is marked reviewed; pass force to overwrite it",
        ));
    }
    Ok(())
}

fn stored_reviewed(db: &Connection, rel_path: &str) -> Result<bool> {
    let reviewed: Option<i64> = db
        .query_row(
            "SELECT reviewed FROM wiki_pages WHERE rel_path = ?1",
            [rel_path],
            |row| row.get(0),
        )
        .optional()?;
    Ok(reviewed.unwrap_or(0) != 0)
}

fn mark_compiled(db: &Connection, paths: impl IntoIterator<Item = String>) -> Result<()> {
    let now = now_unix();
    for path in paths {
        mark_source_compiled(db, &path, now)?;
    }
    Ok(())
}

fn mark_source_compiled(db: &Connection, rel_path: &str, now: i64) -> Result<()> {
    db.execute(
        "UPDATE wiki_sources
            SET state = 'compiled', compiled_at = ?1
          WHERE rel_path = ?2 AND state != 'failed'",
        params![now, rel_path],
    )?;
    Ok(())
}

fn refresh_source_states(db: &Connection, sources: &[String]) -> Result<()> {
    for source in sources {
        db.execute(
            "UPDATE wiki_sources
                SET state = CASE
                    WHEN state IN ('failed', 'stale') THEN state
                    WHEN EXISTS (SELECT 1 FROM wiki_page_sources
                                  WHERE source_path = wiki_sources.rel_path)
                        THEN 'compiled'
                    ELSE 'indexed'
                END
              WHERE rel_path = ?1",
            [source],
        )?;
    }
    Ok(())
}

fn normalize_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RunPayload {
    #[serde(default)]
    stats: RunStats,
    #[serde(default)]
    pages: Vec<String>,
    #[serde(default)]
    sources: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        documents: PathBuf,
        db: Connection,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("wiki");
        let documents = tmp.path().join("documents");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&documents).unwrap();
        let mut db = crate::db::open_db(&tmp.path().join("test.db")).unwrap();
        crate::db::migrations::run_schema(&mut db).unwrap();
        Fixture {
            _tmp: tmp,
            root,
            documents,
            db,
        }
    }

    fn page(title: &str, reviewed: bool) -> String {
        format!(
            "---\ntype: concept\ntitle: {title}\nsummary: A page.\n\
             aliases: [{title}]\nreviewed: {reviewed}\n---\n\
             This page has a body long enough to avoid the empty-body warning.\n"
        )
    }

    fn write_spec(path: &str, content: String, expected: Option<&str>) -> WritePageSpec {
        WritePageSpec {
            rel_path: path.to_string(),
            content,
            expected_hash: expected.map(str::to_string),
            force: false,
            agent: Some("test".into()),
            instructions_version: WIKI_INSTRUCTIONS_VERSION,
        }
    }

    #[test]
    fn write_creates_the_file_row_and_index_entry() {
        let mut f = fixture();
        std::fs::write(f.documents.join("reading.md"), "one\ntwo\n").unwrap();
        sources::refresh_sources(&mut f.db, &f.documents).unwrap();

        let content = "---\ntype: concept\ntitle: Attention\nsummary: Shared idea.\n\
                       aliases: [attention]\nsources: [reading.md]\n---\n\
                       Attention body is long enough to pass the empty-page check.[^1]\n\n\
                       [^1]: documents/reading.md#L1-L2\n";
        let result = write_page(
            &mut f.db,
            &f.root,
            &write_spec("concepts/attention.md", content.into(), None),
        )
        .unwrap();
        assert_eq!(result.action, WriteAction::Created);
        assert!(result.previous_hash.is_none());
        assert!(f.root.join("concepts/attention.md").exists());

        let stored = index::get_page(&f.db, "concepts/attention.md")
            .unwrap()
            .unwrap();
        assert_eq!(stored.kind, PageKind::Concept);
        assert_eq!(stored.sources.len(), 2);
        assert!(f.root.join("index.md").exists());

        let state: String =
            f.db.query_row(
                "SELECT state FROM wiki_sources WHERE rel_path = 'reading.md'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "compiled");

        let again = write_page(
            &mut f.db,
            &f.root,
            &write_spec("concepts/attention.md", content.into(), None),
        )
        .unwrap();
        assert_eq!(again.action, WriteAction::Unchanged);
    }

    #[test]
    fn writes_reject_stale_hashes_reviewed_pages_and_bad_versions() {
        let mut f = fixture();
        let created = write_page(
            &mut f.db,
            &f.root,
            &write_spec("concepts/attention.md", page("Attention", false), None),
        )
        .unwrap();

        let conflict = write_page(
            &mut f.db,
            &f.root,
            &write_spec(
                "concepts/attention.md",
                page("Attention", false),
                Some("stale"),
            ),
        );
        assert!(conflict.is_err());

        let reviewed = write_page(
            &mut f.db,
            &f.root,
            &write_spec(
                "concepts/attention.md",
                page("Attention", true),
                Some(&created.content_hash),
            ),
        )
        .unwrap();
        assert_eq!(reviewed.action, WriteAction::Replaced);

        let refused = write_page(
            &mut f.db,
            &f.root,
            &write_spec("concepts/attention.md", page("Attention", false), None),
        );
        assert!(refused.is_err());

        let mut forced = write_spec("concepts/attention.md", page("Attention", false), None);
        forced.force = true;
        assert!(write_page(&mut f.db, &f.root, &forced).is_ok());

        let mut stale = write_spec("concepts/attention.md", page("Attention", false), None);
        stale.instructions_version = WIKI_INSTRUCTIONS_VERSION + 1;
        assert!(write_page(&mut f.db, &f.root, &stale).is_err());
    }

    #[test]
    fn paths_escaping_the_root_and_generated_index_are_rejected() {
        let mut f = fixture();
        assert!(
            write_page(
                &mut f.db,
                &f.root,
                &write_spec("../escape.md", page("X", false), None)
            )
            .is_err()
        );
        assert!(
            write_page(
                &mut f.db,
                &f.root,
                &write_spec("index.md", page("X", false), None)
            )
            .is_err()
        );
        assert!(
            write_page(
                &mut f.db,
                &f.root,
                &write_spec("concepts/x.txt", page("X", false), None)
            )
            .is_err()
        );
        assert!(
            write_page(
                &mut f.db,
                &f.root,
                &write_spec("concepts/x.md", "no frontmatter".into(), None)
            )
            .is_err()
        );
    }

    #[test]
    fn patch_replaces_text_and_delete_cleans_the_index() {
        let mut f = fixture();
        let spec = write_spec("concepts/attention.md", page("Attention", false), None);
        let created = write_page(&mut f.db, &f.root, &spec).unwrap();

        let patch = patch_page(
            &mut f.db,
            &f.root,
            &PatchPageSpec {
                rel_path: "concepts/attention.md".into(),
                old_text: "A page.".into(),
                new_text: "An updated page.".into(),
                expected_hash: Some(created.content_hash.clone()),
                force: false,
                agent: Some("test".into()),
                instructions_version: WIKI_INSTRUCTIONS_VERSION,
            },
        )
        .unwrap();
        assert_eq!(patch.replaced, 1);
        let text = std::fs::read_to_string(f.root.join("concepts/attention.md")).unwrap();
        assert!(text.contains("An updated page."));

        delete_page(
            &mut f.db,
            &f.root,
            &DeletePageSpec {
                rel_path: "concepts/attention.md".into(),
                expected_hash: Some(patch.content_hash),
                force: false,
                instructions_version: WIKI_INSTRUCTIONS_VERSION,
            },
        )
        .unwrap();
        assert!(!f.root.join("concepts/attention.md").exists());
        assert!(
            index::get_page(&f.db, "concepts/attention.md")
                .unwrap()
                .is_none()
        );
        let fts: i64 =
            f.db.query_row(
                "SELECT COUNT(*) FROM wiki_fts WHERE path = 'concepts/attention.md'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fts, 0);
    }

    #[test]
    fn delete_requires_force_for_a_reviewed_page() {
        let mut f = fixture();
        write_page(
            &mut f.db,
            &f.root,
            &write_spec("concepts/attention.md", page("Attention", true), None),
        )
        .unwrap();
        let blocked = delete_page(
            &mut f.db,
            &f.root,
            &DeletePageSpec {
                rel_path: "concepts/attention.md".into(),
                expected_hash: None,
                force: false,
                instructions_version: WIKI_INSTRUCTIONS_VERSION,
            },
        );
        assert!(blocked.is_err());

        delete_page(
            &mut f.db,
            &f.root,
            &DeletePageSpec {
                rel_path: "concepts/attention.md".into(),
                expected_hash: None,
                force: true,
                instructions_version: WIKI_INSTRUCTIONS_VERSION,
            },
        )
        .unwrap();
    }

    #[test]
    fn runs_are_recorded_and_sources_marked_compiled() {
        let mut f = fixture();
        std::fs::write(f.documents.join("reading.md"), "one\n").unwrap();
        sources::refresh_sources(&mut f.db, &f.documents).unwrap();

        let run = record_run(
            &mut f.db,
            RunRecord {
                agent: Some("codex".into()),
                pages: vec!["concepts/attention.md".into()],
                sources: vec!["reading.md".into()],
                stats: RunStats {
                    pages_created: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(run.agent, "codex");
        assert_eq!(run.stats.pages_created, 1);
        assert_eq!(latest_run(&f.db).unwrap().unwrap().id, run.id);

        let state: String =
            f.db.query_row(
                "SELECT state FROM wiki_sources WHERE rel_path = 'reading.md'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "compiled");
    }

    #[test]
    fn dedup_groups_pages_sharing_a_label() {
        let mut f = fixture();
        write_page(
            &mut f.db,
            &f.root,
            &write_spec(
                "concepts/one.md",
                "---\ntype: concept\ntitle: Attention\nsummary: One.\n\
                 aliases: [attention]\n---\nBody long enough to avoid the empty check.\n"
                    .into(),
                None,
            ),
        )
        .unwrap();
        write_page(
            &mut f.db,
            &f.root,
            &write_spec(
                "concepts/two.md",
                "---\ntype: concept\ntitle: Attention\nsummary: Two.\n---\n\
                 Another body long enough to avoid the empty check.\n"
                    .into(),
                None,
            ),
        )
        .unwrap();

        let groups = dedup_candidates(&f.db).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "attention");
        assert_eq!(groups[0].pages.len(), 2);
    }
}
