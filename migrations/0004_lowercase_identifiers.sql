-- Kandrium schema v4 — project identifiers are stored lowercase.
-- Identifiers are canonical lowercase (1–10 chars, a–z 0–9 - _). The v3 unique
-- index compares COLLATE NOCASE, so lowercasing existing rows cannot introduce
-- a duplicate. Runs atomically with the user_version bump in db::run_schema.

UPDATE projects
SET identifier = lower(identifier)
WHERE identifier <> '' AND identifier <> lower(identifier);
