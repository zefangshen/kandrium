-- Kandrium schema v12 — LLM wiki index.
-- Derived index over the wiki page root and the documents root. The files stay
-- authoritative: dropping these tables and rebuilding reproduces the index.
-- Additive only: earlier schema files stay identical.

CREATE TABLE IF NOT EXISTS wiki_meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS wiki_sources (
    rel_path    TEXT PRIMARY KEY NOT NULL,   -- relative to documents root
    kind        TEXT NOT NULL,               -- 'markdown' | 'pdf'
    hash        TEXT NOT NULL,
    size        INTEGER NOT NULL DEFAULT 0,
    mtime       INTEGER NOT NULL DEFAULT 0,
    state       TEXT NOT NULL DEFAULT 'new', -- new|indexed|compiled|stale|failed
    compiled_at INTEGER,
    error       TEXT
);

CREATE INDEX IF NOT EXISTS idx_wiki_sources_state ON wiki_sources (state);
CREATE INDEX IF NOT EXISTS idx_wiki_sources_hash  ON wiki_sources (hash);

CREATE TABLE IF NOT EXISTS wiki_pages (
    rel_path       TEXT PRIMARY KEY NOT NULL, -- relative to wiki root
    kind           TEXT NOT NULL,             -- source|entity|concept|index|page
    title          TEXT NOT NULL,
    summary        TEXT NOT NULL DEFAULT '',
    aliases_json   TEXT NOT NULL DEFAULT '[]',
    tags_json      TEXT NOT NULL DEFAULT '[]',
    reviewed       INTEGER NOT NULL DEFAULT 0,
    content_hash   TEXT NOT NULL,
    generated_hash TEXT,
    updated_at     INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_wiki_pages_kind ON wiki_pages (kind);

CREATE TABLE IF NOT EXISTS wiki_page_sources (
    page_path   TEXT NOT NULL REFERENCES wiki_pages (rel_path) ON DELETE CASCADE,
    source_path TEXT NOT NULL,
    source_hash TEXT NOT NULL DEFAULT '',
    start_line  INTEGER NOT NULL DEFAULT 0,
    end_line    INTEGER NOT NULL DEFAULT 0,
    start_page  INTEGER NOT NULL DEFAULT 0,
    end_page    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (page_path, source_path, start_line, end_line, start_page, end_page)
);

CREATE INDEX IF NOT EXISTS idx_wiki_page_sources_source
    ON wiki_page_sources (source_path);

CREATE TABLE IF NOT EXISTS wiki_links (
    from_path TEXT NOT NULL,
    to_path   TEXT NOT NULL,
    anchor    TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (from_path, to_path, anchor)
);

CREATE INDEX IF NOT EXISTS idx_wiki_links_to ON wiki_links (to_path);

CREATE TABLE IF NOT EXISTS wiki_runs (
    id                   TEXT PRIMARY KEY NOT NULL,
    operation            TEXT NOT NULL,
    agent                TEXT NOT NULL DEFAULT '',
    instructions_version INTEGER NOT NULL DEFAULT 0,
    started_at           INTEGER NOT NULL DEFAULT 0,
    finished_at          INTEGER,
    stats_json           TEXT NOT NULL DEFAULT '{}',
    note                 TEXT,
    error                TEXT
);

CREATE INDEX IF NOT EXISTS idx_wiki_runs_started ON wiki_runs (started_at DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS wiki_fts USING fts5(
    path UNINDEXED,
    title,
    summary,
    aliases,
    body,
    tokenize = 'unicode61'
);
