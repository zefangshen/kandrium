//! Wiki: the merged Notes and Documents section.
//!
//! The section owns both file trees (notes above, documents below) and one tab
//! row shared by the two kinds. This module only composes the two existing
//! states today; the vertical split, the merged strip and the detail
//! dispatcher land in the staged merge, so the tree panels and the per-kind
//! editors stay in [`super::notes`] and [`super::documents`].

use std::path::PathBuf;

use crate::views::documents::DocumentsState;
use crate::views::notes::NotesState;

/// Which tree a merged tab belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    Note,
    Doc,
}

/// One tab in the merged row. Keyed by kind and path so it survives the
/// per-kind index invalidation a close causes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabKey {
    pub kind: TabKind,
    pub path: PathBuf,
}

impl TabKey {
    pub fn note(path: PathBuf) -> Self {
        Self {
            kind: TabKind::Note,
            path,
        }
    }

    pub fn document(path: PathBuf) -> Self {
        Self {
            kind: TabKind::Doc,
            path,
        }
    }
}

/// Wiki section state: both tree states plus the merged tab order.
///
/// The order and active fields are populated once the merged strip exists;
/// until then the per-kind `active` indices remain the only selection source.
#[allow(dead_code)]
pub struct WikiState {
    pub notes: NotesState,
    pub documents: DocumentsState,
    /// Display order of the merged tab row, oldest first.
    pub order: Vec<TabKey>,
    /// The tab the detail column renders.
    pub active: Option<TabKey>,
    /// Notes-half share of the tree split (0.15–0.85).
    pub split: f32,
    pub notes_collapsed: bool,
    pub docs_collapsed: bool,
}

impl Default for WikiState {
    fn default() -> Self {
        Self {
            notes: NotesState::new(),
            documents: DocumentsState::new(),
            order: Vec::new(),
            active: None,
            split: 0.5,
            notes_collapsed: false,
            docs_collapsed: false,
        }
    }
}

impl WikiState {
    pub fn new() -> Self {
        Self::default()
    }
}
