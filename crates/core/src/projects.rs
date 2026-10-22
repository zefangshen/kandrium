//! Projects data layer — DB-backed folders and projects, each owning its own
//! Kanban board and an on-disk document folder.
//!
//! Unlike notes/documents (pure filesystem), the project tree lives in SQLite
//! (`project_folders` + `projects`, schema v2), so the app and the MCP server
//! share a single source of truth. Every project owns a board whose columns
//! are scaffolded from a template on creation; the folders themselves are
//! database-only (project documents live on disk at `<root>/<project_id>/`).
//!
//! All functions are synchronous and take `&rusqlite::Connection`, following
//! the rest of `kandrium-core`. No MCP tool exposes them yet, but the module
//! is exported from `lib.rs` so a future tool is a one-liner.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::constants::status_from_column_title;
use crate::db::uid;
use crate::error::{KandriumError, Result};
use crate::fsutil::{self, FsEntry};
use crate::models::CardStatus;
use crate::services::{board, column};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single project row.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub folder_id: Option<String>,
    pub name: String,
    /// Short unique key shown in the tree and on cards (1–9 chars).
    pub identifier: String,
    pub description: String,
    pub status: String,
    pub board_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Minimal project info for pickers (card modal "Project" menu).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectChoice {
    pub id: String,
    pub name: String,
    pub identifier: String,
}

/// A folder in the project tree (database-only; not a filesystem directory).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFolder {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
}

/// One node of the assembled project tree.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ProjectNode {
    Folder {
        id: String,
        name: String,
        children: Vec<ProjectNode>,
    },
    Project(Project),
}

/// Checklist + document counts shown in the project overview header.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub todo: i64,
    pub in_progress: i64,
    pub done: i64,
    pub total: i64,
    pub file_count: usize,
    pub updated_at: i64,
}

/// A new-project scaffold: board column titles and the files written into the
/// project's on-disk folder.
pub struct ProjectTemplate {
    pub id: &'static str,
    pub label: &'static str,
    pub columns: &'static [&'static str],
    pub files: &'static [(&'static str, &'static str)],
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Project lifecycle statuses: `(key, label)`.
pub const PROJECT_STATUSES: [(&str, &str); 3] = [
    ("active", "Active"),
    ("on_hold", "On Hold"),
    ("done", "Done"),
];

/// File extensions listed in a project's documents panel. An allowlist so
/// stray/system files never leak into the list.
pub const PROJECT_FILE_EXTS: &[&str] = &[
    "md", "markdown", "txt", "pdf", "png", "jpg", "jpeg", "gif", "webp", "csv", "json", "docx",
    "xlsx", "pptx",
];

/// Column titles match the canonical buckets exactly so
/// [`status_from_column_title`] maps them and the project's cards show up in
/// the summary and (if ever aggregated) the All board.
const DEFAULT_COLUMNS: &[&str] = &["To Do", "In Progress", "Done"];

/// Built-in project templates.
pub const TEMPLATES: [ProjectTemplate; 3] = [
    ProjectTemplate {
        id: "blank",
        label: "Blank",
        columns: DEFAULT_COLUMNS,
        files: &[],
    },
    ProjectTemplate {
        id: "software",
        label: "Software",
        columns: DEFAULT_COLUMNS,
        files: &[
            (
                "README.md",
                "# README\n\nProject overview and setup notes.\n",
            ),
            ("notes.md", "# Notes\n\n"),
        ],
    },
    ProjectTemplate {
        id: "research",
        label: "Research",
        columns: DEFAULT_COLUMNS,
        files: &[
            ("README.md", "# README\n\nResearch question and scope.\n"),
            ("sources.md", "# Sources\n\n"),
        ],
    },
];

/// Look up a template by id.
pub fn template(id: &str) -> Option<&'static ProjectTemplate> {
    TEMPLATES.iter().find(|t| t.id == id)
}

const PROJECT_COLUMNS: &str =
    "id, folder_id, name, identifier, description, status, board_id, created_at, updated_at";

// ---------------------------------------------------------------------------
// Root resolution
// ---------------------------------------------------------------------------

/// Resolve the projects root:
/// 1. `KANDRIUM_PROJECTS` env var (explicit override)
/// 2. `<data_dir>/kandrium/projects`
pub fn resolve_projects_root() -> PathBuf {
    if let Some(p) = std::env::var_os("KANDRIUM_PROJECTS") {
        return PathBuf::from(p);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kandrium")
        .join("projects")
}

/// Create the root directory if missing and return it.
pub fn ensure_root() -> Result<PathBuf> {
    let root = resolve_projects_root();
    fs::create_dir_all(&root)?;
    Ok(root)
}

/// On-disk directory for one project. Keyed by id, so renaming a project
/// never moves its files.
pub fn project_dir(root: &Path, id: &str) -> PathBuf {
    root.join(id)
}

// ---------------------------------------------------------------------------
// Tree
// ---------------------------------------------------------------------------

/// Load the whole tree: one query for all folders, one for all projects,
/// grouped in Rust by `parent_id`. Folders precede projects within each
/// parent; both tiers are ordered by `position` then case-insensitive name.
///
/// Assembly walks down from the root, so a malformed parent cycle can only
/// drop rows — it can never loop.
pub fn list_tree(db: &Connection) -> Result<Vec<ProjectNode>> {
    let mut folder_stmt = db.prepare(
        "SELECT id, parent_id, name FROM project_folders ORDER BY position, name COLLATE NOCASE",
    )?;
    let folders = folder_stmt
        .query_map([], |r| {
            Ok(ProjectFolder {
                id: r.get(0)?,
                parent_id: r.get(1)?,
                name: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut project_stmt = db.prepare(&format!(
        "SELECT {PROJECT_COLUMNS} FROM projects ORDER BY position, name COLLATE NOCASE"
    ))?;
    let projects = project_stmt
        .query_map([], map_project)?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(assemble(folders, projects))
}

/// Find a project anywhere in an assembled tree.
pub fn find_project<'a>(nodes: &'a [ProjectNode], id: &str) -> Option<&'a Project> {
    for node in nodes {
        match node {
            ProjectNode::Folder { children, .. } => {
                if let Some(p) = find_project(children, id) {
                    return Some(p);
                }
            }
            ProjectNode::Project(p) if p.id == id => return Some(p),
            ProjectNode::Project(_) => {}
        }
    }
    None
}

fn assemble(folders: Vec<ProjectFolder>, projects: Vec<Project>) -> Vec<ProjectNode> {
    let mut folder_children: HashMap<Option<String>, Vec<ProjectFolder>> = HashMap::new();
    for f in folders {
        folder_children
            .entry(f.parent_id.clone())
            .or_default()
            .push(f);
    }
    let mut project_children: HashMap<Option<String>, Vec<Project>> = HashMap::new();
    for p in projects {
        project_children
            .entry(p.folder_id.clone())
            .or_default()
            .push(p);
    }
    let mut visited = std::collections::HashSet::new();
    build(None, &folder_children, &project_children, &mut visited)
}

fn build(
    parent: Option<&str>,
    folders: &HashMap<Option<String>, Vec<ProjectFolder>>,
    projects: &HashMap<Option<String>, Vec<Project>>,
    visited: &mut std::collections::HashSet<String>,
) -> Vec<ProjectNode> {
    let key = parent.map(str::to_string);
    let mut out = Vec::new();
    if let Some(fs) = folders.get(&key) {
        for f in fs {
            if !visited.insert(f.id.clone()) {
                continue; // cycle guard: never revisit a folder
            }
            let children = build(Some(&f.id), folders, projects, visited);
            out.push(ProjectNode::Folder {
                id: f.id.clone(),
                name: f.name.clone(),
                children,
            });
        }
    }
    if let Some(ps) = projects.get(&key) {
        for p in ps {
            out.push(ProjectNode::Project(p.clone()));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Project CRUD
// ---------------------------------------------------------------------------

/// Fetch one project by id.
pub fn get_project(db: &Connection, id: &str) -> Result<Project> {
    db.query_row(
        &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?"),
        [id],
        map_project,
    )
    .optional()?
    .ok_or_else(|| KandriumError::new("Project not found"))
}

/// Create a project: scaffolds its board (template columns) and inserts the
/// row in one transaction, then writes the template files to disk.
pub fn create_project(
    db: &Connection,
    root: &Path,
    name: &str,
    folder_id: Option<&str>,
    template_id: &str,
) -> Result<Project> {
    let name = require_name(name)?;
    let tpl = template(template_id)
        .ok_or_else(|| KandriumError::new(format!("Unknown project template \"{template_id}\"")))?;
    let now = now_unix();
    let id = uid();
    // Auto-derived; renames regenerate it until the user pins a value on the
    // Overview page (`identifier_locked`).
    let identifier = suggest_identifier(db, &name, None)?;

    let tx = db.unchecked_transaction()?;
    let board = board::create_board(&tx, &name)?;
    for title in tpl.columns {
        column::create_column(&tx, &board.id, title)?;
    }
    tx.execute(
        "INSERT INTO projects \
             (id, folder_id, name, identifier, identifier_locked, description, status, \
              board_id, position, created_at, updated_at) \
         SELECT ?, ?, ?, ?, 0, '', 'active', ?, COALESCE(MAX(position), -1) + 1, ?, ? \
         FROM projects WHERE folder_id IS ?",
        rusqlite::params![
            id, folder_id, name, identifier, board.id, now, now, folder_id
        ],
    )?;
    tx.commit()?;

    // After the rows are durable, scaffold the on-disk folder. An existing
    // file is left alone so re-running a create never clobbers user content.
    let dir = project_dir(root, &id);
    fs::create_dir_all(&dir)?;
    for (file_name, content) in tpl.files {
        let path = dir.join(file_name);
        if !path.exists() {
            fs::write(&path, content)?;
        }
    }

    get_project(db, &id)
}

/// Rename a project; the backing board title follows so they never diverge.
pub fn rename_project(db: &Connection, id: &str, name: &str) -> Result<Project> {
    let name = require_name(name)?;
    let now = now_unix();
    let tx = db.unchecked_transaction()?;
    let (board_id, old_name, locked): (String, String, i64) = tx
        .query_row(
            "SELECT board_id, name, identifier_locked FROM projects WHERE id = ?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| KandriumError::new("Project not found"))?;
    // An auto-derived identifier follows the name until the user edits it.
    let identifier = if locked == 0 && old_name != name {
        Some(suggest_identifier(&tx, &name, Some(id))?)
    } else {
        None
    };
    tx.execute(
        "UPDATE projects SET name = ?, updated_at = ? WHERE id = ?",
        rusqlite::params![name, now, id],
    )?;
    if let Some(identifier) = identifier {
        tx.execute(
            "UPDATE projects SET identifier = ? WHERE id = ?",
            rusqlite::params![identifier, id],
        )?;
    }
    tx.execute(
        "UPDATE boards SET title = ? WHERE id = ?",
        rusqlite::params![name, board_id],
    )?;
    tx.commit()?;
    get_project(db, id)
}

/// Update a project's description and/or status. Validates `status` against
/// [`PROJECT_STATUSES`]; unknown statuses are rejected before any write.
pub fn update_project(
    db: &Connection,
    id: &str,
    description: Option<&str>,
    status: Option<&str>,
) -> Result<Project> {
    if let Some(status) = status
        && !PROJECT_STATUSES.iter().any(|(key, _)| *key == status)
    {
        return Err(KandriumError::new(format!(
            "Unknown project status \"{status}\""
        )));
    }
    let now = now_unix();
    if let Some(description) = description {
        db.execute(
            "UPDATE projects SET description = ?, updated_at = ? WHERE id = ?",
            rusqlite::params![description, now, id],
        )?;
    }
    if let Some(status) = status {
        db.execute(
            "UPDATE projects SET status = ?, updated_at = ? WHERE id = ?",
            rusqlite::params![status, now, id],
        )?;
    }
    get_project(db, id)
}

/// Pin a project's identifier. Lowercases and validates the value, then flips
/// `identifier_locked` so later renames leave it alone.
pub fn set_identifier(db: &Connection, id: &str, identifier: &str) -> Result<Project> {
    let identifier = validate_identifier(identifier)?;
    let now = now_unix();
    let changed = db
        .execute(
            "UPDATE projects SET identifier = ?, identifier_locked = 1, updated_at = ? \
             WHERE id = ?",
            rusqlite::params![identifier, now, id],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(err, _)
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                KandriumError::new(format!("Identifier \"{identifier}\" is already in use"))
            }
            other => other.into(),
        })?;
    if changed == 0 {
        return Err(KandriumError::new("Project not found"));
    }
    get_project(db, id)
}

/// Flat project list for the card modal's project picker.
pub fn list_project_choices(db: &Connection) -> Result<Vec<ProjectChoice>> {
    let mut stmt = db.prepare(
        "SELECT id, name, identifier FROM projects ORDER BY position, name COLLATE NOCASE",
    )?;
    let choices = stmt
        .query_map([], |r| {
            Ok(ProjectChoice {
                id: r.get(0)?,
                name: r.get(1)?,
                identifier: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(choices)
}

/// Suggest a unique identifier derived from a project name: initials for
/// multi-word names (up to 4), the first three characters for single words,
/// `prj` for names with no ASCII alphanumerics. Collisions get a numeric
/// suffix (`web`, `web2`, …), always trimmed to 10 characters.
pub fn suggest_identifier(db: &Connection, name: &str, exclude_id: Option<&str>) -> Result<String> {
    let base = derive_identifier(name);
    if !identifier_taken(db, &base, exclude_id)? {
        return Ok(base);
    }
    for n in 2..=9999u32 {
        let suffix = n.to_string();
        let keep = 10usize.saturating_sub(suffix.len());
        let mut candidate: String = base.chars().take(keep).collect();
        candidate.push_str(&suffix);
        if !identifier_taken(db, &candidate, exclude_id)? {
            return Ok(candidate);
        }
    }
    Err(KandriumError::new(
        "Cannot generate a unique project identifier",
    ))
}

fn derive_identifier(name: &str) -> String {
    let words: Vec<String> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let base = match words.as_slice() {
        [] => "prj".to_string(),
        [word] => truncate_chars(word, 3),
        many => {
            let initials: String = many
                .iter()
                .filter_map(|w| w.chars().next())
                .take(4)
                .collect();
            if initials.chars().count() >= 2 {
                initials
            } else {
                truncate_chars(&many[0], 3)
            }
        }
    };
    truncate_chars(&base, 10)
}

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Lowercase and validate a user-supplied identifier (1–10 chars, a–z, 0–9,
/// `-`, `_`).
pub fn validate_identifier(raw: &str) -> Result<String> {
    let ident = raw.trim().to_ascii_lowercase();
    if ident.is_empty() {
        return Err(KandriumError::new("Identifier required"));
    }
    if ident.chars().count() > 10 {
        return Err(KandriumError::new(
            "Identifier must be 10 characters or fewer",
        ));
    }
    if !ident
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(KandriumError::new("Use lowercase letters, digits, - or _"));
    }
    Ok(ident)
}

fn identifier_taken(db: &Connection, identifier: &str, exclude_id: Option<&str>) -> Result<bool> {
    let found: i64 = db.query_row(
        "SELECT COUNT(*) FROM projects \
         WHERE identifier COLLATE NOCASE = ?1 AND (?2 IS NULL OR id <> ?2)",
        rusqlite::params![identifier, exclude_id],
        |r| r.get(0),
    )?;
    Ok(found > 0)
}

/// Delete a project: its row and its board (cascading columns → cards →
/// all_card_order), then its on-disk folder.
pub fn delete_project(db: &Connection, root: &Path, id: &str) -> Result<()> {
    let board_id: String = db
        .query_row("SELECT board_id FROM projects WHERE id = ?", [id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| KandriumError::new("Project not found"))?;
    let tx = db.unchecked_transaction()?;
    tx.execute("DELETE FROM projects WHERE id = ?", [id])?;
    tx.execute("DELETE FROM boards WHERE id = ?", [board_id])?;
    tx.commit()?;
    remove_dir_ignore_missing(&project_dir(root, id))
}

// ---------------------------------------------------------------------------
// Folder CRUD
// ---------------------------------------------------------------------------

/// Create a folder under `parent_id` (`None` = root). Errors if the name is
/// empty or contains a path separator.
pub fn create_folder(
    db: &Connection,
    name: &str,
    parent_id: Option<&str>,
) -> Result<ProjectFolder> {
    let name = require_name(name)?;
    let id = uid();
    // Single INSERT...SELECT: the next position is computed inside the same
    // statement (NULL-safe via `IS ?`) so concurrent writers can't race.
    db.execute(
        "INSERT INTO project_folders (id, name, parent_id, position) \
         SELECT ?, ?, ?, COALESCE(MAX(position), -1) + 1 \
         FROM project_folders WHERE parent_id IS ?",
        rusqlite::params![id, name, parent_id, parent_id],
    )?;
    Ok(ProjectFolder {
        id,
        parent_id: parent_id.map(str::to_string),
        name,
    })
}

/// Rename a folder.
pub fn rename_folder(db: &Connection, id: &str, name: &str) -> Result<ProjectFolder> {
    let name = require_name(name)?;
    let changed = db.execute(
        "UPDATE project_folders SET name = ? WHERE id = ?",
        rusqlite::params![name, id],
    )?;
    if changed == 0 {
        return Err(KandriumError::new("Folder not found"));
    }
    db.query_row(
        "SELECT id, parent_id, name FROM project_folders WHERE id = ?",
        [id],
        |r| {
            Ok(ProjectFolder {
                id: r.get(0)?,
                parent_id: r.get(1)?,
                name: r.get(2)?,
            })
        },
    )
    .map_err(Into::into)
}

/// Delete a folder and everything under it: descendant folders and their
/// projects (plus each project's board), then every project's on-disk folder.
pub fn delete_folder(db: &Connection, root: &Path, id: &str) -> Result<()> {
    let folder_ids = descendant_folder_ids(db, id)?;
    if folder_ids.is_empty() {
        return Err(KandriumError::new("Folder not found"));
    }

    // Projects directly inside any affected folder: `(id, board_id)`.
    let mut projects: Vec<(String, String)> = Vec::new();
    {
        let placeholders = vec!["?"; folder_ids.len()].join(", ");
        let sql = format!("SELECT id, board_id FROM projects WHERE folder_id IN ({placeholders})");
        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(folder_ids.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            projects.push(row?);
        }
    }

    let tx = db.unchecked_transaction()?;
    for (project_id, _) in &projects {
        tx.execute("DELETE FROM projects WHERE id = ?", [project_id])?;
    }
    for (_, board_id) in &projects {
        tx.execute("DELETE FROM boards WHERE id = ?", [board_id])?;
    }
    // Cascades to child folders and any project rows still referencing them.
    tx.execute("DELETE FROM project_folders WHERE id = ?", [id])?;
    tx.commit()?;

    for (project_id, _) in &projects {
        remove_dir_ignore_missing(&project_dir(root, project_id))?;
    }
    Ok(())
}

fn descendant_folder_ids(db: &Connection, id: &str) -> Result<Vec<String>> {
    let mut stmt = db.prepare(
        r#"WITH RECURSIVE sub(id) AS (
               SELECT id FROM project_folders WHERE id = ?
               UNION ALL
               SELECT f.id FROM project_folders f JOIN sub ON f.parent_id = sub.id
           )
           SELECT id FROM sub"#,
    )?;
    let ids = stmt
        .query_map([id], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ids)
}

// ---------------------------------------------------------------------------
// Files + summary
// ---------------------------------------------------------------------------

/// Scan a project's on-disk folder into a sorted tree. A missing folder
/// yields an empty list.
pub fn project_files(root: &Path, id: &str) -> Result<Vec<FsEntry>> {
    let dir = project_dir(root, id);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    fsutil::scan_tree(&dir, PROJECT_FILE_EXTS)
}

/// Read one text document inside a project folder. `rel` is joined onto
/// `<root>/<project_id>/` and must stay inside it (see `fsutil::guard`).
pub fn read_doc(root: &Path, id: &str, rel: &Path) -> Result<String> {
    let dir = project_dir(root, id);
    let path = fsutil::guard(&dir, &dir.join(rel))?;
    Ok(fs::read_to_string(path)?)
}

/// Write one text document inside a project folder, with the same path guard
/// as [`read_doc`].
pub fn write_doc(root: &Path, id: &str, rel: &Path, text: &str) -> Result<()> {
    let dir = project_dir(root, id);
    let path = fsutil::guard(&dir, &dir.join(rel))?;
    fs::write(path, text)?;
    Ok(())
}

/// Create an empty markdown/text document inside `parent_rel` (an empty path
/// means the project root). Returns the absolute path of the new file.
///
/// `name` must be a bare file name with a `.md`, `.markdown` or `.txt`
/// extension; separators, hidden names and existing paths are rejected.
pub fn create_doc_file(root: &Path, id: &str, parent_rel: &Path, name: &str) -> Result<PathBuf> {
    let dir = project_dir(root, id);
    let parent = project_parent(&dir, parent_rel)?;
    let name = validate_doc_name(name)?;
    if !fsutil::has_extension(Path::new(name), &["md", "markdown", "txt"]) {
        return Err(KandriumError::new("Use a .md, .markdown or .txt file"));
    }
    let target = fsutil::guard(&dir, &parent.join(name))?;
    if target.exists() {
        return Err(KandriumError::new(format!("\"{name}\" already exists")));
    }
    fs::write(&target, "")?;
    Ok(target)
}

/// Create a folder inside `parent_rel` (an empty path means the project root).
/// Returns the absolute path of the new folder.
pub fn create_doc_folder(root: &Path, id: &str, parent_rel: &Path, name: &str) -> Result<PathBuf> {
    let dir = project_dir(root, id);
    let parent = project_parent(&dir, parent_rel)?;
    let name = validate_doc_name(name)?;
    let target = fsutil::guard(&dir, &parent.join(name))?;
    if target.exists() {
        return Err(KandriumError::new(format!("\"{name}\" already exists")));
    }
    fs::create_dir(&target)?;
    Ok(target)
}

/// Resolve `parent_rel` against the project folder, requiring the result to be
/// an existing directory inside it.
fn project_parent(dir: &Path, parent_rel: &Path) -> Result<PathBuf> {
    let parent = fsutil::guard(dir, &dir.join(parent_rel))?;
    if !parent.is_dir() {
        return Err(KandriumError::new("Parent folder not found"));
    }
    Ok(parent)
}

/// Validate a new document/folder name: bare, non-empty, no separators or
/// hidden dot prefix. Returns the trimmed name.
fn validate_doc_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.starts_with('.')
        || name.contains(['/', '\\'])
    {
        return Err(KandriumError::new("Invalid name"));
    }
    Ok(name)
}

/// Task counts per canonical bucket, total task count, recursive file count
/// and last-updated timestamp for the overview header.
pub fn project_summary(db: &Connection, root: &Path, id: &str) -> Result<ProjectSummary> {
    let project = get_project(db, id)?;

    let mut stmt = db.prepare(
        "SELECT col.title, COUNT(c.id) \
         FROM columns col \
         LEFT JOIN cards c ON c.column_id = col.id AND c.archived = 0 \
         WHERE col.board_id = ? \
         GROUP BY col.id",
    )?;
    let rows = stmt
        .query_map([&project.board_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut summary = ProjectSummary {
        updated_at: project.updated_at,
        ..ProjectSummary::default()
    };
    for (title, count) in rows {
        match status_from_column_title(&title) {
            Some(CardStatus::Todo) => summary.todo += count,
            Some(CardStatus::InProgress) => summary.in_progress += count,
            Some(CardStatus::Done) => summary.done += count,
            None => {}
        }
    }
    summary.total = summary.todo + summary.in_progress + summary.done;
    summary.file_count = project_files(root, id)?.iter().map(count_files).sum();
    Ok(summary)
}

fn count_files(entry: &FsEntry) -> usize {
    match &entry.children {
        Some(children) => children.iter().map(count_files).sum(),
        None => 1,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn map_project(r: &rusqlite::Row) -> rusqlite::Result<Project> {
    Ok(Project {
        id: r.get(0)?,
        folder_id: r.get(1)?,
        name: r.get(2)?,
        identifier: r.get(3)?,
        description: r.get(4)?,
        status: r.get(5)?,
        board_id: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

/// Trim + require a non-empty name without path separators.
fn require_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(KandriumError::new("Name required"));
    }
    if trimmed.contains(['/', '\\']) {
        return Err(KandriumError::new("Name cannot contain a path separator"));
    }
    Ok(trimmed.to_string())
}

fn remove_dir_ignore_missing(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{bootstrap, open_db};

    struct Fixture {
        _tmp: tempfile::TempDir,
        db: Connection,
        root: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let mut db = open_db(&tmp.path().join("t.db")).unwrap();
        bootstrap(&mut db).unwrap();
        let root = tmp.path().join("projects");
        fs::create_dir_all(&root).unwrap();
        Fixture {
            _tmp: tmp,
            db,
            root,
        }
    }

    fn folders(nodes: &[ProjectNode]) -> Vec<&str> {
        nodes
            .iter()
            .filter_map(|n| match n {
                ProjectNode::Folder { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn create_project_scaffolds_board_columns_and_files() {
        let f = fixture();
        let project = create_project(&f.db, &f.root, "Website", None, "software").unwrap();
        assert_eq!(project.name, "Website");
        assert_eq!(project.status, "active");
        assert!(project.created_at > 0);

        // Board exists with the template's canonical columns.
        let board = crate::get_board(&f.db, &project.board_id).unwrap();
        let titles: Vec<&str> = board.columns.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["To Do", "In Progress", "Done"]);

        // On-disk folder with the template files.
        let dir = project_dir(&f.root, &project.id);
        assert!(dir.join("README.md").is_file());
        assert!(dir.join("notes.md").is_file());
    }

    #[test]
    fn list_tree_nests_and_orders() {
        let f = fixture();
        let work = create_folder(&f.db, "Work", None).unwrap();
        create_folder(&f.db, "Archive", None).unwrap();
        let nested = create_folder(&f.db, "Deep", Some(&work.id)).unwrap();
        create_project(&f.db, &f.root, "Alpha", Some(&nested.id), "blank").unwrap();

        let tree = list_tree(&f.db).unwrap();
        // Folders first (position order), then root projects.
        assert_eq!(folders(&tree), ["Work", "Archive"]);
        let ProjectNode::Folder { children, .. } = &tree[0] else {
            panic!("expected folder");
        };
        let ProjectNode::Folder { children: deep, .. } = &children[0] else {
            panic!("expected nested folder");
        };
        assert!(matches!(deep[0], ProjectNode::Project(ref p) if p.name == "Alpha"));
    }

    #[test]
    fn create_folders_are_siblings_and_root_is_null_safe() {
        let f = fixture();
        let parent = create_folder(&f.db, "Parent", None).unwrap();
        create_folder(&f.db, "A", Some(&parent.id)).unwrap();
        create_folder(&f.db, "B", Some(&parent.id)).unwrap();
        // Two root folders + one nested parent: root-level grouping is separate.
        let tree = list_tree(&f.db).unwrap();
        assert_eq!(folders(&tree), ["Parent"]);
        let ProjectNode::Folder { children, .. } = &tree[0] else {
            panic!()
        };
        assert_eq!(folders(children), ["A", "B"]);
    }

    #[test]
    fn rename_project_renames_the_board_too() {
        let f = fixture();
        let project = create_project(&f.db, &f.root, "Old", None, "blank").unwrap();
        let renamed = rename_project(&f.db, &project.id, "New").unwrap();
        assert_eq!(renamed.name, "New");
        let board = crate::get_board(&f.db, &project.board_id).unwrap();
        assert_eq!(board.title, "New");
    }

    #[test]
    fn update_project_validates_status() {
        let f = fixture();
        let project = create_project(&f.db, &f.root, "P", None, "blank").unwrap();
        let updated = update_project(&f.db, &project.id, Some("hello"), Some("on_hold")).unwrap();
        assert_eq!(updated.description, "hello");
        assert_eq!(updated.status, "on_hold");
        assert!(update_project(&f.db, &project.id, None, Some("bogus")).is_err());
        // The failed update left both fields untouched.
        let after = get_project(&f.db, &project.id).unwrap();
        assert_eq!(after.status, "on_hold");
        assert_eq!(after.description, "hello");
    }

    #[test]
    fn delete_project_removes_board_columns_cards_and_dir() {
        let f = fixture();
        let project = create_project(&f.db, &f.root, "Doomed", None, "software").unwrap();
        let board_id = project.board_id.clone();
        let dir = project_dir(&f.root, &project.id);
        assert!(dir.exists());

        delete_project(&f.db, &f.root, &project.id).unwrap();
        assert!(get_project(&f.db, &project.id).is_err());
        assert!(crate::get_board(&f.db, &board_id).is_err());
        assert!(!dir.exists());
        let cols: i64 =
            f.db.query_row(
                "SELECT COUNT(*) FROM columns WHERE board_id = ?",
                [&board_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cols, 0);
    }

    #[test]
    fn delete_folder_cascades_nested_projects_and_boards() {
        let f = fixture();
        let outer = create_folder(&f.db, "Outer", None).unwrap();
        let inner = create_folder(&f.db, "Inner", Some(&outer.id)).unwrap();
        let p1 = create_project(&f.db, &f.root, "Root", None, "blank").unwrap();
        let p2 = create_project(&f.db, &f.root, "Nested", Some(&inner.id), "blank").unwrap();

        delete_folder(&f.db, &f.root, &outer.id).unwrap();

        // The nested project is gone (row, board, dir); the root project stays.
        assert!(get_project(&f.db, &p2.id).is_err());
        assert!(!project_dir(&f.root, &p2.id).exists());
        assert!(get_project(&f.db, &p1.id).is_ok());
        assert!(project_dir(&f.root, &p1.id).exists());
        assert!(get_project(&f.db, "missing").is_err());
    }

    #[test]
    fn project_summary_counts_buckets_and_files() {
        let f = fixture();
        let project = create_project(&f.db, &f.root, "S", None, "research").unwrap();
        let board = crate::get_board(&f.db, &project.board_id).unwrap();
        let todo = board.columns[0].id.clone();
        let done = board.columns[2].id.clone();
        crate::create_card(
            &f.db,
            &todo,
            &crate::CreateCardBody {
                title: "One".into(),
                ..Default::default()
            },
        )
        .unwrap();
        crate::create_card(
            &f.db,
            &done,
            &crate::CreateCardBody {
                title: "Two".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let summary = project_summary(&f.db, &f.root, &project.id).unwrap();
        assert_eq!(summary.todo, 1);
        assert_eq!(summary.done, 1);
        assert_eq!(summary.total, 2);
        // Research template writes README.md + sources.md.
        assert_eq!(summary.file_count, 2);
        assert!(summary.updated_at > 0);
    }

    #[test]
    fn doc_read_write_round_trip_and_guard() {
        let f = fixture();
        let project = create_project(&f.db, &f.root, "Docs", None, "blank").unwrap();

        write_doc(&f.root, &project.id, Path::new("note.md"), "# Hi\n").unwrap();
        assert_eq!(
            read_doc(&f.root, &project.id, Path::new("note.md")).unwrap(),
            "# Hi\n"
        );

        // Nested paths work; escaping the project folder does not.
        fs::create_dir_all(project_dir(&f.root, &project.id).join("sub")).unwrap();
        write_doc(&f.root, &project.id, Path::new("sub/a.txt"), "x").unwrap();
        assert_eq!(
            read_doc(&f.root, &project.id, Path::new("sub/a.txt")).unwrap(),
            "x"
        );
        assert!(read_doc(&f.root, &project.id, Path::new("../escape.md")).is_err());
        assert!(write_doc(&f.root, &project.id, Path::new("../escape.md"), "x").is_err());
        assert!(!f.root.join("escape.md").exists());
    }

    #[test]
    fn create_doc_file_and_folder_inside_project() {
        let f = fixture();
        let project = create_project(&f.db, &f.root, "Docs", None, "blank").unwrap();
        // The doc helpers return guarded (canonicalized) paths, so compare
        // against the canonical project dir (macOS /var → /private/var).
        let dir = project_dir(&f.root, &project.id).canonicalize().unwrap();

        let file = create_doc_file(&f.root, &project.id, Path::new(""), "ideas.md").unwrap();
        assert_eq!(file, dir.join("ideas.md"));
        assert!(file.is_file());
        assert_eq!(fs::read_to_string(&file).unwrap(), "");

        let folder = create_doc_folder(&f.root, &project.id, Path::new(""), "Notes").unwrap();
        assert_eq!(folder, dir.join("Notes"));
        assert!(folder.is_dir());

        // Nested creation resolves the relative parent.
        let nested = create_doc_file(&f.root, &project.id, Path::new("Notes"), "todo.txt").unwrap();
        assert_eq!(nested, dir.join("Notes/todo.txt"));
        assert!(nested.is_file());

        // Collisions and invalid names are rejected.
        let err = create_doc_file(&f.root, &project.id, Path::new(""), "ideas.md").unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert!(create_doc_file(&f.root, &project.id, Path::new(""), "ideas").is_err());
        assert!(create_doc_file(&f.root, &project.id, Path::new(""), "a/b.md").is_err());
        assert!(create_doc_file(&f.root, &project.id, Path::new(""), ".hidden.md").is_err());
        assert!(create_doc_folder(&f.root, &project.id, Path::new(""), "a/b").is_err());
        assert!(create_doc_folder(&f.root, &project.id, Path::new(""), "Notes").is_err());

        // Missing parents and traversal stay out.
        assert!(create_doc_file(&f.root, &project.id, Path::new("nope"), "x.md").is_err());
        assert!(create_doc_file(&f.root, &project.id, Path::new("../.."), "escape.md").is_err());
        assert!(!f.root.join("escape.md").exists());
    }

    #[test]
    fn name_validation_rejects_separators_and_empties() {
        let f = fixture();
        assert!(create_folder(&f.db, "  ", None).is_err());
        assert!(create_folder(&f.db, "a/b", None).is_err());
        assert!(create_project(&f.db, &f.root, "", None, "blank").is_err());
        assert!(create_project(&f.db, &f.root, "x", None, "nope").is_err());
    }

    #[test]
    fn find_project_walks_nested_folders() {
        let f = fixture();
        let folder = create_folder(&f.db, "F", None).unwrap();
        let project = create_project(&f.db, &f.root, "Deep", Some(&folder.id), "blank").unwrap();
        let tree = list_tree(&f.db).unwrap();
        assert_eq!(find_project(&tree, &project.id).unwrap().name, "Deep");
        assert!(find_project(&tree, "nope").is_none());
    }

    #[test]
    fn identifiers_are_derived_deduped_and_short() {
        let f = fixture();
        let a = create_project(&f.db, &f.root, "Kandrium", None, "blank").unwrap();
        let b = create_project(&f.db, &f.root, "Kandrium", None, "blank").unwrap();
        let c = create_project(&f.db, &f.root, "New Project", None, "blank").unwrap();
        assert_eq!(a.identifier, "kan");
        assert_eq!(b.identifier, "kan2");
        assert_eq!(c.identifier, "np");
        assert!(a.identifier.chars().count() <= 10);

        // Non-ASCII names fall back to a stable prefix and still dedupe.
        let d = create_project(&f.db, &f.root, "プロジェクト", None, "blank").unwrap();
        assert_eq!(d.identifier, "prj");
    }

    #[test]
    fn rename_regenerates_auto_identifier_until_pinned() {
        let f = fixture();
        let project = create_project(&f.db, &f.root, "New Project", None, "blank").unwrap();
        assert_eq!(project.identifier, "np");

        let renamed = rename_project(&f.db, &project.id, "Website").unwrap();
        assert_eq!(renamed.identifier, "web");

        let pinned = set_identifier(&f.db, &project.id, "site").unwrap();
        assert_eq!(pinned.identifier, "site");

        let renamed_again = rename_project(&f.db, &project.id, "Kandrium").unwrap();
        assert_eq!(renamed_again.identifier, "site");
    }

    #[test]
    fn set_identifier_validates_and_rejects_duplicates() {
        let f = fixture();
        let a = create_project(&f.db, &f.root, "Alpha", None, "blank").unwrap();
        let _b = create_project(&f.db, &f.root, "Beta", None, "blank").unwrap();

        assert_eq!(
            set_identifier(&f.db, &a.id, "  Ab-1 ").unwrap().identifier,
            "ab-1"
        );
        assert!(set_identifier(&f.db, &a.id, "").is_err());
        // 10 characters are allowed; 11 are not.
        assert_eq!(
            set_identifier(&f.db, &a.id, "1234567890")
                .unwrap()
                .identifier,
            "1234567890"
        );
        assert!(set_identifier(&f.db, &a.id, "12345678901").is_err());
        assert!(set_identifier(&f.db, &a.id, "has space").is_err());

        let duplicate = set_identifier(&f.db, &a.id, "BET");
        assert!(duplicate.is_err());
        assert!(duplicate.unwrap_err().0.contains("already in use"));
    }

    #[test]
    fn project_choices_are_sorted_and_carry_identifiers() {
        let f = fixture();
        create_project(&f.db, &f.root, "Zulu", None, "blank").unwrap();
        create_project(&f.db, &f.root, "Alpha", None, "blank").unwrap();
        let choices = list_project_choices(&f.db).unwrap();
        assert_eq!(
            choices
                .iter()
                .map(|c| (c.name.as_str(), c.identifier.as_str()))
                .collect::<Vec<_>>(),
            [("Zulu", "zul"), ("Alpha", "alp")]
        );
    }
}
