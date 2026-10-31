//! Database snapshots and integrity checks.
//!
//! Snapshots live next to the database file (`<db parent>/backups/`), so a
//! `KANDRIUM_DB` override keeps its backups self-contained. Two kinds are
//! taken: one before every schema migration and one per local calendar day.
//! Snapshots use `VACUUM INTO`, which folds the WAL into a single consistent
//! file without writing to the live database.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::{KandriumError, Result};

use super::migrations;

/// Label prefix for the snapshot taken right before a schema migration.
const PRE_MIGRATION_LABEL: &str = "pre-migration";
/// Label prefix for the once-per-day snapshot.
const DAILY_LABEL: &str = "daily";
/// How many pre-migration snapshots to retain.
const KEEP_PRE_MIGRATION: usize = 5;
/// How many daily snapshots to retain.
const KEEP_DAILY: usize = 7;

/// Backup directory for a database file: `<db parent>/backups/`.
pub fn backups_dir(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("backups")
}

/// Take a consistent snapshot of the database and return its path.
///
/// The file is named `<db-stem>-<label>-<YYYYMMDD-HHMMSS>.db`; the timestamp
/// comes from SQLite itself so no date formatting lives here.
pub fn snapshot(db: &Connection, db_path: &Path, label: &str) -> Result<PathBuf> {
    let dir = backups_dir(db_path);
    fs::create_dir_all(&dir)?;
    let stem = db_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("kandrium");
    let target = dir.join(format!("{stem}-{label}-{}.db", stamp(db)?));
    let target_sql = target.to_string_lossy().replace('\'', "''");
    db.execute_batch(&format!("VACUUM INTO '{target_sql}'"))?;
    Ok(target)
}

/// Snapshot when a migration is pending (always) or when today's daily
/// snapshot is missing (at most one per local day). Returns the new path.
pub fn snapshot_if_due(
    db: &Connection,
    db_path: &Path,
    migration_pending: bool,
) -> Result<Option<PathBuf>> {
    let dir = backups_dir(db_path);
    if migration_pending {
        let version = migrations::current_version(db)?;
        let label = format!("{PRE_MIGRATION_LABEL}-v{version}");
        let path = snapshot(db, db_path, &label)?;
        prune(&dir, PRE_MIGRATION_LABEL, KEEP_PRE_MIGRATION);
        return Ok(Some(path));
    }

    let today = date_stamp(db)?;
    if has_snapshot_for_date(&dir, &today) {
        return Ok(None);
    }
    let path = snapshot(db, db_path, DAILY_LABEL)?;
    prune(&dir, DAILY_LABEL, KEEP_DAILY);
    Ok(Some(path))
}

/// Fail when `PRAGMA quick_check` does not report `ok`. The database file and
/// its WAL sidecars are raw-copied into the backup directory first, because a
/// corrupt database cannot be snapshotted with `VACUUM INTO`.
pub fn check_integrity(db: &Connection, db_path: &Path) -> Result<()> {
    // A file that is not a database at all fails to prepare the pragma; treat
    // any failure here as corruption rather than a plain query error.
    let rows: Vec<String> = match query_quick_check(db) {
        Ok(rows) if rows.iter().all(|row| row == "ok") => return Ok(()),
        Ok(rows) => rows,
        Err(e) => vec![e.to_string()],
    };

    let dir = backups_dir(db_path);
    fs::create_dir_all(&dir)?;
    let stale = stamp(db).unwrap_or_else(|_| epoch_stamp());
    let stem = db_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("kandrium");
    let target = dir.join(format!("{stem}-corrupt-{stale}.db"));
    let mut saved = None;
    if db_path.exists() {
        fs::copy(db_path, &target)?;
        for suffix in ["-wal", "-shm"] {
            let side = db_path.with_extension(format!("db{suffix}"));
            if side.exists() {
                fs::copy(&side, target.with_extension(format!("db{suffix}")))?;
            }
        }
        saved = Some(target);
    }

    let detail = rows.join("; ");
    Err(match saved {
        Some(path) => KandriumError::new(format!(
            "database failed integrity check ({detail}); a copy was saved to {}",
            path.display()
        )),
        None => KandriumError::new(format!("database failed integrity check ({detail})")),
    })
}

/// Delete the oldest snapshots carrying `label` beyond `keep`. Ordering uses
/// the embedded `YYYYMMDD-HHMMSS` stamp (the last two name segments), so a
/// versioned label cannot distort the order.
fn prune(dir: &Path, label: &str, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| name.ends_with(".db") && name.contains(&format!("-{label}-")))
        .collect();
    if names.len() <= keep {
        return;
    }
    names.sort_by_key(|name| snapshot_key(name));
    let excess = names.len() - keep;
    for name in names.into_iter().take(excess) {
        fs::remove_file(dir.join(name)).ok();
    }
}

/// Order key for a snapshot name: `YYYYMMDD-HHMMSS`.
fn snapshot_key(name: &str) -> String {
    let stem = name.strip_suffix(".db").unwrap_or(name);
    let mut parts = stem.rsplitn(3, '-');
    let seconds = parts.next().unwrap_or("");
    let date = parts.next().unwrap_or("");
    format!("{date}-{seconds}")
}

/// True when a snapshot name carries `date` as a hyphen-delimited segment.
fn has_snapshot_for_date(dir: &Path, date: &str) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(".db") && name.contains(&format!("-{date}-")))
    })
}

fn query_quick_check(db: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = db.prepare("PRAGMA quick_check")?;
    stmt.query_map([], |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()
}

fn stamp(db: &Connection) -> Result<String> {
    Ok(db.query_row(
        "SELECT strftime('%Y%m%d-%H%M%S', 'now', 'localtime')",
        [],
        |r| r.get(0),
    )?)
}

fn date_stamp(db: &Connection) -> Result<String> {
    Ok(
        db.query_row("SELECT strftime('%Y%m%d', 'now', 'localtime')", [], |r| {
            r.get(0)
        })?,
    )
}

/// Last-resort timestamp for a database too corrupt to query.
fn epoch_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::bootstrap;
    use crate::db::connection::open_db;
    use crate::db::migrations::{SCHEMA_SQL, run_schema};

    fn snapshot_files(db_path: &Path, label: &str) -> Vec<PathBuf> {
        let dir = backups_dir(db_path);
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let marker = format!("-{label}-");
        entries
            .flatten()
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.ends_with(".db") && name.contains(&marker)
            })
            .map(|entry| entry.path())
            .collect()
    }

    #[test]
    fn pre_migration_snapshot_opens_at_old_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut db = open_db(&path).unwrap();
        db.execute_batch(SCHEMA_SQL).unwrap();
        db.execute_batch("INSERT INTO boards VALUES ('b1', 'Career', 0);")
            .unwrap();
        db.pragma_update(None, "user_version", 1).unwrap();

        bootstrap(&mut db, &path).unwrap();

        let snapshots = snapshot_files(&path, "pre-migration");
        assert_eq!(snapshots.len(), 1, "{snapshots:?}");
        let old = Connection::open(&snapshots[0]).unwrap();
        let version: i64 = old
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
        let boards: i64 = old
            .query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))
            .unwrap();
        assert_eq!(boards, 1);
    }

    #[test]
    fn daily_snapshot_runs_once_per_day() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut db = open_db(&path).unwrap();
        run_schema(&mut db).unwrap();

        bootstrap(&mut db, &path).unwrap();
        let first = snapshot_files(&path, "daily");
        assert_eq!(first.len(), 1, "{first:?}");

        bootstrap(&mut db, &path).unwrap();
        let second = snapshot_files(&path, "daily");
        assert_eq!(second.len(), 1, "{second:?}");
        assert_eq!(first[0], second[0]);
    }

    #[test]
    fn prune_keeps_newest() {
        let dir = tempfile::tempdir().unwrap();
        for day in 1..=8 {
            let name = format!("kandrium-daily-2026010{day}-120000.db");
            fs::write(dir.path().join(name), b"x").unwrap();
        }

        prune(dir.path(), "daily", 5);

        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 5, "{names:?}");
        assert!(!names.iter().any(|name| name.contains("20260101")));
        assert!(!names.iter().any(|name| name.contains("20260102")));
        assert!(!names.iter().any(|name| name.contains("20260103")));
    }

    #[test]
    fn integrity_check_rejects_non_database_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.db");
        fs::write(&path, b"this is not a sqlite database").unwrap();
        let db = Connection::open(&path).unwrap();

        let err = check_integrity(&db, &path).unwrap_err();
        assert!(err.0.contains("integrity check"), "{}", err.0);
        assert_eq!(snapshot_files(&path, "corrupt").len(), 1);
    }
}
