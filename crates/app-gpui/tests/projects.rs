//! End-to-end Projects checks over a real (temp) database and project root.
//!
//! A single test owns this process-wide `KANDRIUM_PROJECTS` value, so the
//! file deliberately holds one test and no other binary is affected.

use std::path::{Path, PathBuf};

use gpui::TestAppContext;
use gpui_kit::test::TestWindowExt;

use kandrium_app_gpui::app::AppShell;
use kandrium_app_gpui::store::Store;

/// A bootstrapped database (schema + seed) in the temp directory.
fn seeded_db(dir: &Path) -> PathBuf {
    let path = dir.join("kandrium.db");
    let mut db = kandrium_core::open_db(&path).expect("open temp db");
    kandrium_core::bootstrap(&mut db, &path).expect("bootstrap temp db");
    path
}

#[gpui::test]
fn a_project_renders_its_tree_overview_tasks_and_documents(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    let projects_root = dir.path().join("projects");
    std::fs::create_dir_all(&projects_root).unwrap();
    // SAFETY: this test binary contains a single test, and the variable is set
    // before any thread reads it.
    unsafe { std::env::set_var("KANDRIUM_PROJECTS", &projects_root) };

    let db = kandrium_core::open_db(&path).expect("open db");
    let project =
        kandrium_core::create_project(&db, &projects_root, "Apollo Launch", None, "blank")
            .expect("create project");
    let board = kandrium_core::get_board_readonly(&db, &project.board_id).expect("project board");
    let column_id = board.columns[0].id.clone();
    let card = kandrium_core::create_card(
        &db,
        &column_id,
        &kandrium_core::CreateCardBody {
            title: "Moon".into(),
            ..Default::default()
        },
    )
    .expect("card");
    let doc = projects_root.join(&project.id).join("brief.md");
    std::fs::write(&doc, "# Brief\n").unwrap();
    // Read the row id from the same listing the view uses: the OS resolves
    // the temp dir's `/var` symlink to `/private/var` on macOS.
    let store = Store::open_path(path.clone());
    let files = store
        .project_files(&projects_root, &project.id)
        .expect("project files");
    let doc_path = files[0].path.clone();
    drop(store);
    drop(db);

    cx.update(gpui_kit::init);
    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click("nav-projects", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find(format!("project-row-{}", project.id)).visible(),
            "the project should appear in the tree"
        );
        assert!(
            window.find("project-overview").visible(),
            "the first project is selected and shows Overview"
        );
        assert!(window.find("project-documents").visible());
    });

    // The Tasks tab is the normal board surface, pointed at the project board.
    cx.update(|window, cx| window.click("project-tab-1", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("board-view").visible(),
            "the project Tasks tab must reuse the board surface"
        );
        assert!(window.find(format!("board-card-{}", card.id)).visible());
    });

    // Overview document rows open a document tab with the shared editor.
    cx.update(|window, cx| window.click("project-tab-0", cx));
    cx.update(|window, cx| window.render_frame(cx));
    let row = format!("project-doc-{}", doc_path.display());
    cx.update(|window, _cx| assert!(window.find(row.clone()).visible()));
    cx.update(|window, cx| window.click(row, cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.find("project-doc-body").visible());
        assert!(window.find("project-doc-editor").visible());
    });

    // Back to Overview: a double-click renames inline; Enter commits and the
    // regenerated identifier follows the name until it is locked.
    cx.update(|window, cx| window.click("project-tab-0", cx));
    cx.update(|window, cx| window.render_frame(cx));
    // Two clicks inside the double-click window; the second swaps the label
    // for the inline field (a helper-level double click would look the label
    // up again after the first click already changed the tree).
    cx.update(|window, cx| window.click("project-title-name", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("project-title-name", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| assert!(window.find("project-title-save").visible()));
    cx.update(|window, cx| {
        window.press("cmd-a", cx);
        window.input("Apollo Two", cx);
    });
    cx.update(|window, cx| window.press("enter", cx));
    cx.update(|window, cx| window.render_frame(cx));

    let db = kandrium_core::open_db(&path).expect("open db");
    let renamed = kandrium_core::projects::get_project(&db, &project.id).expect("project");
    assert_eq!(renamed.name, "Apollo Two");
    assert_eq!(
        renamed.identifier, "at",
        "an unlocked identifier is regenerated from the new name's initials"
    );
    let board = kandrium_core::get_board_readonly(&db, &project.board_id).expect("board");
    assert_eq!(board.title, "Apollo Two", "the project board title follows");
    drop(db);

    // Leaving Projects restores the Tasks section's own board: loading the
    // project board never overwrote the Tasks selection.
    cx.update(|window, cx| window.click("nav-tasks", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.find("board-view").visible());
        assert!(
            window
                .try_find(format!("board-tab-{}", project.board_id))
                .is_none(),
            "the project board must not become the Tasks section's active board"
        );
    });
}
