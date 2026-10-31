-- Kandrium schema v6 — archived projects.
--
-- Archiving is a reversible flag: the project row, its board/cards, and its
-- on-disk documents all stay exactly where they are. Archived projects are
-- hidden from the tree and pickers and listed separately for restore.
-- Additive only: earlier schema files stay byte-identical.

ALTER TABLE projects ADD COLUMN archived    INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN archived_at TEXT;

CREATE INDEX IF NOT EXISTS idx_projects_archived ON projects(archived);
