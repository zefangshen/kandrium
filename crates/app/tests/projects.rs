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
    // A second active project with its own documents, for the document-owner
    // and per-project memory checks below.
    let other_project =
        kandrium_core::create_project(&db, &projects_root, "Borealis", None, "blank")
            .expect("create second project");
    let other_dir = projects_root.join(&other_project.id);
    std::fs::write(other_dir.join("notes.md"), "# Notes\n").unwrap();
    std::fs::write(other_dir.join("plan.md"), "# Plan\n").unwrap();
    let other_notes = std::fs::canonicalize(other_dir.join("notes.md")).unwrap();
    let other_plan = std::fs::canonicalize(other_dir.join("plan.md")).unwrap();
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
    // A card that lives on a normal board but is tagged with the project. The
    // project Tasks page must show it, bucketed by its own column's status.
    let normal = kandrium_core::list_boards(&db)
        .expect("boards")
        .boards
        .into_iter()
        .find(|candidate| candidate.id != project.board_id)
        .expect("normal board");
    let normal_board =
        kandrium_core::get_board_readonly(&db, &normal.id).expect("normal board columns");
    let tagged_column = normal_board.columns[1].id.clone();
    let tagged = kandrium_core::create_card(
        &db,
        &tagged_column,
        &kandrium_core::CreateCardBody {
            title: "Tagged from a normal board".into(),
            ..Default::default()
        },
    )
    .expect("tagged card");
    kandrium_core::set_card_project(&db, &tagged.id, Some(&project.id)).expect("tag card");
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

    // The same "+ Add card" row as a normal board, and a card created there
    // inherits the project from the board.
    let todo = board.columns[0].id.clone();
    cx.update(|window, _cx| {
        assert!(
            window.find(format!("board-add-card-{todo}")).visible(),
            "the project Tasks tab needs the column add row"
        );
    });
    cx.update(|window, cx| window.click(format!("board-add-card-{todo}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find(format!("board-add-form-{todo}")).visible(),
            "the project add draft did not open"
        );
    });
    cx.update(|window, cx| {
        window.input("Earth", cx);
        window.press("enter", cx);
    });
    cx.update(|window, cx| window.render_frame(cx));
    let db = kandrium_core::open_db(&path).expect("open db");
    let board = kandrium_core::get_board_readonly(&db, &project.board_id).expect("board");
    let created = board
        .columns
        .iter()
        .flat_map(|column| column.cards.iter())
        .find(|card| card.title == "Earth")
        .expect("the add row creates a card on the project board");
    let created_id = created.id.clone();
    assert_eq!(
        created.project_id.as_deref(),
        Some(project.id.as_str()),
        "a card created on a project board inherits that project"
    );
    drop(db);
    cx.update(|window, _cx| {
        assert!(
            window.find(format!("board-card-{created_id}")).visible(),
            "the new card should appear in its column"
        );
        assert!(
            window.find(format!("board-add-card-{todo}")).visible(),
            "the add row should come back after the draft closes"
        );
    });

    // A card tagged with the project from a normal board shows here too, in
    // the column matching its own status, while staying on its own board.
    cx.update(|window, _cx| {
        assert!(
            window.find(format!("board-card-{}", tagged.id)).visible(),
            "a project-tagged card from a normal board must show on the project page"
        );
        let card = window.find(format!("board-card-{}", tagged.id)).bounds();
        let lane = window
            .find(format!("board-column-{}", board.columns[1].id))
            .bounds();
        let left = f32::from(lane.origin.x);
        assert!(
            f32::from(card.origin.x) >= left
                && f32::from(card.origin.x) < left + f32::from(lane.size.width),
            "the tagged card sits under the In Progress column"
        );
    });
    let db = kandrium_core::open_db(&path).expect("open db");
    let home = kandrium_core::get_board_readonly(&db, &normal.id).expect("normal board");
    assert!(
        home.columns.iter().any(|column| column.id == tagged_column
            && column.cards.iter().any(|card| card.id == tagged.id)),
        "the tagged card must stay on its own board"
    );
    drop(db);

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

    // Dragging the tagged card on the project page changes its status on its
    // own board instead of pulling it onto the project board.
    let done = board.columns[2].id.clone();
    cx.update(|window, cx| {
        window.drag_to(
            format!("board-card-{}", tagged.id),
            format!("board-column-{done}"),
            cx,
        );
    });
    cx.update(|window, cx| window.render_frame(cx));
    let db = kandrium_core::open_db(&path).expect("open db");
    let home = kandrium_core::get_board_readonly(&db, &normal.id).expect("normal board");
    let on_done = home.columns.iter().any(|column| {
        kandrium_core::status_from_column_title(&column.title)
            == Some(kandrium_core::CardStatus::Done)
            && column.cards.iter().any(|card| card.id == tagged.id)
    });
    assert!(
        on_done,
        "the project-page drop must move the tagged card to Done on its own board"
    );
    let project_after =
        kandrium_core::get_board_readonly(&db, &project.board_id).expect("project board");
    assert!(
        !project_after
            .columns
            .iter()
            .any(|column| column.cards.iter().any(|card| card.id == tagged.id)),
        "the tagged card must not be moved onto the project board"
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

    // --- Document ownership and per-project memory -------------------------
    // A document row belongs to the project that rendered it: expanding
    // another project's documents while a different project is selected must
    // never read the selected project's folder.
    cx.update(|window, cx| window.click(format!("project-row-{}", other_project.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("project-doc-body").is_none(),
            "switching projects does not carry the previous project's document over"
        );
    });
    cx.update(|window, cx| window.click(format!("project-docs-{}", other_project.id), cx));
    cx.update(|window, cx| window.render_frame(cx));

    // A file deleted after the tree was scanned reports a readable failure in
    // the document area, not a raw errno in the tree's banner.
    std::fs::remove_file(&other_plan).unwrap();
    let plan_row = format!(
        "project-tree-doc-{}:{}",
        other_project.id,
        other_plan.display()
    );
    cx.update(|window, _cx| assert!(window.find(plan_row.clone()).visible()));
    cx.update(|window, cx| window.click(plan_row, cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("projects-doc-error").visible(),
            "a failed document open is reported in the document area"
        );
        assert!(
            window.try_find("projects-error").is_none(),
            "the tree banner stays reserved for tree and root failures"
        );
        assert!(window.try_find("project-doc-body").is_none());
    });

    // Apollo's row, clicked while Borealis is still selected, opens Apollo's
    // file because the row carries its owning project.
    cx.update(|window, cx| window.click(format!("project-docs-{}", project.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    let brief_row = format!("project-tree-doc-{}:{}", project.id, doc_path.display());
    cx.update(|window, _cx| assert!(window.find(brief_row.clone()).visible()));
    cx.update(|window, cx| window.click(brief_row, cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("project-doc-body").visible(),
            "the row's own project owns the read, not the previous selection"
        );
        assert!(window.try_find("projects-doc-error").is_none());
    });

    // Open one of Borealis's documents from its Overview, leave, and delete the
    // file while away: coming back must not reopen it.
    cx.update(|window, cx| window.click(format!("project-row-{}", other_project.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    let notes_row = format!("project-doc-{}", other_notes.display());
    cx.update(|window, _cx| assert!(window.find(notes_row.clone()).visible()));
    cx.update(|window, cx| window.click(notes_row, cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| assert!(window.find("project-doc-body").visible()));

    cx.update(|window, cx| window.click(format!("project-row-{}", project.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    std::fs::remove_file(&other_notes).unwrap();
    cx.update(|window, cx| window.click(format!("project-row-{}", other_project.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("project-tab-2").is_none(),
            "a document deleted while the project was away is not reopened"
        );
        assert!(window.try_find("projects-doc-error").is_none());
    });

    // Returning to Apollo restores exactly the file it had open, and the tab
    // that was active when it was left.
    cx.update(|window, cx| window.click(format!("project-row-{}", project.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("project-tab-2").visible(),
            "the project's open document tab is restored"
        );
        assert!(
            window.find("project-doc-body").visible(),
            "the active document is restored too, without another click"
        );
    });

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
