//! Seed default boards (Career, Work, Life) with columns and sample cards.
//!
//! Port of `src/main/services/seed.ts`. This is application seed data, not a
//! schema migration — it runs once after migrations, only when the database
//! has no boards yet. All inserts are guarded by EXISTS checks so the seed is
//! safe to re-run.
//!
//! Note: the TS `seedCard` SQL passes 7 params to a 6-value statement (its
//! `description` param lands nowhere) — an upstream bug that would fail on a
//! fresh database. This port inserts the description and keeps `due_date` ''
//! (matching the data observed in existing seeded databases).

use rusqlite::Connection;

use crate::error::Result;

pub fn seed(db: &Connection) -> Result<()> {
    let board_count: i64 = db.query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))?;
    if board_count > 0 {
        return Ok(());
    }

    let tx = db.unchecked_transaction()?;
    // Boards
    tx.execute(
        "INSERT INTO boards (id, title, position) SELECT 'board_career', 'Career', 0 WHERE NOT EXISTS (SELECT 1 FROM boards)",
        [],
    )?;
    tx.execute(
        "INSERT INTO boards (id, title, position) SELECT 'board_work', 'Work', 1 WHERE NOT EXISTS (SELECT 1 FROM boards WHERE title = 'Work')",
        [],
    )?;
    tx.execute(
        "INSERT INTO boards (id, title, position) SELECT 'board_life', 'Life', 2 WHERE NOT EXISTS (SELECT 1 FROM boards WHERE title = 'Life')",
        [],
    )?;

    // Career board columns
    seed_column(&tx, "col_career_todo", "board_career", "To Do", 0)?;
    seed_column(&tx, "col_career_progress", "board_career", "In Progress", 1)?;
    seed_column(&tx, "col_career_done", "board_career", "Done", 2)?;

    // Work board columns
    seed_column(&tx, "col_work_todo", "board_work", "To Do", 0)?;
    seed_column(&tx, "col_work_progress", "board_work", "In Progress", 1)?;
    seed_column(&tx, "col_work_done", "board_work", "Done", 2)?;

    // Life board columns
    seed_column(&tx, "col_life_todo", "board_life", "To Do", 0)?;
    seed_column(&tx, "col_life_progress", "board_life", "In Progress", 1)?;
    seed_column(&tx, "col_life_done", "board_life", "Done", 2)?;

    // Sample cards
    seed_card(
        &tx,
        "card_career_1",
        "col_career_todo",
        "Update resume",
        "Drag cards across columns. Click a card to edit.",
        "[1]",
    )?;
    seed_card(
        &tx,
        "card_career_2",
        "col_career_progress",
        "Try dragging this card",
        "",
        "[5]",
    )?;
    seed_card(
        &tx,
        "card_career_3",
        "col_career_done",
        "Set up the board",
        "",
        "[0]",
    )?;

    seed_card(
        &tx,
        "card_work_1",
        "col_work_todo",
        "Review project plan",
        "Drag cards across columns. Click a card to edit.",
        "[1]",
    )?;
    seed_card(
        &tx,
        "card_work_2",
        "col_work_progress",
        "Try dragging this card",
        "",
        "[5]",
    )?;
    seed_card(
        &tx,
        "card_work_3",
        "col_work_done",
        "Set up the board",
        "",
        "[0]",
    )?;

    seed_card(
        &tx,
        "card_life_1",
        "col_life_todo",
        "Grocery shopping",
        "Drag cards across columns. Click a card to edit.",
        "[1]",
    )?;
    seed_card(
        &tx,
        "card_life_2",
        "col_life_progress",
        "Try dragging this card",
        "",
        "[5]",
    )?;
    seed_card(
        &tx,
        "card_life_3",
        "col_life_done",
        "Set up the board",
        "",
        "[0]",
    )?;

    tx.commit()?;
    Ok(())
}

fn seed_column(
    db: &Connection,
    id: &str,
    board_id: &str,
    title: &str,
    position: i64,
) -> Result<()> {
    db.execute(
        r#"INSERT INTO columns (id, board_id, title, position)
           SELECT ?, ?, ?, ?
           WHERE EXISTS (SELECT 1 FROM boards WHERE id = ?)
             AND NOT EXISTS (SELECT 1 FROM columns WHERE board_id = ? AND title = ?)"#,
        rusqlite::params![id, board_id, title, position, board_id, board_id, title],
    )?;
    Ok(())
}

fn seed_card(
    db: &Connection,
    id: &str,
    col_id: &str,
    title: &str,
    description: &str,
    labels: &str,
) -> Result<()> {
    db.execute(
        r#"INSERT INTO cards (id, column_id, title, description, due_date, labels, position)
           SELECT ?, ?, ?, ?, '', ?, 0
           WHERE EXISTS (SELECT 1 FROM columns WHERE id = ?)
             AND NOT EXISTS (SELECT 1 FROM cards WHERE id = ?)"#,
        rusqlite::params![id, col_id, title, description, labels, col_id, id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::db::{has_boards, open_db, run_schema};
    use crate::seed::seed;
    use crate::services::board::list_boards;

    #[test]
    fn seed_is_guarded_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(&dir.path().join("t.db")).unwrap();
        run_schema(&db).unwrap();

        assert!(!has_boards(&db).unwrap());
        seed(&db).unwrap();
        assert!(has_boards(&db).unwrap());
        seed(&db).unwrap(); // second run is a no-op

        let boards = list_boards(&db).unwrap().boards;
        assert_eq!(boards.len(), 3);
        assert_eq!(boards[0].title, "Career");
        assert_eq!(boards[1].title, "Work");
        assert_eq!(boards[2].title, "Life");
    }
}
