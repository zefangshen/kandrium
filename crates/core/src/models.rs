//! API + row types ported from `src/shared/types/board.ts` and
//! `src/main/services/types.ts`.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Canonical status
// ---------------------------------------------------------------------------

/// Canonical status buckets of the virtual All board.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CardStatus {
    Todo,
    InProgress,
    Done,
}

impl CardStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CardStatus::Todo => "todo",
            CardStatus::InProgress => "in_progress",
            CardStatus::Done => "done",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "todo" => Some(CardStatus::Todo),
            "in_progress" => Some(CardStatus::InProgress),
            "done" => Some(CardStatus::Done),
            _ => None,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            CardStatus::Todo => "To Do",
            CardStatus::InProgress => "In Progress",
            CardStatus::Done => "Done",
        }
    }

    pub const ALL: [CardStatus; 3] = [CardStatus::Todo, CardStatus::InProgress, CardStatus::Done];

    /// Index into fixed-size per-status arrays ([i64; 3], buckets, ...).
    pub fn index(self) -> usize {
        match self {
            CardStatus::Todo => 0,
            CardStatus::InProgress => 1,
            CardStatus::Done => 2,
        }
    }
}

// ---------------------------------------------------------------------------
// API types (serde camelCase — identical JSON shapes to the TS server)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Board {
    pub id: String,
    pub title: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub id: String,
    pub column_id: String,
    pub title: String,
    pub description: String,
    pub due_date: String,
    pub labels: Vec<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub id: String,
    pub title: String,
    pub cards: Vec<Card>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BoardWithColumns {
    pub id: String,
    pub title: String,
    pub columns: Vec<Column>,
}

/// Virtual "All" board — computed aggregation across every real board.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AllBoardCard {
    #[serde(flatten)]
    pub card: Card,
    pub board_id: String,
    pub board_title: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AllBoardColumn {
    /// Synthetic id, `all:<status>`.
    pub id: String,
    pub title: String,
    pub status: CardStatus,
    pub cards: Vec<AllBoardCard>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AllBoard {
    pub id: String,
    pub title: String,
    pub columns: Vec<AllBoardColumn>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedCard {
    pub id: String,
    pub title: String,
    pub description: String,
    pub due_date: String,
    pub labels: Vec<i64>,
    pub archived_at: String,
    pub column_title: String,
    pub board_id: String,
    pub board_title: String,
}

// ---------------------------------------------------------------------------
// Request payloads
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreateCardBody {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub due_date: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<i64>>,
}

// ---------------------------------------------------------------------------
// Tool/IPC result envelopes (match the TS service return shapes)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BoardsResult {
    pub boards: Vec<Board>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedCardsResult {
    pub cards: Vec<ArchivedCard>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OkResult {
    pub ok: bool,
}

impl OkResult {
    pub fn ok() -> Self {
        OkResult { ok: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveAllResult {
    pub ok: bool,
    pub archived_count: i64,
}

// ---------------------------------------------------------------------------
// Internal row types (main-process only, snake_case = SQL column names)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct BoardRow {
    pub id: String,
    pub title: String,
    pub position: i64,
}

#[derive(Clone, Debug)]
pub struct ColumnRow {
    pub id: String,
    pub board_id: String,
    pub title: String,
    pub position: i64,
}

#[derive(Clone, Debug)]
pub struct CardRow {
    pub id: String,
    pub column_id: String,
    pub title: String,
    pub description: String,
    pub due_date: String,
    /// JSON text: `"[1,4]"`.
    pub labels: String,
    pub position: i64,
    pub archived: i64,
    pub archived_at: Option<String>,
}

/// Joined row for the virtual All board aggregation.
#[derive(Clone, Debug)]
pub struct AllCardRow {
    pub id: String,
    pub title: String,
    pub description: String,
    pub due_date: String,
    pub labels: String,
    pub position: i64,
    pub column_id: String,
    pub column_title: String,
    pub board_id: String,
    pub board_title: String,
    pub order_status: Option<String>,
    pub order_position: Option<i64>,
}

/// Row in the persisted All-board ordering table.
#[derive(Clone, Debug)]
pub struct AllOrderRow {
    pub card_id: String,
    pub status: String,
    pub position: i64,
}

#[derive(Clone, Debug)]
pub struct ArchivedCardRow {
    pub id: String,
    pub title: String,
    pub description: String,
    pub due_date: String,
    pub labels: String,
    pub archived_at: Option<String>,
    pub column_title: String,
    pub board_id: String,
    pub board_title: String,
}
