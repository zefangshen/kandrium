-- Kandrium schema v2 — Projects. Kept in a separate file from schema.sql so
-- the v1 shape stays byte-identical to the legacy Drizzle schema (the row-copy
-- importer and MCP clients depend on that shape).
-- Applied idempotently by db::run_schema via PRAGMA user_version.

CREATE TABLE IF NOT EXISTS project_folders (
    id        TEXT PRIMARY KEY,
    name      TEXT NOT NULL,
    parent_id TEXT REFERENCES project_folders(id) ON DELETE CASCADE,
    position  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS projects (
    id          TEXT PRIMARY KEY,
    folder_id   TEXT REFERENCES project_folders(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status      TEXT NOT NULL DEFAULT 'active',
    board_id    TEXT NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    created_at  INTEGER NOT NULL DEFAULT 0,
    updated_at  INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_project_folders_parent ON project_folders(parent_id);
CREATE INDEX IF NOT EXISTS idx_projects_folder_id     ON projects(folder_id);
CREATE INDEX IF NOT EXISTS idx_projects_board_id      ON projects(board_id);
