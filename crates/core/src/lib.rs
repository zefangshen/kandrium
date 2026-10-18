//! Kandrium data layer — SQLite schema, services, seed, and legacy-DB import.
//!
//! Shared by the `kandrium-app` (egui) and `kandrium-mcp` (stdio MCP server)
//! binaries. All functions take `&rusqlite::Connection` and are synchronous;
//! the database runs in WAL mode so multiple processes can share one file.

pub mod constants;
pub mod db;
pub mod error;
pub mod import;
pub mod models;
pub mod notes;
pub mod seed;
pub mod services;

pub use constants::{
    ALL_BOARD_ID, ALL_COLUMN_PREFIX, CARD_STATUSES, normalize_column_title,
    status_from_column_title,
};
pub use db::{
    bootstrap, migrate_legacy_db, open_db, resolve_db_path, resolve_old_db_path, run_schema, uid,
};
pub use error::{KandriumError, Result};
pub use import::{ImportCounts, import_old_db};
pub use models::*;
pub use seed::seed;
pub use services::board::{get_all_board, get_board, list_boards};
pub use services::card::{
    create_card, delete_card, list_archived_cards, move_all_card, move_card, move_card_to_board,
    move_card_to_status, set_card_archived, update_card,
};
pub use services::column::{
    archive_all_cards, create_column, delete_column, move_column, rename_column,
};
