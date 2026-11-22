-- v14: canonical persons; Clocks keeps placement and travel.
--
-- A person owns identity and profile (Network). A Clocks colleague is a
-- `clock_placements` row that references a person; the legacy `clock_people`
-- and `clock_travels` tables are copied once and then frozen (never read or
-- written by core again). A later v15 may drop them.
CREATE TABLE IF NOT EXISTS persons (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    role_title      TEXT NOT NULL DEFAULT '',
    organization    TEXT NOT NULL DEFAULT '',
    collaboration   TEXT NOT NULL DEFAULT '',
    email           TEXT NOT NULL DEFAULT '',
    url             TEXT NOT NULL DEFAULT '',
    location        TEXT NOT NULL DEFAULT '',
    tz_identifier   TEXT NOT NULL DEFAULT '',
    last_contact_on TEXT,
    how_met         TEXT NOT NULL DEFAULT '',
    notes           TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'active',
    in_clocks       INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS person_expertise (
    person_id TEXT NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
    label     TEXT NOT NULL,
    position  INTEGER NOT NULL,
    PRIMARY KEY (person_id, label)
);

-- One home city per person; travel covers temporary cities.
CREATE TABLE IF NOT EXISTS clock_placements (
    person_id  TEXT PRIMARY KEY REFERENCES persons(id) ON DELETE CASCADE,
    clock_id   TEXT NOT NULL REFERENCES clocks(id) ON DELETE RESTRICT,
    position   INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

-- Travel gets a persons FK; the legacy clock_travels table stays frozen.
CREATE TABLE IF NOT EXISTS clock_travels_v2 (
    id                    TEXT PRIMARY KEY,
    person_id             TEXT NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
    origin_clock_id       TEXT REFERENCES clocks(id) ON DELETE SET NULL,
    destination_clock_id  TEXT REFERENCES clocks(id) ON DELETE SET NULL,
    starts_at             INTEGER NOT NULL,
    ends_at               INTEGER,
    cancelled_at          INTEGER,
    created_at            TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_persons_name
    ON persons(name COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS idx_persons_network
    ON persons(status);
CREATE INDEX IF NOT EXISTS idx_persons_clocks
    ON persons(in_clocks);
CREATE INDEX IF NOT EXISTS idx_persons_last_contact
    ON persons(last_contact_on);
CREATE INDEX IF NOT EXISTS idx_person_expertise_label
    ON person_expertise(label COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS idx_clock_placements_clock_position
    ON clock_placements(clock_id, position);
CREATE INDEX IF NOT EXISTS idx_clock_travels_v2_person_start
    ON clock_travels_v2(person_id, starts_at);
CREATE INDEX IF NOT EXISTS idx_clock_travels_v2_destination_end
    ON clock_travels_v2(destination_clock_id, ends_at);

-- Backfill (idempotent: primary keys make a re-run a no-op and complete a
-- partial copy). Existing colleague ids are preserved so UI ids stay stable.

INSERT OR IGNORE INTO persons
    (id, name, status, in_clocks, created_at, updated_at)
SELECT id, name, 'active', 1, created_at, created_at
FROM clock_people;

INSERT OR IGNORE INTO clock_placements
    (person_id, clock_id, position, created_at)
SELECT id, home_clock_id, position, created_at
FROM clock_people;

INSERT OR IGNORE INTO clock_travels_v2
    (id, person_id, origin_clock_id, destination_clock_id,
     starts_at, ends_at, cancelled_at, created_at)
SELECT id, person_id, origin_clock_id, destination_clock_id,
       starts_at, ends_at, cancelled_at, created_at
FROM clock_travels;
