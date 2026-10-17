//! Canonical constants ported from `src/shared/constants.ts`.

use crate::models::CardStatus;

/// Virtual (non-persisted) board that aggregates cards across every real board.
pub const ALL_BOARD_ID: &str = "all";

/// Synthetic column ids in the All board are `all:<status>`.
pub const ALL_COLUMN_PREFIX: &str = "all:";

/// Canonical status buckets of the All board, in display order.
pub const CARD_STATUSES: [StatusDef; 3] = [
    StatusDef {
        key: CardStatus::Todo,
        title: "To Do",
    },
    StatusDef {
        key: CardStatus::InProgress,
        title: "In Progress",
    },
    StatusDef {
        key: CardStatus::Done,
        title: "Done",
    },
];

#[derive(Clone, Copy, Debug)]
pub struct StatusDef {
    pub key: CardStatus,
    pub title: &'static str,
}

/// Normalize a column title for matching against canonical statuses.
pub fn normalize_column_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Map a real column title to a canonical status, or `None` if it doesn't match.
pub fn status_from_column_title(title: &str) -> Option<CardStatus> {
    let normalized = normalize_column_title(title);
    CARD_STATUSES
        .iter()
        .find(|s| normalize_column_title(s.title) == normalized)
        .map(|s| s.key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_matching() {
        assert_eq!(status_from_column_title("To Do"), Some(CardStatus::Todo));
        assert_eq!(status_from_column_title("done"), Some(CardStatus::Done));
        assert_eq!(
            status_from_column_title("  IN   progress "),
            Some(CardStatus::InProgress)
        );
        assert_eq!(status_from_column_title("Backlog"), None);
        assert_eq!(status_from_column_title(""), None);
    }

    #[test]
    fn status_keys_round_trip() {
        for s in CARD_STATUSES {
            assert_eq!(CardStatus::from_key(s.key.as_str()), Some(s.key));
            assert_eq!(s.key.title(), s.title);
        }
    }
}
