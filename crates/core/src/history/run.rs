//! Run lifecycle: the single open slot, implicit runs, heartbeats and the
//! abandonment sweep.
//!
//! An explicit run holds the one `state = 'open'` row in `run_undo`, enforced by
//! the partial unique index. Implicit and app runs never hold it: they are
//! inserted `committed` when they close. `last_heartbeat_at` is the single
//! staleness signal — ten minutes without a heartbeat lets a later `begin`
//! reclaim the slot, and the 24-hour sweep abandons whatever is left.
//!
//! Abandoned and reclaimed runs keep `state = committed` with
//! `closed_by = abandoned`: the manifest's state enum has no abandoned value,
//! and their captured files stay undoable while their rows are not, because no
//! diff was ever derived.

use std::fs;

use rusqlite::{Connection, params};

use crate::error::{KandriumError, Result};

use super::store::HistoryStore;
use super::{ClosedBy, Manifest, RunKind, RunState};

/// No heartbeat for this long makes the open run reclaimable.
pub const HEARTBEAT_STALE_SECS: i64 = 10 * 60;
/// No heartbeat for this long lets the sweep abandon the run.
pub const ABANDON_AFTER_SECS: i64 = 24 * 60 * 60;

/// The run currently holding the open slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRun {
    pub run_id: String,
    pub label: String,
    pub last_heartbeat_at: i64,
}

/// The open run, if any. Every process sees the same row.
pub fn open_run(db: &Connection, store: &HistoryStore) -> Result<Option<OpenRun>> {
    let mut stmt = db.prepare(
        "SELECT run_id, last_heartbeat_at FROM run_undo
         WHERE state = 'open' ORDER BY last_heartbeat_at DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let run_id: String = row.get(0)?;
    let last_heartbeat_at: i64 = row.get(1)?;
    let label = store
        .load(&run_id)?
        .map(|manifest| manifest.label)
        .unwrap_or_default();
    Ok(Some(OpenRun {
        run_id,
        label,
        last_heartbeat_at,
    }))
}

/// Open an explicit run and return its manifest.
///
/// Refused with `a run is open: <label>` while a fresh run is open. A run
/// without a heartbeat for [`HEARTBEAT_STALE_SECS`] is reclaimed first, and the
/// 24-hour sweep runs before the slot check.
pub fn begin_run(db: &Connection, store: &HistoryStore, label: &str) -> Result<Manifest> {
    let at = now();
    sweep_abandoned(db, store)?;
    if let Some(open) = open_run(db, store)? {
        if at - open.last_heartbeat_at < HEARTBEAT_STALE_SECS {
            return Err(KandriumError(format!("a run is open: {}", open.label)));
        }
        abandon(db, store, &open, "reclaimed: no heartbeat for 10 minutes")?;
    }

    let run_id = crate::db::uid();
    let manifest = Manifest::new(run_id.clone(), RunKind::Explicit, label, at);
    store.save(&manifest)?;
    let inserted = db.execute(
        "INSERT INTO run_undo (run_id, state, last_heartbeat_at) VALUES (?1, 'open', ?2)",
        params![run_id, at],
    );
    if let Err(error) = inserted {
        // Another process won the slot between the check and the insert.
        fs::remove_file(store.manifest_path(&run_id)?).ok();
        return match open_run(db, store)? {
            Some(open) => Err(KandriumError(format!("a run is open: {}", open.label))),
            None => Err(error.into()),
        };
    }
    Ok(manifest)
}

/// Create the manifest for a run that does not hold the open slot, or reopen an
/// existing one. Implicit and client-grouped runs use this so their label and
/// kind describe the work before any capture rewrites the file.
pub fn ensure_manifest(
    store: &HistoryStore,
    run_id: &str,
    kind: RunKind,
    label: &str,
) -> Result<Manifest> {
    let at = now();
    let mut manifest = match store.load(run_id)? {
        Some(manifest) => manifest,
        None => Manifest::new(run_id, kind, label, at),
    };
    manifest.state = RunState::Open;
    manifest.finished_at = None;
    manifest.heartbeat_at = at;
    if manifest.label.is_empty() {
        manifest.label = label.to_string();
    }
    store.save(&manifest)?;
    Ok(manifest)
}

/// Advance an open run's heartbeat, mirrored into its manifest.
pub fn heartbeat_run(db: &Connection, store: &HistoryStore, run_id: &str) -> Result<i64> {
    let at = now();
    let changed = db.execute(
        "UPDATE run_undo SET last_heartbeat_at = ?2 WHERE run_id = ?1 AND state = 'open'",
        params![run_id, at],
    )?;
    if changed == 0 {
        return Err(KandriumError(format!("run {run_id} is not open")));
    }
    if let Some(mut manifest) = store.load(run_id)? {
        manifest.heartbeat_at = at;
        store.save(&manifest)?;
    }
    Ok(at)
}

/// Close the run that holds the open slot.
///
/// Fails with `run ... was reclaimed` when the slot is no longer held: the
/// caller's run was taken over or already closed.
pub fn end_open_run(
    db: &Connection,
    store: &HistoryStore,
    run_id: &str,
    closed_by: ClosedBy,
    note: Option<&str>,
) -> Result<Manifest> {
    let at = now();
    let changed = db.execute(
        "UPDATE run_undo SET state = 'committed', last_heartbeat_at = ?2
         WHERE run_id = ?1 AND state = 'open'",
        params![run_id, at],
    )?;
    if changed == 0 {
        return Err(KandriumError(format!(
            "run {run_id} was reclaimed or already closed"
        )));
    }
    if let Some(note) = note {
        db.execute(
            "UPDATE run_undo SET note = ?2 WHERE run_id = ?1",
            params![run_id, note],
        )?;
    }
    let mut manifest = store
        .load(run_id)?
        .ok_or_else(|| KandriumError(format!("run {run_id} not found")))?;
    manifest.state = RunState::Committed;
    manifest.closed_by = Some(closed_by);
    manifest.finished_at = Some(at);
    manifest.heartbeat_at = at;
    store.save(&manifest)?;
    Ok(manifest)
}

/// Close a run that never held the open slot (implicit, app, client-grouped).
///
/// A run holding the slot is left alone: only `end_open_run` may close it.
pub fn commit_run(
    db: &Connection,
    store: &HistoryStore,
    run_id: &str,
    closed_by: ClosedBy,
) -> Result<Option<Manifest>> {
    let open: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM run_undo WHERE run_id = ?1 AND state = 'open')",
        [run_id],
        |row| row.get(0),
    )?;
    if open {
        return Ok(None);
    }
    let at = now();
    let Some(mut manifest) = store.load(run_id)? else {
        return Ok(None);
    };
    manifest.state = RunState::Committed;
    manifest.closed_by = Some(closed_by);
    manifest.finished_at.get_or_insert(at);
    manifest.heartbeat_at = at;
    store.save(&manifest)?;
    db.execute(
        "INSERT INTO run_undo (run_id, state, last_heartbeat_at)
         VALUES (?1, 'committed', ?2)
         ON CONFLICT(run_id) DO UPDATE SET
             state = 'committed',
             last_heartbeat_at = excluded.last_heartbeat_at
         WHERE run_undo.state <> 'open'",
        params![run_id, at],
    )?;
    Ok(Some(manifest))
}

/// Drop a run that captured nothing, after a call that failed before touching
/// anything. Returns whether a manifest was removed.
pub fn discard_empty_run(db: &Connection, store: &HistoryStore, run_id: &str) -> Result<bool> {
    let Some(manifest) = store.load(run_id)? else {
        return Ok(false);
    };
    let no_rows = manifest
        .rows
        .as_ref()
        .map(|rows| rows.counts.total() == 0)
        .unwrap_or(true);
    if !manifest.files.is_empty()
        || !manifest.dirs.is_empty()
        || !manifest.links.is_empty()
        || !no_rows
    {
        return Ok(false);
    }
    fs::remove_file(store.manifest_path(run_id)?).ok();
    fs::remove_file(store.start_snapshot_path(run_id)?).ok();
    fs::remove_file(store.diff_path(run_id)?).ok();
    db.execute("DELETE FROM run_row_changes WHERE run_id = ?1", [run_id])?;
    db.execute(
        "DELETE FROM run_undo WHERE run_id = ?1 AND state <> 'open'",
        [run_id],
    )?;
    Ok(true)
}

/// Mark every run that has held the slot for a day without a heartbeat
/// abandoned. Returns the abandoned run ids.
pub fn sweep_abandoned(db: &Connection, store: &HistoryStore) -> Result<Vec<String>> {
    let cutoff = now() - ABANDON_AFTER_SECS;
    let stale: Vec<(String, i64)> = {
        let mut stmt = db.prepare(
            "SELECT run_id, last_heartbeat_at FROM run_undo
             WHERE state = 'open' AND last_heartbeat_at <= ?1",
        )?;
        let rows = stmt.query_map([cutoff], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut abandoned = Vec::new();
    for (run_id, last_heartbeat_at) in stale {
        let open = OpenRun {
            label: store
                .load(&run_id)?
                .map(|manifest| manifest.label)
                .unwrap_or_default(),
            run_id,
            last_heartbeat_at,
        };
        abandon(db, store, &open, "abandoned: no heartbeat for 24 hours")?;
        abandoned.push(open.run_id);
    }

    // A process that died between `ensure_manifest` and `commit_run` leaves a
    // manifest in `open` state with no `run_undo` row, so the query above never
    // sees it. Close it here or it can never be pruned or undone.
    let mirrored: std::collections::HashSet<String> = {
        let mut stmt = db.prepare("SELECT run_id FROM run_undo")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for mut manifest in store.load_all()? {
        if manifest.state != RunState::Open
            || manifest.heartbeat_at > cutoff
            || mirrored.contains(&manifest.run_id)
        {
            continue;
        }
        manifest.state = RunState::Committed;
        manifest.closed_by = Some(ClosedBy::Abandoned);
        manifest.finished_at = Some(now());
        store.save(&manifest)?;
        abandoned.push(manifest.run_id);
    }
    Ok(abandoned)
}

/// Reclaim the open slot when its owner stopped heartbeating.
///
/// Returns the reclaimed run, or `None` when the slot is free or still fresh.
/// Callers that would otherwise refuse work because "a run is open" use this
/// so a crashed owner cannot wedge them for a day.
pub fn reclaim_stale(db: &Connection, store: &HistoryStore) -> Result<Option<OpenRun>> {
    let Some(open) = open_run(db, store)? else {
        return Ok(None);
    };
    if now() - open.last_heartbeat_at < HEARTBEAT_STALE_SECS {
        return Ok(None);
    }
    abandon(db, store, &open, "reclaimed: no heartbeat for 10 minutes")?;
    Ok(Some(open))
}

/// Close a run's manifest and mirror row as abandoned.
fn abandon(db: &Connection, store: &HistoryStore, open: &OpenRun, note: &str) -> Result<()> {
    if let Some(mut manifest) = store.load(&open.run_id)? {
        manifest.state = RunState::Committed;
        manifest.closed_by = Some(ClosedBy::Abandoned);
        manifest.finished_at = Some(now());
        store.save(&manifest)?;
    }
    db.execute(
        "UPDATE run_undo SET state = 'committed', note = ?2
         WHERE run_id = ?1 AND state = 'open'",
        params![open.run_id, note],
    )?;
    Ok(())
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run_schema;
    use crate::db::open_db;

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

    fn age_out(f: &Fixture, run_id: &str, seconds: i64) {
        f.db.execute(
            "UPDATE run_undo SET last_heartbeat_at = ?2 WHERE run_id = ?1",
            params![run_id, now() - seconds],
        )
        .unwrap();
    }

    #[test]
    fn begin_opens_one_slot_and_refuses_a_second_run() {
        let f = fixture();
        let first = begin_run(&f.db, &f.store, "ingest the attention papers").unwrap();
        assert_eq!(first.state, RunState::Open);
        assert_eq!(first.kind, RunKind::Explicit);

        let error = begin_run(&f.db, &f.store, "second task").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("a run is open: ingest the attention papers")
        );

        let open = open_run(&f.db, &f.store).unwrap().unwrap();
        assert_eq!(open.run_id, first.run_id);
    }

    #[test]
    fn end_releases_the_slot_and_a_reclaimed_run_refuses_end() {
        let f = fixture();
        let manifest = begin_run(&f.db, &f.store, "first").unwrap();
        end_open_run(&f.db, &f.store, &manifest.run_id, ClosedBy::Explicit, None).unwrap();
        assert!(open_run(&f.db, &f.store).unwrap().is_none());

        let error =
            end_open_run(&f.db, &f.store, &manifest.run_id, ClosedBy::Explicit, None).unwrap_err();
        assert!(error.to_string().contains("reclaimed or already closed"));
    }

    #[test]
    fn a_stale_run_is_reclaimed_by_the_next_begin() {
        let f = fixture();
        let first = begin_run(&f.db, &f.store, "abandoned task").unwrap();
        age_out(&f, &first.run_id, HEARTBEAT_STALE_SECS + 1);

        let second = begin_run(&f.db, &f.store, "next task").unwrap();
        assert_ne!(second.run_id, first.run_id);

        let old = f.store.load(&first.run_id).unwrap().unwrap();
        assert_eq!(old.state, RunState::Committed);
        assert_eq!(old.closed_by, Some(ClosedBy::Abandoned));
        assert_eq!(
            open_run(&f.db, &f.store).unwrap().unwrap().run_id,
            second.run_id
        );
    }

    #[test]
    fn the_sweep_abandons_a_day_old_run() {
        let f = fixture();
        let manifest = begin_run(&f.db, &f.store, "forgotten").unwrap();
        age_out(&f, &manifest.run_id, ABANDON_AFTER_SECS + 1);

        let swept = sweep_abandoned(&f.db, &f.store).unwrap();
        assert_eq!(swept, vec![manifest.run_id.clone()]);
        assert!(open_run(&f.db, &f.store).unwrap().is_none());

        let abandoned = f.store.load(&manifest.run_id).unwrap().unwrap();
        assert_eq!(abandoned.state, RunState::Committed);
        assert_eq!(abandoned.closed_by, Some(ClosedBy::Abandoned));
        assert!(abandoned.rows.is_none());
    }

    #[test]
    fn implicit_runs_commit_without_touching_the_slot() {
        let f = fixture();
        ensure_manifest(&f.store, "implicit1", RunKind::Implicit, "create_card").unwrap();

        let first = begin_run(&f.db, &f.store, "explicit").unwrap();
        commit_run(&f.db, &f.store, "implicit1", ClosedBy::Action).unwrap();

        let open = open_run(&f.db, &f.store).unwrap().unwrap();
        assert_eq!(open.run_id, first.run_id);

        let implicit = f.store.load("implicit1").unwrap().unwrap();
        assert_eq!(implicit.state, RunState::Committed);
        assert_eq!(implicit.closed_by, Some(ClosedBy::Action));

        let state: String =
            f.db.query_row(
                "SELECT state FROM run_undo WHERE run_id = 'implicit1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "committed");
    }

    #[test]
    fn committing_an_open_run_leaves_the_slot_alone() {
        let f = fixture();
        let manifest = begin_run(&f.db, &f.store, "explicit").unwrap();
        commit_run(&f.db, &f.store, &manifest.run_id, ClosedBy::Action).unwrap();

        assert_eq!(
            open_run(&f.db, &f.store).unwrap().unwrap().run_id,
            manifest.run_id
        );
        assert_eq!(
            f.store.load(&manifest.run_id).unwrap().unwrap().state,
            RunState::Open
        );
    }

    #[test]
    fn heartbeat_extends_only_an_open_run() {
        let f = fixture();
        let manifest = begin_run(&f.db, &f.store, "explicit").unwrap();
        let beat = heartbeat_run(&f.db, &f.store, &manifest.run_id).unwrap();
        assert_eq!(
            f.store
                .load(&manifest.run_id)
                .unwrap()
                .unwrap()
                .heartbeat_at,
            beat
        );

        end_open_run(&f.db, &f.store, &manifest.run_id, ClosedBy::Explicit, None).unwrap();
        assert!(heartbeat_run(&f.db, &f.store, &manifest.run_id).is_err());
    }

    #[test]
    fn a_reclaimed_owner_is_refused_like_a_second_client() {
        let f = fixture();
        let first = begin_run(&f.db, &f.store, "owner").unwrap();
        age_out(&f, &first.run_id, HEARTBEAT_STALE_SECS + 1);
        begin_run(&f.db, &f.store, "reclaimer").unwrap();

        let error =
            end_open_run(&f.db, &f.store, &first.run_id, ClosedBy::Explicit, None).unwrap_err();
        assert!(error.to_string().contains("reclaimed"));
    }
}
