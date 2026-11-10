//! Headless window tests for the Stage 1 shell.
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
