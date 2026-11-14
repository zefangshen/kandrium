//! PDF text extraction bridge for wiki sources.
//!
//! The app renders PDFs through `pdf-render`'s worker. Readers need a
//! synchronous answer instead, so this module wraps the crate's text
//! extraction and guards every path to the documents root. A PDF is only a
//! source: it is never modified.

use std::path::Path;

use crate::error::{KandriumError, Result};
use crate::fsutil;

/// Page count for a PDF under the documents root.
pub fn page_count(documents_root: &Path, rel_path: &str) -> Result<usize> {
    let path = guarded(documents_root, rel_path)?;
    pdf_render::page_count(&path).map_err(KandriumError)
}

/// Extract pages in the 1-based inclusive range.
pub fn extract(
    documents_root: &Path,
    rel_path: &str,
    first: Option<usize>,
    last: Option<usize>,
) -> Result<Vec<pdf_render::PageText>> {
    let path = guarded(documents_root, rel_path)?;
    pdf_render::extract_text(&path, first, last).map_err(KandriumError)
}

/// Plain text for a page range: lines joined with newlines, pages separated
/// by a horizontal rule.
pub fn page_text(pages: &[pdf_render::PageText]) -> String {
    let mut out = Vec::new();
    for page in pages {
        let mut lines = Vec::new();
        for line in &page.lines {
            let text: String = page
                .line_glyphs(line)
                .map(|glyph| glyph.text.as_str())
                .collect();
            if !text.trim().is_empty() {
                lines.push(text);
            }
        }
        if !lines.is_empty() {
            out.push(lines.join("\n"));
        }
    }
    out.join("\n\n---\n\n")
}

fn guarded(documents_root: &Path, rel_path: &str) -> Result<std::path::PathBuf> {
    fsutil::guard(documents_root, &documents_root.join(rel_path))
}
