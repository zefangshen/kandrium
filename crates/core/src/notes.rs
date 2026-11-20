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
use crate::history::{RootKind, RunContext};

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
    write_note_in_run(path, root, text, RootKind::Notes, None)
}

/// Run-scoped variant of [`write_note`]: captures the previous bytes before
/// the write and records the after-image.
///
/// `root_kind` names the root the file belongs to in the run manifest; callers
/// that store wiki pages through this helper must pass [`RootKind::Wiki`] so
/// undo resolves the same directory.
pub fn write_note_in_run(
    path: &Path,
    root: &Path,
    text: &str,
    root_kind: RootKind,
    run: Option<&RunContext>,
) -> Result<()> {
    let path = fsutil::guard(root, path)?;
    let guard = match run {
        Some(ctx) => Some(ctx.capture(root_kind, root, &path)?),
        None => None,
    };
    fsutil::write_atomic(&path, text)?;
    if let Some(guard) = guard {
        guard.commit(Some(text.as_bytes()))?;
    }
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
            fsutil::write_atomic(&candidate, "")?;
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
    rename_path_in_run(path, root, new_name, None)
}

/// Run-scoped variant of [`rename_path`]. A rename is recorded as a delete of
/// the old path plus a create of the new one; a directory rename captures both
/// trees.
pub fn rename_path_in_run(
    path: &Path,
    root: &Path,
    new_name: &str,
    run: Option<&RunContext>,
) -> Result<PathBuf> {
    let path = fsutil::guard(root, path)?;
    let is_dir = path.is_dir();
    let old_guard = match run {
        Some(ctx) if !is_dir => Some(ctx.capture(RootKind::Notes, root, &path)?),
        Some(ctx) => {
            ctx.capture_tree(RootKind::Notes, root, &path)?;
            None
        }
        None => None,
    };
    let target = rename_in_root(&path, root, new_name)?;
    if let Some(ctx) = run {
        if is_dir {
            ctx.record_tree(RootKind::Notes, root, &target)?;
        } else {
            if let Some(guard) = old_guard {
                guard.commit(None)?;
            }
            ctx.record_created(RootKind::Notes, root, &target)?;
        }
    }
    Ok(target)
}

/// Create an empty note named `name` inside `in_dir`.
///
/// The `.md` extension is appended when the name has none. The name is a bare
/// file name (separators rejected) and must not collide, so the caller can
/// show the error instead of silently getting `untitled-2.md`.
pub fn create_named_note(root: &Path, in_dir: &Path, name: &str) -> Result<PathBuf> {
    let dir = fsutil::guard(root, in_dir)?;
    let name = bare_name(name)?;
    let file_name = if name.to_ascii_lowercase().ends_with(".md")
        || name.to_ascii_lowercase().ends_with(".markdown")
    {
        name.to_string()
    } else {
        format!("{name}.md")
    };
    let candidate = dir.join(&file_name);
    fsutil::guard(root, &candidate)?;
    // Exclusive create: a file that appears between the guard and the write
    // surfaces as `AlreadyExists` instead of being truncated.
    match fsutil::create_new_file(&candidate) {
        Ok(()) => Ok(candidate),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(KandriumError::new(
            format!("\"{file_name}\" already exists"),
        )),
        Err(error) => Err(error.into()),
    }
}

/// Create a folder named `name` inside `in_dir`, with the same name rules as
/// [`create_named_note`].
pub fn create_named_folder(root: &Path, in_dir: &Path, name: &str) -> Result<PathBuf> {
    let dir = fsutil::guard(root, in_dir)?;
    let name = bare_name(name)?;
    let candidate = dir.join(name);
    fsutil::guard(root, &candidate)?;
    if candidate.exists() {
        return Err(KandriumError::new(format!("\"{name}\" already exists")));
    }
    fs::create_dir(&candidate)?;
    Ok(candidate)
}

/// Reject empty names, `.`/`..` and anything containing a separator.
fn bare_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(KandriumError::new("Invalid name"));
    }
    Ok(name)
}

/// Delete a file or directory (recursively). The notes root itself cannot
/// be deleted.
pub fn delete_path(path: &Path, root: &Path) -> Result<()> {
    delete_path_in_run(path, root, None)
}

/// Run-scoped variant of [`delete_path`]. A directory is enumerated in full
/// before removal; a single file captures its bytes first.
pub fn delete_path_in_run(path: &Path, root: &Path, run: Option<&RunContext>) -> Result<()> {
    let path = fsutil::guard(root, path)?;
    if path == fsutil::canonical_root(root)? {
        return Err(KandriumError::new("Cannot delete the notes root"));
    }
    if path.is_dir() {
        if let Some(ctx) = run {
            ctx.capture_tree(RootKind::Notes, root, &path)?;
        }
        fs::remove_dir_all(path)?;
    } else {
        let guard = match run {
            Some(ctx) => Some(ctx.capture(RootKind::Notes, root, &path)?),
            None => None,
        };
        fs::remove_file(&path)?;
        if let Some(guard) = guard {
            guard.commit(None)?;
        }
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
    fn named_creation_appends_md_and_rejects_collisions() {
        let f = fixture();
        let note = create_named_note(&f.root, &f.root, "Meeting notes").unwrap();
        assert!(note.ends_with("Meeting notes.md"));
        assert!(note.exists());

        let kept = create_named_note(&f.root, &f.root, "todo.markdown").unwrap();
        assert!(kept.ends_with("todo.markdown"));

        fs::write(&note, "keep me").unwrap();
        let collision = create_named_note(&f.root, &f.root, "Meeting notes").unwrap_err();
        assert_eq!(collision.to_string(), "\"Meeting notes.md\" already exists");
        assert_eq!(fs::read_to_string(&note).unwrap(), "keep me");
        assert!(create_named_note(&f.root, &f.root, "../escape").is_err());

        let folder = create_named_folder(&f.root, &f.root, "Archive").unwrap();
        assert!(folder.is_dir());
        assert!(create_named_folder(&f.root, &f.root, "Archive").is_err());
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

    fn run_ctx(f: &Fixture) -> RunContext {
        let store = crate::history::HistoryStore::new(f._tmp.path().join("history"));
        RunContext::new(store, "run1")
    }

    #[test]
    fn run_scoped_writes_renames_and_deletes_are_captured() {
        let f = fixture();
        let ctx = run_ctx(&f);
        let note = f.root.join("topic.md");
        fs::write(&note, "one").unwrap();

        write_note_in_run(&note, &f.root, "two", RootKind::Notes, Some(&ctx)).unwrap();
        write_note_in_run(&note, &f.root, "two", RootKind::Notes, Some(&ctx)).unwrap();
        let renamed = rename_path_in_run(&note, &f.root, "renamed.md", Some(&ctx)).unwrap();
        delete_path_in_run(&renamed, &f.root, Some(&ctx)).unwrap();

        let manifest = ctx.store().load(ctx.run_id()).unwrap().unwrap();
        let ops: Vec<(String, crate::history::FileOp)> = manifest
            .files
            .iter()
            .map(|entry| (entry.path.clone(), entry.op))
            .collect();
        assert_eq!(
            ops,
            vec![
                ("topic.md".to_string(), crate::history::FileOp::Modify),
                ("topic.md".to_string(), crate::history::FileOp::Delete),
                ("renamed.md".to_string(), crate::history::FileOp::Create),
                ("renamed.md".to_string(), crate::history::FileOp::Delete),
            ]
        );
        // A rename records the delete before the create, so reverse order
        // removes the new path and then restores the old one.
        let pre_image = manifest.files[1].before.as_ref().unwrap();
        assert_eq!(ctx.store().read_object(&pre_image.hash).unwrap(), b"two");
    }

    #[test]
    fn run_scoped_directory_delete_captures_the_whole_tree() {
        let f = fixture();
        let ctx = run_ctx(&f);
        let dir = f.root.join("archive");
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("a.md"), "a").unwrap();
        fs::write(dir.join("nested").join("b.md"), "b").unwrap();

        delete_path_in_run(&dir, &f.root, Some(&ctx)).unwrap();
        assert!(!dir.exists());

        let manifest = ctx.store().load(ctx.run_id()).unwrap().unwrap();
        assert_eq!(manifest.files.len(), 2);
        assert!(
            manifest
                .files
                .iter()
                .all(|entry| { entry.op == crate::history::FileOp::Delete && entry.restorable })
        );
        assert!(manifest.dirs.iter().any(|entry| entry.path == "archive"));
        assert!(
            manifest
                .dirs
                .iter()
                .any(|entry| entry.path == "archive/nested")
        );
        for entry in &manifest.files {
            let bytes = ctx
                .store()
                .read_object(&entry.before.as_ref().unwrap().hash)
                .unwrap();
            assert!(bytes == b"a" || bytes == b"b");
        }
    }
}
