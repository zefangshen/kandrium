//! File logging and crash capture shared by every Kandrium binary.
//!
//! A packaged `.app` discards stderr, so startup errors, migration notes, and
//! panics are written to `<data_dir>/kandrium/logs/` (override with
//! `KANDRIUM_LOG_DIR`). Each component has its own `<component>.log`; the
//! active file is rotated to `.log.1` once it exceeds [`MAX_LOG_BYTES`].

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;

use crate::error::Result;

pub use log::{debug, error, info, warn};

/// Rotate the active log file once it reaches this size.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

static LOGGER: OnceLock<FileLogger> = OnceLock::new();

/// Resolve the log directory:
/// 1. `KANDRIUM_LOG_DIR` env var
/// 2. `<data_dir>/kandrium/logs`
pub fn log_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("KANDRIUM_LOG_DIR") {
        return PathBuf::from(path);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kandrium")
        .join("logs")
}

/// Install the global file logger for `component`. Call once at process
/// start; a second call is a no-op.
pub fn init(component: &str) -> Result<()> {
    let dir = log_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{component}.log"));
    let logger = FileLogger::new(path, component, MAX_LOG_BYTES)?;
    let installed = LOGGER.get_or_init(|| logger);
    let _ = log::set_logger(installed);
    log::set_max_level(log::LevelFilter::Info);
    Ok(())
}

/// Log panics with a backtrace and leave a one-off crash file next to the
/// rolling log. Chains the previously installed hook.
pub fn install_panic_hook(component: &str) {
    let component = component.to_string();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("panic");
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();
        error!("panic at {location}: {message}\n{backtrace}");

        let dir = log_dir();
        if fs::create_dir_all(&dir).is_ok() {
            let name = format!("crash-{}-{}.log", std::process::id(), file_stamp());
            let body = format!("{component} panic at {location}\n{message}\n\n{backtrace}\n");
            let _ = fs::write(dir.join(name), body);
        }
        previous(info);
    }));
}

/// One rolling file per component, guarded by a mutex.
struct FileLogger {
    path: PathBuf,
    component: String,
    max_bytes: u64,
    file: Mutex<File>,
}

impl FileLogger {
    fn new(path: PathBuf, component: &str, max_bytes: u64) -> Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            component: component.to_string(),
            max_bytes,
            file: Mutex::new(file),
        })
    }

    fn write_line(&self, level: log::Level, message: &str) {
        let line = format!(
            "{} {:<5} [{}] {}\n",
            timestamp(),
            level.as_str(),
            self.component,
            message
        );
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
        // Keep the previous stderr behavior (useful for CLI runs and the MCP
        // transport, which reserves stdout only).
        eprint!("{line}");
        let needs_rotation = file
            .metadata()
            .map(|meta| meta.len() >= self.max_bytes)
            .unwrap_or(false);
        drop(file);
        if needs_rotation {
            self.rotate();
        }
    }

    fn rotate(&self) {
        let rotated = self.path.with_extension("log.1");
        let _ = fs::rename(&self.path, &rotated);
        if let Ok(file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            && let Ok(mut guard) = self.file.lock()
        {
            *guard = file;
        }
    }
}

impl log::Log for FileLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        self.write_line(record.level(), &record.args().to_string());
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

fn timestamp() -> String {
    jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S").to_string()
}

fn file_stamp() -> String {
    jiff::Zoned::now().strftime("%Y%m%d-%H%M%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_logger_writes_and_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");
        let logger = FileLogger::new(path.clone(), "test", 256).unwrap();

        for i in 0..20 {
            logger.write_line(log::Level::Info, &format!("line {i}"));
        }

        let rotated = path.with_extension("log.1");
        assert!(rotated.exists(), "expected rotated file at {rotated:?}");
        assert!(path.exists(), "expected a fresh active log");
        let active = fs::read_to_string(&path).unwrap();
        assert!(active.contains("line 19"), "active log: {active}");
    }
}
