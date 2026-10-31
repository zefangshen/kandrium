-- Kandrium schema v1 — identical table shapes to the Drizzle-managed schema in
-- the Electron app (src/main/db/schema.ts), so the one-time importer can copy
-- rows as-is and MCP clients see identical data.
-- Applied idempotently by db::run_schema via PRAGMA user_version.

CREATE TABLE IF NOT EXISTS boards (
    id       TEXT PRIMARY KEY,
    title    TEXT NOT NULL,
    position INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS columns (
    id       TEXT PRIMARY KEY,
    board_id TEXT NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
    title    TEXT NOT NULL,
    position INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS cards (
    id          TEXT PRIMARY KEY,
    column_id   TEXT NOT NULL REFERENCES columns(id) ON DELETE CASCADE,
    title       TEXT NOT NULL,
    description TEXT DEFAULT '',
    due_date    TEXT DEFAULT '',
    labels      TEXT DEFAULT '[]',
    position    INTEGER NOT NULL,
    archived    INTEGER NOT NULL DEFAULT 0,
    archived_at TEXT
);

-- Persisted ordering for the virtual All board. Each row records which status
-- bucket ('todo' | 'in_progress' | 'done') a card appears in and its global
-- position within that bucket — independent of the card's position on its
-- home board, so users can freely interleave cards from different boards.
-- Rows are lazily created on first All-board load; cascade-deleted with the card.
CREATE TABLE IF NOT EXISTS all_card_order (
    card_id  TEXT PRIMARY KEY REFERENCES cards(id) ON DELETE CASCADE,
    status   TEXT NOT NULL,
    position INTEGER NOT NULL
);
