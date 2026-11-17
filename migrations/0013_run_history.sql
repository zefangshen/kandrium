-- Kandrium schema v13 — run-scoped file and row history.
-- Mirrors the on-disk history store (`<data_dir>/kandrium/history/`); the
-- manifest file is authoritative and these tables are queryable mirrors.
-- Additive only: earlier schema files stay identical.

CREATE TABLE IF NOT EXISTS run_history (
    run_id      TEXT NOT NULL,
    seq         INTEGER NOT NULL,
    kind        TEXT NOT NULL DEFAULT 'file', -- file | dir | link
    root        TEXT NOT NULL,                -- wiki | notes | documents | projects
    path        TEXT NOT NULL,
    op          TEXT NOT NULL,
    before_hash TEXT,
    after_hash  TEXT,
    mode        INTEGER,
    size        INTEGER,
    target      TEXT,                         -- symlink target
    empty       INTEGER NOT NULL DEFAULT 0,   -- directories only
    reason      TEXT,                         -- unrestorable cause
    restorable  INTEGER NOT NULL DEFAULT 1,
    captured_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (run_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_run_history_path ON run_history (path);
CREATE INDEX IF NOT EXISTS idx_run_history_run  ON run_history (run_id);

CREATE TABLE IF NOT EXISTS run_row_changes (
    run_id         TEXT PRIMARY KEY NOT NULL,
    label          TEXT NOT NULL DEFAULT '',
    kind           TEXT NOT NULL DEFAULT 'implicit',
    closed_by      TEXT NOT NULL DEFAULT '',
    snapshot       TEXT NOT NULL,
    snapshot_bytes INTEGER NOT NULL DEFAULT 0,
    user_version   INTEGER NOT NULL DEFAULT 0,
    schema_json    TEXT NOT NULL DEFAULT '{}',
    diff_path      TEXT NOT NULL DEFAULT '',
    inserted       INTEGER NOT NULL DEFAULT 0,
    updated        INTEGER NOT NULL DEFAULT 0,
    deleted        INTEGER NOT NULL DEFAULT 0,
    contended      INTEGER NOT NULL DEFAULT 0,
    restorable     INTEGER NOT NULL DEFAULT 1,
    captured_at    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS run_undo (
    run_id            TEXT PRIMARY KEY NOT NULL,
    state             TEXT NOT NULL DEFAULT 'open',
    last_heartbeat_at INTEGER NOT NULL DEFAULT 0,
    undone_at         INTEGER,
    note              TEXT
);

-- At most one open run, enforced by SQLite rather than by convention.
CREATE UNIQUE INDEX IF NOT EXISTS idx_run_undo_one_open
    ON run_undo (state) WHERE state = 'open';
