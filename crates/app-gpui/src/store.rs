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

use std::path::PathBuf;

use jiff::Timestamp;
use kandrium_core::{
    Board, BoardWithColumns, ClockText, CreateCardBody, ProjectChoice, bootstrap, city_name,
    format_clock, get_board_readonly, list_boards, list_clocks, list_project_choices,
    migrate_legacy_db, move_area_card, open_db, open_db_readonly, resolve_db_path, resolve_zone,
    set_card_area, set_card_project, update_card,
};

/// One saved city, formatted for display.
pub struct ClockRow {
    pub id: String,
    pub city: String,
    pub text: ClockText,
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

    /// Saved cities formatted at `now`.
    pub fn clocks(&self, now: Timestamp, use_24h: bool) -> StoreResult<Vec<ClockRow>> {
        let db = self.conn()?;
        let mut rows = Vec::new();
        for clock in list_clocks(db).map_err(|e| e.to_string())? {
            let Some(zone) = resolve_zone(&clock.tz_identifier) else {
                continue;
            };
            rows.push(ClockRow {
                id: clock.id.clone(),
                city: city_name(&clock.tz_identifier),
                text: format_clock(now, &zone, use_24h),
            });
        }
        Ok(rows)
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
