//! Headless tests for the merged Knowledge section: the three stacked trees,
//! their two drag seams, the half collapse flags and the shared tab row.
//!
//! The roots are process-global, so each is set exactly once for this binary
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

/// The three tree roots, kept alive for the whole binary.
struct Roots {
    _notes_tmp: tempfile::TempDir,
    notes: PathBuf,
    _wiki_tmp: tempfile::TempDir,
    wiki: PathBuf,
    _docs_tmp: tempfile::TempDir,
    docs: PathBuf,
}

fn shared_root(var: &str) -> &'static Path {
    static ROOTS: OnceLock<Roots> = OnceLock::new();
    let roots = ROOTS.get_or_init(|| {
        let temp = || {
            let dir = tempfile::tempdir().expect("temp root");
            let path = dir.path().canonicalize().expect("canonical root");
            (dir, path)
        };
        let (notes_tmp, notes) = temp();
        let (wiki_tmp, wiki) = temp();
        let (docs_tmp, docs) = temp();
        // SAFETY: runs once, before any test reads the variables, and the
        // values never change afterwards.
        unsafe {
            std::env::set_var("KANDRIUM_NOTES", &notes);
            std::env::set_var("KANDRIUM_WIKI", &wiki);
            std::env::set_var("KANDRIUM_DOCS", &docs);
        }
        Roots {
            _notes_tmp: notes_tmp,
            notes,
            _wiki_tmp: wiki_tmp,
            wiki,
            _docs_tmp: docs_tmp,
            docs,
        }
    });
    match var {
        "KANDRIUM_NOTES" => &roots.notes,
        "KANDRIUM_WIKI" => &roots.wiki,
        _ => &roots.docs,
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

fn page_row(path: &Path) -> String {
    format!("wiki-page-row-{}", path.display())
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
fn knowledge_renders_three_halves_and_the_shared_strip(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_wiki(cx);

    cx.update(|window, _cx| {
        assert!(window.find("wiki-view").visible());
        assert!(window.find("wiki-tree").visible());
        assert!(window.find("wiki-notes-tree").visible());
        assert!(window.find("wiki-pages-tree").visible());
        assert!(window.find("wiki-documents-tree").visible());
        assert!(window.find("wiki-splitter").visible());
        assert!(window.find("wiki-pages-splitter").visible());
        assert!(window.find("wiki-empty").visible(), "no tabs are open yet");
    });
}

#[gpui::test]
fn the_three_halves_start_with_equal_heights(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_wiki(cx);

    let notes = height(cx, "wiki-notes-tree");
    let pages = height(cx, "wiki-pages-tree");
    let docs = height(cx, "wiki-documents-tree");
    assert!(
        (notes - pages).abs() <= 2.0 && (pages - docs).abs() <= 2.0,
        "halves must start equal: notes {notes}, pages {pages}, docs {docs}"
    );
    assert!(notes > MIN_HALF_H);
}

#[gpui::test]
fn collapsing_a_half_hands_its_height_to_the_others(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_wiki(cx);

    // Collapsing the middle half removes both seams and splits its room
    // between the outer two.
    click(cx, "wiki-pages-collapse");
    assert!((height(cx, "wiki-pages-tree") - HEADER_H).abs() <= 1.0);
    let notes = height(cx, "wiki-notes-tree");
    let docs = height(cx, "wiki-documents-tree");
    assert!((notes - docs).abs() <= 2.0, "notes {notes}, docs {docs}");
    cx.update(|window, _cx| {
        assert!(window.find_all("wiki-splitter").is_empty());
        assert!(window.find_all("wiki-pages-splitter").is_empty());
    });

    // Expanding it brings both seams back.
    click(cx, "wiki-pages-expand");
    cx.update(|window, _cx| {
        assert!(window.find("wiki-splitter").visible());
        assert!(window.find("wiki-pages-splitter").visible());
    });

    // Collapsing the top half leaves the Wiki ↔ Sources seam in place.
    click(cx, "wiki-notes-collapse");
    assert!((height(cx, "wiki-notes-tree") - HEADER_H).abs() <= 1.0);
    cx.update(|window, _cx| {
        assert!(window.find_all("wiki-splitter").is_empty());
        assert!(window.find("wiki-pages-splitter").visible());
    });

    click(cx, "wiki-documents-collapse");
    assert!((height(cx, "wiki-documents-tree") - HEADER_H).abs() <= 1.0);
    cx.update(|window, _cx| {
        assert!(window.find_all("wiki-pages-splitter").is_empty());
    });
}

#[gpui::test]
fn all_three_halves_can_collapse_to_their_headers(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_wiki(cx);

    click(cx, "wiki-notes-collapse");
    click(cx, "wiki-pages-collapse");
    click(cx, "wiki-documents-collapse");
    assert!((height(cx, "wiki-notes-tree") - HEADER_H).abs() <= 1.0);
    assert!((height(cx, "wiki-pages-tree") - HEADER_H).abs() <= 1.0);
    assert!((height(cx, "wiki-documents-tree") - HEADER_H).abs() <= 1.0);
    assert!(
        height(cx, "wiki-view") > HEADER_H * 3.0,
        "the section stays full height"
    );
}

#[gpui::test]
fn a_wiki_page_opens_in_the_shared_note_tab(cx: &mut TestAppContext) {
    let wiki_root = shared_root("KANDRIUM_WIKI");
    let page = wiki_root.join("wiki-page-open.md");
    fs::write(&page, "# page\n").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_wiki(cx);

    click(cx, page_row(&page));
    cx.update(|window, _cx| {
        assert!(
            window.find(tab_id("note", &page)).visible(),
            "pages use the note tab kind"
        );
        assert!(window.find("notes-head").visible());
    });
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
