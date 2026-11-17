//! Kandrium MCP server — port of `src/mcp/server.ts` onto the official Rust
//! MCP SDK (rmcp). Exposes a curated 13-tool Kanban subset plus the additive
//! wiki read surface. Kanban names, schemas, and result shapes match the
//! legacy tools (pretty JSON text; tool-level `isError` + message on
//! failure). Column create/rename/delete/move are intentionally app-only;
//! only `archive_all_cards` remains as a column mutation.
//!
//! DB path resolution matches the core crate: `KANDRIUM_DB` env override, else
//! `<data_dir>/kandrium/kandrium.db`. Logging goes to stderr only — stdout is
//! the MCP transport.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use kandrium_core as core;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::schemars;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Serialize;

// ---------------------------------------------------------------------------
// Tool input structs (mirror the zod inputSchemas, camelCase on the wire)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct BoardIdInput {
    /// Board id
    board_id: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ColIdInput {
    /// Column id
    col_id: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct CardDataInput {
    /// Card title
    title: String,
    /// Card description (markdown-ish plain text)
    #[serde(default)]
    description: Option<String>,
    /// Due date as YYYY-MM-DD (empty string means none)
    #[serde(default)]
    due_date: Option<String>,
    /// Label color indexes (0-7)
    #[serde(default)]
    labels: Option<Vec<i64>>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct CreateCardInput {
    /// Column id
    col_id: String,
    #[serde(flatten)]
    card: CardDataInput,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct UpdateCardInput {
    /// Card id
    id: String,
    #[serde(flatten)]
    card: CardDataInput,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct MoveCardInput {
    /// Card id to move
    id: String,
    /// Destination column id
    to_col_id: String,
    /// Place the card before this card id, null/omit for end
    before_card_id: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct MoveAllCardInput {
    /// Card id to move
    id: String,
    /// Destination status bucket
    status: StatusKey,
    /// Place the card before this card id in the bucket, null/omit for end
    before_card_id: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct MoveCardToBoardInput {
    /// Card id to move
    id: String,
    /// Destination board id
    target_board_id: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct CardIdInput {
    /// Card id
    id: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct SetCardArchivedInput {
    /// Card id
    id: String,
    /// true to archive, false to unarchive
    archived: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum StatusKey {
    Todo,
    InProgress,
    Done,
}

impl StatusKey {
    fn into_core(self) -> core::CardStatus {
        match self {
            StatusKey::Todo => core::CardStatus::Todo,
            StatusKey::InProgress => core::CardStatus::InProgress,
            StatusKey::Done => core::CardStatus::Done,
        }
    }
}

// ---------------------------------------------------------------------------
// Wiki inputs
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ListWikiSourcesInput {
    /// Only sources whose path starts with this prefix
    #[serde(default)]
    path_prefix: Option<String>,
    /// Filter by compile state
    #[serde(default)]
    state: Option<String>,
    /// Maximum rows to return (default 200)
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ReadWikiSourceInput {
    /// Document-relative path, e.g. papers/attention.pdf
    path: String,
    /// First 1-based line (Markdown)
    #[serde(default)]
    start_line: Option<u32>,
    /// Last 1-based line, inclusive (Markdown)
    #[serde(default)]
    end_line: Option<u32>,
    /// First 1-based page (PDF)
    #[serde(default)]
    start_page: Option<u32>,
    /// Last 1-based page, inclusive (PDF)
    #[serde(default)]
    end_page: Option<u32>,
    /// PDF page cap per call (default 4)
    #[serde(default)]
    max_pages: Option<usize>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ListWikiPagesInput {
    /// Filter by kind: source, entity, concept, index, page
    #[serde(default)]
    kind: Option<String>,
    /// Only pages that cite this document path
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ReadWikiPageInput {
    /// Root-relative page path, e.g. concepts/self-attention.md
    path: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct SearchWikiInput {
    /// Full-text query
    query: String,
    /// pages (default). Source text is not indexed; read sources directly.
    #[serde(default)]
    scope: Option<String>,
    /// Maximum hits (default 20)
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct LintWikiInput {
    /// Limit findings to this page or folder
    #[serde(default)]
    path: Option<String>,
    /// Reserved; fixes are not implemented in this release
    #[serde(default)]
    fix: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageSummary {
    path: String,
    kind: core::PageKind,
    title: String,
    summary: String,
    aliases: Vec<String>,
    tags: Vec<String>,
    reviewed: bool,
    updated_at: i64,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct WriteWikiPageInput {
    /// Root-relative Markdown path, e.g. concepts/self-attention.md
    path: String,
    /// Full page content, including frontmatter
    content: String,
    /// Hash returned by read_wiki_page; rejects the write when the file changed
    #[serde(default)]
    expected_hash: Option<String>,
    /// Overwrite a reviewed page
    #[serde(default)]
    force: Option<bool>,
    /// Client name recorded by the caller
    #[serde(default)]
    agent: Option<String>,
    /// Value from get_wiki_instructions
    instructions_version: u32,
    /// Group this write into a history run
    #[serde(default)]
    run_id: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct PatchWikiPageInput {
    /// Root-relative Markdown path
    path: String,
    /// Exact text to replace
    old_text: String,
    /// Replacement text
    new_text: String,
    #[serde(default)]
    expected_hash: Option<String>,
    #[serde(default)]
    force: Option<bool>,
    #[serde(default)]
    agent: Option<String>,
    /// Value from get_wiki_instructions
    instructions_version: u32,
    /// Group this patch into a history run
    #[serde(default)]
    run_id: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct DeleteWikiPageInput {
    /// Root-relative Markdown path
    path: String,
    #[serde(default)]
    expected_hash: Option<String>,
    /// Delete a reviewed page
    #[serde(default)]
    force: Option<bool>,
    /// Value from get_wiki_instructions
    instructions_version: u32,
    /// Group this delete into a history run
    #[serde(default)]
    run_id: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
struct RunStatsInput {
    #[serde(default)]
    pages_created: Option<u32>,
    #[serde(default)]
    pages_updated: Option<u32>,
    #[serde(default)]
    pages_skipped: Option<u32>,
    #[serde(default)]
    sources_compiled: Option<u32>,
    #[serde(default)]
    error_count: Option<u32>,
}

impl RunStatsInput {
    fn into_core(self) -> core::RunStats {
        core::RunStats {
            pages_created: self.pages_created.unwrap_or(0),
            pages_updated: self.pages_updated.unwrap_or(0),
            pages_skipped: self.pages_skipped.unwrap_or(0),
            sources_compiled: self.sources_compiled.unwrap_or(0),
            error_count: self.error_count.unwrap_or(0),
        }
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
struct RecordRunInput {
    #[serde(default)]
    agent: Option<String>,
    /// Default: compile
    #[serde(default)]
    operation: Option<String>,
    /// Default: the current instructions version
    #[serde(default)]
    instructions_version: Option<u32>,
    #[serde(default)]
    pages: Option<Vec<String>>,
    #[serde(default)]
    sources: Option<Vec<String>>,
    #[serde(default)]
    stats: Option<RunStatsInput>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    started_at: Option<i64>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ListWikiDedupCandidatesInput {
    /// Document-relative source path, e.g. papers/attention.pdf
    source: String,
    /// Maximum candidates (default 20, capped at 50)
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(serde::Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
struct ListRunsInput {
    /// Maximum runs to return (default 50, capped at 200)
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct UndoRunInput {
    /// History run id returned by list_runs or a write
    run_id: String,
    /// Skip files that changed since the run and force row restore
    #[serde(default)]
    force: Option<bool>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct BeginWikiRunInput {
    /// Short task name shown in History, e.g. "ingest the attention papers"
    label: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
struct EndWikiRunInput {
    /// Optional note stored with the run
    #[serde(default)]
    note: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct HeartbeatWikiRunInput {
    /// Run id returned by begin_wiki_run
    run_id: String,
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WikiMode {
    Off,
    Read,
    Write,
}

impl WikiMode {
    fn from_env() -> Self {
        match std::env::var("KANDRIUM_MCP_WIKI").ok().as_deref() {
            Some("off") => Self::Off,
            Some("read") => Self::Read,
            _ => Self::Write,
        }
    }

    fn allows_read(self) -> bool {
        self != Self::Off
    }
}

#[derive(Clone)]
struct KandriumServer {
    db: std::sync::Arc<Mutex<rusqlite::Connection>>,
    wiki_root: PathBuf,
    documents_root: PathBuf,
    wiki_mode: WikiMode,
    /// The explicit run this process opened with `begin_wiki_run`, including its
    /// lazily started row snapshot. Shared with `main` so shutdown can close it.
    run: std::sync::Arc<Mutex<Option<ActiveRun>>>,
    /// Populated by the #[tool_router] macro; kept for rmcp's expected shape
    /// (the generated #[tool_handler] impl routes through the router).
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

/// One explicit run owned by this server process.
struct ActiveRun {
    run_id: String,
    label: String,
    /// Taken at the run's first row mutation, not at `begin`.
    rows: Option<core::history::RowSession>,
}

#[tool_router]
impl KandriumServer {
    fn new(db: rusqlite::Connection) -> Self {
        Self::with_roots_and_mode(
            db,
            core::resolve_wiki_root(),
            core::documents::resolve_documents_root(),
            WikiMode::from_env(),
        )
    }

    #[cfg(test)]
    fn with_roots(db: rusqlite::Connection, wiki_root: PathBuf, documents_root: PathBuf) -> Self {
        Self::with_roots_and_mode(db, wiki_root, documents_root, WikiMode::Write)
    }

    fn with_roots_and_mode(
        db: rusqlite::Connection,
        wiki_root: PathBuf,
        documents_root: PathBuf,
        wiki_mode: WikiMode,
    ) -> Self {
        KandriumServer {
            db: std::sync::Arc::new(Mutex::new(db)),
            wiki_root,
            documents_root,
            wiki_mode,
            run: std::sync::Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }

    /// Shared tool plumbing: pretty JSON on success, isError + message on
    /// failure (parity with the TS `result()` helper).
    fn run<T: serde::Serialize>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> core::Result<T>,
    ) -> Result<CallToolResult, ErrorData> {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let text = match f(&db) {
            Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|e| e.to_string()),
            Err(e) => return Ok(error_result(e.to_string())),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    /// Wiki tool plumbing with both roots and the read gate.
    fn run_wiki<T: serde::Serialize>(
        &self,
        f: impl FnOnce(&rusqlite::Connection, &Path, &Path) -> core::Result<T>,
    ) -> Result<CallToolResult, ErrorData> {
        if !self.wiki_mode.allows_read() {
            return Ok(error_result(
                "wiki tools are disabled (KANDRIUM_MCP_WIKI=off)".to_string(),
            ));
        }
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        match f(&db, &self.wiki_root, &self.documents_root) {
            Ok(value) => Ok(json_result(&value)),
            Err(error) => Ok(error_result(error.to_string())),
        }
    }

    /// Wiki write plumbing without a history run: the explicit-run boundary
    /// tools and `undo_run` use it. `KANDRIUM_MCP_WIKI=read` keeps reads
    /// available and refuses every write.
    fn with_db_write<T: serde::Serialize>(
        &self,
        f: impl FnOnce(&mut rusqlite::Connection, &Path, &Path) -> core::Result<T>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.wiki_mode {
            WikiMode::Write => {}
            WikiMode::Off => {
                return Ok(error_result(
                    "wiki tools are disabled (KANDRIUM_MCP_WIKI=off)".to_string(),
                ));
            }
            WikiMode::Read => {
                return Ok(error_result(
                    "wiki writes are disabled (KANDRIUM_MCP_WIKI=read)".to_string(),
                ));
            }
        }
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        match f(&mut db, &self.wiki_root, &self.documents_root) {
            Ok(value) => Ok(json_result(&value)),
            Err(error) => Ok(error_result(error.to_string())),
        }
    }

    /// A mutating wiki tool call: gated like a write, and history-scoped like
    /// every other mutating tool.
    fn run_wiki_mutating<T: serde::Serialize>(
        &self,
        label: String,
        requested: Option<String>,
        f: impl FnOnce(&mut rusqlite::Connection, &Path, Option<String>) -> core::Result<T>,
    ) -> Result<CallToolResult, ErrorData> {
        if self.wiki_mode != WikiMode::Write {
            let message = if self.wiki_mode == WikiMode::Read {
                "wiki writes are disabled (KANDRIUM_MCP_WIKI=read)"
            } else {
                "wiki tools are disabled (KANDRIUM_MCP_WIKI=off)"
            };
            return Ok(error_result(message.to_string()));
        }
        self.run_mutating(label, requested, |db, wiki, _, run_id| f(db, wiki, run_id))
    }

    /// Run one mutating call inside a history run.
    ///
    /// - An explicit run this process opened with `begin_wiki_run` is joined and
    ///   heartbeated; its row snapshot starts at the first mutation and lasts
    ///   until `end_wiki_run`.
    /// - Otherwise an `open` row owned by another client refuses the call.
    /// - Otherwise the call is its own run: implicit when the client passes no
    ///   `runId`, or the client's grouping id when it does.
    fn run_mutating<T: serde::Serialize>(
        &self,
        label: String,
        requested: Option<String>,
        f: impl FnOnce(&mut rusqlite::Connection, &Path, &Path, Option<String>) -> core::Result<T>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        match self.inside_run(&mut db, &label, requested, f) {
            Ok(value) => Ok(json_result(&value)),
            Err(error) => Ok(error_result(error.to_string())),
        }
    }

    fn inside_run<T>(
        &self,
        db: &mut rusqlite::Connection,
        label: &str,
        requested: Option<String>,
        f: impl FnOnce(&mut rusqlite::Connection, &Path, &Path, Option<String>) -> core::Result<T>,
    ) -> core::Result<T> {
        use core::history;
        let Some(store) = history::HistoryStore::for_connection(db) else {
            // In-memory database: nothing to restore, so skip history entirely.
            return f(db, &self.wiki_root, &self.documents_root, requested);
        };
        let mut state = self.run.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(active) = state.as_mut() {
            if let Some(requested) = &requested
                && requested != &active.run_id
            {
                return Err(core::KandriumError(format!(
                    "a run is open: {}",
                    active.label
                )));
            }
            if active.rows.is_none() {
                active.rows = Some(history::row_begin(db, &active.run_id)?);
            }
            history::heartbeat_run(db, &store, &active.run_id)?;
            let run_id = Some(active.run_id.clone());
            return f(db, &self.wiki_root, &self.documents_root, run_id);
        }

        // The slot belongs to another client, or to a previous process whose
        // heartbeat has not expired yet.
        if let Some(open) = history::open_run(db, &store)? {
            return Err(core::KandriumError(format!(
                "a run is open: {}",
                open.label
            )));
        }

        let grouped = requested.is_some();
        let run_id = requested.unwrap_or_else(core::db::uid);
        let label = if label.is_empty() {
            run_id.clone()
        } else {
            label.to_string()
        };
        history::ensure_manifest(&store, &run_id, core::history::RunKind::Implicit, &label)?;
        let session = if grouped {
            None
        } else {
            Some(history::row_begin(db, &run_id)?)
        };

        let outcome = f(
            db,
            &self.wiki_root,
            &self.documents_root,
            Some(run_id.clone()),
        );
        if let Some(session) = session
            && let Err(error) = session.end(db)
        {
            core::logging::warn!("row phase failed for implicit run {run_id}: {error}");
        }
        if outcome.is_err() && history::discard_empty_run(db, &store, &run_id).unwrap_or(false) {
            return outcome;
        }
        if let Err(error) =
            history::commit_run(db, &store, &run_id, core::history::ClosedBy::Action)
        {
            core::logging::warn!("cannot close run {run_id}: {error}");
        }
        outcome
    }

    #[tool(description = "List all Kandrium boards (id + title, ordered by position)")]
    fn list_boards(&self) -> Result<CallToolResult, ErrorData> {
        self.run(core::list_boards)
    }

    #[tool(
        description = "Load one board with all its columns and non-archived cards, ordered by position"
    )]
    fn get_board(
        &self,
        Parameters(BoardIdInput { board_id }): Parameters<BoardIdInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(|db| core::get_board_readonly(db, &board_id))
    }

    // Deliberately the writing path: the virtual All board has no read-only
    // twin and allocates its order rows under an `IMMEDIATE` transaction.
    #[tool(
        description = "Load the virtual All board: all non-archived cards aggregated into To Do / In Progress / Done buckets across every board; each card includes its boardId/boardTitle"
    )]
    fn get_all_board(&self) -> Result<CallToolResult, ErrorData> {
        self.run(core::get_all_board)
    }

    // -- Columns ------------------------------------------------------------

    #[tool(
        description = "Archive every non-archived card in a column; returns how many were archived"
    )]
    fn archive_all_cards(
        &self,
        Parameters(ColIdInput { col_id }): Parameters<ColIdInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_mutating(
            run_label("archive_all_cards", Some(&col_id)),
            None,
            |db, _, _, _| core::archive_all_cards(db, &col_id),
        )
    }

    // -- Cards --------------------------------------------------------------

    #[tool(
        description = "Create a card in a column, appended at the bottom; returns the created card"
    )]
    fn create_card(
        &self,
        Parameters(CreateCardInput { col_id, card }): Parameters<CreateCardInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_mutating(run_label("create_card", None), None, |db, _, _, _| {
            core::create_card(
                db,
                &col_id,
                &core::CreateCardBody {
                    title: card.title,
                    description: card.description,
                    due_date: card.due_date,
                    labels: card.labels,
                },
            )
        })
    }

    #[tool(
        description = "Update a card title/description/dueDate/labels (full replace of provided fields)"
    )]
    fn update_card(
        &self,
        Parameters(UpdateCardInput { id, card }): Parameters<UpdateCardInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_mutating(run_label("update_card", Some(&id)), None, |db, _, _, _| {
            core::update_card(
                db,
                &id,
                &core::CreateCardBody {
                    title: card.title,
                    description: card.description,
                    due_date: card.due_date,
                    labels: card.labels,
                },
            )
        })
    }

    #[tool(
        description = "Move a card into another column and/or position; place before beforeCardId, or at the end if omitted"
    )]
    fn move_card(
        &self,
        Parameters(MoveCardInput {
            id,
            to_col_id,
            before_card_id,
        }): Parameters<MoveCardInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_mutating(run_label("move_card", Some(&id)), None, |db, _, _, _| {
            core::move_card(db, &id, &to_col_id, before_card_id.as_deref())
        })
    }

    #[tool(
        description = "Move or reorder a card in the virtual All board: free ordering within a status bucket or across buckets; cross-bucket moves also move the card to the matching column on its home board. Place before beforeCardId, or null/omit for end of bucket."
    )]
    fn move_all_card(
        &self,
        Parameters(MoveAllCardInput {
            id,
            status,
            before_card_id,
        }): Parameters<MoveAllCardInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_mutating(
            run_label("move_all_card", Some(&id)),
            None,
            |db, _, _, _| {
                core::move_all_card(db, &id, status.into_core(), before_card_id.as_deref())
            },
        )
    }

    #[tool(
        description = "Move a card to a different board, into the column matching its current status (To Do / In Progress / Done), appended at the end. No-op when the card is already on the target board."
    )]
    fn move_card_to_board(
        &self,
        Parameters(MoveCardToBoardInput {
            id,
            target_board_id,
        }): Parameters<MoveCardToBoardInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_mutating(
            run_label("move_card_to_board", Some(&id)),
            None,
            |db, _, _, _| core::move_card_to_board(db, &id, &target_board_id),
        )
    }

    #[tool(description = "Delete a card permanently")]
    fn delete_card(
        &self,
        Parameters(CardIdInput { id }): Parameters<CardIdInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_mutating(run_label("delete_card", Some(&id)), None, |db, _, _, _| {
            core::delete_card(db, &id)
        })
    }

    #[tool(description = "Archive or unarchive a card (archived cards are hidden from the board)")]
    fn archive_card(
        &self,
        Parameters(SetCardArchivedInput { id, archived }): Parameters<SetCardArchivedInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_mutating(run_label("archive_card", Some(&id)), None, |db, _, _, _| {
            core::set_card_archived(db, &id, archived)
        })
    }

    #[tool(description = "Restore an archived card back to its column")]
    fn restore_card(
        &self,
        Parameters(CardIdInput { id }): Parameters<CardIdInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_mutating(run_label("restore_card", Some(&id)), None, |db, _, _, _| {
            core::set_card_archived(db, &id, false)
        })
    }

    #[tool(
        description = "List all archived cards across boards with board/column titles and archive timestamps"
    )]
    fn list_archived_cards(&self) -> Result<CallToolResult, ErrorData> {
        self.run(core::list_archived_cards)
    }

    // -- Wiki (read) --------------------------------------------------------

    #[tool(
        description = "List indexed wiki sources with compile state and per-state counts. Filter by path prefix or state (new/indexed/compiled/stale/failed)."
    )]
    fn list_wiki_sources(
        &self,
        Parameters(ListWikiSourcesInput {
            path_prefix,
            state,
            limit,
        }): Parameters<ListWikiSourcesInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_wiki(|db, _, _| {
            let state = match state.as_deref() {
                None => None,
                Some(value) => Some(core::SourceState::from_db(value).ok_or_else(|| {
                    core::KandriumError(format!("unknown source state: {value}"))
                })?),
            };
            let mut sources = core::list_sources(db, state)?;
            if let Some(prefix) = &path_prefix {
                sources.retain(|source| source.rel_path.starts_with(prefix));
            }
            let counts = core::source_counts(db)?;
            sources.truncate(limit.unwrap_or(200));
            Ok(serde_json::json!({ "sources": sources, "counts": counts }))
        })
    }

    #[tool(
        description = "Read a source document. Markdown is read by line, PDFs by page (maxPages, default 4); follow nextPage while truncated. Documents are never modified."
    )]
    fn read_wiki_source(
        &self,
        Parameters(input): Parameters<ReadWikiSourceInput>,
    ) -> Result<CallToolResult, ErrorData> {
        if !self.wiki_mode.allows_read() {
            return Ok(error_result(
                "wiki tools are disabled (KANDRIUM_MCP_WIKI=off)".to_string(),
            ));
        }
        let range = core::SourceRange {
            start_line: input.start_line.unwrap_or(0),
            end_line: input.end_line.unwrap_or(0),
            start_page: input.start_page.unwrap_or(0),
            end_page: input.end_page.unwrap_or(0),
        };
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        match core::read_source(
            &self.documents_root,
            &input.path,
            range,
            input.max_pages.unwrap_or(4),
        ) {
            Ok(source) => {
                if source.kind == core::SourceKind::Pdf && source.text.trim().is_empty() {
                    let _ = core::mark_source_failed(
                        &db,
                        &source.path,
                        "PDF has no readable text layer",
                    );
                    return Ok(error_result(format!(
                        "{} has no readable text layer",
                        source.path
                    )));
                }
                Ok(json_result(&source))
            }
            Err(error) => Ok(error_result(error.to_string())),
        }
    }

    #[tool(
        description = "List indexed wiki pages, optionally by kind or by the document they cite"
    )]
    fn list_wiki_pages(
        &self,
        Parameters(ListWikiPagesInput {
            kind,
            source,
            limit,
            offset,
        }): Parameters<ListWikiPagesInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_wiki(|db, _, _| {
            let kind =
                match kind.as_deref() {
                    None => None,
                    Some(value) => Some(core::PageKind::from_db(value).ok_or_else(|| {
                        core::KandriumError(format!("unknown page kind: {value}"))
                    })?),
                };
            let mut pages = core::list_wiki_pages(db, kind)?;
            if let Some(source) = &source {
                pages.retain(|page| {
                    page.sources
                        .iter()
                        .any(|cited| &cited.source_path == source)
                });
            }
            let total = pages.len();
            let offset = offset.unwrap_or(0).min(total);
            let limit = limit.unwrap_or(200);
            let summaries: Vec<PageSummary> = pages
                .into_iter()
                .skip(offset)
                .take(limit)
                .map(|page| PageSummary {
                    path: page.rel_path,
                    kind: page.kind,
                    title: page.title,
                    summary: page.summary,
                    aliases: page.aliases,
                    tags: page.tags,
                    reviewed: page.reviewed,
                    updated_at: page.updated_at,
                })
                .collect();
            Ok(serde_json::json!({ "pages": summaries, "total": total }))
        })
    }

    #[tool(
        description = "Read one wiki page: raw frontmatter, Markdown body, content hash, citations, and backlinks"
    )]
    fn read_wiki_page(
        &self,
        Parameters(ReadWikiPageInput { path }): Parameters<ReadWikiPageInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_wiki(|db, wiki, _| {
            let page = core::read_page(wiki, &path)?;
            let stored = core::get_wiki_page(db, &path)?;
            let links = core::backlinks(db, &path)?;
            let reviewed = stored.as_ref().map(|page| page.reviewed).unwrap_or(false);
            let sources = stored.map(|page| page.sources).unwrap_or_default();
            Ok(serde_json::json!({
                "path": path,
                "frontmatter": frontmatter_json(&page.frontmatter),
                "body": page.body,
                "contentHash": page.content_hash,
                "reviewed": reviewed,
                "sources": sources,
                "backlinks": links,
            }))
        })
    }

    #[tool(
        description = "Full-text search over wiki pages (BM25 with snippet). Source text is not indexed; read sources directly."
    )]
    fn search_wiki(
        &self,
        Parameters(SearchWikiInput {
            query,
            scope,
            limit,
        }): Parameters<SearchWikiInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_wiki(|db, _, _| {
            match scope.as_deref() {
                None | Some("pages") => {}
                Some("sources") => {
                    return Err(core::KandriumError(
                        "source text is not indexed; use list_wiki_sources and read_wiki_source"
                            .to_string(),
                    ));
                }
                Some(other) => {
                    return Err(core::KandriumError(format!(
                        "unknown search scope: {other}"
                    )));
                }
            }
            let hits = core::search_wiki(db, &query, limit.unwrap_or(20))?;
            Ok(serde_json::json!({ "hits": hits }))
        })
    }

    #[tool(description = "Wiki status: source and page counts, roots, and instruction version")]
    fn get_wiki_status(&self) -> Result<CallToolResult, ErrorData> {
        self.run_wiki(|db, wiki, documents| {
            let status = core::wiki_status(db)?;
            let last_run = core::latest_run(db)?;
            Ok(serde_json::json!({
                "sourceCounts": status.sources,
                "pageCounts": status.pages,
                "lastRun": last_run,
                "indexRevision": serde_json::Value::Null,
                "wikiRoot": wiki.display().to_string(),
                "documentsRoot": documents.display().to_string(),
                "instructionsVersion": core::WIKI_INSTRUCTIONS_VERSION,
            }))
        })
    }

    #[tool(
        description = "Current authoring instructions. Call this before writing pages and pass its version to every write."
    )]
    fn get_wiki_instructions(&self) -> Result<CallToolResult, ErrorData> {
        self.run_wiki(|_, _, _| {
            Ok(serde_json::json!({
                "version": core::WIKI_INSTRUCTIONS_VERSION,
                "schemaVersion": core::db::SCHEMA_VERSION,
                "instructions": core::WIKI_INSTRUCTIONS,
                "instructionsHash": core::instructions_hash(),
                "tagVocabulary": core::TAG_VOCABULARY,
            }))
        })
    }

    #[tool(
        description = "Report wiki lint findings, optionally scoped to a page or folder. Fixes are not implemented in this release."
    )]
    fn lint_wiki(
        &self,
        Parameters(LintWikiInput { path, fix }): Parameters<LintWikiInput>,
    ) -> Result<CallToolResult, ErrorData> {
        if fix == Some(true) {
            return Ok(error_result(
                "lint fixes are not implemented in this release".to_string(),
            ));
        }
        self.run_wiki(|db, wiki, documents| {
            let findings = core::lint_wiki(db, wiki, documents, path.as_deref())?;
            Ok(serde_json::json!({ "findings": findings, "fixed": [] }))
        })
    }

    // -- Wiki (write) -------------------------------------------------------

    #[tool(
        description = "Create or replace a wiki page. Validates frontmatter, protects reviewed pages, and reindexes atomically. Read get_wiki_instructions first."
    )]
    fn write_wiki_page(
        &self,
        Parameters(input): Parameters<WriteWikiPageInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let label = run_label("write_wiki_page", Some(&input.path));
        self.run_wiki_mutating(label, input.run_id.clone(), |db, wiki, run_id| {
            core::write_page(
                db,
                wiki,
                &core::WritePageSpec {
                    rel_path: input.path,
                    content: input.content,
                    expected_hash: input.expected_hash,
                    force: input.force.unwrap_or(false),
                    agent: input.agent,
                    instructions_version: input.instructions_version,
                    run_id,
                },
            )
        })
    }

    #[tool(
        description = "Replace exact text in an existing wiki page, then revalidate and reindex it"
    )]
    fn patch_wiki_page(
        &self,
        Parameters(input): Parameters<PatchWikiPageInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let label = run_label("patch_wiki_page", Some(&input.path));
        self.run_wiki_mutating(label, input.run_id.clone(), |db, wiki, run_id| {
            core::patch_page(
                db,
                wiki,
                &core::PatchPageSpec {
                    rel_path: input.path,
                    old_text: input.old_text,
                    new_text: input.new_text,
                    expected_hash: input.expected_hash,
                    force: input.force.unwrap_or(false),
                    agent: input.agent,
                    instructions_version: input.instructions_version,
                    run_id,
                },
            )
        })
    }

    #[tool(description = "Delete a wiki page and its index rows. Reviewed pages need force.")]
    fn delete_wiki_page(
        &self,
        Parameters(input): Parameters<DeleteWikiPageInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let label = run_label("delete_wiki_page", Some(&input.path));
        self.run_wiki_mutating(label, input.run_id.clone(), |db, wiki, run_id| {
            let path = input.path;
            core::delete_page(
                db,
                wiki,
                &core::DeletePageSpec {
                    rel_path: path.clone(),
                    expected_hash: input.expected_hash,
                    force: input.force.unwrap_or(false),
                    instructions_version: input.instructions_version,
                    run_id,
                },
            )?;
            Ok(serde_json::json!({ "path": path, "deleted": true }))
        })
    }

    #[tool(
        description = "Open an explicit task run: every wiki and Kanban write until end_wiki_run is undoable as one unit. Returns the run id."
    )]
    fn begin_wiki_run(
        &self,
        Parameters(BeginWikiRunInput { label }): Parameters<BeginWikiRunInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.with_db_write(|db, _, _| {
            use core::history;
            let store = history_store(db)?;
            let mut state = self.run.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(active) = state.as_ref() {
                return Err(core::KandriumError(format!(
                    "a run is open: {}",
                    active.label
                )));
            }
            let manifest = history::begin_run(db, &store, &label)?;
            let response = serde_json::json!({
                "runId": manifest.run_id,
                "label": manifest.label,
                "state": manifest.state.as_str(),
                "startedAt": manifest.started_at,
            });
            *state = Some(ActiveRun {
                run_id: manifest.run_id,
                label: manifest.label,
                rows: None,
            });
            Ok(response)
        })
    }

    #[tool(
        description = "Close the open run: takes its end snapshot, derives the row diff, and makes it undoable. Optional note is stored with the run."
    )]
    fn end_wiki_run(
        &self,
        Parameters(EndWikiRunInput { note }): Parameters<EndWikiRunInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.with_db_write(|db, _, _| {
            use core::history;
            let store = history_store(db)?;
            let mut state = self.run.lock().unwrap_or_else(|e| e.into_inner());
            let Some(active) = state.take() else {
                return Err(core::KandriumError::new("no run is open"));
            };
            if let Some(session) = active.rows
                && let Err(error) = session.end(db)
            {
                core::logging::warn!("row phase failed for run {}: {error}", active.run_id);
            }
            let manifest = history::end_open_run(
                db,
                &store,
                &active.run_id,
                history::ClosedBy::Explicit,
                note.as_deref(),
            )?;
            Ok(RunSummary::from(&manifest))
        })
    }

    #[tool(
        description = "Advance the open run's heartbeat so it stays reclaimable for another 10 minutes; call it between long stretches of work outside the run."
    )]
    fn heartbeat_wiki_run(
        &self,
        Parameters(HeartbeatWikiRunInput { run_id }): Parameters<HeartbeatWikiRunInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.with_db_write(|db, _, _| {
            use core::history;
            let store = history_store(db)?;
            let heartbeat_at = history::heartbeat_run(db, &store, &run_id)?;
            Ok(serde_json::json!({ "runId": run_id, "heartbeatAt": heartbeat_at }))
        })
    }

    #[tool(
        description = "Record a compile run and mark its sources compiled. Call once after a batch of page writes. Closes an open run and records these stats with it."
    )]
    fn record_wiki_run(
        &self,
        Parameters(input): Parameters<RecordRunInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.with_db_write(|db, _, _| {
            use core::history;
            if let Some(store) = history::HistoryStore::for_connection(db) {
                let mut state = self.run.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(active) = state.take() {
                    if let Some(session) = active.rows
                        && let Err(error) = session.end(db)
                    {
                        core::logging::warn!("row phase failed for run {}: {error}", active.run_id);
                    }
                    if let Err(error) = history::end_open_run(
                        db,
                        &store,
                        &active.run_id,
                        history::ClosedBy::Explicit,
                        None,
                    ) {
                        core::logging::warn!("cannot close run {}: {error}", active.run_id);
                    }
                }
            }
            core::record_run(
                db,
                core::RunRecord {
                    agent: input.agent,
                    operation: input.operation,
                    instructions_version: input.instructions_version,
                    pages: input.pages.unwrap_or_default(),
                    sources: input.sources.unwrap_or_default(),
                    stats: input.stats.unwrap_or_default().into_core(),
                    note: input.note,
                    error: input.error,
                    started_at: input.started_at,
                },
            )
        })
    }

    #[tool(description = "Existing pages ranked as merge candidates for a source before writing")]
    fn list_wiki_dedup_candidates(
        &self,
        Parameters(input): Parameters<ListWikiDedupCandidatesInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = input.limit.unwrap_or(20).min(50);
        self.run_wiki(|db, _, _| {
            let candidates = core::dedup_candidates(db, &input.source, limit)?;
            Ok(serde_json::json!({ "candidates": candidates }))
        })
    }

    #[tool(
        description = "List recorded history runs, newest first, with file, directory and row change counts"
    )]
    fn list_runs(
        &self,
        Parameters(input): Parameters<ListRunsInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = input.limit.unwrap_or(50).min(200);
        self.run_wiki(|db, _, _| {
            let store = history_store(db)?;
            let mut manifests = store.load_all()?;
            manifests.sort_by(|a, b| {
                b.started_at
                    .cmp(&a.started_at)
                    .then_with(|| b.run_id.cmp(&a.run_id))
            });
            let runs: Vec<RunSummary> =
                manifests.iter().take(limit).map(RunSummary::from).collect();
            Ok(serde_json::json!({ "runs": runs }))
        })
    }

    #[tool(
        description = "Undo one history run: restores its files, directories and rows. Returns conflicts instead of changing anything when the run cannot be undone without force."
    )]
    fn undo_run(
        &self,
        Parameters(input): Parameters<UndoRunInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let roots = self.restore_roots();
        self.with_db_write(|db, _, _| {
            let store = history_store(db)?;
            let report = core::history::undo_run(
                db,
                &store,
                &roots,
                &input.run_id,
                input.force.unwrap_or(false),
            )?;
            Ok(undo_json(&report))
        })
    }

    /// Roots restored paths are re-joined under: the server's wiki/documents
    /// roots (so tests and overrides match) and the default notes/projects.
    fn restore_roots(&self) -> core::history::Roots {
        let mut roots = core::history::Roots::resolve();
        roots.wiki = self.wiki_root.clone();
        roots.documents = self.documents_root.clone();
        roots
    }
}

fn history_store(db: &rusqlite::Connection) -> core::Result<core::history::HistoryStore> {
    core::history::HistoryStore::for_connection(db)
        .ok_or_else(|| core::KandriumError::new("history needs a file-backed database"))
}

/// Label for a mutating call's implicit run: `tool target`, as the History
/// panel groups it.
fn run_label(tool: &str, target: Option<&str>) -> String {
    match target {
        Some(target) if !target.is_empty() => format!("{tool} {target}"),
        _ => tool.to_string(),
    }
}

fn undo_json(report: &core::history::UndoReport) -> serde_json::Value {
    let skipped: Vec<serde_json::Value> = report
        .files
        .skipped
        .iter()
        .map(|entry| serde_json::json!({ "path": entry.path, "reason": entry.reason }))
        .collect();
    serde_json::json!({
        "runId": report.run_id,
        "state": report.state.as_str(),
        "blocked": report.blocked,
        "conflicts": report.conflicts,
        "files": {
            "restored": report.files.restored,
            "deleted": report.files.deleted,
            "dirsRecreated": report.files.dirs_recreated,
            "dirsRemoved": report.files.dirs_removed,
            "skipped": skipped,
            "orphans": report.files.orphans,
        },
        "rows": {
            "updated": report.rows.updated,
            "inserted": report.rows.inserted,
            "deleted": report.rows.deleted,
            "note": report.rows.note,
        },
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunSummary {
    run_id: String,
    label: String,
    kind: String,
    state: String,
    undoable: bool,
    started_at: i64,
    finished_at: Option<i64>,
    heartbeat_at: i64,
    closed_by: Option<String>,
    operation: Option<String>,
    agent: Option<String>,
    files: usize,
    dirs: usize,
    links: usize,
    unrestorable: Vec<serde_json::Value>,
    rows: Option<serde_json::Value>,
}

impl From<&core::history::Manifest> for RunSummary {
    fn from(manifest: &core::history::Manifest) -> Self {
        let unrestorable = manifest
            .files
            .iter()
            .filter(|entry| !entry.restorable)
            .map(|entry| {
                serde_json::json!({
                    "path": entry.path,
                    "reason": entry.reason.map(|reason| reason.as_str()),
                })
            })
            .collect();
        let rows = manifest.rows.as_ref().map(|rows| {
            serde_json::json!({
                "inserted": rows.counts.inserted,
                "updated": rows.counts.updated,
                "deleted": rows.counts.deleted,
                "contended": rows.contended,
                "restorable": rows.restorable,
            })
        });
        let undoable = matches!(
            manifest.state,
            core::history::RunState::Committed | core::history::RunState::FilesRestored
        );
        Self {
            run_id: manifest.run_id.clone(),
            label: manifest.label.clone(),
            kind: manifest.kind.as_str().to_string(),
            state: manifest.state.as_str().to_string(),
            undoable,
            started_at: manifest.started_at,
            finished_at: manifest.finished_at,
            heartbeat_at: manifest.heartbeat_at,
            closed_by: manifest.closed_by.map(|by| by.as_str().to_string()),
            operation: manifest.operation.clone(),
            agent: manifest.agent.clone(),
            files: manifest.files.len(),
            dirs: manifest.dirs.len(),
            links: manifest.links.len(),
            unrestorable,
            rows,
        }
    }
}

fn json_result<T: Serialize>(value: &T) -> CallToolResult {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|error| error.to_string());
    CallToolResult::success(vec![ContentBlock::text(text)])
}

fn frontmatter_json(frontmatter: &core::Frontmatter) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for field in frontmatter.fields() {
        let value = match &field.value {
            core::FmValue::Scalar(value) => serde_json::Value::String(value.clone()),
            core::FmValue::List(items) => {
                serde_json::Value::Array(items.iter().cloned().map(Into::into).collect())
            }
        };
        map.insert(field.key.clone(), value);
    }
    serde_json::Value::Object(map)
}

fn error_result(message: String) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(message)]);
    result.is_error = Some(true);
    result
}

#[tool_handler]
impl ServerHandler for KandriumServer {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        rmcp::model::ServerConfig::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "kandrium",
            env!("CARGO_PKG_VERSION"),
        ))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = core::logging::init("kandrium-mcp");
    core::logging::install_panic_hook("kandrium-mcp");

    let db_path = core::resolve_db_path();
    if let Ok(Some(from)) = core::migrate_legacy_db() {
        core::logging::info!(
            "migrated database from {} to {}",
            from.display(),
            db_path.display()
        );
    }
    let mut db = core::open_db(&db_path)?;
    core::bootstrap(&mut db, &db_path)?;
    if let Some(store) = core::history::HistoryStore::for_connection(&db) {
        match core::history::sweep_abandoned(&db, &store) {
            Ok(abandoned) if !abandoned.is_empty() => core::logging::info!(
                "abandoned {} run(s) without a heartbeat for 24 hours",
                abandoned.len()
            ),
            Ok(_) => {}
            Err(error) => core::logging::warn!("run sweep failed: {error}"),
        }
        match core::history::gc::prune(
            &db,
            &store,
            core::history::gc::DEFAULT_RETENTION,
            core::history::run::now(),
        ) {
            Ok(report) if !report.pruned.is_empty() => core::logging::info!(
                "pruned {} history run(s), freed {} bytes",
                report.pruned.len(),
                report.freed_bytes
            ),
            Ok(_) => {}
            Err(error) => core::logging::warn!("history prune failed: {error}"),
        }
    }
    core::logging::info!("DB: {}", db_path.display());

    let server = KandriumServer::new(db);
    let db_handle = server.db.clone();
    let run_handle = server.run.clone();
    let service = server.serve(rmcp::transport::stdio()).await?;
    let result = service.waiting().await;
    close_open_run(&db_handle, &run_handle);
    result?;
    Ok(())
}

/// Close this process's explicit run when the transport ends. A run that
/// cannot be closed stays open and a later client reclaims it.
fn close_open_run(
    db: &std::sync::Arc<Mutex<rusqlite::Connection>>,
    run: &std::sync::Arc<Mutex<Option<ActiveRun>>>,
) {
    let Some(active) = run.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let db = db.lock().unwrap_or_else(|e| e.into_inner());
    let Some(store) = core::history::HistoryStore::for_connection(&db) else {
        return;
    };
    if let Some(session) = active.rows
        && let Err(error) = session.end(&db)
    {
        core::logging::warn!("row phase failed for run {}: {error}", active.run_id);
    }
    if let Err(error) = core::history::end_open_run(
        &db,
        &store,
        &active.run_id,
        core::history::ClosedBy::Exit,
        None,
    ) {
        core::logging::warn!("cannot close run {} on exit: {error}", active.run_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_router_exposes_the_curated_set() {
        let server = KandriumServer::new(rusqlite::Connection::open_in_memory().unwrap());
        let mut names: Vec<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        names.sort();

        let expected = [
            "archive_all_cards",
            "archive_card",
            "begin_wiki_run",
            "create_card",
            "delete_card",
            "delete_wiki_page",
            "end_wiki_run",
            "get_all_board",
            "get_board",
            "get_wiki_instructions",
            "get_wiki_status",
            "heartbeat_wiki_run",
            "lint_wiki",
            "list_archived_cards",
            "list_boards",
            "list_runs",
            "list_wiki_dedup_candidates",
            "list_wiki_pages",
            "list_wiki_sources",
            "move_all_card",
            "move_card",
            "move_card_to_board",
            "patch_wiki_page",
            "read_wiki_page",
            "read_wiki_source",
            "record_wiki_run",
            "restore_card",
            "search_wiki",
            "undo_run",
            "update_card",
            "write_wiki_page",
        ];
        let expected: Vec<String> = expected.iter().map(|name| name.to_string()).collect();
        assert_eq!(names, expected);
    }

    fn page(title: &str, body: &str) -> String {
        format!(
            "---\ntype: concept\ntitle: {title}\nsummary: A page about {title}.\n\
             aliases: [{title}]\nreviewed: false\n---\n{body}\n"
        )
    }

    fn file_backed_server(
        wiki: &std::path::Path,
        documents: &std::path::Path,
        db_path: &std::path::Path,
    ) -> KandriumServer {
        let mut db = core::open_db(db_path).unwrap();
        core::bootstrap(&mut db, db_path).unwrap();
        KandriumServer::with_roots(db, wiki.to_path_buf(), documents.to_path_buf())
    }

    #[test]
    fn wiki_writes_group_into_one_run_and_undo_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("wiki");
        let documents = dir.path().join("documents");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::create_dir_all(&documents).unwrap();
        let server = file_backed_server(&wiki, &documents, &dir.path().join("kandrium.db"));

        let path = "concepts/attention.md";
        let original = page("Attention", "Original body.");
        // Written outside any run: the run's undo must bring this back.
        std::fs::create_dir_all(wiki.join("concepts")).unwrap();
        std::fs::write(wiki.join(path), &original).unwrap();

        server
            .write_wiki_page(Parameters(WriteWikiPageInput {
                path: path.into(),
                content: page("Attention", "Middle body."),
                expected_hash: None,
                force: None,
                agent: None,
                instructions_version: core::WIKI_INSTRUCTIONS_VERSION,
                run_id: Some("run1".into()),
            }))
            .unwrap();
        server
            .patch_wiki_page(Parameters(PatchWikiPageInput {
                path: path.into(),
                old_text: "Middle body.".into(),
                new_text: "Final body.".into(),
                expected_hash: None,
                force: None,
                agent: None,
                instructions_version: core::WIKI_INSTRUCTIONS_VERSION,
                run_id: Some("run1".into()),
            }))
            .unwrap();
        server
            .delete_wiki_page(Parameters(DeleteWikiPageInput {
                path: path.into(),
                expected_hash: None,
                force: None,
                instructions_version: core::WIKI_INSTRUCTIONS_VERSION,
                run_id: Some("run1".into()),
            }))
            .unwrap();
        assert!(!wiki.join(path).exists());

        let listed = server
            .list_runs(Parameters(ListRunsInput { limit: None }))
            .unwrap();
        assert_ne!(listed.is_error, Some(true));
        let payload: serde_json::Value = serde_json::from_str(&text(&listed)).unwrap();
        let runs = payload["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 1, "{payload}");
        assert_eq!(runs[0]["runId"], "run1");
        assert_eq!(runs[0]["state"], "committed");
        // Rewrite + patch + delete on one path: one entry per operation.
        assert_eq!(runs[0]["files"], 3);

        let undone = server
            .undo_run(Parameters(UndoRunInput {
                run_id: "run1".into(),
                force: None,
            }))
            .unwrap();
        assert_ne!(undone.is_error, Some(true));
        let payload: serde_json::Value = serde_json::from_str(&text(&undone)).unwrap();
        assert_eq!(payload["state"], "undone", "{payload}");
        assert_eq!(payload["blocked"], false);
        assert_eq!(std::fs::read_to_string(wiki.join(path)).unwrap(), original);
    }

    #[test]
    fn explicit_runs_group_writes_under_one_label() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("wiki");
        let documents = dir.path().join("documents");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::create_dir_all(&documents).unwrap();
        let server = file_backed_server(&wiki, &documents, &dir.path().join("kandrium.db"));

        let begun = json(
            &server
                .begin_wiki_run(Parameters(BeginWikiRunInput {
                    label: "ingest the attention papers".into(),
                }))
                .unwrap(),
        );
        assert_eq!(begun["state"], "open");
        let run_id = begun["runId"].as_str().unwrap().to_string();

        let nested = server
            .begin_wiki_run(Parameters(BeginWikiRunInput {
                label: "nested task".into(),
            }))
            .unwrap();
        assert_eq!(nested.is_error, Some(true));
        assert!(text(&nested).contains("a run is open: ingest the attention papers"));

        for name in ["one", "two", "three"] {
            let written = server
                .write_wiki_page(Parameters(WriteWikiPageInput {
                    path: format!("concepts/{name}.md"),
                    content: page(name, "Body."),
                    expected_hash: None,
                    force: None,
                    agent: None,
                    instructions_version: core::WIKI_INSTRUCTIONS_VERSION,
                    run_id: None,
                }))
                .unwrap();
            assert_ne!(written.is_error, Some(true), "{}", text(&written));
        }

        let ended = json(
            &server
                .end_wiki_run(Parameters(EndWikiRunInput {
                    note: Some("three pages".into()),
                }))
                .unwrap(),
        );
        assert_eq!(ended["runId"], run_id.as_str());
        assert_eq!(ended["state"], "committed");
        assert_eq!(ended["closedBy"], "explicit");
        assert_eq!(ended["files"], 3);

        let listed = json(
            &server
                .list_runs(Parameters(ListRunsInput { limit: None }))
                .unwrap(),
        );
        let runs = listed["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 1, "{listed}");
        assert_eq!(runs[0]["label"], "ingest the attention papers");
        assert_eq!(runs[0]["undoable"], true);

        let undone = json(
            &server
                .undo_run(Parameters(UndoRunInput {
                    run_id: run_id.clone(),
                    force: None,
                }))
                .unwrap(),
        );
        assert_eq!(undone["state"], "undone", "{undone}");
        for name in ["one", "two", "three"] {
            assert!(!wiki.join(format!("concepts/{name}.md")).exists());
        }
    }

    #[test]
    fn without_begin_each_mutating_call_is_its_own_run() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("wiki");
        let documents = dir.path().join("documents");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::create_dir_all(&documents).unwrap();
        let server = file_backed_server(&wiki, &documents, &dir.path().join("kandrium.db"));

        for name in ["one", "two", "three"] {
            let written = server
                .write_wiki_page(Parameters(WriteWikiPageInput {
                    path: format!("concepts/{name}.md"),
                    content: page(name, "Body."),
                    expected_hash: None,
                    force: None,
                    agent: None,
                    instructions_version: core::WIKI_INSTRUCTIONS_VERSION,
                    run_id: None,
                }))
                .unwrap();
            assert_ne!(written.is_error, Some(true), "{}", text(&written));
        }

        let listed = json(
            &server
                .list_runs(Parameters(ListRunsInput { limit: None }))
                .unwrap(),
        );
        let runs = listed["runs"].as_array().unwrap().clone();
        assert_eq!(runs.len(), 3, "{listed}");
        for run in &runs {
            assert_eq!(run["kind"], "implicit");
            assert_eq!(run["undoable"], true);
            assert_eq!(run["files"], 1);
        }
        assert!(
            runs.iter()
                .any(|run| run["label"] == "write_wiki_page concepts/two.md")
        );

        // Read-only calls never open a run.
        server
            .list_wiki_pages(Parameters(ListWikiPagesInput {
                kind: None,
                source: None,
                limit: None,
                offset: None,
            }))
            .unwrap();
        let listed = json(
            &server
                .list_runs(Parameters(ListRunsInput { limit: None }))
                .unwrap(),
        );
        assert_eq!(listed["runs"].as_array().unwrap().len(), 3);

        let middle = runs
            .iter()
            .find(|run| run["label"] == "write_wiki_page concepts/two.md")
            .unwrap()["runId"]
            .as_str()
            .unwrap()
            .to_string();
        let undone = json(
            &server
                .undo_run(Parameters(UndoRunInput {
                    run_id: middle,
                    force: None,
                }))
                .unwrap(),
        );
        assert_eq!(undone["state"], "undone", "{undone}");
        assert!(!wiki.join("concepts/two.md").exists());
        assert!(wiki.join("concepts/one.md").exists());
        assert!(wiki.join("concepts/three.md").exists());
    }

    #[test]
    fn a_second_client_is_refused_while_a_run_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("wiki");
        let documents = dir.path().join("documents");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::create_dir_all(&documents).unwrap();
        let db_path = dir.path().join("kandrium.db");
        let first = file_backed_server(&wiki, &documents, &db_path);
        let second = file_backed_server(&wiki, &documents, &db_path);

        first
            .begin_wiki_run(Parameters(BeginWikiRunInput {
                label: "first task".into(),
            }))
            .unwrap();

        let refused = second
            .write_wiki_page(Parameters(WriteWikiPageInput {
                path: "concepts/other.md".into(),
                content: page("Other", "Body."),
                expected_hash: None,
                force: None,
                agent: None,
                instructions_version: core::WIKI_INSTRUCTIONS_VERSION,
                run_id: None,
            }))
            .unwrap();
        assert_eq!(refused.is_error, Some(true));
        assert!(text(&refused).contains("a run is open: first task"));

        let refused_begin = second
            .begin_wiki_run(Parameters(BeginWikiRunInput {
                label: "second task".into(),
            }))
            .unwrap();
        assert!(text(&refused_begin).contains("a run is open: first task"));

        // Read-only tools stay available.
        assert_ne!(
            second
                .list_runs(Parameters(ListRunsInput { limit: None }))
                .unwrap()
                .is_error,
            Some(true)
        );
    }

    #[test]
    fn a_stale_open_run_is_reclaimed_by_another_client() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("wiki");
        let documents = dir.path().join("documents");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::create_dir_all(&documents).unwrap();
        let db_path = dir.path().join("kandrium.db");
        let owner = file_backed_server(&wiki, &documents, &db_path);

        let begun = json(
            &owner
                .begin_wiki_run(Parameters(BeginWikiRunInput {
                    label: "killed task".into(),
                }))
                .unwrap(),
        );
        let stale = begun["runId"].as_str().unwrap().to_string();
        age_heartbeat(&db_path, &stale, core::history::HEARTBEAT_STALE_SECS + 1);

        let reclaimer = file_backed_server(&wiki, &documents, &db_path);
        let taken = json(
            &reclaimer
                .begin_wiki_run(Parameters(BeginWikiRunInput {
                    label: "reclaimer".into(),
                }))
                .unwrap(),
        );
        assert_eq!(taken["state"], "open");

        let refused = reclaimer
            .end_wiki_run(Parameters(EndWikiRunInput::default()))
            .unwrap();
        assert_ne!(refused.is_error, Some(true));
        let stale_end = owner
            .end_wiki_run(Parameters(EndWikiRunInput::default()))
            .unwrap();
        assert_eq!(stale_end.is_error, Some(true));
        assert!(text(&stale_end).contains("reclaimed"));

        let listed = json(
            &reclaimer
                .list_runs(Parameters(ListRunsInput { limit: None }))
                .unwrap(),
        );
        let stale_run = listed["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["runId"] == stale.as_str())
            .unwrap()
            .clone();
        assert_eq!(stale_run["state"], "committed");
        assert_eq!(stale_run["closedBy"], "abandoned");
    }

    fn age_heartbeat(db_path: &Path, run_id: &str, seconds: i64) {
        let db = rusqlite::Connection::open(db_path).unwrap();
        db.execute(
            "UPDATE run_undo SET last_heartbeat_at = last_heartbeat_at - ?2 WHERE run_id = ?1",
            rusqlite::params![run_id, seconds],
        )
        .unwrap();
    }

    fn json(result: &CallToolResult) -> serde_json::Value {
        serde_json::from_str(&text(result)).unwrap()
    }

    #[test]
    fn read_only_mode_refuses_writes_and_undo_but_allows_listing() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("wiki");
        let documents = dir.path().join("documents");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::create_dir_all(&documents).unwrap();
        let db_path = dir.path().join("kandrium.db");
        let mut db = core::open_db(&db_path).unwrap();
        core::bootstrap(&mut db, &db_path).unwrap();
        let server = KandriumServer::with_roots_and_mode(db, wiki, documents, WikiMode::Read);

        let write = server
            .write_wiki_page(Parameters(WriteWikiPageInput {
                path: "concepts/attention.md".into(),
                content: page("Attention", "Body."),
                expected_hash: None,
                force: None,
                agent: None,
                instructions_version: core::WIKI_INSTRUCTIONS_VERSION,
                run_id: None,
            }))
            .unwrap();
        assert_eq!(write.is_error, Some(true));
        assert!(text(&write).contains("wiki writes are disabled"));

        let undo = server
            .undo_run(Parameters(UndoRunInput {
                run_id: "run1".into(),
                force: None,
            }))
            .unwrap();
        assert_eq!(undo.is_error, Some(true));
        assert!(text(&undo).contains("wiki writes are disabled"));

        let listed = server
            .list_runs(Parameters(ListRunsInput { limit: None }))
            .unwrap();
        assert_ne!(listed.is_error, Some(true));
    }

    fn text(result: &CallToolResult) -> String {
        match result.content.first() {
            Some(ContentBlock::Text(content)) => content.text.clone(),
            _ => panic!("expected a text result"),
        }
    }

    #[test]
    fn wiki_source_reads_are_scoped_to_the_documents_root() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("wiki");
        let documents = dir.path().join("documents");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::create_dir_all(&documents).unwrap();
        std::fs::write(documents.join("notes.md"), "one\ntwo\nthree\n").unwrap();

        let server = KandriumServer::with_roots(
            rusqlite::Connection::open_in_memory().unwrap(),
            wiki,
            documents,
        );

        let result = server
            .read_wiki_source(Parameters(ReadWikiSourceInput {
                path: "notes.md".into(),
                start_line: Some(2),
                end_line: Some(3),
                start_page: None,
                end_page: None,
                max_pages: None,
            }))
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        let payload: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(payload["text"], "two\nthree");
        assert_eq!(payload["startLine"], 2);

        let escaped = server
            .read_wiki_source(Parameters(ReadWikiSourceInput {
                path: "../outside.md".into(),
                start_line: None,
                end_line: None,
                start_page: None,
                end_page: None,
                max_pages: None,
            }))
            .unwrap();
        assert_eq!(escaped.is_error, Some(true));
    }

    #[test]
    fn wiki_instructions_and_gate_are_served() {
        let server = KandriumServer::with_roots(
            rusqlite::Connection::open_in_memory().unwrap(),
            PathBuf::from("/tmp/wiki"),
            PathBuf::from("/tmp/documents"),
        );
        let result = server.get_wiki_instructions().unwrap();
        assert_ne!(result.is_error, Some(true));
        let payload: serde_json::Value = serde_json::from_str(&text(&result)).unwrap();
        assert_eq!(payload["version"], 1);
        assert!(
            payload["instructions"]
                .as_str()
                .unwrap()
                .contains("write_wiki_page")
        );
        assert!(!payload["tagVocabulary"].as_array().unwrap().is_empty());

        let off = KandriumServer::with_roots_and_mode(
            rusqlite::Connection::open_in_memory().unwrap(),
            PathBuf::from("/tmp/wiki"),
            PathBuf::from("/tmp/documents"),
            WikiMode::Off,
        );
        let denied = off.get_wiki_status().unwrap();
        assert_eq!(denied.is_error, Some(true));
    }

    #[test]
    fn wiki_write_pipeline_is_optimistic_and_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("wiki");
        let documents = dir.path().join("documents");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::create_dir_all(&documents).unwrap();
        let db_path = dir.path().join("kandrium.db");
        let mut db = core::db::open_db(&db_path).unwrap();
        core::db::bootstrap(&mut db, &db_path).unwrap();
        let server = KandriumServer::with_roots(db, wiki.clone(), documents);

        let content = "---\ntype: concept\ntitle: Attention\nsummary: A shared idea.\n\
                       aliases: [attention]\n---\n\
                       Attention relates every token to every other token in a sequence.\n";

        let created = server
            .write_wiki_page(Parameters(WriteWikiPageInput {
                path: "concepts/attention.md".into(),
                content: content.into(),
                expected_hash: None,
                force: None,
                agent: Some("codex".into()),
                instructions_version: core::WIKI_INSTRUCTIONS_VERSION,
                run_id: None,
            }))
            .unwrap();
        assert_ne!(created.is_error, Some(true));
        let payload: serde_json::Value = serde_json::from_str(&text(&created)).unwrap();
        assert_eq!(payload["action"], "created");
        let hash = payload["contentHash"].as_str().unwrap().to_string();
        assert!(wiki.join("concepts/attention.md").exists());

        let conflict = server
            .write_wiki_page(Parameters(WriteWikiPageInput {
                path: "concepts/attention.md".into(),
                content: content.into(),
                expected_hash: Some("deadbeef".into()),
                force: None,
                agent: None,
                instructions_version: core::WIKI_INSTRUCTIONS_VERSION,
                run_id: None,
            }))
            .unwrap();
        assert_eq!(conflict.is_error, Some(true));

        let read = server
            .read_wiki_page(Parameters(ReadWikiPageInput {
                path: "concepts/attention.md".into(),
            }))
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&text(&read)).unwrap();
        assert_eq!(payload["frontmatter"]["title"], "Attention");
        assert_eq!(payload["contentHash"], hash);

        let patched = server
            .patch_wiki_page(Parameters(PatchWikiPageInput {
                path: "concepts/attention.md".into(),
                old_text: "A shared idea.".into(),
                new_text: "A refined idea.".into(),
                expected_hash: Some(hash),
                force: None,
                agent: Some("codex".into()),
                instructions_version: core::WIKI_INSTRUCTIONS_VERSION,
                run_id: None,
            }))
            .unwrap();
        assert_ne!(patched.is_error, Some(true));
        let payload: serde_json::Value = serde_json::from_str(&text(&patched)).unwrap();
        assert_eq!(payload["replaced"], 1);

        let run = server
            .record_wiki_run(Parameters(RecordRunInput {
                agent: Some("codex".into()),
                pages: Some(vec!["concepts/attention.md".into()]),
                sources: Some(vec![]),
                stats: Some(RunStatsInput {
                    pages_updated: Some(1),
                    ..Default::default()
                }),
                ..Default::default()
            }))
            .unwrap();
        assert_ne!(run.is_error, Some(true));
        let payload: serde_json::Value = serde_json::from_str(&text(&run)).unwrap();
        assert_eq!(payload["agent"], "codex");

        let status = server.get_wiki_status().unwrap();
        let payload: serde_json::Value = serde_json::from_str(&text(&status)).unwrap();
        assert_eq!(payload["lastRun"]["agent"], "codex");
        assert_eq!(payload["pageCounts"]["concept"], 1);

        let dedup = server
            .list_wiki_dedup_candidates(Parameters(ListWikiDedupCandidatesInput {
                source: "reading.md".into(),
                limit: None,
            }))
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&text(&dedup)).unwrap();
        assert_eq!(payload["candidates"].as_array().unwrap().len(), 0);

        let ranked = server
            .list_wiki_dedup_candidates(Parameters(ListWikiDedupCandidatesInput {
                source: "papers/attention.pdf".into(),
                limit: None,
            }))
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&text(&ranked)).unwrap();
        let candidates = payload["candidates"].as_array().unwrap();
        assert_eq!(candidates[0]["existingPath"], "concepts/attention.md");
        assert_eq!(candidates[0]["score"], 50);
        assert!(
            candidates[0]["reason"]
                .as_str()
                .unwrap()
                .contains("matches the source name")
        );

        let deleted = server
            .delete_wiki_page(Parameters(DeleteWikiPageInput {
                path: "concepts/attention.md".into(),
                expected_hash: None,
                force: None,
                instructions_version: core::WIKI_INSTRUCTIONS_VERSION,
                run_id: None,
            }))
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&text(&deleted)).unwrap();
        assert_eq!(payload["deleted"], true);
        assert!(!wiki.join("concepts/attention.md").exists());
    }

    /// Pins the field names of the JSON an MCP client receives. `camelCase`
    /// protects spelling; this test protects presence, so adding, renaming or
    /// dropping a field is a visible failure rather than a silent contract
    /// change.
    #[test]
    fn board_payload_key_sets_are_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kandrium.db");
        let mut db = core::db::open_db(&path).unwrap();
        core::db::bootstrap(&mut db, &path).unwrap();

        let board = core::get_board_readonly(&db, "board_career").unwrap();
        let payload = serde_json::to_value(&board).unwrap();
        assert_eq!(keys(&payload), ["columns", "id", "title"]);

        let column = &payload["columns"][0];
        assert_eq!(keys(column), ["cards", "id", "title"]);

        let card = &column["cards"][0];
        assert_eq!(
            keys(card),
            [
                "boardId",
                "columnId",
                "description",
                "dueDate",
                "id",
                "labels",
                "title"
            ]
        );

        // Optional card fields and an empty `mirrored` are skipped only when
        // unset. A fully populated card and column pin their wire names.
        let full = core::Card {
            id: "card".into(),
            column_id: "col".into(),
            board_id: "board".into(),
            title: "Title".into(),
            description: "Body".into(),
            due_date: "2026-01-01".into(),
            labels: vec![1],
            project_id: Some("proj".into()),
            area_board_id: Some("area".into()),
            project_identifier: Some("proj".into()),
            ..Default::default()
        };
        assert_eq!(
            keys(&serde_json::to_value(&full).unwrap()),
            [
                "areaBoardId",
                "boardId",
                "columnId",
                "description",
                "dueDate",
                "id",
                "labels",
                "projectId",
                "projectIdentifier",
                "title"
            ]
        );

        let column = core::Column {
            id: "col".into(),
            title: "To Do".into(),
            cards: vec![],
            mirrored: vec![full],
        };
        assert_eq!(
            keys(&serde_json::to_value(&column).unwrap()),
            ["cards", "id", "mirrored", "title"]
        );
    }

    fn keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }
}
