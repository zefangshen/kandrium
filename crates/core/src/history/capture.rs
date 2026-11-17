//! Capture guards: record a path's pre-image before a mutation and its
//! after-hash once the mutation completed.
//!
//! ```ignore
//! let guard = ctx.capture(RootKind::Wiki, &wiki_root, &target)?; // pre-image
//! // ... mutate the file ...
//! guard.commit(Some(new_bytes))?;                               // after hash
//! ```
//!
//! The pre-image is written to the object store before the manifest is saved,
//! so a crash after the mutation still leaves a restorable manifest entry (and
//! at worst an orphan object that GC collects).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::{KandriumError, Result};
use crate::fsutil;
use crate::wiki::{hash_bytes, hash_file};

use super::store::HistoryStore;
use super::{
    BlobMeta, DirEntry, DirOp, FileEntry, FileOp, LinkEntry, MAX_FILE_BYTES, Manifest, RootKind,
    RunKind, UnrestorableReason,
};

/// Manifest rewrites are serialized in-process. Cross-process exclusion comes
/// from the single-open-run rule: a run has one owner at a time.
static MANIFEST_LOCK: Mutex<()> = Mutex::new(());

/// One run being captured, bound to its history store.
#[derive(Debug, Clone)]
pub struct RunContext {
    store: HistoryStore,
    run_id: String,
}

impl RunContext {
    pub fn new(store: HistoryStore, run_id: impl Into<String>) -> Self {
        Self {
            store,
            run_id: run_id.into(),
        }
    }

    /// Context for a run recorded beside the connection's database file.
    ///
    /// Returns `None` for an in-memory database: there is nowhere persistent to
    /// put the history, and no user file can be restored from it anyway.
    pub fn for_connection(db: &rusqlite::Connection, run_id: &str) -> Option<Self> {
        let path = db.path()?;
        if path.is_empty() {
            return None;
        }
        Some(Self::new(HistoryStore::for_db(Path::new(path)), run_id))
    }

    /// Same as [`Self::for_connection`], for the optional run id carried by a
    /// write spec.
    pub fn from_run_id(db: &rusqlite::Connection, run_id: &Option<String>) -> Option<Self> {
        Self::for_connection(db, run_id.as_deref()?)
    }

    pub fn store(&self) -> &HistoryStore {
        &self.store
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Capture a regular file before mutating it.
    ///
    /// A missing file yields a `create` entry with no pre-image. A file above
    /// [`MAX_FILE_BYTES`] is recorded as `oversize` and not stored, so the run
    /// knows exactly what it cannot restore.
    pub fn capture(&self, root: RootKind, root_path: &Path, path: &Path) -> Result<CaptureGuard> {
        fs::create_dir_all(root_path)?;
        let canonical_root = fsutil::canonical_root(root_path)?;
        let absolute = fsutil::guard(&canonical_root, path)?;
        let rel_path = relative_path(&canonical_root, &absolute)?;
        let captured_at = now();

        let (before, oversize, restorable, reason) = pre_image(&self.store, &absolute)?;
        let before_hash = before.as_ref().map(|blob| blob.hash.clone());
        let op = if before.is_some() {
            FileOp::Modify
        } else {
            FileOp::Create
        };

        let seq = self.update_run(|manifest| {
            let seq = next_seq(manifest);
            manifest.files.push(FileEntry {
                seq,
                root,
                path: rel_path.clone(),
                op,
                before: before.clone(),
                after: None,
                captured_at,
                oversize,
                restorable,
                reason,
                sidecar: false,
            });
            Ok(seq)
        })?;

        Ok(CaptureGuard {
            store: self.store.clone(),
            run_id: self.run_id.clone(),
            seq,
            absolute,
            rel_path,
            before_hash,
            before_present: before.is_some(),
        })
    }

    /// Enumerate a directory tree and record it as a deletion before it is
    /// removed: a pre-image per regular file, an entry per directory (including
    /// empty ones), and a target per symlink. The walk runs to completion
    /// before anything is deleted, so an interrupted deletion still has a
    /// complete manifest.
    pub fn capture_tree(
        &self,
        root: RootKind,
        root_path: &Path,
        dir: &Path,
    ) -> Result<TreeCapture> {
        let canonical_root = fsutil::canonical_root(root_path)?;
        let dir = fsutil::guard(&canonical_root, dir)?;
        if !dir.is_dir() {
            return Err(KandriumError(format!("not a directory: {}", dir.display())));
        }
        let walked = walk(&canonical_root, &dir)?;
        let captured_at = now();
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        let mut links = Vec::new();
        let mut restorable = true;

        for item in walked {
            match item {
                Walked::Dir { rel, mode, empty } => dirs.push((rel, mode, empty)),
                Walked::Link { rel, target } => links.push((rel, target)),
                Walked::File {
                    rel,
                    absolute,
                    mode,
                    size,
                    kind,
                } => {
                    let (hash, ok, reason) = match kind {
                        WalkedFileKind::Regular if size <= MAX_FILE_BYTES => {
                            match fs::read(&absolute) {
                                Ok(bytes) => (self.store.store_object(&bytes)?, true, None),
                                Err(error) => {
                                    crate::logging::warn!(
                                        "cannot read {} while capturing: {error}",
                                        absolute.display()
                                    );
                                    (String::new(), false, Some(UnrestorableReason::Unreadable))
                                }
                            }
                        }
                        WalkedFileKind::Regular => {
                            (String::new(), false, Some(UnrestorableReason::Oversize))
                        }
                        WalkedFileKind::Special => {
                            (String::new(), false, Some(UnrestorableReason::Special))
                        }
                    };
                    restorable &= ok;
                    files.push((
                        rel,
                        BlobMeta { hash, mode, size },
                        size > MAX_FILE_BYTES,
                        ok,
                        reason,
                    ));
                }
            }
        }

        let (files_len, dirs_len, links_len) = (files.len(), dirs.len(), links.len());
        self.update_run(|manifest| {
            for (rel, mode, empty) in dirs {
                let seq = next_seq(manifest);
                manifest.dirs.push(DirEntry {
                    seq,
                    root,
                    path: rel,
                    op: DirOp::Rmdir,
                    mode,
                    empty,
                    captured_at,
                });
            }
            for (rel, before, oversize, ok, reason) in files {
                let seq = next_seq(manifest);
                manifest.files.push(FileEntry {
                    seq,
                    root,
                    path: rel,
                    op: FileOp::Delete,
                    before: Some(before),
                    after: None,
                    captured_at,
                    oversize,
                    restorable: ok,
                    reason,
                    sidecar: false,
                });
            }
            for (rel, target) in links {
                let seq = next_seq(manifest);
                manifest.links.push(LinkEntry {
                    seq,
                    root,
                    path: rel,
                    target,
                    captured_at,
                });
            }
            Ok(())
        })?;

        Ok(TreeCapture {
            files: files_len,
            dirs: dirs_len,
            links: links_len,
            restorable,
        })
    }

    /// Record a newly created file that already exists on disk (for example
    /// the destination of an import).
    pub fn record_created(&self, root: RootKind, root_path: &Path, path: &Path) -> Result<()> {
        let canonical_root = fsutil::canonical_root(root_path)?;
        let path = fsutil::guard(&canonical_root, path)?;
        if !path.is_file() {
            return Err(KandriumError(format!(
                "not a regular file: {}",
                path.display()
            )));
        }
        self.push_created_file(root, &canonical_root, &path, now())
    }

    /// Record a newly created tree (for example a renamed directory) as
    /// `mkdir` directories plus `create` files and links.
    pub fn record_tree(&self, root: RootKind, root_path: &Path, dir: &Path) -> Result<TreeCapture> {
        let canonical_root = fsutil::canonical_root(root_path)?;
        let dir = fsutil::guard(&canonical_root, dir)?;
        if !dir.is_dir() {
            return Err(KandriumError(format!("not a directory: {}", dir.display())));
        }
        let walked = walk(&canonical_root, &dir)?;
        let captured_at = now();
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        let mut links = Vec::new();
        let mut restorable = true;

        for item in walked {
            match item {
                Walked::Dir { rel, mode, empty } => dirs.push((rel, mode, empty)),
                Walked::Link { rel, target } => links.push((rel, target)),
                Walked::File {
                    rel,
                    absolute,
                    mode,
                    size,
                    kind,
                } => {
                    let (hash, ok, reason) = match kind {
                        WalkedFileKind::Regular => match hash_file(&absolute) {
                            Ok(hash) => (hash, true, None),
                            Err(error) => {
                                crate::logging::warn!(
                                    "cannot hash {} while recording: {error}",
                                    absolute.display()
                                );
                                (String::new(), false, Some(UnrestorableReason::Unreadable))
                            }
                        },
                        WalkedFileKind::Special => {
                            (String::new(), false, Some(UnrestorableReason::Special))
                        }
                    };
                    restorable &= ok;
                    files.push((rel, BlobMeta { hash, mode, size }, ok, reason));
                }
            }
        }

        let (files_len, dirs_len, links_len) = (files.len(), dirs.len(), links.len());
        self.update_run(|manifest| {
            for (rel, mode, empty) in dirs {
                let seq = next_seq(manifest);
                manifest.dirs.push(DirEntry {
                    seq,
                    root,
                    path: rel,
                    op: DirOp::Mkdir,
                    mode,
                    empty,
                    captured_at,
                });
            }
            for (rel, after, ok, reason) in files {
                let seq = next_seq(manifest);
                manifest.files.push(FileEntry {
                    seq,
                    root,
                    path: rel,
                    op: FileOp::Create,
                    before: None,
                    after: Some(after),
                    captured_at,
                    oversize: false,
                    restorable: ok,
                    reason,
                    sidecar: false,
                });
            }
            for (rel, target) in links {
                let seq = next_seq(manifest);
                manifest.links.push(LinkEntry {
                    seq,
                    root,
                    path: rel,
                    target,
                    captured_at,
                });
            }
            Ok(())
        })?;

        Ok(TreeCapture {
            files: files_len,
            dirs: dirs_len,
            links: links_len,
            restorable,
        })
    }

    fn push_created_file(
        &self,
        root: RootKind,
        canonical_root: &Path,
        path: &Path,
        captured_at: i64,
    ) -> Result<()> {
        let rel = relative_path(canonical_root, path)?;
        let metadata = fs::metadata(path)?;
        let after = BlobMeta {
            hash: hash_file(path)?,
            mode: mode_of(path),
            size: metadata.len(),
        };
        self.update_run(|manifest| {
            let seq = next_seq(manifest);
            manifest.files.push(FileEntry {
                seq,
                root,
                path: rel,
                op: FileOp::Create,
                before: None,
                after: Some(after),
                captured_at,
                oversize: false,
                restorable: true,
                reason: None,
                sidecar: false,
            });
            Ok(())
        })
    }

    /// Load-or-create the manifest and apply `change` under the in-process
    /// manifest lock, then save it atomically.
    fn update_run<T>(&self, change: impl FnOnce(&mut Manifest) -> Result<T>) -> Result<T> {
        let _lock = MANIFEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut manifest = match self.store.load(&self.run_id)? {
            Some(manifest) => manifest,
            None => Manifest::new(
                self.run_id.clone(),
                RunKind::Implicit,
                self.run_id.clone(),
                now(),
            ),
        };
        manifest.heartbeat_at = now();
        let out = change(&mut manifest)?;
        self.store.save(&manifest)?;
        Ok(out)
    }
}

/// A captured pre-image waiting for its after-image.
#[derive(Debug)]
pub struct CaptureGuard {
    store: HistoryStore,
    run_id: String,
    seq: i64,
    absolute: PathBuf,
    rel_path: String,
    before_hash: Option<String>,
    before_present: bool,
}

impl CaptureGuard {
    pub fn seq(&self) -> i64 {
        self.seq
    }

    pub fn before_hash(&self) -> Option<&str> {
        self.before_hash.as_deref()
    }

    /// Relative path as recorded in the manifest.
    pub fn path(&self) -> &str {
        &self.rel_path
    }

    /// Record the after-image. `None` means the mutation removed the file.
    ///
    /// A byte-identical write is dropped from the manifest: it changed nothing
    /// and does not belong in history.
    pub fn commit(self, new_bytes: Option<&[u8]>) -> Result<()> {
        let after = new_bytes.map(|bytes| BlobMeta {
            hash: hash_bytes(bytes),
            mode: mode_of(&self.absolute),
            size: bytes.len() as u64,
        });
        self.finish(after)
    }

    /// Record the after-image of a file the run created by copying or renaming,
    /// without loading it into memory.
    pub fn commit_file(self) -> Result<()> {
        let metadata = fs::metadata(&self.absolute)?;
        let after = Some(BlobMeta {
            hash: hash_file(&self.absolute)?,
            mode: mode_of(&self.absolute),
            size: metadata.len(),
        });
        self.finish(after)
    }

    fn finish(self, after: Option<BlobMeta>) -> Result<()> {
        let _lock = MANIFEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some(mut manifest) = self.store.load(&self.run_id)? else {
            return Err(KandriumError(format!(
                "run {} has no manifest",
                self.run_id
            )));
        };
        manifest.heartbeat_at = now();
        let index = manifest
            .files
            .iter()
            .position(|entry| entry.seq == self.seq)
            .ok_or_else(|| {
                KandriumError(format!(
                    "run {} lost entry {} for {}",
                    self.run_id, self.seq, self.rel_path
                ))
            })?;

        match after {
            Some(after) => {
                if self.before_hash.as_deref() == Some(after.hash.as_str()) {
                    manifest.files.remove(index);
                } else {
                    manifest.files[index].after = Some(after);
                }
            }
            None if self.before_present => {
                manifest.files[index].op = FileOp::Delete;
                manifest.files[index].after = None;
            }
            None => {
                // Nothing existed and nothing was created: no history entry.
                manifest.files.remove(index);
            }
        }
        self.store.save(&manifest)?;
        Ok(())
    }
}

/// Read a file's pre-image and store its bytes, or classify why it cannot be
/// restored.
fn pre_image(
    store: &HistoryStore,
    path: &Path,
) -> Result<(Option<BlobMeta>, bool, bool, Option<UnrestorableReason>)> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, false, true, None));
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Err(KandriumError(format!(
            "not a regular file: {}",
            path.display()
        )));
    }

    let mode = mode_of(path);
    let size = metadata.len();
    if size > MAX_FILE_BYTES {
        return Ok((
            Some(BlobMeta {
                hash: String::new(),
                mode,
                size,
            }),
            true,
            false,
            Some(UnrestorableReason::Oversize),
        ));
    }

    let bytes = fs::read(path)?;
    let hash = store.store_object(&bytes)?;
    Ok((Some(BlobMeta { hash, mode, size }), false, true, None))
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| KandriumError(format!("{} is outside {}", path.display(), root.display())))?;
    let mut parts = Vec::new();
    for component in rel.components() {
        let part = component
            .as_os_str()
            .to_str()
            .ok_or_else(|| KandriumError("path is not valid UTF-8".to_string()))?;
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(KandriumError("cannot capture the root itself".to_string()));
    }
    Ok(parts.join("/"))
}

fn mode_of(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o777)
            .unwrap_or(0o644)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0o644
    }
}

fn next_seq(manifest: &Manifest) -> i64 {
    let last = manifest
        .files
        .iter()
        .map(|entry| entry.seq)
        .chain(manifest.dirs.iter().map(|entry| entry.seq))
        .chain(manifest.links.iter().map(|entry| entry.seq))
        .max()
        .unwrap_or(0);
    last + 1
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// What [`RunContext::capture_tree`] and [`RunContext::record_tree`] recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeCapture {
    pub files: usize,
    pub dirs: usize,
    pub links: usize,
    /// False when at least one entry is unrestorable (oversize, unreadable,
    /// special), which is the only thing that makes the run `partial`.
    pub restorable: bool,
}

enum Walked {
    Dir {
        rel: String,
        mode: u32,
        empty: bool,
    },
    File {
        rel: String,
        absolute: PathBuf,
        mode: u32,
        size: u64,
        kind: WalkedFileKind,
    },
    Link {
        rel: String,
        target: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WalkedFileKind {
    Regular,
    /// FIFO, socket, device, or anything else that is not a regular file.
    Special,
}

/// Enumerate a tree without following symlinks. Directories come before their
/// children; entries within a directory are sorted by name for stable output.
/// An unreadable directory aborts the walk so nothing gets deleted that could
/// not be captured.
fn walk(root: &Path, dir: &Path) -> Result<Vec<Walked>> {
    let mut out = Vec::new();
    walk_dir(root, dir, &mut out)?;
    Ok(out)
}

fn walk_dir(root: &Path, dir: &Path, out: &mut Vec<Walked>) -> Result<()> {
    let mut entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    out.push(Walked::Dir {
        rel: relative_path(root, dir)?,
        mode: mode_of(dir),
        empty: entries.is_empty(),
    });
    for entry in entries {
        let path = entry.path();
        let rel = relative_path(root, &path)?;
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                // A vanished or unreadable entry is recorded as unrestorable
                // instead of aborting the whole capture.
                crate::logging::warn!("cannot inspect {}: {error}", path.display());
                out.push(Walked::File {
                    rel,
                    absolute: path,
                    mode: 0,
                    size: 0,
                    kind: WalkedFileKind::Special,
                });
                continue;
            }
        };
        if file_type.is_symlink() {
            let target = fs::read_link(&path)?;
            out.push(Walked::Link {
                rel,
                target: target.to_string_lossy().into_owned(),
            });
        } else if file_type.is_dir() {
            walk_dir(root, &path, out)?;
        } else {
            let (size, kind) = match entry.metadata() {
                Ok(metadata) if file_type.is_file() => (metadata.len(), WalkedFileKind::Regular),
                _ => (0, WalkedFileKind::Special),
            };
            out.push(Walked::File {
                rel,
                absolute: path,
                mode: mode_of(&entry.path()),
                size,
                kind,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, RunContext, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("wiki");
        fs::create_dir_all(&root).unwrap();
        let store = HistoryStore::new(dir.path().join("history"));
        (dir, RunContext::new(store, "run1"), root)
    }

    fn entries(ctx: &RunContext) -> Vec<FileEntry> {
        ctx.store()
            .load(ctx.run_id())
            .unwrap()
            .expect("manifest")
            .files
    }

    #[test]
    fn capture_records_a_create_without_a_pre_image() {
        let (_dir, ctx, root) = fixture();
        let target = root.join("concepts").join("attention.md");
        fs::create_dir_all(target.parent().unwrap()).unwrap();

        let guard = ctx.capture(RootKind::Wiki, &root, &target).unwrap();
        guard.commit(Some(b"# Attention\n")).unwrap();

        let entries = entries(&ctx);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.op, FileOp::Create);
        assert_eq!(entry.path, "concepts/attention.md");
        assert!(entry.before.is_none());
        assert_eq!(
            entry.after.as_ref().unwrap().hash,
            hash_bytes(b"# Attention\n")
        );
        // Only pre-images become objects; a create has none.
        assert!(!ctx.store().has_object(&entry.after.as_ref().unwrap().hash));
    }

    #[test]
    fn capture_records_a_modify_with_a_readable_pre_image() {
        let (_dir, ctx, root) = fixture();
        let target = root.join("page.md");
        fs::write(&target, "old").unwrap();

        let guard = ctx.capture(RootKind::Wiki, &root, &target).unwrap();
        guard.commit(Some(b"new")).unwrap();

        let entry = &entries(&ctx)[0];
        assert_eq!(entry.op, FileOp::Modify);
        let before = entry.before.as_ref().unwrap();
        assert_eq!(before.hash, hash_bytes(b"old"));
        assert_eq!(ctx.store().read_object(&before.hash).unwrap(), b"old");
        assert_eq!(entry.after.as_ref().unwrap().hash, hash_bytes(b"new"));
    }

    #[test]
    fn capture_records_a_delete_when_commit_passes_none() {
        let (_dir, ctx, root) = fixture();
        let target = root.join("gone.md");
        fs::write(&target, "bye").unwrap();

        let guard = ctx.capture(RootKind::Wiki, &root, &target).unwrap();
        fs::remove_file(&target).unwrap();
        guard.commit(None).unwrap();

        let entry = &entries(&ctx)[0];
        assert_eq!(entry.op, FileOp::Delete);
        assert!(entry.after.is_none());
        assert_eq!(entry.before.as_ref().unwrap().hash, hash_bytes(b"bye"));
    }

    #[test]
    fn byte_identical_writes_are_dropped() {
        let (_dir, ctx, root) = fixture();
        let target = root.join("same.md");
        fs::write(&target, "same").unwrap();

        let guard = ctx.capture(RootKind::Wiki, &root, &target).unwrap();
        guard.commit(Some(b"same")).unwrap();

        assert!(entries(&ctx).is_empty());
    }

    #[test]
    fn a_delete_of_a_missing_file_is_dropped() {
        let (_dir, ctx, root) = fixture();
        let target = root.join("never.md");

        let guard = ctx.capture(RootKind::Wiki, &root, &target).unwrap();
        guard.commit(None).unwrap();

        assert!(entries(&ctx).is_empty());
    }

    #[test]
    fn entries_get_increasing_sequence_numbers() {
        let (_dir, ctx, root) = fixture();
        for name in ["a.md", "b.md"] {
            let target = root.join(name);
            let guard = ctx.capture(RootKind::Wiki, &root, &target).unwrap();
            guard.commit(Some(name.as_bytes())).unwrap();
        }
        let entries = entries(&ctx);
        assert_eq!(entries[0].seq, 1);
        assert_eq!(entries[1].seq, 2);
    }

    #[test]
    fn identical_pre_images_share_one_object() {
        let (_dir, ctx, root) = fixture();
        for name in ["a.md", "b.md"] {
            let target = root.join(name);
            fs::write(&target, "shared").unwrap();
            let guard = ctx.capture(RootKind::Wiki, &root, &target).unwrap();
            guard.commit(Some(name.as_bytes())).unwrap();
        }
        let entries = entries(&ctx);
        assert_eq!(
            entries[0].before.as_ref().unwrap().hash,
            entries[1].before.as_ref().unwrap().hash
        );
        assert_eq!(ctx.store().gc_orphan_objects().unwrap(), 0);

        let objects: Vec<_> = fs::read_dir(ctx.store().objects_dir())
            .unwrap()
            .flatten()
            .flat_map(|shard| fs::read_dir(shard.path()).unwrap().flatten())
            .collect();
        assert_eq!(objects.len(), 1);
    }

    #[test]
    fn paths_outside_the_root_are_rejected() {
        let (dir, ctx, root) = fixture();
        let outside = dir.path().join("outside.md");
        fs::write(&outside, "x").unwrap();
        assert!(ctx.capture(RootKind::Wiki, &root, &outside).is_err());
    }

    #[test]
    fn capture_tree_records_files_dirs_and_links_before_removal() {
        use std::io::Write;

        let (_dir, ctx, root) = fixture();
        let papers = root.join("papers");
        fs::create_dir_all(papers.join("2026")).unwrap();
        fs::create_dir_all(papers.join("empty")).unwrap();
        fs::write(papers.join("2026").join("a.md"), "paper").unwrap();
        let mut huge = fs::File::create(papers.join("huge.bin")).unwrap();
        huge.set_len(MAX_FILE_BYTES + 1).unwrap();
        huge.write_all(b"x").unwrap();
        std::os::unix::fs::symlink("2026", papers.join("current")).unwrap();

        let captured = ctx
            .capture_tree(RootKind::Projects, &root, &papers)
            .unwrap();
        assert_eq!(
            captured,
            TreeCapture {
                files: 2,
                dirs: 3,
                links: 1,
                restorable: false,
            }
        );

        // The manifest is complete while the tree still exists.
        let manifest = ctx.store().load(ctx.run_id()).unwrap().unwrap();
        let dirs: Vec<(&str, bool)> = manifest
            .dirs
            .iter()
            .map(|entry| (entry.path.as_str(), entry.empty))
            .collect();
        assert_eq!(
            dirs,
            vec![
                ("papers", false),
                ("papers/2026", false),
                ("papers/empty", true)
            ]
        );
        assert!(manifest.dirs.iter().all(|entry| entry.op == DirOp::Rmdir));

        let file = manifest
            .files
            .iter()
            .find(|entry| entry.path.ends_with("a.md"))
            .unwrap();
        assert_eq!(file.op, FileOp::Delete);
        assert_eq!(
            ctx.store()
                .read_object(&file.before.as_ref().unwrap().hash)
                .unwrap(),
            b"paper"
        );

        let oversize = manifest
            .files
            .iter()
            .find(|entry| entry.path.ends_with("huge.bin"))
            .unwrap();
        assert!(oversize.oversize);
        assert!(!oversize.restorable);
        assert_eq!(oversize.reason, Some(UnrestorableReason::Oversize));

        let link = &manifest.links[0];
        assert_eq!(link.path, "papers/current");
        assert_eq!(link.target, "2026");

        fs::remove_dir_all(&papers).unwrap();
        assert!(ctx.store().load(ctx.run_id()).unwrap().is_some());
    }

    #[test]
    fn record_tree_marks_new_directories_and_files() {
        let (_dir, ctx, root) = fixture();
        let renamed = root.join("renamed");
        fs::create_dir_all(renamed.join("inner")).unwrap();
        fs::write(renamed.join("inner").join("b.md"), "new").unwrap();

        let recorded = ctx.record_tree(RootKind::Notes, &root, &renamed).unwrap();
        assert_eq!(
            recorded,
            TreeCapture {
                files: 1,
                dirs: 2,
                links: 0,
                restorable: true,
            }
        );
        let manifest = ctx.store().load(ctx.run_id()).unwrap().unwrap();
        assert!(manifest.dirs.iter().all(|entry| entry.op == DirOp::Mkdir));
        let file = &manifest.files[0];
        assert_eq!(file.op, FileOp::Create);
        assert!(file.before.is_none());
        assert_eq!(file.after.as_ref().unwrap().hash, hash_bytes(b"new"));
    }

    #[test]
    fn record_created_and_commit_file_hash_without_loading_bytes() {
        let (_dir, ctx, root) = fixture();
        let target = root.join("imported.pdf");
        let guard = ctx.capture(RootKind::Documents, &root, &target).unwrap();
        fs::write(&target, b"%PDF-1.4 fake").unwrap();
        assert!(guard.before_hash().is_none());
        guard.commit_file().unwrap();
        let entry = &entries(&ctx)[0];
        assert_eq!(entry.op, FileOp::Create);
        assert_eq!(
            entry.after.as_ref().unwrap().hash,
            hash_bytes(b"%PDF-1.4 fake")
        );

        let second = root.join("renamed.pdf");
        fs::rename(&target, &second).unwrap();
        ctx.record_created(RootKind::Documents, &root, &second)
            .unwrap();
        assert_eq!(entries(&ctx).len(), 2);
    }
}
