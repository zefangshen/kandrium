//! One-time import from the legacy Electron app database.
//!
//! Copies every row of `boards`, `columns`, `cards`, and `all_card_order`
//! into the new database, preserving ids and positions so MCP history and
//! card-id references keep working. The old database is copied to a scratch
//! directory and opened read-write there, so the original file (possibly in
//! WAL mode with a running Electron app) is never touched.

use std::path::Path;

use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, OpenFlags};

use crate::db::{has_boards, open_db, run_schema, uid};
use crate::error::{KandriumError, Result};

#[derive(Clone, Copy, Debug, Default)]
pub struct ImportCounts {
    pub boards: usize,
    pub columns: usize,
    pub cards: usize,
    pub all_card_order: usize,
}

/// Import all rows from the legacy database at `old_path` into the new
/// database at `new_path`. Refuses to run if the target already has boards.
pub fn import_old_db(old_path: &Path, new_path: &Path) -> Result<ImportCounts> {
    if !old_path.exists() {
        return Err(KandriumError(format!(
            "Old database not found: {}",
            old_path.display()
        )));
    }

    // Stage a private copy so we can open it read-write (WAL recovery happens
    // on the copy, never on the user's original file).
    let scratch = std::env::temp_dir().join(format!("kandrium-import-{}", uid()));
    std::fs::create_dir_all(&scratch)?;
    let staged = scratch.join("old.db");
    let result = (|| {
        std::fs::copy(old_path, &staged)?;
        for suffix in ["-wal", "-shm"] {
            let side = old_path.with_extension(format!("db{suffix}"));
            if side.exists() {
                std::fs::copy(side, staged.with_extension(format!("db{suffix}")))?;
            }
        }

        let old = Connection::open_with_flags(&staged, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        for table in ["boards", "columns", "cards", "all_card_order"] {
            let exists: i64 = old.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
                [table],
                |r| r.get(0),
            )?;
            if exists == 0 {
                return Err(KandriumError(format!(
                    "Old database is missing table `{table}`"
                )));
            }
        }

        let mut new = open_db(new_path)?;
        run_schema(&mut new)?;
        if has_boards(&new)? {
            return Err(KandriumError(
                "Target database already contains boards; refusing to import".to_string(),
            ));
        }

        let tx = new.unchecked_transaction()?;
        let counts = ImportCounts {
            boards: copy_table(&old, &tx, "boards", "id, title, position")?,
            columns: copy_table(&old, &tx, "columns", "id, board_id, title, position")?,
            cards: copy_table(
                &old,
                &tx,
                "cards",
                "id, column_id, title, description, due_date, labels, position, archived, archived_at",
            )?,
            all_card_order: copy_table(&old, &tx, "all_card_order", "card_id, status, position")?,
        };
        tx.commit()?;
        Ok(counts)
    })();

    std::fs::remove_dir_all(&scratch).ok();
    result
}

/// Copy all rows of `columns_csv` from `src.table` into the same-named table
/// in `dst`. Returns the number of rows copied.
fn copy_table(src: &Connection, dst: &Connection, table: &str, columns_csv: &str) -> Result<usize> {
    let read_sql = format!("SELECT {columns_csv} FROM {table}");
    let write_sql = format!(
        "INSERT INTO {table} ({columns_csv}) VALUES ({placeholders})",
        placeholders = {
            let n = columns_csv.split(',').count();
            vec!["?"; n].join(", ")
        }
    );

    let mut stmt = src.prepare(&read_sql)?;
    let col_count = stmt.column_count();
    let rows = stmt.query_map([], |r| {
        (0..col_count)
            .map(|i| Ok(owned_value(r.get_ref(i)?)))
            .collect::<std::result::Result<Vec<Value>, _>>()
    })?;

    let mut writer = dst.prepare(&write_sql)?;
    let mut copied = 0usize;
    for row in rows {
        let values = row?;
        // Foreign keys are enforced and copy order satisfies dependencies
        // (boards → columns → cards → all_card_order).
        writer.execute(rusqlite::params_from_iter(values.iter()))?;
        copied += 1;
    }
    Ok(copied)
}

/// Convert a borrowed SQLite value into an owned one for re-binding.
fn owned_value(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Integer(i),
        ValueRef::Real(f) => Value::Real(f),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::Blob(b.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use crate::db::open_db;
    use crate::services::board::list_boards;

    use super::*;

    fn make_legacy_db(path: &Path) {
        let db = open_db(path).unwrap();
        // Legacy shape is the same schema; insert a minimal dataset directly.
        db.execute_batch(
            "CREATE TABLE boards (id TEXT PRIMARY KEY, title TEXT NOT NULL, position INTEGER NOT NULL);
             CREATE TABLE columns (id TEXT PRIMARY KEY, board_id TEXT NOT NULL REFERENCES boards(id) ON DELETE CASCADE, title TEXT NOT NULL, position INTEGER NOT NULL);
             CREATE TABLE cards (id TEXT PRIMARY KEY, column_id TEXT NOT NULL REFERENCES columns(id) ON DELETE CASCADE, title TEXT NOT NULL, description TEXT DEFAULT '', due_date TEXT DEFAULT '', labels TEXT DEFAULT '[]', position INTEGER NOT NULL, archived INTEGER NOT NULL DEFAULT 0, archived_at TEXT);
             CREATE TABLE all_card_order (card_id TEXT PRIMARY KEY REFERENCES cards(id) ON DELETE CASCADE, status TEXT NOT NULL, position INTEGER NOT NULL);
             INSERT INTO boards VALUES ('b1', 'Career', 0);
             INSERT INTO columns VALUES ('c1', 'b1', 'To Do', 0);
             INSERT INTO cards VALUES ('k1', 'c1', 'Task', 'desc', '', '[1]', 0, 0, NULL);
             INSERT INTO all_card_order VALUES ('k1', 'todo', 0);",
        )
        .unwrap();
    }

    #[test]
    fn imports_all_rows_preserving_ids() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.db");
        make_legacy_db(&old);

        let new = dir.path().join("new.db");
        let counts = import_old_db(&old, &new).unwrap();
        assert_eq!(
            (
                counts.boards,
                counts.columns,
                counts.cards,
                counts.all_card_order
            ),
            (1, 1, 1, 1)
        );

        let new_db = open_db(&new).unwrap();
        let boards = list_boards(&new_db).unwrap().boards;
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].id, "b1");
        assert_eq!(boards[0].title, "Career");
    }

    #[test]
    fn refuses_to_overwrite_existing_boards() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.db");
        let new = dir.path().join("new.db");
        make_legacy_db(&old);
        import_old_db(&old, &new).unwrap();
        let err = import_old_db(&old, &new).unwrap_err();
        assert!(err.to_string().contains("refusing to import"));
    }

    #[test]
    fn errors_when_old_db_missing() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            import_old_db(&dir.path().join("nope.db"), &dir.path().join("new.db")).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
