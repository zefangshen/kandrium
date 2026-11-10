-- v8: Clocks section (Kwora world-clock port). One row per saved city.
-- Additive and idempotent: a fresh database and an upgraded one converge on
-- the same shape, and re-running the step is a no-op.
CREATE TABLE IF NOT EXISTS clocks (
    id TEXT PRIMARY KEY,
    tz_identifier TEXT NOT NULL UNIQUE,
    position INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_clocks_position ON clocks(position);
