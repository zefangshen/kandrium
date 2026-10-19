//! Documents file-system layer — PDF files under a per-user root, with
//! per-file annotation sidecars.
//!
//! PDFs live outside the SQLite database: `<data_dir>/kandrium/documents/`
//! (override with `KANDRIUM_DOCS`). Highlights and sticky notes are stored
//! as a sidecar JSON file next to each PDF (`<name>.pdf.kandrium.json`) so
//! the PDF itself is never modified and read-only files work. All functions
//! are synchronous and validate that every touched path stays inside the
//! root (no `..` traversal, no symlink escapes); the shared plumbing lives
//! in [`crate::fsutil`].

use std::fs;
use std::path::{Path, PathBuf};

use crate::db::uid;
use crate::error::{KandriumError, Result};
use crate::fsutil::{self, FsEntry};

/// Sidecar suffix appended to a PDF's file name.
const SIDECAR_SUFFIX: &str = ".kandrium.json";

/// Sidecar format version this build reads and writes. A version mismatch
/// fails the load instead of silently downgrading the file on the next save.
pub const ANNOTATION_VERSION: u32 = 1;

/// One node in the documents tree.
pub type DocEntry = FsEntry;

/// Annotation kind. The JSON encoding is the wire format of the sidecar;
/// unknown kinds from newer versions fail the load (see [`AnnotationDoc`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationKind {
    Highlight,
    Note,
}

/// One highlight or sticky note on a page.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DocAnnotation {
    pub id: String,
    pub kind: AnnotationKind,
    /// 0-based page index.
    pub page: usize,
    /// Quads in page view space: points, origin at the top-left of the
    /// displayed page (rotation applied), y down — the same space hayro
    /// renders in, so overlays are a pure `× zoom + offset` transform.
    /// Four corners, `[x, y]` each; for notes this is a small anchor quad.
    pub quads: Vec<[[f32; 2]; 4]>,
    /// Fill color as `#rrggbb`.
    pub color: String,
    /// Note body, or a highlight memo (may be empty).
    #[serde(default)]
    pub text: String,
    pub created_at: u64,
    pub modified_at: u64,
}

/// Versioned sidecar document. Deserialization is strict about the version
/// so a future format change surfaces as an error instead of silent data
/// loss on the next save.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AnnotationDoc {
    pub version: u32,
    #[serde(default)]
    pub annotations: Vec<DocAnnotation>,
}

impl Default for AnnotationDoc {
    fn default() -> Self {
        Self {
            version: ANNOTATION_VERSION,
            annotations: Vec::new(),
        }
    }
}

/// Resolve the documents root:
/// 1. `KANDRIUM_DOCS` env var (explicit override)
/// 2. `<data_dir>/kandrium/documents`
pub fn resolve_documents_root() -> PathBuf {
    if let Some(p) = std::env::var_os("KANDRIUM_DOCS") {
        return PathBuf::from(p);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kandrium")
        .join("documents")
}

/// Create the root directory if missing and return it.
pub fn ensure_root() -> Result<PathBuf> {
    let root = resolve_documents_root();
    fs::create_dir_all(&root)?;
    Ok(root)
}

/// Recursively scan `root` into a sorted tree; only `.pdf` files are
/// listed (sidecars are `.json` and never appear).
pub fn scan_tree(root: &Path) -> Result<Vec<DocEntry>> {
    fsutil::scan_tree(root, &["pdf"])
}

/// Sidecar path for a PDF: the same file name with `.kandrium.json`
/// appended (`report.pdf` → `report.pdf.kandrium.json`).
pub fn annotations_path(pdf: &Path) -> PathBuf {
    let mut name = pdf
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(SIDECAR_SUFFIX);
    pdf.with_file_name(name)
}

/// Load a PDF's annotations; a missing sidecar yields an empty document.
pub fn load_annotations(pdf: &Path, root: &Path) -> Result<AnnotationDoc> {
    let pdf = fsutil::guard(root, pdf)?;
    let sidecar = annotations_path(&pdf);
    if !sidecar.exists() {
        return Ok(AnnotationDoc::default());
    }
    let text = fs::read_to_string(&sidecar)?;
    let doc: AnnotationDoc = serde_json::from_str(&text)
        .map_err(|e| KandriumError::new(format!("Cannot parse annotations: {e}")))?;
    if doc.version != ANNOTATION_VERSION {
        return Err(KandriumError::new(format!(
            "Unsupported annotations version {} (this build reads {ANNOTATION_VERSION})",
            doc.version
        )));
    }
    Ok(doc)
}

/// Atomically persist a PDF's annotations. An empty document removes the
/// sidecar so fully un-annotated PDFs stay clean.
pub fn save_annotations(pdf: &Path, root: &Path, doc: &AnnotationDoc) -> Result<()> {
    let pdf = fsutil::guard(root, pdf)?;
    let sidecar = annotations_path(&pdf);
    if doc.annotations.is_empty() {
        if sidecar.exists() {
            fs::remove_file(&sidecar)?;
        }
        return Ok(());
    }
    let json = serde_json::to_string_pretty(doc)
        .map_err(|e| KandriumError::new(format!("Cannot serialize annotations: {e}")))?;
    // Staged write: never leave a half-written sidecar behind.
    let tmp = sidecar.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &sidecar)?;
    Ok(())
}

/// Build a new annotation with a fresh id and timestamps.
pub fn new_annotation(
    kind: AnnotationKind,
    page: usize,
    quads: Vec<[[f32; 2]; 4]>,
    color: &str,
) -> DocAnnotation {
    let now = now_unix();
    DocAnnotation {
        id: uid(),
        kind,
        page,
        quads,
        color: color.to_string(),
        text: String::new(),
        created_at: now,
        modified_at: now,
    }
}

/// Rename a file or directory in place (same parent). When the target is a
/// PDF, its sidecar moves with it (when one exists).
pub fn rename_path(path: &Path, root: &Path, new_name: &str) -> Result<PathBuf> {
    let path = fsutil::guard(root, path)?;
    let sidecar = path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
    let sidecar_from = sidecar.then(|| annotations_path(&path));
    let new_path = crate::notes::rename_in_root(&path, root, new_name)?;
    if let Some(from) = sidecar_from {
        let to = annotations_path(&new_path);
        if from.exists() {
            fs::rename(&from, &to)?;
        }
    }
    Ok(new_path)
}

/// Delete a file or directory (recursively). The documents root itself
/// cannot be deleted. Deleting a PDF removes its sidecar.
pub fn delete_path(path: &Path, root: &Path) -> Result<()> {
    let path = fsutil::guard(root, path)?;
    if path == fsutil::canonical_root(root)? {
        return Err(KandriumError::new("Cannot delete the documents root"));
    }
    if path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    {
        let sidecar = annotations_path(&path);
        if sidecar.exists() {
            fs::remove_file(&sidecar)?;
        }
    }
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("documents");
        fs::create_dir_all(&root).unwrap();
        Fixture { _tmp: tmp, root }
    }

    #[test]
    fn scan_lists_only_pdfs() {
        let f = fixture();
        fs::write(f.root.join("a.pdf"), "").unwrap();
        fs::write(f.root.join("b.kandrium.json"), "").unwrap();
        fs::write(f.root.join("c.md"), "").unwrap();
        fs::create_dir(f.root.join("sub")).unwrap();
        fs::write(f.root.join("sub").join("d.PDF"), "").unwrap();

        let tree = scan_tree(&f.root).unwrap();
        let names: Vec<&str> = tree.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["sub", "a.pdf"]);
        assert_eq!(tree[0].children.as_ref().unwrap()[0].name, "d.PDF");
    }

    #[test]
    fn sidecar_path_appends_suffix() {
        let p = annotations_path(Path::new("/x/report.pdf"));
        assert_eq!(p, Path::new("/x/report.pdf.kandrium.json"));
    }

    #[test]
    fn annotations_round_trip_and_missing_file() {
        let f = fixture();
        let pdf = f.root.join("report.pdf");
        fs::write(&pdf, b"%PDF-1.4 fake").unwrap();

        // Missing sidecar → empty doc.
        assert_eq!(
            load_annotations(&pdf, &f.root).unwrap(),
            AnnotationDoc::default()
        );

        let mut doc = AnnotationDoc::default();
        doc.annotations.push(new_annotation(
            AnnotationKind::Highlight,
            2,
            vec![[[10.0, 20.0], [110.0, 20.0], [110.0, 34.0], [10.0, 34.0]]],
            "#f6d448",
        ));
        save_annotations(&pdf, &f.root, &doc).unwrap();
        assert!(annotations_path(&pdf).exists());

        let loaded = load_annotations(&pdf, &f.root).unwrap();
        assert_eq!(loaded, doc);
        assert_eq!(loaded.annotations[0].page, 2);
        assert_eq!(loaded.annotations[0].color, "#f6d448");
    }

    #[test]
    fn foreign_version_fails_the_load() {
        let f = fixture();
        let pdf = f.root.join("report.pdf");
        fs::write(&pdf, b"x").unwrap();
        // A sidecar written by a future version must surface as an error
        // instead of being silently downgraded on the next save.
        fs::write(
            annotations_path(&pdf),
            r#"{"version": 2, "annotations": []}"#,
        )
        .unwrap();
        let err = load_annotations(&pdf, &f.root).unwrap_err();
        assert!(err.to_string().contains("Unsupported"), "{err}");
    }

    #[test]
    fn empty_annotations_remove_the_sidecar() {
        let f = fixture();
        let pdf = f.root.join("report.pdf");
        fs::write(&pdf, b"x").unwrap();

        let mut doc = AnnotationDoc::default();
        doc.annotations.push(new_annotation(
            AnnotationKind::Note,
            0,
            vec![[[0.0; 2]; 4]],
            "#8ab4f8",
        ));
        save_annotations(&pdf, &f.root, &doc).unwrap();
        assert!(annotations_path(&pdf).exists());

        save_annotations(&pdf, &f.root, &AnnotationDoc::default()).unwrap();
        assert!(!annotations_path(&pdf).exists());
    }

    #[test]
    fn save_is_atomic_no_tmp_left_behind() {
        let f = fixture();
        let pdf = f.root.join("report.pdf");
        fs::write(&pdf, b"x").unwrap();

        let mut doc = AnnotationDoc::default();
        doc.annotations.push(new_annotation(
            AnnotationKind::Highlight,
            0,
            vec![],
            "#ffffff",
        ));
        save_annotations(&pdf, &f.root, &doc).unwrap();

        let dir: Vec<_> = fs::read_dir(&f.root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            dir.iter().all(|n| !n.ends_with(".tmp")),
            "leftover tmp: {dir:?}"
        );
    }

    #[test]
    fn rename_moves_the_sidecar() {
        let f = fixture();
        let pdf = f.root.join("report.pdf");
        fs::write(&pdf, b"x").unwrap();
        let mut doc = AnnotationDoc::default();
        doc.annotations.push(new_annotation(
            AnnotationKind::Highlight,
            0,
            vec![],
            "#f6d448",
        ));
        save_annotations(&pdf, &f.root, &doc).unwrap();

        let renamed = rename_path(&pdf, &f.root, "renamed.pdf").unwrap();
        assert!(renamed.ends_with("renamed.pdf"));
        assert!(!annotations_path(&pdf).exists());
        assert_eq!(load_annotations(&renamed, &f.root).unwrap(), doc);
    }

    #[test]
    fn delete_removes_the_sidecar() {
        let f = fixture();
        let pdf = f.root.join("report.pdf");
        fs::write(&pdf, b"x").unwrap();
        let mut doc = AnnotationDoc::default();
        doc.annotations.push(new_annotation(
            AnnotationKind::Highlight,
            0,
            vec![],
            "#f6d448",
        ));
        save_annotations(&pdf, &f.root, &doc).unwrap();

        delete_path(&pdf, &f.root).unwrap();
        assert!(!pdf.exists());
        assert!(!annotations_path(&pdf).exists());
    }

    #[test]
    fn guard_rejects_traversal_and_deleting_the_root() {
        let f = fixture();
        fs::write(f.root.join("a.pdf"), "x").unwrap();
        assert!(load_annotations(&f.root.join("../a.pdf.kandrium.json"), &f.root).is_err());
        assert!(delete_path(&f.root, &f.root).is_err());
    }
}
