//! Notes flows through `kandrium-core` plus a headless render check.
//!
//! The shell keeps no filesystem logic of its own: every create, read, write,
//! rename and delete below is the same `kandrium_core::notes` call the view
//! makes. The render test covers the tree header and the empty content state.

use std::fs;
use std::path::{Path, PathBuf};

use gpui::TestAppContext;
use gpui_kit::test::TestWindowExt;

use kandrium_app::app::AppShell;
use kandrium_app::store::Store;

fn notes_root(dir: &Path) -> PathBuf {
    let root = dir.join("notes");
    fs::create_dir_all(&root).expect("notes root");
    root
}

/// A bootstrapped database (schema + seed) in the temp directory.
fn seeded_db(dir: &Path) -> PathBuf {
    let path = dir.join("kandrium.db");
    let mut db = kandrium_core::open_db(&path).expect("open temp db");
    kandrium_core::bootstrap(&mut db, &path).expect("bootstrap temp db");
    path
}

/// Create → read → write → rename → delete, the way the shell drives core.
#[test]
fn notes_round_trip_through_core() {
    let dir = tempfile::tempdir().unwrap();
    let root = notes_root(dir.path());
    let notes = kandrium_core::notes::scan_tree(&root).expect("scan");
    assert!(notes.is_empty());

    let folder =
        kandrium_core::notes::create_named_folder(&root, &root, "Work").expect("create folder");
    let note =
        kandrium_core::notes::create_named_note(&root, &folder, "Standup").expect("create note");
    assert!(note.ends_with("Standup.md"));
    assert_eq!(kandrium_core::notes::read_note(&note, &root).unwrap(), "");

    kandrium_core::notes::write_note(&note, &root, "# Standup\n").expect("write");
    assert_eq!(
        kandrium_core::notes::read_note(&note, &root).unwrap(),
        "# Standup\n"
    );

    let renamed = kandrium_core::notes::rename_path(&note, &root, "Daily.md").expect("rename");
    assert!(renamed.ends_with("Daily.md"));
    assert!(!note.exists());
    assert_eq!(
        kandrium_core::notes::read_note(&renamed, &root).unwrap(),
        "# Standup\n"
    );

    let tree = kandrium_core::notes::scan_tree(&root).expect("scan");
    assert_eq!(tree.len(), 1);
    assert!(tree[0].is_dir);
    assert_eq!(tree[0].children.as_ref().unwrap().len(), 1);

    kandrium_core::notes::delete_path(&renamed, &root).expect("delete file");
    assert!(!renamed.exists());
    kandrium_core::notes::delete_path(&folder, &root).expect("delete folder");
    assert!(!folder.exists());
    assert!(kandrium_core::notes::scan_tree(&root).unwrap().is_empty());
}

/// The root guard and name rules are what the inline name field surfaces.
#[test]
fn notes_reject_escapes_and_collisions() {
    let dir = tempfile::tempdir().unwrap();
    let root = notes_root(dir.path());
    kandrium_core::notes::create_named_note(&root, &root, "Todo").expect("create");

    let collision =
        kandrium_core::notes::create_named_note(&root, &root, "Todo").expect_err("must collide");
    assert!(
        collision.to_string().contains("already exists"),
        "unexpected message: {collision}"
    );
    assert!(kandrium_core::notes::create_named_note(&root, &root, "../escape").is_err());
    assert!(kandrium_core::notes::create_named_folder(&root, &root, "..").is_err());
    assert!(kandrium_core::notes::delete_path(&root, &root).is_err());
}

/// The Wiki section renders the notes tree and the empty content state.
#[gpui::test]
fn notes_view_renders_the_tree_and_empty_content(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded_db(dir.path());
    cx.update(gpui_kit::init);

    let (_view, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path)));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("nav-wiki", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, _cx| {
        assert!(window.find("wiki-view").visible());
        assert!(window.find("wiki-notes-tree").visible());
        assert!(window.find("notes-new-note").visible());
        assert!(window.find("notes-new-folder").visible());
        assert!(window.find("notes-collapse").visible());
        assert!(window.find("wiki-empty").visible());
    });
}
