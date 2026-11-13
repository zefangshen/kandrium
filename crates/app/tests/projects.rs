//! End-to-end Projects checks over a real (temp) database and project root.
//!
//! A single test owns this process-wide `KANDRIUM_PROJECTS` value, so the
//! file deliberately holds one test and no other binary is affected.

use std::path::{Path, PathBuf};

use gpui::TestAppContext;
use gpui_kit::test::TestWindowExt;

use kandrium_app::app::AppShell;
use kandrium_app::store::Store;

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
    // One archived project so the footer's disclosure has a row to show.
    let archived_project =
        kandrium_core::create_project(&db, &projects_root, "Old Idea", None, "blank")
            .expect("create archived project");
    kandrium_core::archive_project(&db, &archived_project.id).expect("archive project");
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

        // Tree: the search field sits under the header and the archived list
        // is a pinned footer row rather than a chip.
        assert!(window.find("projects-search").visible());
        assert!(window.find("projects-archived-toggle").visible());
        assert!(window.find("projects-archived-chevron").visible());

        // The disclosure expands an inline list above the footer and splits
        // the tree area with the active rows.
        assert!(
            window.find("projects-tree-rows").visible(),
            "the active tree stays visible while the disclosure is open"
        );
    });
    cx.update(|window, cx| window.click("projects-archived-chevron", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.find("projects-archived-list").visible());
        assert!(
            window
                .find(format!("project-archived-tree-{}", archived_project.id))
                .visible(),
            "the archived project appears inside the disclosure"
        );
        let active = window.find("projects-tree-rows").bounds();
        let list = window.find("projects-archived-list").bounds();
        let row = window.find("projects-archived-toggle").bounds();
        assert!(
            f32::from(row.origin.y) + f32::from(row.size.height) <= f32::from(list.origin.y) + 0.5,
            "the Archived row sits directly above its expanded list"
        );
        assert!(
            (f32::from(active.size.height) - f32::from(list.size.height)).abs() <= 2.0,
            "the archived list takes about half of the tree area"
        );
        // Archived rows line up with normal project rows at depth 0.
        let archived_label = window
            .find(format!("project-archived-name-{}", archived_project.id))
            .bounds();
        let project_label = window.find(format!("project-name-{}", project.id)).bounds();
        assert!(
            (f32::from(archived_label.origin.x) - f32::from(project_label.origin.x)).abs() < 0.5,
            "archived identifiers align with the tree's project identifiers"
        );
    });
    // The label/icon/count area is a header, not a control: clicking it must
    // not swap the tree into the old archived view.
    cx.update(|window, cx| window.click("projects-archived-row", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window
                .try_find(format!("project-archived-{}", archived_project.id))
                .is_none(),
            "clicking the Archived label does not switch views"
        );
    });
    cx.update(|window, cx| window.click("projects-archived-chevron", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("projects-archived-list").is_none(),
            "collapsing returns the whole area to the active tree"
        );
    });
    cx.update(|window, _cx| {
        // A project with no folders still shows an actionable Add folder.
        assert!(
            window.find("project-folder-add").visible(),
            "Add folder stays visible with zero folders"
        );

        // Overview: the identifier group stacks under the title, the status is
        // a segmented control and every block shares the page measure.
        let title = window.find("project-title-name").bounds();
        let identifier = window.find("project-identifier").bounds();
        assert!(
            identifier.origin.y >= title.origin.y + title.size.height,
            "the identifier group sits beneath the title"
        );
        assert!(
            window
                .find(format!("project-status-{}", project.status))
                .visible()
        );
        assert!(window.find("project-description-toggle-preview").visible());
        let description = window.find("project-description").bounds();
        let summary = window.find("project-summary").bounds();
        let documents = window.find("project-documents").bounds();
        let right =
            |bounds: gpui::Bounds<gpui::Pixels>| f32::from(bounds.origin.x + bounds.size.width);
        assert!(
            (right(description) - right(summary)).abs() < 0.5,
            "the description and the summary strip share a right edge"
        );
        assert!(
            (right(description) - right(documents)).abs() < 0.5,
            "the description and the documents list share a right edge"
        );
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

    // Dropping on the project's Tasks tab must reload the project board the
    // section is showing, not the Tasks section's own board; otherwise the
    // project cards disappear from the surface right after the drop.
    let in_progress = board.columns[1].id.clone();
    cx.update(|window, cx| {
        window.drag_to(
            format!("board-card-{}", card.id),
            format!("board-column-{in_progress}"),
            cx,
        );
    });
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find(format!("board-card-{}", card.id)).visible(),
            "the project board must stay on screen after a drop on its Tasks tab"
        );
    });
    let db = kandrium_core::open_db(&path).expect("open db");
    let moved = kandrium_core::get_board_readonly(&db, &project.board_id).expect("board");
    assert!(
        moved
            .columns
            .iter()
            .find(|column| column.id == in_progress)
            .is_some_and(|column| column.cards.iter().any(|c| c.id == card.id)),
        "the drop must move the card into the project board's second column"
    );
    drop(db);

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
    // The label is replaced by the inline field; there is no edit/save button.
    cx.update(|window, _cx| assert!(window.try_find("project-title-name").is_none()));
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
