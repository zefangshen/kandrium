//! Column services — port of `src/main/services/column-service.ts`.

use rusqlite::Connection;

use crate::constants::{ALL_COLUMN_PREFIX, status_from_column_title};
use crate::db::uid;
use crate::error::{KandriumError, Result};
use crate::models::*;

/// Create a new column on a board. Errors if title is empty.
pub fn create_column(db: &Connection, board_id: &str, title: &str) -> Result<Column> {
    let title = require_title(title)?;
    // Single INSERT...SELECT: the next position is computed inside the same
    // statement, so concurrent writers (app + MCP share one DB) can't race a
    // separate MAX(position) read and persist duplicate positions.
    let id = uid();
    db.execute(
        "INSERT INTO columns (id, board_id, title, position) \
         SELECT ?, ?, ?, COALESCE(MAX(position), -1) + 1 FROM columns WHERE board_id = ?",
        rusqlite::params![id, board_id, title, board_id],
    )?;
    Ok(Column {
        id,
        title,
        cards: vec![],
        mirrored: vec![],
    })
}

/// Rename a column. Errors if title is empty.
pub fn rename_column(db: &Connection, id: &str, title: &str) -> Result<OkResult> {
    let title = require_title(title)?;
    db.execute(
        "UPDATE columns SET title = ? WHERE id = ?",
        rusqlite::params![title, id],
    )?;
    Ok(OkResult::ok())
}

/// Delete a column and all its cards.
pub fn delete_column(db: &Connection, id: &str) -> Result<OkResult> {
    let tx = db.unchecked_transaction()?;
    tx.execute("DELETE FROM cards WHERE column_id = ?", [id])?;
    tx.execute("DELETE FROM columns WHERE id = ?", [id])?;
    tx.commit()?;
    Ok(OkResult::ok())
}

/// Reorder a column within its board. Errors if column not found.
pub fn move_column(
    db: &Connection,
    col_id: &str,
    before_column_id: Option<&str>,
) -> Result<OkResult> {
    let col: Option<(String, String)> = db
        .query_row(
            "SELECT board_id, id FROM columns WHERE id = ?",
            [col_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some((board_id, _)) = col else {
        return Err(KandriumError::new("Column not found"));
    };

    let mut stmt =
        db.prepare("SELECT id FROM columns WHERE board_id = ? AND id != ? ORDER BY position")?;
    let mut target_ids: Vec<String> = stmt
        .query_map(rusqlite::params![board_id, col_id], |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let insert_idx = match before_column_id {
        Some(before) => target_ids
            .iter()
            .position(|id| id == before)
            .unwrap_or(target_ids.len()),
        None => target_ids.len(),
    };
    target_ids.insert(insert_idx, col_id.to_string());

    let tx = db.unchecked_transaction()?;
    {
        let mut upd = tx.prepare("UPDATE columns SET position = ? WHERE id = ?")?;
        for (i, id) in target_ids.iter().enumerate() {
            upd.execute(rusqlite::params![i as i64, id])?;
        }
    }
    tx.commit()?;
    Ok(OkResult::ok())
}

/// Archive all cards in a column. Accepts real column ids as well as virtual
/// All board columns (`all:<status>`), which archive every non-archived card
/// whose home column title maps to that canonical status across all boards.
pub fn archive_all_cards(db: &Connection, col_id: &str) -> Result<ArchiveAllResult> {
    let archived_count = if let Some(status_key) = col_id.strip_prefix(ALL_COLUMN_PREFIX) {
        let Some(status) = CardStatus::from_key(status_key) else {
            return Err(KandriumError::new("Unknown status"));
        };
        let mut stmt = db.prepare("SELECT id, title FROM columns")?;
        let cols = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let col_ids: Vec<&str> = cols
            .iter()
            .filter(|(_, title)| status_from_column_title(title) == Some(status))
            .map(|(id, _)| id.as_str())
            .collect();
        if col_ids.is_empty() {
            return Ok(ArchiveAllResult {
                ok: true,
                archived_count: 0,
            });
        }
        let placeholders = vec!["?"; col_ids.len()].join(", ");
        let sql = format!(
            "UPDATE cards SET archived = 1, archived_at = datetime('now') WHERE column_id IN ({placeholders}) AND archived = 0"
        );
        let n = db.execute(&sql, rusqlite::params_from_iter(col_ids.iter()))?;
        n as i64
    } else {
        db.execute(
            "UPDATE cards SET archived = 1, archived_at = datetime('now') WHERE column_id = ? AND archived = 0",
            [col_id],
        )? as i64
    };
    Ok(ArchiveAllResult {
        ok: true,
        archived_count,
    })
}

/// Trim + require a non-empty title (mirrors the TS `!title || !title.trim()` guard).
pub(crate) fn require_title(title: &str) -> Result<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err(KandriumError::new("Title required"));
    }
    Ok(trimmed.to_string())
}
