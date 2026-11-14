//! Derived wiki index: page rows, citations, FTS5, and `index.md`.
//!
//! Files are authoritative. This module walks the wiki root, parses pages,
//! and keeps `wiki_pages`, `wiki_page_sources`, and `wiki_fts` in step. The
//! generated `index.md` is written from the same rows, so deleting the wiki
//! tables and reindexing reproduces everything from the files.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::{KandriumError, Result};
use crate::fsutil::{self, FsEntry};

use super::links;
use super::pages::{self, PAGE_EXTENSIONS};
use super::sources::{self, hash_bytes};
use super::{PageCounts, PageKind, PageSource, WikiPage, WikiStatus, source_counts};

/// Path of the generated table of contents.
pub const INDEX_PAGE: &str = "index.md";

/// Bodies above this size stay out of FTS; the page is still searchable by
/// title, summary, and aliases.
const MAX_FTS_BODY: usize = 1024 * 1024;

/// One search hit over `wiki_fts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub path: String,
    pub kind: PageKind,
    pub title: String,
    pub summary: String,
    /// Higher is better; derived from `bm25` with the sign flipped.
    pub score: f64,
    pub snippet: String,
}

/// Index one page: row, citations, and FTS entry.
///
/// `generated_hash` is deliberately not touched, so a page written by the
/// generator keeps the hash it was generated with even after a user edit.
pub fn index_page(db: &Connection, _root: &Path, page: &WikiPage, body: &str) -> Result<()> {
    let aliases = serde_json::to_string(&page.aliases)
        .map_err(|err| KandriumError(format!("cannot encode aliases: {err}")))?;
    let tags = serde_json::to_string(&page.tags)
        .map_err(|err| KandriumError(format!("cannot encode tags: {err}")))?;

    db.execute(
        "INSERT INTO wiki_pages
             (rel_path, kind, title, summary, aliases_json, tags_json,
              reviewed, content_hash, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (rel_path) DO UPDATE SET
             kind = excluded.kind,
             title = excluded.title,
             summary = excluded.summary,
             aliases_json = excluded.aliases_json,
             tags_json = excluded.tags_json,
             reviewed = excluded.reviewed,
             content_hash = excluded.content_hash,
             updated_at = excluded.updated_at",
        params![
            page.rel_path,
            page.kind.as_str(),
            page.title,
            page.summary,
            aliases,
            tags,
            page.reviewed as i64,
            page.content_hash,
            page.updated_at
        ],
    )?;

    db.execute(
        "DELETE FROM wiki_page_sources WHERE page_path = ?1",
        [&page.rel_path],
    )?;
    for source in &page.sources {
        db.execute(
            "INSERT OR IGNORE INTO wiki_page_sources
                 (page_path, source_path, source_hash, start_line, end_line,
                  start_page, end_page)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                page.rel_path,
                source.source_path,
                source.source_hash,
                source.start_line,
                source.end_line,
                source.start_page,
                source.end_page
            ],
        )?;
    }

    sync_fts_row(
        db,
        &page.rel_path,
        &page.title,
        &page.summary,
        &page.aliases,
        body,
    )
}

/// Reindex every page under `root` and regenerate `index.md`.
///
/// Pages that disappeared are dropped, `reviewed` is preserved when the file
/// does not state it, and the generated index is never treated as an input.
pub fn index_all_pages(db: &Connection, root: &Path) -> Result<usize> {
    if !root.exists() {
        return Ok(0);
    }
    let root = fsutil::canonical_root(root)?;
    let tree = fsutil::scan_tree(&root, PAGE_EXTENSIONS)?;
    let mut files = Vec::new();
    collect_files(&tree, &mut files);

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut bodies: Vec<(String, String)> = Vec::new();
    let tx = db.unchecked_transaction()?;
    for entry in files {
        let rel_path = sources::relative_path(&root, &entry.path)?;
        if rel_path == INDEX_PAGE {
            continue;
        }
        let bytes = std::fs::read(&entry.path)?;
        let text = String::from_utf8_lossy(&bytes);
        let mut parsed = pages::parse_page(&rel_path, &text);
        parsed.content_hash = hash_bytes(&bytes);

        let reviewed = match parsed.reviewed() {
            Some(reviewed) => reviewed,
            None => lookup_reviewed(&tx, &rel_path)?.unwrap_or(false),
        };
        let mut page_sources = pages::page_sources(&parsed);
        for source in &mut page_sources {
            source.source_hash = lookup_source_hash(&tx, &source.source_path)?.unwrap_or_default();
        }

        let page = WikiPage {
            rel_path: rel_path.clone(),
            kind: parsed.kind,
            title: parsed.title(),
            summary: parsed.summary(),
            aliases: parsed.aliases(),
            tags: parsed.tags(),
            reviewed,
            content_hash: parsed.content_hash.clone(),
            generated_hash: None,
            updated_at: modified_seconds(&entry.path),
            sources: page_sources,
        };
        index_page(&tx, &root, &page, &parsed.body)?;
        bodies.push((rel_path.clone(), parsed.body.clone()));
        seen.insert(rel_path);
    }
    links::rebuild_links(&tx, &bodies)?;

    let existing: Vec<String> = {
        let mut stmt = tx.prepare("SELECT rel_path FROM wiki_pages")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for rel_path in existing {
        if rel_path == INDEX_PAGE || seen.contains(&rel_path) {
            continue;
        }
        tx.execute("DELETE FROM wiki_pages WHERE rel_path = ?1", [&rel_path])?;
        tx.execute(
            "DELETE FROM wiki_page_sources WHERE page_path = ?1",
            [&rel_path],
        )?;
        tx.execute("DELETE FROM wiki_fts WHERE path = ?1", [&rel_path])?;
    }
    tx.commit()?;

    write_index_md(db, &root)?;
    Ok(seen.len())
}

/// Rebuild the FTS table from the files on disk. Used as the recovery path;
/// the per-page path updates one row instead.
pub fn rebuild_fts(db: &Connection, root: &Path) -> Result<usize> {
    if !root.exists() {
        return Ok(0);
    }
    let root = fsutil::canonical_root(root)?;
    let tree = fsutil::scan_tree(&root, PAGE_EXTENSIONS)?;
    let mut files = Vec::new();
    collect_files(&tree, &mut files);

    db.execute("DELETE FROM wiki_fts", [])?;
    let mut count = 0;
    for entry in files {
        let rel_path = sources::relative_path(&root, &entry.path)?;
        let text = std::fs::read_to_string(&entry.path)?;
        let parsed = pages::parse_page(&rel_path, &text);
        sync_fts_row(
            db,
            &rel_path,
            &parsed.title(),
            &parsed.summary(),
            &parsed.aliases(),
            &parsed.body,
        )?;
        count += 1;
    }
    Ok(count)
}

/// Regenerate `index.md` from `wiki_pages` and index the generated page.
pub fn write_index_md(db: &Connection, root: &Path) -> Result<()> {
    let markdown = render_index_markdown(db)?;
    fsutil::write_atomic(&root.join(INDEX_PAGE), &markdown)?;

    let hash = hash_bytes(markdown.as_bytes());
    db.execute(
        "INSERT INTO wiki_pages
             (rel_path, kind, title, summary, aliases_json, tags_json, reviewed,
              content_hash, generated_hash, updated_at)
         VALUES (?1, 'index', 'Wiki index', '', '[]', '[]', 0, ?2, ?2, ?3)
         ON CONFLICT (rel_path) DO UPDATE SET
             kind = 'index',
             title = 'Wiki index',
             summary = '',
             content_hash = excluded.content_hash,
             generated_hash = excluded.generated_hash,
             updated_at = excluded.updated_at",
        params![INDEX_PAGE, hash, now_unix()],
    )?;
    db.execute(
        "DELETE FROM wiki_page_sources WHERE page_path = ?1",
        [INDEX_PAGE],
    )?;
    sync_fts_row(db, INDEX_PAGE, "Wiki index", "", &[], &markdown)?;
    Ok(())
}

/// Full-text search over pages. The user query is tokenized and quoted, so
/// FTS operators in the input cannot produce a syntax error.
pub fn search(db: &Connection, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let Some(match_query) = fts_query(query) else {
        return Ok(Vec::new());
    };
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = db.prepare(
        "SELECT wiki_fts.path, p.kind, wiki_fts.title, wiki_fts.summary,
                bm25(wiki_fts) AS rank,
                snippet(wiki_fts, 4, '[', ']', '…', 12)
         FROM wiki_fts
         JOIN wiki_pages p ON p.rel_path = wiki_fts.path
         WHERE wiki_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![match_query, limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, f64>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;

    let mut hits = Vec::new();
    for row in rows {
        let (path, kind, title, summary, rank, snippet) = row?;
        let kind = PageKind::from_db(&kind)
            .ok_or_else(|| KandriumError(format!("unknown page kind in database: {kind}")))?;
        hits.push(SearchHit {
            path,
            kind,
            title,
            summary,
            score: -rank,
            snippet,
        });
    }
    Ok(hits)
}

/// Page metadata, optionally filtered by kind.
pub fn list_pages(db: &Connection, kind: Option<PageKind>) -> Result<Vec<WikiPage>> {
    const SELECT: &str = "SELECT rel_path, kind, title, summary, aliases_json,
                                 tags_json, reviewed, content_hash, generated_hash,
                                 updated_at
                          FROM wiki_pages";
    let mut out = Vec::new();
    if let Some(kind) = kind {
        let mut stmt = db.prepare(&format!("{SELECT} WHERE kind = ?1 ORDER BY rel_path"))?;
        let rows = stmt.query_map([kind.as_str()], map_page_row)?;
        for row in rows {
            out.push(decode_page(db, row?)?);
        }
    } else {
        let mut stmt = db.prepare(&format!("{SELECT} ORDER BY rel_path"))?;
        let rows = stmt.query_map([], map_page_row)?;
        for row in rows {
            out.push(decode_page(db, row?)?);
        }
    }
    Ok(out)
}

/// One page row, or `None` when the path is not indexed.
pub fn get_page(db: &Connection, rel_path: &str) -> Result<Option<WikiPage>> {
    let row = db
        .query_row(
            "SELECT rel_path, kind, title, summary, aliases_json, tags_json,
                    reviewed, content_hash, generated_hash, updated_at
             FROM wiki_pages WHERE rel_path = ?1",
            [rel_path],
            map_page_row,
        )
        .optional()?;
    match row {
        Some(row) => Ok(Some(decode_page(db, row)?)),
        None => Ok(None),
    }
}

/// Reindex one page from disk: row, citations, links, and `index.md`.
///
/// Used by the write pipeline so a single write never leaves the index half
/// updated.
pub fn reindex_page(db: &Connection, root: &Path, rel_path: &str) -> Result<()> {
    let root = fsutil::canonical_root(root)?;
    let path = fsutil::guard(&root, &root.join(rel_path))?;
    let bytes = std::fs::read(&path)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut parsed = pages::parse_page(rel_path, &text);
    parsed.content_hash = hash_bytes(&bytes);

    let reviewed = match parsed.reviewed() {
        Some(reviewed) => reviewed,
        None => lookup_reviewed(db, rel_path)?.unwrap_or(false),
    };
    let mut sources = pages::page_sources(&parsed);
    for source in &mut sources {
        source.source_hash = lookup_source_hash(db, &source.source_path)?.unwrap_or_default();
    }
    let page = WikiPage {
        rel_path: rel_path.to_string(),
        kind: parsed.kind,
        title: parsed.title(),
        summary: parsed.summary(),
        aliases: parsed.aliases(),
        tags: parsed.tags(),
        reviewed,
        content_hash: parsed.content_hash.clone(),
        generated_hash: None,
        updated_at: modified_seconds(&path),
        sources,
    };

    let tx = db.unchecked_transaction()?;
    index_page(&tx, &root, &page, &parsed.body)?;
    links::index_links(&tx, rel_path, &parsed.body)?;
    tx.commit()?;
    write_index_md(db, &root)?;
    Ok(())
}

/// Counts for status surfaces.
pub fn status(db: &Connection) -> Result<WikiStatus> {
    let mut pages = PageCounts::default();
    let mut stmt = db.prepare("SELECT kind, COUNT(*) FROM wiki_pages GROUP BY kind")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (kind, count) = row?;
        let count = count.max(0) as usize;
        match PageKind::from_db(&kind) {
            Some(PageKind::Source) => pages.source = count,
            Some(PageKind::Entity) => pages.entity = count,
            Some(PageKind::Concept) => pages.concept = count,
            Some(PageKind::Index) => pages.index = count,
            Some(PageKind::Page) => pages.page = count,
            None => {
                return Err(KandriumError(format!(
                    "unknown page kind in database: {kind}"
                )));
            }
        }
    }
    Ok(WikiStatus {
        sources: source_counts(db)?,
        pages,
    })
}

fn render_index_markdown(db: &Connection) -> Result<String> {
    let mut rows = Vec::new();
    {
        let mut stmt = db.prepare(
            "SELECT rel_path, kind, title, summary, aliases_json
             FROM wiki_pages
             WHERE kind != 'index'
             ORDER BY kind, title COLLATE NOCASE, rel_path",
        )?;
        let mapped = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in mapped {
            let (path, kind, title, summary, aliases_json) = row?;
            let kind = PageKind::from_db(&kind)
                .ok_or_else(|| KandriumError(format!("unknown page kind in database: {kind}")))?;
            rows.push((path, kind, title, summary, decode_strings(&aliases_json)));
        }
    }

    let mut first_source: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = db.prepare(
            "SELECT page_path, source_path FROM wiki_page_sources
             ORDER BY page_path, source_path",
        )?;
        let mapped = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in mapped {
            let (page, source) = row?;
            first_source.entry(page).or_insert(source);
        }
    }

    let mut source_pages: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = db.prepare(
            "SELECT ps.source_path, p.rel_path
             FROM wiki_page_sources ps
             JOIN wiki_pages p ON p.rel_path = ps.page_path
             WHERE p.kind = 'source'
             ORDER BY p.rel_path",
        )?;
        let mapped = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in mapped {
            let (source, page) = row?;
            source_pages.entry(source).or_insert(page);
        }
    }

    let mut out = String::from("# Wiki index\n\n");
    for (heading, kind) in [
        ("Concepts", PageKind::Concept),
        ("Entities", PageKind::Entity),
        ("Sources", PageKind::Source),
        ("Pages", PageKind::Page),
    ] {
        let mut entries: Vec<_> = rows.iter().filter(|row| row.1 == kind).collect();
        if entries.is_empty() {
            continue;
        }
        entries.sort_by(|a, b| {
            a.2.to_lowercase()
                .cmp(&b.2.to_lowercase())
                .then_with(|| a.0.cmp(&b.0))
        });
        out.push_str(&format!("## {heading}\n"));
        for (path, row_kind, title, summary, aliases) in entries {
            out.push_str(&format!("- [[{}|{}]]", link_target(path), one_line(title)));
            if !aliases.is_empty() {
                out.push_str(&format!(" `aliases: {}`", aliases.join(", ")));
            }
            if let Some(label) = source_label(*row_kind, path, &first_source, &source_pages) {
                out.push_str(&format!(" `source: {}`", one_line(&label)));
            }
            if !summary.trim().is_empty() {
                out.push_str(&format!(" - {}", one_line(summary)));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    Ok(out)
}

fn source_label(
    kind: PageKind,
    page_path: &str,
    first_source: &HashMap<String, String>,
    source_pages: &HashMap<String, String>,
) -> Option<String> {
    let document = first_source.get(page_path)?;
    if kind == PageKind::Source {
        return Some(document.clone());
    }
    let source_page = source_pages.get(document)?;
    Some(page_slug(source_page))
}

fn page_slug(rel_path: &str) -> String {
    let file = rel_path.rsplit('/').next().unwrap_or(rel_path);
    file.rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file)
        .to_string()
}

/// Wikilink target for a page: the root-relative path without the extension.
fn link_target(rel_path: &str) -> &str {
    rel_path
        .strip_suffix(".markdown")
        .or_else(|| rel_path.strip_suffix(".md"))
        .unwrap_or(rel_path)
}

fn one_line(value: &str) -> String {
    value
        .replace(['\r', '\n'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn sync_fts_row(
    db: &Connection,
    path: &str,
    title: &str,
    summary: &str,
    aliases: &[String],
    body: &str,
) -> Result<()> {
    db.execute("DELETE FROM wiki_fts WHERE path = ?1", [path])?;
    let body = if body.len() > MAX_FTS_BODY { "" } else { body };
    db.execute(
        "INSERT INTO wiki_fts (path, title, summary, aliases, body)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![path, title, summary, aliases.join(" "), body],
    )?;
    Ok(())
}

fn fts_query(input: &str) -> Option<String> {
    let tokens: Vec<String> = input
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|ch: char| {
                    matches!(
                        ch,
                        '"' | '\'' | ':' | '*' | '(' | ')' | '^' | '{' | '}' | '[' | ']' | '-'
                    )
                })
                .to_string()
        })
        .filter(|token| !token.is_empty())
        .filter(|token| {
            !matches!(
                token.to_ascii_uppercase().as_str(),
                "AND" | "OR" | "NOT" | "NEAR"
            )
        })
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
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

fn modified_seconds(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn lookup_source_hash(db: &Connection, source_path: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT hash FROM wiki_sources WHERE rel_path = ?1",
            [source_path],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

fn lookup_reviewed(db: &Connection, rel_path: &str) -> Result<Option<bool>> {
    let reviewed: Option<i64> = db
        .query_row(
            "SELECT reviewed FROM wiki_pages WHERE rel_path = ?1",
            [rel_path],
            |row| row.get(0),
        )
        .optional()?;
    Ok(reviewed.map(|value| value != 0))
}

fn decode_strings(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_default()
}

type PageRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
    Option<String>,
    i64,
);

fn map_page_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PageRow> {
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
        row.get(9)?,
    ))
}

fn decode_page(db: &Connection, row: PageRow) -> Result<WikiPage> {
    let (
        rel_path,
        kind,
        title,
        summary,
        aliases_json,
        tags_json,
        reviewed,
        content_hash,
        generated_hash,
        updated_at,
    ) = row;
    let kind = PageKind::from_db(&kind)
        .ok_or_else(|| KandriumError(format!("unknown page kind in database: {kind}")))?;
    let mut stmt = db.prepare(
        "SELECT source_path, source_hash, start_line, end_line, start_page, end_page
         FROM wiki_page_sources
         WHERE page_path = ?1
         ORDER BY source_path, start_line, start_page",
    )?;
    let rows = stmt.query_map([&rel_path], |row| {
        Ok(PageSource {
            source_path: row.get(0)?,
            source_hash: row.get(1)?,
            start_line: row.get::<_, i64>(2)?.max(0) as u32,
            end_line: row.get::<_, i64>(3)?.max(0) as u32,
            start_page: row.get::<_, i64>(4)?.max(0) as u32,
            end_page: row.get::<_, i64>(5)?.max(0) as u32,
        })
    })?;
    let mut sources = Vec::new();
    for source in rows {
        sources.push(source?);
    }

    Ok(WikiPage {
        rel_path,
        kind,
        title,
        summary,
        aliases: decode_strings(&aliases_json),
        tags: decode_strings(&tags_json),
        reviewed: reviewed != 0,
        content_hash,
        generated_hash,
        updated_at,
        sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        db: Connection,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("wiki");
        std::fs::create_dir_all(root.join("concepts")).unwrap();
        std::fs::create_dir_all(root.join("entities")).unwrap();
        std::fs::create_dir_all(root.join("sources")).unwrap();
        std::fs::create_dir_all(root.join("handwritten")).unwrap();
        let mut db = crate::db::open_db(&tmp.path().join("test.db")).unwrap();
        crate::db::migrations::run_schema(&mut db).unwrap();
        Fixture {
            _tmp: tmp,
            root,
            db,
        }
    }

    fn write(root: &Path, rel_path: &str, text: &str) {
        let path = root.join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn index_reconciles_pages_and_generates_the_index() {
        let f = fixture();
        f.db.execute(
            "INSERT INTO wiki_sources (rel_path, kind, hash, size, mtime, state)
             VALUES ('papers/attention.pdf', 'pdf', 'hash-attention', 10, 0, 'new')",
            [],
        )
        .unwrap();

        write(
            &f.root,
            "sources/attention-is-all-you-need.md",
            "---\ntype: source\ntitle: Attention Is All You Need\n\
             summary: The 2017 Transformer paper.\n\
             sources: [papers/attention.pdf]\n---\n\nBody.\n",
        );
        write(
            &f.root,
            "concepts/self-attention.md",
            "---\ntype: concept\ntitle: Self-Attention\n\
             summary: Relates each token to every other token.\n\
             aliases: [self attention]\ntags: [ml]\n\
             sources: [papers/attention.pdf]\n---\n\n\
             A claim.[^1]\n\n[^1]: documents/papers/attention.pdf#page=3\n",
        );
        write(
            &f.root,
            "handwritten/reading-notes.md",
            "# Reading notes\n\nPlain page.\n",
        );

        let count = index_all_pages(&f.db, &f.root).unwrap();
        assert_eq!(count, 3);

        let pages = list_pages(&f.db, None).unwrap();
        assert_eq!(pages.len(), 4);
        let concept = pages
            .iter()
            .find(|page| page.rel_path == "concepts/self-attention.md")
            .unwrap();
        assert_eq!(concept.kind, PageKind::Concept);
        assert_eq!(concept.title, "Self-Attention");
        assert_eq!(concept.summary, "Relates each token to every other token.");
        assert_eq!(concept.aliases, vec!["self attention"]);
        assert_eq!(concept.sources.len(), 2);
        let citation = concept
            .sources
            .iter()
            .find(|source| source.start_page == 3)
            .unwrap();
        assert_eq!(citation.source_path, "papers/attention.pdf");
        assert_eq!(citation.source_hash, "hash-attention");

        let hand = pages
            .iter()
            .find(|page| page.rel_path == "handwritten/reading-notes.md")
            .unwrap();
        assert_eq!(hand.kind, PageKind::Page);
        assert_eq!(hand.title, "Reading notes");

        let index = pages
            .iter()
            .find(|page| page.rel_path == INDEX_PAGE)
            .unwrap();
        assert_eq!(index.kind, PageKind::Index);
        assert_eq!(index.generated_hash, Some(index.content_hash.clone()));

        let markdown = std::fs::read_to_string(f.root.join(INDEX_PAGE)).unwrap();
        assert!(markdown.starts_with("# Wiki index\n"));
        assert!(markdown.contains("## Concepts"));
        assert!(
            markdown.contains(
                "- [[concepts/self-attention|Self-Attention]] `aliases: self attention` \
                 `source: attention-is-all-you-need` - Relates each token to every other token."
            ),
            "{markdown}"
        );
        assert!(markdown.contains("## Sources"));
        assert!(markdown.contains("- [[sources/attention-is-all-you-need|Attention Is All You Need]] `source: papers/attention.pdf` - The 2017 Transformer paper."));
        assert!(markdown.contains("## Pages"));
        assert!(markdown.contains("- [[handwritten/reading-notes|Reading notes]]"));

        let status = status(&f.db).unwrap();
        assert_eq!(status.sources.new, 1);
        assert_eq!(status.pages.concept, 1);
        assert_eq!(status.pages.source, 1);
        assert_eq!(status.pages.index, 1);
        assert_eq!(status.pages.page, 1);
    }

    #[test]
    fn reindex_is_idempotent_and_preserves_reviewed_and_generated_hash() {
        let f = fixture();
        write(
            &f.root,
            "concepts/attention.md",
            "---\ntype: concept\ntitle: Attention\n---\nBody.\n",
        );
        index_all_pages(&f.db, &f.root).unwrap();
        let first = list_pages(&f.db, Some(PageKind::Concept)).unwrap();
        assert_eq!(first.len(), 1);
        assert!(!first[0].reviewed);

        f.db.execute(
            "UPDATE wiki_pages SET reviewed = 1, generated_hash = 'abc'
             WHERE rel_path = 'concepts/attention.md'",
            [],
        )
        .unwrap();

        let count = index_all_pages(&f.db, &f.root).unwrap();
        assert_eq!(count, 1);
        let again = list_pages(&f.db, Some(PageKind::Concept)).unwrap();
        assert_eq!(again.len(), 1);
        assert!(again[0].reviewed);
        assert_eq!(again[0].generated_hash.as_deref(), Some("abc"));
        assert_eq!(again[0].content_hash, first[0].content_hash);
    }

    #[test]
    fn removed_pages_drop_rows_and_search_hits() {
        let f = fixture();
        write(
            &f.root,
            "concepts/attention.md",
            "---\ntype: concept\ntitle: Attention\n---\nUniqueToken body.\n",
        );
        index_all_pages(&f.db, &f.root).unwrap();
        assert_eq!(search(&f.db, "UniqueToken", 10).unwrap().len(), 1);

        std::fs::remove_file(f.root.join("concepts/attention.md")).unwrap();
        index_all_pages(&f.db, &f.root).unwrap();
        assert!(
            list_pages(&f.db, Some(PageKind::Concept))
                .unwrap()
                .is_empty()
        );
        assert!(search(&f.db, "UniqueToken", 10).unwrap().is_empty());
        let sources: i64 =
            f.db.query_row(
                "SELECT COUNT(*) FROM wiki_page_sources
                 WHERE page_path = 'concepts/attention.md'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sources, 0);
    }

    #[test]
    fn search_matches_body_and_survives_fts_syntax() {
        let f = fixture();
        write(
            &f.root,
            "entities/vaswani.md",
            "---\ntype: entity\ntitle: Ashish Vaswani\naliases: [vaswani]\n---\n\
             Lead author of the transformer paper.\n",
        );
        index_all_pages(&f.db, &f.root).unwrap();

        let hits = search(&f.db, "transformer", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "entities/vaswani.md");
        assert_eq!(hits[0].kind, PageKind::Entity);
        assert!(
            hits[0].snippet.contains("[transformer]") || hits[0].snippet.contains("transformer")
        );

        let hits = search(&f.db, "vaswani", 5).unwrap();
        assert!(
            hits.iter()
                .any(|hit| hit.path == "entities/vaswani.md" && hit.kind == PageKind::Entity)
        );
        assert!(search(&f.db, "\"unbalanced", 5).unwrap().is_empty());
        assert!(search(&f.db, "path:foo*", 5).unwrap().is_empty());
        assert!(search(&f.db, "   ", 5).unwrap().is_empty());
        assert!(search(&f.db, "transformer", 0).unwrap().is_empty());
    }

    #[test]
    fn rebuild_fts_repopulates_from_files() {
        let f = fixture();
        write(
            &f.root,
            "concepts/attention.md",
            "---\ntype: concept\ntitle: Attention\n---\nRebuildToken.\n",
        );
        index_all_pages(&f.db, &f.root).unwrap();
        f.db.execute("DELETE FROM wiki_fts", []).unwrap();
        assert!(search(&f.db, "RebuildToken", 10).unwrap().is_empty());

        let count = rebuild_fts(&f.db, &f.root).unwrap();
        assert_eq!(count, 2);
        assert_eq!(search(&f.db, "RebuildToken", 10).unwrap().len(), 1);
        assert!(search(&f.db, "index", 10).unwrap().len() == 1);
    }

    #[test]
    fn missing_root_indexes_nothing() {
        let f = fixture();
        let missing = f.root.join("does-not-exist");
        assert_eq!(index_all_pages(&f.db, &missing).unwrap(), 0);
        assert!(list_pages(&f.db, None).unwrap().is_empty());
    }

    #[test]
    fn malformed_frontmatter_is_still_indexed() {
        let f = fixture();
        write(
            &f.root,
            "concepts/broken.md",
            "---\ntitle: Open\nBody without closing.\n",
        );
        index_all_pages(&f.db, &f.root).unwrap();
        let pages = list_pages(&f.db, Some(PageKind::Concept)).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].rel_path, "concepts/broken.md");
        assert_eq!(pages[0].title, "broken");
        assert!(pages[0].summary.is_empty());
    }

    #[test]
    fn index_builds_the_link_graph_and_backlinks() {
        let f = fixture();
        write(
            &f.root,
            "concepts/attention.md",
            "---\ntype: concept\ntitle: Attention\n---\nSee [[entities/vaswani]].\n",
        );
        write(
            &f.root,
            "entities/vaswani.md",
            "---\ntype: entity\ntitle: Ashish Vaswani\n---\nAuthor.\n",
        );
        index_all_pages(&f.db, &f.root).unwrap();

        let inbound = super::links::backlinks(&f.db, "entities/vaswani.md").unwrap();
        assert_eq!(inbound.len(), 1);
        assert_eq!(inbound[0].from, "concepts/attention.md");

        std::fs::remove_file(f.root.join("concepts/attention.md")).unwrap();
        index_all_pages(&f.db, &f.root).unwrap();
        assert!(
            super::links::backlinks(&f.db, "entities/vaswani.md")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn source_state_counts_are_reported_alongside_pages() {
        let f = fixture();
        f.db.execute(
            "INSERT INTO wiki_sources (rel_path, kind, hash, size, mtime, state)
             VALUES ('a.md', 'markdown', 'h', 1, 0, 'stale')",
            [],
        )
        .unwrap();
        let status = status(&f.db).unwrap();
        assert_eq!(status.sources.stale, 1);
        assert_eq!(status.sources.new, 0);
        assert_eq!(status.pages.index, 0);
    }
}
