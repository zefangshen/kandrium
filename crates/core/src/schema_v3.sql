-- Kandrium schema v3 — Project identifiers and card ↔ project links.
-- Additive only; the v1 and v2 files stay byte-identical so the legacy importer
-- and MCP clients keep their existing shape.
--
-- Applied atomically by db::run_schema (DDL + backfill + user_version in one
-- transaction), so a crash can never leave the database with the columns added
-- but the version still at 2.

-- Short, unique, user-facing project key shown in the tree and on cards.
-- The default is only a migration placeholder: run_schema backfills every row
-- from the project name in the same transaction.
ALTER TABLE projects ADD COLUMN identifier TEXT NOT NULL DEFAULT '';
-- 0 = still auto-derived from the name (renames regenerate it); 1 = pinned by
-- a manual edit on the Overview page.
ALTER TABLE projects ADD COLUMN identifier_locked INTEGER NOT NULL DEFAULT 0;

-- Project a card belongs to / is tagged with.
ALTER TABLE cards ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE SET NULL;
-- Area board (Career / Work / Life / any non-project board) that mirrors a
-- project card. Only meaningful when the card lives on a project board.
ALTER TABLE cards ADD COLUMN area_board_id TEXT REFERENCES boards(id) ON DELETE SET NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_identifier
    ON projects(identifier COLLATE NOCASE) WHERE identifier <> '';
CREATE INDEX IF NOT EXISTS idx_cards_project_id ON cards(project_id);
CREATE INDEX IF NOT EXISTS idx_cards_area_board_id ON cards(area_board_id);
