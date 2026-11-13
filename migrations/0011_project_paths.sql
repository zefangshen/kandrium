-- Kandrium schema v11 — project folders.
-- Each row references one folder attached to a project (local path or a URL
-- kept verbatim). Removing a row only drops the reference; nothing on disk is
-- touched. Additive only: earlier schema files stay identical.

CREATE TABLE IF NOT EXISTS project_paths (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_project_paths_unique
    ON project_paths (project_id, path);

CREATE INDEX IF NOT EXISTS idx_project_paths_order
    ON project_paths (project_id, position);

-- Carry the legacy single `projects.source` value over as the project's first
-- folder. Any non-empty value is copied (local paths and URLs alike) so
-- nothing becomes invisible when the field turns into a list; only projects
-- with no row yet are touched, so re-running the step is a no-op.
INSERT INTO project_paths (id, project_id, path, position, created_at)
SELECT lower(hex(randomblob(12))), id, trim(source), 0, CAST(strftime('%s', 'now') AS INTEGER)
FROM projects
WHERE trim(source) <> ''
  AND NOT EXISTS (
      SELECT 1 FROM project_paths WHERE project_paths.project_id = projects.id
  );
