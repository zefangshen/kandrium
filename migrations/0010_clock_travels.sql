-- v10: temporary travel windows for Clocks people (Clocks section, Phase 4).
--
-- A travel row moves one person to another saved city for a window. Home
-- (`clock_people.home_clock_id`) is never changed by travel; the resolver
-- picks the latest active row at an instant and otherwise falls back home.
--
-- Cascade rules (decided in Phase 4):
--   * `person_id` -> `clock_people` ON DELETE CASCADE: removing a person
--     discards their travel history.
--   * `origin_clock_id` / `destination_clock_id` -> `clocks` ON DELETE SET
--     NULL: a deleted city keeps its historical trips. `remove_clock`
--     cancels every still-active trip to the city before deleting it so no
--     visitor is left pointing at a vanished destination (residents still
--     block the delete with ON DELETE RESTRICT).
--
-- Times are Unix epoch seconds in UTC. `ends_at` NULL means open-ended;
-- `cancelled_at` marks a trip ended early or invalidated by relocation.
CREATE TABLE IF NOT EXISTS clock_travels (
    id TEXT PRIMARY KEY,
    person_id TEXT NOT NULL REFERENCES clock_people(id) ON DELETE CASCADE,
    origin_clock_id TEXT REFERENCES clocks(id) ON DELETE SET NULL,
    destination_clock_id TEXT REFERENCES clocks(id) ON DELETE SET NULL,
    starts_at INTEGER NOT NULL,
    ends_at INTEGER,
    cancelled_at INTEGER,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_clock_travels_person_start
    ON clock_travels(person_id, starts_at);

CREATE INDEX IF NOT EXISTS idx_clock_travels_destination_end
    ON clock_travels(destination_clock_id, ends_at);
