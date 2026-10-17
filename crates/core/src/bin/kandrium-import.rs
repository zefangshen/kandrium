//! One-time importer: copies all data from the legacy Electron app database
//! into the Kandrium database, preserving ids and positions.
//!
//! Usage:
//!   kandrium-import [OLD_DB] [NEW_DB]
//!
//! Defaults: OLD_DB = $KANDRIUM_OLD_DB or <data_dir>/planly/planly.db (the
//! Electron app's database), NEW_DB = $KANDRIUM_DB or <data_dir>/kandrium/kandrium.db.

use std::path::PathBuf;

use kandrium_core::{import_old_db, resolve_db_path, resolve_old_db_path};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let old: PathBuf = args
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(resolve_old_db_path);
    let new: PathBuf = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(resolve_db_path);

    eprintln!("[kandrium-import] old: {}", old.display());
    eprintln!("[kandrium-import] new: {}", new.display());

    match import_old_db(&old, &new) {
        Ok(counts) => {
            eprintln!(
                "[kandrium-import] done: {} boards, {} columns, {} cards, {} all_card_order rows",
                counts.boards, counts.columns, counts.cards, counts.all_card_order
            );
        }
        Err(e) => {
            eprintln!("[kandrium-import] failed: {e}");
            std::process::exit(1);
        }
    }
}
