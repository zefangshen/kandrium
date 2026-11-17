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
use crate::wiki::hash_bytes;

use super::store::HistoryStore;
use super::{
    BlobMeta, FileEntry, FileOp, MAX_FILE_BYTES, Manifest, RootKind, RunKind, UnrestorableReason,
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

        match new_bytes {
            Some(bytes) => {
                let after_hash = hash_bytes(bytes);
                if self.before_hash.as_deref() == Some(after_hash.as_str()) {
                    manifest.files.remove(index);
                } else {
                    manifest.files[index].after = Some(BlobMeta {
                        hash: after_hash,
                        mode: mode_of(&self.absolute),
                        size: bytes.len() as u64,
                    });
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
}
