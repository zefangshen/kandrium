//! Wiki page parsing, path rules, and provenance.
//!
//! A page is a Markdown file under the wiki root. Compiled pages live in
//! `sources/`, `entities/`, or `concepts/`; hand-written pages may live
//! anywhere else under the root. Files stay authoritative, so this module
//! never writes anything: it parses frontmatter, derives a title, and turns
//! citations into [`PageSource`] rows for the derived index.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::error::{KandriumError, Result};
use crate::fsutil;

use super::sources::hash_bytes;
use super::{PageKind, PageSource, is_compiled_path};

/// File types indexed as wiki pages.
pub const PAGE_EXTENSIONS: &[&str] = &["md", "markdown"];

/// Frontmatter block delimiter.
const DELIMITER: &str = "---";

/// One frontmatter value: a scalar or an inline/block string array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FmValue {
    Scalar(String),
    List(Vec<String>),
}

/// One ordered frontmatter entry. Unknown keys keep their position and value
/// so a rewrite does not drop them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmField {
    pub key: String,
    pub value: FmValue,
}

/// Parsed frontmatter, kept as an ordered field list.
///
/// The typed keys are read through accessors (`title`, `sources`, …) while
/// unknown keys stay untouched, which makes read/render round-trips lossless.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    fields: Vec<FmField>,
}

impl Frontmatter {
    /// Ordered fields, including unknown keys.
    pub fn fields(&self) -> &[FmField] {
        &self.fields
    }

    pub fn get(&self, key: &str) -> Option<&FmValue> {
        self.fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| &field.value)
    }

    /// Scalar value for `key`; `None` when absent or stored as a list.
    pub fn scalar(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(FmValue::Scalar(value)) => Some(value),
            _ => None,
        }
    }

    /// String list for `key`. A scalar is treated as a one-item list so a
    /// hand-edited `sources: papers/a.pdf` keeps working.
    pub fn strings(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(FmValue::List(items)) => items.clone(),
            Some(FmValue::Scalar(value)) if !value.trim().is_empty() => {
                vec![value.trim().to_string()]
            }
            _ => Vec::new(),
        }
    }

    /// Boolean value for `key`; `None` when absent or not a recognized bool.
    pub fn boolean(&self, key: &str) -> Option<bool> {
        match self.scalar(key)?.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" => Some(false),
            _ => None,
        }
    }

    pub fn set_scalar(&mut self, key: &str, value: impl Into<String>) {
        self.set_field(key, FmValue::Scalar(value.into()));
    }

    pub fn set_list(&mut self, key: &str, values: Vec<String>) {
        self.set_field(key, FmValue::List(values));
    }

    pub fn remove(&mut self, key: &str) {
        self.fields.retain(|field| field.key != key);
    }

    fn set_field(&mut self, key: &str, value: FmValue) {
        if let Some(field) = self.fields.iter_mut().find(|field| field.key == key) {
            field.value = value;
        } else {
            self.fields.push(FmField {
                key: key.to_string(),
                value,
            });
        }
    }
}

/// One parsed page. `body` is the text after the frontmatter block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageText {
    pub rel_path: String,
    pub kind: PageKind,
    pub frontmatter: Frontmatter,
    pub body: String,
    pub content_hash: String,
    /// Set when the frontmatter block is malformed. The page is still
    /// indexed; lint reports `bad-frontmatter`.
    pub frontmatter_error: Option<String>,
}

impl PageText {
    /// Frontmatter title, else the first `# ` heading, else the file name.
    pub fn title(&self) -> String {
        if let Some(title) = self.frontmatter.scalar("title")
            && !title.trim().is_empty()
        {
            return title.trim().to_string();
        }
        if let Some(heading) = first_heading(&self.body) {
            return heading;
        }
        title_from_path(&self.rel_path)
    }

    pub fn summary(&self) -> String {
        self.frontmatter
            .scalar("summary")
            .map(|summary| summary.trim().to_string())
            .unwrap_or_default()
    }

    pub fn aliases(&self) -> Vec<String> {
        non_empty(self.frontmatter.strings("aliases"))
    }

    pub fn tags(&self) -> Vec<String> {
        non_empty(self.frontmatter.strings("tags"))
    }

    /// `None` when the page does not state `reviewed`, so callers can keep
    /// the stored flag instead of clearing it on every scan.
    pub fn reviewed(&self) -> Option<bool> {
        self.frontmatter.boolean("reviewed")
    }
}

/// One citation anchor recovered from a footnote definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub source_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub start_page: u32,
    pub end_page: u32,
}

/// Parse the frontmatter block and return it with the remaining body.
///
/// The subset is deliberately small: `key: scalar`, inline lists
/// (`[a, b]`), and block lists (`- a`). Unknown keys are kept verbatim.
pub fn parse_frontmatter(text: &str) -> Result<(Frontmatter, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let first_end = line_end(text, 0);
    if text[..first_end].trim_end_matches('\r').trim() != DELIMITER {
        return Err(KandriumError::new("page has no frontmatter block"));
    }

    let mut cursor = if first_end == text.len() {
        text.len()
    } else {
        first_end + 1
    };
    let mut fields: Vec<FmField> = Vec::new();
    let mut pending: Option<(String, Vec<String>)> = None;

    while cursor < text.len() {
        let end = line_end(text, cursor);
        let line = text[cursor..end].trim_end_matches('\r');
        cursor = if end == text.len() {
            text.len()
        } else {
            end + 1
        };

        if line.trim() == DELIMITER {
            flush_pending(&mut fields, &mut pending);
            return Ok((Frontmatter { fields }, &text[cursor..]));
        }
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(item) = line.trim_start().strip_prefix("- ") {
            match pending.as_mut() {
                Some((_, items)) => items.push(parse_scalar(item)?),
                None => {
                    return Err(KandriumError::new("list item without a key in frontmatter"));
                }
            }
            continue;
        }

        flush_pending(&mut fields, &mut pending);
        let (key, raw) = line
            .split_once(':')
            .ok_or_else(|| KandriumError::new(format!("malformed frontmatter line: {line}")))?;
        let key = key.trim();
        if key.is_empty() {
            return Err(KandriumError::new("empty frontmatter key"));
        }
        let raw = raw.trim();
        if raw.is_empty() {
            pending = Some((key.to_string(), Vec::new()));
        } else {
            upsert(&mut fields, key, parse_value(raw)?);
        }
    }

    Err(KandriumError::new("unterminated frontmatter block"))
}

/// Render a page from frontmatter and body. Field order is preserved, so an
/// unknown key written by the user survives a rewrite.
pub fn render_page(frontmatter: &Frontmatter, body: &str) -> String {
    let mut out = String::from("---\n");
    for field in &frontmatter.fields {
        out.push_str(&field.key);
        out.push(':');
        match &field.value {
            FmValue::Scalar(value) => {
                out.push(' ');
                out.push_str(&render_scalar(value));
            }
            FmValue::List(items) => {
                out.push_str(" [");
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&render_scalar(item));
                }
                out.push(']');
            }
        }
        out.push('\n');
    }
    out.push_str("---\n");
    out.push_str(body);
    out
}

/// Page kind derived from the root-relative path.
pub fn page_kind_for_path(rel_path: &str) -> PageKind {
    if rel_path == "index.md" || rel_path == "index.markdown" {
        return PageKind::Index;
    }
    match rel_path.split_once('/') {
        Some(("sources", rest)) if !rest.is_empty() => PageKind::Source,
        Some(("entities", rest)) if !rest.is_empty() => PageKind::Entity,
        Some(("concepts", rest)) if !rest.is_empty() => PageKind::Concept,
        _ => PageKind::Page,
    }
}

/// Validate a root-relative page path before it touches the filesystem.
pub fn validate_page_path(rel_path: &str) -> Result<()> {
    if rel_path.is_empty() || rel_path.starts_with('/') || rel_path.contains('\\') {
        return Err(KandriumError::new("invalid page path"));
    }
    for segment in rel_path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.starts_with('.') {
            return Err(KandriumError::new("invalid page path"));
        }
    }
    if !fsutil::has_extension(Path::new(rel_path), PAGE_EXTENSIONS) {
        return Err(KandriumError::new("pages must be Markdown files"));
    }
    Ok(())
}

/// Longest slug accepted for a compiled page path.
pub const SLUG_MAX: usize = 80;

/// Turn a title into a page slug: lowercase ASCII, runs of anything else
/// collapse to one hyphen, no leading or trailing hyphen. Non-ASCII input can
/// yield an empty slug; callers must fall back to a hand-picked name.
pub fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = slug.get(..SLUG_MAX.min(slug.len())).unwrap_or(slug);
    slug.trim_end_matches('-').to_string()
}

/// Check a slug: lowercase `a-z 0-9 -`, [`SLUG_MAX`] characters at most, no
/// leading or trailing hyphen.
pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() || slug.len() > SLUG_MAX {
        return Err(KandriumError::new("slug must be 1-80 characters"));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(KandriumError::new(
            "slug must not start or end with a hyphen",
        ));
    }
    if !slug
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(KandriumError::new("slug must be lowercase a-z, 0-9, or -"));
    }
    Ok(())
}

/// Validate the file stem of a compiled page (`sources/`, `entities/`,
/// `concepts/`). Hand-written pages are exempt.
pub fn validate_compiled_slug(rel_path: &str) -> Result<()> {
    if !is_compiled_path(rel_path) {
        return Ok(());
    }
    let stem = rel_path
        .rsplit('/')
        .next()
        .and_then(|name| Path::new(name).file_stem())
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    validate_slug(stem)
}

/// Parse a page without failing on malformed frontmatter: the raw text
/// becomes the body and `frontmatter_error` carries the reason.
pub fn parse_page(rel_path: &str, raw: &str) -> PageText {
    let (frontmatter, body, frontmatter_error) = match parse_frontmatter(raw) {
        Ok((frontmatter, body)) => (frontmatter, body.to_string(), None),
        Err(err) => (Frontmatter::default(), raw.to_string(), Some(err.0)),
    };
    PageText {
        rel_path: rel_path.to_string(),
        kind: page_kind_for_path(rel_path),
        frontmatter,
        body,
        content_hash: hash_bytes(raw.as_bytes()),
        frontmatter_error,
    }
}

/// Read and parse one page, guarded to the wiki root.
pub fn read_page(root: &Path, rel_path: &str) -> Result<PageText> {
    validate_page_path(rel_path)?;
    let path = fsutil::guard(root, &root.join(rel_path))?;
    let bytes = fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut page = parse_page(rel_path, &text);
    page.content_hash = hash_bytes(&bytes);
    Ok(page)
}

/// Combine frontmatter `sources` and footnote citations, normalized and
/// deduplicated. `source_hash` stays empty; the index fills it from
/// `wiki_sources`.
pub fn page_sources(page: &PageText) -> Vec<PageSource> {
    let mut rows: BTreeMap<(String, u32, u32, u32, u32), PageSource> = BTreeMap::new();
    for raw in page.frontmatter.strings("sources") {
        if let Some(source_path) = normalize_source_path(&raw) {
            rows.insert(
                (source_path.clone(), 0, 0, 0, 0),
                PageSource {
                    source_path,
                    source_hash: String::new(),
                    start_line: 0,
                    end_line: 0,
                    start_page: 0,
                    end_page: 0,
                },
            );
        }
    }
    for citation in parse_citations(&page.body) {
        rows.insert(
            (
                citation.source_path.clone(),
                citation.start_line,
                citation.end_line,
                citation.start_page,
                citation.end_page,
            ),
            PageSource {
                source_path: citation.source_path,
                source_hash: String::new(),
                start_line: citation.start_line,
                end_line: citation.end_line,
                start_page: citation.start_page,
                end_page: citation.end_page,
            },
        );
    }
    rows.into_values().collect()
}

/// Footnote definitions of the form
/// `[^1]: documents/papers/attention.pdf#page=3` or
/// `[^2]: readings/reading.md#L12-L18`. Definitions inside fenced code are
/// ignored.
pub fn parse_citations(body: &str) -> Vec<Citation> {
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
        let Some(rest) = trimmed.strip_prefix("[^") else {
            continue;
        };
        let Some((_, target)) = rest.split_once("]:") else {
            continue;
        };
        if let Some(citation) = citation_from_target(target) {
            out.push(citation);
        }
    }
    out
}

/// Normalize a document-relative citation path. A leading `documents/` is
/// stripped because selected sources are stored without it.
pub fn normalize_source_path(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_matches(['<', '>']).trim();
    let raw = raw.strip_prefix("documents/").unwrap_or(raw);
    if raw.is_empty() || raw.starts_with('/') || raw.contains('\\') {
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

fn citation_from_target(target: &str) -> Option<Citation> {
    let target = target.trim().trim_matches(['<', '>']).trim();
    let (path, fragment) = target.split_once('#').unwrap_or((target, ""));
    let source_path = normalize_source_path(path)?;
    let mut citation = Citation {
        source_path,
        start_line: 0,
        end_line: 0,
        start_page: 0,
        end_page: 0,
    };
    if let Some((start, end)) = parse_line_span(fragment) {
        citation.start_line = start;
        citation.end_line = end;
    } else if let Some((start, end)) = parse_page_span(fragment) {
        citation.start_page = start;
        citation.end_page = end;
    }
    Some(citation)
}

fn parse_line_span(fragment: &str) -> Option<(u32, u32)> {
    let rest = fragment.strip_prefix(['L', 'l'])?;
    let (start, end) = rest.split_once('-').unwrap_or((rest, rest));
    let start: u32 = start.trim().trim_start_matches(['L', 'l']).parse().ok()?;
    let end: u32 = end.trim().trim_start_matches(['L', 'l']).parse().ok()?;
    Some((start.min(end), start.max(end)))
}

fn parse_page_span(fragment: &str) -> Option<(u32, u32)> {
    let lower = fragment.to_ascii_lowercase();
    let rest = lower.strip_prefix("page=")?;
    let (start, end) = rest.split_once('-').unwrap_or((rest, rest));
    let start: u32 = start.trim().parse().ok()?;
    let end: u32 = end.trim().parse().ok()?;
    Some((start.min(end), start.max(end)))
}

fn first_heading(body: &str) -> Option<String> {
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
        if let Some(heading) = trimmed.strip_prefix("# ") {
            let heading = heading.trim().trim_end_matches('#').trim();
            if !heading.is_empty() {
                return Some(heading.to_string());
            }
        }
    }
    None
}

fn title_from_path(rel_path: &str) -> String {
    let file = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let stem = file.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(file);
    let title = stem.replace(['-', '_'], " ");
    let title = title.trim();
    if title.is_empty() {
        file.to_string()
    } else {
        title.to_string()
    }
}

fn non_empty(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn line_end(text: &str, start: usize) -> usize {
    text[start..]
        .find('\n')
        .map_or(text.len(), |offset| start + offset)
}

fn flush_pending(fields: &mut Vec<FmField>, pending: &mut Option<(String, Vec<String>)>) {
    if let Some((key, items)) = pending.take() {
        let value = if items.is_empty() {
            FmValue::Scalar(String::new())
        } else {
            FmValue::List(items)
        };
        upsert(fields, &key, value);
    }
}

fn upsert(fields: &mut Vec<FmField>, key: &str, value: FmValue) {
    if let Some(field) = fields.iter_mut().find(|field| field.key == key) {
        field.value = value;
    } else {
        fields.push(FmField {
            key: key.to_string(),
            value,
        });
    }
}

fn parse_value(raw: &str) -> Result<FmValue> {
    if raw.starts_with('[') {
        if !raw.ends_with(']') {
            return Err(KandriumError::new("malformed array in frontmatter"));
        }
        let inner = &raw[1..raw.len() - 1];
        let mut items = Vec::new();
        for part in split_list(inner) {
            items.push(parse_scalar(part)?);
        }
        return Ok(FmValue::List(items));
    }
    Ok(FmValue::Scalar(parse_scalar(raw)?))
}

fn split_list(inner: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    for (index, ch) in inner.char_indices() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == ',' => {
                parts.push(inner[start..index].trim());
                start = index + ch.len_utf8();
            }
            None => {}
        }
    }
    parts.push(inner[start..].trim());
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

fn parse_scalar(raw: &str) -> Result<String> {
    let raw = raw.trim();
    let first = raw.chars().next();
    let last = raw.chars().last();
    if raw.len() >= 2 && first == Some('"') && last == Some('"') {
        return Ok(unescape_double(&raw[1..raw.len() - 1]));
    }
    if raw.len() >= 2 && first == Some('\'') && last == Some('\'') {
        return Ok(raw[1..raw.len() - 1].replace("''", "'"));
    }
    if first == Some('"') || first == Some('\'') {
        return Err(KandriumError::new(
            "unterminated quoted value in frontmatter",
        ));
    }
    Ok(raw.to_string())
}

fn unescape_double(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn render_scalar(value: &str) -> String {
    if !needs_quote(value) {
        return value.to_string();
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

fn needs_quote(value: &str) -> bool {
    value.is_empty()
        || value.trim() != value
        || value.starts_with([
            '[', '{', '"', '\'', '-', '?', '#', '&', '*', '!', '|', '>', '@', '`',
        ])
        || value.ends_with(':')
        || value.contains([':', '#', ',', '\n', '\r', '\t'])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_round_trips_and_preserves_unknown_keys() {
        let text = "---\ntype: concept\ntitle: Self-Attention\nsummary: A one-liner.\n\
                    tags: [ml, transformers]\naliases: [\"self attention\", MHA]\n\
                    custom-key: keep me\nreviewed: true\n---\n\nBody text.\n";
        let (frontmatter, body) = parse_frontmatter(text).unwrap();
        assert_eq!(body, "\nBody text.\n");
        assert_eq!(frontmatter.scalar("title"), Some("Self-Attention"));
        assert_eq!(frontmatter.strings("tags"), vec!["ml", "transformers"]);
        assert_eq!(
            frontmatter.strings("aliases"),
            vec!["self attention", "MHA"]
        );
        assert_eq!(frontmatter.scalar("custom-key"), Some("keep me"));
        assert_eq!(frontmatter.boolean("reviewed"), Some(true));

        let rendered = render_page(&frontmatter, body);
        let (again, again_body) = parse_frontmatter(&rendered).unwrap();
        assert_eq!(again, frontmatter);
        assert_eq!(again_body, body);
        assert!(rendered.contains("custom-key: keep me"));
    }

    #[test]
    fn frontmatter_parses_block_lists_and_crlf() {
        let text = "---\r\ntitle: Block\r\ntags:\r\n  - one\r\n  - two\r\n---\r\nBody\r\n";
        let (frontmatter, body) = parse_frontmatter(text).unwrap();
        assert_eq!(frontmatter.strings("tags"), vec!["one", "two"]);
        assert_eq!(body, "Body\r\n");
    }

    #[test]
    fn frontmatter_rejects_malformed_blocks() {
        assert!(parse_frontmatter("no frontmatter").is_err());
        assert!(parse_frontmatter("---\ntitle: Open\n").is_err());
        assert!(parse_frontmatter("---\ntags: [a, b\n---\n").is_err());
        assert!(parse_frontmatter("---\ntitle: \"open\n---\n").is_err());
        assert!(parse_frontmatter("---\n- stray\n---\n").is_err());
    }

    #[test]
    fn page_paths_map_to_kinds_and_are_validated() {
        assert_eq!(page_kind_for_path("sources/attention.md"), PageKind::Source);
        assert_eq!(page_kind_for_path("entities/vaswani.md"), PageKind::Entity);
        assert_eq!(
            page_kind_for_path("concepts/attention.md"),
            PageKind::Concept
        );
        assert_eq!(page_kind_for_path("index.md"), PageKind::Index);
        assert_eq!(page_kind_for_path("handwritten/note.md"), PageKind::Page);
        assert_eq!(page_kind_for_path("sources/"), PageKind::Page);

        assert!(validate_page_path("concepts/attention.md").is_ok());
        assert!(validate_page_path("../escape.md").is_err());
        assert!(validate_page_path("/abs.md").is_err());
        assert!(validate_page_path("notes.txt").is_err());
        assert!(validate_page_path("a//b.md").is_err());
        assert!(validate_page_path(".hidden/b.md").is_err());
    }

    #[test]
    fn slugs_are_lowercase_kebab_and_validated() {
        assert_eq!(slugify("Self-Attention"), "self-attention");
        assert_eq!(
            slugify("  Attention Is All You Need!  "),
            "attention-is-all-you-need"
        );
        assert_eq!(slugify("C++ / Rust"), "c-rust");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("注意力"), "");

        assert!(validate_slug("self-attention").is_ok());
        assert!(validate_slug("gpt-4").is_ok());
        assert!(validate_slug("").is_err());
        assert!(validate_slug("-leading").is_err());
        assert!(validate_slug("trailing-").is_err());
        assert!(validate_slug("Upper").is_err());
        assert!(validate_slug("under_score").is_err());
        assert!(validate_slug(&"a".repeat(SLUG_MAX + 1)).is_err());

        assert!(validate_compiled_slug("concepts/self-attention.md").is_ok());
        assert!(validate_compiled_slug("concepts/Self_Attention.md").is_err());
        assert!(validate_compiled_slug("Hand Written.md").is_ok());
    }

    #[test]
    fn pages_without_frontmatter_fall_back_to_heading_and_file_name() {
        let page = parse_page("handwritten/note.md", "# My Notes\n\nBody.\n");
        assert_eq!(page.kind, PageKind::Page);
        assert_eq!(page.title(), "My Notes");
        assert!(page.frontmatter_error.is_some());

        let page = parse_page("handwritten/plain-note.md", "Body only.\n");
        assert_eq!(page.title(), "plain note");
    }

    #[test]
    fn citations_normalize_prefix_and_ranges() {
        let body = "Claim.[^1]\n\n[^1]: documents/papers/attention.pdf#page=3\n\
                    [^2]: readings/reading.md#L12-L18\n\
                    [^3]: <papers/whole.pdf>\n\
                    ```\n[^4]: ignored.md#L1\n```\n";
        let citations = parse_citations(body);
        assert_eq!(citations.len(), 3);
        assert_eq!(citations[0].source_path, "papers/attention.pdf");
        assert_eq!((citations[0].start_page, citations[0].end_page), (3, 3));
        assert_eq!(citations[1].source_path, "readings/reading.md");
        assert_eq!((citations[1].start_line, citations[1].end_line), (12, 18));
        assert_eq!(citations[2].source_path, "papers/whole.pdf");
        assert_eq!(citations[2].start_page, 0);

        let normalized = normalize_source_path("documents/a/b.md");
        assert_eq!(normalized.as_deref(), Some("a/b.md"));
        assert!(normalize_source_path("../outside.md").is_none());
        assert!(normalize_source_path("/abs.md").is_none());
    }

    #[test]
    fn page_sources_merge_frontmatter_and_footnotes() {
        let text = "---\ntype: source\ntitle: Source\ntitle: x\nsources: [papers/a.pdf]\n---\n\
                    A claim.[^1]\n\n[^1]: documents/papers/a.pdf#page=2\n";
        let page = parse_page("sources/a.md", text);
        let sources = page_sources(&page);
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].source_path, "papers/a.pdf");
        assert_eq!(sources[0].start_page, 0);
        assert_eq!(sources[1].start_page, 2);
    }

    #[test]
    fn frontmatter_accessors_set_and_remove_fields() {
        let mut frontmatter = Frontmatter::default();
        frontmatter.set_scalar("title", "  Quoted: title  ");
        frontmatter.set_list("aliases", vec!["one".into(), "two".into()]);
        frontmatter.set_scalar("title", "Replaced");
        assert_eq!(frontmatter.scalar("title"), Some("Replaced"));

        let rendered = render_page(&frontmatter, "Body\n");
        let (parsed, body) = parse_frontmatter(&rendered).unwrap();
        assert_eq!(body, "Body\n");
        assert_eq!(parsed.scalar("title"), Some("Replaced"));
        assert_eq!(parsed.strings("aliases"), vec!["one", "two"]);

        frontmatter.remove("aliases");
        assert!(frontmatter.get("aliases").is_none());
    }
}
