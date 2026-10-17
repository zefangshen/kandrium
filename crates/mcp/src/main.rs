//! Kandrium MCP server — port of `src/mcp/server.ts` onto the official Rust
//! MCP SDK (rmcp). Same 17 tools, same names/descriptions/schemas, same
//! result shapes (pretty JSON text; tool-level `isError` + message on failure).
//!
//! DB path resolution matches the core crate: `KANDRIUM_DB` env override, else
//! `<data_dir>/kandrium/kandrium.db`. Logging goes to stderr only — stdout is
//! the MCP transport.

use std::sync::Mutex;

use kandrium_core as core;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ErrorData};
use rmcp::schemars;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};

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
struct CreateColumnInput {
    /// Board id
    board_id: String,
    /// Column title
    title: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ColumnIdInput {
    /// Column id
    id: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct RenameColumnInput {
    /// Column id
    id: String,
    /// New column title
    title: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct MoveColumnInput {
    /// Column id to move
    id: String,
    /// Place the column before this column id, null/omit for end
    before_column_id: Option<String>,
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
// Server
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct KandriumServer {
    db: std::sync::Arc<Mutex<rusqlite::Connection>>,
    /// Populated by the #[tool_router] macro; kept for rmcp's expected shape
    /// (the generated #[tool_handler] impl routes through the router).
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl KandriumServer {
    fn new(db: rusqlite::Connection) -> Self {
        KandriumServer {
            db: std::sync::Arc::new(Mutex::new(db)),
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

    // -- Boards -------------------------------------------------------------

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
        self.run(|db| core::get_board(db, &board_id))
    }

    #[tool(
        description = "Load the virtual All board: all non-archived cards aggregated into To Do / In Progress / Done buckets across every board; each card includes its boardId/boardTitle"
    )]
    fn get_all_board(&self) -> Result<CallToolResult, ErrorData> {
        self.run(core::get_all_board)
    }

    // -- Columns ------------------------------------------------------------

    #[tool(description = "Create a new column (list) on a board, appended at the end")]
    fn create_column(
        &self,
        Parameters(CreateColumnInput { board_id, title }): Parameters<CreateColumnInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(|db| core::create_column(db, &board_id, &title))
    }

    #[tool(description = "Rename a column")]
    fn rename_column(
        &self,
        Parameters(RenameColumnInput { id, title }): Parameters<RenameColumnInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(|db| core::rename_column(db, &id, &title))
    }

    #[tool(description = "Delete a column and all its cards (permanent)")]
    fn delete_column(
        &self,
        Parameters(ColumnIdInput { id }): Parameters<ColumnIdInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(|db| core::delete_column(db, &id))
    }

    #[tool(
        description = "Move/reorder a column within its board; place it before beforeColumnId, or at the end if omitted"
    )]
    fn move_column(
        &self,
        Parameters(MoveColumnInput {
            id,
            before_column_id,
        }): Parameters<MoveColumnInput>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run(|db| core::move_column(db, &id, before_column_id.as_deref()))
    }

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
    let db_path = core::resolve_db_path();
    if let Ok(Some(from)) = core::migrate_legacy_db() {
        eprintln!(
            "[kandrium-mcp] migrated database from {} to {}",
            from.display(),
            db_path.display()
        );
    }
    let db = core::open_db(&db_path)?;
    core::bootstrap(&db)?;
    eprintln!("[kandrium-mcp] DB: {}", db_path.display());

    let service = KandriumServer::new(db)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}
