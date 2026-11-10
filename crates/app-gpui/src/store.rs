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

use std::path::{Path, PathBuf};

use kandrium_core::notes::{self, NoteEntry};
use kandrium_core::{
    Board, BoardWithColumns, CardStatus, Clock, CreateCardBody, Person, ProjectChoice, Travel,
    add_clock, add_person, bootstrap, end_travel, extend_travel, get_all_board, get_board_readonly,
    list_all_people, list_all_travels, list_boards, list_clocks, list_project_choices,
    migrate_legacy_db, move_all_card, move_area_card, open_db, open_db_readonly, relocate_person,
    remove_clock, remove_person, remove_travel, rename_person, reorder_clocks, resolve_db_path,
    set_card_area, set_card_project, start_travel, update_card,
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

    /// The notes root (`KANDRIUM_NOTES` honoured), created when missing.
    pub fn notes_root(&self) -> StoreResult<PathBuf> {
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

    /// Projects offered by the card modal's project picker.
    pub fn projects(&self) -> StoreResult<Vec<ProjectChoice>> {
        list_project_choices(self.conn()?).map_err(|e| e.to_string())
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
