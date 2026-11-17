//! File restore: the directory, file and symlink phases of undo.
//!
//! The row phase lives in [`super::rows`] and runs after this one. Restore is
//! deliberately explicit: recreate removed directories parent-first, put files
//! and links back in reverse capture order, then remove directories the run
//! created child-first. Everything that cannot be restored is reported rather
//! than silently skipped.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::Result;
use crate::fsutil;
use crate::wiki::{hash_file, index, refresh_sources};

use super::store::HistoryStore;
use super::{DirEntry, DirOp, FileEntry, FileOp, LinkEntry, Manifest, RootKind};

/// One path the restore refused to touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileConflict {
    pub root: RootKind,
    pub path: String,
    pub reason: String,
}

/// One path the restore could not bring back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEntry {
    pub path: String,
    pub reason: String,
}

/// What the file phase did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileUndoReport {
    /// Conflicts were found and nothing was touched (`force` false).
    pub blocked: bool,
    pub restored: usize,
    pub deleted: usize,
    pub skipped: Vec<SkippedEntry>,
    pub conflicts: Vec<FileConflict>,
    pub dirs_recreated: usize,
    pub dirs_removed: usize,
    /// Project ids referenced by restored paths with no `projects` row.
    pub orphans: Vec<String>,
}

impl FileUndoReport {
    /// Undo finished, but some entries could not be restored.
    pub fn partial(&self) -> bool {
        !self.skipped.is_empty()
    }
}

/// Resolved roots a run's entries are re-joined under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    pub wiki: PathBuf,
    pub notes: PathBuf,
    pub documents: PathBuf,
    pub projects: PathBuf,
}

impl Roots {
    /// Current roots, honouring the `KANDRIUM_*` overrides.
    pub fn resolve() -> Self {
        Self {
            wiki: crate::wiki::resolve_wiki_root(),
            notes: crate::notes::resolve_notes_root(),
            documents: crate::documents::resolve_documents_root(),
            projects: crate::projects::resolve_projects_root(),
        }
    }

    pub fn path(&self, root: RootKind) -> &Path {
        match root {
            RootKind::Wiki => &self.wiki,
            RootKind::Notes => &self.notes,
            RootKind::Documents => &self.documents,
            RootKind::Projects => &self.projects,
        }
    }
}

/// Restore a run's files, directories and links.
///
/// Without `force`, a conflict anywhere aborts before anything is touched and
/// the returned report has `blocked: true`. With `force`, conflicted entries are
/// skipped and every other entry still restores. Entries already marked
/// unrestorable are reported, not treated as conflicts.
pub fn restore_files(
    db: &Connection,
    store: &HistoryStore,
    roots: &Roots,
    manifest: &Manifest,
    force: bool,
) -> Result<FileUndoReport> {
    let mut report = FileUndoReport {
        skipped: expected_skips(manifest),
        ..FileUndoReport::default()
    };
    let conflicts = preflight(store, roots, manifest);
    if !conflicts.is_empty() && !force {
        report.blocked = true;
        report.conflicts = conflicts;
        return Ok(report);
    }
    // `force` skips conflicted files rather than overwriting them; rows use the
    // same flag differently (they restore the start snapshot).
    let skip: std::collections::HashMap<(RootKind, String), String> = conflicts
        .iter()
        .map(|conflict| {
            (
                (conflict.root, conflict.path.clone()),
                conflict.reason.clone(),
            )
        })
        .collect();
    report.conflicts = conflicts;

    // Step 4: recreate removed directories, parents before children.
    let mut rmdir: Vec<&DirEntry> = manifest
        .dirs
        .iter()
        .filter(|entry| entry.op == DirOp::Rmdir)
        .collect();
    rmdir.sort_by_key(|entry| (depth(&entry.path), entry.path.clone()));
    for entry in rmdir {
        if let Some(reason) = skip.get(&(entry.root, entry.path.clone())) {
            report.skip(&entry.path, format!("conflict: {reason}"));
            continue;
        }
        let path = match resolve(roots, entry.root, &entry.path) {
            Ok(path) => path,
            Err(error) => {
                report.skip(&entry.path, error.to_string());
                continue;
            }
        };
        if path.is_file() || (path.exists() && !is_empty_dir(&path)) {
            report.skip(&entry.path, "directory path occupied".to_string());
            continue;
        }
        fs::create_dir_all(&path)?;
        set_mode(&path, entry.mode);
        report.dirs_recreated += 1;
    }

    // Step 5: files and links in reverse capture order.
    let mut items: Vec<Item<'_>> = manifest
        .files
        .iter()
        .map(Item::File)
        .chain(manifest.links.iter().map(Item::Link))
        .collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.seq()));
    for item in items {
        match item {
            Item::File(entry) => restore_file(roots, store, entry, &skip, &mut report)?,
            Item::Link(entry) => restore_link(roots, entry, &skip, &mut report)?,
        }
    }

    // Step 6: remove directories the run created, children before parents.
    let mut mkdir: Vec<&DirEntry> = manifest
        .dirs
        .iter()
        .filter(|entry| entry.op == DirOp::Mkdir)
        .collect();
    mkdir.sort_by_key(|entry| (std::cmp::Reverse(depth(&entry.path)), entry.path.clone()));
    for entry in mkdir {
        if let Some(reason) = skip.get(&(entry.root, entry.path.clone())) {
            report.skip(&entry.path, format!("conflict: {reason}"));
            continue;
        }
        let path = match resolve(roots, entry.root, &entry.path) {
            Ok(path) => path,
            Err(error) => {
                report.skip(&entry.path, error.to_string());
                continue;
            }
        };
        if !path.exists() {
            continue;
        }
        if !path.is_dir() || !is_empty_dir(&path) {
            report.skip(&entry.path, "directory is not empty".to_string());
            continue;
        }
        fs::remove_dir(&path)?;
        report.dirs_removed += 1;
    }

    report.orphans = orphan_projects(db, manifest);
    reconcile(db, roots, manifest)?;
    Ok(report)
}

enum Item<'a> {
    File(&'a FileEntry),
    Link(&'a LinkEntry),
}

impl Item<'_> {
    fn seq(&self) -> i64 {
        match self {
            Self::File(entry) => entry.seq,
            Self::Link(entry) => entry.seq,
        }
    }
}

fn restore_file(
    roots: &Roots,
    store: &HistoryStore,
    entry: &FileEntry,
    skip: &std::collections::HashMap<(RootKind, String), String>,
    report: &mut FileUndoReport,
) -> Result<()> {
    if !entry.restorable {
        return Ok(());
    }
    if let Some(reason) = skip.get(&(entry.root, entry.path.clone())) {
        report.skip(&entry.path, format!("conflict: {reason}"));
        return Ok(());
    }
    let path = match resolve(roots, entry.root, &entry.path) {
        Ok(path) => path,
        Err(error) => {
            report.skip(&entry.path, error.to_string());
            return Ok(());
        }
    };

    match entry.op {
        FileOp::Create => {
            if !path.exists() {
                // Already gone, or never written again: undoing is a no-op.
                report.deleted += 1;
                return Ok(());
            }
            if path.is_dir() {
                report.skip(&entry.path, "path is a directory".to_string());
                return Ok(());
            }
            fs::remove_file(&path)?;
            report.deleted += 1;
        }
        FileOp::Modify | FileOp::Delete => {
            let Some(before) = &entry.before else {
                report.skip(&entry.path, "no pre-image".to_string());
                return Ok(());
            };
            if before.hash.is_empty() {
                report.skip(&entry.path, "pre-image not stored".to_string());
                return Ok(());
            }
            let bytes = match store.read_object(&before.hash) {
                Ok(bytes) => bytes,
                Err(_) => {
                    report.skip(&entry.path, "pre-image object is missing".to_string());
                    return Ok(());
                }
            };
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fsutil::write_atomic(&path, &bytes)?;
            set_mode(&path, before.mode);
            report.restored += 1;
        }
        FileOp::Rename => {
            // Renames are captured as delete + create; a lone rename entry is
            // treated as a restore of the pre-image.
            report.skip(&entry.path, "unpaired rename".to_string());
        }
    }
    Ok(())
}

fn restore_link(
    roots: &Roots,
    entry: &LinkEntry,
    skip: &std::collections::HashMap<(RootKind, String), String>,
    report: &mut FileUndoReport,
) -> Result<()> {
    if let Some(reason) = skip.get(&(entry.root, entry.path.clone())) {
        report.skip(&entry.path, format!("conflict: {reason}"));
        return Ok(());
    }
    let path = match resolve(roots, entry.root, &entry.path) {
        Ok(path) => path,
        Err(error) => {
            report.skip(&entry.path, error.to_string());
            return Ok(());
        }
    };
    let existing = fs::symlink_metadata(&path).ok();
    match existing {
        Some(metadata) if metadata.file_type().is_symlink() => {
            if fs::read_link(&path).ok().as_deref() == Some(Path::new(&entry.target)) {
                return Ok(());
            }
            fs::remove_file(&path)?;
        }
        Some(_) => {
            fs::remove_file(&path)?;
        }
        None => {}
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&entry.target, &path)?;
    report.restored += 1;
    Ok(())
}

/// Conflicts reported by §5 for files, directories and links.
pub fn preflight(store: &HistoryStore, roots: &Roots, manifest: &Manifest) -> Vec<FileConflict> {
    let mut conflicts = Vec::new();
    let later = later_entries(store, manifest).unwrap_or_default();

    for entry in &manifest.files {
        if !entry.restorable {
            continue;
        }
        if later.contains(&(entry.root, entry.path.clone())) {
            conflicts.push(conflict(entry.root, &entry.path, "written by a later run"));
            continue;
        }
        let Ok(path) = resolve(roots, entry.root, &entry.path) else {
            conflicts.push(conflict(entry.root, &entry.path, "path escapes its root"));
            continue;
        };
        let current = fs::symlink_metadata(&path).ok();
        match entry.op {
            FileOp::Modify => match current {
                None => conflicts.push(conflict(entry.root, &entry.path, "missing")),
                Some(metadata) if !metadata.file_type().is_file() => {
                    conflicts.push(conflict(entry.root, &entry.path, "not a regular file"))
                }
                Some(_) => {
                    if hash_of(&path) != entry.after.as_ref().map(|blob| blob.hash.clone()) {
                        conflicts.push(conflict(entry.root, &entry.path, "modified after the run"));
                    }
                }
            },
            FileOp::Create => {
                if let Some(metadata) = current {
                    if !metadata.file_type().is_file() {
                        conflicts.push(conflict(entry.root, &entry.path, "not a regular file"));
                    } else if hash_of(&path) != entry.after.as_ref().map(|blob| blob.hash.clone()) {
                        conflicts.push(conflict(entry.root, &entry.path, "modified after the run"));
                    }
                }
            }
            FileOp::Delete => {
                if let Some(metadata) = current {
                    if !metadata.file_type().is_file() {
                        conflicts.push(conflict(entry.root, &entry.path, "not a regular file"));
                    } else if entry
                        .before
                        .as_ref()
                        .map(|blob| blob.hash.clone())
                        .filter(|hash| !hash.is_empty())
                        .is_some_and(|hash| hash_of(&path) != Some(hash))
                    {
                        conflicts.push(conflict(entry.root, &entry.path, "changed after the run"));
                    }
                }
            }
            FileOp::Rename => {}
        }
    }

    for entry in &manifest.links {
        if later.contains(&(entry.root, entry.path.clone())) {
            conflicts.push(conflict(entry.root, &entry.path, "written by a later run"));
            continue;
        }
        let Ok(path) = resolve(roots, entry.root, &entry.path) else {
            conflicts.push(conflict(entry.root, &entry.path, "path escapes its root"));
            continue;
        };
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if fs::read_link(&path).ok().as_deref() != Some(Path::new(&entry.target)) {
                    conflicts.push(conflict(entry.root, &entry.path, "link target changed"));
                }
            }
            Ok(_) => conflicts.push(conflict(entry.root, &entry.path, "path is not a symlink")),
            Err(_) => {}
        }
    }

    for entry in &manifest.dirs {
        if later.contains(&(entry.root, entry.path.clone())) {
            conflicts.push(conflict(entry.root, &entry.path, "written by a later run"));
            continue;
        }
        let Ok(path) = resolve(roots, entry.root, &entry.path) else {
            conflicts.push(conflict(entry.root, &entry.path, "path escapes its root"));
            continue;
        };
        match entry.op {
            DirOp::Rmdir => {
                if path.is_file() {
                    conflicts.push(conflict(
                        entry.root,
                        &entry.path,
                        "a file occupies the path",
                    ));
                } else if path.exists() && !is_empty_dir(&path) {
                    conflicts.push(conflict(entry.root, &entry.path, "directory is not empty"));
                }
            }
            DirOp::Mkdir => {
                if path.exists() && (!path.is_dir() || !is_empty_dir(&path)) {
                    conflicts.push(conflict(entry.root, &entry.path, "directory is not empty"));
                }
            }
        }
    }
    conflicts
}

/// Entries from runs that started at or after this one, so undo never deletes
/// content a later run wrote.
fn later_entries(store: &HistoryStore, manifest: &Manifest) -> Result<HashSet<(RootKind, String)>> {
    let mut out = HashSet::new();
    for other in store.load_all()? {
        if other.run_id == manifest.run_id || other.started_at < manifest.started_at {
            continue;
        }
        out.extend(
            other
                .files
                .iter()
                .map(|entry| (entry.root, entry.path.clone())),
        );
        out.extend(
            other
                .dirs
                .iter()
                .map(|entry| (entry.root, entry.path.clone())),
        );
        out.extend(
            other
                .links
                .iter()
                .map(|entry| (entry.root, entry.path.clone())),
        );
    }
    Ok(out)
}

fn expected_skips(manifest: &Manifest) -> Vec<SkippedEntry> {
    manifest
        .files
        .iter()
        .filter(|entry| !entry.restorable)
        .map(|entry| SkippedEntry {
            path: entry.path.clone(),
            reason: entry
                .reason
                .map(|reason| reason.as_str().to_string())
                .unwrap_or_else(|| "unrestorable".to_string()),
        })
        .collect()
}

/// Rebuild derived state from the restored files: the wiki index and
/// `index.md`, and the documents source table.
fn reconcile(db: &Connection, roots: &Roots, manifest: &Manifest) -> Result<()> {
    let mut wiki = false;
    let mut documents = false;
    for entry in &manifest.files {
        match entry.root {
            RootKind::Wiki if entry.path != "index.md" => {
                wiki = true;
                let exists = roots.wiki.join(&entry.path).exists();
                if exists {
                    index::reindex_page(db, &roots.wiki, &entry.path)?;
                } else {
                    index::forget_page(db, &entry.path)?;
                }
            }
            RootKind::Documents => documents = true,
            _ => {}
        }
    }
    if wiki {
        index::write_index_md(db, &roots.wiki)?;
    }
    if documents {
        refresh_sources(db, &roots.documents)?;
    }
    Ok(())
}

fn orphan_projects(db: &Connection, manifest: &Manifest) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for entry in &manifest.files {
        if entry.root == RootKind::Projects {
            if let Some(id) = entry.path.split('/').next() {
                ids.insert(id.to_string());
            }
        }
    }
    ids.into_iter()
        .filter(|id| {
            db.query_row("SELECT 1 FROM projects WHERE id = ?", [id], |_| Ok(()))
                .is_err()
        })
        .collect()
}

fn resolve(roots: &Roots, root: RootKind, rel: &str) -> Result<PathBuf> {
    let root = roots.path(root);
    fs::create_dir_all(root).ok();
    fsutil::guard(root, &root.join(rel))
}

fn conflict(root: RootKind, path: &str, reason: &str) -> FileConflict {
    FileConflict {
        root,
        path: path.to_string(),
        reason: reason.to_string(),
    }
}

impl FileUndoReport {
    fn skip(&mut self, path: &str, reason: String) {
        self.skipped.push(SkippedEntry {
            path: path.to_string(),
            reason,
        });
    }
}

fn depth(path: &str) -> usize {
    path.matches('/').count()
}

fn is_empty_dir(path: &Path) -> bool {
    fs::read_dir(path)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false)
}

fn hash_of(path: &Path) -> Option<String> {
    hash_file(path).ok()
}

fn set_mode(path: &Path, mode: u32) {
    if mode == 0 {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_schema;
    use crate::db::open_db;
    use crate::history::{
        BlobMeta, DirEntry, DirOp, FileEntry, FileOp, LinkEntry, Manifest, RunKind,
        UnrestorableReason,
    };

    struct Fixture {
        _tmp: tempfile::TempDir,
        db: Connection,
        roots: Roots,
        store: HistoryStore,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.db");
        let mut db = open_db(&path).unwrap();
        run_schema(&mut db).unwrap();
        let roots = Roots {
            wiki: tmp.path().join("wiki"),
            notes: tmp.path().join("notes"),
            documents: tmp.path().join("documents"),
            projects: tmp.path().join("projects"),
        };
        for root in [&roots.wiki, &roots.notes, &roots.documents, &roots.projects] {
            fs::create_dir_all(root).unwrap();
        }
        let store = HistoryStore::new(tmp.path().join("history"));
        Fixture {
            _tmp: tmp,
            db,
            roots,
            store,
        }
    }

    fn manifest() -> Manifest {
        Manifest::new("run1", RunKind::Explicit, "test", 1)
    }

    fn blob(store: &HistoryStore, bytes: &[u8]) -> BlobMeta {
        BlobMeta {
            hash: store.store_object(bytes).unwrap(),
            mode: 0o644,
            size: bytes.len() as u64,
        }
    }

    fn file_entry(
        store: &HistoryStore,
        seq: i64,
        root: RootKind,
        path: &str,
        op: FileOp,
        before: Option<&[u8]>,
        after: Option<&[u8]>,
    ) -> FileEntry {
        FileEntry {
            seq,
            root,
            path: path.to_string(),
            op,
            before: before.map(|bytes| blob(store, bytes)),
            after: after.map(|bytes| blob(store, bytes)),
            captured_at: 1,
            oversize: false,
            restorable: true,
            reason: None,
            sidecar: false,
        }
    }

    #[test]
    fn restores_modify_create_and_delete() {
        let f = fixture();
        fs::write(f.roots.notes.join("modify.md"), "new").unwrap();
        fs::write(f.roots.notes.join("created.md"), "new").unwrap();

        let mut m = manifest();
        m.files = vec![
            file_entry(
                &f.store,
                1,
                RootKind::Notes,
                "modify.md",
                FileOp::Modify,
                Some(b"old"),
                Some(b"new"),
            ),
            file_entry(
                &f.store,
                2,
                RootKind::Notes,
                "created.md",
                FileOp::Create,
                None,
                Some(b"new"),
            ),
            file_entry(
                &f.store,
                3,
                RootKind::Notes,
                "deleted.md",
                FileOp::Delete,
                Some(b"old"),
                None,
            ),
        ];

        let report = restore_files(&f.db, &f.store, &f.roots, &m, false).unwrap();
        assert!(!report.blocked);
        assert_eq!(report.restored, 2);
        assert_eq!(report.deleted, 1);
        assert!(report.skipped.is_empty());
        assert_eq!(
            fs::read_to_string(f.roots.notes.join("modify.md")).unwrap(),
            "old"
        );
        assert!(!f.roots.notes.join("created.md").exists());
        assert_eq!(
            fs::read_to_string(f.roots.notes.join("deleted.md")).unwrap(),
            "old"
        );
    }

    #[test]
    fn recreates_directories_and_links_and_removes_created_dirs() {
        let f = fixture();
        fs::create_dir_all(f.roots.projects.join("scratch")).unwrap();

        let mut m = manifest();
        m.dirs = vec![
            DirEntry {
                seq: 1,
                root: RootKind::Projects,
                path: "p1".into(),
                op: DirOp::Rmdir,
                mode: 0o755,
                empty: false,
                captured_at: 1,
            },
            DirEntry {
                seq: 2,
                root: RootKind::Projects,
                path: "p1/papers".into(),
                op: DirOp::Rmdir,
                mode: 0o755,
                empty: false,
                captured_at: 1,
            },
            DirEntry {
                seq: 3,
                root: RootKind::Projects,
                path: "p1/papers/empty".into(),
                op: DirOp::Rmdir,
                mode: 0o755,
                empty: true,
                captured_at: 1,
            },
            DirEntry {
                seq: 4,
                root: RootKind::Projects,
                path: "scratch".into(),
                op: DirOp::Mkdir,
                mode: 0o755,
                empty: true,
                captured_at: 1,
            },
        ];
        m.files.push(file_entry(
            &f.store,
            5,
            RootKind::Projects,
            "p1/papers/a.md",
            FileOp::Delete,
            Some(b"a"),
            None,
        ));
        m.links.push(LinkEntry {
            seq: 6,
            root: RootKind::Projects,
            path: "p1/current".into(),
            target: "papers".into(),
            captured_at: 1,
        });

        let report = restore_files(&f.db, &f.store, &f.roots, &m, false).unwrap();
        assert!(!report.blocked, "{report:?}");
        assert_eq!(report.dirs_recreated, 3);
        assert_eq!(report.dirs_removed, 1);
        assert!(f.roots.projects.join("p1/papers/empty").is_dir());
        assert_eq!(
            fs::read_to_string(f.roots.projects.join("p1/papers/a.md")).unwrap(),
            "a"
        );
        assert_eq!(
            fs::read_link(f.roots.projects.join("p1/current")).unwrap(),
            Path::new("papers")
        );
        assert!(!f.roots.projects.join("scratch").exists());
        // No project row: the restored folder is reported, not failed.
        assert_eq!(report.orphans, vec!["p1".to_string()]);
    }

    #[test]
    fn conflicts_block_without_force_and_force_skips_them() {
        let f = fixture();
        fs::write(f.roots.notes.join("page.md"), "user edit").unwrap();

        let mut m = manifest();
        m.files.push(file_entry(
            &f.store,
            1,
            RootKind::Notes,
            "page.md",
            FileOp::Modify,
            Some(b"old"),
            Some(b"new"),
        ));

        let blocked = restore_files(&f.db, &f.store, &f.roots, &m, false).unwrap();
        assert!(blocked.blocked);
        assert_eq!(blocked.conflicts.len(), 1);
        assert_eq!(blocked.restored, 0);
        assert_eq!(
            fs::read_to_string(f.roots.notes.join("page.md")).unwrap(),
            "user edit"
        );

        let forced = restore_files(&f.db, &f.store, &f.roots, &m, true).unwrap();
        assert!(!forced.blocked);
        assert_eq!(forced.restored, 0);
        assert_eq!(forced.conflicts.len(), 1);
        assert_eq!(forced.skipped.len(), 1);
        assert!(forced.partial());
        // Force never overwrites a conflicted file.
        assert_eq!(
            fs::read_to_string(f.roots.notes.join("page.md")).unwrap(),
            "user edit"
        );
    }

    #[test]
    fn unrestorable_entries_are_reported_while_others_restore() {
        let f = fixture();
        let mut m = manifest();
        let mut oversize = file_entry(
            &f.store,
            1,
            RootKind::Notes,
            "huge.bin",
            FileOp::Delete,
            None,
            None,
        );
        oversize.before = Some(BlobMeta {
            hash: String::new(),
            mode: 0o644,
            size: 999,
        });
        oversize.restorable = false;
        oversize.reason = Some(UnrestorableReason::Oversize);
        m.files = vec![
            oversize,
            file_entry(
                &f.store,
                2,
                RootKind::Notes,
                "ok.md",
                FileOp::Delete,
                Some(b"ok"),
                None,
            ),
        ];

        let report = restore_files(&f.db, &f.store, &f.roots, &m, false).unwrap();
        assert!(!report.blocked);
        assert_eq!(report.restored, 1);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].path, "huge.bin");
        assert_eq!(report.skipped[0].reason, "oversize");
        assert!(report.partial());
        assert_eq!(
            fs::read_to_string(f.roots.notes.join("ok.md")).unwrap(),
            "ok"
        );
    }

    #[test]
    fn a_later_run_on_the_same_path_conflicts() {
        let f = fixture();
        fs::write(f.roots.notes.join("page.md"), "new").unwrap();
        let mut m = manifest();
        m.files.push(file_entry(
            &f.store,
            1,
            RootKind::Notes,
            "page.md",
            FileOp::Modify,
            Some(b"old"),
            Some(b"new"),
        ));
        f.store.save(&m).unwrap();

        let mut later = Manifest::new("run2", RunKind::Implicit, "later", 2);
        later.files.push(file_entry(
            &f.store,
            1,
            RootKind::Notes,
            "page.md",
            FileOp::Modify,
            Some(b"new"),
            Some(b"later"),
        ));
        f.store.save(&later).unwrap();

        let report = restore_files(&f.db, &f.store, &f.roots, &m, false).unwrap();
        assert!(report.blocked);
        assert!(report.conflicts[0].reason.contains("later run"));
        assert_eq!(
            fs::read_to_string(f.roots.notes.join("page.md")).unwrap(),
            "new"
        );
    }

    #[test]
    fn wiki_restore_keeps_reviewed_and_regenerates_index() {
        let f = fixture();
        let page_path = f.roots.wiki.join("concepts/attention.md");
        fs::create_dir_all(page_path.parent().unwrap()).unwrap();
        let before = "---\ntype: concept\ntitle: Attention\nsummary: A page.\n\
                      aliases: [Attention]\nreviewed: true\n---\n\
                      A body long enough to avoid the empty-body warning.\n";
        let after = before.replace("reviewed: true", "reviewed: false");
        fs::write(&page_path, &after).unwrap();
        crate::wiki::index::reindex_page(&f.db, &f.roots.wiki, "concepts/attention.md").unwrap();

        let mut m = manifest();
        m.files.push(file_entry(
            &f.store,
            1,
            RootKind::Wiki,
            "concepts/attention.md",
            FileOp::Modify,
            Some(before.as_bytes()),
            Some(after.as_bytes()),
        ));

        let report = restore_files(&f.db, &f.store, &f.roots, &m, false).unwrap();
        assert!(!report.blocked);
        assert_eq!(fs::read_to_string(&page_path).unwrap(), before);
        let reviewed: i64 =
            f.db.query_row(
                "SELECT reviewed FROM wiki_pages WHERE rel_path = 'concepts/attention.md'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reviewed, 1);
        let index = fs::read_to_string(f.roots.wiki.join("index.md")).unwrap();
        assert!(index.contains("Attention"), "{index}");
    }
}
