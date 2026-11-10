//! Headless window tests for the shell and Stage 2 board interactivity.
//!
//! They boot the real `AppShell` over a freshly seeded temp database under
//! `TestAppContext` and assert on laid-out elements. No GPU or visible window
//! is involved.

use std::path::{Path, PathBuf};

use gpui::TestAppContext;
use gpui_kit::px;
use gpui_kit::test::TestWindowExt;

use kandrium_app_gpui::app::{AppShell, Section};
use kandrium_app_gpui::store::Store;

/// A bootstrapped database (schema + seed) in the temp directory.
fn seeded_db(dir: &Path) -> PathBuf {
    let path = dir.join("kandrium.db");
    let mut db = kandrium_core::open_db(&path).expect("open temp db");
    kandrium_core::bootstrap(&mut db, &path).expect("bootstrap temp db");
    path
}

#[gpui::test]
fn every_section_renders_its_body(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    // Tasks is the initial section and shows the real board.
    cx.update(|window, _cx| {
        assert!(window.find("board-view").visible());
    });

    let expected = [
        (Section::Clocks, "clocks-view"),
        (Section::Notes, "notes-placeholder"),
        (Section::Documents, "documents-placeholder"),
        (Section::Projects, "projects-placeholder"),
        (Section::Tasks, "board-view"),
    ];
    for (section, body) in expected {
        cx.update(|window, cx| window.click(format!("nav-{}", section.id()), cx));
        cx.update(|window, cx| window.render_frame(cx));
        cx.update(|window, _cx| {
            assert!(
                window.find(body).visible(),
                "{} did not render {body}",
                section
            );
        });
    }
}

#[gpui::test]
fn board_view_draws_real_columns_and_cards(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());

    // Read the expected ids through the same store the shell uses.
    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let board = store.board(&board_id).expect("board");
    let column = &board.columns[0];
    let column_id = column.id.clone();
    let card = column.ordered_cards()[0];
    let card_id = card.id.clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    let (column_width, card_width, card_visible) = cx.update(|window, _cx| {
        let column = window.find(format!("board-column-{column_id}"));
        let card = window.find(format!("board-card-{card_id}"));
        (
            column.bounds().size.width,
            card.bounds().size.width,
            card.visible(),
        )
    });

    assert_eq!(column_width, px(280.), "columns keep the app's 280px width");
    assert!(card_width > px(0.), "card did not lay out");
    assert!(card_visible, "card is not visible");
}

#[gpui::test]
fn clocks_view_renders_seeded_cities(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());

    let rows = Store::open_path(path.clone())
        .clocks(jiff::Timestamp::now(), true)
        .expect("clocks");
    assert!(!rows.is_empty(), "seed should contain cities");

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("nav-clocks", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        assert!(window.find("clocks-view").visible());
        assert!(window.find("clocks-toggle").visible());
        for row in &rows {
            assert!(
                window.find(format!("clock-card-{}", row.id)).visible(),
                "missing city card for {}",
                row.city
            );
        }
    });
}

/// Drag a card from the first column onto the second and assert the write
/// landed in SQLite. This exercises the whole Stage 2 path: gpui drag session
/// → `AppShell::drop_card` → `Store::move_card` → `kandrium_core::move_area_card`.
#[gpui::test]
fn dragging_a_card_to_another_column_persists(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());

    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let before = store.board(&board_id).expect("board");
    let source_column = before.columns[0].id.clone();
    let target_column = before.columns[1].id.clone();
    let card_id = before.columns[0].ordered_cards()[0].id.clone();
    assert!(
        before.columns[1]
            .ordered_cards()
            .iter()
            .all(|card| card.id != card_id),
        "seed should start the card on the first column"
    );

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    // Press the card, move past the 2px drag threshold, cross columns and drop.
    cx.update(|window, cx| {
        window.drag_to(
            format!("board-card-{card_id}"),
            format!("board-column-{target_column}"),
            cx,
        );
    });

    let after = Store::open_path(path).board(&board_id).expect("board");
    let source = after
        .columns
        .iter()
        .find(|column| column.id == source_column)
        .expect("source column");
    let target = after
        .columns
        .iter()
        .find(|column| column.id == target_column)
        .expect("target column");

    assert!(
        source.ordered_cards().iter().all(|card| card.id != card_id),
        "card {card_id} is still in {source_column}"
    );
    assert!(
        target.ordered_cards().iter().any(|card| card.id == card_id),
        "card {card_id} did not land in {target_column}"
    );
}

/// Dropping onto the top half of a card must insert *before* it, which is the
/// within-column reorder path (and `move_area_card`'s `before_card_id`).
#[gpui::test]
fn dropping_on_a_cards_top_half_inserts_before_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());

    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let before = store.board(&board_id).expect("board");
    let target_column = before.columns[1].id.clone();
    let moving = before.columns[0].ordered_cards()[0].id.clone();
    let anchor = before.columns[1].ordered_cards()[0].id.clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    let (from, to) = cx.update(|window, _cx| {
        let from = window
            .find(format!("board-card-{moving}"))
            .bounds()
            .center();
        let anchor = window.find(format!("board-card-{anchor}")).bounds();
        // A few pixels below the top edge: above the card's midline.
        (
            from,
            gpui::point(anchor.center().x, anchor.origin.y + px(4.)),
        )
    });
    cx.update(|window, cx| window.drag(from, to, cx));

    let after = Store::open_path(path).board(&board_id).expect("board");
    let order: Vec<_> = after
        .columns
        .iter()
        .find(|column| column.id == target_column)
        .expect("target column")
        .ordered_cards()
        .iter()
        .map(|card| card.id.clone())
        .collect();

    assert_eq!(
        order,
        vec![moving.clone(), anchor.clone()],
        "dropping on the top half should place {moving} before {anchor}"
    );
}

/// Project cards filed onto an area board are mirrors. Dragging a mirror to
/// another area column must move the card's *project* column, never file it
/// into the area board's own column (`move_area_card` mirror semantics).
#[gpui::test]
fn dragging_a_mirror_across_area_statuses_moves_its_project_column(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());

    let mut db = kandrium_core::open_db(&path).expect("open db");
    kandrium_core::bootstrap(&mut db, &path).expect("bootstrap");
    let projects_root = dir.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();
    let project = kandrium_core::create_project(&db, &projects_root, "Kandrium", None, "blank")
        .expect("project");
    let project_board =
        kandrium_core::get_board_readonly(&db, &project.board_id).expect("project board");
    let project_todo = project_board.columns[0].id.clone();
    let project_progress = project_board.columns[1].id.clone();
    let card = kandrium_core::create_card(
        &db,
        &project_todo,
        &kandrium_core::CreateCardBody {
            title: "Mirror".into(),
            ..Default::default()
        },
    )
    .expect("card");
    kandrium_core::set_card_area(&db, &card.id, Some("board_career"))
        .expect("file onto area board");
    drop(db);

    let store = Store::open_path(path.clone());
    let area = store.board("board_career").expect("area board");
    let area_todo = area
        .columns
        .iter()
        .find(|column| column.title == "To Do")
        .expect("area To Do")
        .id
        .clone();
    let area_progress = area
        .columns
        .iter()
        .find(|column| column.title == "In Progress")
        .expect("area In Progress")
        .id
        .clone();
    assert!(
        area.columns
            .iter()
            .find(|column| column.id == area_todo)
            .expect("area To Do")
            .ordered_cards()
            .iter()
            .any(|card_on_board| card_on_board.id == card.id),
        "the mirror should start in the area board's To Do column"
    );
    drop(store);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| {
        window.drag_to(
            format!("board-card-{}", card.id),
            format!("board-column-{area_progress}"),
            cx,
        );
    });

    // The mirror's status change moved the real (project) column.
    let project_after = Store::open_path(path.clone())
        .board(&project.board_id)
        .expect("project board");
    let progress = project_after
        .columns
        .iter()
        .find(|column| column.id == project_progress)
        .expect("project In Progress");
    assert!(
        progress
            .ordered_cards()
            .iter()
            .any(|card_on_board| card_on_board.id == card.id),
        "the mirror's project card should now be in the project's In Progress column"
    );

    // And the area board shows it under In Progress, not filed as a native To Do card.
    let area_after = Store::open_path(path)
        .board("board_career")
        .expect("area board");
    let todo = area_after
        .columns
        .iter()
        .find(|column| column.id == area_todo)
        .expect("area To Do");
    assert!(
        todo.ordered_cards()
            .iter()
            .all(|card_on_board| card_on_board.id != card.id),
        "the mirror must not stay in the area board's To Do column"
    );
    let progress = area_after
        .columns
        .iter()
        .find(|column| column.id == area_progress)
        .expect("area In Progress");
    assert!(
        progress
            .ordered_cards()
            .iter()
            .any(|card_on_board| card_on_board.id == card.id),
        "the mirror should now render under the area board's In Progress column"
    );
}

/// Clicking a card opens the editor; editing the title and saving writes it
/// through `kandrium_core::update_card`.
#[gpui::test]
fn card_modal_saves_a_new_title(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let card = store.board(&board_id).expect("board").columns[0].ordered_cards()[0].clone();
    let card_id = card.id.clone();
    assert_ne!(card.title, "Renamed card");

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click(format!("board-card-{card_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| assert!(window.find("card-modal").visible()));

    // The title field is focused on open, so select all and replace.
    cx.update(|window, cx| {
        window.press("cmd-a", cx);
        window.input("Renamed card", cx);
    });
    cx.update(|window, cx| window.click("card-modal-save", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-modal").is_none(),
            "save should close the modal"
        );
    });

    let reloaded = Store::open_path(path).board(&board_id).expect("board");
    let saved = reloaded
        .columns
        .iter()
        .flat_map(|column| column.ordered_cards())
        .find(|card| card.id == card_id)
        .expect("card after save");
    assert_eq!(saved.title, "Renamed card");
}

/// Label chips toggle in the session and persist on save.
#[gpui::test]
fn card_modal_label_toggle_persists(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let card = store.board(&board_id).expect("board").columns[0].ordered_cards()[0].clone();
    let card_id = card.id.clone();
    let mut expected = card.labels.clone();
    expected.push(3);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click(format!("board-card-{card_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("card-label-3", cx));
    cx.update(|window, cx| window.render_frame(cx));
    // The eye/pencil toggle swaps the textarea for the markdown preview.
    cx.update(|window, cx| window.click("card-description-toggle", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("card-description-preview").visible(),
            "preview should replace the editor"
        );
    });
    cx.update(|window, cx| window.click("card-modal-save", cx));
    cx.update(|window, cx| window.render_frame(cx));

    let reloaded = Store::open_path(path).board(&board_id).expect("board");
    let saved = reloaded
        .columns
        .iter()
        .flat_map(|column| column.ordered_cards())
        .find(|card| card.id == card_id)
        .expect("card after save");
    assert_eq!(saved.labels, expected);
}

/// Cancelling (or a backdrop press) must not write anything.
#[gpui::test]
fn card_modal_cancel_discards_changes(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let card = store.board(&board_id).expect("board").columns[0].ordered_cards()[0].clone();
    let card_id = card.id.clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click(format!("board-card-{card_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("card-label-5", cx));
    cx.update(|window, cx| window.click("card-modal-cancel", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-modal").is_none(),
            "cancel should close the modal"
        );
    });

    let reloaded = Store::open_path(path).board(&board_id).expect("board");
    let saved = reloaded
        .columns
        .iter()
        .flat_map(|column| column.ordered_cards())
        .find(|card| card.id == card_id)
        .expect("card after cancel");
    assert_eq!(saved.labels, card.labels, "cancel must not persist labels");
}
