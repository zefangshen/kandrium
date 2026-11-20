//! Lint rules for the wiki.
//!
//! Lint is read-only: it reports findings and never edits a page. Every rule
//! is evaluated from the files plus the derived index, so a page the agent
//! wrote badly becomes visible instead of silently wrong.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::fsutil::{self, FsEntry};

use super::pages::{self, PAGE_EXTENSIONS, PageText};
use super::sources;
use super::{PageKind, SourceKind, SourceState, WikiSource, links, page_kind_for_path};

/// Finding codes from the plan's lint table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LintCode {
    BadFrontmatter,
    UnmanagedPage,
    DeadLink,
    MissingAlias,
    OrphanPage,
    EmptyPage,
    MissingSource,
    StalePage,
    OrphanedBySource,
    BadCitation,
    DuplicatePage,
    Contradiction,
    IndexDrift,
    PdfUnreadable,
    UnrecordedRun,
}

impl LintCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadFrontmatter => "bad-frontmatter",
            Self::UnmanagedPage => "unmanaged-page",
            Self::DeadLink => "dead-link",
            Self::MissingAlias => "missing-alias",
            Self::OrphanPage => "orphan-page",
            Self::EmptyPage => "empty-page",
            Self::MissingSource => "missing-source",
            Self::StalePage => "stale-page",
            Self::OrphanedBySource => "orphaned-by-source",
            Self::BadCitation => "bad-citation",
            Self::DuplicatePage => "duplicate-page",
            Self::Contradiction => "contradiction",
            Self::IndexDrift => "index-drift",
            Self::PdfUnreadable => "pdf-unreadable",
            Self::UnrecordedRun => "unrecorded-run",
        }
    }

    pub fn severity(self) -> LintSeverity {
        match self {
            Self::BadFrontmatter
            | Self::DeadLink
            | Self::MissingSource
            | Self::BadCitation
            | Self::IndexDrift
            | Self::PdfUnreadable => LintSeverity::Error,
            Self::UnmanagedPage
            | Self::MissingAlias
            | Self::OrphanPage
            | Self::EmptyPage
            | Self::StalePage
            | Self::OrphanedBySource
            | Self::DuplicatePage => LintSeverity::Warn,
            Self::Contradiction | Self::UnrecordedRun => LintSeverity::Info,
        }
    }

    pub fn fixable(self) -> bool {
        matches!(
            self,
            Self::DeadLink | Self::MissingAlias | Self::DuplicatePage | Self::IndexDrift
        )
    }
}

/// Finding severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LintSeverity {
    Error,
    Warn,
    Info,
}

/// One lint result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LintFinding {
    pub code: LintCode,
    pub severity: LintSeverity,
    pub path: String,
    /// 1-based line, or 0 for page-level findings.
    pub line: u32,
    pub message: String,
    pub fixable: bool,
}

impl LintFinding {
    fn new(code: LintCode, path: &str, line: u32, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: code.severity(),
            path: path.to_string(),
            line,
            message: message.into(),
            fixable: code.fixable(),
        }
    }
}

/// Run every rule. `scope` limits findings to one page or folder.
///
/// `documents_root` is needed to validate citation line ranges against the
/// source text; the wiki never edits a document.
pub fn lint(
    db: &Connection,
    root: &Path,
    documents_root: &Path,
    scope: Option<&str>,
) -> Result<Vec<LintFinding>> {
    let mut findings = Vec::new();
    if !root.exists() {
        return Ok(findings);
    }
    let root = fsutil::canonical_root(root)?;
    let tree = fsutil::scan_tree(&root, PAGE_EXTENSIONS)?;
    let mut files = Vec::new();
    collect_files(&tree, &mut files);

    let db_pages = load_db_pages(db)?;
    let sources = sources::list_sources(db, None)?;
    let source_map: HashMap<&str, &WikiSource> = sources
        .iter()
        .map(|source| (source.rel_path.as_str(), source))
        .collect();
    let mut paths: HashSet<String> = db_pages.keys().cloned().collect();

    let mut parsed_pages = Vec::new();
    for entry in &files {
        let rel_path = sources::relative_path(&root, &entry.path)?;
        if rel_path == super::INDEX_PAGE {
            continue;
        }
        let raw = std::fs::read_to_string(&entry.path)?;
        let parsed = pages::parse_page(&rel_path, &raw);
        paths.insert(rel_path.clone());
        findings.extend(check_page_shape(&rel_path, &raw, &parsed));
        parsed_pages.push((rel_path, raw, parsed));
    }

    // Index drift: every file must have a row with the same hash, and every
    // row must still have a file.
    for (rel_path, _, parsed) in &parsed_pages {
        match db_pages.get(rel_path) {
            None => findings.push(LintFinding::new(
                LintCode::IndexDrift,
                rel_path,
                0,
                "page is not indexed yet",
            )),
            Some((hash, _)) if hash != &parsed.content_hash => findings.push(LintFinding::new(
                LintCode::IndexDrift,
                rel_path,
                0,
                "content hash differs from the index",
            )),
            Some(_) => {}
        }
    }
    for rel_path in db_pages.keys() {
        if rel_path == super::INDEX_PAGE {
            continue;
        }
        if !parsed_pages.iter().any(|(seen, _, _)| seen == rel_path) {
            findings.push(LintFinding::new(
                LintCode::IndexDrift,
                rel_path,
                0,
                "indexed page is missing from disk",
            ));
        }
    }
    check_generated_index(&root, &db_pages, &mut findings)?;

    let inbound = load_inbound(db)?;
    let duplicates = duplicate_labels(&parsed_pages);
    for (rel_path, raw, parsed) in &parsed_pages {
        // The body excludes frontmatter; findings must name real file lines.
        let offset = raw
            .lines()
            .count()
            .saturating_sub(parsed.body.lines().count()) as u32;
        findings.extend(check_provenance(
            db,
            documents_root,
            rel_path,
            parsed,
            &source_map,
            offset,
        )?);

        let kind = page_kind_for_path(rel_path);
        if matches!(
            kind,
            PageKind::Source | PageKind::Entity | PageKind::Concept
        ) && parsed.aliases().is_empty()
        {
            findings.push(LintFinding::new(
                LintCode::MissingAlias,
                rel_path,
                0,
                "compiled page has no aliases",
            ));
        }
        if body_is_empty(&parsed.body) {
            findings.push(LintFinding::new(
                LintCode::EmptyPage,
                rel_path,
                0,
                "body has fewer than 50 characters",
            ));
        }
        if kind != PageKind::Index && !inbound.contains(rel_path) {
            findings.push(LintFinding::new(
                LintCode::OrphanPage,
                rel_path,
                0,
                "page has no inbound links",
            ));
        }
        if !parsed.frontmatter.strings("contradicted_by").is_empty() {
            findings.push(LintFinding::new(
                LintCode::Contradiction,
                rel_path,
                0,
                "page records a contradiction",
            ));
        }
        if duplicates.contains(rel_path.as_str()) {
            findings.push(LintFinding::new(
                LintCode::DuplicatePage,
                rel_path,
                0,
                "title or alias collides with another page",
            ));
        }
        for link in links::resolve_links(db, rel_path, &parsed.body)? {
            if !paths.contains(&link.to) && link.to != super::INDEX_PAGE {
                findings.push(LintFinding::new(
                    LintCode::DeadLink,
                    rel_path,
                    0,
                    format!("link target does not exist: {}", link.to),
                ));
            }
        }
    }

    for source in &sources {
        if source.kind == SourceKind::Pdf && source.state == SourceState::Failed {
            findings.push(LintFinding::new(
                LintCode::PdfUnreadable,
                &source.rel_path,
                0,
                source
                    .error
                    .clone()
                    .unwrap_or_else(|| "PDF has no readable text layer".to_string()),
            ));
        }
    }

    if let Some(scope) = scope {
        let prefix = format!("{scope}/");
        findings.retain(|finding| finding.path == scope || finding.path.starts_with(&prefix));
    }
    findings.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.code.as_str().cmp(b.code.as_str()))
    });
    Ok(findings)
}

fn check_page_shape(rel_path: &str, raw: &str, parsed: &PageText) -> Vec<LintFinding> {
    let mut findings = Vec::new();
    let kind = page_kind_for_path(rel_path);
    let compiled = matches!(
        kind,
        PageKind::Source | PageKind::Entity | PageKind::Concept
    );

    if let Some(error) = &parsed.frontmatter_error {
        if has_frontmatter_block(raw) {
            findings.push(LintFinding::new(
                LintCode::BadFrontmatter,
                rel_path,
                1,
                format!("malformed frontmatter: {error}"),
            ));
        } else if compiled {
            findings.push(LintFinding::new(
                LintCode::UnmanagedPage,
                rel_path,
                1,
                "compiled page has no frontmatter",
            ));
        }
        return findings;
    }

    if !compiled {
        return findings;
    }
    let expected = match kind {
        PageKind::Source => "source",
        PageKind::Entity => "entity",
        PageKind::Concept => "concept",
        _ => return findings,
    };
    match parsed.frontmatter.scalar("type") {
        Some(value) if value.trim().eq_ignore_ascii_case(expected) => {}
        Some(value) => findings.push(LintFinding::new(
            LintCode::BadFrontmatter,
            rel_path,
            1,
            format!("type is \"{value}\", expected \"{expected}\""),
        )),
        None => findings.push(LintFinding::new(
            LintCode::BadFrontmatter,
            rel_path,
            1,
            "frontmatter is missing type",
        )),
    }
    if parsed
        .frontmatter
        .scalar("title")
        .map(|title| title.trim().is_empty())
        .unwrap_or(true)
    {
        findings.push(LintFinding::new(
            LintCode::BadFrontmatter,
            rel_path,
            1,
            "frontmatter is missing title",
        ));
    }
    findings
}

fn check_provenance(
    db: &Connection,
    documents_root: &Path,
    rel_path: &str,
    parsed: &PageText,
    source_map: &HashMap<&str, &WikiSource>,
    offset: u32,
) -> Result<Vec<LintFinding>> {
    let mut findings = Vec::new();
    let mut cited: BTreeSet<String> = BTreeSet::new();
    for source in pages::page_sources(parsed) {
        cited.insert(source.source_path);
    }

    if page_kind_for_path(rel_path) == PageKind::Source && cited.is_empty() {
        findings.push(LintFinding::new(
            LintCode::MissingSource,
            rel_path,
            0,
            "source page cites no document",
        ));
    }

    for source_path in &cited {
        if !source_map.contains_key(source_path.as_str()) {
            let code = if cited.len() == 1 {
                LintCode::OrphanedBySource
            } else {
                LintCode::MissingSource
            };
            findings.push(LintFinding::new(
                code,
                rel_path,
                0,
                format!("cited source does not exist: {source_path}"),
            ));
        }
    }

    for source in pages::page_sources(parsed) {
        if source.start_line > 0
            && let Some(entry) = source_map.get(source.source_path.as_str())
            && entry.kind == SourceKind::Markdown
        {
            let line_count = source_line_count(documents_root, &source.source_path);
            if let Some(line_count) = line_count
                && source.end_line as usize > line_count
            {
                findings.push(LintFinding::new(
                    LintCode::BadCitation,
                    rel_path,
                    0,
                    format!(
                        "citation range ends at line {} but {} has {} lines",
                        source.end_line, source.source_path, line_count
                    ),
                ));
            }
        }
    }

    let stale = stale_page(db, rel_path, source_map)?;
    if stale {
        findings.push(LintFinding::new(
            LintCode::StalePage,
            rel_path,
            0,
            "a cited source changed since this page was written",
        ));
    }

    findings.extend(check_footnotes(rel_path, &parsed.body, offset));
    Ok(findings)
}

fn stale_page(
    db: &Connection,
    rel_path: &str,
    source_map: &HashMap<&str, &WikiSource>,
) -> Result<bool> {
    let mut stmt =
        db.prepare("SELECT source_path, source_hash FROM wiki_page_sources WHERE page_path = ?1")?;
    let rows = stmt.query_map([rel_path], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (source_path, source_hash) = row?;
        if source_hash.is_empty() {
            continue;
        }
        if let Some(source) = source_map.get(source_path.as_str())
            && source.hash != source_hash
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn check_footnotes(rel_path: &str, body: &str, file_offset: u32) -> Vec<LintFinding> {
    let mut findings = Vec::new();
    let mut definitions: HashSet<String> = HashSet::new();
    let mut references: Vec<(usize, String)> = Vec::new();
    let mut fence: Option<&str> = None;

    for (index, line) in body.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(marker) = fence {
            if trimmed.starts_with(marker) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        let mut cursor = 0;
        while let Some(offset) = line[cursor..].find("[^") {
            let start = cursor + offset;
            let rest = &line[start + 2..];
            let Some(end) = rest.find(']') else {
                break;
            };
            let id = &rest[..end];
            let after = rest[end + 1..].trim_start();
            if after.starts_with(':') {
                definitions.insert(id.to_string());
                let target = after.trim_start_matches(':').trim();
                if target.is_empty()
                    || pages::normalize_source_path(target.split('#').next().unwrap_or(target))
                        .is_none()
                {
                    findings.push(LintFinding::new(
                        LintCode::BadCitation,
                        rel_path,
                        (index + 1) as u32 + file_offset,
                        format!("malformed citation for footnote {id}"),
                    ));
                }
            } else {
                references.push((index + 1, id.to_string()));
            }
            cursor = start + 2 + end + 1;
        }
    }

    for (line, id) in references {
        if !definitions.contains(&id) {
            findings.push(LintFinding::new(
                LintCode::BadCitation,
                rel_path,
                line as u32 + file_offset,
                format!("footnote {id} has no definition"),
            ));
        }
    }
    findings
}

fn check_generated_index(
    root: &Path,
    db_pages: &HashMap<String, (String, i64)>,
    findings: &mut Vec<LintFinding>,
) -> Result<()> {
    let path = root.join(super::INDEX_PAGE);
    if !path.exists() {
        if !db_pages.is_empty() {
            findings.push(LintFinding::new(
                LintCode::IndexDrift,
                super::INDEX_PAGE,
                0,
                "generated index.md is missing",
            ));
        }
        return Ok(());
    }
    let hash = sources::hash_file(&path)?;
    match db_pages.get(super::INDEX_PAGE) {
        Some((indexed, _)) if indexed != &hash => findings.push(LintFinding::new(
            LintCode::IndexDrift,
            super::INDEX_PAGE,
            0,
            "index.md differs from the indexed copy",
        )),
        None => findings.push(LintFinding::new(
            LintCode::IndexDrift,
            super::INDEX_PAGE,
            0,
            "index.md is not indexed",
        )),
        Some(_) => {}
    }
    Ok(())
}

fn duplicate_labels(pages: &[(String, String, PageText)]) -> HashSet<String> {
    let mut owners: HashMap<String, BTreeSet<String>> = HashMap::new();
    for (rel_path, _, page) in pages {
        let mut labels = vec![page.title()];
        labels.extend(page.aliases());
        for label in labels {
            let key = normalize_label(&label);
            if key.is_empty() {
                continue;
            }
            owners.entry(key).or_default().insert(rel_path.clone());
        }
    }
    let mut duplicates = HashSet::new();
    for (rel_path, _, page) in pages {
        let mut labels = vec![page.title()];
        labels.extend(page.aliases());
        for label in labels {
            let key = normalize_label(&label);
            if let Some(paths) = owners.get(&key)
                && paths.len() > 1
            {
                duplicates.insert(rel_path.clone());
            }
        }
    }
    duplicates
}

fn normalize_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn body_is_empty(body: &str) -> bool {
    body.chars().filter(|ch| !ch.is_whitespace()).count() < 50
}

fn has_frontmatter_block(raw: &str) -> bool {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let first = raw.lines().next().unwrap_or("");
    first.trim() == "---"
}

fn load_db_pages(db: &Connection) -> Result<HashMap<String, (String, i64)>> {
    let mut stmt = db.prepare("SELECT rel_path, content_hash, updated_at FROM wiki_pages")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            (row.get::<_, String>(1)?, row.get::<_, i64>(2)?),
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (path, value) = row?;
        out.insert(path, value);
    }
    Ok(out)
}

fn load_inbound(db: &Connection) -> Result<HashSet<String>> {
    let mut stmt = db.prepare("SELECT DISTINCT to_path FROM wiki_links")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = HashSet::new();
    for row in rows {
        out.insert(row?);
    }
    Ok(out)
}

fn source_line_count(documents_root: &Path, rel_path: &str) -> Option<usize> {
    let text = std::fs::read_to_string(documents_root.join(rel_path)).ok()?;
    Some(text.lines().count())
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
        std::fs::create_dir_all(root.join("concepts")).unwrap();
        std::fs::create_dir_all(root.join("sources")).unwrap();
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

    fn write(root: &Path, rel_path: &str, text: &str) {
        let path = root.join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn reindex(f: &mut Fixture) {
        crate::wiki::refresh_sources(&f.db, &f.documents).unwrap();
        crate::wiki::index_all_pages(&f.db, &f.root).unwrap();
    }

    fn clean_wiki(f: &mut Fixture) {
        write(
            &f.documents,
            "reading.md",
            "# Reading\n\nOne.\nTwo.\nThree.\n",
        );
        write(
            &f.root,
            "sources/reading.md",
            "---\ntype: source\ntitle: Reading\nsummary: A short reading.\n\
             aliases: [reading]\nsources: [reading.md]\n---\n\
             This source page summarises the reading in a few sentences.[^1]\n\n\
             [^1]: documents/reading.md#L1-L3\n\n[[concepts/attention]]\n",
        );
        write(
            &f.root,
            "concepts/attention.md",
            "---\ntype: concept\ntitle: Attention\nsummary: A shared idea.\n\
             aliases: [attention]\ntags: [ml]\nsources: [reading.md]\n---\n\
             Attention relates every token to every other token in the sequence.[^1]\n\n\
             [^1]: documents/reading.md#L1-L3\n\n[[sources/reading]]\n",
        );
    }

    fn codes(findings: &[LintFinding]) -> Vec<&'static str> {
        findings
            .iter()
            .map(|finding| finding.code.as_str())
            .collect()
    }

    #[test]
    fn a_clean_wiki_has_no_findings() {
        let mut f = fixture();
        clean_wiki(&mut f);
        reindex(&mut f);
        let findings = lint(&f.db, &f.root, &f.documents, None).unwrap();
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn shape_rules_cover_frontmatter_alias_orphans_and_empty_bodies() {
        let mut f = fixture();
        write(
            &f.root,
            "concepts/broken.md",
            "---\ntype: concept\n---\nshort\n",
        );
        write(&f.root, "concepts/plain.md", "# Plain\n\nBody only.\n");
        write(
            &f.root,
            "concepts/lonely.md",
            "---\ntype: concept\ntitle: Lonely\nsummary: A lonely page with no aliases.\n---\n\
             This page has a body long enough to clear the empty-page threshold.\n",
        );
        reindex(&mut f);
        let findings = lint(&f.db, &f.root, &f.documents, None).unwrap();
        let found = codes(&findings);
        assert!(found.contains(&"bad-frontmatter"), "{found:?}");
        assert!(found.contains(&"unmanaged-page"), "{found:?}");
        assert!(found.contains(&"missing-alias"), "{found:?}");
        assert!(found.contains(&"orphan-page"), "{found:?}");
        assert!(found.contains(&"empty-page"), "{found:?}");
    }

    #[test]
    fn links_and_duplicates_are_reported() {
        let mut f = fixture();
        write(
            &f.root,
            "concepts/one.md",
            "---\ntype: concept\ntitle: Same\nsummary: One.\naliases: [same]\n---\n\
             This page links to [[missing/page]] and has enough body characters to pass.\n",
        );
        write(
            &f.root,
            "concepts/two.md",
            "---\ntype: concept\ntitle: Same\nsummary: Two.\naliases: [same]\n---\n\
             This page repeats the same title and alias to collide with the first one.\n",
        );
        reindex(&mut f);
        let findings = lint(&f.db, &f.root, &f.documents, None).unwrap();
        let found = codes(&findings);
        assert!(found.contains(&"dead-link"), "{found:?}");
        assert!(found.contains(&"duplicate-page"), "{found:?}");
    }

    #[test]
    fn provenance_rules_cover_missing_stale_and_bad_citations() {
        let mut f = fixture();
        write(&f.documents, "short.md", "one\ntwo\nthree\n");
        write(
            &f.root,
            "concepts/attention.md",
            "---\ntype: concept\ntitle: Attention\nsummary: A page.\n\
             aliases: [attention]\nsources: [short.md, gone.md]\n---\n\
             A claim with an out-of-range citation.[^1] A missing footnote.[^9]\n\n\
             [^1]: documents/short.md#L2-L20\n",
        );
        reindex(&mut f);

        let findings = lint(&f.db, &f.root, &f.documents, None).unwrap();
        let found = codes(&findings);
        assert!(found.contains(&"missing-source"), "{found:?}");
        assert!(found.contains(&"bad-citation"), "{found:?}");

        // Change the source after the page was written, then refresh the
        // source index: the page now records an old hash.
        write(&f.documents, "short.md", "one\ntwo\nthree\nfour\n");
        crate::wiki::refresh_sources(&f.db, &f.documents).unwrap();
        let findings = lint(&f.db, &f.root, &f.documents, None).unwrap();
        assert!(codes(&findings).contains(&"stale-page"), "{findings:?}");
    }

    #[test]
    fn orphaned_by_source_and_contradictions_are_reported() {
        let mut f = fixture();
        write(&f.documents, "only.md", "one\ntwo\n");
        write(
            &f.root,
            "concepts/alone.md",
            "---\ntype: concept\ntitle: Alone\nsummary: Alone.\naliases: [alone]\n\
             sources: [only.md]\ncontradicted_by: [concepts/other.md]\n---\n\
             This page has enough characters to avoid the empty-body warning entirely.\n",
        );
        reindex(&mut f);
        std::fs::remove_file(f.documents.join("only.md")).unwrap();
        crate::wiki::refresh_sources(&f.db, &f.documents).unwrap();

        let findings = lint(&f.db, &f.root, &f.documents, None).unwrap();
        let found = codes(&findings);
        assert!(found.contains(&"orphaned-by-source"), "{found:?}");
        assert!(found.contains(&"contradiction"), "{found:?}");
    }

    #[test]
    fn index_drift_is_reported_before_a_page_is_indexed() {
        let mut f = fixture();
        write(
            &f.root,
            "concepts/unindexed.md",
            "---\ntype: concept\ntitle: Unindexed\nsummary: New.\naliases: [unindexed]\n---\n\
             This page was written after the last index pass and is not indexed yet.\n",
        );
        let findings = lint(&f.db, &f.root, &f.documents, None).unwrap();
        assert!(codes(&findings).contains(&"index-drift"), "{findings:?}");

        reindex(&mut f);
        let findings = lint(&f.db, &f.root, &f.documents, None).unwrap();
        assert!(
            !codes(&findings).contains(&"index-drift"),
            "reindex should clear drift: {findings:?}"
        );
    }

    #[test]
    fn failed_pdf_sources_are_reported() {
        let f = fixture();
        f.db.execute(
            "INSERT INTO wiki_sources (rel_path, kind, hash, size, mtime, state, error)
             VALUES ('scan.pdf', 'pdf', 'h', 1, 0, 'failed', 'no text layer')",
            [],
        )
        .unwrap();
        let findings = lint(&f.db, &f.root, &f.documents, None).unwrap();
        let pdf = findings
            .iter()
            .find(|finding| finding.code == LintCode::PdfUnreadable)
            .expect("pdf finding");
        assert_eq!(pdf.path, "scan.pdf");
    }

    #[test]
    fn scope_limits_findings_to_one_path() {
        let mut f = fixture();
        write(
            &f.root,
            "concepts/a.md",
            "---\ntype: concept\ntitle: A\nsummary: A.\n---\nshort\n",
        );
        write(
            &f.root,
            "concepts/b.md",
            "---\ntype: concept\ntitle: B\nsummary: B.\n---\nshort\n",
        );
        reindex(&mut f);
        let findings = lint(&f.db, &f.root, &f.documents, Some("concepts/a.md")).unwrap();
        assert!(
            findings
                .iter()
                .all(|finding| finding.path == "concepts/a.md")
        );
        assert!(!findings.is_empty());
    }
}
