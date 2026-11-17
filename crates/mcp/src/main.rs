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
    /// Populated by the #[tool_router] macro; kept for rmcp's expected shape
    /// (the generated #[tool_handler] impl routes through the router).
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
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

    /// Wiki write plumbing. `KANDRIUM_MCP_WIKI=read` keeps reads available
    /// and refuses every write.
    fn run_wiki_write<T: serde::Serialize>(
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
        self.run(|db| core::archive_all_cards(db, &col_id))
    }

    // -- Cards --------------------------------------------------------------

    #[tool(
        description = "Create a card in a column, appended at the bottom; returns the created card"
    )]
    fn create_card(
        &self,
        Parameters(CreateCardInput { col_id, card }): Parameters<CreateCardInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(|db| {
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
        self.run(|db| {
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
        self.run(|db| core::move_card(db, &id, &to_col_id, before_card_id.as_deref()))
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
        self.run(|db| core::move_all_card(db, &id, status.into_core(), before_card_id.as_deref()))
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
        self.run(|db| core::move_card_to_board(db, &id, &target_board_id))
    }

    #[tool(description = "Delete a card permanently")]
    fn delete_card(
        &self,
        Parameters(CardIdInput { id }): Parameters<CardIdInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(|db| core::delete_card(db, &id))
    }

    #[tool(description = "Archive or unarchive a card (archived cards are hidden from the board)")]
    fn archive_card(
        &self,
        Parameters(SetCardArchivedInput { id, archived }): Parameters<SetCardArchivedInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(|db| core::set_card_archived(db, &id, archived))
    }

    #[tool(description = "Restore an archived card back to its column")]
    fn restore_card(
        &self,
        Parameters(CardIdInput { id }): Parameters<CardIdInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(|db| core::set_card_archived(db, &id, false))
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
        self.run_wiki_write(|db, wiki, _| {
            let run_id = input.run_id.clone();
            let result = core::write_page(
                db,
                wiki,
                &core::WritePageSpec {
                    rel_path: input.path,
                    content: input.content,
                    expected_hash: input.expected_hash,
                    force: input.force.unwrap_or(false),
                    agent: input.agent,
                    instructions_version: input.instructions_version,
                    run_id: run_id.clone(),
                },
            )?;
            settle_run(db, &run_id)?;
            Ok(result)
        })
    }

    #[tool(
        description = "Replace exact text in an existing wiki page, then revalidate and reindex it"
    )]
    fn patch_wiki_page(
        &self,
        Parameters(input): Parameters<PatchWikiPageInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_wiki_write(|db, wiki, _| {
            let run_id = input.run_id.clone();
            let result = core::patch_page(
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
                    run_id: run_id.clone(),
                },
            )?;
            settle_run(db, &run_id)?;
            Ok(result)
        })
    }

    #[tool(description = "Delete a wiki page and its index rows. Reviewed pages need force.")]
    fn delete_wiki_page(
        &self,
        Parameters(input): Parameters<DeleteWikiPageInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_wiki_write(|db, wiki, _| {
            let path = input.path;
            let run_id = input.run_id.clone();
            core::delete_page(
                db,
                wiki,
                &core::DeletePageSpec {
                    rel_path: path.clone(),
                    expected_hash: input.expected_hash,
                    force: input.force.unwrap_or(false),
                    instructions_version: input.instructions_version,
                    run_id: run_id.clone(),
                },
            )?;
            settle_run(db, &run_id)?;
            Ok(serde_json::json!({ "path": path, "deleted": true }))
        })
    }

    #[tool(
        description = "Record a compile run and mark its sources compiled. Call once after a batch of page writes."
    )]
    fn record_wiki_run(
        &self,
        Parameters(input): Parameters<RecordRunInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_wiki_write(|db, _, _| {
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
            let store = core::history::HistoryStore::for_connection(db)
                .ok_or_else(|| core::KandriumError::new("history needs a file-backed database"))?;
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
        self.run_wiki_write(|db, _, _| {
            let store = core::history::HistoryStore::for_connection(db)
                .ok_or_else(|| core::KandriumError::new("history needs a file-backed database"))?;
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

/// Close a client-supplied run id once its mutating call finishes. A live
/// explicit run (with an open `run_undo` row) is left for its closer.
fn settle_run(db: &rusqlite::Connection, run_id: &Option<String>) -> core::Result<()> {
    let (Some(run_id), Some(store)) = (
        run_id.as_deref(),
        core::history::HistoryStore::for_connection(db),
    ) else {
        return Ok(());
    };
    core::history::close_unmanaged_run(db, &store, run_id, core::history::ClosedBy::Explicit)
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
    started_at: i64,
    finished_at: Option<i64>,
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
        Self {
            run_id: manifest.run_id.clone(),
            label: manifest.label.clone(),
            kind: manifest.kind.as_str().to_string(),
            state: manifest.state.as_str().to_string(),
            started_at: manifest.started_at,
            finished_at: manifest.finished_at,
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
    core::logging::info!("DB: {}", db_path.display());

    let service = KandriumServer::new(db)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
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
            "create_card",
            "delete_card",
            "delete_wiki_page",
            "get_all_board",
            "get_board",
            "get_wiki_instructions",
            "get_wiki_status",
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
        server
            .write_wiki_page(Parameters(WriteWikiPageInput {
                path: path.into(),
                content: original.clone(),
                expected_hash: None,
                force: None,
                agent: None,
                instructions_version: core::WIKI_INSTRUCTIONS_VERSION,
                run_id: None,
            }))
            .unwrap();

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
