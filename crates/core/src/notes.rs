//! Notes file-system layer — plain markdown files under a per-user root.
//!
//! Notes live outside the SQLite database: `<data_dir>/kandrium/notes/`
//! (override with `KANDRIUM_NOTES`). All functions are synchronous and
//! validate that every touched path stays inside the root (no `..`
//! traversal, no symlink escapes).

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::{KandriumError, Result};

/// One node in the notes tree.
#[derive(Debug, Clone)]
pub struct NoteEntry {
    pub name: String,
    /// Absolute path, guaranteed to live under the root it was scanned from.
    pub path: PathBuf,
    pub is_dir: bool,
    /// `Some` for directories (possibly empty), `None` for files.
    pub children: Option<Vec<NoteEntry>>,
}

/// Resolve the notes root:
/// 1. `KANDRIUM_NOTES` env var (explicit override)
/// 2. `<data_dir>/kandrium/notes`
pub fn resolve_notes_root() -> PathBuf {
    if let Some(p) = std::env::var_os("KANDRIUM_NOTES") {
        return PathBuf::from(p);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kandrium")
        .join("notes")
}

/// Create the root directory if missing and return it.
pub fn ensure_root() -> Result<PathBuf> {
    let root = resolve_notes_root();
    fs::create_dir_all(&root)?;
    Ok(root)
}

/// Recursively scan `root` into a sorted tree: directories first, then
/// files, each tier ordered by case-insensitive name. Dot-prefixed entries,
/// non-markdown files, and symlinks are skipped.
pub fn scan_tree(root: &Path) -> Result<Vec<NoteEntry>> {
    let root = canonical_root(root)?;
    scan_dir(&root)
}

pub fn read_note(path: &Path, root: &Path) -> Result<String> {
    let path = guard(root, path)?;
    Ok(fs::read_to_string(path)?)
}

pub fn write_note(path: &Path, root: &Path, text: &str) -> Result<()> {
    let path = guard(root, path)?;
    fs::write(path, text)?;
    Ok(())
}

/// Create `untitled.md` (or `untitled-N.md` when taken) inside `in_dir`
/// and return its path.
pub fn create_note(root: &Path, in_dir: &Path) -> Result<PathBuf> {
    let dir = guard(root, in_dir)?;
    for n in 1.. {
        let name = if n == 1 {
            "untitled.md".to_string()
        } else {
            format!("untitled-{n}.md")
        };
        let candidate = dir.join(&name);
        if !candidate.exists() {
            fs::write(&candidate, "")?;
            return Ok(candidate);
        }
    }
    unreachable!()
}

/// Create `New Folder` (or `New Folder N` when taken) inside `in_dir`
/// and return its path.
pub fn create_folder(root: &Path, in_dir: &Path) -> Result<PathBuf> {
    let dir = guard(root, in_dir)?;
    for n in 1.. {
        let name = if n == 1 {
            "New Folder".to_string()
        } else {
            format!("New Folder {n}")
        };
        let candidate = dir.join(&name);
        if !candidate.exists() {
            fs::create_dir(&candidate)?;
            return Ok(candidate);
        }
    }
    unreachable!()
}

/// Rename a file or directory in place (same parent). `new_name` must be a
/// bare file name — separators are rejected. Returns the new path.
pub fn rename_path(path: &Path, root: &Path, new_name: &str) -> Result<PathBuf> {
    let path = guard(root, path)?;
    if new_name.is_empty() || new_name == "." || new_name == ".." || new_name.contains(['/', '\\'])
    {
        return Err(KandriumError::new("Invalid name"));
    }
    let target = path
        .parent()
        .ok_or_else(|| KandriumError::new("Cannot rename the root"))?
        .join(new_name);
    guard(root, &target)?;
    if target.exists() {
        return Err(KandriumError::new(format!("\"{new_name}\" already exists")));
    }
    fs::rename(&path, &target)?;
    Ok(target)
}

/// Delete a file or directory (recursively). The notes root itself cannot
/// be deleted.
pub fn delete_path(path: &Path, root: &Path) -> Result<()> {
    let path = guard(root, path)?;
    if path == canonical_root(root)? {
        return Err(KandriumError::new("Cannot delete the notes root"));
    }
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn canonical_root(root: &Path) -> Result<PathBuf> {
    Ok(root.canonicalize()?)
}

/// Normalize a path lexically (resolve `.`/`..` without touching the
/// filesystem) so guards work for paths that do not exist yet.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Validate that `path` stays inside `root`; returns a normalized absolute
/// path. Existing paths are canonicalized (resolving symlinks); missing
/// paths canonicalize their deepest existing ancestor and re-append the
/// not-yet-existing tail, so e.g. macOS `/var` → `/private/var` symlinks
/// can't slip past the prefix check.
fn guard(root: &Path, path: &Path) -> Result<PathBuf> {
    let root = canonical_root(root)?;
    let resolved = if path.exists() {
        path.canonicalize()?
    } else {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let abs = normalize(&abs);
        let mut ancestor = abs.as_path();
        let mut tail = Vec::new();
        while !ancestor.exists() {
            let name = ancestor
                .file_name()
                .ok_or_else(|| KandriumError::new("Invalid path"))?;
            tail.push(name.to_os_string());
            ancestor = ancestor
                .parent()
                .ok_or_else(|| KandriumError::new("Invalid path"))?;
        }
        let mut resolved = ancestor.canonicalize()?;
        for comp in tail.iter().rev() {
            resolved.push(comp);
        }
        resolved
    };
    if !resolved.starts_with(&root) {
        return Err(KandriumError::new("Path escapes the notes root"));
    }
    Ok(resolved)
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false)
}

fn scan_dir(dir: &Path) -> Result<Vec<NoteEntry>> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        // Skip symlinks outright: a link pointing outside the root would
        // otherwise leak external paths into the tree.
        if entry.file_type()?.is_symlink() {
            continue;
        }
        if path.is_dir() {
            dirs.push(NoteEntry {
                name,
                children: Some(scan_dir(&path)?),
                is_dir: true,
                path,
            });
        } else if is_markdown(&path) {
            files.push(NoteEntry {
                name,
                path,
                is_dir: false,
                children: None,
            });
        }
    }
    let key = |e: &NoteEntry| e.name.to_lowercase();
    dirs.sort_by_key(key);
    files.sort_by_key(key);
    dirs.extend(files);
    Ok(dirs)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("notes");
        fs::create_dir_all(&root).unwrap();
        Fixture { _tmp: tmp, root }
    }

    #[test]
    fn guard_rejects_traversal_and_external_paths() {
        let f = fixture();
        let outside = f.root.parent().unwrap().join("outside.md");
        fs::write(&outside, "x").unwrap();

        assert!(read_note(&f.root.join("../outside.md"), &f.root).is_err());
        assert!(read_note(&outside, &f.root).is_err());
        assert!(write_note(&f.root.join("../../evil.md"), &f.root, "x").is_err());
    }

    #[test]
    fn guard_allows_missing_paths_inside_root() {
        let f = fixture();
        let new_file = f.root.join("sub").join("new.md");
        // Parent doesn't exist yet; lexical normalization must still pass.
        fs::create_dir_all(f.root.join("sub")).unwrap();
        write_note(&new_file, &f.root, "hello").unwrap();
        assert_eq!(read_note(&new_file, &f.root).unwrap(), "hello");
    }

    #[test]
    fn scan_filters_and_sorts() {
        let f = fixture();
        fs::write(f.root.join("b.md"), "").unwrap();
        fs::write(f.root.join("A.md"), "").unwrap();
        fs::write(f.root.join("skip.txt"), "").unwrap();
        fs::write(f.root.join(".hidden.md"), "").unwrap();
        fs::create_dir(f.root.join("Zed")).unwrap();
        fs::create_dir(f.root.join("alpha")).unwrap();
        fs::write(f.root.join("alpha").join("inner.md"), "").unwrap();

        let tree = scan_tree(&f.root).unwrap();
        let names: Vec<&str> = tree.iter().map(|e| e.name.as_str()).collect();
        // Dirs first (case-insensitive), then files.
        assert_eq!(names, ["alpha", "Zed", "A.md", "b.md"]);
        assert!(tree[0].is_dir);
        assert_eq!(tree[0].children.as_ref().unwrap().len(), 1);
        assert_eq!(tree[0].children.as_ref().unwrap()[0].name, "inner.md");
    }

    #[test]
    fn scan_skips_symlinks() {
        let f = fixture();
        let target = f.root.join("real.md");
        fs::write(&target, "x").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, f.root.join("link.md")).unwrap();
            let tree = scan_tree(&f.root).unwrap();
            let names: Vec<&str> = tree.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, ["real.md"]);
        }
    }

    #[test]
    fn crud_round_trip() {
        let f = fixture();
        let dir = create_folder(&f.root, &f.root).unwrap();
        let note = create_note(&f.root, &dir).unwrap();
        write_note(&note, &f.root, "# Title").unwrap();
        assert_eq!(read_note(&note, &f.root).unwrap(), "# Title");

        let renamed = rename_path(&note, &f.root, "renamed.md").unwrap();
        assert!(renamed.ends_with("renamed.md"));
        assert!(!note.exists());

        delete_path(&dir, &f.root).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn delete_rejects_the_root_itself() {
        let f = fixture();
        fs::write(f.root.join("keep.md"), "x").unwrap();
        assert!(delete_path(&f.root, &f.root).is_err());
        assert!(f.root.join("keep.md").exists());
    }

    #[test]
    fn create_note_dedupes_names() {
        let f = fixture();
        let a = create_note(&f.root, &f.root).unwrap();
        let b = create_note(&f.root, &f.root).unwrap();
        let c = create_note(&f.root, &f.root).unwrap();
        assert!(a.ends_with("untitled.md"));
        assert!(b.ends_with("untitled-2.md"));
        assert!(c.ends_with("untitled-3.md"));
    }

    #[test]
    fn rename_rejects_bad_names_and_conflicts() {
        let f = fixture();
        let note = create_note(&f.root, &f.root).unwrap();
        assert!(rename_path(&note, &f.root, "").is_err());
        assert!(rename_path(&note, &f.root, "a/b.md").is_err());
        assert!(rename_path(&note, &f.root, "..").is_err());

        let other = create_note(&f.root, &f.root).unwrap();
        let taken = other.file_name().unwrap().to_string_lossy().to_string();
        assert!(rename_path(&note, &f.root, &taken).is_err());
    }
}
