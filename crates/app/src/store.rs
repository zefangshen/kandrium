//! Database access for the gpui front-end.
//!
//! Two handles on one file:
//!
//! - a **read** handle (`open_db_readonly`) used by every query, so rendering
//!   can never take the write lock, bump `data_version`, or hit
//!   `SQLITE_READONLY` because a read path reconciled a lazy order;
//! - a **write** handle opened through `db::bootstrap` with the real path, so
//!   integrity checking, quarantine and the pre-migration / daily snapshots
//!   apply to gpui exactly as they do to the app and the MCP server.
//!
//! All logic stays in `kandrium-core`; these are thin calls plus the
//! `CreateCardBody` shaping a form needs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kandrium_core::documents::{self, AnnotationDoc, DocEntry};
use kandrium_core::fsutil::FsEntry;
use kandrium_core::notes::{self, NoteEntry};
use kandrium_core::projects::{
    self as project_data, Project, ProjectFolder, ProjectNode, ProjectPath, ProjectSummary,
};
use kandrium_core::wiki::{self, LintFinding, SourceDelta, SourceState, WikiStatus};
use kandrium_core::{
    ArchivedCard, Board, BoardWithColumns, CardStatus, Clock, CreateCardBody, Person,
    ProjectChoice, Travel, add_clock, add_person, bootstrap, delete_card, end_travel,
    extend_travel, get_all_board, get_board_readonly, list_all_people, list_all_travels,
    list_archived_cards, list_boards, list_clocks, list_project_choices, migrate_legacy_db,
    move_all_card, move_area_card, move_card_to_board, open_db, open_db_readonly, relocate_person,
    remove_clock, remove_person, remove_travel, rename_card, rename_person, reorder_clocks,
    resolve_db_path, set_card_archived, set_card_area, set_card_project, start_travel, update_card,
};

/// One snapshot of the Clocks data. The app caches this so the minute tick
/// never touches SQLite.
pub struct ClockData {
    pub clocks: Vec<Clock>,
    pub people: Vec<Person>,
    pub travels: Vec<Travel>,
}

/// Result type for the store: a user-facing message string.
pub type StoreResult<T> = Result<T, String>;

/// The open database handles plus the path they came from.
pub struct Store {
    read: Option<rusqlite::Connection>,
    write: Option<rusqlite::Connection>,
    pub path: PathBuf,
    pub error: Option<String>,
}

impl Store {
    /// Open the default database (or the `KANDRIUM_DB` override), migrating a
    /// legacy database first like the other front-ends.
    pub fn open() -> Self {
        match migrate_legacy_db() {
            Ok(Some(from)) => kandrium_core::logging::info!(
                "migrated database from {} to {}",
                from.display(),
                resolve_db_path().display()
            ),
            Ok(None) => {}
            Err(e) => kandrium_core::logging::warn!("legacy database migration failed: {e}"),
        }
        Self::open_at(resolve_db_path())
    }

    /// Open a specific database file writable. Used by the headless tests.
    pub fn open_path(path: PathBuf) -> Self {
        Self::open_at(path)
    }

    fn open_at(path: PathBuf) -> Self {
        let mut write = match open_db(&path) {
            Ok(db) => db,
            Err(e) => {
                return Self {
                    read: None,
                    write: None,
                    path,
                    error: Some(e.to_string()),
                };
            }
        };
        if let Err(e) = bootstrap(&mut write, &path) {
            return Self {
                read: None,
                write: None,
                path,
                error: Some(e.0),
            };
        }
        // The writable open above created the file, so the read-only handle
        // always has something to open.
        match open_db_readonly(&path) {
            Ok(read) => Self {
                read: Some(read),
                write: Some(write),
                path,
                error: None,
            },
            Err(e) => Self {
                read: None,
                write: None,
                path,
                error: Some(e.to_string()),
            },
        }
    }

    fn conn(&self) -> StoreResult<&rusqlite::Connection> {
        self.read.as_ref().ok_or_else(|| self.unavailable())
    }

    fn wconn(&self) -> StoreResult<&rusqlite::Connection> {
        self.write.as_ref().ok_or_else(|| self.unavailable())
    }

    fn unavailable(&self) -> String {
        self.error
            .clone()
            .unwrap_or_else(|| "database unavailable".into())
    }

    /// Boards shown in the Tasks section (project boards are excluded by core).
    pub fn boards(&self) -> StoreResult<Vec<Board>> {
        list_boards(self.conn()?)
            .map(|result| result.boards)
            .map_err(|e| e.to_string())
    }

    /// One board, derived without persisting the lazy area order.
    pub fn board(&self, id: &str) -> StoreResult<BoardWithColumns> {
        get_board_readonly(self.conn()?, id).map_err(|e| e.to_string())
    }

    /// The virtual All board, shaped like a real one so the board surface can
    /// render it unchanged. Each synthetic column id is `all:<status>`, and
    /// `meta_label` carries the owning board (or area board) title.
    pub fn all_board(&self) -> StoreResult<BoardWithColumns> {
        // `get_all_board` backfills `all_card_order`, so it needs the writable
        // handle even though the caller only renders.
        let all = get_all_board(self.wconn()?).map_err(|e| e.to_string())?;
        Ok(BoardWithColumns {
            id: all.id,
            title: all.title,
            columns: all
                .columns
                .into_iter()
                .map(|column| kandrium_core::Column {
                    id: column.id,
                    title: column.title,
                    cards: column
                        .cards
                        .into_iter()
                        .map(|entry| {
                            let mut card = entry.card;
                            card.meta_label = Some(entry.board_title);
                            card
                        })
                        .collect(),
                    mirrored: Vec::new(),
                })
                .collect(),
        })
    }

    /// One consistent snapshot of cities, people and trips. The app caches
    /// these rows; rendering and the minute tick never query SQLite.
    pub fn clock_data(&self) -> StoreResult<ClockData> {
        let db = self.conn()?;
        Ok(ClockData {
            clocks: list_clocks(db).map_err(|e| e.to_string())?,
            people: list_all_people(db).map_err(|e| e.to_string())?,
            travels: list_all_travels(db).map_err(|e| e.to_string())?,
        })
    }

    /// Add a saved city. Core rejects duplicates.
    pub fn add_clock(&self, tz_identifier: &str) -> StoreResult<()> {
        add_clock(self.wconn()?, tz_identifier)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Remove a saved city. Core refuses while it has residents
    /// (`ON DELETE RESTRICT`); the returned message is the one to show.
    pub fn remove_clock(&self, clock_id: &str) -> StoreResult<()> {
        remove_clock(self.wconn()?, clock_id).map_err(|e| e.to_string())
    }

    /// Commit a complete permutation of clock ids.
    pub fn reorder_clocks(&self, ids: &[String]) -> StoreResult<()> {
        reorder_clocks(self.wconn()?, ids).map_err(|e| e.to_string())
    }

    pub fn add_person(&self, clock_id: &str, name: &str) -> StoreResult<()> {
        add_person(self.wconn()?, clock_id, name)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn rename_person(&self, person_id: &str, name: &str) -> StoreResult<()> {
        rename_person(self.wconn()?, person_id, name).map_err(|e| e.to_string())
    }

    pub fn remove_person(&self, person_id: &str) -> StoreResult<()> {
        remove_person(self.wconn()?, person_id).map_err(|e| e.to_string())
    }

    pub fn relocate_person(&self, person_id: &str, clock_id: &str) -> StoreResult<()> {
        relocate_person(self.wconn()?, person_id, clock_id).map_err(|e| e.to_string())
    }

    pub fn start_travel(
        &self,
        person_id: &str,
        destination: &str,
        starts_at: i64,
        ends_at: Option<i64>,
    ) -> StoreResult<()> {
        start_travel(self.wconn()?, person_id, destination, starts_at, ends_at)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// End a running trip now.
    pub fn end_travel(&self, travel_id: &str) -> StoreResult<()> {
        end_travel(self.wconn()?, travel_id).map_err(|e| e.to_string())
    }

    /// Set or push a trip's return time. `None` makes it open-ended.
    pub fn extend_travel(&self, travel_id: &str, ends_at: Option<i64>) -> StoreResult<()> {
        extend_travel(self.wconn()?, travel_id, ends_at).map_err(|e| e.to_string())
    }

    pub fn remove_travel(&self, travel_id: &str) -> StoreResult<()> {
        remove_travel(self.wconn()?, travel_id).map_err(|e| e.to_string())
    }

    // -----------------------------------------------------------------------
    // Notes — the filesystem layer needs no database handle, so these work
    // even when the database itself failed to open.
    // -----------------------------------------------------------------------

    /// The compiled wiki page root (`KANDRIUM_WIKI`), created when missing.
    pub fn notes_root(&self) -> StoreResult<PathBuf> {
        wiki::ensure_wiki_root().map_err(|e| e.to_string())
    }

    /// The personal notes root (`KANDRIUM_NOTES`), created when missing. The
    /// Notes half of the Knowledge tree reads and writes here.
    pub fn personal_notes_root(&self) -> StoreResult<PathBuf> {
        notes::ensure_root().map_err(|e| e.to_string())
    }

    pub fn notes_tree(&self, root: &Path) -> StoreResult<Vec<NoteEntry>> {
        notes::scan_tree(root).map_err(|e| e.to_string())
    }

    pub fn read_note(&self, path: &Path, root: &Path) -> StoreResult<String> {
        notes::read_note(path, root).map_err(|e| e.to_string())
    }

    pub fn write_note(&self, path: &Path, root: &Path, text: &str) -> StoreResult<()> {
        notes::write_note(path, root, text).map_err(|e| e.to_string())
    }

    /// Create a name-validated note inside `in_dir` (`.md` appended).
    pub fn create_note(&self, root: &Path, in_dir: &Path, name: &str) -> StoreResult<PathBuf> {
        notes::create_named_note(root, in_dir, name).map_err(|e| e.to_string())
    }

    pub fn create_folder(&self, root: &Path, in_dir: &Path, name: &str) -> StoreResult<PathBuf> {
        notes::create_named_folder(root, in_dir, name).map_err(|e| e.to_string())
    }

    pub fn rename_note(&self, path: &Path, root: &Path, new_name: &str) -> StoreResult<PathBuf> {
        notes::rename_path(path, root, new_name).map_err(|e| e.to_string())
    }

    pub fn delete_note(&self, path: &Path, root: &Path) -> StoreResult<()> {
        notes::delete_path(path, root).map_err(|e| e.to_string())
    }

    // -----------------------------------------------------------------------
    // Wiki index — a derived view over the page files and the documents
    // folder. Reads use the read handle; refresh/reindex use the write one.
    // -----------------------------------------------------------------------

    pub fn wiki_status(&self) -> StoreResult<WikiStatus> {
        wiki::wiki_status(self.conn()?).map_err(|e| e.to_string())
    }

    pub fn wiki_lint(&self) -> StoreResult<Vec<LintFinding>> {
        let root = self.notes_root()?;
        let documents = self.documents_root()?;
        wiki::lint_wiki(self.conn()?, &root, &documents, None).map_err(|e| e.to_string())
    }

    /// Absolute source path → compile state, for the tree dots.
    pub fn wiki_source_states(&self) -> StoreResult<HashMap<PathBuf, SourceState>> {
        let documents = self.documents_root()?;
        let sources = wiki::list_sources(self.conn()?, None).map_err(|e| e.to_string())?;
        Ok(sources
            .into_iter()
            .map(|source| (documents.join(source.rel_path), source.state))
            .collect())
    }

    /// Rescan sources and rebuild the page index plus `index.md`.
    pub fn refresh_wiki(&self) -> StoreResult<(SourceDelta, usize)> {
        let root = self.notes_root()?;
        let documents = self.documents_root()?;
        let delta = wiki::refresh_sources(self.wconn()?, &documents).map_err(|e| e.to_string())?;
        let pages = wiki::index_all_pages(self.wconn()?, &root).map_err(|e| e.to_string())?;
        Ok((delta, pages))
    }

    /// Reindex one page after an editor save.
    pub fn reindex_wiki_page(&self, rel_path: &str) -> StoreResult<()> {
        let root = self.notes_root()?;
        wiki::reindex_page(self.wconn()?, &root, rel_path).map_err(|e| e.to_string())
    }

    // -----------------------------------------------------------------------
    // Documents — same filesystem layer, PDFs plus Markdown and annotation
    // sidecars. No database handle involved.
    // -----------------------------------------------------------------------

    /// The documents root (`KANDRIUM_DOCS` honoured), created when missing and
    /// canonicalized so tab paths can be stored relative to it.
    pub fn documents_root(&self) -> StoreResult<PathBuf> {
        let root = documents::ensure_root().map_err(|e| e.to_string())?;
        std::fs::canonicalize(root).map_err(|e| e.to_string())
    }

    pub fn documents_tree(&self, root: &Path) -> StoreResult<Vec<DocEntry>> {
        documents::scan_tree(root).map_err(|e| e.to_string())
    }

    pub fn read_document(&self, path: &Path, root: &Path) -> StoreResult<String> {
        documents::read_text(path, root).map_err(|e| e.to_string())
    }

    pub fn write_document(&self, path: &Path, root: &Path, text: &str) -> StoreResult<()> {
        documents::write_text(path, root, text).map_err(|e| e.to_string())
    }

    /// Create a Markdown file inside `parent` (bare names gain `.md`).
    pub fn create_markdown(&self, parent: &Path, root: &Path, name: &str) -> StoreResult<PathBuf> {
        documents::create_markdown(parent, root, name).map_err(|e| e.to_string())
    }

    pub fn create_doc_folder(
        &self,
        parent: &Path,
        root: &Path,
        name: &str,
    ) -> StoreResult<PathBuf> {
        documents::create_folder(parent, root, name).map_err(|e| e.to_string())
    }

    /// Copy files into `dest`, never overwriting (`-2`, `-3`, …). A failed
    /// file is reported in its slot so the rest of the batch still lands.
    pub fn import_documents(
        &self,
        sources: &[PathBuf],
        dest: &Path,
        root: &Path,
    ) -> StoreResult<Vec<(PathBuf, Option<String>)>> {
        let mut results = Vec::with_capacity(sources.len());
        for source in sources {
            let outcome = documents::import_file(source, dest, root)
                .map(|path| (path, None))
                .unwrap_or_else(|e| (source.clone(), Some(e.to_string())));
            results.push(outcome);
        }
        Ok(results)
    }

    pub fn rename_document(
        &self,
        path: &Path,
        root: &Path,
        new_name: &str,
    ) -> StoreResult<PathBuf> {
        documents::rename_path(path, root, new_name).map_err(|e| e.to_string())
    }

    pub fn delete_document(&self, path: &Path, root: &Path) -> StoreResult<()> {
        documents::delete_path(path, root).map_err(|e| e.to_string())
    }

    pub fn load_annotations(&self, pdf: &Path, root: &Path) -> StoreResult<AnnotationDoc> {
        documents::load_annotations(pdf, root).map_err(|e| e.to_string())
    }

    pub fn save_annotations(
        &self,
        pdf: &Path,
        root: &Path,
        doc: &AnnotationDoc,
    ) -> StoreResult<()> {
        documents::save_annotations(pdf, root, doc).map_err(|e| e.to_string())
    }

    /// Projects offered by the card modal's project picker.
    pub fn projects(&self) -> StoreResult<Vec<ProjectChoice>> {
        list_project_choices(self.conn()?).map_err(|e| e.to_string())
    }

    // -----------------------------------------------------------------------
    // Projects — the tree lives in SQLite, documents on disk under
    // `<projects root>/<project id>/`. All guards and atomic writes are core's.
    // -----------------------------------------------------------------------

    /// The projects root (`KANDRIUM_PROJECTS` honoured), created when missing.
    pub fn projects_root(&self) -> StoreResult<PathBuf> {
        project_data::ensure_root().map_err(|e| e.to_string())
    }

    pub fn project_tree(&self) -> StoreResult<Vec<ProjectNode>> {
        project_data::list_tree(self.conn()?).map_err(|e| e.to_string())
    }

    pub fn project(&self, id: &str) -> StoreResult<Project> {
        project_data::get_project(self.conn()?, id).map_err(|e| e.to_string())
    }

    pub fn project_summary(&self, root: &Path, id: &str) -> StoreResult<ProjectSummary> {
        project_data::project_summary(self.conn()?, root, id).map_err(|e| e.to_string())
    }

    pub fn project_files(&self, root: &Path, id: &str) -> StoreResult<Vec<FsEntry>> {
        project_data::project_files(root, id).map_err(|e| e.to_string())
    }

    pub fn archived_projects(&self) -> StoreResult<Vec<Project>> {
        project_data::list_archived_projects(self.conn()?).map_err(|e| e.to_string())
    }

    /// Create a project with the blank template and its on-disk folder.
    pub fn create_project(
        &self,
        root: &Path,
        name: &str,
        folder: Option<&str>,
    ) -> StoreResult<Project> {
        project_data::create_project(self.wconn()?, root, name, folder, "blank")
            .map_err(|e| e.to_string())
    }

    pub fn create_project_folder(
        &self,
        name: &str,
        parent: Option<&str>,
    ) -> StoreResult<ProjectFolder> {
        project_data::create_folder(self.wconn()?, name, parent).map_err(|e| e.to_string())
    }

    pub fn rename_project_folder(&self, id: &str, name: &str) -> StoreResult<ProjectFolder> {
        project_data::rename_folder(self.wconn()?, id, name).map_err(|e| e.to_string())
    }

    pub fn delete_project_folder(&self, root: &Path, id: &str) -> StoreResult<()> {
        project_data::delete_folder(self.wconn()?, root, id).map_err(|e| e.to_string())
    }

    pub fn rename_project(&self, id: &str, name: &str) -> StoreResult<Project> {
        project_data::rename_project(self.wconn()?, id, name).map_err(|e| e.to_string())
    }

    pub fn update_project(
        &self,
        id: &str,
        description: Option<&str>,
        status: Option<&str>,
    ) -> StoreResult<Project> {
        project_data::update_project(self.wconn()?, id, description, status)
            .map_err(|e| e.to_string())
    }

    pub fn set_project_source(&self, id: &str, source: &str) -> StoreResult<Project> {
        project_data::set_source(self.wconn()?, id, source).map_err(|e| e.to_string())
    }

    pub fn project_paths(&self, id: &str) -> StoreResult<Vec<ProjectPath>> {
        project_data::list_project_paths(self.conn()?, id).map_err(|e| e.to_string())
    }

    pub fn add_project_path(&self, id: &str, path: &str) -> StoreResult<ProjectPath> {
        project_data::add_project_path(self.wconn()?, id, path).map_err(|e| e.to_string())
    }

    pub fn remove_project_path(&self, path_id: &str) -> StoreResult<()> {
        project_data::remove_project_path(self.wconn()?, path_id).map_err(|e| e.to_string())
    }

    pub fn set_project_identifier(&self, id: &str, identifier: &str) -> StoreResult<Project> {
        project_data::set_identifier(self.wconn()?, id, identifier).map_err(|e| e.to_string())
    }

    pub fn archive_project(&self, id: &str) -> StoreResult<()> {
        project_data::archive_project(self.wconn()?, id).map_err(|e| e.to_string())
    }

    pub fn unarchive_project(&self, id: &str) -> StoreResult<()> {
        project_data::unarchive_project(self.wconn()?, id).map_err(|e| e.to_string())
    }

    pub fn delete_project(&self, root: &Path, id: &str) -> StoreResult<()> {
        project_data::delete_project(self.wconn()?, root, id).map_err(|e| e.to_string())
    }

    pub fn reorder_project(&self, id: &str, index: usize) -> StoreResult<()> {
        project_data::reorder_project(self.wconn()?, id, index).map_err(|e| e.to_string())
    }

    pub fn read_project_doc(&self, root: &Path, id: &str, rel: &Path) -> StoreResult<String> {
        project_data::read_doc(root, id, rel).map_err(|e| e.to_string())
    }

    pub fn write_project_doc(
        &self,
        root: &Path,
        id: &str,
        rel: &Path,
        text: &str,
    ) -> StoreResult<()> {
        project_data::write_doc(root, id, rel, text).map_err(|e| e.to_string())
    }

    pub fn create_project_doc(
        &self,
        root: &Path,
        id: &str,
        parent_rel: &Path,
        name: &str,
    ) -> StoreResult<PathBuf> {
        project_data::create_doc_file(root, id, parent_rel, name).map_err(|e| e.to_string())
    }

    pub fn create_project_doc_folder(
        &self,
        root: &Path,
        id: &str,
        parent_rel: &Path,
        name: &str,
    ) -> StoreResult<PathBuf> {
        project_data::create_doc_folder(root, id, parent_rel, name).map_err(|e| e.to_string())
    }

    /// Move or reorder a card on the visible board. `move_area_card` keeps the
    /// unified area order and the mirrored-project-card semantics; it falls
    /// back to `move_card` on project boards and non-status columns.
    pub fn move_card(
        &self,
        card_id: &str,
        to_column_id: &str,
        before_card_id: Option<&str>,
    ) -> StoreResult<()> {
        move_area_card(self.wconn()?, card_id, to_column_id, before_card_id)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Move a card between status buckets from the All board. The home board's
    /// matching status column is updated by core.
    pub fn move_all_card(
        &self,
        card_id: &str,
        status: CardStatus,
        before_card_id: Option<&str>,
    ) -> StoreResult<()> {
        move_all_card(self.wconn()?, card_id, status, before_card_id)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Move a card to another Tasks board, keeping its column by status when
    /// core can (mirrors the egui modal's "Move to board").
    pub fn move_card_to_board(&self, card_id: &str, board_id: &str) -> StoreResult<()> {
        move_card_to_board(self.wconn()?, card_id, board_id)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Create a card in `column_id` from the inline "+ Add card" row.
    pub fn create_card(&self, column_id: &str, title: &str) -> StoreResult<()> {
        kandrium_core::create_card(
            self.wconn()?,
            column_id,
            &CreateCardBody {
                title: title.to_string(),
                description: None,
                due_date: None,
                labels: None,
            },
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// Rename one card's title only, through the same core path as the modal.
    pub fn rename_card(&self, card_id: &str, title: &str) -> StoreResult<()> {
        rename_card(self.wconn()?, card_id, title)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Every archived card with its board and column labels.
    pub fn archived_cards(&self) -> StoreResult<Vec<ArchivedCard>> {
        list_archived_cards(self.conn()?)
            .map(|result| result.cards)
            .map_err(|e| e.to_string())
    }

    /// Archive or restore one card. Archives go through the write handle so
    /// the board and the Archived page see the same row.
    pub fn set_card_archived(&self, card_id: &str, archived: bool) -> StoreResult<()> {
        set_card_archived(self.wconn()?, card_id, archived)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Delete one card permanently.
    pub fn delete_card(&self, card_id: &str) -> StoreResult<()> {
        delete_card(self.wconn()?, card_id)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Link (or unlink) a card to a project. This is the modal dropdown's
    /// immediate write; it touches no other card column.
    pub fn set_card_project(&self, card_id: &str, project_id: Option<&str>) -> StoreResult<()> {
        set_card_project(self.wconn()?, card_id, project_id)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Save the fields the modal owns. `project` / `area` are `Some` only when
    /// the user changed the picker.
    pub fn save_card(
        &self,
        card_id: &str,
        body: &CreateCardBody,
        project: Option<Option<&str>>,
        area: Option<Option<&str>>,
    ) -> StoreResult<()> {
        let db = self.wconn()?;
        update_card(db, card_id, body).map_err(|e| e.to_string())?;
        if let Some(project_id) = project {
            set_card_project(db, card_id, project_id).map_err(|e| e.to_string())?;
        }
        if let Some(area_id) = area {
            set_card_area(db, card_id, area_id).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kandrium_core::projects::find_project;

    fn temp_store(dir: &Path) -> Store {
        let path = dir.join("kandrium.db");
        let mut db = kandrium_core::open_db(&path).expect("open temp db");
        kandrium_core::bootstrap(&mut db, &path).expect("bootstrap temp db");
        Store::open_path(path)
    }

    #[test]
    fn project_crud_round_trips_through_core() {
        let dir = tempfile::tempdir().unwrap();
        let store = temp_store(dir.path());
        let root = dir.path().join("projects");

        let folder = store
            .create_project_folder("Work", None)
            .expect("create folder");
        let project = store
            .create_project(&root, "Alpha Project", Some(&folder.id))
            .expect("create project");

        let tree = store.project_tree().expect("tree");
        let tree = tree
            .iter()
            .find_map(|node| match node {
                ProjectNode::Folder { id, children, .. } if *id == folder.id => Some(children),
                _ => None,
            })
            .expect("folder in tree");
        assert_eq!(tree.len(), 1);
        assert_eq!(
            find_project(tree, &project.id).map(|found| found.name.as_str()),
            Some("Alpha Project")
        );

        store.archive_project(&project.id).expect("archive");
        assert!(store.project_tree().expect("tree").iter().all(
            |node| matches!(node, ProjectNode::Folder { children, .. } if children.is_empty())
        ));
        assert_eq!(store.archived_projects().expect("archived").len(), 1);
        store.unarchive_project(&project.id).expect("restore");
        assert!(find_project(&store.project_tree().expect("tree"), &project.id).is_some());
    }

    #[test]
    fn reorder_moves_only_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let store = temp_store(dir.path());
        let root = dir.path().join("projects");
        let folder = store
            .create_project_folder("Work", None)
            .expect("create folder");

        let first = store.create_project(&root, "First", None).expect("first");
        let second = store.create_project(&root, "Second", None).expect("second");
        let nested = store
            .create_project(&root, "Nested", Some(&folder.id))
            .expect("nested");

        store.reorder_project(&second.id, 0).expect("reorder");
        let tree = store.project_tree().expect("tree");
        let root_projects: Vec<&str> = tree
            .iter()
            .filter_map(|node| match node {
                ProjectNode::Project(project) => Some(project.id.as_str()),
                ProjectNode::Folder { .. } => None,
            })
            .collect();
        assert_eq!(root_projects, vec![second.id.as_str(), first.id.as_str()]);
        assert!(find_project(&tree, &nested.id).is_some());
    }

    #[test]
    fn project_documents_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = temp_store(dir.path());
        let root = dir.path().join("projects");
        let project = store.create_project(&root, "Docs", None).expect("project");

        let created = store
            .create_project_doc(&root, &project.id, Path::new(""), "notes.md")
            .expect("create doc");
        store
            .write_project_doc(&root, &project.id, Path::new("notes.md"), "hello")
            .expect("write");
        assert_eq!(
            store
                .read_project_doc(&root, &project.id, Path::new("notes.md"))
                .expect("read"),
            "hello"
        );
        assert_eq!(created.file_name().unwrap(), "notes.md");
        assert_eq!(
            store
                .project_files(&root, &project.id)
                .expect("files")
                .len(),
            1
        );
    }

    /// The shared bootstrap path is what the retired egui app wrote too, so a
    /// database it created opens here with its seed intact.
    #[test]
    fn opens_a_database_written_by_the_shared_bootstrap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kandrium.db");
        let mut db = kandrium_core::open_db(&path).expect("open temp db");
        kandrium_core::bootstrap(&mut db, &path).expect("bootstrap temp db");
        drop(db);

        let store = Store::open_path(path);
        let boards = store.boards().expect("boards");
        assert!(!boards.is_empty(), "seeded boards must stay readable");
        let board = store.board(&boards[0].id).expect("board");
        assert!(
            !board.columns.is_empty(),
            "seeded columns must stay readable"
        );
    }

    /// The All board labels every card with the board it lives on.
    #[test]
    fn all_board_labels_a_card_with_its_home_board_title() {
        let dir = tempfile::tempdir().unwrap();
        let store = temp_store(dir.path());
        let board = store
            .boards()
            .expect("boards")
            .into_iter()
            .next()
            .expect("board");
        let column = store.board(&board.id).expect("board").columns[0].id.clone();
        store.create_card(&column, "Labelled").expect("card");
        let card = store
            .board(&board.id)
            .expect("board")
            .columns
            .iter()
            .flat_map(|column| column.cards.iter())
            .find(|card| card.title == "Labelled")
            .cloned()
            .expect("created card");

        let all = store.all_board().expect("all board");
        let label = all
            .columns
            .iter()
            .flat_map(|column| column.cards.iter())
            .find(|entry| entry.id == card.id)
            .and_then(|entry| entry.meta_label.clone());
        assert_eq!(label.as_deref(), Some(board.title.as_str()));
    }

    /// A project card filed onto a normal board joins that board's own order
    /// and acts as a drop anchor, not as a read-only mirror.
    #[test]
    fn area_board_treats_a_filed_project_card_as_a_drop_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let store = temp_store(dir.path());
        let root = dir.path().join("projects");
        let project = store
            .create_project(&root, "Filed Project", None)
            .expect("project");
        store
            .create_card(
                &store.board(&project.board_id).expect("board").columns[0].id,
                "Filed",
            )
            .expect("project card");
        let filed = store
            .board(&project.board_id)
            .expect("board")
            .columns
            .iter()
            .flat_map(|column| column.cards.iter())
            .find(|card| card.title == "Filed")
            .cloned()
            .expect("filed card");

        let area = store
            .boards()
            .expect("boards")
            .into_iter()
            .next()
            .expect("board");
        let area_column = store.board(&area.id).expect("board").columns[0].id.clone();
        store.create_card(&area_column, "Anchor").expect("anchor");
        let anchor = store
            .board(&area.id)
            .expect("board")
            .columns
            .iter()
            .flat_map(|column| column.cards.iter())
            .find(|card| card.title == "Anchor")
            .cloned()
            .expect("anchor card");

        store
            .save_card(
                &filed.id,
                &CreateCardBody {
                    title: filed.title.clone(),
                    ..Default::default()
                },
                None,
                Some(Some(&area.id)),
            )
            .expect("file card onto the area board");
        let column = store
            .board(&area.id)
            .expect("board")
            .columns
            .into_iter()
            .find(|column| column.mirrored.iter().any(|card| card.id == filed.id))
            .expect("mirrored card");
        assert!(
            column
                .ordered_cards()
                .iter()
                .any(|card| card.id == filed.id),
            "a filed project card is part of the board's unified order"
        );

        store
            .move_card(&filed.id, &area_column, Some(&anchor.id))
            .expect("drop the mirror above the anchor");
        let column = store
            .board(&area.id)
            .expect("board")
            .columns
            .into_iter()
            .find(|column| column.id == area_column)
            .expect("column");
        let order = column.ordered_cards();
        let filed_at = order
            .iter()
            .position(|card| card.id == filed.id)
            .expect("filed card");
        let anchor_at = order
            .iter()
            .position(|card| card.id == anchor.id)
            .expect("anchor card");
        assert!(
            filed_at < anchor_at,
            "the mirror is a real drop anchor: {filed_at} < {anchor_at}"
        );
    }
}
