//! Headless window tests for the shell and Stage 2 board interactivity.
//!
//! They boot the real `AppShell` over a freshly seeded temp database under
//! `TestAppContext` and assert on laid-out elements. No GPU or visible window
//! is involved.

use std::path::{Path, PathBuf};

use gpui::TestAppContext;
use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render, Styled,
    TestSupportExt, Window, div, px,
};

use kandrium_app::app::{AppShell, Section};
use kandrium_app::settings::{Settings, TreePanel};
use kandrium_app::store::Store;
use kandrium_app::theme::Tokens;

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
    cx.update(gpui_kit::init);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    // Tasks is the initial section and shows the real board.
    cx.update(|window, _cx| {
        assert!(window.find("board-view").visible());
    });

    let expected = [
        (Section::Clocks, "clocks-view"),
        (Section::Wiki, "wiki-view"),
        (Section::Projects, "projects-view"),
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
    cx.update(gpui_kit::init);

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
    cx.update(gpui_kit::init);

    let data = Store::open_path(path.clone()).clock_data().expect("clocks");
    assert!(!data.clocks.is_empty(), "seed should contain cities");

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("nav-clocks", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        assert!(window.find("clocks-view").visible());
        assert!(window.find("clocks-units").visible());
        assert!(window.find("clocks-add-city").visible());
        assert!(window.find("clocks-search").visible());
        for row in &data.clocks {
            assert!(
                window.find(format!("clock-card-{}", row.id)).visible(),
                "missing city card for {}",
                row.tz_identifier
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
    cx.update(gpui_kit::init);

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
    cx.update(gpui_kit::init);

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
    cx.update(gpui_kit::init);

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

/// The titlebar clock at its 10 px mono size still leaves the drag stretch
/// alive at the 940 px minimum window width, stays flush right, and never
/// overlaps the section chips.
#[gpui::test]
fn titlebar_clock_fits_at_minimum_window_width(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    // The seed already saves a city, so the titlebar clock renders.

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        let bar = window.find("titlebar").bounds();
        let drag = window.find("titlebar-drag-right").bounds();
        let chips = window.find("section-control").bounds();
        let clock = window.find("titlebar-clock").bounds();
        assert!(drag.size.width > px(0.), "drag stretch collapsed: {drag:?}");
        assert!(
            chips.right() <= clock.left(),
            "clock overlaps the chips: chips {chips:?}, clock {clock:?}"
        );
        // Layout is linear from the 68 px traffic-light inset, so the width a
        // window needs to show the same row is the clocks' natural width plus
        // the 10 px row padding (the drag stretch is the only flexible part).
        let needed = chips.right() + clock.size.width + px(10.);
        assert!(
            needed <= px(940.),
            "row needs {needed:?} at 10 px mono, over the 940 px minimum"
        );
        let gap = bar.right() - clock.right();
        assert!(
            gap >= px(0.) && gap <= px(10.),
            "clock is not flush right (gap {gap:?})"
        );
    });
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
    // The mode control keeps both segments visible; the preview one swaps the
    // textarea for the markdown preview.
    cx.update(|window, _cx| {
        assert!(window.find("card-description-toggle-edit").visible());
        assert!(window.find("card-description-toggle-preview").visible());
    });
    cx.update(|window, cx| window.click("card-description-toggle-preview", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("card-description-preview").visible(),
            "preview should replace the editor"
        );
    });
    cx.update(|window, cx| window.click("card-description-toggle-edit", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-description-preview").is_none(),
            "Edit switches back to the editor"
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

/// The All tab comes first and groups cards by status; dropping a card on a
/// status bucket moves it on its home board.
#[gpui::test]
fn all_board_tab_groups_cards_and_moves_status(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let card = store.board(&board_id).expect("board").columns[0].ordered_cards()[0].clone();
    let card_id = card.id.clone();
    let home_column = card.column_id.clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click("board-tab-all", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.find("board-column-all:todo").visible());
        assert!(window.find("board-column-all:done").visible());
        assert!(window.find(format!("board-card-{card_id}")).visible());
    });

    cx.update(|window, cx| {
        window.drag_to(format!("board-card-{card_id}"), "board-column-all:done", cx)
    });
    cx.update(|window, cx| window.render_frame(cx));

    let reloaded = Store::open_path(path).board(&board_id).expect("board");
    let done = reloaded
        .columns
        .iter()
        .find(|column| column.title == "Done")
        .expect("Done column");
    assert_ne!(done.id, home_column);
    assert!(
        done.ordered_cards().iter().any(|card| card.id == card_id),
        "the card should have moved into the Done column of its home board"
    );
}

/// Escape closes the editor without writing.
#[gpui::test]
fn card_modal_escape_closes(cx: &mut TestAppContext) {
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
    cx.update(|window, _cx| assert!(window.find("card-modal").visible()));
    cx.update(|window, cx| window.press("escape", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-modal").is_none(),
            "escape should close the modal"
        );
    });
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

/// "Move to board" persists the edit, moves the card and closes the editor.
#[gpui::test]
fn card_modal_moves_card_to_another_board(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let store = Store::open_path(path.clone());
    let boards = store.boards().expect("boards");
    let home = boards[0].id.clone();
    let target = boards[1].id.clone();
    let card_id = store.board(&home).expect("board").columns[0].ordered_cards()[0]
        .id
        .clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("board-card-{card_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("card-move-{target}"), cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-modal").is_none(),
            "a move should close the editor"
        );
    });
    let store = Store::open_path(path.clone());
    let moved = store.board(&target).expect("target board");
    assert!(
        moved
            .columns
            .iter()
            .flat_map(|column| column.ordered_cards())
            .any(|card| card.id == card_id),
        "card should be on the target board"
    );
    let home_board = store.board(&home).expect("home board");
    assert!(
        !home_board
            .columns
            .iter()
            .flat_map(|column| column.ordered_cards())
            .any(|card| card.id == card_id),
        "card should have left its home board"
    );
}

/// The due-date calendar opens on the stored month, writes a picked day
/// through the card update path, clears it, and closes on Escape untouched.
#[gpui::test]
fn card_modal_due_calendar_picks_clears_and_escapes(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    let mut db = kandrium_core::open_db(&path).expect("open db");
    kandrium_core::bootstrap(&mut db, &path).expect("bootstrap");
    let board = kandrium_core::get_board_readonly(&db, "board_career").expect("board");
    let column = board.columns[0].id.clone();
    let card = kandrium_core::create_card(
        &db,
        &column,
        &kandrium_core::CreateCardBody {
            title: "Dated".into(),
            due_date: Some("2026-03-05".into()),
            ..Default::default()
        },
    )
    .expect("card");
    drop(db);

    cx.update(gpui_kit::init);
    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("board-card-{}", card.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("card-due-picker", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.find("card-due-menu").visible());
        assert!(
            window.find("card-due-day-2026-03-05").visible(),
            "the calendar must open on the stored month"
        );
        let row = window.find("card-due-picker").bounds();
        let menu = window.find("card-due-menu").bounds();
        assert!(
            row.size.width < px(170.),
            "the due field still stretches across the row: {row:?}"
        );
        assert!(
            row.size.width > px(110.),
            "the due field is too cramped: {row:?}"
        );
        let icon = window.find("card-due-calendar").bounds();
        assert!(
            icon.right() <= row.right(),
            "the calendar icon leaves the field: {icon:?} vs {row:?}"
        );
        assert!(
            menu.left() >= row.right(),
            "the calendar covers the field: {menu:?} vs {row:?}"
        );
        assert_eq!(
            menu.origin.y, row.origin.y,
            "the calendar is not top-aligned with the field"
        );
        assert!(
            menu.right() <= px(1920.),
            "the calendar clips the window edge: {menu:?}"
        );
    });

    // The icon and the field text are one toggle: closing leaves the value
    // alone and keeps the editor open, and both reopen it.
    cx.update(|window, cx| window.click("card-due-calendar", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-due-menu").is_none(),
            "the icon must close the calendar"
        );
        assert!(window.find("card-modal").visible());
    });
    assert_eq!(
        saved_due_date(&path, "board_career", &card.id),
        "2026-03-05"
    );

    cx.update(|window, cx| window.click("card-due-picker", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("card-due-menu").visible(),
            "clicking the field must reopen the calendar"
        );
    });
    cx.update(|window, cx| window.click("card-due-picker", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-due-menu").is_none(),
            "clicking the open field must close the calendar"
        );
    });
    cx.update(|window, cx| window.click("card-due-picker", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click("card-due-day-2026-03-05", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.try_find("card-due-menu").is_none());
        assert!(window.find("card-modal").visible());
    });
    assert_eq!(
        saved_due_date(&path, "board_career", &card.id),
        "2026-03-05"
    );

    cx.update(|window, cx| window.click("card-due-picker", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("card-due-clear", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert_eq!(saved_due_date(&path, "board_career", &card.id), "");

    cx.update(|window, cx| window.click("card-due-picker", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.press("escape", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-due-menu").is_none(),
            "Escape should close the calendar"
        );
        assert!(
            window.find("card-modal").visible(),
            "Escape must not close the editor"
        );
    });
    assert_eq!(saved_due_date(&path, "board_career", &card.id), "");
}

fn saved_due_date(path: &std::path::Path, board_id: &str, card_id: &str) -> String {
    Store::open_path(path.to_path_buf())
        .board(board_id)
        .expect("board")
        .columns
        .iter()
        .flat_map(|column| column.ordered_cards())
        .find(|card| card.id == card_id)
        .expect("card")
        .due_date
        .clone()
}

/// Picking a project in the dropdown writes the link immediately, refreshes
/// the board's rows and `meta_label`, and leaves the editor open.
#[gpui::test]
fn card_modal_project_dropdown_tags_the_card_immediately(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let store = Store::open_path(path.clone());
    let root = dir.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let project = store
        .create_project(&root, "Parity", None)
        .expect("project");
    let board_id = store.boards().expect("boards")[0].id.clone();
    let card_id = store.board(&board_id).expect("board").columns[0].ordered_cards()[0]
        .id
        .clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("board-card-{card_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("card-project-picker", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.find("card-project-menu").visible());
        assert!(window.find("card-project-option-none").visible());
    });

    cx.update(|window, cx| window.click(format!("card-project-option-{}", project.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("card-modal").visible(),
            "picking a project must not close the editor"
        );
        assert!(
            window.try_find("card-project-menu").is_none(),
            "the menu should close after a pick"
        );
    });

    let reloaded = Store::open_path(path.clone())
        .board(&board_id)
        .expect("board");
    let card = reloaded
        .columns
        .iter()
        .flat_map(|column| column.ordered_cards())
        .find(|card| card.id == card_id)
        .expect("card");
    assert_eq!(card.project_id.as_deref(), Some(project.id.as_str()));
    assert_eq!(
        card.meta_label.as_deref(),
        Some(project.identifier.as_str()),
        "the row label must catch up without reopening the editor"
    );
}

/// A normal card's editor explains the project tag and offers board moves;
/// the project-card-only rows are absent.
#[gpui::test]
fn card_modal_shows_project_and_move_rows(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let store = Store::open_path(path.clone());
    let root = dir.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_project(&root, "Parity", None)
        .expect("project");
    let board_id = store.boards().expect("boards")[0].id.clone();
    let card_id = store.board(&board_id).expect("board").columns[0].ordered_cards()[0]
        .id
        .clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("board-card-{card_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        assert!(
            window.find("card-project-hint").visible(),
            "normal cards should explain the project tag"
        );
        assert!(
            window.find("card-move").visible(),
            "normal cards should offer other boards"
        );
        assert!(window.try_find("card-area-hint").is_none());
        assert!(window.try_find("card-modal-project").is_none());
    });
}

/// Open the first seeded card, focus its description and clear it from Normal
/// mode so the vim tests start from a known empty buffer.
fn open_empty_description(cx: &mut gpui::VisualTestContext, path: &Path) -> (String, String) {
    cx.update(|_window, cx| gpui_kit::init(cx));
    let store = Store::open_path(path.to_path_buf());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let card_id = store.board(&board_id).expect("board").columns[0].ordered_cards()[0]
        .id
        .clone();

    cx.update(|window, cx| window.click(format!("board-card-{card_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.find("card-modal").visible(), "modal did not open");
    });
    cx.update(|window, cx| window.click("card-description-field", cx));
    cx.update(|window, cx| window.render_frame(cx));
    for key in ["g", "g", "d", "d"] {
        cx.update(|window, cx| window.press(key, cx));
    }
    (board_id, card_id)
}

fn saved_description(path: &Path, board_id: &str, card_id: &str) -> String {
    let board = Store::open_path(path.to_path_buf())
        .board(board_id)
        .expect("board");
    board
        .columns
        .iter()
        .flat_map(|column| column.ordered_cards())
        .find(|card| card.id == card_id)
        .expect("card after save")
        .description
        .clone()
}

/// `i` types through the textarea, `Esc` returns to Normal mode without
/// closing the modal, and Normal-mode keys edit instead of typing.
#[gpui::test]
fn card_modal_description_vim_insert_then_normal(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    let (board_id, card_id) = open_empty_description(cx, &path);

    cx.update(|window, cx| window.press("i", cx));
    cx.update(|window, cx| window.input("vim typed", cx));
    cx.update(|window, cx| window.press("escape", cx));
    cx.update(|window, cx| window.press("x", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("card-modal").visible(),
            "Esc in Insert must only leave Insert"
        )
    });

    cx.update(|window, cx| window.click("card-modal-save", cx));
    cx.update(|window, cx| window.render_frame(cx));

    assert_eq!(
        saved_description(&path, &board_id, &card_id),
        "vim type",
        "insert text should be kept and `x` should delete the last character"
    );
}

/// Uppercase commands arrive through the keystroke's `key_char`, so
/// `Shift-G` and `Shift-D` must reach the vim layer as `G` and `D`.
#[gpui::test]
fn card_modal_description_vim_uppercase_commands(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    let (board_id, card_id) = open_empty_description(cx, &path);

    cx.update(|window, cx| window.press("i", cx));
    cx.update(|window, cx| window.input("ab", cx));
    cx.update(|window, cx| window.press("enter", cx));
    cx.update(|window, cx| window.input("cd", cx));
    cx.update(|window, cx| window.press("escape", cx));
    for key in ["g", "g", "shift-g", "shift-d"] {
        cx.update(|window, cx| window.press(key, cx));
    }
    cx.update(|window, cx| window.click("card-modal-save", cx));
    cx.update(|window, cx| window.render_frame(cx));

    assert_eq!(
        saved_description(&path, &board_id, &card_id),
        "ab",
        "`G` should move to the last line and `D` delete its contents"
    );
}

/// Visual mode writes an inclusive selection back to the textarea.
#[gpui::test]
fn card_modal_description_vim_visual_delete(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    let (board_id, card_id) = open_empty_description(cx, &path);

    cx.update(|window, cx| window.press("i", cx));
    cx.update(|window, cx| window.input("abcd", cx));
    cx.update(|window, cx| window.press("escape", cx));
    for key in ["g", "g", "v", "l", "l", "d"] {
        cx.update(|window, cx| window.press(key, cx));
    }
    cx.update(|window, cx| window.click("card-modal-save", cx));
    cx.update(|window, cx| window.render_frame(cx));

    assert_eq!(saved_description(&path, &board_id, &card_id), "d");
}

/// `:w` writes the card and keeps the editor open; `Esc` afterwards closes it.
#[gpui::test]
fn card_modal_colon_write_saves_without_closing(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    let (board_id, card_id) = open_empty_description(cx, &path);

    cx.update(|window, cx| window.press("i", cx));
    cx.update(|window, cx| window.input("saved by vim", cx));
    cx.update(|window, cx| window.press("escape", cx));
    for key in [":", "w", "enter"] {
        cx.update(|window, cx| window.press(key, cx));
    }
    cx.update(|window, cx| window.render_frame(cx));

    assert_eq!(
        saved_description(&path, &board_id, &card_id),
        "saved by vim"
    );
    cx.update(|window, _cx| {
        assert!(
            window.find("card-modal").visible(),
            ":w must save without closing the modal"
        )
    });

    cx.update(|window, cx| window.press("escape", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-modal").is_none(),
            "Esc in Normal mode closes the editor"
        )
    });
}

/// The shell writes section, tree collapse and the vim preference to
/// `gpui-settings.conf` next to the database; with vim off the description
/// takes plain typing and needs no `i`.
#[gpui::test]
fn settings_file_records_section_tree_and_vim(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let settings_path = dir.path().join(kandrium_app::settings::FILE_NAME);

    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let card_id = store.board(&board_id).expect("board").columns[0].ordered_cards()[0]
        .id
        .clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    // Section and tree flag.
    cx.update(|window, cx| window.click("nav-wiki", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("notes-collapse", cx));
    cx.update(|window, cx| window.render_frame(cx));

    let saved = Settings::load(&settings_path);
    assert_eq!(saved.section(), Section::Wiki);
    assert!(!saved.tree_open(TreePanel::Wiki));

    // Vim preference from the card modal's mode badge.
    cx.update(|window, cx| window.click("nav-tasks", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("board-card-{card_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("card-description-vim-toggle", cx));
    cx.update(|window, cx| window.render_frame(cx));

    let saved = Settings::load(&settings_path);
    assert!(!saved.vim_mode(), "turning vim off must be persisted");

    // Plain typing without `i`, then save.
    cx.update(|window, cx| window.click("card-description-field", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.press("cmd-a", cx));
    cx.update(|window, cx| window.input("plain typing", cx));
    cx.update(|window, cx| window.click("card-modal-save", cx));
    cx.update(|window, cx| window.render_frame(cx));

    assert_eq!(
        saved_description(&path, &board_id, &card_id),
        "plain typing",
        "with vim off the description must accept typing directly"
    );
}

/// Columns shrink-wrap their content: a column with one card must be shorter
/// than the board, a column whose cards overflow must stop at the board and
/// scroll, and an empty column must still be tall enough to drop onto.
#[gpui::test]
fn columns_hug_their_cards_within_the_viewport(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    // Use the three canonical seeded columns: To Do is overfilled, In Progress
    // keeps its one seeded card, Done is emptied. Extra columns would be folded
    // into a canonical lane by `lanes`, so this stays on standard ids.
    let db = kandrium_core::open_db(&path).expect("open temp db");
    let board_id = kandrium_core::list_boards(&db).expect("boards").boards[0]
        .id
        .clone();
    let board = kandrium_core::get_board(&db, &board_id).expect("board");
    let tall_column = board.columns[0].id.clone();
    let short_column = board.columns[1].id.clone();
    let empty_column = board.columns[2].id.clone();
    for index in 0..20 {
        kandrium_core::create_card(
            &db,
            &tall_column,
            &kandrium_core::CreateCardBody {
                title: format!("Height QA {index}"),
                description: None,
                due_date: None,
                labels: None,
            },
        )
        .expect("create card");
    }
    for card in board.columns[2].ordered_cards() {
        kandrium_core::delete_card(&db, &card.id).expect("delete seeded card");
    }
    assert!(
        kandrium_core::get_board(&db, &board_id)
            .expect("board")
            .columns
            .iter()
            .find(|column| column.id == empty_column)
            .expect("empty column")
            .cards
            .is_empty(),
        "the empty column must be empty before the window opens"
    );
    drop(db);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    let (ceiling, tall, short, empty) = cx.update(|window, _cx| {
        let ceiling = window.find("board-columns").bounds().size.height;
        let tall = window
            .find(format!("board-column-{tall_column}"))
            .bounds()
            .size
            .height;
        let short = window
            .find(format!("board-column-{short_column}"))
            .bounds()
            .size
            .height;
        let empty = window
            .find(format!("board-column-{empty_column}"))
            .bounds()
            .size
            .height;
        (ceiling, tall, short, empty)
    });

    assert!(ceiling > px(0.), "board columns did not lay out");
    assert!(
        short < ceiling - px(40.),
        "a one-card column should hug its content ({short:?} vs {ceiling:?})"
    );
    assert!(
        tall <= ceiling,
        "an overfull column must stop at the viewport ({tall:?} vs {ceiling:?})"
    );
    assert!(
        tall > ceiling - px(24.),
        "an overfull column should fill the viewport before scrolling ({tall:?} vs {ceiling:?})"
    );
    assert!(
        empty < ceiling,
        "an empty column should not stretch to the board ({empty:?} vs {ceiling:?})"
    );
    assert!(
        empty > px(55.),
        "an empty column must stay a usable drop target ({empty:?})"
    );
}

/// The "+ Add card" footer exists on every column of a real board, opens an
/// inline draft, and Enter creates the card in that column.
#[gpui::test]
fn the_column_footer_adds_a_card(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let board = store.board(&board_id).expect("board");
    let column_id = board.columns[0].id.clone();
    let before = board.columns[0].ordered_cards().len();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        for column in &board.columns {
            assert!(
                window
                    .find(format!("board-add-card-{}", column.id))
                    .visible(),
                "missing add row for {}",
                column.id
            );
        }
    });

    cx.update(|window, cx| window.click(format!("board-add-card-{column_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find(format!("board-add-form-{column_id}")).visible(),
            "the draft did not open"
        );
    });

    cx.update(|window, cx| {
        window.input("From the footer", cx);
        window.press("enter", cx);
    });
    cx.update(|window, cx| window.render_frame(cx));

    let after = Store::open_path(path).board(&board_id).expect("board");
    let column = after
        .columns
        .iter()
        .find(|column| column.id == column_id)
        .expect("column");
    assert_eq!(
        column.ordered_cards().len(),
        before + 1,
        "Enter did not create a card"
    );
    assert!(
        column
            .ordered_cards()
            .iter()
            .any(|card| card.title == "From the footer"),
        "the card title was not saved"
    );
    cx.update(|window, _cx| {
        assert!(
            window
                .try_find(format!("board-add-form-{column_id}"))
                .is_none(),
            "the draft should close after a successful add"
        );
        assert!(
            window.find(format!("board-add-card-{column_id}")).visible(),
            "the add row should come back"
        );
    });
}

/// An empty column still offers the row, Escape keeps the draft without
/// creating a card, and reopening restores that text.
#[gpui::test]
fn the_add_draft_survives_escape_on_an_empty_column(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let db = kandrium_core::open_db(&path).expect("open temp db");
    let board_id = kandrium_core::list_boards(&db).expect("boards").boards[0]
        .id
        .clone();
    let board = kandrium_core::get_board(&db, &board_id).expect("board");
    let empty_column = board.columns[2].id.clone();
    for card in board.columns[2].ordered_cards() {
        kandrium_core::delete_card(&db, &card.id).expect("delete seeded card");
    }
    drop(db);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click(format!("board-add-card-{empty_column}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| {
        window.input("Draft survives", cx);
        window.press("escape", cx);
    });
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window
                .try_find(format!("board-add-form-{empty_column}"))
                .is_none(),
            "Escape should close the draft"
        );
    });
    assert!(
        Store::open_path(path.clone())
            .board(&board_id)
            .expect("board")
            .columns
            .iter()
            .find(|column| column.id == empty_column)
            .expect("empty column")
            .ordered_cards()
            .is_empty(),
        "Escape must not create a card"
    );

    // Reopening restores the stashed text, so Enter alone commits it.
    cx.update(|window, cx| window.click(format!("board-add-card-{empty_column}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.press("enter", cx));
    cx.update(|window, cx| window.render_frame(cx));

    let cards = Store::open_path(path)
        .board(&board_id)
        .expect("board")
        .columns
        .iter()
        .find(|column| column.id == empty_column)
        .expect("empty column")
        .ordered_cards()
        .iter()
        .map(|card| card.title.clone())
        .collect::<Vec<_>>();
    assert_eq!(cards, vec!["Draft survives"], "the draft was not restored");
}

/// The synthetic All board has no real column to write to, so it shows no
/// add row; real boards do.
#[gpui::test]
fn the_all_board_has_no_add_row(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click("board-tab-all", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        let footers = window
            .find_all("board-add-card-all:todo")
            .into_iter()
            .filter(|footer| footer.visible())
            .count();
        assert_eq!(footers, 0, "the All board must not offer a footer");
    });
}

/// Saved cities in display order: `(clock id, city name)`.
fn saved_cities(path: &Path) -> Vec<(String, String)> {
    Store::open_path(path.to_path_buf())
        .clock_data()
        .expect("clock data")
        .clocks
        .into_iter()
        .map(|clock| {
            let city = kandrium_core::city_name(&clock.tz_identifier);
            (clock.id, city)
        })
        .collect()
}

/// The window move surface is only the empty stretch. The section control —
/// chips, their 2 px gaps and its 2 px padding — lies left of it, and the
/// clock sits at the far right, so neither can start a window move or zoom.
#[gpui::test]
fn the_titlebar_drag_surface_excludes_the_section_control(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path)));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        let left = window.find("titlebar-drag-left").bounds();
        let control = window.find("section-control").bounds();
        let right = window.find("titlebar-drag-right").bounds();
        let clock = window.find("titlebar-clock").bounds();
        let left_end = left.origin.x + left.size.width;
        let control_end = control.origin.x + control.size.width;
        let right_end = right.origin.x + right.size.width;
        assert!(
            left_end <= control.origin.x,
            "the left drag region overlaps the section control"
        );
        assert!(
            control_end <= right.origin.x,
            "the empty stretch overlaps the section control"
        );
        assert!(
            right_end <= clock.origin.x,
            "the clock must sit after the empty stretch"
        );
    });
}

/// At the 940 px minimum window width the wider clock still fits: it keeps
/// its 8 px right inset and the section control keeps its space.
#[gpui::test]
fn the_titlebar_clock_fits_the_minimum_window_width(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path)));
    cx.simulate_resize(gpui::size(px(940.), px(700.)));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        let control = window.find("section-control").bounds();
        let clock = window.find("titlebar-clock").bounds();
        let clock_end = clock.origin.x + clock.size.width;
        assert!(
            control.origin.x + control.size.width <= clock.origin.x,
            "the section control collides with the clock"
        );
        assert!(
            (clock_end - px(930.)).abs() < px(1.),
            "the clock must stay flush right with the 10 px inset, ended at {clock_end:?}"
        );
        assert!(
            window.find("titlebar-drag-right").bounds().size.width > px(0.),
            "the empty drag stretch must survive at 940 px"
        );
        assert!(window.find("titlebar-clock-text").visible());
    });
}

/// With saved cities the titlebar clock renders one mono run ending with the
/// first city's name.
#[gpui::test]
fn the_titlebar_clock_shows_a_saved_city(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let cities = saved_cities(&path);
    assert!(!cities.is_empty(), "seed should contain cities");

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path)));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        let clock = window.find("titlebar-clock");
        assert!(clock.visible(), "the titlebar clock did not render");
        assert!(
            clock
                .label()
                .is_some_and(|label| label.ends_with(&cities[0].1)),
            "the clock label must end with the city: {:?}",
            clock.label()
        );
        assert!(
            window.try_find("titlebar-clock-text").is_some(),
            "the combined date/time/city run is missing"
        );
    });
}

/// A click on the clock opens the city list; picking a row switches the
/// clock, closes the menu and writes the id into the settings file next to
/// the database.
#[gpui::test]
fn the_titlebar_clock_menu_switches_and_persists(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let cities = saved_cities(&path);
    assert!(cities.len() >= 2, "need two cities to switch");

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click("titlebar-clock", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.find("titlebar-clock-menu").visible());
        for (id, _) in &cities {
            assert!(
                window
                    .try_find(format!("titlebar-clock-item-{id}"))
                    .is_some(),
                "menu is missing city {id}"
            );
        }
        // Rows lead with the city and end with the `weekday day month time`
        // run (its exact text is unit-tested in `clock_menu_meta_matches_the_row_spec`).
        for (id, _) in &cities {
            let city = window.find(format!("titlebar-clock-city-{id}")).bounds();
            let meta_bounds = window.find(format!("titlebar-clock-meta-{id}")).bounds();
            assert!(
                city.left() <= meta_bounds.left(),
                "the city must lead the row: {city:?} vs {meta_bounds:?}"
            );
            assert!(
                meta_bounds.size.width > px(0.),
                "the row has no date/time run: {meta_bounds:?}"
            );
            assert!(
                meta_bounds.right() <= city.right() + px(240.),
                "the meta overflows its row: {meta_bounds:?} vs {city:?}"
            );
        }
        let menu = window.find("titlebar-clock-menu").bounds();
        assert!(
            menu.size.width + px(8.) <= px(940.),
            "the right-anchored menu does not fit the 940 px minimum: {menu:?}"
        );
    });

    let (second_id, second_city) = cities[1].clone();
    cx.update(|window, cx| window.click(format!("titlebar-clock-item-{second_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));

    let settings_path = dir.path().join("gpui-settings.conf");
    cx.update(|window, _cx| {
        assert!(
            window.try_find("titlebar-clock-menu").is_none(),
            "the menu should close after a pick"
        );
        assert!(
            window
                .find("titlebar-clock")
                .label()
                .is_some_and(|label| label.ends_with(&second_city)),
            "the clock did not switch city"
        );
    });
    let text = std::fs::read_to_string(&settings_path).expect("settings file");
    assert!(
        text.contains(&format!("titlebar_clock={second_id}")),
        "the pick was not persisted: {text}"
    );
    assert_eq!(
        Settings::load(&settings_path).titlebar_clock(),
        Some(second_id.as_str()),
        "the next boot must seed the same city"
    );
}

/// A deleted city falls back to the first saved city, silently: the stale id
/// stays in the settings file until the user picks again.
#[gpui::test]
fn the_titlebar_clock_falls_back_when_its_city_is_removed(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let cities = saved_cities(&path);
    assert!(cities.len() >= 2, "need two cities to switch");
    let (second_id, _) = cities[1].clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("titlebar-clock", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("titlebar-clock-item-{second_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));

    Store::open_path(path.clone())
        .remove_clock(&second_id)
        .expect("remove the picked city");
    cx.update(|window, cx| window.click("nav-clocks", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("nav-tasks", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        assert!(
            window
                .find("titlebar-clock")
                .label()
                .is_some_and(|label| label.ends_with(&cities[0].1)),
            "a removed city must fall back to the first saved one"
        );
    });
    let text = std::fs::read_to_string(dir.path().join("gpui-settings.conf")).expect("settings");
    assert!(
        text.contains(&format!("titlebar_clock={second_id}")),
        "the stale id is kept until the next pick: {text}"
    );
}

/// No saved cities means no titlebar clock at all.
#[gpui::test]
fn the_titlebar_clock_disappears_without_cities(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let cities = saved_cities(&path);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    let store = Store::open_path(path.clone());
    for (id, _) in &cities {
        store.remove_clock(id).expect("remove every city");
    }
    cx.update(|window, cx| window.click("nav-clocks", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("nav-tasks", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        assert!(
            window.try_find("titlebar-clock").is_none(),
            "the clock must not render without cities"
        );
    });
}

/// A long column, a rendered board, and the id of a visible card to pick up.
fn open_long_column(
    cx: &mut TestAppContext,
) -> (
    tempfile::TempDir,
    String,
    Vec<String>,
    &mut gpui::VisualTestContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let db = kandrium_core::open_db(&path).expect("open temp db");
    let board_id = kandrium_core::list_boards(&db).expect("boards").boards[0]
        .id
        .clone();
    let board = kandrium_core::get_board(&db, &board_id).expect("board");
    let todo = board.columns[0].id.clone();
    for index in 0..18 {
        kandrium_core::create_card(
            &db,
            &todo,
            &kandrium_core::CreateCardBody {
                title: format!("Long title card {index}"),
                description: None,
                due_date: None,
                labels: None,
            },
        )
        .expect("card");
    }
    drop(db);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.simulate_resize(gpui::size(px(940.), px(700.)));
    cx.update(|window, cx| window.render_frame(cx));

    let board = Store::open_path(path.clone())
        .board(&board_id)
        .expect("board");
    let cards: Vec<String> = board
        .columns
        .iter()
        .find(|column| column.id == todo)
        .unwrap()
        .ordered_cards()
        .iter()
        .map(|card| card.id.clone())
        .collect();
    assert!(cards.len() > 12, "the column must overflow the viewport");
    (dir, cards[1].clone(), cards, cx)
}

fn row_tops(window: &gpui::Window, cards: &[String]) -> Vec<f32> {
    cards
        .iter()
        .filter_map(|id| window.try_find(format!("board-card-{id}")))
        .map(|found| found.bounds().origin.y.as_f32())
        .collect()
}

fn press_and_move(
    cx: &mut gpui::VisualTestContext,
    start: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
) {
    use gpui::InputEvent;
    use gpui_kit::{Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent};

    let modifiers = Modifiers::default();
    cx.update(|window, cx| {
        window.dispatch_event(
            MouseMoveEvent {
                position: start,
                pressed_button: None,
                modifiers,
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        window.dispatch_event(
            MouseDownEvent {
                button: MouseButton::Left,
                position: start,
                modifiers,
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        window.dispatch_event(
            MouseMoveEvent {
                position: to,
                pressed_button: Some(MouseButton::Left),
                modifiers,
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    });
}

fn move_pressed(cx: &mut gpui::VisualTestContext, to: gpui::Point<gpui::Pixels>) {
    use gpui::InputEvent;
    use gpui_kit::{Modifiers, MouseButton, MouseMoveEvent};

    cx.update(|window, cx| {
        window.dispatch_event(
            MouseMoveEvent {
                position: to,
                pressed_button: Some(MouseButton::Left),
                modifiers: Modifiers::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    });
}

/// The gap must replace the dragged card's own row at pickup. It used to be
/// seeded as `previous = None`, which made the offset table treat pickup as a
/// brand-new gap and slide every card below by one row.
#[gpui::test]
fn picking_a_card_up_leaves_its_siblings_where_they_were(cx: &mut TestAppContext) {
    let (_dir, grab, cards, cx) = open_long_column(cx);
    let at_rest = cx.update(|window, _cx| row_tops(window, &cards));
    let grab_y = at_rest[1];

    let start =
        cx.update(|window, _cx| window.find(format!("board-card-{grab}")).bounds().center());
    press_and_move(cx, start, gpui::point(start.x + px(1.), start.y + px(8.)));

    let gap = cx.update(|window, _cx| {
        window
            .try_find("board-drop-gap")
            .expect("the gap is rendered while dragging")
            .bounds()
    });
    assert!(
        (gap.origin.y.as_f32() - grab_y).abs() < 1.0,
        "the gap sits in the card's own row: gap {gap:?} vs row {grab_y}"
    );

    let after = cx.update(|window, _cx| row_tops(window, &cards));
    let rest: Vec<f32> = at_rest
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 1)
        .map(|(_, y)| *y)
        .collect();
    assert_eq!(after.len(), rest.len(), "the dragged card leaves the flow");
    for (index, (y, rest)) in after.iter().zip(rest).enumerate() {
        assert!(
            (y - rest).abs() < 1.0,
            "remaining card {index} must not move at pickup: {rest} -> {y}"
        );
    }
}

/// A slow drag used to move the gap to the column's end first (the column's
/// ancestor fallback) and back to the hovered slot for every pointer move,
/// which reflowed the whole column twice per move. The gap must stay next to
/// the pointer and the board/column geometry must not move at all.
#[gpui::test]
fn a_slow_drag_moves_the_gap_one_slot_at_a_time(cx: &mut TestAppContext) {
    let (_dir, grab, cards, cx) = open_long_column(cx);
    let at_rest = cx.update(|window, _cx| row_tops(window, &cards));
    let board_before = cx.update(|window, _cx| window.find("board-columns").bounds());
    let column_id = "col_career_todo";
    let column_before =
        cx.update(|window, _cx| window.find(format!("board-column-{column_id}")).bounds());

    let start =
        cx.update(|window, _cx| window.find(format!("board-card-{grab}")).bounds().center());
    press_and_move(cx, start, gpui::point(start.x + px(1.), start.y + px(8.)));

    let mut previous_gap =
        cx.update(|window, _cx| window.find("board-drop-gap").bounds().origin.y.as_f32());
    let row = at_rest[2] - at_rest[1];
    for step in 1..=11 {
        let to = gpui::point(start.x + px(1.), start.y + px(row * (step as f32)));
        move_pressed(cx, to);

        let gap = cx.update(|window, _cx| window.find("board-drop-gap").bounds().origin.y.as_f32());
        assert!(
            gap >= previous_gap - 1.0,
            "the gap only moves down during a downward drag: {previous_gap} -> {gap}"
        );
        assert!(
            gap - previous_gap <= row * 2.0,
            "the gap moves at most one row per step: {previous_gap} -> {gap}"
        );
        assert!(
            gap < at_rest[at_rest.len() - 1],
            "the gap never detours to the column's end: gap {gap}"
        );
        previous_gap = gap;

        let board_now = cx.update(|window, _cx| window.find("board-columns").bounds());
        assert_eq!(
            board_now, board_before,
            "the board must not move during a drag"
        );
        let column_now =
            cx.update(|window, _cx| window.find(format!("board-column-{column_id}")).bounds());
        assert_eq!(
            column_now, column_before,
            "a full column is capped, so its box must not change either"
        );
    }
}

/// A view with the two shared field helpers, so their painted geometry can be
/// measured without the rest of the shell.
struct FieldProbe {
    single: Entity<InputState>,
    multi: Entity<TextareaState>,
}

impl Render for FieldProbe {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::dark();
        div()
            .w(px(240.))
            .flex()
            .flex_col()
            .child(
                div()
                    .id("field-single")
                    .test_support()
                    .w_full()
                    .child(kandrium_app::views::field_input_tall(&self.single, &t)),
            )
            .child(
                div()
                    .id("field-multi")
                    .test_support()
                    .w_full()
                    .child(kandrium_app::views::field_textarea(&self.multi, &t).h(px(120.))),
            )
    }
}

/// Every text field must paint exactly one box: the control's own fill and
/// border. A frame around it would double the edge and push the caret and the
/// selection highlight inward, so the text inset is pinned to one border plus
/// one padding step on each side.
#[gpui::test]
fn text_fields_paint_one_box_and_inset_the_text_once(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let single = cx.new(|cx| InputState::new(window, cx).default_value("Identifier"));
        let multi = cx.new(|cx| TextareaState::new(window, cx).default_value("Description"));
        FieldProbe { single, multi }
    });
    cx.update(|window, cx| window.render_frame(cx));

    let (single_box, single_text, multi_box, multi_text) = cx.update(|window, cx| {
        let probe = view.read(cx);
        (
            window.find("field-single").bounds(),
            probe.single.read(cx).input_bounds(),
            window.find("field-multi").bounds(),
            probe.multi.read(cx).input_bounds(),
        )
    });

    assert_eq!(
        single_box.size.height,
        px(34.),
        "the tall field keeps the card editor's row height"
    );
    assert_eq!(
        multi_box.size.height,
        px(120.),
        "the textarea sizes its own box"
    );

    // Single line: 1 px border + 8 px padding, and no second inset.
    assert_eq!(
        single_text.origin.x - single_box.origin.x,
        px(9.),
        "the title text must sit one border plus one padding step inside its box"
    );
    assert_eq!(
        single_box.origin.x + single_box.size.width
            - (single_text.origin.x + single_text.size.width),
        px(9.),
        "the right inset must match the left one"
    );

    // Multi line: the textarea adds its own border and editor padding
    // (10 px sideways, 8 px vertically).
    assert_eq!(
        multi_text.origin.x - multi_box.origin.x,
        px(11.),
        "the description text must not be inset by a second frame"
    );
    assert_eq!(
        multi_box.origin.x + multi_box.size.width - (multi_text.origin.x + multi_text.size.width),
        px(11.),
        "the right inset must match the left one"
    );
    assert_eq!(
        multi_text.origin.y - multi_box.origin.y,
        px(9.),
        "the description text starts one border plus one padding below the top"
    );
}

/// The title of a card as stored, read back through the same store.
fn saved_title(path: &Path, board_id: &str, card_id: &str) -> String {
    Store::open_path(path.to_path_buf())
        .board(board_id)
        .expect("board")
        .columns
        .iter()
        .flat_map(|column| column.ordered_cards())
        .find(|card| card.id == card_id)
        .expect("card")
        .title
        .clone()
}

/// Hovering a card surfaces the pencil; clicking it swaps the title for a
/// focused field with the caret at the end, and Enter writes through core.
#[gpui::test]
fn inline_title_edit_commits_with_enter(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let card = store.board(&board_id).expect("board").columns[0].ordered_cards()[0].clone();
    let original = card.title.clone();
    cx.update(gpui_kit::init);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    // The pencil only exists while the row is hovered, like the egui card.
    cx.update(|window, _cx| {
        assert!(
            window
                .try_find(format!("board-card-edit-{}", card.id))
                .is_none(),
            "the pencil must stay hidden until the card is hovered"
        );
    });
    cx.update(|window, cx| window.hover(format!("board-card-{}", card.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window
                .find(format!("board-card-edit-{}", card.id))
                .visible(),
            "hovering the card must surface its pencil"
        );
    });

    cx.update(|window, cx| window.click(format!("board-card-edit-{}", card.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window
                .find(format!("board-card-title-edit-{}", card.id))
                .visible(),
            "clicking the pencil must reveal the inline field"
        );
        assert!(
            window.try_find("card-modal").is_none(),
            "opening the inline editor must not open the modal"
        );
    });

    // The field starts focused with the caret at the end: typing appends.
    cx.update(|window, cx| {
        window.input(" (edited)", cx);
        window.press("enter", cx);
    });
    cx.update(|window, cx| window.render_frame(cx));

    assert_eq!(
        saved_title(&path, &board_id, &card.id),
        format!("{original} (edited)"),
        "Enter must persist the trimmed draft"
    );
    cx.update(|window, _cx| {
        assert!(
            window
                .try_find(format!("board-card-title-edit-{}", card.id))
                .is_none(),
            "the field must close after committing"
        );
        assert!(
            window.try_find("card-modal").is_none(),
            "committing must not open the modal"
        );
    });

    // A press outside the field also commits, like egui's blur.
    cx.update(|window, cx| window.hover(format!("board-card-{}", card.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("board-card-edit-{}", card.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.input("!", cx));
    cx.update(|window, cx| window.click(format!("board-tab-{board_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert_eq!(
        saved_title(&path, &board_id, &card.id),
        format!("{original} (edited)!"),
        "a press outside the field must commit the draft"
    );
}

/// Escape restores the original title, and a whitespace-only draft is
/// rejected instead of clearing the card.
#[gpui::test]
fn inline_title_edit_cancels_and_rejects_empty(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let card = store.board(&board_id).expect("board").columns[0].ordered_cards()[0].clone();
    let original = card.title.clone();
    cx.update(gpui_kit::init);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    let open = |cx: &mut gpui::VisualTestContext| {
        cx.update(|window, cx| window.hover(format!("board-card-{}", card.id), cx));
        cx.update(|window, cx| window.render_frame(cx));
        cx.update(|window, cx| window.click(format!("board-card-edit-{}", card.id), cx));
        cx.update(|window, cx| window.render_frame(cx));
    };

    open(cx);
    cx.update(|window, cx| {
        window.input("discarded", cx);
        window.press("escape", cx);
    });
    cx.update(|window, cx| window.render_frame(cx));
    assert_eq!(
        saved_title(&path, &board_id, &card.id),
        original,
        "Escape must leave the stored title untouched"
    );

    open(cx);
    cx.update(|window, cx| {
        window.press("cmd-a", cx);
        window.input("   ", cx);
        window.press("enter", cx);
    });
    cx.update(|window, cx| window.render_frame(cx));
    assert_eq!(
        saved_title(&path, &board_id, &card.id),
        original,
        "a whitespace-only draft must be rejected, not clear the card"
    );
    cx.update(|window, _cx| {
        assert!(
            window
                .try_find(format!("board-card-title-edit-{}", card.id))
                .is_none(),
            "the field must close after a rejected draft"
        );
    });
}

/// While the field is open, a drag started on that card neither moves it nor
/// opens the modal; the editor survives.
#[gpui::test]
fn inline_title_edit_blocks_drag_and_modal(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let board = store.board(&board_id).expect("board");
    let source = board.columns[0].id.clone();
    let target = board.columns[1].id.clone();
    let card = board.columns[0].ordered_cards()[0].clone();
    cx.update(gpui_kit::init);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.hover(format!("board-card-{}", card.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("board-card-edit-{}", card.id), cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| {
        window.drag_to(
            format!("board-card-{}", card.id),
            format!("board-column-{target}"),
            cx,
        )
    });
    cx.update(|window, cx| window.render_frame(cx));

    let moved = Store::open_path(path.clone())
        .board(&board_id)
        .expect("board")
        .columns
        .iter()
        .find(|column| column.id == target)
        .expect("target column")
        .ordered_cards()
        .iter()
        .any(|candidate| candidate.id == card.id);
    assert!(
        !moved,
        "a drag must not start from the inline editor's card"
    );
    assert!(
        Store::open_path(path.clone())
            .board(&board_id)
            .expect("board")
            .columns
            .iter()
            .find(|column| column.id == source)
            .expect("source column")
            .ordered_cards()
            .iter()
            .any(|candidate| candidate.id == card.id),
        "the card must stay in its column"
    );
    cx.update(|window, _cx| {
        assert!(
            window
                .find(format!("board-card-title-edit-{}", card.id))
                .visible(),
            "the editor must survive the attempted drag"
        );
        assert!(
            window.try_find("card-modal").is_none(),
            "the modal must not open from the editor's card"
        );
    });
}

/// The right-click menu archives and deletes through core, and cancelling the
/// delete confirmation leaves the card untouched.
#[gpui::test]
fn card_menu_archives_and_deletes(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let cards: Vec<String> = store
        .board(&board_id)
        .expect("board")
        .columns
        .iter()
        .flat_map(|column| column.ordered_cards().into_iter())
        .map(|card| card.id.clone())
        .collect();
    assert!(cards.len() >= 2, "seed needs two cards");
    let archived_card = cards[0].clone();
    let deleted_card = cards[1].clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.right_click(format!("board-card-{archived_card}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("card-menu").visible(),
            "right-click opens the menu"
        );
    });

    cx.update(|window, cx| window.click("card-menu-archive", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        Store::open_path(path.clone())
            .archived_cards()
            .expect("archived")
            .iter()
            .any(|card| card.id == archived_card),
        "Archiving must move the card into the archive"
    );
    assert!(
        !Store::open_path(path.clone())
            .board(&board_id)
            .expect("board")
            .columns
            .iter()
            .flat_map(|column| column.ordered_cards())
            .any(|card| card.id == archived_card),
        "the archived card must leave its board"
    );
    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-menu").is_none(),
            "picking an entry closes the menu"
        );
    });

    cx.update(|window, cx| window.right_click(format!("board-card-{deleted_card}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("card-menu-delete", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.find("card-confirm").visible(), "Delete asks first");
    });
    cx.update(|window, cx| window.click("card-confirm-cancel", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        Store::open_path(path.clone())
            .board(&board_id)
            .expect("board")
            .columns
            .iter()
            .flat_map(|column| column.ordered_cards())
            .any(|card| card.id == deleted_card),
        "cancelling must leave the card in place"
    );

    cx.update(|window, cx| window.right_click(format!("board-card-{deleted_card}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("card-menu-delete", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("card-confirm-delete", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        !Store::open_path(path.clone())
            .board(&board_id)
            .expect("board")
            .columns
            .iter()
            .flat_map(|column| column.ordered_cards())
            .any(|card| card.id == deleted_card),
        "confirming must delete the card"
    );
}

/// The Archived tab opens the page, lists the card, and restore returns it to
/// its board.
#[gpui::test]
fn archived_tab_lists_and_restores(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let board = store.board(&board_id).expect("board");
    let card = board
        .columns
        .iter()
        .flat_map(|column| column.ordered_cards())
        .next()
        .expect("a card")
        .clone();
    store.set_card_archived(&card.id, true).expect("archive");

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    // A board tab is the baseline: the strip and its entries must keep the
    // same geometry once Archived is selected.
    let board_row = cx.update(|window, _cx| window.find("board-tabs").bounds());
    let board_tab = cx.update(|window, _cx| window.find("board-tab-all").bounds());

    cx.update(|window, cx| window.click("board-tab-archived", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("archive-view").visible(),
            "the tab opens the page"
        );
        assert_eq!(
            window.find("board-tabs").bounds(),
            board_row,
            "the tab strip must not move when Archived is selected"
        );
        let archived_tab = window.find("board-tab-archived").bounds();
        assert_eq!(
            archived_tab.origin.y, board_tab.origin.y,
            "Archived must share the board tabs' baseline"
        );
        assert_eq!(
            archived_tab.size.height, board_tab.size.height,
            "Archived must share the board tabs' height"
        );
        assert!(
            window.find(format!("archive-row-{}", card.id)).visible(),
            "the archived card is listed"
        );
        assert!(
            window.find("archive-board-filter").visible(),
            "the board filter defaults to All Boards"
        );
    });

    cx.update(|window, cx| window.click("archive-board-filter", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("archive-board-filter-menu").visible(),
            "the filter opens its list"
        );
    });
    // The menu overlays: the table must stay where it is while it is open.
    let opened = cx.update(|window, _cx| window.find("archive-view").bounds());
    cx.update(|window, cx| window.press("escape", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("archive-board-filter-menu").is_none(),
            "Esc closes the filter menu"
        );
        assert_eq!(
            window.find("archive-view").bounds(),
            opened,
            "opening and closing the filter must not move the page"
        );
    });

    cx.update(|window, cx| window.click("archive-board-filter", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("archive-filter-all", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click(format!("archive-restore-{}", card.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        Store::open_path(path.clone())
            .archived_cards()
            .expect("archived")
            .is_empty(),
        "restore removes the card from the archive"
    );
    cx.update(|window, _cx| {
        assert!(
            window.find("archive-empty").visible(),
            "an empty archive shows its empty state"
        );
    });
}

/// Clicking the project field while its menu is open closes it again, and the
/// field stays clickable because the outside-click strips exclude it.
#[gpui::test]
fn card_modal_project_picker_toggles(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let store = Store::open_path(path.clone());
    let root = dir.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_project(&root, "Parity", None)
        .expect("project");
    let board_id = store.boards().expect("boards")[0].id.clone();
    let card_id = store.board(&board_id).expect("board").columns[0].ordered_cards()[0]
        .id
        .clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("board-card-{card_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click("card-project-picker", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.find("card-project-menu").visible());
    });

    cx.update(|window, cx| window.click("card-project-picker", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-project-menu").is_none(),
            "a second click on the field closes the menu"
        );
        assert!(window.find("card-modal").visible(), "the editor stays open");
    });

    cx.update(|window, cx| window.click("card-project-picker", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.press("escape", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-project-menu").is_none(),
            "Esc closes the project menu"
        );
    });

    let project_id = Store::open_path(path)
        .board(&board_id)
        .expect("board")
        .columns
        .iter()
        .flat_map(|column| column.ordered_cards())
        .find(|card| card.id == card_id)
        .and_then(|card| card.project_id.clone());
    assert!(
        project_id.is_none(),
        "toggling the menu must not change the selection"
    );
}

/// The Archived selection survives a restart through the settings file.
#[gpui::test]
fn archived_selection_persists(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let settings_path = path.parent().unwrap().join("gpui-settings.conf");
    std::fs::write(&settings_path, "tasks_archive=1\n").unwrap();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("archive-view").visible(),
            "the saved Archived selection is restored on boot"
        );
    });
}

/// Esc closes the menu without touching the card.
#[gpui::test]
fn card_menu_escape_closes(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let board = store.board(&board_id).expect("board");
    let card = board
        .columns
        .iter()
        .flat_map(|column| column.ordered_cards())
        .next()
        .expect("a card")
        .clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.right_click(format!("board-card-{}", card.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.press("escape", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("card-menu").is_none(),
            "Esc closes the menu"
        );
    });
}

/// Typing in the board strip's search narrows every column to title matches,
/// case-insensitively; the clear button and Esc bring the whole board back.
#[gpui::test]
fn board_search_filters_every_column(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let board = store.board(&board_id).expect("board");
    let columns: Vec<String> = board
        .columns
        .iter()
        .map(|column| column.id.clone())
        .collect();
    let keep = board.columns[0].ordered_cards()[0].id.clone();
    let hidden = board.columns[1].ordered_cards()[0].id.clone();
    let also_hidden = board.columns[2].ordered_cards()[0].id.clone();
    assert!(
        board.columns[0].ordered_cards()[0].title.contains("resume"),
        "the seed's first card is the search fixture"
    );

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    // Uppercase input still matches the lowercase title.
    cx.update(|window, cx| window.click("board-search", cx));
    cx.update(|window, cx| window.input("RESUME", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.find(format!("board-card-{keep}")).visible());
        assert!(window.try_find(format!("board-card-{hidden}")).is_none());
        assert!(
            window
                .try_find(format!("board-card-{also_hidden}"))
                .is_none()
        );
        for column in &columns[1..] {
            assert!(
                window
                    .find(format!("board-column-filtered-empty-{column}"))
                    .visible(),
                "a filtered-empty column keeps its place and shows a hint"
            );
        }
        assert!(window.find("board-search-clear").visible());
    });

    // The clear button restores the whole board.
    cx.update(|window, cx| window.click("board-search-clear", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        for card in [&keep, &hidden, &also_hidden] {
            assert!(window.find(format!("board-card-{card}")).visible());
        }
        assert!(window.try_find("board-search-clear").is_none());
    });

    // Esc while the field is focused clears it as well.
    cx.update(|window, cx| window.click("board-search", cx));
    cx.update(|window, cx| window.input("resume", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.try_find(format!("board-card-{hidden}")).is_none());
    });
    cx.update(|window, cx| window.press("escape", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        for card in [&keep, &hidden, &also_hidden] {
            assert!(window.find(format!("board-card-{card}")).visible());
        }
    });
}

/// Opening a card and closing it again leaves the query in place; Esc inside
/// the modal closes the modal instead of clearing the board search.
#[gpui::test]
fn board_search_survives_opening_a_card(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let board = store.board(&board_id).expect("board");
    let keep = board.columns[0].ordered_cards()[0].id.clone();
    let hidden = board.columns[1].ordered_cards()[0].id.clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("board-search", cx));
    cx.update(|window, cx| window.input("resume", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click(format!("board-card-{keep}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| assert!(window.find("card-modal").visible()));

    // Esc closes the modal; the query stays, so the other card is still out.
    cx.update(|window, cx| window.press("escape", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.try_find("card-modal").is_none());
        assert!(window.find(format!("board-card-{keep}")).visible());
        assert!(window.try_find(format!("board-card-{hidden}")).is_none());
        assert!(
            window.find("board-search-clear").visible(),
            "the query survives the card round trip"
        );
    });
}

/// A typed draft that cannot match the active filter says so, and the draft
/// itself is never hidden by the filter.
#[gpui::test]
fn board_search_flags_a_draft_that_cannot_match(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let board = store.board(&board_id).expect("board");
    let column = board.columns[1].id.clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("board-search", cx));
    cx.update(|window, cx| window.input("resume", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click(format!("board-add-card-{column}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.input("unrelated draft", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find(format!("board-add-form-{column}")).visible(),
            "the draft stays visible while the filter is active"
        );
        assert!(
            window
                .find(format!("board-add-filter-hint-{column}"))
                .visible(),
            "a draft that cannot match the filter is flagged"
        );
    });

    cx.update(|window, cx| window.press("cmd-a", cx));
    cx.update(|window, cx| window.input("resume follow-up", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window
                .try_find(format!("board-add-filter-hint-{column}"))
                .is_none(),
            "a matching draft drops the hint"
        );
    });
}

/// With many boards the tab cluster scrolls inside its remaining width while
/// the right group — search, divider, Archived — stays fixed, in order, and
/// fully inside the strip at the 940 px minimum window width.
#[gpui::test]
fn board_strip_keeps_the_search_and_archived_at_minimum_width(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    {
        let db = kandrium_core::open_db(&path).expect("open temp db");
        for index in 0..10 {
            kandrium_core::create_board(&db, &format!("A rather long board name {index}"))
                .expect("create board");
        }
    }

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.simulate_resize(gpui::size(px(940.), px(700.)));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        let strip = window.find("board-tabs").bounds();
        let tabs = window.find("board-tab-list").bounds();
        let search = window.find("board-search").bounds();
        let divider = window.find("board-tab-divider").bounds();
        let archived = window.find("board-tab-archived").bounds();
        assert_eq!(strip.size.height, px(40.), "the strip height is fixed");
        // The right group reads left to right: tabs | search | divider |
        // Archived, with the search anchored to the right edge.
        assert!(
            tabs.right() <= search.left(),
            "the search sits right of the tabs: tabs {tabs:?}, search {search:?}"
        );
        assert!(
            search.right() <= divider.left(),
            "the divider sits right of the search: search {search:?}, divider {divider:?}"
        );
        assert!(
            divider.right() <= archived.left(),
            "Archived sits right of the divider: divider {divider:?}, archived {archived:?}"
        );
        assert!(
            archived.right() <= strip.right(),
            "Archived must stay inside the strip: archived {archived:?}, strip {strip:?}"
        );
        assert!(
            search.left() >= strip.left() && search.right() <= strip.right(),
            "the search must stay inside the strip: search {search:?}, strip {strip:?}"
        );
        assert!(
            search.size.width >= px(199.),
            "the search keeps its width while the tabs give way: {search:?}"
        );
        assert!(
            tabs.left() >= strip.left(),
            "the tab cluster keeps the left edge: tabs {tabs:?}, strip {strip:?}"
        );
        // The group is right-aligned: only the strip's own 14 px inset is left.
        let trailing = strip.right() - archived.right();
        assert!(
            trailing >= px(0.) && trailing <= px(14.),
            "the right group is not flush right (trailing {trailing:?})"
        );
    });
}

/// Landing on Tasks must not put the caret in the strip: typing before any
/// click belongs to the board, not the search field.
#[gpui::test]
fn board_search_does_not_take_focus_on_landing(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();
    let board = store.board(&board_id).expect("board");
    let cards: Vec<String> = board
        .columns
        .iter()
        .flat_map(|column| column.ordered_cards())
        .map(|card| card.id.clone())
        .collect();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| {
        assert!(
            window.focused(cx).is_none(),
            "landing on Tasks must leave focus on the board surface"
        );
    });
    cx.update(|window, cx| window.input("zz", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("board-search-clear").is_none(),
            "typing on landing reached the search field"
        );
        for card in &cards {
            assert!(
                window.find(format!("board-card-{card}")).visible(),
                "typing on landing filtered the board"
            );
        }
    });
}

/// Focus must not survive leaving the Tasks section or crossing the Archived
/// page: coming back lands on the board, not in the search field.
#[gpui::test]
fn board_search_does_not_regain_focus_after_navigating_away(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let store = Store::open_path(path.clone());
    let board_id = store.boards().expect("boards")[0].id.clone();

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    // Focus the field the only way it can be focused: by clicking it.
    cx.update(|window, cx| window.click("board-search", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| {
        assert!(
            window.focused(cx).is_some(),
            "clicking the field must focus it"
        );
    });

    // Tasks -> Wiki -> Tasks.
    cx.update(|window, cx| window.click("nav-wiki", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("nav-tasks", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| {
        assert!(
            window.focused(cx).is_none(),
            "returning to Tasks restored the search caret"
        );
    });

    // Tasks -> Archived -> Tasks.
    cx.update(|window, cx| window.click("board-search", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("board-tab-archived", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("board-tab-{board_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| {
        assert!(
            window.focused(cx).is_none(),
            "returning from Archived restored the search caret"
        );
    });

    // Tasks -> another board tab -> back.
    let other_board = store.boards().expect("boards")[1].id.clone();
    cx.update(|window, cx| window.click("board-search", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("board-tab-{other_board}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| {
        assert!(
            window.focused(cx).is_none(),
            "switching board tabs left the caret in the search"
        );
    });
}

/// A column that outgrows the board scrolls its card list instead of running
/// past the board: the "+ Add card" row stays pinned below the scroller and the
/// scrollbar overlays the list without reserving a lane.
#[gpui::test]
fn tall_column_scrolls_and_pins_the_add_row(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);
    let db = kandrium_core::open_db(&path).expect("open temp db");
    let board_id = kandrium_core::list_boards(&db).expect("boards").boards[0]
        .id
        .clone();
    let board = kandrium_core::get_board(&db, &board_id).expect("board");
    let todo = board.columns[0].id.clone();
    for index in 0..18 {
        kandrium_core::create_card(
            &db,
            &todo,
            &kandrium_core::CreateCardBody {
                title: format!("Long title card {index}"),
                description: None,
                due_date: None,
                labels: None,
            },
        )
        .expect("card");
    }
    drop(db);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.simulate_resize(gpui::size(px(940.), px(700.)));
    cx.update(|window, cx| window.render_frame(cx));

    let first_card = Store::open_path(path.clone())
        .board(&board_id)
        .expect("board")
        .columns[0]
        .ordered_cards()[0]
        .id
        .clone();
    let last_card = Store::open_path(path.clone())
        .board(&board_id)
        .expect("board")
        .columns[0]
        .ordered_cards()
        .last()
        .expect("cards")
        .id
        .clone();

    cx.update(|window, _cx| {
        let board = window.find("board-columns").bounds();
        let column = window.find(format!("board-column-{todo}")).bounds();
        let list = window.find(format!("board-column-cards-{todo}")).bounds();
        let add = window.find(format!("board-add-card-{todo}")).bounds();
        assert!(
            column.size.height <= board.size.height + px(1.),
            "the column must stop at the board: column {column:?}, board {board:?}"
        );
        assert!(
            list.size.height < column.size.height - px(40.),
            "the card list is the scroller, not the column's full height: list {list:?}, column {column:?}"
        );
        assert!(
            add.top() >= list.bottom() - px(1.),
            "the add row sits outside the scroller: list {list:?}, add {add:?}"
        );
        assert!(
            add.bottom() <= column.bottom(),
            "the add row stays pinned inside the column: add {add:?}, column {column:?}"
        );
        // The bar overlays the list's right edge; the only inset the cards pay
        // is the list's own 8 px padding, never a permanent scrollbar lane.
        let card = window.find(format!("board-card-{first_card}")).bounds();
        let inset = list.size.width - card.size.width;
        assert!(
            inset >= px(14.) && inset <= px(18.),
            "cards must only pay the content inset, not a lane: inset {inset:?}"
        );
        // The overflowing cards continue past the list's viewport: the list is
        // the scroller, and the floating bar overlays it instead of a lane.
        let last = window.find(format!("board-card-{last_card}")).bounds();
        assert!(
            last.top() >= list.bottom(),
            "cards past the fold belong to the scrolled content: last {last:?}, list {list:?}"
        );
    });
}

/// Boots the app on the Projects section with a given tree width and checks
/// the header's right edge: the collapse chevron is the last item, inside the
/// header's padding, and the action buttons sit left of it.
fn probe_projects_header(cx: &mut TestAppContext, width: f32) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    std::fs::write(
        dir.path().join(kandrium_app::settings::FILE_NAME),
        format!("section=projects\nprojects_tree_width={width}\n"),
    )
    .unwrap();
    cx.update(gpui_kit::init);
    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        let panel = window.find("projects-tree").bounds();
        let collapse = window.find("projects-collapse").bounds();
        let actions = [
            ("new project", window.find("projects-new-project").bounds()),
            ("new folder", window.find("projects-new-folder").bounds()),
            ("new file", window.find("projects-new-file").bounds()),
            ("upload", window.find("projects-upload").bounds()),
        ];
        assert_eq!(panel.size.width, px(width), "the tree takes the saved width");
        assert!(
            collapse.right() <= panel.right(),
            "the chevron stays inside the panel: chevron {collapse:?}, panel {panel:?}"
        );
        assert!(
            panel.right() - collapse.right() <= px(8.),
            "the chevron sits on the panel's right edge: chevron {collapse:?}, panel {panel:?}"
        );
        // All four actions sit left of the chevron, in order, and none of
        // them overflows the panel at the narrowest dragged width.
        let mut previous_right = panel.left();
        for (name, bounds) in actions {
            assert!(
                bounds.left() >= previous_right && bounds.right() <= collapse.left(),
                "the {name} action stays in the header at {width} px: {bounds:?}, chevron {collapse:?}"
            );
            assert_eq!(
                bounds.size.height,
                px(19.),
                "the {name} action keeps the 19 px box"
            );
            previous_right = bounds.right();
        }
        // The open tree draws one rule: the splitter's 1 px line. The panel
        // itself paints no border, and nothing else sits beside the splitter.
        let splitter = window
            .try_find("projects-tree-splitter")
            .expect("the open tree has a splitter");
        // The 5 px splitter is centred on its 1 px rule with -2 px side
        // margins, so the rule abuts the panel even though the hit box
        // overlaps it by half of the slack.
        let rule = splitter.bounds().left() + px(2.);
        assert_eq!(
            rule,
            panel.right(),
            "the divider abuts the panel: one rule, not two"
        );
        assert_eq!(
            splitter.bounds().size.height,
            panel.size.height,
            "the divider reaches the panel's full height"
        );
    });
}

/// Collapsing to the rail swaps the splitter for the rail's own right rule, so
/// there is still exactly one divider and never two.
#[gpui::test]
fn collapsed_projects_rail_keeps_a_single_divider(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    std::fs::write(
        dir.path().join(kandrium_app::settings::FILE_NAME),
        "section=projects\nprojects_tree_width=240\n",
    )
    .unwrap();
    cx.update(gpui_kit::init);
    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click("projects-collapse", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        let rail = window.find("projects-tree-rail").bounds();
        assert!(
            window.try_find("projects-tree").is_none(),
            "the expanded panel is gone"
        );
        assert!(
            window.try_find("projects-tree-splitter").is_none(),
            "the rail has no splitter, so no second divider"
        );
        assert_eq!(
            rail.size.width,
            px(kandrium_app::views::projects::RAIL_W),
            "the rail keeps its fixed width"
        );
    });
}

/// The header holds its right-edge alignment at the narrowest width the
/// splitter allows.
#[gpui::test]
fn projects_header_puts_collapse_on_the_right_edge_at_minimum_width(cx: &mut TestAppContext) {
    probe_projects_header(cx, kandrium_app::settings::MIN_TREE_W);
}

/// …and at a wide dragged width.
#[gpui::test]
fn projects_header_puts_collapse_on_the_right_edge_at_a_wide_width(cx: &mut TestAppContext) {
    probe_projects_header(cx, 360.0);
}

/// Folder, file and upload act inside the selected project, so they are inert
/// until one is picked; new project works from an empty selection.
#[gpui::test]
fn projects_header_actions_need_a_selected_project(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    std::fs::write(
        dir.path().join(kandrium_app::settings::FILE_NAME),
        "section=projects\nprojects_tree_width=240\n",
    )
    .unwrap();
    cx.update(gpui_kit::init);
    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click("projects-new-folder", cx));
    cx.update(|window, cx| window.click("projects-new-file", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("project-doc-draft").is_none(),
            "no document draft without a selected project"
        );
        assert!(
            window.try_find("projects-draft").is_none(),
            "the disabled actions do not start anything"
        );
    });

    cx.update(|window, cx| window.click("projects-new-project", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("projects-draft").is_some(),
            "new project stays available without a selection"
        );
        assert!(window.try_find("project-doc-draft").is_none());
    });
}
