//! Shared filesystem plumbing for root-scanned file collections (notes,
//! documents).
//!
//! Every collection lives under a per-user root directory. All helpers
//! validate that every touched path stays inside the root (no `..`
//! traversal, no symlink escapes) and scan trees with a fixed set of file
//! extensions.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::{KandriumError, Result};

/// One node in a scanned file tree.
#[derive(Debug, Clone)]
pub struct FsEntry {
    pub name: String,
    /// Absolute path, guaranteed to live under the root it was scanned from.
    pub path: PathBuf,
    pub is_dir: bool,
    /// `Some` for directories (possibly empty), `None` for files.
    pub children: Option<Vec<FsEntry>>,
}

/// Recursively scan `root` into a sorted tree: directories first, then
/// files, each tier ordered by case-insensitive name. Dot-prefixed entries,
/// files whose (case-insensitive) extension is not in `exts`, and symlinks
/// are skipped.
pub fn scan_tree(root: &Path, exts: &[&str]) -> Result<Vec<FsEntry>> {
    let root = canonical_root(root)?;
    scan_dir(&root, exts)
}

pub fn canonical_root(root: &Path) -> Result<PathBuf> {
    Ok(root.canonicalize()?)
}

/// Normalize a path lexically (resolve `.`/`..` without touching the
/// filesystem) so guards work for paths that do not exist yet.
pub fn normalize(path: &Path) -> PathBuf {
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
pub fn guard(root: &Path, path: &Path) -> Result<PathBuf> {
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
        return Err(KandriumError::new("Path escapes the root"));
    }
    Ok(resolved)
}

/// True when `path` has one of the given case-insensitive extensions.
pub fn has_extension(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
        .unwrap_or(false)
}

fn scan_dir(dir: &Path, exts: &[&str]) -> Result<Vec<FsEntry>> {
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
            dirs.push(FsEntry {
                name,
                children: Some(scan_dir(&path, exts)?),
                is_dir: true,
                path,
            });
        } else if has_extension(&path, exts) {
            files.push(FsEntry {
                name,
                path,
                is_dir: false,
                children: None,
            });
        }
    }
    let key = |e: &FsEntry| e.name.to_lowercase();
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

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        (tmp, root)
    }

    #[test]
    fn guard_rejects_traversal_and_external_paths() {
        let (_tmp, root) = fixture();
        let outside = root.parent().unwrap().join("outside.md");
        fs::write(&outside, "x").unwrap();

        assert!(guard(&root, &root.join("../outside.md")).is_err());
        assert!(guard(&root, &outside).is_err());
        assert!(guard(&root, &root.join("../../evil.md")).is_err());
    }

    #[test]
    fn guard_allows_missing_paths_inside_root() {
        let (_tmp, root) = fixture();
        let new_file = root.join("sub").join("new.md");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(&new_file, "hello").unwrap();
        assert_eq!(
            guard(&root, &new_file).unwrap(),
            new_file.canonicalize().unwrap()
        );
    }

    #[test]
    fn scan_filters_and_sorts() {
        let (_tmp, root) = fixture();
        fs::write(root.join("b.pdf"), "").unwrap();
        fs::write(root.join("A.pdf"), "").unwrap();
        fs::write(root.join("skip.txt"), "").unwrap();
        fs::write(root.join(".hidden.pdf"), "").unwrap();
        fs::create_dir(root.join("Zed")).unwrap();
        fs::create_dir(root.join("alpha")).unwrap();
        fs::write(root.join("alpha").join("inner.pdf"), "").unwrap();

        let tree = scan_tree(&root, &["pdf"]).unwrap();
        let names: Vec<&str> = tree.iter().map(|e| e.name.as_str()).collect();
        // Dirs first (case-insensitive), then files.
        assert_eq!(names, ["alpha", "Zed", "A.pdf", "b.pdf"]);
        assert!(tree[0].is_dir);
        assert_eq!(tree[0].children.as_ref().unwrap().len(), 1);
        assert_eq!(tree[0].children.as_ref().unwrap()[0].name, "inner.pdf");
    }

    #[test]
    fn scan_matches_extensions_case_insensitively() {
        let (_tmp, root) = fixture();
        fs::write(root.join("a.PDF"), "").unwrap();
        fs::write(root.join("b.Pdf"), "").unwrap();
        fs::write(root.join("c.pdf"), "").unwrap();
        assert_eq!(scan_tree(&root, &["pdf"]).unwrap().len(), 3);
    }

    #[test]
    fn scan_skips_symlinks() {
        let (_tmp, root) = fixture();
        let target = root.join("real.pdf");
        fs::write(&target, "x").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, root.join("link.pdf")).unwrap();
            let tree = scan_tree(&root, &["pdf"]).unwrap();
            let names: Vec<&str> = tree.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, ["real.pdf"]);
        }
    }
}
