-- Kandrium schema v7 — project source reference.
-- A free-form hint (local folder path or repository URL) pointing at the
-- project's source code. Additive only: earlier schema files stay identical.

ALTER TABLE projects ADD COLUMN source TEXT NOT NULL DEFAULT '';
