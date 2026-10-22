//! Database open/path resolution, schema runner, and id generation.
//!
//! Port of `src/main/services/database-service.ts` (minus the Drizzle runner —
//! the fresh Rust database is versioned by `rusqlite_migration` via
//! `PRAGMA user_version` instead).

use std::path::PathBuf;
use std::time::Duration;

use rand::Rng;
use rusqlite::Connection;
use rusqlite_migration::{HookError, M, Migrations};

use crate::error::{KandriumError, Result};

/// Embedded schema, applied idempotently.
pub const SCHEMA_SQL: &str = include_str!("schema.sql");
/// Additive v2 step (Projects). Separate from `SCHEMA_SQL` so the v1 shape
/// stays byte-identical to the legacy Drizzle schema.
pub const SCHEMA_V2_SQL: &str = include_str!("schema_v2.sql");
/// Additive v3 step (project identifiers + card ↔ project links).
pub const SCHEMA_V3_SQL: &str = include_str!("schema_v3.sql");
/// v4 step: normalize project identifiers to their canonical lowercase form.
pub const SCHEMA_V4_SQL: &str = include_str!("schema_v4.sql");
/// Latest schema version. Must match the number of entries in the migration set.
pub const SCHEMA_VERSION: i64 = 4;

/// Generate an id in the same format as the TS app: 12 random bytes as hex
/// (`crypto.randomBytes(12).toString('hex')` → 24 lowercase hex chars).
pub fn uid() -> String {
    let mut bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut bytes);
    let mut s = String::with_capacity(24);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Resolve the Kandrium database path:
/// 1. `KANDRIUM_DB` env var (explicit override)
/// 2. `<data_dir>/kandrium/kandrium.db`
pub fn resolve_db_path() -> PathBuf {
    if let Some(p) = std::env::var_os("KANDRIUM_DB") {
        return PathBuf::from(p);
    }
    data_dir().join("kandrium").join("kandrium.db")
}

/// Resolve the legacy Electron app database path (read source for the importer):
/// 1. `KANDRIUM_OLD_DB` env var
/// 2. `<data_dir>/planly/planly.db` (same default the Electron app uses)
pub fn resolve_old_db_path() -> PathBuf {
    if let Some(p) = std::env::var_os("KANDRIUM_OLD_DB") {
        return PathBuf::from(p);
    }
    data_dir().join("planly").join("planly.db")
}

fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// If the Kandrium database does not exist yet but the pre-rebrand database
/// (`<data_dir>/planly-rs/planly.db`) does, snapshot the old database into the
/// new location so existing boards and cards carry over after the rename.
/// Returns the path migrated from when a migration happened.
///
/// Only the default location participates — a `KANDRIUM_DB` override is left
/// untouched. The source is staged in a scratch directory (the same approach
/// as the legacy importer) and copied with `VACUUM INTO`, which folds the WAL
/// into one consistent file and never writes to the user's original database.
pub fn migrate_legacy_db() -> Result<Option<PathBuf>> {
    if std::env::var_os("KANDRIUM_DB").is_some() {
        return Ok(None);
    }
    let target = resolve_db_path();
    if target.exists() {
        return Ok(None);
    }
    let legacy = data_dir().join("planly-rs").join("planly.db");
    if !legacy.exists() {
        return Ok(None);
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let scratch = std::env::temp_dir().join(format!("kandrium-migrate-{}", uid()));
    std::fs::create_dir_all(&scratch)?;
    let staged = scratch.join("old.db");
    let result = (|| {
        std::fs::copy(&legacy, &staged)?;
        for suffix in ["-wal", "-shm"] {
            let side = legacy.with_extension(format!("db{suffix}"));
            if side.exists() {
                std::fs::copy(side, staged.with_extension(format!("db{suffix}")))?;
            }
        }
        let src = Connection::open(&staged)?;
        // Escape single quotes for the SQLite filename string literal.
        let target_sql = target.to_string_lossy().replace('\'', "''");
        src.execute_batch(&format!("VACUUM INTO '{target_sql}'"))?;
        Ok::<(), KandriumError>(())
    })();

    std::fs::remove_dir_all(&scratch).ok();
    if result.is_err() {
        // VACUUM INTO writes the target incrementally and leaves the partial
        // file behind on failure. Remove it (and any WAL sidecars) so the next
        // boot doesn't mistake the half-written file for a completed migration
        // and initialize an empty schema over it.
        for path in [
            target.clone(),
            target.with_extension("db-wal"),
            target.with_extension("db-shm"),
        ] {
            std::fs::remove_file(path).ok();
        }
    }
    result?;
    Ok(Some(legacy))
}

/// Open a Kandrium SQLite database with the standard pragmas.
/// WAL + busy_timeout allow the app and MCP server processes to share one DB.
pub fn open_db(path: &std::path::Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db = Connection::open(path)?;
    // journal_mode returns a row, so it must be queried, not just executed.
    let _: String = db.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    db.busy_timeout(Duration::from_millis(5000))?;
    Ok(db)
}

/// The ordered migration set. `rusqlite_migration` tracks the applied version
/// in `PRAGMA user_version`, so v1 databases upgrade through only the missing
/// steps and a database from a future schema is rejected.
fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(SCHEMA_SQL),
        M::up(SCHEMA_V2_SQL),
        M::up_with_hook(SCHEMA_V3_SQL, |tx| {
            backfill_v3(tx).map_err(|e| HookError::Hook(e.0))
        }),
        M::up(SCHEMA_V4_SQL),
    ])
}

/// Apply all pending schema migrations atomically.
///
/// The v3 step needs Rust-side identifier generation, so it runs as a
/// migration hook after its SQL. `rusqlite_migration` bumps `user_version`
/// once the whole run commits; a database already at the latest version is a
/// no-op, and a database ahead of this binary returns an error.
pub fn run_schema(db: &mut Connection) -> Result<()> {
    match migrations().to_latest(db) {
        Ok(()) => Ok(()),
        Err(first) => {
            // The app and MCP server can boot simultaneously. The winner
            // commits the migrations; the loser can then fail on the now
            // non-idempotent v3 ALTERs even though the schema is current.
            // A retry sees the winner's version and is a no-op.
            match migrations().to_latest(db) {
                Ok(()) => Ok(()),
                Err(second) => Err(KandriumError(format!(
                    "schema migration failed: {first} (retry also failed: {second})"
                ))),
            }
        }
    }
}

/// Fill the v3 columns for pre-existing rows: derive a unique identifier for
/// every project, and point cards on project boards at their project. Both
/// statements are idempotent (they only touch empty/NULL values).
fn backfill_v3(db: &Connection) -> Result<()> {
    let projects: Vec<(String, String)> = {
        let mut stmt = db.prepare(
            "SELECT id, name FROM projects WHERE identifier = '' \
             ORDER BY position, name COLLATE NOCASE",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (id, name) in projects {
        let identifier = crate::projects::suggest_identifier(db, &name, Some(&id))?;
        db.execute(
            "UPDATE projects SET identifier = ? WHERE id = ?",
            rusqlite::params![identifier, id],
        )?;
    }

    db.execute(
        "UPDATE cards SET project_id = (\
             SELECT p.id FROM projects p JOIN columns col ON col.board_id = p.board_id \
             WHERE col.id = cards.column_id\
         ) \
         WHERE project_id IS NULL AND column_id IN (\
             SELECT col.id FROM columns col JOIN projects p ON p.board_id = col.board_id\
         )",
        [],
    )?;
    Ok(())
}

/// True when the database has at least one board (i.e. seed/import already ran).
pub fn has_boards(db: &Connection) -> Result<bool> {
    let count: i64 = db.query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))?;
    Ok(count > 0)
}

/// Convenience: open + schema + (conditionally) seed, the standard bootstrap
/// shared by the app and MCP binaries.
pub fn bootstrap(db: &mut Connection) -> Result<()> {
    run_schema(db)?;
    crate::seed::seed(db)
}

/// Parse a `labels` JSON column into a vector, defaulting to empty on null-ish
/// input (mirrors `JSON.parse(r.labels || '[]')`).
pub fn parse_labels(json: &str) -> Result<Vec<i64>> {
    if json.trim().is_empty() {
        return Ok(vec![]);
    }
    serde_json::from_str(json).map_err(|e| KandriumError(format!("invalid labels JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_format() {
        let id = uid();
        assert_eq!(id.len(), 24);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(uid(), uid());
    }

    #[test]
    fn parse_labels_cases() {
        assert_eq!(parse_labels("[]").unwrap(), vec![] as Vec<i64>);
        assert_eq!(parse_labels("[1,4]").unwrap(), vec![1, 4]);
        assert_eq!(parse_labels("").unwrap(), vec![] as Vec<i64>);
        assert!(parse_labels("nope").is_err());
    }

    #[test]
    fn migrations_validate() {
        migrations().validate().unwrap();
    }

    #[test]
    fn fresh_database_is_at_latest_version() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("t.db")).unwrap();
        run_schema(&mut db).unwrap();

        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn future_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("t.db")).unwrap();
        run_schema(&mut db).unwrap();
        db.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();

        let err = run_schema(&mut db).unwrap_err();
        assert!(err.0.contains("schema migration failed"), "{}", err.0);
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION + 1);
    }

    /// A v1 database (user_version = 1, only the v1 tables) upgrades through
    /// the additive batches without touching existing rows.
    #[test]
    fn v1_database_upgrades_to_latest_and_stays_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("t.db")).unwrap();

        // Minimal v1 database with one seeded-style board/column/card.
        db.execute_batch(SCHEMA_SQL).unwrap();
        db.execute_batch(
            "INSERT INTO boards VALUES ('b1', 'Career', 0);
             INSERT INTO columns VALUES ('c1', 'b1', 'To Do', 0);
             INSERT INTO cards (id, column_id, title, description, position)
                 VALUES ('k1', 'c1', 'Task', '', 0);",
        )
        .unwrap();
        db.pragma_update(None, "user_version", 1).unwrap();

        run_schema(&mut db).unwrap();
        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 4);
        // New tables exist.
        let tables: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' \
                 AND name IN ('project_folders', 'projects')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 2);
        // Existing rows are untouched.
        let cards: i64 = db
            .query_row("SELECT COUNT(*) FROM cards", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cards, 1);

        // Re-running is a no-op.
        run_schema(&mut db).unwrap();
        let cards: i64 = db
            .query_row("SELECT COUNT(*) FROM cards", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cards, 1);
    }

    /// A v2 database gains identifiers (deduped) and project links for cards
    /// already sitting on project boards.
    #[test]
    fn v2_database_backfills_v3_columns() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("t.db")).unwrap();
        db.execute_batch(SCHEMA_SQL).unwrap();
        db.execute_batch(SCHEMA_V2_SQL).unwrap();
        db.execute_batch(
            "INSERT INTO boards VALUES ('b1', 'Kandrium', 0);
             INSERT INTO boards VALUES ('b2', 'Kandrium Two', 1);
             INSERT INTO columns VALUES ('c1', 'b1', 'To Do', 0);
             INSERT INTO projects
                 (id, folder_id, name, description, status, board_id, position, created_at, updated_at)
                 VALUES ('p1', NULL, 'Kandrium', '', 'active', 'b1', 0, 1, 1);
             INSERT INTO projects
                 (id, folder_id, name, description, status, board_id, position, created_at, updated_at)
                 VALUES ('p2', NULL, 'Kandrium', '', 'active', 'b2', 1, 1, 1);
             INSERT INTO cards (id, column_id, title, description, position)
                 VALUES ('k1', 'c1', 'Task', '', 0);",
        )
        .unwrap();
        db.pragma_update(None, "user_version", 2).unwrap();

        run_schema(&mut db).unwrap();

        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 4);

        let ident = |db: &Connection, id: &str| -> String {
            db.query_row("SELECT identifier FROM projects WHERE id = ?", [id], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert_eq!(ident(&db, "p1"), "kan");
        assert_eq!(ident(&db, "p2"), "kan2");

        let project_id: Option<String> = db
            .query_row("SELECT project_id FROM cards WHERE id = 'k1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(project_id.as_deref(), Some("p1"));
        let locked: i64 = db
            .query_row(
                "SELECT identifier_locked FROM projects WHERE id = 'p1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(locked, 0);

        // The partial unique index is case-insensitive.
        assert!(
            db.execute(
                "UPDATE projects SET identifier = 'KAN2' WHERE id = 'p1'",
                [],
            )
            .is_err()
        );

        // Re-running does not regenerate pinned identifiers.
        db.execute("UPDATE projects SET identifier = 'knd' WHERE id = 'p1'", [])
            .unwrap();
        run_schema(&mut db).unwrap();
        assert_eq!(ident(&db, "p1"), "knd");
    }

    /// A v3 database (which could hold upper-case identifiers) is normalized to
    /// lowercase by v4, without touching the lock flag.
    #[test]
    fn v3_database_lowercases_identifiers() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("t.db")).unwrap();
        db.execute_batch(SCHEMA_SQL).unwrap();
        db.execute_batch(SCHEMA_V2_SQL).unwrap();
        db.execute_batch(SCHEMA_V3_SQL).unwrap();
        db.execute_batch(
            "INSERT INTO boards VALUES ('b1', 'Kandrium', 0);
             INSERT INTO projects
                 (id, folder_id, name, identifier, identifier_locked, description, status,
                  board_id, position, created_at, updated_at)
                 VALUES ('p1', NULL, 'Kandrium', 'KAN-1', 1, '', 'active', 'b1', 0, 1, 1);",
        )
        .unwrap();
        db.pragma_update(None, "user_version", 3).unwrap();

        run_schema(&mut db).unwrap();

        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 4);
        let (identifier, locked): (String, i64) = db
            .query_row(
                "SELECT identifier, identifier_locked FROM projects WHERE id = 'p1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(identifier, "kan-1");
        assert_eq!(locked, 1);
    }
}
