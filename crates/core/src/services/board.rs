//! Board services — port of `src/main/services/board-service.ts`.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::constants::{ALL_BOARD_ID, ALL_COLUMN_PREFIX, CARD_STATUSES, status_from_column_title};
use crate::db::{parse_labels, uid};
use crate::error::{KandriumError, Result};
use crate::models::*;
use crate::services::column::require_title;

/// List all boards for the Tasks section. Project-owned boards are excluded
/// (they are reachable from the Projects overview instead).
pub fn list_boards(db: &Connection) -> Result<BoardsResult> {
    let mut stmt = db.prepare(
        "SELECT id, title FROM boards \
         WHERE id NOT IN (SELECT board_id FROM projects) \
         ORDER BY position",
    )?;
    let boards = stmt
        .query_map([], |r| {
            Ok(Board {
                id: r.get(0)?,
                title: r.get(1)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(BoardsResult { boards })
}

/// Create a bare board at the end of the list. No default columns — callers
/// (the project templates) scaffold those. Errors if title is empty.
pub fn create_board(db: &Connection, title: &str) -> Result<Board> {
    let title = require_title(title)?;
    // Single INSERT...SELECT computes the next position in the same statement,
    // so concurrent writers can't race a separate MAX(position) read.
    let id = uid();
    db.execute(
        "INSERT INTO boards (id, title, position) \
         SELECT ?, ?, COALESCE(MAX(position), -1) + 1 FROM boards",
        rusqlite::params![id, title],
    )?;
    Ok(Board { id, title })
}

/// Load a single board with columns and cards. Errors if board not found.
pub fn get_board(db: &Connection, id: &str) -> Result<BoardWithColumns> {
    let board: Option<(String, String)> = db
        .query_row("SELECT id, title FROM boards WHERE id = ?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some((board_id, board_title)) = board else {
        return Err(KandriumError::new("Board not found"));
    };

    let project_board: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE board_id = ?)",
        [&board_id],
        |r| r.get(0),
    )?;

    let mut col_stmt =
        db.prepare("SELECT id, title FROM columns WHERE board_id = ? ORDER BY position")?;
    let columns = col_stmt
        .query_map([&board_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut card_stmt = db.prepare(
        "SELECT c.id, c.column_id, c.title, c.description, c.due_date, c.labels, \
                c.project_id, c.area_board_id, p.identifier, ab.title \
         FROM cards c LEFT JOIN projects p ON p.id = c.project_id \
         LEFT JOIN boards ab ON ab.id = c.area_board_id \
         WHERE c.column_id = ? AND c.archived = 0 ORDER BY c.position",
    )?;

    let mut board_columns = Vec::with_capacity(columns.len());
    for (col_id, col_title) in columns {
        let cards = card_stmt
            .query_map([&col_id], |r| map_card_row(r, &board_id, project_board))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        board_columns.push(Column {
            id: col_id,
            title: col_title,
            cards,
            mirrored: Vec::new(),
        });
    }

    // Area boards also show project cards filed to them ("mirror"). They stay
    // in their real project column in the database; here they are bucketed
    // into the area column with the same canonical status, appended after the
    // board's own cards so native drag/drop indices are unaffected.
    if !project_board {
        let mut mirror_stmt = db.prepare(
            r#"SELECT c.id, c.column_id, c.title, c.description, c.due_date, c.labels,
                      COALESCE(c.project_id, p.id) AS project_id,
                      c.area_board_id, p.identifier, ab.title,
                      col.board_id AS home_board_id, col.title AS home_column_title
               FROM cards c
               JOIN columns col ON c.column_id = col.id
               JOIN projects p ON p.board_id = col.board_id
               LEFT JOIN boards ab ON ab.id = c.area_board_id
               WHERE c.area_board_id = ? AND c.archived = 0
               ORDER BY p.position, c.position"#,
        )?;
        let rows: Vec<(Card, String)> = mirror_stmt
            .query_map([&board_id], |r| {
                let home_board_id: String = r.get(10)?;
                let home_column_title: String = r.get(11)?;
                Ok((map_card_row(r, &home_board_id, false)?, home_column_title))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (card, home_column_title) in rows {
            let Some(status) = status_from_column_title(&home_column_title) else {
                continue;
            };
            if let Some(column) = board_columns
                .iter_mut()
                .find(|c| status_from_column_title(&c.title) == Some(status))
            {
                column.mirrored.push(card);
            }
        }
    }

    Ok(BoardWithColumns {
        id: board_id,
        title: board_title,
        columns: board_columns,
    })
}

/// Row mapper for non-archived card selects. `board_id` is the home board of
/// the card's column (not necessarily the board being rendered).
///
/// `project_board` selects the second-row label: a project board shows the
/// area the card is filed to (the project identifier is redundant there),
/// while a normal/area board shows the project identifier.
fn map_card_row(r: &rusqlite::Row, board_id: &str, project_board: bool) -> rusqlite::Result<Card> {
    let project_identifier: Option<String> = r.get(8)?;
    let area_board_title: Option<String> = r.get(9)?;
    let meta_label = if project_board {
        area_board_title
    } else {
        project_identifier.clone()
    };
    Ok(Card {
        id: r.get(0)?,
        column_id: r.get(1)?,
        board_id: board_id.to_string(),
        title: r.get(2)?,
        description: r.get(3)?,
        due_date: r.get(4)?,
        labels: r
            .get::<_, String>(5)
            .map(|s| parse_labels(&s).unwrap_or_default())
            .unwrap_or_default(),
        project_id: r.get(6)?,
        area_board_id: r.get(7)?,
        project_identifier,
        meta_label,
    })
}

/// Build the virtual "All" board: every non-archived card whose home column
/// title matches a canonical status (To Do / In Progress / Done) is bucketed
/// across all real boards. Cards keep their real columnId and carry board info.
///
/// Card order is FREE within each bucket (not grouped by board) and persisted
/// in `all_card_order`. This call lazily:
///   - inserts rows for cards that have none yet (initial backfill follows the
///     board → column → card traversal order);
///   - reconciles rows whose stored status no longer matches the card's home
///     column (e.g. the card was moved on its real board) by appending it to
///     the end of the now-correct bucket.
pub fn get_all_board(db: &Connection) -> Result<AllBoard> {
    let mut stmt = db.prepare(
        r#"SELECT c.id, c.title, c.description, c.due_date, c.labels, c.position,
                  col.id AS column_id, col.title AS column_title,
                  b.id AS board_id,
                  COALESCE(ab.title, b.title) AS board_title,
                  o.status AS order_status, o.position AS order_position,
                  c.project_id, c.area_board_id, p.identifier AS project_identifier
           FROM cards c
           JOIN columns col ON c.column_id = col.id
           JOIN boards b ON col.board_id = b.id
           LEFT JOIN boards ab ON ab.id = c.area_board_id
           LEFT JOIN projects p ON p.id = c.project_id
           LEFT JOIN all_card_order o ON o.card_id = c.id
           WHERE c.archived = 0
             AND (col.board_id NOT IN (SELECT board_id FROM projects)
                  OR c.area_board_id IS NOT NULL)
           ORDER BY b.position, col.position, c.position"#,
    )?;
    let rows: Vec<AllCardRow> = stmt
        .query_map([], |r| {
            Ok(AllCardRow {
                id: r.get(0)?,
                title: r.get(1)?,
                description: r.get(2)?,
                due_date: r.get(3)?,
                labels: r.get(4)?,
                position: r.get(5)?,
                column_id: r.get(6)?,
                column_title: r.get(7)?,
                board_id: r.get(8)?,
                board_title: r.get(9)?,
                order_status: r.get(10)?,
                order_position: r.get(11)?,
                project_id: r.get(12)?,
                area_board_id: r.get(13)?,
                project_identifier: r.get(14)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    // Which cards need an order row written: no row yet (initial backfill), or
    // a stored status that no longer matches the card's home column (the card
    // was moved on its real board). Positions are assigned later, inside the
    // write transaction.
    let mut inserts: Vec<(String, CardStatus)> = Vec::new();
    let mut updates: Vec<(String, CardStatus)> = Vec::new();
    // Effective bucket + position for this load.
    let mut placement: HashMap<String, (CardStatus, i64)> = HashMap::new();

    for r in &rows {
        let Some(derived) = status_from_column_title(&r.column_title) else {
            continue;
        };

        if r.order_status.is_none() || r.order_position.is_none() {
            inserts.push((r.id.clone(), derived));
        } else if r.order_status.as_deref().map(CardStatus::from_key) != Some(Some(derived)) {
            updates.push((r.id.clone(), derived));
        } else if let Some(position) = r.order_position {
            placement.insert(r.id.clone(), (derived, position));
        }
    }

    if !inserts.is_empty() || !updates.is_empty() {
        // IMMEDIATE: the write lock is taken up front, so the running max read
        // below can't observe a snapshot from before a concurrent backfill in
        // the other process (app vs MCP) — both sides then assign disjoint
        // positions instead of computing the same ones.
        let tx =
            rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
        {
            // Running max position per status, seeded from every persisted row.
            let mut max_pos: [i64; 3] = [-1; 3];
            {
                let mut stmt = tx.prepare(
                    "SELECT status, COALESCE(MAX(position), -1) FROM all_card_order GROUP BY status",
                )?;
                let max_rows = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for (status, m) in max_rows {
                    if let Some(key) = CardStatus::from_key(&status) {
                        max_pos[key.index()] = m;
                    }
                }
            }
            let mut ins = tx.prepare(
                "INSERT INTO all_card_order (card_id, status, position) VALUES (?, ?, ?)",
            )?;
            for (id, status) in &inserts {
                max_pos[status.index()] += 1;
                let position = max_pos[status.index()];
                ins.execute(rusqlite::params![id, status.as_str(), position])?;
                placement.insert(id.clone(), (*status, position));
            }
            let mut upd =
                tx.prepare("UPDATE all_card_order SET status = ?, position = ? WHERE card_id = ?")?;
            for (id, status) in &updates {
                max_pos[status.index()] += 1;
                let position = max_pos[status.index()];
                upd.execute(rusqlite::params![status.as_str(), position, id])?;
                placement.insert(id.clone(), (*status, position));
            }
        }
        tx.commit()?;
    }

    let mut buckets: [Vec<AllBoardCard>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for r in &rows {
        let Some(&(status, _)) = placement.get(&r.id) else {
            continue;
        };
        let labels = parse_labels(&r.labels).unwrap_or_default();
        buckets[status.index()].push(AllBoardCard {
            card: Card {
                id: r.id.clone(),
                column_id: r.column_id.clone(),
                board_id: r.board_id.clone(),
                title: r.title.clone(),
                description: r.description.clone(),
                due_date: r.due_date.clone(),
                labels,
                project_id: r.project_id.clone(),
                area_board_id: r.area_board_id.clone(),
                project_identifier: r.project_identifier.clone(),
                meta_label: None,
            },
            board_id: r.board_id.clone(),
            board_title: r.board_title.clone(),
        });
    }

    // Sort each bucket by persisted All-board position.
    let positions: HashMap<String, i64> = placement
        .iter()
        .map(|(id, (_, pos))| (id.clone(), *pos))
        .collect();
    for bucket in buckets.iter_mut() {
        bucket.sort_by_key(|c| positions.get(&c.card.id).copied().unwrap_or(i64::MAX));
    }

    // Map buckets to columns by status index (not a positional zip), so the
    // pairing can't panic or silently misalign if the two arrays ever diverge.
    let columns = CARD_STATUSES
        .iter()
        .map(|s| AllBoardColumn {
            id: format!("{ALL_COLUMN_PREFIX}{}", s.key.as_str()),
            title: s.title.to_string(),
            status: s.key,
            cards: std::mem::take(&mut buckets[s.key.index()]),
        })
        .collect();

    Ok(AllBoard {
        id: ALL_BOARD_ID.to_string(),
        title: "All".to_string(),
        columns,
    })
}
