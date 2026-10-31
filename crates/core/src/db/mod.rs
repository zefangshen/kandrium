//! Database open/path resolution, schema runner, and id generation.
//!
//! Port of `src/main/services/database-service.ts` (minus the Drizzle runner —
//! the fresh Rust database is versioned by `rusqlite_migration` via
//! `PRAGMA user_version` instead).
//!
//! Split into:
//! - [`connection`] — opening the DB, path resolution, legacy-DB migration,
//!   bootstrap.
//! - [`migrations`] — the ordered migration set and `run_schema`.

pub mod backup;
pub mod connection;
pub mod migrations;

pub use backup::backups_dir;
pub use connection::{
    bootstrap, has_boards, migrate_legacy_db, open_db, resolve_db_path, resolve_old_db_path,
};
pub use migrations::{SCHEMA_VERSION, run_schema};

use rand::Rng;

use crate::error::{KandriumError, Result};

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
