//! On-disk history store: content-addressed file objects, run manifests and
//! the database snapshot directory.
//!
//! Layout under `<db parent>/history/`:
//!
//! ```text
//! objects/<first-two-hex>/<content-hash>   # deduplicated file pre-images
//! db/<run-id>-start.db                     # VACUUM INTO at run start
//! db/<run-id>-rows.json                    # derived table diff
//! runs/<run-id>.json                       # manifest, rewritten atomically
//! ```
//!
//! The store never captures itself, and a `KANDRIUM_DB` override keeps its
//! history beside its own database.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{KandriumError, Result};
use crate::fsutil;
use crate::wiki::hash_bytes;

use super::Manifest;

/// Directory name under the database's parent.
pub const HISTORY_DIR: &str = "history";

/// Root of one history store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryStore {
    root: PathBuf,
}

impl HistoryStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// History beside the database file, so `KANDRIUM_DB` overrides stay
    /// self-contained and tests never touch the user's store.
    pub fn for_db(db_path: &Path) -> Self {
        Self::new(
            db_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(HISTORY_DIR),
        )
    }

    /// History for the resolved database path.
    pub fn resolve() -> Self {
        Self::for_db(&crate::db::resolve_db_path())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn objects_dir(&self) -> PathBuf {
        self.root.join("objects")
    }

    pub fn db_dir(&self) -> PathBuf {
        self.root.join("db")
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
    }

    pub fn manifest_path(&self, run_id: &str) -> Result<PathBuf> {
        Ok(self
            .runs_dir()
            .join(format!("{}.json", checked_run_id(run_id)?)))
    }

    /// `db/<run-id>-start.db`.
    pub fn start_snapshot_path(&self, run_id: &str) -> Result<PathBuf> {
        Ok(self
            .db_dir()
            .join(format!("{}-start.db", checked_run_id(run_id)?)))
    }

    /// `db/<run-id>-end.db`, deleted after a successful diff.
    pub fn end_snapshot_path(&self, run_id: &str) -> Result<PathBuf> {
        Ok(self
            .db_dir()
            .join(format!("{}-end.db", checked_run_id(run_id)?)))
    }

    /// `db/<run-id>-rows.json`.
    pub fn diff_path(&self, run_id: &str) -> Result<PathBuf> {
        Ok(self
            .db_dir()
            .join(format!("{}-rows.json", checked_run_id(run_id)?)))
    }

    /// Record a pre-image and return its content hash.
    ///
    /// Objects are shared across runs and roots; an existing hash is not
    /// written again. The bytes land through `fsutil::write_atomic`, so a crash
    /// mid-copy leaves no partial object.
    pub fn store_object(&self, bytes: &[u8]) -> Result<String> {
        let hash = hash_bytes(bytes);
        let path = self.object_path(&hash)?;
        if path.exists() {
            return Ok(hash);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fsutil::write_atomic(&path, bytes)?;
        Ok(hash)
    }

    pub fn object_path(&self, hash: &str) -> Result<PathBuf> {
        if hash.len() < 2 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(KandriumError(format!("invalid content hash: {hash}")));
        }
        Ok(self.objects_dir().join(&hash[..2]).join(hash))
    }

    pub fn has_object(&self, hash: &str) -> bool {
        self.object_path(hash)
            .map(|path| path.exists())
            .unwrap_or(false)
    }

    /// Read a stored pre-image. A missing object means the run cannot restore
    /// that entry; callers report it rather than failing the whole store.
    pub fn read_object(&self, hash: &str) -> Result<Vec<u8>> {
        Ok(fs::read(self.object_path(hash)?)?)
    }

    /// Persist a manifest atomically. The JSON file is authoritative; the
    /// database tables are mirrors.
    pub fn save(&self, manifest: &Manifest) -> Result<()> {
        fs::create_dir_all(self.runs_dir())?;
        let json = serde_json::to_vec_pretty(manifest)
            .map_err(|error| KandriumError(format!("cannot encode run manifest: {error}")))?;
        fsutil::write_atomic(&self.manifest_path(&manifest.run_id)?, json)?;
        Ok(())
    }

    pub fn load(&self, run_id: &str) -> Result<Option<Manifest>> {
        let path = self.manifest_path(run_id)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let manifest = serde_json::from_str(&text).map_err(|error| {
            KandriumError(format!(
                "cannot read run manifest {}: {error}",
                path.display()
            ))
        })?;
        Ok(Some(manifest))
    }

    /// Every stored manifest, oldest run id first. A corrupt manifest is an
    /// error: GC must never treat its objects as unreferenced.
    pub fn load_all(&self) -> Result<Vec<Manifest>> {
        let mut manifests = Vec::new();
        let entries = match fs::read_dir(self.runs_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(manifests),
            Err(error) => return Err(error.into()),
        };
        let mut run_ids: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.strip_suffix(".json").map(str::to_string)
            })
            .collect();
        run_ids.sort();
        for run_id in run_ids {
            if let Some(manifest) = self.load(&run_id)? {
                manifests.push(manifest);
            }
        }
        Ok(manifests)
    }

    /// Remove every object no manifest references. This is the crash-orphan
    /// collector: an object is written before its manifest, so a crash in
    /// between leaves bytes nothing can restore.
    pub fn gc_orphan_objects(&self) -> Result<usize> {
        let mut referenced: HashSet<String> = HashSet::new();
        for manifest in self.load_all()? {
            for entry in &manifest.files {
                for blob in [&entry.before, &entry.after].into_iter().flatten() {
                    if !blob.hash.is_empty() {
                        referenced.insert(blob.hash.clone());
                    }
                }
            }
        }

        let mut removed = 0;
        let shards = match fs::read_dir(self.objects_dir()) {
            Ok(shards) => shards,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        for shard in shards.flatten() {
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for object in fs::read_dir(shard.path())?.flatten() {
                let name = object.file_name().to_string_lossy().to_string();
                if !referenced.contains(&name) {
                    fs::remove_file(object.path())?;
                    removed += 1;
                }
            }
            // Leave empty shard directories in place: they cost nothing and
            // keep the two-level layout stable.
        }
        Ok(removed)
    }
}

/// Run ids become file names, so reject anything that is not a plain token.
fn checked_run_id(run_id: &str) -> Result<&str> {
    if run_id.is_empty()
        || !run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(KandriumError(format!("invalid run id: {run_id}")));
    }
    Ok(run_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{BlobMeta, FileEntry, FileOp, RootKind};

    fn store() -> (tempfile::TempDir, HistoryStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path().join("history"));
        (dir, store)
    }

    fn entry(seq: i64, path: &str, hash: &str) -> FileEntry {
        FileEntry {
            seq,
            root: RootKind::Wiki,
            path: path.into(),
            op: FileOp::Modify,
            before: Some(BlobMeta {
                hash: hash.into(),
                mode: 420,
                size: 1,
            }),
            after: None,
            captured_at: 1,
            oversize: false,
            restorable: true,
            reason: None,
            sidecar: false,
        }
    }

    #[test]
    fn history_lives_beside_the_database() {
        let store = HistoryStore::for_db(Path::new("/data/kandrium/kandrium.db"));
        assert_eq!(store.root(), Path::new("/data/kandrium/history"));
    }

    #[test]
    fn objects_are_deduplicated() {
        let (_dir, store) = store();
        let first = store.store_object(b"same bytes").unwrap();
        let second = store.store_object(b"same bytes").unwrap();
        assert_eq!(first, second);

        let path = store.object_path(&first).unwrap();
        assert!(path.exists());
        assert_eq!(path.parent().unwrap().file_name().unwrap(), &first[..2]);

        let objects: Vec<_> = fs::read_dir(store.objects_dir())
            .unwrap()
            .flatten()
            .flat_map(|shard| fs::read_dir(shard.path()).unwrap().flatten())
            .collect();
        assert_eq!(objects.len(), 1);
        assert_eq!(store.read_object(&first).unwrap(), b"same bytes");
    }

    #[test]
    fn invalid_ids_and_hashes_are_rejected() {
        let (_dir, store) = store();
        assert!(store.manifest_path("../escape").is_err());
        assert!(store.manifest_path("").is_err());
        assert!(store.object_path("zz").is_err());
        assert!(store.store_object(b"x").is_ok());
    }

    #[test]
    fn manifest_round_trips_through_the_store() {
        let (_dir, store) = store();
        let mut manifest = Manifest::new("run1", crate::history::RunKind::Implicit, "run1", 1);
        manifest
            .files
            .push(entry(1, "concepts/attention.md", "abcd"));
        store.save(&manifest).unwrap();

        assert_eq!(store.load("run1").unwrap(), Some(manifest.clone()));
        assert_eq!(store.load("missing").unwrap(), None);
        assert_eq!(store.load_all().unwrap(), vec![manifest]);
    }

    #[test]
    fn gc_removes_unreferenced_objects_only() {
        let (_dir, store) = store();
        let kept = store.store_object(b"kept").unwrap();
        let orphan = store.store_object(b"orphan").unwrap();
        let mut manifest = Manifest::new("run1", crate::history::RunKind::Implicit, "run1", 1);
        manifest.files.push(entry(1, "a.md", &kept));
        store.save(&manifest).unwrap();

        assert_eq!(store.gc_orphan_objects().unwrap(), 1);
        assert!(store.has_object(&kept));
        assert!(!store.has_object(&orphan));

        // A second pass has nothing left to collect.
        assert_eq!(store.gc_orphan_objects().unwrap(), 0);
    }
}
