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
        db.prepare("SELECT id, title, position FROM columns WHERE board_id = ? ORDER BY position")?;
    let columns = col_stmt
        .query_map([&board_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
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
    for (col_id, col_title, _) in &columns {
        let cards = card_stmt
            .query_map([col_id], |r| map_card_row(r, &board_id, project_board))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        board_columns.push(Column {
            id: col_id.clone(),
            title: col_title.clone(),
            cards,
            mirrored: Vec::new(),
        });
    }

    // Area boards also show project cards filed to them ("mirror"). They stay
    // in their real project column in the database; here they are bucketed
    // into the area column with the same canonical status.
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

        apply_area_order(db, &board_id, &columns, &mut board_columns)?;
    }

    Ok(BoardWithColumns {
        id: board_id,
        title: board_title,
        columns: board_columns,
    })
}

/// One card visible on a non-project board, as seen by the unified order.
struct AreaCandidate {
    id: String,
    status: CardStatus,
    /// The board's own card (true) or a project card filed to it (false).
    /// Backfill appends mirrors after native cards, the pre-v5 layout.
    native: bool,
    /// Native: the card's column position. Mirrored: the project's position.
    group: i64,
    /// Native: `cards.position`. Mirrored: the card's home-column position.
    order: i64,
}

/// Load, backfill and reconcile the unified display order of a non-project
/// board, then stamp every card with its [`Card::display_position`].
///
/// Native cards and project cards filed to the board ("mirrors") are one
/// sequence per canonical status bucket, persisted in `area_card_order`. This
/// is lazy, exactly like the All board's order:
///   - missing rows are inserted (initial backfill: the board's own cards by
///     column/card position first, then project cards by project/card
///     position — the pre-v5 layout);
///   - rows whose stored status no longer matches the card's real column are
///     re-bucketed at the end of their new status;
///   - rows for cards no longer visible on this board are deleted.
fn apply_area_order(
    db: &Connection,
    board_id: &str,
    columns: &[(String, String, i64)],
    board_columns: &mut [Column],
) -> Result<()> {
    let status_of: HashMap<&str, CardStatus> = columns
        .iter()
        .filter_map(|(id, title, _)| status_from_column_title(title).map(|s| (id.as_str(), s)))
        .collect();

    // Backfill order: native cards first (column, then card position), then
    // mirrored project cards (project, then card position).
    let mut candidates: Vec<AreaCandidate> = Vec::new();
    {
        let mut stmt = db.prepare(
            "SELECT c.id, c.column_id, col.position, c.position \
             FROM cards c JOIN columns col ON col.id = c.column_id \
             WHERE col.board_id = ? AND c.archived = 0 \
             ORDER BY col.position, c.position",
        )?;
        let rows = stmt
            .query_map([board_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (id, column_id, group, order) in rows {
            if let Some(status) = status_of.get(column_id.as_str()) {
                candidates.push(AreaCandidate {
                    id,
                    status: *status,
                    native: true,
                    group,
                    order,
                });
            }
        }
    }
    {
        let mut stmt = db.prepare(
            "SELECT c.id, col.title, p.position, c.position \
             FROM cards c JOIN columns col ON col.id = c.column_id \
             JOIN projects p ON p.board_id = col.board_id \
             WHERE c.area_board_id = ? AND c.archived = 0 \
             ORDER BY p.position, c.position",
        )?;
        let rows = stmt
            .query_map([board_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (id, column_title, group, order) in rows {
            if let Some(status) = status_from_column_title(&column_title) {
                candidates.push(AreaCandidate {
                    id,
                    status,
                    native: false,
                    group,
                    order,
                });
            }
        }
    }
    // Deterministic backfill independent of query interleaving: native cards
    // first (column, then card position), then mirrors (project, then card).
    candidates.sort_by_key(|c| (c.status.index(), !c.native, c.group, c.order));

    let mut stmt = db.prepare(
        "SELECT card_id, status, position FROM area_card_order \
         WHERE board_id = ? ORDER BY position",
    )?;
    let existing: Vec<(String, String, i64)> = stmt
        .query_map([board_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let by_id: HashMap<&str, &AreaCandidate> =
        candidates.iter().map(|c| (c.id.as_str(), c)).collect();
    // card id -> persisted slot, for everything that already has a row.
    let mut placement: HashMap<String, i64> = HashMap::new();
    let mut inserts: Vec<&AreaCandidate> = Vec::new();
    let mut rebuckets: Vec<&AreaCandidate> = Vec::new();
    let mut stale: Vec<String> = Vec::new();

    for (card_id, status, position) in &existing {
        match by_id.get(card_id.as_str()) {
            None => stale.push(card_id.clone()),
            Some(derived) if CardStatus::from_key(status) != Some(derived.status) => {
                rebuckets.push(derived);
            }
            Some(_) => {
                placement.insert(card_id.clone(), *position);
            }
        }
    }
    for candidate in &candidates {
        if !existing.iter().any(|(id, _, _)| *id == candidate.id) {
            inserts.push(candidate);
        }
    }

    if !inserts.is_empty() || !rebuckets.is_empty() || !stale.is_empty() {
        // IMMEDIATE: the write lock is taken up front so the running max
        // position read can't race a concurrent backfill in the other
        // process (app vs MCP).
        let tx =
            rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
        {
            // Running max position per status, seeded from every retained row.
            let mut max_pos: [i64; 3] = [-1; 3];
            for (card_id, position) in &placement {
                if let Some(candidate) = by_id.get(card_id.as_str()) {
                    let slot = candidate.status.index();
                    max_pos[slot] = max_pos[slot].max(*position);
                }
            }
            let mut del =
                tx.prepare("DELETE FROM area_card_order WHERE board_id = ? AND card_id = ?")?;
            for card_id in &stale {
                del.execute(rusqlite::params![board_id, card_id])?;
            }
            drop(del);
            let mut upsert = tx.prepare(
                "INSERT INTO area_card_order (board_id, card_id, status, position) \
                 VALUES (?, ?, ?, ?) \
                 ON CONFLICT(board_id, card_id) \
                 DO UPDATE SET status = excluded.status, position = excluded.position",
            )?;
            // Re-bucketed cards are appended to their new status; the insert
            // list (initial backfill) follows the deterministic order above.
            for candidate in rebuckets.iter().chain(inserts.iter()) {
                let slot = candidate.status.index();
                max_pos[slot] += 1;
                upsert.execute(rusqlite::params![
                    board_id,
                    candidate.id,
                    candidate.status.as_str(),
                    max_pos[slot]
                ])?;
                placement.insert(candidate.id.clone(), max_pos[slot]);
            }
        }
        tx.commit()?;
    }

    // Stamp per-column display positions: merge the column's own cards and
    // its mirrors by their persisted slot.
    for column in board_columns.iter_mut() {
        column
            .cards
            .sort_by_key(|c| placement.get(&c.id).copied().unwrap_or(i64::MAX));
        column
            .mirrored
            .sort_by_key(|c| placement.get(&c.id).copied().unwrap_or(i64::MAX));

        let mut order: Vec<(bool, usize)> =
            Vec::with_capacity(column.cards.len() + column.mirrored.len());
        order.extend((0..column.cards.len()).map(|i| (false, i)));
        order.extend((0..column.mirrored.len()).map(|i| (true, i)));
        order.sort_by_key(|(mirrored, idx)| {
            let id = if *mirrored {
                &column.mirrored[*idx].id
            } else {
                &column.cards[*idx].id
            };
            (placement.get(id).copied().unwrap_or(i64::MAX), *mirrored)
        });
        for (display_position, (mirrored, idx)) in order.into_iter().enumerate() {
            let card = if mirrored {
                &mut column.mirrored[idx]
            } else {
                &mut column.cards[idx]
            };
            card.display_position = display_position as i64;
        }
    }

    Ok(())
}

/// Rewrite the unified order of a non-project board after a card moved.
///
/// The board's own cards keep `cards.position` as the authoritative order, so
/// their slots are rebuilt from the column order; mirrored project cards keep
/// the slots they already have (only `move_area_card` changes those), and
/// cards that are new to a bucket are appended. Project boards have no area
/// order, so this is a no-op there.
pub(crate) fn sync_area_order(db: &Connection, board_id: &str) -> Result<()> {
    let project_board: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE board_id = ?)",
        [board_id],
        |r| r.get(0),
    )?;
    if project_board {
        return Ok(());
    }

    let mut stmt =
        db.prepare("SELECT id, title FROM columns WHERE board_id = ? ORDER BY position")?;
    let columns: Vec<(String, String)> = stmt
        .query_map([board_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let status_of: HashMap<&str, CardStatus> = columns
        .iter()
        .filter_map(|(id, title)| status_from_column_title(title).map(|s| (id.as_str(), s)))
        .collect();

    let mut native: [Vec<String>; 3] = std::array::from_fn(|_| Vec::new());
    {
        let mut stmt = db.prepare(
            "SELECT c.id, c.column_id \
             FROM cards c JOIN columns col ON col.id = c.column_id \
             WHERE col.board_id = ? AND c.archived = 0 \
             ORDER BY col.position, c.position",
        )?;
        let rows = stmt
            .query_map([board_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (card_id, column_id) in rows {
            if let Some(status) = status_of.get(column_id.as_str()) {
                native[status.index()].push(card_id);
            }
        }
    }

    let mut project: [Vec<String>; 3] = std::array::from_fn(|_| Vec::new());
    {
        let mut stmt = db.prepare(
            "SELECT c.id, col.title \
             FROM cards c JOIN columns col ON col.id = c.column_id \
             JOIN projects p ON p.board_id = col.board_id \
             WHERE c.area_board_id = ? AND c.archived = 0 \
             ORDER BY p.position, c.position",
        )?;
        let rows = stmt
            .query_map([board_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (card_id, column_title) in rows {
            if let Some(status) = status_from_column_title(&column_title) {
                project[status.index()].push(card_id);
            }
        }
    }

    let mut existing: [Vec<String>; 3] = std::array::from_fn(|_| Vec::new());
    {
        let mut stmt = db.prepare(
            "SELECT card_id, status, position FROM area_card_order \
             WHERE board_id = ? ORDER BY status, position",
        )?;
        let rows = stmt
            .query_map([board_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (card_id, status, _) in rows {
            if let Some(key) = CardStatus::from_key(&status) {
                existing[key.index()].push(card_id);
            }
        }
    }

    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    {
        tx.execute("DELETE FROM area_card_order WHERE board_id = ?", [board_id])?;
        let mut ins = tx.prepare(
            "INSERT INTO area_card_order (board_id, card_id, status, position) \
             VALUES (?, ?, ?, ?)",
        )?;
        for status in CardStatus::ALL {
            let i = status.index();
            let project_set: std::collections::HashSet<&str> =
                project[i].iter().map(String::as_str).collect();
            let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
            let mut seq: Vec<&str> = Vec::new();
            let mut natives = native[i].iter();
            // Project cards hold their existing slots; each native slot is
            // refilled from the authoritative native order.
            for card_id in &existing[i] {
                if project_set.contains(card_id.as_str()) {
                    if seen.insert(card_id.as_str()) {
                        seq.push(card_id.as_str());
                    }
                } else if let Some(next) = natives.next()
                    && seen.insert(next.as_str())
                {
                    seq.push(next.as_str());
                }
            }
            for card_id in native[i].iter().chain(project[i].iter()) {
                if seen.insert(card_id.as_str()) {
                    seq.push(card_id.as_str());
                }
            }
            for (position, card_id) in seq.iter().enumerate() {
                ins.execute(rusqlite::params![
                    board_id,
                    card_id,
                    status.as_str(),
                    position as i64
                ])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

/// Move one card to an explicit slot in a non-project board's unified order,
/// creating its row when needed. Used by `move_area_card` so a drop lands
/// exactly where the pointer was — between any two cards, native or mirrored.
pub(crate) fn reorder_area_bucket(
    db: &Connection,
    board_id: &str,
    status: CardStatus,
    card_id: &str,
    before_card_id: Option<&str>,
) -> Result<()> {
    let before = before_card_id.filter(|id| *id != card_id);
    let tx = rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate)?;
    {
        let mut seq: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT card_id FROM area_card_order \
                 WHERE board_id = ? AND status = ? AND card_id != ? ORDER BY position",
            )?;
            stmt.query_map(rusqlite::params![board_id, status.as_str(), card_id], |r| {
                r.get(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let insert_idx = match before {
            Some(before) => seq.iter().position(|id| id == before).unwrap_or(seq.len()),
            None => seq.len(),
        };
        seq.insert(insert_idx, card_id.to_string());

        // Drop the card's previous row (it may sit in another status bucket)
        // and rewrite this bucket's positions.
        tx.execute(
            "DELETE FROM area_card_order WHERE board_id = ? AND (status = ? OR card_id = ?)",
            rusqlite::params![board_id, status.as_str(), card_id],
        )?;
        let mut ins = tx.prepare(
            "INSERT INTO area_card_order (board_id, card_id, status, position) \
             VALUES (?, ?, ?, ?)",
        )?;
        for (position, id) in seq.iter().enumerate() {
            ins.execute(rusqlite::params![
                board_id,
                id,
                status.as_str(),
                position as i64
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Rewrite `cards.position` for the native cards of one column from the
/// board's unified order, so the persisted "real" order (MCP `cards` payload,
/// backward-compatible consumers) stays in step with what the board shows.
pub(crate) fn reindex_area_native_positions(
    db: &Connection,
    board_id: &str,
    status: CardStatus,
    column_id: &str,
) -> Result<()> {
    let tx = db.unchecked_transaction()?;
    {
        let ids: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT o.card_id FROM area_card_order o \
                 JOIN cards c ON c.id = o.card_id \
                 WHERE o.board_id = ? AND o.status = ? AND c.column_id = ? \
                 ORDER BY o.position",
            )?;
            stmt.query_map(
                rusqlite::params![board_id, status.as_str(), column_id],
                |r| r.get(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut upd = tx.prepare("UPDATE cards SET position = ? WHERE id = ?")?;
        for (position, card_id) in ids.iter().enumerate() {
            upd.execute(rusqlite::params![position as i64, card_id])?;
        }
    }
    tx.commit()?;
    Ok(())
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
        // Stamp by `apply_area_order` on non-project boards; 0 elsewhere so
        // the stable sort in `Column::ordered_cards` keeps source order.
        display_position: 0,
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
                display_position: 0,
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
