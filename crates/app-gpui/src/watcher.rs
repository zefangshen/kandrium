//! External-change watcher — the gpui port of `crates/app/src/watcher.rs`.
//!
//! Polls `PRAGMA data_version` on its own connection. Only commits from
//! *other* processes bump the value (our own writes don't), so there is no
//! echo loop after a save. Bursts are coalesced with a short debounce and
//! reported as one unit over a channel.
//!
//! Nothing here touches UI state: the thread owns a connection and a sender,
//! the shell drains the receiver on the UI thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use rusqlite::Connection;

/// How often the watcher thread reads `data_version`.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// How long to wait before re-reading, to fold rapid commits into one event.
pub const DEBOUNCE: Duration = Duration::from_millis(120);

/// A running watcher thread. Dropping it stops the thread.
pub struct Watcher {
    rx: Receiver<()>,
    stop: Arc<AtomicBool>,
}

impl Watcher {
    /// Start watching `db_path`. The thread retries the open briefly, so it may
    /// be spawned while another process is still creating the file.
    pub fn spawn(db_path: PathBuf) -> Self {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        std::thread::spawn(move || watch(db_path, tx, flag));
        Self { rx, stop }
    }

    /// `true` when at least one external change arrived since the last call.
    /// Coalesces every queued event.
    pub fn drain(&self) -> bool {
        let mut seen = false;
        while self.rx.try_recv().is_ok() {
            seen = true;
        }
        seen
    }

    /// Ask the thread to exit. The flag is also set on drop.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop();
    }
}

fn watch(path: PathBuf, tx: Sender<()>, stop: Arc<AtomicBool>) {
    let db = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(db) = open_connection(&path) {
            break db;
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    let Ok(mut last) = data_version(&db) else {
        return;
    };

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(POLL_INTERVAL);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(current) = data_version(&db) else {
            continue;
        };
        if current == last {
            continue;
        }
        last = current;
        // Coalesce the commits of a burst into a single notification.
        std::thread::sleep(DEBOUNCE);
        if let Ok(value) = data_version(&db) {
            last = value;
        }
        if tx.send(()).is_err() {
            return; // the shell dropped its receiver
        }
    }
}

fn open_connection(path: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    db.busy_timeout(Duration::from_millis(5000))?;
    Ok(db)
}

fn data_version(db: &Connection) -> rusqlite::Result<i64> {
    db.query_row("PRAGMA data_version", [], |row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn seeded(dir: &Path) -> PathBuf {
        let path = dir.join("kandrium.db");
        let mut db = kandrium_core::open_db(&path).expect("open temp db");
        kandrium_core::bootstrap(&mut db, &path).expect("bootstrap temp db");
        path
    }

    /// Let the thread take its baseline reading before the external write.
    fn settle() {
        std::thread::sleep(POLL_INTERVAL + DEBOUNCE);
    }

    #[test]
    fn reports_an_external_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = seeded(dir.path());
        let watcher = Watcher::spawn(path.clone());
        settle();

        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE watcher_probe (id INTEGER);")
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if watcher.drain() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the watcher did not report an external commit");
    }

    #[test]
    fn stopped_watcher_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = seeded(dir.path());
        let watcher = Watcher::spawn(path.clone());
        settle();
        watcher.stop();

        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE watcher_probe (id INTEGER);")
            .unwrap();
        std::thread::sleep(POLL_INTERVAL * 3);
        assert!(
            !watcher.drain(),
            "a stopped watcher must not report changes"
        );
    }
}
