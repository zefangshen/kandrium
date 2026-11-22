//! Schema migration definitions and runner.
//!
//! The SQL files live at the workspace root (`migrations/000X_*.sql`) and are
//! embedded at compile time. `rusqlite_migration` tracks the applied version in
//! `PRAGMA user_version`, so v1 databases upgrade through only the missing
//! steps and a database from a future schema is rejected.

use rusqlite::Connection;
use rusqlite_migration::{HookError, M, Migrations};

use crate::error::{KandriumError, Result};

/// Embedded schema, applied idempotently.
pub const SCHEMA_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0001_initial_schema.sql"
));
/// Additive v2 step (Projects). Separate from `SCHEMA_SQL` so the v1 shape
/// stays byte-identical to the legacy Drizzle schema.
pub const SCHEMA_V2_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0002_add_projects.sql"
));
/// Additive v3 step (project identifiers + card ↔ project links).
pub const SCHEMA_V3_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0003_add_project_identifiers.sql"
));
/// v4 step: normalize project identifiers to their canonical lowercase form.
pub const SCHEMA_V4_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0004_lowercase_identifiers.sql"
));
/// Additive v5 step (unified display order for non-project boards).
pub const SCHEMA_V5_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0005_area_card_order.sql"
));
/// Additive v6 step (archived projects).
pub const SCHEMA_V6_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0006_archive_projects.sql"
));
/// Additive v7 step (project source reference).
pub const SCHEMA_V7_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0007_project_source.sql"
));
/// Additive v8 step (Clocks section city list).
pub const SCHEMA_V8_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0008_clocks.sql"
));
/// Additive v9 step (people attached to saved Clocks cities).
pub const SCHEMA_V9_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0009_clock_people.sql"
));
/// Additive v10 step (temporary travel windows for Clocks people).
pub const SCHEMA_V10_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0010_clock_travels.sql"
));
/// Additive v11 step (multiple folders per project).
pub const SCHEMA_V11_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0011_project_paths.sql"
));
/// Additive v12 step (derived index for the LLM wiki).
pub const SCHEMA_V12_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0012_wiki.sql"
));
/// Additive v13 step (run-scoped file and row history).
pub const SCHEMA_V13_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0013_run_history.sql"
));
/// Additive v14 step (canonical persons; Clocks keeps placement and travel).
pub const SCHEMA_V14_SQL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../migrations/0014_person_identity.sql"
));
/// Latest schema version. Must match the number of entries in the migration set.
pub const SCHEMA_VERSION: i64 = 14;

/// The SQL of every migration, in application order. `SCHEMA_VERSION` must
/// equal its length; `every_sql_file_is_registered` proves the list still
/// covers every file under `migrations/`.
const MIGRATION_SQL: [&str; 14] = [
    SCHEMA_SQL,
    SCHEMA_V2_SQL,
    SCHEMA_V3_SQL,
    SCHEMA_V4_SQL,
    SCHEMA_V5_SQL,
    SCHEMA_V6_SQL,
    SCHEMA_V7_SQL,
    SCHEMA_V8_SQL,
    SCHEMA_V9_SQL,
    SCHEMA_V10_SQL,
    SCHEMA_V11_SQL,
    SCHEMA_V12_SQL,
    SCHEMA_V13_SQL,
    SCHEMA_V14_SQL,
];

/// The ordered migration set. `rusqlite_migration` tracks the applied version
/// in `PRAGMA user_version`, so v1 databases upgrade through only the missing
/// steps and a database from a future schema is rejected.
pub(crate) fn migrations() -> Migrations<'static> {
    let mut steps: Vec<M<'static>> = MIGRATION_SQL.iter().copied().map(M::up).collect();
    // v3 also needs the Rust-side identifier backfill, so it carries a hook.
    steps[2] = M::up_with_hook(SCHEMA_V3_SQL, |tx| {
        backfill_v3(tx).map_err(|e| HookError::Hook(e.0))
    });
    Migrations::new(steps)
}

/// Schema version currently recorded in the database.
pub(crate) fn current_version(db: &Connection) -> Result<i64> {
    Ok(db.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// True when the database is behind this binary's schema.
pub(crate) fn pending_migrations(db: &Connection) -> Result<bool> {
    Ok(current_version(db)? < SCHEMA_VERSION)
}

/// Apply all pending schema migrations atomically.
///
/// The v3 step needs Rust-side identifier generation, so it runs as a
/// migration hook after its SQL. `rusqlite_migration` bumps `user_version`
/// once the whole run commits; a database already at the latest version is a
/// no-op, and a database ahead of this binary returns an error.
pub(crate) fn run_schema(db: &mut Connection) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connection::open_db;

    #[test]
    fn migrations_validate() {
        migrations().validate().unwrap();
    }

    #[test]
    fn schema_version_matches_the_migration_set() {
        assert_eq!(MIGRATION_SQL.len() as i64, SCHEMA_VERSION);
        let db = Connection::open_in_memory().unwrap();
        assert_eq!(
            migrations().pending_migrations(&db).unwrap(),
            SCHEMA_VERSION as i32
        );
    }

    #[test]
    fn every_sql_file_is_registered() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("sql"))
            .collect();
        files.sort();

        assert_eq!(files.len(), MIGRATION_SQL.len());
        for (index, (file, embedded)) in files.iter().zip(MIGRATION_SQL).enumerate() {
            let name = file.file_name().unwrap().to_string_lossy();
            assert!(
                name.starts_with(&format!("{:04}_", index + 1)),
                "{} is out of order",
                file.display()
            );
            assert_eq!(
                std::fs::read_to_string(file).unwrap(),
                embedded,
                "{} is not embedded in migrations.rs",
                file.display()
            );
        }
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

    /// A v4 database only gains the additive v5 table; existing rows are
    /// untouched (the step is `CREATE TABLE IF NOT EXISTS` plus indexes).
    #[test]
    fn v4_database_gains_the_area_order_table() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("t.db")).unwrap();
        Migrations::new(vec![
            M::up(SCHEMA_SQL),
            M::up(SCHEMA_V2_SQL),
            M::up(SCHEMA_V3_SQL),
            M::up(SCHEMA_V4_SQL),
        ])
        .to_latest(&mut db)
        .unwrap();
        db.execute(
            "INSERT INTO boards (id, title, position) VALUES ('b1', 'Board', 0)",
            [],
        )
        .unwrap();

        run_schema(&mut db).unwrap();

        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let tables: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'area_card_order'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
        let boards: i64 = db
            .query_row("SELECT COUNT(*) FROM boards WHERE id = 'b1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(boards, 1);
    }

    /// A v7 database only gains the additive v8 clocks table; existing rows
    /// stay untouched and the clocks list starts empty.
    #[test]
    fn v7_database_gains_the_clocks_table() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("t.db")).unwrap();
        Migrations::new(vec![
            M::up(SCHEMA_SQL),
            M::up(SCHEMA_V2_SQL),
            M::up(SCHEMA_V3_SQL),
            M::up(SCHEMA_V4_SQL),
            M::up(SCHEMA_V5_SQL),
            M::up(SCHEMA_V6_SQL),
            M::up(SCHEMA_V7_SQL),
        ])
        .to_latest(&mut db)
        .unwrap();
        db.execute(
            "INSERT INTO boards (id, title, position) VALUES ('b1', 'Board', 0)",
            [],
        )
        .unwrap();

        run_schema(&mut db).unwrap();

        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let tables: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'clocks'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
        let clocks: i64 = db
            .query_row("SELECT COUNT(*) FROM clocks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(clocks, 0);
        let boards: i64 = db
            .query_row("SELECT COUNT(*) FROM boards WHERE id = 'b1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(boards, 1);
    }

    /// A v8 database only gains the additive v9 people table; saved clocks
    /// and their order stay untouched and no people are seeded.
    #[test]
    fn v8_database_gains_the_clock_people_table() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("t.db")).unwrap();
        Migrations::new(vec![
            M::up(SCHEMA_SQL),
            M::up(SCHEMA_V2_SQL),
            M::up(SCHEMA_V3_SQL),
            M::up(SCHEMA_V4_SQL),
            M::up(SCHEMA_V5_SQL),
            M::up(SCHEMA_V6_SQL),
            M::up(SCHEMA_V7_SQL),
            M::up(SCHEMA_V8_SQL),
        ])
        .to_latest(&mut db)
        .unwrap();
        db.execute_batch(
            "INSERT INTO clocks (id, tz_identifier, position, created_at)
             VALUES ('clock_tokyo', 'Asia/Tokyo', 0, '2026-01-01T00:00:00Z');",
        )
        .unwrap();

        run_schema(&mut db).unwrap();

        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let tables: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'clock_people'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
        let people: i64 = db
            .query_row("SELECT COUNT(*) FROM clock_people", [], |r| r.get(0))
            .unwrap();
        assert_eq!(people, 0);
        let clocks: i64 = db
            .query_row("SELECT COUNT(*) FROM clocks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(clocks, 1);
    }

    /// A v9 database only gains the additive v10 travel table; saved clocks,
    /// residents and their order stay untouched and no travel is seeded.
    #[test]
    fn v9_database_gains_the_clock_travels_table() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("t.db")).unwrap();
        Migrations::new(vec![
            M::up(SCHEMA_SQL),
            M::up(SCHEMA_V2_SQL),
            M::up(SCHEMA_V3_SQL),
            M::up(SCHEMA_V4_SQL),
            M::up(SCHEMA_V5_SQL),
            M::up(SCHEMA_V6_SQL),
            M::up(SCHEMA_V7_SQL),
            M::up(SCHEMA_V8_SQL),
            M::up(SCHEMA_V9_SQL),
        ])
        .to_latest(&mut db)
        .unwrap();
        db.execute_batch(
            "INSERT INTO clocks (id, tz_identifier, position, created_at)
                 VALUES ('clock_tokyo', 'Asia/Tokyo', 0, '2026-01-01T00:00:00Z');
             INSERT INTO clock_people (id, name, home_clock_id, position, created_at)
                 VALUES ('person_ana', 'Ana', 'clock_tokyo', 0, '2026-01-01T00:00:00Z');",
        )
        .unwrap();

        run_schema(&mut db).unwrap();

        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let tables: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'clock_travels'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
        let travels: i64 = db
            .query_row("SELECT COUNT(*) FROM clock_travels", [], |r| r.get(0))
            .unwrap();
        assert_eq!(travels, 0);
        let people: i64 = db
            .query_row("SELECT COUNT(*) FROM clock_people", [], |r| r.get(0))
            .unwrap();
        assert_eq!(people, 1);
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
        assert_eq!(version, SCHEMA_VERSION);
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
        assert_eq!(version, SCHEMA_VERSION);

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
        assert_eq!(version, SCHEMA_VERSION);
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

    /// A v11 database only gains the additive wiki tables. Existing boards,
    /// columns, and cards stay untouched; the index starts empty and FTS5 is
    /// queryable.
    #[test]
    fn v11_database_gains_the_wiki_tables() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("t.db")).unwrap();
        Migrations::new(vec![
            M::up(SCHEMA_SQL),
            M::up(SCHEMA_V2_SQL),
            M::up(SCHEMA_V3_SQL),
            M::up(SCHEMA_V4_SQL),
            M::up(SCHEMA_V5_SQL),
            M::up(SCHEMA_V6_SQL),
            M::up(SCHEMA_V7_SQL),
            M::up(SCHEMA_V8_SQL),
            M::up(SCHEMA_V9_SQL),
            M::up(SCHEMA_V10_SQL),
            M::up(SCHEMA_V11_SQL),
        ])
        .to_latest(&mut db)
        .unwrap();
        db.execute_batch(
            "INSERT INTO boards (id, title, position) VALUES ('b1', 'Board', 0);
             INSERT INTO columns (id, board_id, title, position)
                 VALUES ('c1', 'b1', 'To Do', 0);
             INSERT INTO cards (id, column_id, title, description, position)
                 VALUES ('k1', 'c1', 'Task', '', 0);",
        )
        .unwrap();

        run_schema(&mut db).unwrap();

        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        for table in [
            "wiki_meta",
            "wiki_sources",
            "wiki_pages",
            "wiki_page_sources",
            "wiki_links",
            "wiki_runs",
            "wiki_fts",
        ] {
            let count: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing table {table}");
        }

        let boards: i64 = db
            .query_row("SELECT COUNT(*) FROM boards WHERE id = 'b1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(boards, 1);
        let cards: i64 = db
            .query_row("SELECT COUNT(*) FROM cards WHERE id = 'k1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(cards, 1);

        db.execute(
            "INSERT INTO wiki_fts (path, title, summary, aliases, body)
             VALUES ('concepts/self-attention.md', 'Self-Attention', '', 'attention', 'unique_token')",
            [],
        )
        .unwrap();
        let hits: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM wiki_fts WHERE wiki_fts MATCH 'unique_token'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);

        let violations: i64 = db
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
    }

    /// A v12 database only gains the additive history tables; existing rows
    /// stay untouched, no run is open, and the partial unique index makes a
    /// second `open` row impossible while `committed` rows coexist.
    #[test]
    fn v12_database_gains_the_run_history_tables() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("t.db")).unwrap();
        Migrations::new(vec![
            M::up(SCHEMA_SQL),
            M::up(SCHEMA_V2_SQL),
            M::up(SCHEMA_V3_SQL),
            M::up(SCHEMA_V4_SQL),
            M::up(SCHEMA_V5_SQL),
            M::up(SCHEMA_V6_SQL),
            M::up(SCHEMA_V7_SQL),
            M::up(SCHEMA_V8_SQL),
            M::up(SCHEMA_V9_SQL),
            M::up(SCHEMA_V10_SQL),
            M::up(SCHEMA_V11_SQL),
            M::up(SCHEMA_V12_SQL),
        ])
        .to_latest(&mut db)
        .unwrap();
        db.execute(
            "INSERT INTO boards (id, title, position) VALUES ('b1', 'Board', 0)",
            [],
        )
        .unwrap();

        run_schema(&mut db).unwrap();

        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(version, 14);

        for table in ["run_history", "run_row_changes", "run_undo"] {
            let count: i64 = db
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing table {table}");
        }
        let index: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_run_undo_one_open'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(index, 1);
        let boards: i64 = db
            .query_row("SELECT COUNT(*) FROM boards WHERE id = 'b1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(boards, 1);

        // The open slot is a database guarantee, not a convention.
        db.execute(
            "INSERT INTO run_undo (run_id, state, last_heartbeat_at) VALUES ('r1', 'committed', 1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO run_undo (run_id, state, last_heartbeat_at) VALUES ('r2', 'open', 1)",
            [],
        )
        .unwrap();
        assert!(
            db.execute(
                "INSERT INTO run_undo (run_id, state, last_heartbeat_at) VALUES ('r3', 'open', 1)",
                [],
            )
            .is_err()
        );
        // Closing the first open run frees the slot.
        db.execute(
            "UPDATE run_undo SET state = 'committed' WHERE run_id = 'r2'",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO run_undo (run_id, state, last_heartbeat_at) VALUES ('r3', 'open', 1)",
            [],
        )
        .unwrap();
    }

    /// A v13 database upgrades to v14 by copying every colleague into
    /// `persons` + `clock_placements` (same ids) and every trip into
    /// `clock_travels_v2`. The legacy tables stay untouched, and re-running
    /// the v14 SQL is a no-op.
    #[test]
    fn v13_database_backfills_persons_placements_and_travels() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("t.db")).unwrap();
        Migrations::new(vec![
            M::up(SCHEMA_SQL),
            M::up(SCHEMA_V2_SQL),
            M::up(SCHEMA_V3_SQL),
            M::up(SCHEMA_V4_SQL),
            M::up(SCHEMA_V5_SQL),
            M::up(SCHEMA_V6_SQL),
            M::up(SCHEMA_V7_SQL),
            M::up(SCHEMA_V8_SQL),
            M::up(SCHEMA_V9_SQL),
            M::up(SCHEMA_V10_SQL),
            M::up(SCHEMA_V11_SQL),
            M::up(SCHEMA_V12_SQL),
            M::up(SCHEMA_V13_SQL),
        ])
        .to_latest(&mut db)
        .unwrap();
        db.execute_batch(
            "INSERT INTO clocks (id, tz_identifier, position, created_at)
                 VALUES ('clock_tokyo', 'Asia/Tokyo', 0, '2026-01-01T00:00:00Z');
             INSERT INTO clocks (id, tz_identifier, position, created_at)
                 VALUES ('clock_paris', 'Europe/Paris', 1, '2026-01-01T00:00:00Z');
             INSERT INTO clock_people (id, name, home_clock_id, position, created_at)
                 VALUES ('person_ana', 'Ana', 'clock_tokyo', 0, '2026-02-01T00:00:00Z');
             INSERT INTO clock_travels
                 (id, person_id, origin_clock_id, destination_clock_id,
                  starts_at, ends_at, cancelled_at, created_at)
                 VALUES ('travel_paris', 'person_ana', 'clock_tokyo', 'clock_paris',
                         100, NULL, NULL, '2026-02-02T00:00:00Z');",
        )
        .unwrap();

        run_schema(&mut db).unwrap();

        let version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let person: (String, String, i64, String, String) = db
            .query_row(
                "SELECT name, status, in_clocks, created_at, updated_at
                 FROM persons WHERE id = 'person_ana'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(person.0, "Ana");
        assert_eq!(person.1, "active");
        assert_eq!(person.2, 1);
        assert_eq!(person.3, "2026-02-01T00:00:00Z");
        assert_eq!(person.4, "2026-02-01T00:00:00Z");

        let placement: (String, i64, String) = db
            .query_row(
                "SELECT clock_id, position, created_at FROM clock_placements
                 WHERE person_id = 'person_ana'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(placement.0, "clock_tokyo");
        assert_eq!(placement.1, 0);
        assert_eq!(placement.2, "2026-02-01T00:00:00Z");

        let travel: (String, String, i64) = db
            .query_row(
                "SELECT person_id, destination_clock_id, starts_at
                 FROM clock_travels_v2 WHERE id = 'travel_paris'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            travel,
            ("person_ana".to_string(), "clock_paris".to_string(), 100)
        );

        // The legacy tables still exist with their original rows.
        for table in ["clock_people", "clock_travels"] {
            let count: i64 = db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1, "legacy {table} was modified");
        }

        // Re-running the v14 SQL completes nothing twice.
        db.execute_batch(SCHEMA_V14_SQL).unwrap();
        for table in ["persons", "clock_placements", "clock_travels_v2"] {
            let count: i64 = db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1, "{table} duplicated on re-run");
        }
    }
}
