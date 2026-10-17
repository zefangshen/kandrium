//! Database open/path resolution, schema runner, and id generation.
//!
//! Port of `src/main/services/database-service.ts` (minus the Drizzle runner —
//! the fresh Rust database is versioned via `PRAGMA user_version` instead).

use std::path::PathBuf;
use std::time::Duration;

use rand::Rng;
use rusqlite::Connection;

use crate::error::{KandriumError, Result};

/// Embedded schema, applied idempotently.
pub const SCHEMA_SQL: &str = include_str!("schema.sql");
pub const SCHEMA_VERSION: i64 = 1;

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

/// Apply schema migrations. Steps are keyed on `PRAGMA user_version`; each
/// step is idempotent (`CREATE TABLE IF NOT EXISTS`). Currently only v1.
pub fn run_schema(db: &Connection) -> Result<()> {
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        db.execute_batch(SCHEMA_SQL)?;
        db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(())
}

/// True when the database has at least one board (i.e. seed/import already ran).
pub fn has_boards(db: &Connection) -> Result<bool> {
    let count: i64 = db.query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))?;
    Ok(count > 0)
}

/// Convenience: open + schema + (conditionally) seed, the standard bootstrap
/// shared by the app and MCP binaries.
pub fn bootstrap(db: &Connection) -> Result<()> {
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
}
