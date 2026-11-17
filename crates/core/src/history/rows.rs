//! Row capture: a start/end database snapshot pair and a derived table diff.
//!
//! A run takes a start snapshot lazily at its first row mutation, an end
//! snapshot at close, diffs the eleven app-owned tables, and deletes the end
//! snapshot once the diff is stored. The diff holds keys and row hashes only;
//! before values live in the start snapshot ([`crate::history`] §3B).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, params};

use crate::error::{KandriumError, Result};
use crate::fsutil;
use crate::wiki::hash_bytes;

use super::store::HistoryStore;
use super::{
    ChangeOp, MANIFEST_VERSION, Manifest, RowChange, RowCounts, RowDiff, RowsMeta, RunKind,
    TableDiff,
};

/// One captured table and its primary-key columns.
#[derive(Debug, Clone, Copy)]
pub struct TableSpec {
    pub name: &'static str,
    pub keys: &'static [&'static str],
}

/// The eleven app-owned tables captured by the row diff. Derived wiki tables
/// and `sqlite_*` are deliberately excluded: files own the wiki.
pub const CAPTURED_TABLES: &[TableSpec] = &[
    TableSpec {
        name: "boards",
        keys: &["id"],
    },
    TableSpec {
        name: "columns",
        keys: &["id"],
    },
    TableSpec {
        name: "cards",
        keys: &["id"],
    },
    TableSpec {
        name: "all_card_order",
        keys: &["card_id"],
    },
    TableSpec {
        name: "area_card_order",
        keys: &["board_id", "card_id"],
    },
    TableSpec {
        name: "projects",
        keys: &["id"],
    },
    TableSpec {
        name: "project_folders",
        keys: &["id"],
    },
    TableSpec {
        name: "project_paths",
        keys: &["id"],
    },
    TableSpec {
        name: "clocks",
        keys: &["id"],
    },
    TableSpec {
        name: "clock_people",
        keys: &["id"],
    },
    TableSpec {
        name: "clock_travels",
        keys: &["id"],
    },
];

/// The captured table allowlist, exposed so a test can compare it against
/// `sqlite_master` when the app gains a table.
pub fn captured_tables() -> &'static [TableSpec] {
    CAPTURED_TABLES
}

/// A run's row phase, open between the first row mutation and the run close.
#[derive(Debug)]
pub struct RowSession {
    run_id: String,
    store: HistoryStore,
    start: std::path::PathBuf,
    user_version: i64,
    schema: BTreeMap<String, String>,
    data_version: i64,
    snapshot_bytes: u64,
    captured_at: i64,
}

/// Result of closing the row phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowEnd {
    pub counts: RowCounts,
    /// Another connection committed inside the window.
    pub contended: bool,
    pub snapshot_bytes: u64,
    /// False when the diff failed; the end snapshot is kept for a retry.
    pub restorable: bool,
}

/// Take the start snapshot at the run's first row mutation.
///
/// Must run outside any transaction: `VACUUM INTO` cannot run inside one. The
/// caller invokes this lazily, so a run that mutates no rows takes no snapshot.
pub fn row_begin(db: &Connection, run_id: &str) -> Result<RowSession> {
    if !db.is_autocommit() {
        return Err(KandriumError::new(
            "row history needs a connection outside a transaction",
        ));
    }
    let store = store_for(db)?;
    fs::create_dir_all(store.db_dir())?;
    let start = store.start_snapshot_path(run_id)?;
    if start.exists() {
        fs::remove_file(&start)?;
    }
    vacuum_into(db, &start)?;

    let user_version = read_user_version(db)?;
    let schema = schema_hashes(db)?;
    let data_version = data_version(db)?;
    let snapshot_bytes = file_size(&start);

    heartbeat(db, run_id)?;

    Ok(RowSession {
        run_id: run_id.to_string(),
        store,
        start,
        user_version,
        schema,
        data_version,
        snapshot_bytes,
        captured_at: now(),
    })
}

impl RowSession {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn snapshot_bytes(&self) -> u64 {
        self.snapshot_bytes
    }

    /// Take the end snapshot, diff the allowlist, store the diff and delete the
    /// end snapshot. A failed diff keeps the end snapshot and records the run
    /// as not row-restorable so it can be retried.
    pub fn end(self, db: &Connection) -> Result<RowEnd> {
        fs::create_dir_all(self.store.db_dir())?;
        let end = self.store.end_snapshot_path(&self.run_id)?;
        if end.exists() {
            fs::remove_file(&end)?;
        }
        vacuum_into(db, &end)?;
        let contended = data_version(db)? != self.data_version;

        let diff = diff_snapshots(&self.start, &end, self.user_version);
        let mut meta = RowsMeta {
            snapshot: relative(&self.store, &self.start),
            snapshot_bytes: self.snapshot_bytes,
            user_version: self.user_version,
            schema: self.schema.clone(),
            diff: String::new(),
            counts: RowCounts::default(),
            contended,
            restorable: diff.is_ok(),
        };

        let end_result = match diff {
            Ok(diff) => {
                let diff_path = self.store.diff_path(&self.run_id)?;
                let json = serde_json::to_vec_pretty(&diff)
                    .map_err(|error| KandriumError(format!("cannot encode row diff: {error}")))?;
                fsutil::write_atomic(&diff_path, json)?;
                meta.diff = relative(&self.store, &diff_path);
                meta.counts = RowCounts {
                    inserted: diff
                        .tables
                        .values()
                        .map(|t| {
                            t.changes
                                .iter()
                                .filter(|c| c.op == ChangeOp::Insert)
                                .count()
                        })
                        .sum(),
                    updated: diff
                        .tables
                        .values()
                        .map(|t| {
                            t.changes
                                .iter()
                                .filter(|c| c.op == ChangeOp::Update)
                                .count()
                        })
                        .sum(),
                    deleted: diff
                        .tables
                        .values()
                        .map(|t| {
                            t.changes
                                .iter()
                                .filter(|c| c.op == ChangeOp::Delete)
                                .count()
                        })
                        .sum(),
                };
                true
            }
            Err(error) => {
                crate::logging::warn!("row diff failed for run {}: {error}", self.run_id);
                false
            }
        };

        self.record_meta(db, &meta)?;
        if end_result {
            // The diff carries after-hashes; the end bytes are no longer needed.
            fs::remove_file(&end).ok();
        }
        heartbeat(db, &self.run_id)?;

        Ok(RowEnd {
            counts: meta.counts,
            contended,
            snapshot_bytes: meta.snapshot_bytes,
            restorable: end_result,
        })
    }

    /// Write the row metadata into the manifest and its `run_row_changes`
    /// mirror.
    fn record_meta(&self, db: &Connection, meta: &RowsMeta) -> Result<()> {
        let mut manifest = match self.store.load(&self.run_id)? {
            Some(manifest) => manifest,
            None => Manifest::new(
                self.run_id.clone(),
                RunKind::Implicit,
                self.run_id.clone(),
                self.captured_at,
            ),
        };
        manifest.rows = Some(meta.clone());
        manifest.heartbeat_at = now();
        self.store.save(&manifest)?;

        let schema_json = serde_json::to_string(&meta.schema)
            .map_err(|error| KandriumError(format!("cannot encode table schema: {error}")))?;
        db.execute(
            "INSERT INTO run_row_changes
                 (run_id, label, kind, closed_by, snapshot, snapshot_bytes, user_version,
                  schema_json, diff_path, inserted, updated, deleted, contended, restorable,
                  captured_at)
             VALUES (?1, ?2, ?3, '', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(run_id) DO UPDATE SET
                 label = excluded.label,
                 kind = excluded.kind,
                 snapshot = excluded.snapshot,
                 snapshot_bytes = excluded.snapshot_bytes,
                 user_version = excluded.user_version,
                 schema_json = excluded.schema_json,
                 diff_path = excluded.diff_path,
                 inserted = excluded.inserted,
                 updated = excluded.updated,
                 deleted = excluded.deleted,
                 contended = excluded.contended,
                 restorable = excluded.restorable,
                 captured_at = excluded.captured_at",
            params![
                self.run_id,
                manifest.label,
                manifest.kind.as_str(),
                meta.snapshot,
                meta.snapshot_bytes as i64,
                meta.user_version,
                schema_json,
                meta.diff,
                meta.counts.inserted as i64,
                meta.counts.updated as i64,
                meta.counts.deleted as i64,
                meta.contended as i64,
                meta.restorable as i64,
                self.captured_at,
            ],
        )?;
        Ok(())
    }
}

/// Compare the start and end snapshots over the allowlist.
pub fn diff_snapshots(start: &Path, end: &Path, user_version: i64) -> Result<RowDiff> {
    let start_db = open_snapshot(start)?;
    let end_db = open_snapshot(end)?;
    let mut tables = BTreeMap::new();
    for table in CAPTURED_TABLES {
        let before = read_table(&start_db, table)?;
        let after = read_table(&end_db, table)?;
        let changes = diff_keys(before, after);
        if !changes.is_empty() {
            tables.insert(table.name.to_string(), TableDiff { changes });
        }
    }
    Ok(RowDiff {
        version: MANIFEST_VERSION,
        user_version,
        tables,
    })
}

fn diff_keys(
    before: BTreeMap<Vec<String>, String>,
    after: BTreeMap<Vec<String>, String>,
) -> Vec<RowChange> {
    let mut changes: BTreeMap<Vec<String>, RowChange> = BTreeMap::new();
    for (key, before_hash) in &before {
        match after.get(key) {
            None => {
                changes.insert(
                    key.clone(),
                    RowChange {
                        key: key.clone(),
                        op: ChangeOp::Delete,
                        before_hash: Some(before_hash.clone()),
                        after_hash: None,
                    },
                );
            }
            Some(after_hash) if after_hash != before_hash => {
                changes.insert(
                    key.clone(),
                    RowChange {
                        key: key.clone(),
                        op: ChangeOp::Update,
                        before_hash: Some(before_hash.clone()),
                        after_hash: Some(after_hash.clone()),
                    },
                );
            }
            Some(_) => {}
        }
    }
    for (key, after_hash) in &after {
        if !before.contains_key(key) {
            changes.insert(
                key.clone(),
                RowChange {
                    key: key.clone(),
                    op: ChangeOp::Insert,
                    before_hash: None,
                    after_hash: Some(after_hash.clone()),
                },
            );
        }
    }
    changes.into_values().collect()
}

/// Read one table into `key -> row hash`.
pub(crate) fn read_table(
    db: &Connection,
    table: &TableSpec,
) -> Result<BTreeMap<Vec<String>, String>> {
    let columns = table_columns(db, table.name)?;
    let column_list = columns
        .iter()
        .map(|column| quote_ident(&column.name))
        .collect::<Vec<_>>()
        .join(", ");

    // Tables without a declared primary key fall back to `rowid`.
    let (select, offset, key_indexes) = if table.keys.is_empty() {
        (format!("rowid, {column_list}"), 1, vec![0])
    } else {
        let indexes = table
            .keys
            .iter()
            .map(|key| {
                columns
                    .iter()
                    .position(|column| &column.name == key)
                    .ok_or_else(|| KandriumError(format!("{} has no column {key}", table.name)))
            })
            .collect::<Result<Vec<_>>>()?;
        (column_list, 0, indexes)
    };

    let sql = format!("SELECT {select} FROM {}", quote_ident(table.name));
    let mut stmt = db.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let mut out = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let mut key = Vec::with_capacity(key_indexes.len());
        for index in &key_indexes {
            key.push(key_component(row.get_ref(*index)?));
        }
        out.insert(key, hash_row(row, columns.len(), offset)?);
    }
    Ok(out)
}

fn hash_row(row: &rusqlite::Row<'_>, columns: usize, offset: usize) -> Result<String> {
    let mut bytes = Vec::new();
    for index in offset..offset + columns {
        match row.get_ref(index)? {
            ValueRef::Null => bytes.push(0),
            ValueRef::Integer(value) => {
                bytes.push(1);
                bytes.extend_from_slice(&value.to_be_bytes());
            }
            ValueRef::Real(value) => {
                bytes.push(2);
                bytes.extend_from_slice(&value.to_bits().to_be_bytes());
            }
            ValueRef::Text(value) => {
                bytes.push(3);
                bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
                bytes.extend_from_slice(value);
            }
            ValueRef::Blob(value) => {
                bytes.push(4);
                bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
                bytes.extend_from_slice(value);
            }
        }
    }
    Ok(hash_bytes(&bytes))
}

fn key_component(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => String::new(),
        ValueRef::Integer(value) => value.to_string(),
        ValueRef::Real(value) => value.to_string(),
        ValueRef::Text(value) => String::from_utf8_lossy(value).into_owned(),
        ValueRef::Blob(value) => value.iter().map(|byte| format!("{byte:02x}")).collect(),
    }
}

#[derive(Debug, Clone)]
struct ColumnInfo {
    name: String,
    declared_type: String,
    not_null: bool,
    default: Option<String>,
    pk: i64,
}

fn table_columns(db: &Connection, table: &str) -> Result<Vec<ColumnInfo>> {
    let mut stmt = db.prepare(&format!("PRAGMA table_info({})", quote_ident(table)))?;
    let columns = stmt
        .query_map([], |row| {
            Ok(ColumnInfo {
                name: row.get(1)?,
                declared_type: row.get(2)?,
                not_null: row.get::<_, i64>(3)? != 0,
                default: row.get(4)?,
                pk: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(columns)
}

/// Stable hash of every allowlisted table's `PRAGMA table_info`.
pub(crate) fn schema_hashes(db: &Connection) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for table in CAPTURED_TABLES {
        let columns = table_columns(db, table.name)?;
        let mut canonical = String::new();
        for column in columns {
            canonical.push_str(&format!(
                "{}|{}|{}|{}|{};",
                column.name,
                column.declared_type,
                column.not_null,
                column.default.unwrap_or_default(),
                column.pk
            ));
        }
        out.insert(table.name.to_string(), hash_bytes(canonical.as_bytes()));
    }
    Ok(out)
}

pub(crate) fn open_snapshot(path: &Path) -> Result<Connection> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}

/// Snapshot one table as column names plus keyed owned values, for row restore.
pub(crate) fn table_rows(
    db: &Connection,
    table: &TableSpec,
) -> Result<(
    Vec<String>,
    BTreeMap<Vec<String>, Vec<rusqlite::types::Value>>,
)> {
    let columns = table_columns(db, table.name)?;
    let names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
    let key_indexes: Vec<usize> = table
        .keys
        .iter()
        .map(|key| {
            columns
                .iter()
                .position(|column| &column.name == key)
                .ok_or_else(|| KandriumError(format!("{} has no column {key}", table.name)))
        })
        .collect::<Result<_>>()?;
    let select = names
        .iter()
        .map(|name| quote_ident(name))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = db.prepare(&format!("SELECT {select} FROM {}", quote_ident(table.name)))?;
    let mut rows = stmt.query([])?;
    let mut out = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let mut key = Vec::with_capacity(key_indexes.len());
        for index in &key_indexes {
            key.push(key_component(row.get_ref(*index)?));
        }
        let mut values = Vec::with_capacity(names.len());
        for index in 0..names.len() {
            values.push(value_of(row.get_ref(index)?));
        }
        out.insert(key, values);
    }
    Ok((names, out))
}

fn value_of(value: ValueRef<'_>) -> rusqlite::types::Value {
    match value {
        ValueRef::Null => rusqlite::types::Value::Null,
        ValueRef::Integer(value) => rusqlite::types::Value::Integer(value),
        ValueRef::Real(value) => rusqlite::types::Value::Real(value),
        ValueRef::Text(value) => {
            rusqlite::types::Value::Text(String::from_utf8_lossy(value).into_owned())
        }
        ValueRef::Blob(value) => rusqlite::types::Value::Blob(value.to_vec()),
    }
}

pub(crate) fn vacuum_into(db: &Connection, target: &Path) -> Result<()> {
    let target_sql = target.to_string_lossy().replace('\'', "''");
    db.execute_batch(&format!("VACUUM INTO '{target_sql}'"))?;
    Ok(())
}

pub(crate) fn read_user_version(db: &Connection) -> Result<i64> {
    Ok(db.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

pub(crate) fn data_version(db: &Connection) -> Result<i64> {
    Ok(db.query_row("PRAGMA data_version", [], |row| row.get(0))?)
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn store_for(db: &Connection) -> Result<HistoryStore> {
    let path = db
        .path()
        .ok_or_else(|| KandriumError::new("row history needs a file-backed database"))?;
    if path.is_empty() {
        return Err(KandriumError::new(
            "row history needs a file-backed database",
        ));
    }
    Ok(HistoryStore::for_db(Path::new(path)))
}

fn relative(store: &HistoryStore, path: &Path) -> String {
    path.strip_prefix(store.root())
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

/// Refresh the open run's heartbeat. A run without a `run_undo` row (an
/// unopened implicit run) is a no-op.
fn heartbeat(db: &Connection, run_id: &str) -> Result<()> {
    db.execute(
        "UPDATE run_undo SET last_heartbeat_at = ?1 WHERE run_id = ?2",
        params![now(), run_id],
    )?;
    Ok(())
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

    struct Fixture {
        _tmp: tempfile::TempDir,
        db: Connection,
        run_id: String,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let mut db = open_db(&tmp.path().join("t.db")).unwrap();
        run_schema(&mut db).unwrap();
        db.execute_batch(
            "INSERT INTO boards (id, title, position) VALUES ('b1', 'Board', 0);
             INSERT INTO columns (id, board_id, title, position) VALUES ('c1', 'b1', 'To Do', 0);
             INSERT INTO cards (id, column_id, title, description, position)
                 VALUES ('k1', 'c1', 'One', '', 0), ('k2', 'c1', 'Two', '', 1);
             INSERT INTO all_card_order (card_id, status, position) VALUES ('k1', 'todo', 0), ('k2', 'todo', 1);
             INSERT INTO area_card_order (board_id, card_id, status, position)
                 VALUES ('b1', 'k1', 'todo', 0), ('b1', 'k2', 'todo', 1);",
        )
        .unwrap();
        Fixture {
            _tmp: tmp,
            db,
            run_id: "run1".to_string(),
        }
    }

    fn diff(f: &Fixture) -> RowDiff {
        let store = HistoryStore::for_db(Path::new(f.db.path().unwrap()));
        let path = store.diff_path(&f.run_id).unwrap();
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn snapshots_and_diff_detect_inserts_updates_deletes() {
        let f = fixture();
        let session = row_begin(&f.db, &f.run_id).unwrap();
        assert_eq!(session.snapshot_bytes(), file_size(&session.start));
        assert!(session.start.exists());

        f.db.execute_batch(
            "INSERT INTO cards (id, column_id, title, description, position)
                 VALUES ('k3', 'c1', 'Three', '', 2);
             UPDATE cards SET title = 'Two v2' WHERE id = 'k2';
             DELETE FROM cards WHERE id = 'k1';",
        )
        .unwrap();
        let ended = session.end(&f.db).unwrap();
        assert!(ended.restorable);
        assert!(!ended.contended);
        // Deleting k1 also cascades into both order tables.
        assert_eq!(
            ended.counts,
            RowCounts {
                inserted: 1,
                updated: 1,
                deleted: 3
            }
        );

        let diff = diff(&f);
        let cards = &diff.tables["cards"].changes;
        assert_eq!(cards.len(), 3);
        assert!(
            cards
                .iter()
                .any(|c| c.key == vec!["k3"] && c.op == ChangeOp::Insert)
        );
        assert!(
            cards
                .iter()
                .any(|c| c.key == vec!["k2"] && c.op == ChangeOp::Update)
        );
        assert!(
            cards
                .iter()
                .any(|c| c.key == vec!["k1"] && c.op == ChangeOp::Delete)
        );
        assert_eq!(diff.user_version, crate::db::SCHEMA_VERSION);

        // The end snapshot is deleted after the diff; the start one remains.
        let store = HistoryStore::for_db(Path::new(f.db.path().unwrap()));
        assert!(store.start_snapshot_path(&f.run_id).unwrap().exists());
        assert!(!store.end_snapshot_path(&f.run_id).unwrap().exists());

        // The manifest and the mirror both carry the metadata.
        let manifest = store.load(&f.run_id).unwrap().unwrap();
        assert_eq!(manifest.rows.as_ref().unwrap().counts, ended.counts);
        assert_eq!(
            manifest.rows.as_ref().unwrap().user_version,
            crate::db::SCHEMA_VERSION
        );
        assert_eq!(
            manifest.rows.as_ref().unwrap().schema.len(),
            CAPTURED_TABLES.len()
        );
        let (inserted, contended): (i64, i64) =
            f.db.query_row(
                "SELECT inserted, contended FROM run_row_changes WHERE run_id = 'run1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(inserted, 1);
        assert_eq!(contended, 0);
    }

    #[test]
    fn a_board_delete_reports_cascaded_children() {
        let f = fixture();
        let session = row_begin(&f.db, &f.run_id).unwrap();
        f.db.execute("DELETE FROM boards WHERE id = 'b1'", [])
            .unwrap();
        session.end(&f.db).unwrap();

        let diff = diff(&f);
        for table in [
            "boards",
            "columns",
            "cards",
            "all_card_order",
            "area_card_order",
        ] {
            let changes = &diff.tables[table].changes;
            assert!(
                changes.iter().all(|change| change.op == ChangeOp::Delete),
                "{table}: {changes:?}"
            );
            assert!(!changes.is_empty(), "{table} has no deletes");
        }
    }

    #[test]
    fn two_connections_set_contended() {
        let f = fixture();
        let other = open_db(Path::new(f.db.path().unwrap())).unwrap();
        let session = row_begin(&f.db, &f.run_id).unwrap();
        other
            .execute("UPDATE cards SET title = 'Other' WHERE id = 'k1'", [])
            .unwrap();
        let ended = session.end(&f.db).unwrap();
        assert!(ended.contended);
        assert!(
            diff(&f).tables["cards"]
                .changes
                .iter()
                .any(|change| { change.key == vec!["k1"] && change.op == ChangeOp::Update })
        );
    }

    #[test]
    fn no_row_session_means_no_snapshot() {
        let f = fixture();
        let store = HistoryStore::for_db(Path::new(f.db.path().unwrap()));
        assert!(!store.start_snapshot_path(&f.run_id).unwrap().exists());
        assert!(store.load(&f.run_id).unwrap().is_none());
    }

    #[test]
    fn allowlist_matches_the_captured_tables() {
        let f = fixture();
        let names: Vec<String> = CAPTURED_TABLES
            .iter()
            .map(|table| table.name.to_string())
            .collect();
        for name in &names {
            let exists: i64 =
                f.db.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
                    [name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "missing table {name}");
        }
        assert!(!names.iter().any(|name| name.starts_with("wiki_")));
    }
}
