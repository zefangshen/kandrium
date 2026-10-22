//! Integration-style tests for the ported services, exercising the same
//! flows the Electron app's services implement (reindexing, All-board
//! backfill/reconcile, virtual-column archive).

use kandrium_core::db::bootstrap;
use kandrium_core::*;

/// Fresh temp DB with the standard seed (3 boards × 3 columns × 3 cards).
fn fresh_db() -> (tempfile::TempDir, rusqlite::Connection) {
    let dir = tempfile::tempdir().unwrap();
    let mut db = open_db(&dir.path().join("t.db")).unwrap();
    bootstrap(&mut db).unwrap();
    (dir, db)
}

/// Col ids of a seeded board, in position order: [To Do, In Progress, Done].
fn col_ids(db: &rusqlite::Connection, board_id: &str) -> Vec<String> {
    let mut stmt = db
        .prepare("SELECT id FROM columns WHERE board_id = ? ORDER BY position")
        .unwrap();
    stmt.query_map([board_id], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
}

fn card_titles(col: &Column) -> Vec<&str> {
    col.cards.iter().map(|c| c.title.as_str()).collect()
}

#[test]
fn move_card_within_column_reindexes() {
    let (_d, db) = fresh_db();
    let cols = col_ids(&db, "board_career");
    let board = get_board(&db, "board_career").unwrap();
    assert_eq!(card_titles(&board.columns[0]), ["Update resume"]);

    // Seed has one card per column; add two more to test ordering.
    let b = create_card(
        &db,
        &cols[0],
        &CreateCardBody {
            title: "Second".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let c = create_card(
        &db,
        &cols[0],
        &CreateCardBody {
            title: "Third".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let board = get_board(&db, "board_career").unwrap();
    assert_eq!(
        card_titles(&board.columns[0]),
        ["Update resume", "Second", "Third"]
    );

    // Move first card to the end (before = None).
    let first = &board.columns[0].cards[0].id;
    move_card(&db, first, &cols[0], None).unwrap();
    let board = get_board(&db, "board_career").unwrap();
    assert_eq!(
        card_titles(&board.columns[0]),
        ["Second", "Third", "Update resume"]
    );

    // Move it back before the first card.
    move_card(&db, first, &cols[0], Some(&b.id)).unwrap();
    let board = get_board(&db, "board_career").unwrap();
    assert_eq!(card_titles(&board.columns[0])[0], "Update resume");
    let _ = c;
}

#[test]
fn move_card_cross_column_and_positions() {
    let (_d, db) = fresh_db();
    let cols = col_ids(&db, "board_career");
    let board = get_board(&db, "board_career").unwrap();
    let todo_first = board.columns[0].cards[0].id.clone();
    let done_first = board.columns[2].cards[0].id.clone();

    // Move the To Do card into Done, before its first card.
    move_card(&db, &todo_first, &cols[2], Some(&done_first)).unwrap();

    let board = get_board(&db, "board_career").unwrap();
    assert_eq!(board.columns[0].cards.len(), 0);
    assert_eq!(board.columns[2].cards.len(), 2);
    assert_eq!(board.columns[2].cards[0].id, todo_first);
    assert_eq!(board.columns[2].cards[1].id, done_first);

    // Unknown card → "Card not found"
    let err = move_card(&db, "nope", &cols[0], None).unwrap_err();
    assert_eq!(err.to_string(), "Card not found");
}

#[test]
fn create_update_and_delete_card() {
    let (_d, db) = fresh_db();
    let cols = col_ids(&db, "board_work");

    let card = create_card(
        &db,
        &cols[0],
        &CreateCardBody {
            title: "New task".into(),
            description: Some("  details  ".into()),
            due_date: Some("2026-11-01".into()),
            labels: Some(vec![2, 5]),
        },
    )
    .unwrap();
    assert_eq!(card.title, "New task");
    assert_eq!(card.description, "details");
    assert_eq!(card.labels, vec![2, 5]);

    // Empty title → "Title required"
    let err = create_card(
        &db,
        &cols[0],
        &CreateCardBody {
            title: "   ".into(),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "Title required");

    update_card(
        &db,
        &card.id,
        &CreateCardBody {
            title: "Renamed".into(),
            description: None,
            due_date: None,
            labels: Some(vec![]),
        },
    )
    .unwrap();
    let board = get_board(&db, "board_work").unwrap();
    let updated = board.columns[0]
        .cards
        .iter()
        .find(|c| c.id == card.id)
        .unwrap();
    assert_eq!(updated.title, "Renamed");
    assert_eq!(updated.description, "");
    assert_eq!(updated.labels, vec![] as Vec<i64>);

    delete_card(&db, &card.id).unwrap();
    let board = get_board(&db, "board_work").unwrap();
    assert!(!board.columns[0].cards.iter().any(|c| c.id == card.id));
}

#[test]
fn create_card_in_status_targets_board_column() {
    let (_d, db) = fresh_db();

    let card = create_card_in_status(
        &db,
        "board_work",
        CardStatus::InProgress,
        &CreateCardBody {
            title: "Board-picked task".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let work = get_board(&db, "board_work").unwrap();
    let progress = &work.columns[1];
    assert_eq!(card.column_id, progress.id);
    assert_eq!(card_titles(progress).last(), Some(&"Board-picked task"));

    // The All board picks it up in the In Progress bucket on its home board.
    let all = get_all_board(&db).unwrap();
    assert!(
        all.columns[1]
            .cards
            .iter()
            .any(|c| c.card.id == card.id && c.board_id == "board_work")
    );

    // A bare board has no canonical status columns → clear error.
    let bare = create_board(&db, "Bare").unwrap();
    let err = create_card_in_status(
        &db,
        &bare.id,
        CardStatus::Todo,
        &CreateCardBody {
            title: "nope".into(),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("\"To Do\" column not found on board \"Bare\""),
        "unexpected error: {err}"
    );

    // Unknown board → "Board not found".
    let err = create_card_in_status(&db, "missing", CardStatus::Todo, &CreateCardBody::default())
        .unwrap_err();
    assert_eq!(err.to_string(), "Board not found");
}

#[test]
fn all_board_buckets_and_lazy_backfill() {
    let (_d, db) = fresh_db();

    // First load backfills all_card_order for all 9 seeded cards
    // (one per status column per board).
    let all = get_all_board(&db).unwrap();
    assert_eq!(all.id, "all");
    assert_eq!(all.columns.len(), 3);
    assert_eq!(all.columns[0].id, "all:todo");
    assert_eq!(all.columns[0].title, "To Do");
    assert_eq!(all.columns[0].cards.len(), 3);
    assert_eq!(all.columns[1].cards.len(), 3);
    assert_eq!(all.columns[2].cards.len(), 3);
    // Each All card carries board info
    assert_eq!(all.columns[0].cards[0].board_title, "Career");
    assert_eq!(
        all.columns[0].cards[0].card.column_id,
        col_ids(&db, "board_career")[0]
    );

    let n: i64 = db
        .query_row("SELECT COUNT(*) FROM all_card_order", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 9);

    // Second load is stable (no duplicates).
    let all2 = get_all_board(&db).unwrap();
    assert_eq!(all2.columns[0].cards.len(), 3);
}

#[test]
fn move_all_card_cross_bucket_updates_home_board_and_order() {
    let (_d, db) = fresh_db();
    let all = get_all_board(&db).unwrap();
    let card = &all.columns[0].cards[0]; // first To Do card (Career board)
    let card_id = card.card.id.clone();
    let board_id = card.board_id.clone();

    move_all_card(&db, &card_id, CardStatus::Done, None).unwrap();

    // All board: card appended to the Done bucket (3 seeded Done cards + 1).
    let all = get_all_board(&db).unwrap();
    assert_eq!(all.columns[2].cards.len(), 4);
    assert_eq!(all.columns[2].cards[3].card.id, card_id);
    assert_eq!(all.columns[0].cards.len(), 2);

    // Home board: card moved to the Done column.
    let board = get_board(&db, &board_id).unwrap();
    assert_eq!(
        board.columns[2]
            .cards
            .iter()
            .filter(|c| c.id == card_id)
            .count(),
        1
    );

    // Ordering persisted (appended at end of the done bucket).
    let (status, position): (String, i64) = db
        .query_row(
            "SELECT status, position FROM all_card_order WHERE card_id = ?",
            [&card_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "done");
    assert_eq!(position, 3);
}

#[test]
fn move_all_card_reorder_within_bucket() {
    let (_d, db) = fresh_db();
    let all = get_all_board(&db).unwrap();
    let first = all.columns[0].cards[0].card.id.clone();
    let third = all.columns[0].cards[2].card.id.clone();

    // Move first card before the third.
    move_all_card(&db, &first, CardStatus::Todo, Some(&third)).unwrap();

    let all = get_all_board(&db).unwrap();
    let ids: Vec<&str> = all.columns[0]
        .cards
        .iter()
        .map(|c| c.card.id.as_str())
        .collect();
    assert_eq!(ids[1], first.as_str());
    assert_eq!(ids[2], third.as_str());
}

#[test]
fn all_board_reconciles_after_home_board_move() {
    let (_d, db) = fresh_db();
    get_all_board(&db).unwrap(); // backfill

    // Move a card from To Do to In Progress on its real board.
    let cols = col_ids(&db, "board_work");
    let board = get_board(&db, "board_work").unwrap();
    let card_id = board.columns[0].cards[0].id.clone();
    move_card(&db, &card_id, &cols[1], None).unwrap();

    // All board reconciles the bucket without leaving a stale row.
    let all = get_all_board(&db).unwrap();
    assert_eq!(all.columns[0].cards.len(), 2);
    assert_eq!(all.columns[1].cards.len(), 4);
    assert!(all.columns[1].cards.iter().any(|c| c.card.id == card_id));

    // Persisted row updated, not duplicated.
    let (status, n): (String, i64) = db
        .query_row(
            "SELECT status, COUNT(*) FROM all_card_order WHERE card_id = ?",
            [&card_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "in_progress");
    assert_eq!(n, 1);
}

#[test]
fn archive_all_cards_real_and_virtual_columns() {
    let (_d, db) = fresh_db();
    let cols = col_ids(&db, "board_career");

    // Real column (holds 1 seeded card).
    let res = archive_all_cards(&db, &cols[0]).unwrap();
    assert_eq!(res.archived_count, 1);
    let board = get_board(&db, "board_career").unwrap();
    assert_eq!(board.columns[0].cards.len(), 0);

    // Virtual All column archives across every board's matching columns
    // (one In Progress card per board → 3).
    let res = archive_all_cards(&db, "all:in_progress").unwrap();
    assert_eq!(res.archived_count, 3);
    let res = archive_all_cards(&db, "all:done").unwrap();
    assert_eq!(res.archived_count, 3);

    let archived = list_archived_cards(&db).unwrap();
    assert_eq!(archived.cards.len(), 7);
    assert!(!archived.cards[0].board_title.is_empty());
    assert!(!archived.cards[0].archived_at.is_empty());

    // Unknown virtual status.
    let err = archive_all_cards(&db, "all:bogus").unwrap_err();
    assert_eq!(err.to_string(), "Unknown status");
}

#[test]
fn move_card_to_board_lands_in_matching_status_column() {
    let (_d, db) = fresh_db();
    let board = get_board(&db, "board_career").unwrap();
    let in_progress_card = board.columns[1].cards[0].id.clone();

    move_card_to_board(&db, &in_progress_card, "board_work").unwrap();

    // Work board's In Progress column had 1 seeded card, now 2.
    let work = get_board(&db, "board_work").unwrap();
    assert_eq!(work.columns[1].cards.len(), 2);
    assert!(
        work.columns[1]
            .cards
            .iter()
            .any(|c| c.id == in_progress_card)
    );
    let career = get_board(&db, "board_career").unwrap();
    assert_eq!(career.columns[1].cards.len(), 0);
}

#[test]
fn direct_cross_board_move_reconciles_project_and_area_links() {
    let (_d, db) = fresh_db();
    let root = _d.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let project = create_project(&db, &root, "Kandrium", None, "blank").unwrap();
    let project_todo = get_board(&db, &project.board_id).unwrap().columns[0]
        .id
        .clone();
    let card = create_card(
        &db,
        &project_todo,
        &CreateCardBody {
            title: "Ship it".into(),
            ..Default::default()
        },
    )
    .unwrap();
    set_card_area(&db, &card.id, Some("board_career")).unwrap();

    // Moving across boards through the generic `move_card` path must clear
    // the area mirror and keep the project link.
    let work_todo = col_ids(&db, "board_work")[0].clone();
    move_card(&db, &card.id, &work_todo, None).unwrap();
    let work = get_board(&db, "board_work").unwrap();
    let moved = work
        .columns
        .iter()
        .flat_map(|c| &c.cards)
        .find(|c| c.id == card.id)
        .unwrap();
    assert!(moved.area_board_id.is_none());
    assert_eq!(moved.project_id.as_deref(), Some(project.id.as_str()));

    // A plain card moved onto a project board becomes that project's card.
    let plain = create_card(
        &db,
        &col_ids(&db, "board_career")[0],
        &CreateCardBody {
            title: "Become project work".into(),
            ..Default::default()
        },
    )
    .unwrap();
    move_card(&db, &plain.id, &project_todo, None).unwrap();
    let project_board = get_board(&db, &project.board_id).unwrap();
    let landed = project_board
        .columns
        .iter()
        .flat_map(|c| &c.cards)
        .find(|c| c.id == plain.id)
        .unwrap();
    assert_eq!(landed.project_id.as_deref(), Some(project.id.as_str()));
    assert!(landed.area_board_id.is_none());
}

#[test]
fn rename_card_updates_only_the_title() {
    let (_d, db) = fresh_db();
    let board = get_board(&db, "board_career").unwrap();
    let card = &board.columns[0].cards[0];

    rename_card(&db, &card.id, "  Refreshed title  ").unwrap();

    let board = get_board(&db, "board_career").unwrap();
    let renamed = board.columns[0]
        .cards
        .iter()
        .find(|c| c.id == card.id)
        .unwrap();
    assert_eq!(renamed.title, "Refreshed title");
    assert_eq!(renamed.description, card.description);
    assert_eq!(renamed.due_date, card.due_date);
    assert_eq!(renamed.labels, card.labels);

    set_card_archived(&db, &card.id, true).unwrap();
    assert!(rename_card(&db, &card.id, "Too late").is_err());
}

#[test]
fn column_crud_and_reorder() {
    let (_d, db) = fresh_db();

    let col = create_column(&db, "board_life", "  Ideas  ").unwrap();
    assert_eq!(col.title, "Ideas");

    rename_column(&db, &col.id, "Renamed").unwrap();
    let board = get_board(&db, "board_life").unwrap();
    assert_eq!(board.columns.last().unwrap().title, "Renamed");

    move_column(&db, &col.id, Some(&board.columns[0].id)).unwrap();
    let board = get_board(&db, "board_life").unwrap();
    assert_eq!(board.columns[0].id, col.id);

    // Delete cascades cards.
    let cols = col_ids(&db, "board_life");
    delete_column(&db, &cols[0]).unwrap();
    let board = get_board(&db, "board_life").unwrap();
    assert_eq!(board.columns.len(), 3); // 3 original + 1 new - 1 deleted = 3

    let err = create_column(&db, "board_life", " ").unwrap_err();
    assert_eq!(err.to_string(), "Title required");
}

#[test]
fn set_card_archived_round_trip() {
    let (_d, db) = fresh_db();
    let board = get_board(&db, "board_career").unwrap();
    let card_id = board.columns[0].cards[0].id.clone();

    set_card_archived(&db, &card_id, true).unwrap();
    let board = get_board(&db, "board_career").unwrap();
    assert_eq!(board.columns[0].cards.len(), 0);

    set_card_archived(&db, &card_id, false).unwrap();
    let board = get_board(&db, "board_career").unwrap();
    assert_eq!(board.columns[0].cards.len(), 1);
}

#[test]
fn get_board_errors_for_missing_board() {
    let (_d, db) = fresh_db();
    let err = get_board(&db, "missing").unwrap_err();
    assert_eq!(err.to_string(), "Board not found");
}

#[test]
fn create_board_appends_without_columns() {
    let (_d, db) = fresh_db();
    let board = create_board(&db, "  Extra  ").unwrap();
    assert_eq!(board.title, "Extra");
    assert!(get_board(&db, &board.id).unwrap().columns.is_empty());

    let boards = list_boards(&db).unwrap().boards;
    assert_eq!(boards.last().unwrap().id, board.id);

    assert_eq!(
        create_board(&db, " ").unwrap_err().to_string(),
        "Title required"
    );
}

#[test]
fn project_boards_are_hidden_from_tasks_and_all_board() {
    let (_d, db) = fresh_db();
    let root = _d.path().join("projects");
    let project = create_project(&db, &root, "Private", None, "blank").unwrap();

    // The project's board is absent from the Tasks tab list…
    let boards = list_boards(&db).unwrap().boards;
    assert_eq!(boards.len(), 3);
    assert!(!boards.iter().any(|b| b.id == project.board_id));

    // …and its cards never appear on the virtual All board.
    let board = get_board(&db, &project.board_id).unwrap();
    let todo = board.columns[0].id.clone();
    create_card(
        &db,
        &todo,
        &CreateCardBody {
            title: "Hidden task".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let all = get_all_board(&db).unwrap();
    let hidden = all
        .columns
        .iter()
        .flat_map(|c| &c.cards)
        .any(|c| c.card.title == "Hidden task");
    assert!(!hidden, "project card leaked into the All board");
    // The seeded boards are still aggregated.
    assert_eq!(all.columns[0].cards.len(), 3);
}

#[test]
fn delete_board_via_project_cascades_cards_and_all_order() {
    let (_d, db) = fresh_db();
    let root = _d.path().join("projects");
    let project = create_project(&db, &root, "Cascade", None, "blank").unwrap();
    let board_id = project.board_id.clone();
    let board = get_board(&db, &board_id).unwrap();
    let todo = board.columns[0].id.clone();
    let card = create_card(
        &db,
        &todo,
        &CreateCardBody {
            title: "Task".into(),
            ..Default::default()
        },
    )
    .unwrap();
    // Give the card an All-board order row so the cascade has something to hit.
    db.execute(
        "INSERT INTO all_card_order (card_id, status, position) VALUES (?, 'todo', 0)",
        [&card.id],
    )
    .unwrap();

    delete_project(&db, &root, &project.id).unwrap();

    let leftover: i64 = db
        .query_row(
            "SELECT (SELECT COUNT(*) FROM cards WHERE id = ?) \
                  + (SELECT COUNT(*) FROM all_card_order WHERE card_id = ?) \
                  + (SELECT COUNT(*) FROM columns WHERE board_id = ?)",
            rusqlite::params![card.id, card.id, board_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftover, 0);
}

#[test]
fn project_cards_mirror_into_the_filed_area_board() {
    let (_d, db) = fresh_db();
    let root = _d.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let project = create_project(&db, &root, "Kandrium", None, "blank").unwrap();

    let project_board = get_board(&db, &project.board_id).unwrap();
    let todo = project_board.columns[0].id.clone();
    let progress = project_board.columns[1].id.clone();
    assert!(project_board.columns.iter().all(|c| c.mirrored.is_empty()));

    let card = create_card(
        &db,
        &todo,
        &CreateCardBody {
            title: "Ship it".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(card.project_id.as_deref(), Some(project.id.as_str()));
    assert_eq!(card.project_identifier.as_deref(), Some("kan"));

    // An unfiled project card has no area context to show.
    let project_board = get_board(&db, &project.board_id).unwrap();
    let home = project_board.columns[0]
        .cards
        .iter()
        .find(|c| c.id == card.id)
        .unwrap();
    assert!(home.meta_label.is_none());

    // File it to Career and it appears in the matching status column.
    set_card_area(&db, &card.id, Some("board_career")).unwrap();
    let career = get_board(&db, "board_career").unwrap();
    assert!(career.columns[0].cards.iter().all(|c| c.id != card.id));
    assert_eq!(career.columns[0].mirrored.len(), 1);
    let mirrored = &career.columns[0].mirrored[0];
    assert_eq!(mirrored.title, "Ship it");
    assert_eq!(
        mirrored.project_identifier.as_deref(),
        Some(project.identifier.as_str())
    );
    assert_eq!(mirrored.meta_label.as_deref(), Some("kan"));
    assert_eq!(mirrored.board_id, project.board_id);

    // On the project board the area replaces the redundant identifier.
    let project_board = get_board(&db, &project.board_id).unwrap();
    let home = project_board.columns[0]
        .cards
        .iter()
        .find(|c| c.id == card.id)
        .unwrap();
    assert_eq!(home.meta_label.as_deref(), Some("Career"));
    assert_eq!(home.project_identifier.as_deref(), Some("kan"));

    // Status changes on the home board follow the mirror.
    move_card(&db, &card.id, &progress, None).unwrap();
    let career = get_board(&db, "board_career").unwrap();
    assert!(career.columns[0].mirrored.is_empty());
    assert_eq!(career.columns[1].mirrored.len(), 1);

    // Area-filed project cards are part of the All board.
    let all = get_all_board(&db).unwrap();
    let all_card = all
        .columns
        .iter()
        .flat_map(|c| &c.cards)
        .find(|c| c.card.id == card.id)
        .expect("area-filed project card should be on the All board");
    // The badge names the board the card is filed onto, not the hidden
    // project board it physically lives on.
    assert_eq!(all_card.board_title, "Career");
    assert_eq!(all_card.board_id, project.board_id);

    // Clearing the area removes both mirrors.
    set_card_area(&db, &card.id, None).unwrap();
    let career = get_board(&db, "board_career").unwrap();
    assert!(career.columns.iter().all(|c| c.mirrored.is_empty()));
    let all = get_all_board(&db).unwrap();
    assert!(
        !all.columns
            .iter()
            .flat_map(|c| &c.cards)
            .any(|c| c.card.id == card.id)
    );
}

#[test]
fn card_project_and_area_setters_validate() {
    let (_d, db) = fresh_db();
    let root = _d.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let project = create_project(&db, &root, "Kandrium", None, "blank").unwrap();
    let project_todo = get_board(&db, &project.board_id).unwrap().columns[0]
        .id
        .clone();
    let card = create_card(
        &db,
        &project_todo,
        &CreateCardBody {
            title: "Ship it".into(),
            ..Default::default()
        },
    )
    .unwrap();

    // Area must be a non-project board.
    assert!(set_card_area(&db, &card.id, Some(&project.board_id)).is_err());
    assert!(set_card_area(&db, &card.id, Some("nope")).is_err());

    // A card that already lives on an area board cannot be mirrored to one.
    let career = get_board(&db, "board_career").unwrap();
    let native = career.columns[0].cards[0].id.clone();
    assert!(set_card_area(&db, &native, Some("board_work")).is_err());

    // Tagging a native card with a project resolves the identifier.
    set_card_project(&db, &native, Some(&project.id)).unwrap();
    let career = get_board(&db, "board_career").unwrap();
    assert_eq!(
        career.columns[0].cards[0].project_identifier.as_deref(),
        Some(project.identifier.as_str())
    );
    set_card_project(&db, &native, None).unwrap();
    let career = get_board(&db, "board_career").unwrap();
    assert!(career.columns[0].cards[0].project_identifier.is_none());
}

#[test]
fn moving_a_project_card_to_an_area_board_clears_its_mirror() {
    let (_d, db) = fresh_db();
    let root = _d.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let project = create_project(&db, &root, "Kandrium", None, "blank").unwrap();
    let todo = get_board(&db, &project.board_id).unwrap().columns[0]
        .id
        .clone();
    let card = create_card(
        &db,
        &todo,
        &CreateCardBody {
            title: "Ship it".into(),
            ..Default::default()
        },
    )
    .unwrap();
    set_card_area(&db, &card.id, Some("board_career")).unwrap();

    move_card_to_board(&db, &card.id, "board_work").unwrap();

    let work = get_board(&db, "board_work").unwrap();
    let moved = work
        .columns
        .iter()
        .flat_map(|c| &c.cards)
        .find(|c| c.id == card.id)
        .unwrap();
    assert!(moved.area_board_id.is_none());
    assert_eq!(
        moved.project_identifier.as_deref(),
        Some(project.identifier.as_str())
    );
    // On a normal board the project identifier is still the meta label.
    assert_eq!(
        moved.meta_label.as_deref(),
        Some(project.identifier.as_str())
    );
    let career = get_board(&db, "board_career").unwrap();
    assert!(career.columns.iter().all(|c| c.mirrored.is_empty()));
}
