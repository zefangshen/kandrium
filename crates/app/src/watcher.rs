//! External-change watcher — port of `src/main/services/change-watcher.ts`.
//!
//! Polls `PRAGMA data_version` on its own connection. Only commits from
//! *other* processes bump the value (our own writes don't), so there is no
//! echo loop. Emits one event per burst through a 120 ms debounce, then asks
//! egui to repaint.

use std::sync::mpsc::Sender;
use std::time::Duration;

use rusqlite::Connection;

use kandrium_core::resolve_db_path;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const DEBOUNCE: Duration = Duration::from_millis(120);

/// Spawn the watcher thread. Sends a unit event whenever another process
/// has committed changes to the database.
pub fn spawn(tx: Sender<()>, repaint: egui::Context) {
    std::thread::spawn(move || {
        // Retry open briefly — the app may still be creating the file.
        let db = loop {
            match open_connection() {
                Ok(db) => break db,
                Err(_) => std::thread::sleep(POLL_INTERVAL),
            }
        };

        let mut last: i64 = match data_version(&db) {
            Ok(v) => v,
            Err(_) => return, // DB vanished; watcher is best-effort
        };

        loop {
            std::thread::sleep(POLL_INTERVAL);
            let Ok(current) = data_version(&db) else {
                continue;
            };
            if current != last {
                last = current;
                // Debounce: coalesce rapid successive commits into one event.
                std::thread::sleep(DEBOUNCE);
                if let Ok(v) = data_version(&db) {
                    last = v;
                }
                let _ = tx.send(());
                repaint.request_repaint();
            }
        }
    });
}

fn open_connection() -> rusqlite::Result<Connection> {
    let path = resolve_db_path();
    let db = Connection::open(path)?;
    db.busy_timeout(Duration::from_millis(5000))?;
    Ok(db)
}

fn data_version(db: &Connection) -> rusqlite::Result<i64> {
    db.query_row("PRAGMA data_version", [], |r| r.get(0))
}
