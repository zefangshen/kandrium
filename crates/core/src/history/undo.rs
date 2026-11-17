//! Unified undo: the row phase and the `undo_run` entry point.
//!
//! Files restore first (paths must exist before rows reference them), then the
//! start snapshot is replayed per key inside one transaction with foreign keys
//! disabled, so cascade-deleted children come back without transient unique
//! conflicts. Schema drift is a hard refusal; contention is a conflict `force`
//! may override.

use std::fs;

use rusqlite::types::Value;
use rusqlite::{Connection, params, params_from_iter};

use crate::error::{KandriumError, Result};

use super::restore::{FileUndoReport, Roots, restore_files};
use super::rows::{self, CAPTURED_TABLES, TableSpec};
use super::store::HistoryStore;
use super::{ChangeOp, Manifest, RowDiff, RowsMeta, RunState};

/// One row key the restore refused because it changed after the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowConflict {
    pub table: String,
    pub key: Vec<String>,
    pub reason: String,
}

impl RowConflict {
    fn render(&self) -> String {
        if self.key.is_empty() {
            format!("row {}: {}", self.table, self.reason)
        } else {
            format!("row {} {}: {}", self.table, self.key.join("/"), self.reason)
        }
    }
}

/// What the row phase did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowUndoReport {
    pub updated: usize,
    pub inserted: usize,
    pub deleted: usize,
    pub conflicts: Vec<RowConflict>,
    /// Why rows were not restored (no diff, abandoned run, and so on).
    pub note: Option<String>,
}

/// What `undo_run` did.
#[derive(Debug, Clone)]
pub struct UndoReport {
    pub run_id: String,
    pub state: RunState,
    /// A conflict list was returned and nothing was touched.
    pub blocked: bool,
    pub files: FileUndoReport,
    pub rows: RowUndoReport,
    pub conflicts: Vec<String>,
}

impl UndoReport {
    fn blocked(run_id: &str, state: RunState, files: FileUndoReport, rows: RowUndoReport) -> Self {
        let mut conflicts: Vec<String> = files
            .conflicts
            .iter()
            .map(|conflict| format!("file {}: {}", conflict.path, conflict.reason))
            .collect();
        conflicts.extend(rows.conflicts.iter().map(RowConflict::render));
        Self {
            run_id: run_id.to_string(),
            state,
            blocked: true,
            files,
            rows,
            conflicts,
        }
    }
}

/// Undo one run: files, then rows, then derived state.
///
/// `force` overwrites row conflicts and skips conflicted files. Schema drift is
/// refused even with `force`.
pub fn undo_run(
    db: &Connection,
    store: &HistoryStore,
    roots: &Roots,
    run_id: &str,
    force: bool,
) -> Result<UndoReport> {
    let manifest = store
        .load(run_id)?
        .ok_or_else(|| KandriumError(format!("run {run_id} not found")))?;
    let mut manifest = manifest;
    match manifest.state {
        RunState::Open => {
            return Err(KandriumError::new(
                "run is still in progress; close it before undo",
            ));
        }
        RunState::Undone | RunState::Partial => {
            return Err(KandriumError::new(format!(
                "run {run_id} is already undone"
            )));
        }
        RunState::Failed => {
            return Err(KandriumError::new(format!(
                "run {run_id} failed; inspect it before retrying"
            )));
        }
        RunState::Committed | RunState::FilesRestored => {}
    }

    // A fresh run may still write the same rows; undo would race its owner.
    if let Some(open) = super::run::open_run(db, store)? {
        return Err(KandriumError(format!(
            "close the open run first: {}",
            open.label
        )));
    }

    // Row preflight runs first: a row conflict must abort before files change.
    let mut rows_report = RowUndoReport::default();
    if let Some(meta) = &manifest.rows {
        if let Some(drift) = schema_drift(db, meta)? {
            return Err(KandriumError(format!("schema drift: {drift}")));
        }
        if !meta.restorable {
            rows_report.note = Some("row diff was not captured; rows are not undoable".into());
        } else {
            rows_report.conflicts = preflight_rows(db, store, meta)?;
        }
    } else {
        rows_report.note = Some("no row changes".into());
    }
    if !rows_report.conflicts.is_empty() && !force {
        return Ok(UndoReport::blocked(
            run_id,
            manifest.state,
            FileUndoReport::default(),
            rows_report,
        ));
    }

    let files_done = manifest.state == RunState::FilesRestored;
    let mut files_report = FileUndoReport::default();
    if !files_done {
        files_report = restore_files(db, store, roots, &manifest, force)?;
        if files_report.blocked {
            return Ok(UndoReport::blocked(
                run_id,
                manifest.state,
                files_report,
                rows_report,
            ));
        }
    }

    let backup = take_pre_undo_backup(db, store, run_id)?;
    if let Some(meta) = &manifest.rows
        && meta.restorable
    {
        match restore_rows(db, store, meta) {
            Ok((updated, inserted, deleted)) => {
                rows_report.updated = updated;
                rows_report.inserted = inserted;
                rows_report.deleted = deleted;
            }
            Err(error) => {
                // Files already changed, rows did not: keep a retry point.
                mark_state(db, store, &mut manifest, RunState::FilesRestored)?;
                return Err(error);
            }
        }
    }

    let partial = files_report.partial();
    let state = if partial {
        RunState::Partial
    } else {
        RunState::Undone
    };
    mark_state(db, store, &mut manifest, state)?;
    if let Some(backup) = backup {
        fs::remove_file(backup).ok();
    }

    let mut conflicts: Vec<String> = files_report
        .conflicts
        .iter()
        .map(|conflict| format!("file {}: {}", conflict.path, conflict.reason))
        .collect();
    conflicts.extend(rows_report.conflicts.iter().map(RowConflict::render));
    conflicts.extend(
        files_report
            .skipped
            .iter()
            .map(|skipped| format!("skipped {}: {}", skipped.path, skipped.reason)),
    );

    Ok(UndoReport {
        run_id: run_id.to_string(),
        state,
        blocked: false,
        files: files_report,
        rows: rows_report,
        conflicts,
    })
}

/// Schema drift: a different `user_version`, a missing allowlisted table, or a
/// changed `PRAGMA table_info` hash. Never overridable by `force`.
pub fn schema_drift(db: &Connection, meta: &RowsMeta) -> Result<Option<String>> {
    let version = rows::read_user_version(db)?;
    if version != meta.user_version {
        return Ok(Some(format!(
            "database is at version {version}, the run captured version {}",
            meta.user_version
        )));
    }
    let current = rows::schema_hashes(db)?;
    for (table, expected) in &meta.schema {
        match current.get(table) {
            None => return Ok(Some(format!("table {table} is missing"))),
            Some(actual) if actual != expected => {
                return Ok(Some(format!("table {table} changed shape")));
            }
            Some(_) => {}
        }
    }
    Ok(None)
}

/// Compare every diff key with the current row (§5 rows).
pub fn preflight_rows(
    db: &Connection,
    store: &HistoryStore,
    meta: &RowsMeta,
) -> Result<Vec<RowConflict>> {
    let diff = match load_diff(store, meta)? {
        Some(diff) => diff,
        None => {
            return Ok(vec![RowConflict {
                table: "*".into(),
                key: vec![],
                reason: "row diff is missing".into(),
            }]);
        }
    };
    let mut conflicts = Vec::new();
    if meta.contended {
        conflicts.push(RowConflict {
            table: "*".into(),
            key: vec![],
            reason: "another connection wrote during the run".into(),
        });
    }

    let snapshot_path = store.root().join(&meta.snapshot);
    let snapshot = rows::open_snapshot(&snapshot_path)?;
    for (name, table) in &diff.tables {
        let Some(spec) = spec_for(name) else {
            conflicts.push(RowConflict {
                table: name.clone(),
                key: vec![],
                reason: "table is not in the allowlist".into(),
            });
            continue;
        };
        let current = rows::read_table(db, spec)?;
        let (_columns, start) = rows::table_rows(&snapshot, spec)?;
        for change in &table.changes {
            match change.op {
                ChangeOp::Insert | ChangeOp::Update => match current.get(&change.key) {
                    Some(hash) if Some(hash) == change.after_hash.as_ref() => {}
                    Some(_) => conflicts.push(RowConflict {
                        table: name.clone(),
                        key: change.key.clone(),
                        reason: "changed after the run".into(),
                    }),
                    None => conflicts.push(RowConflict {
                        table: name.clone(),
                        key: change.key.clone(),
                        reason: "missing".into(),
                    }),
                },
                ChangeOp::Delete => {
                    if current.contains_key(&change.key) {
                        conflicts.push(RowConflict {
                            table: name.clone(),
                            key: change.key.clone(),
                            reason: "row exists again".into(),
                        });
                    }
                }
            }
            if change.op != ChangeOp::Insert && !start.contains_key(&change.key) {
                conflicts.push(RowConflict {
                    table: name.clone(),
                    key: change.key.clone(),
                    reason: "start row is missing from the snapshot".into(),
                });
            }
        }
    }
    Ok(conflicts)
}

/// Replay the diff inside one `BEGIN IMMEDIATE` with foreign keys off. Returns
/// `(updated, inserted, deleted)` counts.
fn restore_rows(
    db: &Connection,
    store: &HistoryStore,
    meta: &RowsMeta,
) -> Result<(usize, usize, usize)> {
    let Some(diff) = load_diff(store, meta)? else {
        return Err(KandriumError::new("row diff is missing"));
    };
    let snapshot_path = store.root().join(&meta.snapshot);
    let snapshot = rows::open_snapshot(&snapshot_path)?;

    // `foreign_keys` is a no-op inside a transaction, so set it first.
    db.pragma_update(None, "foreign_keys", "OFF")?;
    let outcome = replay(db, &snapshot, &diff);
    let _ = db.pragma_update(None, "foreign_keys", "ON");
    outcome
}

fn replay(db: &Connection, snapshot: &Connection, diff: &RowDiff) -> Result<(usize, usize, usize)> {
    db.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<(usize, usize, usize)> {
        // Two passes: delete every key in the diff first, then insert the start
        // rows. A single delete-then-insert pass can hit a unique index when two
        // rows swap a unique value (project identifiers do exactly that).
        for (name, table) in &diff.tables {
            let spec = spec_for(name)
                .ok_or_else(|| KandriumError(format!("{name} is not in the allowlist")))?;
            let delete_sql = delete_sql(spec);
            for change in &table.changes {
                db.execute(&delete_sql, params_from_iter(change.key.iter()))?;
            }
        }

        let (mut updated, mut inserted, mut deleted) = (0, 0, 0);
        for (name, table) in &diff.tables {
            let spec = spec_for(name)
                .ok_or_else(|| KandriumError(format!("{name} is not in the allowlist")))?;
            let (columns, start) = rows::table_rows(snapshot, spec)?;
            let insert_sql = insert_sql(spec, &columns);
            for change in &table.changes {
                match change.op {
                    ChangeOp::Insert => inserted += 1,
                    ChangeOp::Update | ChangeOp::Delete => {
                        let values: &Vec<Value> = start.get(&change.key).ok_or_else(|| {
                            KandriumError(format!(
                                "{name} {} is missing from the start snapshot",
                                change.key.join("/")
                            ))
                        })?;
                        db.execute(&insert_sql, params_from_iter(values.iter()))?;
                        if change.op == ChangeOp::Update {
                            updated += 1;
                        } else {
                            deleted += 1;
                        }
                    }
                }
            }
        }

        let mut check = db.prepare("PRAGMA foreign_key_check")?;
        if check.query([])?.next()?.is_some() {
            return Err(KandriumError::new(
                "undo would leave a foreign-key violation; rolled back",
            ));
        }
        Ok((updated, inserted, deleted))
    })();

    match result {
        Ok(counts) => {
            db.execute_batch("COMMIT")?;
            Ok(counts)
        }
        Err(error) => {
            db.execute_batch("ROLLBACK").ok();
            Err(error)
        }
    }
}

fn delete_sql(table: &TableSpec) -> String {
    let keys = table
        .keys
        .iter()
        .map(|key| format!("{} = ?", quote_ident(key)))
        .collect::<Vec<_>>()
        .join(" AND ");
    format!("DELETE FROM {} WHERE {keys}", quote_ident(table.name))
}

fn insert_sql(table: &TableSpec, columns: &[String]) -> String {
    let names = columns
        .iter()
        .map(|column| quote_ident(column))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (1..=columns.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {} ({names}) VALUES ({placeholders})",
        quote_ident(table.name)
    )
}

fn load_diff(store: &HistoryStore, meta: &RowsMeta) -> Result<Option<RowDiff>> {
    if meta.diff.is_empty() {
        return Ok(None);
    }
    let path = store.root().join(&meta.diff);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let diff = serde_json::from_str::<RowDiff>(&text).map_err(|error| {
        KandriumError(format!("cannot read row diff {}: {error}", path.display()))
    })?;
    Ok(Some(diff))
}

fn take_pre_undo_backup(
    db: &Connection,
    store: &HistoryStore,
    run_id: &str,
) -> Result<Option<std::path::PathBuf>> {
    fs::create_dir_all(store.db_dir())?;
    let target = store.db_dir().join(format!("{run_id}-pre-undo.db"));
    if target.exists() {
        fs::remove_file(&target)?;
    }
    rows::vacuum_into(db, &target)?;
    Ok(Some(target))
}

fn mark_state(
    db: &Connection,
    store: &HistoryStore,
    manifest: &mut Manifest,
    state: RunState,
) -> Result<()> {
    manifest.state = state;
    store.save(manifest)?;
    db.execute(
        "INSERT INTO run_undo (run_id, state, last_heartbeat_at, undone_at)
         VALUES (?1, ?2, 0, ?3)
         ON CONFLICT(run_id) DO UPDATE SET
             state = excluded.state,
             undone_at = excluded.undone_at",
        params![
            manifest.run_id,
            state.as_str(),
            matches!(state, RunState::Undone | RunState::Partial).then(now)
        ],
    )?;
    Ok(())
}

fn spec_for(name: &str) -> Option<&'static TableSpec> {
    CAPTURED_TABLES.iter().find(|table| table.name == name)
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
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
    use crate::db::migrations::run_schema;
    use crate::db::open_db;
    use crate::history::{ClosedBy, RootKind, Roots, RunContext, RunKind, row_begin};

    struct Fixture {
        _tmp: tempfile::TempDir,
        db: Connection,
        store: HistoryStore,
        roots: Roots,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.db");
        let mut db = open_db(&path).unwrap();
        run_schema(&mut db).unwrap();
        db.execute_batch(
            "INSERT INTO boards (id, title, position) VALUES ('b1', 'Board', 0), ('b2', 'Other', 1);
             INSERT INTO columns (id, board_id, title, position)
                 VALUES ('c1', 'b1', 'Todo', 0), ('c2', 'b1', 'Doing', 1);
             INSERT INTO cards (id, column_id, title, description, position)
                 VALUES ('k1', 'c1', 'One', '', 0), ('k2', 'c1', 'Two', '', 1);
             INSERT INTO all_card_order (card_id, status, position)
                 VALUES ('k1', 'todo', 0), ('k2', 'todo', 1);
             INSERT INTO area_card_order (board_id, card_id, status, position)
                 VALUES ('b1', 'k1', 'todo', 0), ('b1', 'k2', 'todo', 1);",
        )
        .unwrap();
        let roots = Roots {
            wiki: tmp.path().join("wiki"),
            notes: tmp.path().join("notes"),
            documents: tmp.path().join("documents"),
            projects: tmp.path().join("projects"),
        };
        for root in [&roots.wiki, &roots.notes, &roots.documents, &roots.projects] {
            std::fs::create_dir_all(root).unwrap();
        }
        let store = HistoryStore::for_db(&path);
        Fixture {
            _tmp: tmp,
            db,
            store,
            roots,
        }
    }

    fn close(f: &Fixture, run_id: &str) {
        let mut manifest = f.store.load(run_id).unwrap().unwrap();
        manifest.state = RunState::Committed;
        manifest.closed_by = Some(ClosedBy::Explicit);
        f.store.save(&manifest).unwrap();
    }

    fn record(f: &Fixture, run_id: &str, mutate: impl FnOnce()) {
        let session = row_begin(&f.db, run_id).unwrap();
        mutate();
        session.end(&f.db).unwrap();
        close(f, run_id);
    }

    fn title(f: &Fixture, id: &str) -> String {
        f.db.query_row("SELECT title FROM cards WHERE id = ?", [id], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[test]
    fn a_mixed_file_and_row_run_undoes_both() {
        let f = fixture();
        let note = f.roots.notes.join("note.md");
        std::fs::write(&note, "old").unwrap();
        let ctx = RunContext::new(f.store.clone(), "run1");
        let guard = ctx.capture(RootKind::Notes, &f.roots.notes, &note).unwrap();

        let session = row_begin(&f.db, "run1").unwrap();
        std::fs::write(&note, "new").unwrap();
        f.db.execute_batch(
            "UPDATE cards SET column_id = 'c2', position = 0 WHERE id = 'k1';
             UPDATE all_card_order SET status = 'doing', position = 0 WHERE card_id = 'k1';",
        )
        .unwrap();
        session.end(&f.db).unwrap();
        guard.commit(Some(b"new")).unwrap();
        close(&f, "run1");

        let report = undo_run(&f.db, &f.store, &f.roots, "run1", false).unwrap();
        assert!(!report.blocked, "{report:?}");
        assert_eq!(report.state, RunState::Undone);
        assert_eq!(report.files.restored, 1);
        assert_eq!(report.rows.updated, 2);
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "old");
        let (column, position): (String, i64) =
            f.db.query_row(
                "SELECT column_id, position FROM cards WHERE id = 'k1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((column.as_str(), position), ("c1", 0));
        let status: String =
            f.db.query_row(
                "SELECT status FROM all_card_order WHERE card_id = 'k1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "todo");
    }

    #[test]
    fn a_board_delete_restores_cascaded_children() {
        let f = fixture();
        let session = row_begin(&f.db, "run1").unwrap();
        f.db.execute("DELETE FROM boards WHERE id = 'b1'", [])
            .unwrap();
        let ended = session.end(&f.db).unwrap();
        close(&f, "run1");
        // boards + 2 columns + 2 cards + 2 all_card_order + 2 area_card_order.
        assert_eq!(ended.counts.deleted, 9);

        let report = undo_run(&f.db, &f.store, &f.roots, "run1", false).unwrap();
        assert_eq!(report.rows.deleted, 9);
        assert_eq!(title(&f, "k1"), "One");
        let columns: i64 =
            f.db.query_row(
                "SELECT COUNT(*) FROM columns WHERE board_id = 'b1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(columns, 2);
    }

    #[test]
    fn card_move_undo_restores_order_rows() {
        let f = fixture();
        let session = row_begin(&f.db, "run1").unwrap();
        f.db.execute_batch(
            "UPDATE cards SET column_id = 'c2' WHERE id = 'k1';
             UPDATE all_card_order SET status = 'doing' WHERE card_id = 'k1';",
        )
        .unwrap();
        session.end(&f.db).unwrap();
        close(&f, "run1");

        let report = undo_run(&f.db, &f.store, &f.roots, "run1", false).unwrap();
        assert_eq!(report.rows.updated, 2);
        assert_eq!(title(&f, "k1"), "One");
    }

    #[test]
    fn identifier_swap_restores_without_a_unique_failure() {
        let f = fixture();
        f.db.execute_batch(
            "INSERT INTO projects (id, name, identifier, board_id, position, folder_id)
                 VALUES ('p1', 'A', 'aaa', 'b1', 0, NULL), ('p2', 'B', 'bbb', 'b2', 1, NULL);",
        )
        .unwrap();
        let session = row_begin(&f.db, "run1").unwrap();
        f.db.execute_batch(
            "UPDATE projects SET identifier = 'zzz' WHERE id = 'p1';
             UPDATE projects SET identifier = 'aaa' WHERE id = 'p2';
             UPDATE projects SET identifier = 'bbb' WHERE id = 'p1';",
        )
        .unwrap();
        session.end(&f.db).unwrap();
        close(&f, "run1");

        let report = undo_run(&f.db, &f.store, &f.roots, "run1", false).unwrap();
        assert_eq!(report.rows.updated, 2);
        let identifiers: Vec<String> = {
            let mut stmt =
                f.db.prepare("SELECT identifier FROM projects ORDER BY id")
                    .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        assert_eq!(identifiers, vec!["aaa".to_string(), "bbb".to_string()]);
    }

    #[test]
    fn a_later_runs_untouched_rows_survive() {
        let f = fixture();
        record(&f, "run1", || {
            f.db.execute("UPDATE cards SET title = 'Run one' WHERE id = 'k1'", [])
                .unwrap();
        });
        record(&f, "run2", || {
            f.db.execute("UPDATE cards SET title = 'Later' WHERE id = 'k2'", [])
                .unwrap();
        });

        undo_run(&f.db, &f.store, &f.roots, "run1", false).unwrap();
        assert_eq!(title(&f, "k1"), "One");
        assert_eq!(title(&f, "k2"), "Later");
    }

    #[test]
    fn a_row_conflict_aborts_before_files_change() {
        let f = fixture();
        let note = f.roots.notes.join("note.md");
        std::fs::write(&note, "old").unwrap();
        let ctx = RunContext::new(f.store.clone(), "run1");
        let guard = ctx.capture(RootKind::Notes, &f.roots.notes, &note).unwrap();

        let session = row_begin(&f.db, "run1").unwrap();
        std::fs::write(&note, "new").unwrap();
        f.db.execute("UPDATE cards SET title = 'Run one' WHERE id = 'k1'", [])
            .unwrap();
        session.end(&f.db).unwrap();
        guard.commit(Some(b"new")).unwrap();
        close(&f, "run1");

        // A later edit to both the row and the file.
        f.db.execute("UPDATE cards SET title = 'User' WHERE id = 'k1'", [])
            .unwrap();
        std::fs::write(&note, "user edit").unwrap();

        let report = undo_run(&f.db, &f.store, &f.roots, "run1", false).unwrap();
        assert!(report.blocked);
        assert_eq!(report.rows.conflicts.len(), 1);
        assert_eq!(title(&f, "k1"), "User");
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "user edit");
    }

    #[test]
    fn a_contended_run_requires_force() {
        let f = fixture();
        let other = open_db(std::path::Path::new(f.db.path().unwrap())).unwrap();
        let session = row_begin(&f.db, "run1").unwrap();
        f.db.execute("UPDATE cards SET title = 'Run one' WHERE id = 'k1'", [])
            .unwrap();
        other
            .execute("UPDATE cards SET title = 'Other' WHERE id = 'k1'", [])
            .unwrap();
        let ended = session.end(&f.db).unwrap();
        assert!(ended.contended);
        close(&f, "run1");

        let blocked = undo_run(&f.db, &f.store, &f.roots, "run1", false).unwrap();
        assert!(blocked.blocked);
        assert!(
            blocked
                .rows
                .conflicts
                .iter()
                .any(|c| c.reason.contains("another connection"))
        );

        let forced = undo_run(&f.db, &f.store, &f.roots, "run1", true).unwrap();
        assert!(!forced.blocked);
        assert_eq!(title(&f, "k1"), "One");
    }

    #[test]
    fn schema_drift_refuses_even_with_force() {
        let f = fixture();
        record(&f, "run1", || {
            f.db.execute("UPDATE cards SET title = 'Run one' WHERE id = 'k1'", [])
                .unwrap();
        });
        f.db.execute(
            "ALTER TABLE cards ADD COLUMN extra TEXT NOT NULL DEFAULT ''",
            [],
        )
        .unwrap();

        let error = undo_run(&f.db, &f.store, &f.roots, "run1", true).unwrap_err();
        assert!(error.0.contains("schema drift"), "{}", error.0);
    }

    #[test]
    fn retry_from_files_restored_skips_the_file_phase() {
        let f = fixture();
        let note = f.roots.notes.join("note.md");
        std::fs::write(&note, "old").unwrap();
        let ctx = RunContext::new(f.store.clone(), "run1");
        let guard = ctx.capture(RootKind::Notes, &f.roots.notes, &note).unwrap();
        std::fs::write(&note, "new").unwrap();
        guard.commit(Some(b"new")).unwrap();

        // As if the file phase already ran: the pre-image is back on disk.
        std::fs::write(&note, "old").unwrap();
        let mut manifest = f.store.load("run1").unwrap().unwrap();
        manifest.state = RunState::FilesRestored;
        f.store.save(&manifest).unwrap();

        let report = undo_run(&f.db, &f.store, &f.roots, "run1", false).unwrap();
        assert_eq!(report.state, RunState::Undone);
        assert_eq!(report.files.restored, 0);
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "old");
    }

    #[test]
    fn a_second_undo_is_refused() {
        let f = fixture();
        record(&f, "run1", || {
            f.db.execute("UPDATE cards SET title = 'Run one' WHERE id = 'k1'", [])
                .unwrap();
        });
        undo_run(&f.db, &f.store, &f.roots, "run1", false).unwrap();
        let error = undo_run(&f.db, &f.store, &f.roots, "run1", false).unwrap_err();
        assert!(error.0.contains("already undone"), "{}", error.0);
    }

    #[test]
    fn an_open_run_cannot_be_undone() {
        let f = fixture();
        let manifest = Manifest::new("run1", RunKind::Explicit, "open task", 1);
        f.store.save(&manifest).unwrap();
        let error = undo_run(&f.db, &f.store, &f.roots, "run1", false).unwrap_err();
        assert!(error.0.contains("in progress"), "{}", error.0);
    }
}
