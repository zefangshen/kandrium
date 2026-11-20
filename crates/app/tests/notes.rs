//! Notes flows through `kandrium-core`.
//!
//! The shell keeps no filesystem logic of its own: every create, read, write,
//! rename and delete below is the same `kandrium_core::notes` call the wiki
//! page editor makes. The Knowledge tree no longer lists the notes root, but
//! the files and these core operations are unchanged.

use std::fs;
use std::path::{Path, PathBuf};

fn notes_root(dir: &Path) -> PathBuf {
    let root = dir.join("notes");
    fs::create_dir_all(&root).expect("notes root");
    root
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

/// The root guard and name rules are what core enforces for both roots.
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
