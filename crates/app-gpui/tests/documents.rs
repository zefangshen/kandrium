//! Headless tests for the Documents section: the tree, tabs, Markdown
//! autosave/vim, the PDF annotation sidecar and the tab session file.
//!
//! Every case works inside its own subfolder of one shared documents root.
//! `KANDRIUM_DOCS` is process-global, so the root is set exactly once for the
//! whole test binary and never mutated afterwards.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use gpui::{Entity, TestAppContext, VisualTestContext};
use gpui_kit::test::TestWindowExt;

use kandrium_app_gpui::app::AppShell;
use kandrium_app_gpui::store::Store;
use kandrium_app_gpui::views::documents as docs_view;
use kandrium_core::documents::{AnnotationDoc, AnnotationKind, new_annotation};

/// A bootstrapped database (schema + seed) in the temp directory.
fn seeded_db(dir: &Path) -> PathBuf {
    let path = dir.join("kandrium.db");
    let mut db = kandrium_core::open_db(&path).expect("open temp db");
    kandrium_core::bootstrap(&mut db, &path).expect("bootstrap temp db");
    path
}

/// One documents root for the whole binary (see the module comment).
fn shared_root() -> &'static Path {
    static ROOT: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    let (_, root) = ROOT.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("documents root");
        let root = tmp.path().canonicalize().expect("canonical documents root");
        // SAFETY: runs once, before any test reads the variable, and the
        // value never changes afterwards.
        unsafe { std::env::set_var("KANDRIUM_DOCS", &root) };
        (tmp, root)
    });
    root
}

/// A fresh, empty folder for one case.
fn case_dir(name: &str) -> PathBuf {
    let dir = shared_root().join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("case dir");
    dir
}

fn boot(cx: &mut TestAppContext, db: PathBuf) -> (Entity<AppShell>, &mut VisualTestContext) {
    cx.update(gpui_kit::init);
    cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(db)))
}

fn open_documents(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.click("nav-wiki", cx));
    cx.update(|window, cx| window.render_frame(cx));
}

/// Expand one folder row in the tree (every case lives in its own folder under
/// the shared root, so tests must open that folder before its files render).
fn expand(cx: &mut VisualTestContext, folder: &Path) {
    cx.update(|window, cx| window.click(row_id(folder), cx));
    cx.update(|window, cx| window.render_frame(cx));
}

fn row_id(path: &Path) -> String {
    format!("doc-row-{}", path.display())
}

fn tab_id(path: &Path) -> String {
    format!("wiki-tab-doc-{}", path.display())
}

/// One-page PDF with a single text run, enough for the worker to render.
fn minimal_pdf(text: &str) -> Vec<u8> {
    let content = format!("BT /F1 24 Tf 72 700 Td ({text}) Tj ET\n");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = Vec::from(&b"%PDF-1.4\n"[..]);
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, body).as_bytes());
    }
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            xref_at
        )
        .as_bytes(),
    );
    out
}

fn quad(x: f32, y: f32, w: f32, h: f32) -> [[f32; 2]; 4] {
    [[x, y], [x + w, y], [x + w, y + h], [x, y + h]]
}

#[gpui::test]
fn selects_the_documents_section_and_lists_the_tree(cx: &mut TestAppContext) {
    let root = case_dir("tree");
    let folder = root.join("Zed");
    fs::create_dir_all(&folder).unwrap();
    let pdf = root.join("a.pdf");
    fs::write(&pdf, minimal_pdf("tree")).unwrap();
    let md = root.join("b.md");
    fs::write(&md, "# b\n").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_documents(cx);
    expand(cx, &root);

    cx.update(|window, _cx| {
        assert!(window.find("wiki-view").visible());
        assert!(window.find("wiki-documents-tree").visible());
        let folder_row = window.find(row_id(&folder));
        let pdf_row = window.find(row_id(&pdf));
        let md_row = window.find(row_id(&md));
        assert!(folder_row.visible(), "folder row missing");
        assert!(pdf_row.visible(), "pdf row missing");
        assert!(md_row.visible(), "markdown row missing");
        assert!(
            folder_row.bounds().origin.y < pdf_row.bounds().origin.y,
            "folders must render before files"
        );
        assert!(
            pdf_row.bounds().origin.y < md_row.bounds().origin.y,
            "files are sorted by name"
        );
    });
}

#[gpui::test]
fn creates_a_markdown_file_and_opens_its_tab(cx: &mut TestAppContext) {
    let root = case_dir("create");
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_documents(cx);
    // Selecting the case folder makes it the target for the new file.
    expand(cx, &root);

    cx.update(|window, cx| window.click("documents-new-file", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| assert!(window.find("documents-draft").visible()));
    cx.update(|window, cx| window.input("Plan", cx));
    cx.update(|window, cx| window.press("enter", cx));

    let created = root.join("Plan.md");
    assert!(created.exists(), "bare names gain .md");
    assert_eq!(fs::read_to_string(&created).unwrap(), "");
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find(tab_id(&created)).visible(),
            "the new file should open in a tab"
        );
        assert!(window.find("doc-editor").visible());
        // The editor status bar closes the pane and keeps a fixed height.
        let status = window.find("doc-status").bounds();
        let pane = window.find("doc-markdown").bounds();
        assert!(
            (f32::from(status.size.height) - 26.0).abs() < 0.5,
            "the status bar is a 26 px row"
        );
        assert!(
            f32::from(status.origin.y) + f32::from(status.size.height)
                - (f32::from(pane.origin.y) + f32::from(pane.size.height))
                < 0.5,
            "the status bar sits on the pane's bottom edge: status {:?} pane {:?}",
            status,
            pane
        );
        // The editing body fills the pane: no measure cap, no dead band.
        let area = window.find("doc-editor").bounds();
        assert!(
            (f32::from(area.size.width) - f32::from(pane.size.width)).abs() < 0.5,
            "the editor spans the pane width: area {:?} pane {:?}",
            area,
            pane
        );
        assert!(
            (f32::from(area.size.height) - (f32::from(pane.size.height) - 26.0)).abs() < 1.0,
            "the editor fills the height above the status bar: area {:?} pane {:?}",
            area,
            pane
        );
    });
}

#[gpui::test]
fn imports_copy_with_the_unique_name_rule_and_report_partial_failures(_cx: &mut TestAppContext) {
    let root = case_dir("import");
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let store = Store::open_path(db);

    let source_dir = tempfile::tempdir().unwrap();
    let source = source_dir.path().join("Report.pdf");
    fs::write(&source, minimal_pdf("upload")).unwrap();

    let first = store
        .import_documents(std::slice::from_ref(&source), &root, &root)
        .expect("first import");
    assert_eq!(first.len(), 1);
    assert!(first[0].1.is_none());
    assert_eq!(
        first[0].0.file_name().and_then(|name| name.to_str()),
        Some("Report.pdf")
    );

    let second = store
        .import_documents(std::slice::from_ref(&source), &root, &root)
        .expect("second import");
    assert!(second[0].1.is_none());
    assert_eq!(
        second[0].0.file_name().and_then(|name| name.to_str()),
        Some("Report-2.pdf")
    );
    // No sidecars are invented for imported PDFs.
    assert!(!root.join("Report.pdf.kandrium.json").exists());

    let missing = source_dir.path().join("gone.md");
    let partial = store
        .import_documents(&[source, missing], &root, &root)
        .expect("partial import");
    assert!(partial[0].1.is_none());
    assert!(
        partial[1].1.is_some(),
        "a failed source is reported without aborting the batch"
    );
}

#[gpui::test]
fn renames_a_pdf_and_its_sidecar_follows(cx: &mut TestAppContext) {
    let root = case_dir("rename");
    let pdf = root.join("old.pdf");
    fs::write(&pdf, minimal_pdf("rename")).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let store = Store::open_path(db.clone());
    let mut doc = AnnotationDoc::default();
    doc.annotations.push(new_annotation(
        AnnotationKind::Highlight,
        0,
        vec![quad(10.0, 10.0, 40.0, 12.0)],
        "#f6d448",
    ));
    store
        .save_annotations(&pdf, &root, &doc)
        .expect("seed sidecar");
    assert!(root.join("old.pdf.kandrium.json").exists());

    let (_view, cx) = boot(cx, db);
    open_documents(cx);
    expand(cx, &root);
    cx.update(|window, cx| window.click(row_id(&pdf), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| assert!(window.find(tab_id(&pdf)).visible()));

    cx.update(|window, cx| window.click(format!("doc-file-rename-{}", pdf.display()), cx));
    cx.update(|window, cx| window.input("new.pdf", cx));
    cx.update(|window, cx| window.press("enter", cx));
    cx.update(|window, cx| window.render_frame(cx));

    let renamed = root.join("new.pdf");
    assert!(renamed.exists());
    assert!(!pdf.exists());
    assert!(root.join("new.pdf.kandrium.json").exists());
    assert!(!root.join("old.pdf.kandrium.json").exists());
    // The open tab follows the rename.
    cx.update(|window, _cx| {
        assert!(window.find(tab_id(&renamed)).visible());
        assert!(window.try_find(tab_id(&pdf)).is_none());
    });
    let loaded = store
        .load_annotations(&renamed, &root)
        .expect("annotations");
    assert_eq!(loaded.annotations.len(), 1);
}

#[gpui::test]
fn deleting_a_document_drops_its_tabs_and_sidecar(cx: &mut TestAppContext) {
    let root = case_dir("delete");
    let pdf = root.join("gone.pdf");
    fs::write(&pdf, minimal_pdf("delete")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let store = Store::open_path(db.clone());
    let mut doc = AnnotationDoc::default();
    doc.annotations.push(new_annotation(
        AnnotationKind::Note,
        0,
        vec![quad(30.0, 30.0, 20.0, 20.0)],
        "#f6d448",
    ));
    store.save_annotations(&pdf, &root, &doc).expect("seed");

    let (_view, cx) = boot(cx, db);
    open_documents(cx);
    expand(cx, &root);
    cx.update(|window, cx| window.click(row_id(&pdf), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| assert!(window.find(tab_id(&pdf)).visible()));

    cx.update(|window, cx| window.click(format!("doc-file-delete-{}", pdf.display()), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| assert!(window.find("documents-delete-confirm").visible()));
    cx.update(|window, cx| window.click("documents-delete-confirm-yes", cx));
    cx.update(|window, cx| window.render_frame(cx));

    assert!(!pdf.exists());
    assert!(!root.join("gone.pdf.kandrium.json").exists());
    cx.update(|window, _cx| {
        assert!(
            window.try_find(tab_id(&pdf)).is_none(),
            "the deleted document's tab must go away"
        );
    });
}

#[gpui::test]
fn markdown_autosave_debounces_and_writes_once(cx: &mut TestAppContext) {
    let root = case_dir("autosave");
    let md = root.join("notes.md");
    fs::write(&md, "").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_documents(cx);
    expand(cx, &root);

    cx.update(|window, cx| window.click(row_id(&md), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.press("i", cx));
    cx.update(|window, cx| window.input("autosaved", cx));
    cx.update(|window, cx| window.press("escape", cx));

    // Still inside the debounce window: nothing on disk yet.
    assert_eq!(fs::read_to_string(&md).unwrap(), "");

    cx.background_executor
        .advance_clock(docs_view::AUTOSAVE + Duration::from_millis(50));
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&md).unwrap(), "autosaved");
    cx.update(|window, cx| window.render_frame(cx));
}

#[gpui::test]
fn vim_ex_close_saves_and_closes(cx: &mut TestAppContext) {
    let root = case_dir("vim");
    let md = root.join("vim.md");
    fs::write(&md, "").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_documents(cx);
    expand(cx, &root);

    cx.update(|window, cx| window.click(row_id(&md), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.press("i", cx));
    cx.update(|window, cx| window.input("vim saved", cx));
    cx.update(|window, cx| window.press("escape", cx));
    for key in [":", "x", "enter"] {
        cx.update(|window, cx| window.press(key, cx));
    }

    assert_eq!(fs::read_to_string(&md).unwrap(), "vim saved");
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find(tab_id(&md)).is_none(),
            ":x must close the tab"
        );
    });
}

#[gpui::test]
fn annotation_round_trip_and_empty_document_removes_the_sidecar(cx: &mut TestAppContext) {
    let root = case_dir("annotations");
    let pdf = root.join("annotated.pdf");
    fs::write(&pdf, minimal_pdf("annotate me")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let store = Store::open_path(db.clone());

    let mut doc = AnnotationDoc::default();
    let note = new_annotation(
        AnnotationKind::Note,
        0,
        vec![quad(72.0, 72.0, 20.0, 20.0)],
        "#f6d448",
    );
    let note_id = note.id.clone();
    doc.annotations.push(note);
    store.save_annotations(&pdf, &root, &doc).expect("seed");

    let (_view, cx) = boot(cx, db);
    open_documents(cx);
    expand(cx, &root);
    cx.update(|window, cx| window.click(row_id(&pdf), cx));

    // The worker renders off-thread; drive frames until the overlay exists.
    let mut found = false;
    for _ in 0..200 {
        cx.update(|window, cx| window.render_frame(cx));
        let hit = cx.update(|window, _cx| {
            window
                .try_find(format!("doc-ann-{note_id}"))
                .is_some_and(|snapshot| snapshot.visible())
        });
        if hit {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        found,
        "the note overlay should appear once the page renders"
    );

    cx.update(|window, cx| window.click(format!("doc-ann-{note_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| assert!(window.find("doc-annotation-popover").visible()));
    cx.update(|window, cx| window.input("memo", cx));
    cx.background_executor
        .advance_clock(docs_view::ANNOTATION_AUTOSAVE + Duration::from_millis(50));
    cx.run_until_parked();
    cx.update(|window, cx| window.render_frame(cx));
    let saved = store.load_annotations(&pdf, &root).expect("saved");
    assert_eq!(saved.annotations[0].text, "memo");

    cx.update(|window, cx| window.click("doc-annotation-delete", cx));
    cx.background_executor
        .advance_clock(docs_view::ANNOTATION_AUTOSAVE + Duration::from_millis(50));
    cx.run_until_parked();
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        !root.join("annotated.pdf.kandrium.json").exists(),
        "an empty annotation document removes the sidecar"
    );
}

#[gpui::test]
fn session_round_trip_is_root_relative(cx: &mut TestAppContext) {
    let root = case_dir("session");
    let docs = root.join("sub");
    fs::create_dir_all(&docs).unwrap();
    let md = docs.join("session.md");
    fs::write(&md, "hello").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_first, cx) = boot(cx, db.clone());
    open_documents(cx);
    expand(cx, &root);
    expand(cx, &docs);
    cx.update(|window, cx| window.click(row_id(&md), cx));
    cx.update(|window, cx| window.render_frame(cx));

    let session = db
        .parent()
        .expect("db parent")
        .join("gpui-documents.session");
    let text = fs::read_to_string(&session).expect("session written");
    assert!(
        text.contains("session/sub/session.md") || text.contains("sub/session.md"),
        "session stores root-relative paths, got: {text}"
    );
    assert!(!text.contains(&root.display().to_string()));

    // A missing file is dropped instead of restored as a broken tab.
    fs::remove_file(&md).unwrap();
    let second = tempfile::tempdir().unwrap();
    let db2 = seeded_db(second.path());
    fs::copy(&session, second.path().join("gpui-documents.session")).unwrap();
    let (_second, cx) = cx
        .cx
        .add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(db2)));
    open_documents(cx);
    cx.update(|window, _cx| {
        assert!(
            window.try_find(tab_id(&md)).is_none(),
            "missing files must not restore"
        );
    });
}

#[gpui::test]
fn tree_collapses_to_a_rail_and_expands_back(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded_db(dir.path());
    let (_view, cx) = boot(cx, db);
    open_documents(cx);

    cx.update(|window, cx| window.click("documents-collapse", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(window.find("wiki-tree-rail").visible());
        assert!(window.try_find("wiki-tree").is_none());
    });

    cx.update(|window, cx| window.click("wiki-expand", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| assert!(window.find("wiki-tree").visible()));
}
