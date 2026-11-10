//! Read-only view data for the gpui front-end.
//!
//! Everything here is a thin call into `kandrium-core`; the crate owns no
//! query logic. The connection is opened read-only so this second binary can
//! share the app's database without taking the write lock (see
//! `kandrium_core::open_db_readonly`).

use std::path::PathBuf;

use jiff::Timestamp;
use kandrium_core::{
    Board, BoardWithColumns, ClockText, city_name, format_clock, get_board_readonly, list_boards,
    list_clocks, open_db_readonly, resolve_db_path, resolve_zone,
};

/// One saved city, formatted for display.
pub struct ClockRow {
    pub id: String,
    pub city: String,
    pub text: ClockText,
}

/// Result type for the store: a user-facing message string.
pub type StoreResult<T> = Result<T, String>;

/// The open read-only database plus the path it came from.
pub struct Store {
    db: Option<rusqlite::Connection>,
    pub path: PathBuf,
    pub error: Option<String>,
}

impl Store {
    /// Open the default database (or the `KANDRIUM_DB` override).
    pub fn open() -> Self {
        Self::open_at(resolve_db_path())
    }

    fn open_at(path: PathBuf) -> Self {
        match open_db_readonly(&path) {
            Ok(db) => Self {
                db: Some(db),
                path,
                error: None,
            },
            Err(e) => Self {
                db: None,
                path,
                error: Some(e.to_string()),
            },
        }
    }

    /// Open a specific database file. Used by the headless tests.
    pub fn open_path(path: PathBuf) -> Self {
        Self::open_at(path)
    }

    fn conn(&self) -> StoreResult<&rusqlite::Connection> {
        self.db.as_ref().ok_or_else(|| {
            self.error
                .clone()
                .unwrap_or_else(|| "database unavailable".into())
        })
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
}
