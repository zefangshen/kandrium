//! Wikilinks: parsing, resolution, and backlinks.
//!
//! Pages link with `[[target]]`, `[[target|Display]]`, and
//! `[[target#Anchor]]`, where `target` is root-relative and carries no
//! extension. Resolution prefers an exact path, then a unique file stem, then
//! a title or alias. Dead links stay in `wiki_links` with their raw target so
//! lint can report them; self-links are dropped.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// One link from a page body to another page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WikiLink {
    pub from: String,
    pub to: String,
    pub anchor: String,
}

/// One inbound link to a page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backlink {
    pub from: String,
    pub anchor: String,
}

/// Extract wikilinks from a page body.
///
/// Fenced code, inline code, table rows, Markdown link labels, images, and
/// embeds (`![[...]]`) never produce a link.
pub fn parse_links(rel_path: &str, body: &str) -> Vec<WikiLink> {
    let mut out = Vec::new();
    let mut fence: Option<&str> = None;
    for line in body.lines() {
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
        if trimmed.starts_with('|') {
            continue;
        }

        let chars = mask_line(line);
        let mut index = 0;
        while index + 1 < chars.len() {
            let embedded = index > 0 && chars[index - 1] == '!';
            if chars[index] == '['
                && chars[index + 1] == '['
                && !embedded
                && let Some((link, end)) = read_wikilink(rel_path, &chars, index)
            {
                out.push(link);
                index = end;
                continue;
            }
            index += 1;
        }
    }
    out
}

/// Resolve a page's links against the indexed pages. Self-links are dropped;
/// unresolved targets are kept verbatim so lint can report them.
pub fn resolve_links(db: &Connection, rel_path: &str, body: &str) -> Result<Vec<WikiLink>> {
    let index = PageIndex::load(db)?;
    Ok(parse_links(rel_path, body)
        .into_iter()
        .filter_map(|link| index.resolve(rel_path, link))
        .collect())
}

/// Resolve and store one page's links, replacing its previous rows.
pub fn index_links(db: &Connection, rel_path: &str, body: &str) -> Result<usize> {
    let links = resolve_links(db, rel_path, body)?;
    db.execute("DELETE FROM wiki_links WHERE from_path = ?1", [rel_path])?;
    for link in &links {
        db.execute(
            "INSERT OR IGNORE INTO wiki_links (from_path, to_path, anchor)
             VALUES (?1, ?2, ?3)",
            params![link.from, link.to, link.anchor],
        )?;
    }
    Ok(links.len())
}

/// Re-resolve stored dead links that may now point at `rel_path`.
///
/// Other pages keep the raw target they were written with; when a page
/// appears (or is renamed) their rows are never revisited by the per-page
/// index. Returns how many rows changed.
pub fn resolve_pending_links(db: &Connection, rel_path: &str) -> Result<usize> {
    let bare = rel_path
        .strip_suffix(".md")
        .or_else(|| rel_path.strip_suffix(".markdown"))
        .unwrap_or(rel_path);
    let stem = stem_key(rel_path);
    let title: String = db
        .query_row(
            "SELECT title FROM wiki_pages WHERE rel_path = ?1",
            [rel_path],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_default();
    let title = title.trim().to_lowercase();
    let changed = db.execute(
        "UPDATE OR IGNORE wiki_links SET to_path = ?1
         WHERE from_path <> ?1
           AND lower(to_path) IN (lower(?2), lower(?3), lower(?4))",
        params![rel_path, bare, stem, title],
    )?;
    Ok(changed)
}

/// Rebuild the whole graph from page bodies. `wiki_pages` must already hold
/// the current titles and aliases so resolution can see every page.
pub fn rebuild_links(db: &Connection, bodies: &[(String, String)]) -> Result<usize> {
    let index = PageIndex::load(db)?;
    db.execute("DELETE FROM wiki_links", [])?;
    let mut count = 0;
    for (rel_path, body) in bodies {
        for link in parse_links(rel_path, body) {
            let Some(link) = index.resolve(rel_path, link) else {
                continue;
            };
            db.execute(
                "INSERT OR IGNORE INTO wiki_links (from_path, to_path, anchor)
                 VALUES (?1, ?2, ?3)",
                params![link.from, link.to, link.anchor],
            )?;
            count += 1;
        }
    }
    Ok(count)
}

/// Inbound links to a page, ordered by source path.
pub fn backlinks(db: &Connection, rel_path: &str) -> Result<Vec<Backlink>> {
    let mut stmt = db.prepare(
        "SELECT from_path, anchor FROM wiki_links
         WHERE to_path = ?1
         ORDER BY from_path, anchor",
    )?;
    let rows = stmt.query_map([rel_path], |row| {
        Ok(Backlink {
            from: row.get(0)?,
            anchor: row.get(1)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

struct PageIndex {
    /// Lowercased root-relative path (with extension) → path.
    paths: HashMap<String, String>,
    /// Lowercased file stem → paths.
    stems: HashMap<String, Vec<String>>,
    /// Lowercased title or alias → paths.
    names: HashMap<String, Vec<String>>,
}

impl PageIndex {
    fn load(db: &Connection) -> Result<Self> {
        let mut index = Self {
            paths: HashMap::new(),
            stems: HashMap::new(),
            names: HashMap::new(),
        };
        let mut stmt =
            db.prepare("SELECT rel_path, title, aliases_json FROM wiki_pages ORDER BY rel_path")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (rel_path, title, aliases_json) = row?;
            index
                .paths
                .insert(rel_path.to_lowercase(), rel_path.clone());
            index
                .stems
                .entry(stem_key(&rel_path))
                .or_default()
                .push(rel_path.clone());

            let mut names = vec![title];
            if let Ok(aliases) = serde_json::from_str::<Vec<String>>(&aliases_json) {
                names.extend(aliases);
            }
            for name in names {
                let name = name.trim().to_lowercase();
                if name.is_empty() {
                    continue;
                }
                let candidates = index.names.entry(name).or_default();
                if !candidates.contains(&rel_path) {
                    candidates.push(rel_path.clone());
                }
            }
        }
        Ok(index)
    }

    fn resolve(&self, from: &str, mut link: WikiLink) -> Option<WikiLink> {
        let target = normalize_target(&link.to)?;
        match self.lookup(&target) {
            Some(resolved) if resolved == from => None,
            Some(resolved) => {
                link.to = resolved;
                Some(link)
            }
            None => {
                // Keep the raw target: lint reports the dead link, and the
                // row does not point at a page that does not exist.
                link.to = target;
                Some(link)
            }
        }
    }

    fn lookup(&self, target: &str) -> Option<String> {
        let lower = target.to_lowercase();
        for extension in [".md", ".markdown"] {
            if let Some(path) = self.paths.get(&format!("{lower}{extension}")) {
                return Some(path.clone());
            }
        }
        if target.contains('/') {
            return None;
        }
        if let Some(candidates) = self.stems.get(&lower)
            && let Some(path) = candidates.first()
        {
            return Some(path.clone());
        }
        if let Some(candidates) = self.names.get(&lower)
            && let Some(path) = candidates.first()
        {
            return Some(path.clone());
        }
        None
    }
}

fn read_wikilink(from: &str, chars: &[char], start: usize) -> Option<(WikiLink, usize)> {
    let mut end = None;
    let mut index = start + 2;
    while index + 1 < chars.len() {
        if chars[index] == ']' && chars[index + 1] == ']' {
            end = Some(index);
            break;
        }
        if chars[index] == '[' {
            return None;
        }
        index += 1;
    }
    let end = end?;
    let inner: String = chars[start + 2..end].iter().collect();
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }

    let target = inner.split_once('|').map_or(inner, |(target, _)| target);
    let (target, anchor) = target
        .split_once('#')
        .map_or((target, ""), |(target, anchor)| (target, anchor));
    let target = target.trim();
    if target.is_empty() {
        return None;
    }

    Some((
        WikiLink {
            from: from.to_string(),
            to: target.to_string(),
            anchor: anchor.trim().to_string(),
        },
        end + 2,
    ))
}

fn normalize_target(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_matches(['<', '>']).trim();
    let raw = raw.strip_prefix('/').unwrap_or(raw);
    let raw = raw.strip_prefix("./").unwrap_or(raw);
    let raw = raw
        .strip_suffix(".markdown")
        .or_else(|| raw.strip_suffix(".md"))
        .unwrap_or(raw);
    if raw.is_empty() || raw.contains('\\') {
        return None;
    }
    let parts: Vec<&str> = raw.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return None;
    }
    Some(parts.join("/"))
}

fn stem_key(rel_path: &str) -> String {
    let file = rel_path.rsplit('/').next().unwrap_or(rel_path);
    file.rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file)
        .to_lowercase()
}

/// Blank out inline code, Markdown link labels, and images so their contents
/// cannot produce a wikilink. Offsets are preserved.
fn mask_line(line: &str) -> Vec<char> {
    let mut chars: Vec<char> = line.chars().collect();

    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '`' {
            index += 1;
            continue;
        }
        let start = index;
        let mut run = 1;
        while index + run < chars.len() && chars[index + run] == '`' {
            run += 1;
        }
        let content_start = index + run;
        let mut cursor = content_start;
        let mut close = None;
        while cursor < chars.len() {
            if chars[cursor] == '`' {
                let mut close_run = 1;
                while cursor + close_run < chars.len() && chars[cursor + close_run] == '`' {
                    close_run += 1;
                }
                if close_run == run {
                    close = Some(cursor + run);
                    break;
                }
                cursor += close_run;
            } else {
                cursor += 1;
            }
        }
        match close {
            Some(end) => {
                for ch in chars.iter_mut().take(end).skip(start) {
                    *ch = ' ';
                }
                index = end;
            }
            None => index = content_start,
        }
    }

    let mut index = 0;
    while index < chars.len() {
        let image = chars[index] == '!' && index + 1 < chars.len() && chars[index + 1] == '[';
        let label = chars[index] == '['
            && !(index + 1 < chars.len() && chars[index + 1] == '[')
            && !(index > 0 && chars[index - 1] == '!');
        if !image && !label {
            index += 1;
            continue;
        }
        let open = if image { index + 1 } else { index };
        let mut depth = 0usize;
        let mut close = None;
        for (cursor, ch) in chars.iter().enumerate().skip(open) {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        close = Some(cursor);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else {
            index = open + 1;
            continue;
        };
        let next = close + 1;
        let is_link = next < chars.len() && matches!(chars[next], '(' | '[' | ':');
        if is_link {
            for ch in chars.iter_mut().take(close + 1).skip(index) {
                *ch = ' ';
            }
            index = close + 1;
        } else {
            index = open + 1;
        }
    }

    chars
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let mut db = Connection::open_in_memory().unwrap();
        crate::db::migrations::run_schema(&mut db).unwrap();
        db
    }

    fn add_page(db: &Connection, rel_path: &str, title: &str, aliases: &[&str]) {
        db.execute(
            "INSERT INTO wiki_pages
                 (rel_path, kind, title, summary, aliases_json, tags_json, reviewed,
                  content_hash, updated_at)
             VALUES (?1, 'page', ?2, '', ?3, '[]', 0, '', 0)",
            params![rel_path, title, serde_json::to_string(aliases).unwrap()],
        )
        .unwrap();
    }

    #[test]
    fn parse_excludes_code_tables_links_and_embeds() {
        let body = "A [[concepts/attention|Attention]] link.\n\
                    Inline `[[inline/code]]` is ignored.\n\
                    | [[table/cell]] | cell |\n\
                    [see [[link/text]]](https://example.com)\n\
                    ![alt [[image/alt]]](image.png)\n\
                    ![[embeds/page]]\n\
                    ```\n[[fenced/code]]\n```\n\
                    [[entities/vaswani#Bio]]\n\
                    [[Plain Target]]\n";
        let links = parse_links("concepts/index.md", body);
        let targets: Vec<&str> = links.iter().map(|link| link.to.as_str()).collect();
        assert_eq!(
            targets,
            vec!["concepts/attention", "entities/vaswani", "Plain Target"]
        );
        assert_eq!(links[1].anchor, "Bio");
        assert_eq!(links[0].from, "concepts/index.md");
    }

    #[test]
    fn resolution_prefers_path_then_stem_then_alias_and_drops_self_links() {
        let db = db();
        add_page(
            &db,
            "concepts/self-attention.md",
            "Self-Attention",
            &["self attention"],
        );
        add_page(&db, "entities/vaswani.md", "Ashish Vaswani", &["vaswani"]);
        add_page(&db, "handwritten/note.md", "Note", &[]);

        let body = "[[concepts/self-attention]] [[vaswani]] [[Self Attention]] \
                    [[note]] [[handwritten/note]] [[missing/page]] [[concepts/self-attention#X]]";
        let links = resolve_links(&db, "concepts/self-attention.md", body).unwrap();
        let targets: Vec<&str> = links.iter().map(|link| link.to.as_str()).collect();
        assert_eq!(
            targets,
            vec![
                "entities/vaswani.md",
                "handwritten/note.md",
                "handwritten/note.md",
                "missing/page"
            ]
        );
    }

    #[test]
    fn resolution_is_case_insensitive_and_ignores_extensions() {
        let db = db();
        add_page(&db, "entities/vaswani.md", "Ashish Vaswani", &["Vaswani"]);

        let body = "[[ENTITIES/VASWANI]] [[vaswani.md]] [[VASWANI]]";
        let links = resolve_links(&db, "index.md", body).unwrap();
        assert_eq!(links.len(), 3);
        assert!(links.iter().all(|link| link.to == "entities/vaswani.md"));
    }

    #[test]
    fn parse_and_resolution_reject_traversal_and_empty_targets() {
        let db = db();
        let body = "[[../escape]] [[a/b/../c]] [[|only display]] [[ok/page]]";
        let links = resolve_links(&db, "a.md", body).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].to, "ok/page");
    }
}
