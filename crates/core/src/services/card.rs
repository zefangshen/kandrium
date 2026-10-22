//! Card services — port of `src/main/services/card-service.ts`.

use rusqlite::{Connection, OptionalExtension};

use crate::constants::{CARD_STATUSES, normalize_column_title, status_from_column_title};
use crate::db::{parse_labels, uid};
use crate::error::{KandriumError, Result};
use crate::models::*;
use crate::services::column::require_title;

/// Create a card in a column. Errors if title is empty.
pub fn create_card(db: &Connection, col_id: &str, data: &CreateCardBody) -> Result<Card> {
    let title = require_title(&data.title)?;
    let description = data.description.as_deref().unwrap_or("").trim().to_string();
    let due_date = data.due_date.clone().unwrap_or_default();
    let labels = data.labels.clone().unwrap_or_default();

    // Resolve the owning board and, when this is a project board, the project
    // the new card belongs to.
    let (board_id, project_id): (String, Option<String>) = db
        .query_row(
            "SELECT col.board_id, p.id FROM columns col \
             LEFT JOIN projects p ON p.board_id = col.board_id WHERE col.id = ?",
            [col_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| KandriumError::new("Column not found"))?;

    // Single INSERT...SELECT: the next position is computed inside the same
    // statement, so concurrent writers (app + MCP share one DB) can't race a
    // separate MAX(position) read and persist duplicate positions.
    let id = uid();
    db.execute(
        "INSERT INTO cards \
             (id, column_id, title, description, due_date, labels, position, project_id) \
         SELECT ?, ?, ?, ?, ?, ?, COALESCE(MAX(position), -1) + 1, ? \
         FROM cards WHERE column_id = ?",
        rusqlite::params![
            id,
            col_id,
            title,
            description,
            due_date,
            to_labels_json(&labels)?,
            project_id,
            col_id
        ],
    )?;
    let project_identifier = match &project_id {
        Some(project_id) => db
            .query_row(
                "SELECT identifier FROM projects WHERE id = ?",
                [project_id],
                |r| r.get(0),
            )
            .optional()?,
        None => None,
    };
    Ok(Card {
        id,
        column_id: col_id.to_string(),
        board_id,
        title,
        description,
        due_date,
        labels,
        project_id,
        area_board_id: None,
        project_identifier,
        meta_label: None,
    })
}

/// Create a card in the column of `board_id` whose normalized title matches
/// `status` (To Do / In Progress / Done).
///
/// Used by the virtual All board, where the bucket supplies the status and the
/// form supplies the target board. Errors if the board does not exist or has no
/// matching status column.
pub fn create_card_in_status(
    db: &Connection,
    board_id: &str,
    status: CardStatus,
    data: &CreateCardBody,
) -> Result<Card> {
    let status_def = CARD_STATUSES
        .iter()
        .find(|s| s.key == status)
        .ok_or_else(|| KandriumError::new("Unknown status"))?;

    let board_title: Option<String> = db
        .query_row("SELECT title FROM boards WHERE id = ?", [board_id], |r| {
            r.get(0)
        })
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some(board_title) = board_title else {
        return Err(KandriumError::new("Board not found"));
    };

    let mut stmt =
        db.prepare("SELECT id, title FROM columns WHERE board_id = ? ORDER BY position")?;
    let board_cols = stmt
        .query_map([board_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let target_col = board_cols
        .iter()
        .find(|(_, title)| status_from_column_title(title) == Some(status))
        .map(|(id, _)| id.clone());
    let Some(target_col) = target_col else {
        let matched = board_cols
            .iter()
            .map(|(_, title)| normalize_column_title(title))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(KandriumError::new(format!(
            "\"{}\" column not found on board \"{}\" (matched columns: {})",
            status_def.title,
            board_title,
            if matched.is_empty() { "none" } else { &matched }
        )));
    };

    create_card(db, &target_col, data)
}

/// Update a card. Errors if title is empty.
pub fn update_card(db: &Connection, id: &str, data: &CreateCardBody) -> Result<OkResult> {
    let title = require_title(&data.title)?;
    let description = data.description.as_deref().unwrap_or("").trim().to_string();
    let due_date = data.due_date.clone().unwrap_or_default();
    let labels = data.labels.clone().unwrap_or_default();

    db.execute(
        "UPDATE cards SET title = ?, description = ?, due_date = ?, labels = ? WHERE id = ?",
        rusqlite::params![title, description, due_date, to_labels_json(&labels)?, id],
    )?;
    Ok(OkResult::ok())
}

/// Rename a card's title only, leaving description/due date/labels untouched.
/// Archived cards are treated as absent so a stale inline editor can't edit
/// them after an external archive.
pub fn rename_card(db: &Connection, id: &str, title: &str) -> Result<OkResult> {
    let title = require_title(title)?;
    let changed = db.execute(
        "UPDATE cards SET title = ? WHERE id = ? AND archived = 0",
        rusqlite::params![title, id],
    )?;
    if changed == 0 {
        return Err(KandriumError::new("Card not found"));
    }
    Ok(OkResult::ok())
}

/// Tag an area-board card with a project, or clear the tag (`None`).
/// Project-board cards inherit their project automatically, so this is
/// normally only used from normal boards.
pub fn set_card_project(db: &Connection, id: &str, project_id: Option<&str>) -> Result<OkResult> {
    let exists: i64 = db.query_row("SELECT COUNT(*) FROM cards WHERE id = ?", [id], |r| {
        r.get(0)
    })?;
    if exists == 0 {
        return Err(KandriumError::new("Card not found"));
    }
    if let Some(project_id) = project_id {
        let found: i64 = db.query_row(
            "SELECT COUNT(*) FROM projects WHERE id = ?",
            [project_id],
            |r| r.get(0),
        )?;
        if found == 0 {
            return Err(KandriumError::new("Project not found"));
        }
    }
    db.execute(
        "UPDATE cards SET project_id = ? WHERE id = ?",
        rusqlite::params![project_id, id],
    )?;
    Ok(OkResult::ok())
}

/// File a project card to an area board, or clear the area (`None`). The
/// target must be a normal (non-project) board, and the card must live on a
/// project board — a card can't mirror onto someone else's area.
pub fn set_card_area(db: &Connection, id: &str, area_board_id: Option<&str>) -> Result<OkResult> {
    let exists: i64 = db.query_row("SELECT COUNT(*) FROM cards WHERE id = ?", [id], |r| {
        r.get(0)
    })?;
    if exists == 0 {
        return Err(KandriumError::new("Card not found"));
    }
    if let Some(board_id) = area_board_id {
        let (found, project_board): (i64, i64) = db.query_row(
            "SELECT (SELECT COUNT(*) FROM boards WHERE id = ?1), \
                    (SELECT COUNT(*) FROM projects WHERE board_id = ?1)",
            [board_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if found == 0 {
            return Err(KandriumError::new("Board not found"));
        }
        if project_board != 0 {
            return Err(KandriumError::new("Area must be a non-project board"));
        }
        let on_project_board: i64 = db.query_row(
            "SELECT COUNT(*) FROM cards c \
             JOIN columns col ON c.column_id = col.id \
             JOIN projects p ON p.board_id = col.board_id \
             WHERE c.id = ?",
            [id],
            |r| r.get(0),
        )?;
        if on_project_board == 0 {
            return Err(KandriumError::new(
                "Only project cards can be filed to an area",
            ));
        }
    }
    db.execute(
        "UPDATE cards SET area_board_id = ? WHERE id = ?",
        rusqlite::params![area_board_id, id],
    )?;
    Ok(OkResult::ok())
}

/// Move a card to another column/position. Errors if card not found.
pub fn move_card(
    db: &Connection,
    card_id: &str,
    to_col_id: &str,
    before_card_id: Option<&str>,
) -> Result<OkResult> {
    let card = get_full_card(db, card_id)?.ok_or_else(|| KandriumError::new("Card not found"))?;

    let target_board_id: Option<String> = db
        .query_row(
            "SELECT board_id FROM columns WHERE id = ?",
            [to_col_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(target_board_id) = target_board_id else {
        return Err(KandriumError::new("Column not found"));
    };
    let source_board_id: Option<String> = db
        .query_row(
            "SELECT board_id FROM columns WHERE id = ?",
            [&card.column_id],
            |r| r.get(0),
        )
        .optional()?;
    let cross_board = source_board_id.as_deref() != Some(target_board_id.as_str());
    // Moving across boards must keep the project/area links consistent; a
    // same-board move leaves them alone.
    let target_project: Option<String> = if cross_board {
        db.query_row(
            "SELECT id FROM projects WHERE board_id = ?",
            [&target_board_id],
            |r| r.get(0),
        )
        .optional()?
    } else {
        None
    };

    let mut stmt = db.prepare(
        "SELECT id, column_id, title, description, due_date, labels, position, archived, archived_at \
         FROM cards WHERE column_id = ? AND id != ? ORDER BY position",
    )?;
    let mut target_cards: Vec<CardRow> = stmt
        .query_map(rusqlite::params![to_col_id, card_id], map_full_card_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let insert_idx = match before_card_id {
        Some(before) => target_cards
            .iter()
            .position(|c| c.id == before)
            .unwrap_or(target_cards.len()),
        None => target_cards.len(),
    };
    target_cards.insert(insert_idx, card);

    let tx = db.unchecked_transaction()?;
    {
        let mut upd = tx.prepare("UPDATE cards SET column_id = ?, position = ? WHERE id = ?")?;
        for (i, c) in target_cards.iter().enumerate() {
            upd.execute(rusqlite::params![to_col_id, i as i64, c.id])?;
        }
    }
    if cross_board {
        match target_project {
            Some(project_id) => {
                tx.execute(
                    "UPDATE cards SET project_id = ?, area_board_id = NULL WHERE id = ?",
                    rusqlite::params![project_id, card_id],
                )?;
            }
            None => {
                tx.execute(
                    "UPDATE cards SET area_board_id = NULL WHERE id = ?",
                    [card_id],
                )?;
            }
        }
    }
    tx.commit()?;
    Ok(OkResult::ok())
}

pub fn delete_card(db: &Connection, id: &str) -> Result<OkResult> {
    db.execute("DELETE FROM cards WHERE id = ?", [id])?;
    Ok(OkResult::ok())
}

/// Move a card to the canonical-status column (To Do / In Progress / Done) on
/// its OWN board — used by the virtual All board. The card is appended to the
/// end of the target column, which is matched by normalized title. Errors if
/// the card or the target status column doesn't exist on the card's board.
pub fn move_card_to_status(db: &Connection, card_id: &str, status: CardStatus) -> Result<OkResult> {
    let card = get_full_card(db, card_id)?.ok_or_else(|| KandriumError::new("Card not found"))?;

    let status_def = CARD_STATUSES
        .iter()
        .find(|s| s.key == status)
        .ok_or_else(|| KandriumError::new("Unknown status"))?;

    let home: Option<(String, String)> = db
        .query_row(
            r#"SELECT col.board_id, b.title
               FROM columns col JOIN boards b ON col.board_id = b.id
               WHERE col.id = ?"#,
            [&card.column_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some((board_id, board_title)) = home else {
        return Err(KandriumError::new("Source column not found"));
    };

    let mut stmt = db.prepare("SELECT id, title FROM columns WHERE board_id = ?")?;
    let board_cols = stmt
        .query_map([&board_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let target_col = board_cols
        .iter()
        .find(|(_, title)| status_from_column_title(title) == Some(status))
        .map(|(id, _)| id.clone());
    let Some(target_col) = target_col else {
        let matched = board_cols
            .iter()
            .map(|(_, title)| normalize_column_title(title))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(KandriumError::new(format!(
            "\"{}\" column not found on board \"{}\" (matched columns: {})",
            status_def.title,
            board_title,
            if matched.is_empty() { "none" } else { &matched }
        )));
    };

    reorder_column_cards(db, &target_col, &card)
}

/// Move a card to a DIFFERENT board, landing it in the column whose canonical
/// status matches the card's source column (e.g. In Progress → In Progress),
/// appended at the end. Reuses `move_card` for the atomic position reindex.
/// No-op (returns ok) when the card is already on the target board.
pub fn move_card_to_board(
    db: &Connection,
    card_id: &str,
    target_board_id: &str,
) -> Result<OkResult> {
    let card = get_full_card(db, card_id)?.ok_or_else(|| KandriumError::new("Card not found"))?;

    let src_col: Option<(String, String, String)> = db
        .query_row(
            "SELECT id, title, board_id FROM columns WHERE id = ?",
            [&card.column_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some((src_col_id, src_col_title, src_board_id)) = src_col else {
        return Err(KandriumError::new("Source column not found"));
    };
    let _ = src_col_id; // source column id kept for parity with TS naming

    if src_board_id == target_board_id {
        return Ok(OkResult::ok());
    }

    let Some(status) = status_from_column_title(&src_col_title) else {
        return Err(KandriumError::new(
            "Card is not in a To Do / In Progress / Done column",
        ));
    };

    let status_def = CARD_STATUSES.iter().find(|s| s.key == status).unwrap();
    let mut stmt =
        db.prepare("SELECT id, title FROM columns WHERE board_id = ? ORDER BY position")?;
    let target_cols = stmt
        .query_map([target_board_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let target_col = target_cols
        .iter()
        .find(|(_, title)| status_from_column_title(title) == Some(status))
        .map(|(id, _)| id.clone());
    let Some(target_col) = target_col else {
        let matched = target_cols
            .iter()
            .map(|(_, title)| normalize_column_title(title))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(KandriumError::new(format!(
            "\"{}\" column not found on target board (matched columns: {})",
            status_def.title,
            if matched.is_empty() { "none" } else { &matched }
        )));
    };

    // `move_card` also reconciles project/area links when the target column
    // belongs to another board.
    move_card(db, card_id, &target_col, None)?;
    Ok(OkResult::ok())
}

/// Move/reorder a card inside the virtual All board.
///
/// - Cross-bucket (e.g. To Do → Done): the card is also moved onto the
///   matching status column of its home board via `move_card_to_status`.
/// - Same-bucket: only the All-board position changes.
///
/// Ordering is global per bucket (cards from different boards interleave
/// freely) and persisted in `all_card_order`. The card is placed before
/// `before_card_id`, or at the end when `None`.
pub fn move_all_card(
    db: &Connection,
    card_id: &str,
    to_status: CardStatus,
    before_card_id: Option<&str>,
) -> Result<OkResult> {
    let card = get_full_card(db, card_id)?.ok_or_else(|| KandriumError::new("Card not found"))?;

    let home_col_title: Option<String> = db
        .query_row(
            "SELECT title FROM columns WHERE id = ?",
            [&card.column_id],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let current_status = home_col_title.as_deref().and_then(status_from_column_title);
    let Some(current_status) = current_status else {
        return Err(KandriumError::new(
            "Card is not in a To Do / In Progress / Done column",
        ));
    };

    // Sync the home board first — it errors (before any order writes) if the
    // target status column doesn't exist on the card's board.
    if to_status != current_status {
        move_card_to_status(db, card_id, to_status)?;
    }

    let mut stmt = db.prepare(
        r#"SELECT card_id, status, position FROM all_card_order
           WHERE status IN (?, ?) ORDER BY position"#,
    )?;
    let order_rows: Vec<AllOrderRow> = stmt
        .query_map(
            rusqlite::params![current_status.as_str(), to_status.as_str()],
            |r| {
                Ok(AllOrderRow {
                    card_id: r.get(0)?,
                    status: r.get(1)?,
                    position: r.get(2)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut source_ids: Vec<String> = order_rows
        .iter()
        .filter(|r| r.status == current_status.as_str())
        .map(|r| r.card_id.clone())
        .collect();
    let dest_ids: Vec<String> = if to_status == current_status {
        source_ids.clone()
    } else {
        order_rows
            .iter()
            .filter(|r| r.status == to_status.as_str())
            .map(|r| r.card_id.clone())
            .collect()
    };

    // get_all_board normally backfills the row; defensively make sure the card
    // is present in the source bucket.
    if !source_ids.iter().any(|id| id == card_id) {
        source_ids.push(card_id.to_string());
    }

    let mut reordered: Vec<String> = dest_ids.into_iter().filter(|id| id != card_id).collect();
    let insert_idx = match before_card_id {
        Some(before) => reordered
            .iter()
            .position(|id| id == before)
            .unwrap_or(reordered.len()),
        None => reordered.len(),
    };
    reordered.insert(insert_idx, card_id.to_string());

    let tx = db.unchecked_transaction()?;
    {
        let mut upsert = tx.prepare(
            r#"INSERT INTO all_card_order (card_id, status, position) VALUES (?, ?, ?)
               ON CONFLICT(card_id) DO UPDATE SET status = excluded.status, position = excluded.position"#,
        )?;
        for (i, id) in reordered.iter().enumerate() {
            upsert.execute(rusqlite::params![id, to_status.as_str(), i as i64])?;
        }
        if to_status != current_status {
            let mut upd = tx.prepare("UPDATE all_card_order SET position = ? WHERE card_id = ?")?;
            for (i, id) in source_ids
                .iter()
                .filter(|id| id.as_str() != card_id)
                .enumerate()
            {
                upd.execute(rusqlite::params![i as i64, id])?;
            }
        }
    }
    tx.commit()?;
    Ok(OkResult::ok())
}

pub fn set_card_archived(db: &Connection, id: &str, archived: bool) -> Result<OkResult> {
    if archived {
        db.execute(
            "UPDATE cards SET archived = 1, archived_at = datetime('now') WHERE id = ?",
            [id],
        )?;
    } else {
        db.execute(
            "UPDATE cards SET archived = 0, archived_at = NULL WHERE id = ?",
            [id],
        )?;
    }
    Ok(OkResult::ok())
}

/// List all archived cards with board/column info.
pub fn list_archived_cards(db: &Connection) -> Result<ArchivedCardsResult> {
    let mut stmt = db.prepare(
        r#"SELECT c.id, c.title, c.description, c.due_date, c.labels, c.archived_at,
                  col.title AS column_title, col.board_id, b.title AS board_title
           FROM cards c
           LEFT JOIN columns col ON c.column_id = col.id
           LEFT JOIN boards b ON col.board_id = b.id
           WHERE c.archived = 1
           ORDER BY c.archived_at DESC"#,
    )?;
    let cards = stmt
        .query_map([], |r| {
            Ok(ArchivedCardRow {
                id: r.get(0)?,
                title: r.get(1)?,
                description: r.get(2)?,
                due_date: r.get(3)?,
                labels: r.get(4)?,
                archived_at: r.get(5)?,
                column_title: r.get(6)?,
                board_id: r.get(7)?,
                board_title: r.get(8)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(ArchivedCardsResult {
        cards: cards
            .into_iter()
            .map(|r| ArchivedCard {
                labels: parse_labels(&r.labels).unwrap_or_default(),
                id: r.id,
                title: r.title,
                description: r.description,
                due_date: r.due_date,
                archived_at: r.archived_at.unwrap_or_default(),
                column_title: r.column_title,
                board_id: r.board_id,
                board_title: r.board_title,
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn to_labels_json(labels: &[i64]) -> Result<String> {
    Ok(serde_json::to_string(labels)?)
}

/// Fetch the full internal card row (all columns).
fn get_full_card(db: &Connection, id: &str) -> Result<Option<CardRow>> {
    let mut stmt = db.prepare("SELECT * FROM cards WHERE id = ?")?;
    let mut rows = stmt.query_map([id], map_full_card_row)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

fn map_full_card_row(r: &rusqlite::Row) -> rusqlite::Result<CardRow> {
    Ok(CardRow {
        id: r.get("id")?,
        column_id: r.get("column_id")?,
        title: r.get("title")?,
        description: r.get("description")?,
        due_date: r.get("due_date")?,
        labels: r.get("labels")?,
        position: r.get("position")?,
        archived: r.get("archived")?,
        archived_at: r.get("archived_at")?,
    })
}

/// Rebuild a column's order so `card` lands at the end (used by
/// `move_card_to_status`): all other cards keep relative order, the moved card
/// is appended. Port of the same inline logic in the TS service.
fn reorder_column_cards(db: &Connection, target_col_id: &str, card: &CardRow) -> Result<OkResult> {
    let mut stmt = db.prepare("SELECT * FROM cards WHERE column_id = ? ORDER BY position")?;
    let target_cards: Vec<CardRow> = stmt
        .query_map([target_col_id], map_full_card_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut new_order: Vec<&CardRow> = target_cards.iter().filter(|c| c.id != card.id).collect();
    new_order.push(card);

    let tx = db.unchecked_transaction()?;
    {
        let mut upd = tx.prepare("UPDATE cards SET column_id = ?, position = ? WHERE id = ?")?;
        for (i, c) in new_order.iter().enumerate() {
            upd.execute(rusqlite::params![target_col_id, i as i64, c.id])?;
        }
    }
    tx.commit()?;
    Ok(OkResult::ok())
}
