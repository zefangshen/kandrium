//! Headless tests for the merged Wiki section: the vertical tree split, the
//! half collapse flags and the shared tab row.
//!
//! Both roots are process-global, so each is set exactly once for this binary
//! and files live directly at the root of each.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use gpui::{Entity, TestAppContext, VisualTestContext};
use gpui_kit::test::TestWindowExt;

use kandrium_app::app::AppShell;
use kandrium_app::store::Store;
use kandrium_app::views::wiki::{HEADER_H, MIN_HALF_H};

/// A bootstrapped database (schema + seed) in the temp directory.
fn seeded_db(dir: &Path) -> PathBuf {
    let path = dir.join("kandrium.db");
    let mut db = kandrium_core::open_db(&path).expect("open temp db");
    kandrium_core::bootstrap(&mut db, &path).expect("bootstrap temp db");
    path
}

fn shared_root(var: &str) -> &'static Path {
    static ROOTS: OnceLock<(tempfile::TempDir, PathBuf, tempfile::TempDir, PathBuf)> =
        OnceLock::new();
    let (_, notes, _, docs) = ROOTS.get_or_init(|| {
        let notes_tmp = tempfile::tempdir().expect("notes root");
        let notes = notes_tmp
            .path()
            .canonicalize()
            .expect("canonical notes root");
        let docs_tmp = tempfile::tempdir().expect("documents root");
        let docs = docs_tmp.path().canonicalize().expect("canonical docs root");
        // SAFETY: runs once, before any test reads the variables, and the
        // values never change afterwards.
        unsafe {
            std::env::set_var("KANDRIUM_NOTES", &notes);
            std::env::set_var("KANDRIUM_DOCS", &docs);
        }
        (notes_tmp, notes, docs_tmp, docs)
    });
    match var {
        "KANDRIUM_NOTES" => notes,
        _ => docs,
    }
}

fn boot(cx: &mut TestAppContext, db: PathBuf) -> (Entity<AppShell>, &mut VisualTestContext) {
    cx.update(gpui_kit::init);
    cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(db)))
}

fn open_wiki(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.click("nav-wiki", cx));
    cx.update(|window, cx| window.render_frame(cx));
}

fn note_row(path: &Path) -> String {
    format!("note-row-{}", path.display())
}

fn doc_row(path: &Path) -> String {
    format!("doc-row-{}", path.display())
}

fn tab_id(kind: &str, path: &Path) -> String {
    format!("wiki-tab-{kind}-{}", path.display())
}

fn height(cx: &mut VisualTestContext, id: &'static str) -> f32 {
    cx.update(|window, _cx| f32::from(window.find(id).bounds().size.height))
}

fn click(cx: &mut VisualTestContext, id: impl Into<String>) {
    let id = id.into();
    cx.update(|window, cx| window.click(id, cx));
    cx.update(|window, cx| window.render_frame(cx));
}

#[gpui::test]
fn wiki_renders_both_halves_and_the_shared_strip(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_wiki(cx);

    cx.update(|window, _cx| {
        assert!(window.find("wiki-view").visible());
        assert!(window.find("wiki-tree").visible());
        assert!(window.find("wiki-notes-tree").visible());
        assert!(window.find("wiki-documents-tree").visible());
        assert!(window.find("wiki-splitter").visible());
        assert!(window.find("wiki-empty").visible(), "no tabs are open yet");
    });
}

#[gpui::test]
fn the_split_starts_half_and_half(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_wiki(cx);

    let notes = height(cx, "wiki-notes-tree");
    let docs = height(cx, "wiki-documents-tree");
    assert!(
        (notes - docs).abs() <= 2.0,
        "halves must start equal: notes {notes}, documents {docs}"
    );
    assert!(notes > MIN_HALF_H);
}

#[gpui::test]
fn collapsing_one_half_gives_the_other_the_height(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_wiki(cx);

    let total = height(cx, "wiki-notes-tree") + height(cx, "wiki-documents-tree");
    click(cx, "wiki-notes-collapse");
    let notes = height(cx, "wiki-notes-tree");
    let docs = height(cx, "wiki-documents-tree");
    assert!(
        (notes - HEADER_H).abs() <= 1.0,
        "notes header only: {notes}"
    );
    assert!(
        docs > total - HEADER_H - 2.0,
        "documents must take the rest"
    );
    cx.update(|window, _cx| {
        assert!(
            window.find_all("wiki-splitter").is_empty(),
            "the splitter is meaningless with one half collapsed"
        );
    });

    click(cx, "wiki-notes-expand");
    let restored = height(cx, "wiki-notes-tree");
    assert!(
        (restored - (total - HEADER_H)).abs() > 1.0,
        "expanding restores a proportional half, not the header"
    );

    click(cx, "wiki-documents-collapse");
    let docs = height(cx, "wiki-documents-tree");
    assert!(
        (docs - HEADER_H).abs() <= 1.0,
        "documents header only: {docs}"
    );
}

#[gpui::test]
fn both_halves_can_collapse_to_their_headers(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_wiki(cx);

    click(cx, "wiki-notes-collapse");
    click(cx, "wiki-documents-collapse");
    assert!((height(cx, "wiki-notes-tree") - HEADER_H).abs() <= 1.0);
    assert!((height(cx, "wiki-documents-tree") - HEADER_H).abs() <= 1.0);
    assert!(
        height(cx, "wiki-view") > HEADER_H * 2.0,
        "the section stays full height"
    );
}

#[gpui::test]
fn tabs_from_both_halves_share_one_row_in_open_order(cx: &mut TestAppContext) {
    let notes_root = shared_root("KANDRIUM_NOTES");
    let docs_root = shared_root("KANDRIUM_DOCS");
    let note = notes_root.join("wiki-order-note.md");
    fs::write(&note, "# note\n").unwrap();
    let doc = docs_root.join("wiki-order-doc.md");
    fs::write(&doc, "# doc\n").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_wiki(cx);

    click(cx, note_row(&note));
    cx.update(|window, _cx| {
        assert!(window.find(tab_id("note", &note)).visible());
        assert!(window.find("notes-head").visible());
    });

    click(cx, doc_row(&doc));
    cx.update(|window, _cx| {
        assert!(window.find(tab_id("doc", &doc)).visible());
        assert!(
            window.find("documents-head").visible(),
            "detail follows the document"
        );
        let note_x = window.find(tab_id("note", &note)).bounds().origin.x;
        let doc_x = window.find(tab_id("doc", &doc)).bounds().origin.x;
        assert!(
            note_x < doc_x,
            "tabs must keep true open order across kinds"
        );
    });

    // Re-opening an already-open note must activate it, not duplicate it.
    click(cx, note_row(&note));
    cx.update(|window, _cx| {
        let tabs = window
            .find_all(tab_id("note", &note))
            .into_iter()
            .filter(|tab| tab.visible())
            .count();
        assert_eq!(tabs, 1, "re-opening reuses the existing tab");
        assert!(window.find("notes-head").visible());
    });
}

#[gpui::test]
fn toolbar_opens_status_lint_and_setup_panels(cx: &mut TestAppContext) {
    let notes_root = shared_root("KANDRIUM_NOTES");
    let note = notes_root.join("wiki-panel-note.md");
    fs::write(&note, "# panel note\n").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_wiki(cx);

    cx.update(|window, _cx| {
        assert!(window.find("wiki-toolbar").visible());
    });

    click(cx, "wiki-status");
    cx.update(|window, _cx| {
        assert!(window.find("wiki-panel-status").visible());
    });

    click(cx, "wiki-refresh");
    cx.update(|window, _cx| {
        assert!(window.find("wiki-panel-status").visible());
    });

    click(cx, "wiki-lint");
    cx.update(|window, _cx| {
        assert!(window.find("wiki-panel-lint").visible());
    });

    click(cx, "wiki-setup");
    cx.update(|window, _cx| {
        assert!(window.find("wiki-panel-setup").visible());
        assert!(
            window.find("wiki-setup-instructions").visible(),
            "the setup panel renders the embedded instructions"
        );
    });

    // Opening a tab leaves the panel and shows the editor again.
    click(cx, note_row(&note));
    cx.update(|window, _cx| {
        assert!(window.find_all("wiki-panel-setup").is_empty());
        assert!(window.find("notes-head").visible());
    });
}

#[gpui::test]
fn closing_a_merged_tab_keeps_the_other_kind_intact(cx: &mut TestAppContext) {
    let notes_root = shared_root("KANDRIUM_NOTES");
    let docs_root = shared_root("KANDRIUM_DOCS");
    let note = notes_root.join("wiki-close-note.md");
    fs::write(&note, "# note\n").unwrap();
    let doc = docs_root.join("wiki-close-doc.md");
    fs::write(&doc, "# doc\n").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_wiki(cx);

    click(cx, note_row(&note));
    click(cx, doc_row(&doc));

    click(cx, format!("wiki-tab-close-note-{}", note.display()));
    cx.update(|window, _cx| {
        assert!(
            window.find_all(tab_id("note", &note)).is_empty(),
            "the closed note tab must be gone"
        );
        assert!(window.find(tab_id("doc", &doc)).visible());
        assert!(window.find("documents-head").visible());
    });
}
