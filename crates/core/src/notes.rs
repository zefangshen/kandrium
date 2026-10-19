//! Notes file-system layer — plain markdown files under a per-user root.
//!
//! Notes live outside the SQLite database: `<data_dir>/kandrium/notes/`
//! (override with `KANDRIUM_NOTES`). All functions are synchronous and
//! validate that every touched path stays inside the root (no `..`
//! traversal, no symlink escapes); the shared plumbing lives in
//! [`crate::fsutil`].

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{KandriumError, Result};
use crate::fsutil::{self, FsEntry};

/// One node in the notes tree.
pub type NoteEntry = FsEntry;

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
    fsutil::scan_tree(root, &["md", "markdown"])
}

pub fn read_note(path: &Path, root: &Path) -> Result<String> {
    let path = fsutil::guard(root, path)?;
    Ok(fs::read_to_string(path)?)
}

pub fn write_note(path: &Path, root: &Path, text: &str) -> Result<()> {
    let path = fsutil::guard(root, path)?;
    fs::write(path, text)?;
    Ok(())
}

/// Create `untitled.md` (or `untitled-N.md` when taken) inside `in_dir`
/// and return its path.
pub fn create_note(root: &Path, in_dir: &Path) -> Result<PathBuf> {
    let dir = fsutil::guard(root, in_dir)?;
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
    let dir = fsutil::guard(root, in_dir)?;
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
    rename_in_root(path, root, new_name)
}

/// Delete a file or directory (recursively). The notes root itself cannot
/// be deleted.
pub fn delete_path(path: &Path, root: &Path) -> Result<()> {
    let path = fsutil::guard(root, path)?;
    if path == fsutil::canonical_root(root)? {
        return Err(KandriumError::new("Cannot delete the notes root"));
    }
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Shared rename logic (also used by the documents layer): validate the
/// name, reject collisions, rename in place.
pub(crate) fn rename_in_root(path: &Path, root: &Path, new_name: &str) -> Result<PathBuf> {
    let path = fsutil::guard(root, path)?;
    if new_name.is_empty() || new_name == "." || new_name == ".." || new_name.contains(['/', '\\'])
    {
        return Err(KandriumError::new("Invalid name"));
    }
    let target = path
        .parent()
        .ok_or_else(|| KandriumError::new("Cannot rename the root"))?
        .join(new_name);
    fsutil::guard(root, &target)?;
    if target.exists() {
        return Err(KandriumError::new(format!("\"{new_name}\" already exists")));
    }
    fs::rename(&path, &target)?;
    Ok(target)
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
    fn guard_rejects_external_paths() {
        let f = fixture();
        let outside = f.root.parent().unwrap().join("outside.md");
        fs::write(&outside, "x").unwrap();

        assert!(read_note(&f.root.join("../outside.md"), &f.root).is_err());
        assert!(write_note(&f.root.join("../../evil.md"), &f.root, "x").is_err());
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
