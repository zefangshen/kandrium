//! Retention and garbage collection for the history store.
//!
//! Runs are pruned per kind — implicit first, then app, then explicit — by a
//! newest-N count and an age window. Open, `files-restored` and the currently
//! written run are never pruned. After pruning, unreferenced objects and
//! `db/<run-id>-*` files without a manifest are collected, which also sweeps
//! crash orphans. The shared ceiling covers objects, start snapshots and diffs;
//! a directory delete that would cross it marks the overflow `budget` instead of
//! refusing the delete (see `capture::capture_tree`).

use std::fs;
use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

use super::store::HistoryStore;
use super::{Manifest, RunKind, RunState};

/// Shared ceiling for objects, start snapshots and diffs.
pub const TOTAL_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Per-kind retention: newest N runs and a maximum age, plus the shared
/// ceiling. Defaults follow the plan: explicit 20 / 30 days, implicit 200 / 7
/// days, app 100 / 3 days.
#[derive(Debug, Clone, Copy)]
pub struct Retention {
    pub explicit_runs: usize,
    pub explicit_secs: i64,
    pub implicit_runs: usize,
    pub implicit_secs: i64,
    pub app_runs: usize,
    pub app_secs: i64,
    pub total_bytes: u64,
}

pub const DEFAULT_RETENTION: Retention = Retention {
    explicit_runs: 20,
    explicit_secs: 30 * 24 * 60 * 60,
    implicit_runs: 200,
    implicit_secs: 7 * 24 * 60 * 60,
    app_runs: 100,
    app_secs: 3 * 24 * 60 * 60,
    total_bytes: TOTAL_BUDGET_BYTES,
};

impl Retention {
    fn limits(self, kind: RunKind) -> (usize, i64) {
        match kind {
            RunKind::Explicit => (self.explicit_runs, self.explicit_secs),
            RunKind::Implicit => (self.implicit_runs, self.implicit_secs),
            RunKind::App => (self.app_runs, self.app_secs),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub pruned: Vec<String>,
    pub objects_removed: usize,
    pub freed_bytes: u64,
}

/// Apply the per-kind retention windows, then collect unreferenced objects.
pub fn prune(
    db: &Connection,
    store: &HistoryStore,
    policy: Retention,
    now: i64,
) -> Result<PruneReport> {
    let manifests = store.load_all()?;
    let mut doomed = Vec::new();
    for kind in [RunKind::Implicit, RunKind::App, RunKind::Explicit] {
        let (limit, secs) = policy.limits(kind);
        let mut group: Vec<&Manifest> = manifests
            .iter()
            .filter(|manifest| manifest.kind == kind && prunable(manifest))
            .collect();
        group.sort_by_key(|a| std::cmp::Reverse(a.started_at));
        for (index, manifest) in group.iter().enumerate() {
            let over_count = index >= limit;
            let too_old = now.saturating_sub(manifest.started_at) > secs;
            if over_count || too_old {
                doomed.push(manifest.run_id.clone());
            }
        }
    }
    remove_runs(db, store, &doomed)
}

/// Prune the oldest prunable runs until `need_bytes` fits under the ceiling.
///
/// Returns without pruning when the store already has room, so the ordinary
/// path never scans the store.
pub fn prune_to_fit(
    db: &Connection,
    store: &HistoryStore,
    policy: Retention,
    need_bytes: u64,
) -> Result<PruneReport> {
    let mut usage = disk_usage(store)?;
    if usage.saturating_add(need_bytes) <= policy.total_bytes {
        return Ok(PruneReport::default());
    }
    let manifests = store.load_all()?;
    let mut candidates: Vec<&Manifest> = manifests.iter().filter(|m| prunable(m)).collect();
    candidates.sort_by(|a, b| {
        order_key(a.kind)
            .cmp(&order_key(b.kind))
            .then_with(|| a.started_at.cmp(&b.started_at))
    });

    let mut report = PruneReport::default();
    for manifest in candidates {
        if usage.saturating_add(need_bytes) <= policy.total_bytes {
            break;
        }
        let freed = remove_run(db, store, &manifest.run_id)?;
        // Objects only this run referenced are freed by the same pass, so the
        // next check measures what actually landed on disk.
        let (objects_removed, object_bytes) = collect(store)?;
        report.objects_removed += objects_removed;
        report.freed_bytes += freed + object_bytes;
        report.pruned.push(manifest.run_id.clone());
        usage = disk_usage(store)?;
    }
    Ok(report)
}

/// Bytes held by file objects, start snapshots and diffs.
pub fn disk_usage(store: &HistoryStore) -> Result<u64> {
    Ok(dir_bytes(&store.objects_dir())? + dir_bytes(&store.db_dir())?)
}

/// Bytes still available under the shared ceiling.
pub fn remaining_budget(store: &HistoryStore) -> Result<u64> {
    Ok(DEFAULT_RETENTION
        .total_bytes
        .saturating_sub(disk_usage(store)?))
}

/// Delete objects no manifest references and `db/<run-id>-*` files left by a
/// crash. Returns the object count and bytes removed.
pub fn collect(store: &HistoryStore) -> Result<(usize, u64)> {
    let manifests = store.load_all()?;
    let referenced = referenced_hashes(&manifests);

    let mut removed = 0;
    let mut bytes = 0;
    let objects = store.objects_dir();
    if objects.is_dir() {
        for shard in fs::read_dir(&objects)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(shard.path())? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                if referenced.contains(&name) {
                    continue;
                }
                let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                if fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
                    bytes += size;
                }
            }
            fs::remove_dir(shard.path()).ok();
        }
    }

    let live: std::collections::HashSet<&str> = manifests
        .iter()
        .map(|manifest| manifest.run_id.as_str())
        .collect();
    if let Ok(db_dir) = fs::read_dir(store.db_dir()) {
        for entry in db_dir {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let Some((run_id, _)) = name.split_once('-') else {
                continue;
            };
            if live.contains(run_id) {
                continue;
            }
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            if fs::remove_file(entry.path()).is_ok() {
                bytes += size;
            }
        }
    }
    Ok((removed, bytes))
}

/// Never prune a run someone can still act on.
fn prunable(manifest: &Manifest) -> bool {
    !matches!(manifest.state, RunState::Open | RunState::FilesRestored)
}

fn order_key(kind: RunKind) -> u8 {
    match kind {
        RunKind::Implicit => 0,
        RunKind::App => 1,
        RunKind::Explicit => 2,
    }
}

fn remove_runs(db: &Connection, store: &HistoryStore, run_ids: &[String]) -> Result<PruneReport> {
    let mut report = PruneReport::default();
    for run_id in run_ids {
        let freed = remove_run(db, store, run_id)?;
        report.freed_bytes += freed;
        report.pruned.push(run_id.clone());
    }
    let (objects_removed, object_bytes) = collect(store)?;
    report.objects_removed = objects_removed;
    report.freed_bytes += object_bytes;
    Ok(report)
}

/// Remove one run: manifest, snapshot, diff, mirror rows and its objects.
fn remove_run(db: &Connection, store: &HistoryStore, run_id: &str) -> Result<u64> {
    let mut freed = 0;
    for path in [
        store.start_snapshot_path(run_id)?,
        store.end_snapshot_path(run_id)?,
        store.diff_path(run_id)?,
        store.manifest_path(run_id)?,
    ] {
        if let Ok(meta) = fs::metadata(&path) {
            freed += meta.len();
        }
        fs::remove_file(path).ok();
    }
    db.execute("DELETE FROM run_row_changes WHERE run_id = ?1", [run_id])?;
    db.execute(
        "DELETE FROM run_undo WHERE run_id = ?1 AND state <> 'open'",
        [run_id],
    )?;
    Ok(freed)
}

fn referenced_hashes(manifests: &[Manifest]) -> std::collections::HashSet<String> {
    let mut hashes = std::collections::HashSet::new();
    for manifest in manifests {
        for entry in &manifest.files {
            if let Some(before) = &entry.before
                && !before.hash.is_empty()
            {
                hashes.insert(before.hash.clone());
            }
            if let Some(after) = &entry.after
                && !after.hash.is_empty()
            {
                hashes.insert(after.hash.clone());
            }
        }
    }
    hashes
}

fn dir_bytes(path: &Path) -> Result<u64> {
    if !path.is_dir() {
        return Ok(0);
    }
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_schema;
    use crate::db::open_db;
    use crate::history::{
        BlobMeta, ClosedBy, FileEntry, FileOp, Manifest, RootKind, RunKind, RunState, commit_run,
        ensure_manifest,
    };

    struct Fixture {
        _tmp: tempfile::TempDir,
        db: Connection,
        store: HistoryStore,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.db");
        let mut db = open_db(&path).unwrap();
        run_schema(&mut db).unwrap();
        let store = HistoryStore::for_db(&path);
        Fixture {
            _tmp: tmp,
            db,
            store,
        }
    }

    fn file_entry(path: &str, hash: &str, size: u64) -> FileEntry {
        FileEntry {
            seq: 1,
            root: RootKind::Wiki,
            path: path.into(),
            op: FileOp::Modify,
            before: Some(BlobMeta {
                hash: hash.into(),
                mode: 420,
                size,
            }),
            after: None,
            captured_at: 0,
            oversize: false,
            restorable: true,
            reason: None,
            sidecar: false,
        }
    }

    fn run(f: &Fixture, id: &str, kind: RunKind, started_at: i64, bytes: &[u8]) -> String {
        let mut manifest = Manifest::new(id, kind, id, started_at);
        let hash = f.store.store_object(bytes).unwrap();
        manifest
            .files
            .push(file_entry("page.md", &hash, bytes.len() as u64));
        f.store.save(&manifest).unwrap();
        commit_run(&f.db, &f.store, id, ClosedBy::Action).unwrap();
        id.to_string()
    }

    #[test]
    fn prune_keeps_the_newest_runs_and_skips_restored_ones() {
        let f = fixture();
        let now = 1_000_000_000;
        let policy = Retention {
            explicit_runs: 1,
            explicit_secs: 30 * 86_400,
            implicit_runs: 1,
            implicit_secs: 86_400,
            app_runs: 1,
            app_secs: 86_400,
            total_bytes: TOTAL_BUDGET_BYTES,
        };
        for (index, id) in ["implicit1", "implicit2", "implicit3"].iter().enumerate() {
            run(
                &f,
                id,
                RunKind::Implicit,
                now - 300 + index as i64,
                b"implicit",
            );
        }
        run(&f, "app1", RunKind::App, now - 100, b"app");
        run(&f, "explicit1", RunKind::Explicit, now - 100, b"explicit1");
        run(&f, "explicit2", RunKind::Explicit, now - 50, b"explicit2");
        run(&f, "explicit3", RunKind::Explicit, now - 10, b"explicit3");

        // A files-restored run is a retry point and survives any window.
        let mut restored = f.store.load("explicit1").unwrap().unwrap();
        restored.state = RunState::FilesRestored;
        f.store.save(&restored).unwrap();

        let report = prune(&f.db, &f.store, policy, now).unwrap();
        assert!(report.pruned.contains(&"implicit1".to_string()));
        assert!(report.pruned.contains(&"implicit2".to_string()));
        assert!(!report.pruned.contains(&"implicit3".to_string()));
        assert!(report.pruned.contains(&"explicit2".to_string()));
        assert!(!report.pruned.contains(&"explicit3".to_string()));
        assert!(f.store.load("explicit2").unwrap().is_none());
        assert!(f.store.load("explicit3").unwrap().is_some());
        assert!(f.store.load("explicit1").unwrap().is_some());
        assert!(f.store.load("app1").unwrap().is_some());
    }

    #[test]
    fn age_prunes_even_within_the_count_window() {
        let f = fixture();
        let now = 1_000_000_000;
        let policy = DEFAULT_RETENTION;
        run(&f, "old", RunKind::App, now - 4 * 86_400, b"old");
        run(&f, "fresh", RunKind::App, now - 60, b"fresh");
        let report = prune(&f.db, &f.store, policy, now).unwrap();
        assert_eq!(report.pruned, vec!["old".to_string()]);
        assert!(f.store.load("old").unwrap().is_none());
        assert!(f.store.load("fresh").unwrap().is_some());
    }

    #[test]
    fn a_shared_object_survives_pruning_one_of_its_runs() {
        let f = fixture();
        let now = 1_000_000_000;
        let bytes = b"shared pre-image";
        let policy = Retention {
            explicit_runs: 1,
            explicit_secs: 30 * 86_400,
            implicit_runs: 1,
            implicit_secs: 86_400,
            app_runs: 0,
            app_secs: 86_400,
            total_bytes: TOTAL_BUDGET_BYTES,
        };
        let first = run(&f, "one", RunKind::Implicit, now - 200, bytes);
        run(&f, "two", RunKind::Implicit, now - 100, bytes);
        run(&f, "three", RunKind::Implicit, now, bytes);
        let hash = f.store.load("three").unwrap().unwrap().files[0]
            .before
            .as_ref()
            .unwrap()
            .hash
            .clone();

        prune(&f.db, &f.store, policy, now).unwrap();
        assert!(f.store.load(&first).unwrap().is_none());
        assert!(f.store.load("two").unwrap().is_none());
        assert!(f.store.load("three").unwrap().is_some());
        assert!(
            f.store.has_object(&hash),
            "the object is still referenced by the surviving run"
        );
    }

    #[test]
    fn collection_removes_orphans_without_manifests() {
        let f = fixture();
        let hash = f.store.store_object(b"nobody references me").unwrap();
        let orphan_snapshot = f.store.start_snapshot_path("deadbeef").unwrap();
        fs::create_dir_all(orphan_snapshot.parent().unwrap()).unwrap();
        fs::write(&orphan_snapshot, b"snapshot").unwrap();

        let (objects, bytes) = collect(&f.store).unwrap();
        assert_eq!(objects, 1);
        assert!(bytes > 0);
        assert!(!f.store.has_object(&hash));
        assert!(!orphan_snapshot.exists());
    }

    #[test]
    fn prune_to_fit_stops_at_the_ceiling() {
        let f = fixture();
        let now = 1_000_000_000;
        let bytes_one = vec![1u8; 2048];
        let bytes_two = vec![2u8; 2048];
        let policy = Retention {
            explicit_runs: 100,
            explicit_secs: 30 * 86_400,
            implicit_runs: 100,
            implicit_secs: 7 * 86_400,
            app_runs: 100,
            app_secs: 86_400,
            total_bytes: disk_usage(&f.store).unwrap() + 2048 + 1500,
        };
        run(&f, "one", RunKind::Implicit, now - 200, &bytes_one);
        run(&f, "two", RunKind::Implicit, now - 100, &bytes_two);
        let need = 1024;
        let report = prune_to_fit(&f.db, &f.store, policy, need).unwrap();
        assert!(!report.pruned.is_empty());
        assert!(disk_usage(&f.store).unwrap() + need <= policy.total_bytes);
        assert!(f.store.load("one").unwrap().is_none());
        assert!(f.store.load("two").unwrap().is_some());
    }

    #[test]
    fn a_run_created_by_ensure_manifest_is_listed_before_it_closes() {
        let f = fixture();
        ensure_manifest(&f.store, "pending", RunKind::App, "pending action").unwrap();
        assert!(f.store.load("pending").unwrap().is_some());
    }
}
