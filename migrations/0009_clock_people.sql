-- v9: people attached to saved Clocks cities (Clocks section, Phase 3).
--
-- `home_clock_id` is the resident relation: a person belongs to exactly one
-- saved city. Deleting a city is blocked while it still has residents
-- (`ON DELETE RESTRICT`); the core layer reports that as a friendly error.
-- Travel/visitor rows arrive in a later additive migration (Phase 4).
CREATE TABLE IF NOT EXISTS clock_people (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    home_clock_id TEXT NOT NULL REFERENCES clocks(id) ON DELETE RESTRICT,
    position INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_clock_people_home_position
    ON clock_people(home_clock_id, position);
