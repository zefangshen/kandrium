//! Kandrium data layer — SQLite schema, services, seed, and legacy-DB import.
//!
//! Shared by the `kandrium-app` (egui) and `kandrium-mcp` (stdio MCP server)
//! binaries. All functions take `&rusqlite::Connection` and are synchronous;
//! the database runs in WAL mode so multiple processes can share one file.

pub mod clocks;
pub mod constants;
pub mod db;
pub mod documents;
pub mod error;
pub mod fsutil;
pub mod import;
pub mod logging;
pub mod models;
pub mod notes;
pub mod projects;
pub mod seed;
pub mod services;

pub use clocks::{
    Clock, ClockText, Person, ProbeAmbiguity, ProbeError, Travel, add_clock, add_person, city_name,
    day_end_seconds, day_start_seconds, end_travel, extend_travel, format_clock, format_offset,
    list_all_people, list_all_travels, list_clocks, list_people, list_travels, parse_probe_time,
    placement_at, placement_for, probe_ambiguity, probe_date, relocate_person, remove_clock,
    remove_person, remove_travel, rename_person, reorder_clocks, reorder_people, resolve_zone,
    seed_defaults as seed_clocks, start_travel, system_zone,
};
pub use constants::{
    ALL_BOARD_ID, ALL_COLUMN_PREFIX, CARD_STATUSES, normalize_column_title,
    status_from_column_title,
};
pub use db::{
    backups_dir, bootstrap, migrate_legacy_db, open_db, open_db_readonly, resolve_db_path,
    resolve_old_db_path, run_schema, uid,
};
pub use error::{KandriumError, Result};
pub use import::{ImportCounts, import_old_db};
pub use models::*;
pub use projects::{
    PROJECT_FILE_EXTS, PROJECT_STATUSES, Project, ProjectChoice, ProjectFolder, ProjectNode,
    ProjectPath, ProjectSummary, ProjectTemplate, TEMPLATES, add_project_path, archive_project,
    create_doc_file, create_doc_folder, create_folder, create_project, delete_folder,
    delete_project, ensure_root, find_project, get_project, list_archived_projects,
    list_project_choices, list_project_paths, list_tree, project_dir, project_files,
    project_summary, remove_project_path, rename_folder, rename_project, reorder_project,
    resolve_projects_root, set_identifier, set_source, suggest_identifier, template,
    unarchive_project, update_project, validate_identifier,
};
pub use seed::seed;
pub use services::board::{
    create_board, get_all_board, get_board, get_board_readonly, list_boards,
};
pub use services::card::{
    create_card, create_card_in_status, delete_card, list_archived_cards, move_all_card,
    move_area_card, move_card, move_card_to_board, move_card_to_status, rename_card,
    set_card_archived, set_card_area, set_card_project, update_card,
};
pub use services::column::{
    archive_all_cards, create_column, delete_column, move_column, rename_column,
};
