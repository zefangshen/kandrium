//! Run-scoped history store: the manifest model shared by file capture, row
//! capture, and undo.
//!
//! A run is one task-sized unit of agent or user work. Its manifest records
//! every file, directory and symlink it touched, plus the database snapshot and
//! derived table diff for its rows. The JSON manifest is authoritative; the
//! `run_history`, `run_row_changes` and `run_undo` tables are queryable mirrors
//! that can be rebuilt from it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod capture;
pub mod gc;
pub mod restore;
pub mod rows;
pub mod run;
pub mod store;
pub mod undo;

pub use capture::{CaptureGuard, RunContext, TreeCapture};
pub use restore::{FileConflict, FileUndoReport, Roots, SkippedEntry, preflight, restore_files};
pub use rows::{CAPTURED_TABLES, RowEnd, RowSession, captured_tables, row_begin};
pub use run::{
    ABANDON_AFTER_SECS, HEARTBEAT_STALE_SECS, OpenRun, begin_run, commit_run, discard_empty_run,
    end_open_run, ensure_manifest, heartbeat_run, open_run, sweep_abandoned,
};
pub use store::{HISTORY_DIR, HistoryStore};
pub use undo::{RowConflict, RowUndoReport, UndoReport, undo_run};

/// Manifest schema version.
pub const MANIFEST_VERSION: u32 = 1;
/// Per-file capture cap. Entries above it are recorded as unrestorable.
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// What opened the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunKind {
    /// `begin_wiki_run` with an agent-supplied task label.
    Explicit,
    /// One mutating MCP call with no open run.
    Implicit,
    /// One mutating `Store` action in the app.
    App,
}

impl RunKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Implicit => "implicit",
            Self::App => "app",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "explicit" => Some(Self::Explicit),
            "implicit" => Some(Self::Implicit),
            "app" => Some(Self::App),
            _ => None,
        }
    }
}

/// How a run stopped accepting changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClosedBy {
    /// `end_wiki_run` (or the compatibility close).
    Explicit,
    /// The MCP server's shutdown path.
    Exit,
    /// The 24-hour sweep after a missed heartbeat.
    Abandoned,
    /// The app's per-action wrapper.
    Action,
}

impl ClosedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Exit => "exit",
            Self::Abandoned => "abandoned",
            Self::Action => "action",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "explicit" => Some(Self::Explicit),
            "exit" => Some(Self::Exit),
            "abandoned" => Some(Self::Abandoned),
            "action" => Some(Self::Action),
            _ => None,
        }
    }
}

/// Where the run is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunState {
    /// Holds the single open slot in `run_undo`.
    Open,
    Committed,
    Undone,
    /// Files came back but the row phase did not; retry repeats rows only.
    FilesRestored,
    Failed,
    /// Undo finished, but entries marked `restorable: false` were skipped.
    Partial,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Committed => "committed",
            Self::Undone => "undone",
            Self::FilesRestored => "files-restored",
            Self::Failed => "failed",
            Self::Partial => "partial",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "committed" => Some(Self::Committed),
            "undone" => Some(Self::Undone),
            "files-restored" => Some(Self::FilesRestored),
            "failed" => Some(Self::Failed),
            "partial" => Some(Self::Partial),
            _ => None,
        }
    }
}

/// Root a captured path belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootKind {
    Wiki,
    Notes,
    Documents,
    Projects,
}

impl RootKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wiki => "wiki",
            Self::Notes => "notes",
            Self::Documents => "documents",
            Self::Projects => "projects",
        }
    }

    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "wiki" => Some(Self::Wiki),
            "notes" => Some(Self::Notes),
            "documents" => Some(Self::Documents),
            "projects" => Some(Self::Projects),
            _ => None,
        }
    }
}

/// What happened to a captured file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileOp {
    Modify,
    Create,
    Delete,
    /// A delete of the old path plus a create of the new one.
    Rename,
}

/// What happened to a captured directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DirOp {
    Mkdir,
    Rmdir,
}

/// Direction of one row change in the derived diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeOp {
    Insert,
    Update,
    Delete,
}

/// Why an entry cannot be restored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnrestorableReason {
    /// Above [`MAX_FILE_BYTES`].
    Oversize,
    /// Could not be read at capture time.
    Unreadable,
    /// FIFO, socket, device, or another non-regular entry.
    Special,
    /// The shared retention ceiling would be exceeded.
    Budget,
}

impl UnrestorableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Oversize => "oversize",
            Self::Unreadable => "unreadable",
            Self::Special => "special",
            Self::Budget => "budget",
        }
    }
}

/// Bytes and mode of one side of a file entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobMeta {
    /// Content hash; the object store key.
    pub hash: String,
    /// Unix permission bits only.
    pub mode: u32,
    pub size: u64,
}

/// One file the run touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub seq: i64,
    pub root: RootKind,
    /// Root-relative path; project files are keyed by `<project_id>/<rel>`.
    pub path: String,
    pub op: FileOp,
    /// `None` for `create`.
    pub before: Option<BlobMeta>,
    /// `None` for `delete`.
    pub after: Option<BlobMeta>,
    pub captured_at: i64,
    #[serde(default)]
    pub oversize: bool,
    #[serde(default = "default_true")]
    pub restorable: bool,
    pub reason: Option<UnrestorableReason>,
    /// True for a PDF annotation sidecar.
    #[serde(default)]
    pub sidecar: bool,
}

/// One directory the run touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub seq: i64,
    pub root: RootKind,
    pub path: String,
    pub op: DirOp,
    pub mode: u32,
    /// A file-only manifest cannot recreate empty directories, so this is
    /// recorded even when the directory holds nothing.
    #[serde(default)]
    pub empty: bool,
    pub captured_at: i64,
}

/// One symlink the run touched. Links are recorded by target, never followed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkEntry {
    pub seq: i64,
    pub root: RootKind,
    pub path: String,
    pub target: String,
    pub captured_at: i64,
}

/// Row-side metadata for a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowsMeta {
    /// `db/<run-id>-start.db`.
    pub snapshot: String,
    pub snapshot_bytes: u64,
    /// Schema version at capture time; a mismatch refuses undo.
    pub user_version: i64,
    /// `PRAGMA table_info` hash per allowlisted table.
    pub schema: BTreeMap<String, String>,
    /// `db/<run-id>-rows.json`.
    pub diff: String,
    pub counts: RowCounts,
    /// Another connection committed inside the window.
    #[serde(default)]
    pub contended: bool,
    #[serde(default = "default_true")]
    pub restorable: bool,
}

/// Change counts in a [`RowDiff`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowCounts {
    pub inserted: usize,
    pub updated: usize,
    pub deleted: usize,
}

impl RowCounts {
    /// Number of changed keys, whatever the direction.
    pub fn total(self) -> usize {
        self.inserted + self.updated + self.deleted
    }
}

/// The derived table diff stored at `db/<run-id>-rows.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowDiff {
    pub version: u32,
    pub user_version: i64,
    pub tables: BTreeMap<String, TableDiff>,
}

/// Changes for one table, keys and row hashes only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDiff {
    #[serde(default)]
    pub changes: Vec<RowChange>,
}

/// One changed primary key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowChange {
    /// Primary-key values in schema order: one for `id`, two for
    /// `(board_id, card_id)`.
    pub key: Vec<String>,
    pub op: ChangeOp,
    /// `None` for an insert.
    pub before_hash: Option<String>,
    /// `None` for a delete.
    pub after_hash: Option<String>,
}

/// One run's history manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: u32,
    pub run_id: String,
    pub kind: RunKind,
    #[serde(default)]
    pub label: String,
    pub closed_by: Option<ClosedBy>,
    pub heartbeat_at: i64,
    pub operation: Option<String>,
    pub agent: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub state: RunState,
    #[serde(default)]
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub dirs: Vec<DirEntry>,
    #[serde(default)]
    pub links: Vec<LinkEntry>,
    pub rows: Option<RowsMeta>,
}

impl Manifest {
    /// A fresh open run with no captured entries yet.
    pub fn new(
        run_id: impl Into<String>,
        kind: RunKind,
        label: impl Into<String>,
        at: i64,
    ) -> Self {
        Self {
            version: MANIFEST_VERSION,
            run_id: run_id.into(),
            kind,
            label: label.into(),
            closed_by: None,
            heartbeat_at: at,
            operation: None,
            agent: None,
            started_at: at,
            finished_at: None,
            state: RunState::Open,
            files: Vec::new(),
            dirs: Vec::new(),
            links: Vec::new(),
            rows: None,
        }
    }
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        let mut manifest = Manifest::new(
            "run1",
            RunKind::Explicit,
            "ingest the attention papers",
            1_760_000_000,
        );
        manifest.closed_by = Some(ClosedBy::Explicit);
        manifest.heartbeat_at = 1_760_000_009;
        manifest.operation = Some("compile".into());
        manifest.agent = Some("codex".into());
        manifest.finished_at = Some(1_760_000_012);
        manifest.state = RunState::Committed;
        manifest.files.push(FileEntry {
            seq: 1,
            root: RootKind::Wiki,
            path: "concepts/attention.md".into(),
            op: FileOp::Modify,
            before: Some(BlobMeta {
                hash: "old".into(),
                mode: 420,
                size: 1_234,
            }),
            after: Some(BlobMeta {
                hash: "new".into(),
                mode: 420,
                size: 1_301,
            }),
            captured_at: 1_760_000_004,
            oversize: false,
            restorable: true,
            reason: None,
            sidecar: false,
        });
        manifest.dirs.push(DirEntry {
            seq: 2,
            root: RootKind::Projects,
            path: "p1/papers".into(),
            op: DirOp::Mkdir,
            mode: 493,
            empty: true,
            captured_at: 1_760_000_004,
        });
        manifest.links.push(LinkEntry {
            seq: 3,
            root: RootKind::Projects,
            path: "p1/current".into(),
            target: "papers/2026".into(),
            captured_at: 1_760_000_004,
        });
        let mut schema = BTreeMap::new();
        schema.insert("cards".to_string(), "hash".to_string());
        manifest.rows = Some(RowsMeta {
            snapshot: "db/run1-start.db".into(),
            snapshot_bytes: 274_432,
            user_version: 13,
            schema,
            diff: "db/run1-rows.json".into(),
            counts: RowCounts {
                inserted: 1,
                updated: 5,
                deleted: 1,
            },
            contended: false,
            restorable: true,
        });
        manifest
    }

    #[test]
    fn manifest_round_trips() {
        let manifest = sample();
        let json = serde_json::to_string(&manifest).unwrap();
        let back: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, manifest);
    }

    #[test]
    fn wire_names_match_the_plan() {
        assert_eq!(
            serde_json::to_string(&RunState::FilesRestored).unwrap(),
            "\"files-restored\""
        );
        assert_eq!(serde_json::to_string(&RunKind::App).unwrap(), "\"app\"");
        assert_eq!(
            serde_json::to_string(&FileOp::Create).unwrap(),
            "\"create\""
        );
        assert_eq!(
            serde_json::to_string(&UnrestorableReason::Oversize).unwrap(),
            "\"oversize\""
        );
        assert_eq!(
            RunState::from_db("files-restored"),
            Some(RunState::FilesRestored)
        );
        assert_eq!(RootKind::from_db("projects"), Some(RootKind::Projects));
        assert_eq!(RunKind::from_db("bogus"), None);
    }

    #[test]
    fn a_fresh_manifest_is_open_and_empty() {
        let manifest = Manifest::new("r", RunKind::Implicit, "write_wiki_page", 7);
        assert_eq!(manifest.state, RunState::Open);
        assert_eq!(manifest.heartbeat_at, 7);
        assert!(manifest.files.is_empty() && manifest.dirs.is_empty() && manifest.links.is_empty());
        assert!(manifest.rows.is_none());
        // Optional fields serialize instead of being skipped, so the manifest
        // shape stays stable for readers.
        let json = serde_json::to_value(&manifest).unwrap();
        assert!(json.get("rows").is_some());
        assert_eq!(json["kind"], "implicit");
    }

    #[test]
    fn row_diff_round_trips() {
        let mut tables = BTreeMap::new();
        tables.insert(
            "cards".to_string(),
            TableDiff {
                changes: vec![RowChange {
                    key: vec!["k1".into()],
                    op: ChangeOp::Update,
                    before_hash: Some("a".into()),
                    after_hash: Some("b".into()),
                }],
            },
        );
        let diff = RowDiff {
            version: MANIFEST_VERSION,
            user_version: 13,
            tables,
        };
        let back: RowDiff = serde_json::from_str(&serde_json::to_string(&diff).unwrap()).unwrap();
        assert_eq!(back, diff);
    }
}
