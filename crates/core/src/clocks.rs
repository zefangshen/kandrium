//! Clocks section data layer (Kwora world-clock port).
//!
//! The section stores the user's saved cities (an IANA time-zone identifier
//! per row plus a display order) and the people attached to each city. Tick
//! state, formatting and the 12/24-hour preference live in the UI; every
//! function here is synchronous and takes `&rusqlite::Connection`, like the
//! rest of `kandrium-core`.
//!
//! People are residents of exactly one city (`home_clock_id`). A travel row
//! temporarily places someone in another saved city for a window; home is
//! never mutated by travel. A city with residents refuses to be deleted
//! (`ON DELETE RESTRICT`); a city that is only a travel destination cancels
//! the active trips pointing at it before deleting (`SET NULL` keeps the
//! history). Placement is a pure function of the cached travel rows and an
//! instant, so callers can expire a trip without touching the database.
//!
//! Time-zone math uses `jiff` (already a workspace dependency), which bundles
//! IANA zone data, so DST transitions and historical offsets are handled by
//! the crate instead of hand-rolled offset arithmetic.

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::uid;
use crate::error::{KandriumError, Result};

/// Cities seeded on a fresh database, in display order. Kwora's reference
/// trio; `Europe/Kyiv` uses its pre-2022 alias on platforms whose zone
/// database only knows the old name.
pub const DEFAULT_CLOCKS: [&str; 3] = ["Europe/Kyiv", "Asia/Tokyo", "America/Los_Angeles"];

/// Fallback aliases tried when the primary identifier is absent from the
/// runtime zone database.
const DEFAULT_CLOCK_FALLBACK: [(&str, &str); 3] = [
    ("Europe/Kyiv", "Europe/Kiev"),
    ("Asia/Tokyo", "Asia/Tokyo"),
    ("America/Los_Angeles", "America/Los_Angeles"),
];

/// A saved clock row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clock {
    pub id: String,
    pub tz_identifier: String,
    pub position: i64,
    pub created_at: String,
}

/// A person attached to one saved city.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub id: String,
    pub name: String,
    /// Resident city: the clock this person belongs to.
    pub home_clock_id: String,
    pub position: i64,
    pub created_at: String,
}

/// A temporary placement of one person in another saved city.
///
/// `starts_at` / `ends_at` are Unix epoch seconds in UTC. `ends_at == None`
/// is an open-ended trip; `cancelled_at` marks a trip ended early or
/// invalidated by a relocation. The destination becomes `None` only when its
/// city was deleted (the history row survives).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Travel {
    pub id: String,
    pub person_id: String,
    /// City the person was in when the trip started.
    pub origin_clock_id: Option<String>,
    pub destination_clock_id: Option<String>,
    pub starts_at: i64,
    pub ends_at: Option<i64>,
    pub cancelled_at: Option<i64>,
    pub created_at: String,
}

/// Formatted text for one city at one instant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockText {
    /// `HH:mm` for 24-hour mode, `h:mm` (no leading zero) for 12-hour mode.
    pub time: String,
    /// Lowercase `am`/`pm` in 12-hour mode; `None` in 24-hour mode.
    pub period: Option<String>,
    /// `EEE, d MMM.` — for example `Sun, 8 Mar.`
    pub date: String,
    /// `GMT`, `GMT+5`, `GMT+5:30`, `GMT-4`.
    pub offset: String,
}

/// All saved clocks, ordered by display position.
pub fn list_clocks(db: &Connection) -> Result<Vec<Clock>> {
    let mut stmt = db.prepare(
        "SELECT id, tz_identifier, position, created_at
         FROM clocks
         ORDER BY position, id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Clock {
            id: row.get(0)?,
            tz_identifier: row.get(1)?,
            position: row.get(2)?,
            created_at: row.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Append a city. Rejects unknown IANA identifiers and duplicates.
pub fn add_clock(db: &Connection, tz_identifier: &str) -> Result<Clock> {
    let identifier = tz_identifier.trim();
    if identifier.is_empty() {
        return Err(KandriumError::new("Time zone identifier required"));
    }
    if resolve_zone(identifier).is_none() {
        return Err(KandriumError::new(format!(
            "Unknown time zone: {identifier}"
        )));
    }
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM clocks WHERE tz_identifier = ?1)",
        [identifier],
        |row| row.get(0),
    )?;
    if exists {
        return Err(KandriumError::new(format!(
            "Time zone already added: {identifier}"
        )));
    }

    let position: i64 = db.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM clocks",
        [],
        |row| row.get(0),
    )?;
    let clock = Clock {
        id: uid(),
        tz_identifier: identifier.to_string(),
        position,
        created_at: Timestamp::now().to_string(),
    };
    db.execute(
        "INSERT INTO clocks (id, tz_identifier, position, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            clock.id,
            clock.tz_identifier,
            clock.position,
            clock.created_at
        ],
    )?;
    Ok(clock)
}

/// Remove a city and close the gap it leaves in the display order.
///
/// A city that still has residents cannot be removed: the foreign key is
/// `ON DELETE RESTRICT`, and this check turns the raw constraint error into
/// a readable one. Removing the last resident then frees the city.
///
/// Travel is different from residency: an active trip *to* this city is
/// cancelled first (the traveller returns home) and the history row survives
/// with a `NULL` destination (`ON DELETE SET NULL`). Deleting a city must not
/// strand a visitor in a place that no longer exists.
pub fn remove_clock(db: &Connection, id: &str) -> Result<()> {
    let position: Option<i64> = db
        .query_row("SELECT position FROM clocks WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .ok();
    let Some(position) = position else {
        return Err(KandriumError::new("Clock not found"));
    };

    let residents: i64 = db.query_row(
        "SELECT COUNT(*) FROM clock_people WHERE home_clock_id = ?1",
        [id],
        |row| row.get(0),
    )?;
    if residents > 0 {
        let noun = if residents == 1 { "person" } else { "people" };
        return Err(KandriumError::new(format!(
            "This city still has {residents} {noun}. Remove them first."
        )));
    }

    let tx = db.unchecked_transaction()?;
    let now = Timestamp::now().as_second();
    tx.execute(
        "UPDATE clock_travels SET cancelled_at = ?2
         WHERE destination_clock_id = ?1
           AND cancelled_at IS NULL
           AND (ends_at IS NULL OR ends_at > ?2)",
        rusqlite::params![id, now],
    )?;
    tx.execute("DELETE FROM clocks WHERE id = ?1", [id])?;
    tx.execute(
        "UPDATE clocks SET position = position - 1 WHERE position > ?1",
        [position],
    )?;
    tx.commit()?;
    Ok(())
}

/// Rewrite the display order from a complete list of ids.
pub fn reorder_clocks(db: &Connection, ordered_ids: &[String]) -> Result<()> {
    let existing = list_clocks(db)?;
    if ordered_ids.len() != existing.len() {
        return Err(KandriumError::new(
            "Reorder must include every saved clock exactly once",
        ));
    }
    let mut expected: Vec<&str> = existing.iter().map(|c| c.id.as_str()).collect();
    expected.sort_unstable();
    let mut offered: Vec<&str> = ordered_ids.iter().map(String::as_str).collect();
    offered.sort_unstable();
    if expected != offered {
        return Err(KandriumError::new(
            "Reorder must include every saved clock exactly once",
        ));
    }

    let tx = db.unchecked_transaction()?;
    for (position, id) in ordered_ids.iter().enumerate() {
        tx.execute(
            "UPDATE clocks SET position = ?1 WHERE id = ?2",
            rusqlite::params![position as i64, id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// People attached to one city, in display order.
///
/// `home_clock_id` is the resident relation. Visitors (people currently in
/// this city because of travel) arrive with Phase 4's travel rows.
pub fn list_people(db: &Connection, clock_id: &str) -> Result<Vec<Person>> {
    let mut stmt = db.prepare(
        "SELECT id, name, home_clock_id, position, created_at
         FROM clock_people
         WHERE home_clock_id = ?1
         ORDER BY position, id",
    )?;
    let rows = stmt.query_map([clock_id], person_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Every person, ordered by city then display position. The UI groups these
/// by `home_clock_id` when reloading the Clocks page.
pub fn list_all_people(db: &Connection) -> Result<Vec<Person>> {
    let mut stmt = db.prepare(
        "SELECT id, name, home_clock_id, position, created_at
         FROM clock_people
         ORDER BY home_clock_id, position, id",
    )?;
    let rows = stmt.query_map([], person_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn person_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Person> {
    Ok(Person {
        id: row.get(0)?,
        name: row.get(1)?,
        home_clock_id: row.get(2)?,
        position: row.get(3)?,
        created_at: row.get(4)?,
    })
}

/// Attach a person to a city. Rejects blank names and unknown clocks; the
/// person is appended to the end of that city's list.
pub fn add_person(db: &Connection, clock_id: &str, name: &str) -> Result<Person> {
    let name = name.trim();
    if name.is_empty() {
        return Err(KandriumError::new("Person name required"));
    }
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM clocks WHERE id = ?1)",
        [clock_id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(KandriumError::new("Clock not found"));
    }

    let position: i64 = db.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM clock_people WHERE home_clock_id = ?1",
        [clock_id],
        |row| row.get(0),
    )?;
    let person = Person {
        id: uid(),
        name: name.to_string(),
        home_clock_id: clock_id.to_string(),
        position,
        created_at: Timestamp::now().to_string(),
    };
    db.execute(
        "INSERT INTO clock_people (id, name, home_clock_id, position, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            person.id,
            person.name,
            person.home_clock_id,
            person.position,
            person.created_at
        ],
    )?;
    Ok(person)
}

/// Rename a person in place (blank names are rejected).
pub fn rename_person(db: &Connection, id: &str, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(KandriumError::new("Person name required"));
    }
    let changed = db.execute(
        "UPDATE clock_people SET name = ?1 WHERE id = ?2",
        rusqlite::params![name, id],
    )?;
    if changed == 0 {
        return Err(KandriumError::new("Person not found"));
    }
    Ok(())
}

/// Remove a person and close the gap they leave in their city's order.
pub fn remove_person(db: &Connection, id: &str) -> Result<()> {
    let row: Option<(String, i64)> = db
        .query_row(
            "SELECT home_clock_id, position FROM clock_people WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    let Some((clock_id, position)) = row else {
        return Err(KandriumError::new("Person not found"));
    };

    let tx = db.unchecked_transaction()?;
    tx.execute("DELETE FROM clock_people WHERE id = ?1", [id])?;
    tx.execute(
        "UPDATE clock_people SET position = position - 1
         WHERE home_clock_id = ?1 AND position > ?2",
        rusqlite::params![clock_id, position],
    )?;
    tx.commit()?;
    Ok(())
}

/// Rewrite one city's people order from a complete list of person ids.
pub fn reorder_people(db: &Connection, clock_id: &str, ordered_ids: &[String]) -> Result<()> {
    let existing = list_people(db, clock_id)?;
    let mut expected: Vec<&str> = existing.iter().map(|p| p.id.as_str()).collect();
    expected.sort_unstable();
    let mut offered: Vec<&str> = ordered_ids.iter().map(String::as_str).collect();
    offered.sort_unstable();
    if expected != offered {
        return Err(KandriumError::new(
            "Reorder must include every person for this city exactly once",
        ));
    }

    let tx = db.unchecked_transaction()?;
    for (position, id) in ordered_ids.iter().enumerate() {
        tx.execute(
            "UPDATE clock_people SET position = ?1 WHERE id = ?2 AND home_clock_id = ?3",
            rusqlite::params![position as i64, id, clock_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Every travel row, ordered by person then start. The Clocks UI groups
/// these by person and evaluates placement per frame.
pub fn list_all_travels(db: &Connection) -> Result<Vec<Travel>> {
    let mut stmt = db.prepare(
        "SELECT id, person_id, origin_clock_id, destination_clock_id,
                starts_at, ends_at, cancelled_at, created_at
         FROM clock_travels
         ORDER BY person_id, starts_at, id",
    )?;
    let rows = stmt.query_map([], travel_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// One person's trips, oldest first.
pub fn list_travels(db: &Connection, person_id: &str) -> Result<Vec<Travel>> {
    let mut stmt = db.prepare(
        "SELECT id, person_id, origin_clock_id, destination_clock_id,
                starts_at, ends_at, cancelled_at, created_at
         FROM clock_travels
         WHERE person_id = ?1
         ORDER BY starts_at, id",
    )?;
    let rows = stmt.query_map([person_id], travel_from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn travel_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Travel> {
    Ok(Travel {
        id: row.get(0)?,
        person_id: row.get(1)?,
        origin_clock_id: row.get(2)?,
        destination_clock_id: row.get(3)?,
        starts_at: row.get(4)?,
        ends_at: row.get(5)?,
        cancelled_at: row.get(6)?,
        created_at: row.get(7)?,
    })
}

/// The active trip at `instant`, if any. Pure: callers pass cached rows and
/// the frame's single `now`. The latest start wins; a trip is active while
/// `starts_at <= instant < ends_at` (open-ended trips never expire).
pub fn placement_at(travels: &[Travel], instant: i64) -> Option<&Travel> {
    latest_active(travels.iter(), instant)
}

/// The active trip for one person at `instant`, from the whole travel cache.
pub fn placement_for<'a>(
    travels: &'a [Travel],
    person_id: &str,
    instant: i64,
) -> Option<&'a Travel> {
    latest_active(
        travels
            .iter()
            .filter(|travel| travel.person_id == person_id),
        instant,
    )
}

fn latest_active<'a>(
    travels: impl Iterator<Item = &'a Travel>,
    instant: i64,
) -> Option<&'a Travel> {
    travels
        .filter(|travel| {
            travel.cancelled_at.is_none()
                && travel.starts_at <= instant
                && travel.ends_at.is_none_or(|end| end > instant)
        })
        .max_by(|a, b| a.starts_at.cmp(&b.starts_at).then_with(|| a.id.cmp(&b.id)))
}

/// Start a temporary trip to `destination_clock_id`.
///
/// Rules (mirroring Kwora's `TravelService`):
/// * the window must be non-empty (`ends_at > starts_at` when set);
/// * a person cannot travel to the city they already occupy;
/// * an earlier open-ended trip that starts no later than this one is closed
///   at the new start instead of being rejected;
/// * any other overlap with a non-cancelled trip is refused;
/// * `origin` is where the person actually is at `max(starts_at, now)`, and
///   their home is untouched.
pub fn start_travel(
    db: &Connection,
    person_id: &str,
    destination_clock_id: &str,
    starts_at: i64,
    ends_at: Option<i64>,
) -> Result<Travel> {
    if let Some(end) = ends_at
        && end <= starts_at
    {
        return Err(KandriumError::new(
            "The return date must be after the start date.",
        ));
    }

    let home_clock_id: String = db
        .query_row(
            "SELECT home_clock_id FROM clock_people WHERE id = ?1",
            [person_id],
            |row| row.get(0),
        )
        .map_err(|_| KandriumError::new("Person not found"))?;
    let destination_exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM clocks WHERE id = ?1)",
        [destination_clock_id],
        |row| row.get(0),
    )?;
    if !destination_exists {
        return Err(KandriumError::new("Clock not found"));
    }

    let now = Timestamp::now().as_second();
    let mut travels = list_travels(db, person_id)?;

    // Where the person is at the new trip's reference instant. This is read
    // before closing an open-ended trip: travelling "back home" must still be
    // a move from the current city.
    let reference = starts_at.max(now);
    let current_clock = placement_at(&travels, reference)
        .and_then(|travel| travel.destination_clock_id.clone())
        .unwrap_or_else(|| home_clock_id.clone());
    if current_clock == destination_clock_id {
        return Err(KandriumError::new("This person is already in that city."));
    }

    // Close one earlier open-ended trip at the new start.
    let mut closed: Option<String> = None;
    if let Some(open) = travels
        .iter()
        .filter(|travel| {
            travel.cancelled_at.is_none()
                && travel.ends_at.is_none()
                && travel.starts_at <= starts_at
        })
        .max_by_key(|travel| travel.starts_at)
    {
        closed = Some(open.id.clone());
    }
    if let Some(id) = &closed
        && let Some(open) = travels.iter_mut().find(|travel| travel.id == *id)
    {
        open.ends_at = Some(starts_at);
    }

    // No two non-cancelled trips may overlap.
    let new_end = ends_at.unwrap_or(i64::MAX);
    for travel in &travels {
        if travel.cancelled_at.is_some() {
            continue;
        }
        let existing_end = travel.ends_at.unwrap_or(i64::MAX);
        if starts_at < existing_end && travel.starts_at < new_end {
            let city = clock_city(db, travel.destination_clock_id.as_deref());
            return Err(KandriumError::new(format!(
                "This overlaps an existing trip ({city})."
            )));
        }
    }

    let travel = Travel {
        id: uid(),
        person_id: person_id.to_string(),
        origin_clock_id: Some(current_clock),
        destination_clock_id: Some(destination_clock_id.to_string()),
        starts_at,
        ends_at,
        cancelled_at: None,
        created_at: Timestamp::now().to_string(),
    };

    let tx = db.unchecked_transaction()?;
    if let Some(id) = &closed {
        tx.execute(
            "UPDATE clock_travels SET ends_at = ?1 WHERE id = ?2",
            rusqlite::params![starts_at, id],
        )?;
    }
    tx.execute(
        "INSERT INTO clock_travels
             (id, person_id, origin_clock_id, destination_clock_id,
              starts_at, ends_at, cancelled_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
        rusqlite::params![
            travel.id,
            travel.person_id,
            travel.origin_clock_id,
            travel.destination_clock_id,
            travel.starts_at,
            travel.ends_at,
            travel.created_at
        ],
    )?;
    tx.commit()?;
    Ok(travel)
}

/// End a running trip now (early return). The history row stays with its
/// original window and a shortened `ends_at`.
pub fn end_travel(db: &Connection, travel_id: &str) -> Result<()> {
    let now = Timestamp::now().as_second();
    let (starts_at, ends_at, cancelled_at): (i64, Option<i64>, Option<i64>) = db
        .query_row(
            "SELECT starts_at, ends_at, cancelled_at FROM clock_travels WHERE id = ?1",
            [travel_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|_| KandriumError::new("Travel not found"))?;

    if cancelled_at.is_some() {
        return Err(KandriumError::new("This trip was already cancelled."));
    }
    if starts_at > now {
        return Err(KandriumError::new("This trip has not started yet."));
    }
    if let Some(end) = ends_at
        && end <= now
    {
        return Err(KandriumError::new("This trip has already ended."));
    }

    db.execute(
        "UPDATE clock_travels SET ends_at = ?1 WHERE id = ?2",
        rusqlite::params![now, travel_id],
    )?;
    Ok(())
}

/// Move a trip's return time (or set one on an open-ended trip).
///
/// The new end must be after the start, and it may not overlap another
/// non-cancelled trip for the same person.
pub fn extend_travel(db: &Connection, travel_id: &str, ends_at: Option<i64>) -> Result<()> {
    let travel: Travel = db
        .query_row(
            "SELECT id, person_id, origin_clock_id, destination_clock_id,
                    starts_at, ends_at, cancelled_at, created_at
             FROM clock_travels WHERE id = ?1",
            [travel_id],
            travel_from_row,
        )
        .map_err(|_| KandriumError::new("Travel not found"))?;
    if travel.cancelled_at.is_some() {
        return Err(KandriumError::new("This trip was cancelled."));
    }
    if let Some(end) = ends_at
        && end <= travel.starts_at
    {
        return Err(KandriumError::new(
            "The return date must be after the start date.",
        ));
    }

    let new_end = ends_at.unwrap_or(i64::MAX);
    for other in list_travels(db, &travel.person_id)? {
        if other.id == travel.id || other.cancelled_at.is_some() {
            continue;
        }
        let existing_end = other.ends_at.unwrap_or(i64::MAX);
        if travel.starts_at < existing_end && other.starts_at < new_end {
            let city = clock_city(db, other.destination_clock_id.as_deref());
            return Err(KandriumError::new(format!(
                "This overlaps an existing trip ({city})."
            )));
        }
    }

    db.execute(
        "UPDATE clock_travels SET ends_at = ?1 WHERE id = ?2",
        rusqlite::params![ends_at, travel.id],
    )?;
    Ok(())
}

/// Delete a travel row outright (used to discard a mistake or a finished
/// entry from the history).
pub fn remove_travel(db: &Connection, travel_id: &str) -> Result<()> {
    let changed = db.execute("DELETE FROM clock_travels WHERE id = ?1", [travel_id])?;
    if changed == 0 {
        return Err(KandriumError::new("Travel not found"));
    }
    Ok(())
}

/// Change a person's resident city.
///
/// Active trips whose destination is the old or the new home are cancelled:
/// after the move they would either be meaningless (new home) or point back
/// at a place the person no longer lives (old home). Other active trips stay
/// and revert to the new home when they end. The person is appended to the
/// new city's order and their old city's order is compacted.
pub fn relocate_person(db: &Connection, person_id: &str, new_home_clock_id: &str) -> Result<()> {
    let (old_home, old_position): (String, i64) = db
        .query_row(
            "SELECT home_clock_id, position FROM clock_people WHERE id = ?1",
            [person_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| KandriumError::new("Person not found"))?;
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM clocks WHERE id = ?1)",
        [new_home_clock_id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(KandriumError::new("Clock not found"));
    }
    if old_home == new_home_clock_id {
        return Ok(());
    }

    let new_position: i64 = db.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM clock_people WHERE home_clock_id = ?1",
        [new_home_clock_id],
        |row| row.get(0),
    )?;
    let now = Timestamp::now().as_second();

    let tx = db.unchecked_transaction()?;
    tx.execute(
        "UPDATE clock_travels SET cancelled_at = ?3
         WHERE person_id = ?1
           AND cancelled_at IS NULL
           AND (ends_at IS NULL OR ends_at > ?3)
           AND destination_clock_id IN (?4, ?2)",
        rusqlite::params![person_id, new_home_clock_id, now, old_home],
    )?;
    tx.execute(
        "UPDATE clock_people SET home_clock_id = ?1, position = ?2 WHERE id = ?3",
        rusqlite::params![new_home_clock_id, new_position, person_id],
    )?;
    tx.execute(
        "UPDATE clock_people SET position = position - 1
         WHERE home_clock_id = ?1 AND position > ?2",
        rusqlite::params![old_home, old_position],
    )?;
    tx.commit()?;
    Ok(())
}

fn clock_city(db: &Connection, clock_id: Option<&str>) -> String {
    let Some(clock_id) = clock_id else {
        return "another city".to_string();
    };
    db.query_row(
        "SELECT tz_identifier FROM clocks WHERE id = ?1",
        [clock_id],
        |row| row.get::<_, String>(0),
    )
    .map(|identifier| city_name(&identifier))
    .unwrap_or_else(|_| "another city".to_string())
}

/// Start of `date` in `time_zone`, as Unix epoch seconds.
///
/// Used by the travel form: Kwora resolves the effective day in the
/// destination zone. Midnight always exists, so the only failure is a broken
/// zone handle.
pub fn day_start_seconds(date: Date, time_zone: &TimeZone) -> Result<i64> {
    date.to_zoned(time_zone.clone())
        .map(|zoned| zoned.timestamp().as_second())
        .map_err(|error| KandriumError::new(format!("Invalid travel date: {error}")))
}

/// End of `date` (23:59:59) in `time_zone`, as Unix epoch seconds.
pub fn day_end_seconds(date: Date, time_zone: &TimeZone) -> Result<i64> {
    date.at(23, 59, 59, 0)
        .to_zoned(time_zone.clone())
        .map(|zoned| zoned.timestamp().as_second())
        .map_err(|error| KandriumError::new(format!("Invalid travel date: {error}")))
}

/// Seed the reference city trio when the table is empty.
///
/// Called from [`crate::seed::seed`], which only runs on a database with no
/// boards (i.e. a fresh install). The guard here additionally makes a
/// repeated call a no-op.
pub fn seed_defaults(db: &Connection) -> Result<()> {
    let count: i64 = db.query_row("SELECT COUNT(*) FROM clocks", [], |row| row.get(0))?;
    if count > 0 {
        return Ok(());
    }

    // No transaction here: `seed` already wraps the call in one, and the
    // empty-table guard above makes each insert run at most once.
    for (position, (primary, fallback)) in DEFAULT_CLOCK_FALLBACK.iter().enumerate() {
        let Some(identifier) = resolve_zone(primary)
            .map(|_| *primary)
            .or_else(|| resolve_zone(fallback).map(|_| *fallback))
        else {
            continue;
        };
        db.execute(
            "INSERT INTO clocks (id, tz_identifier, position, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                uid(),
                identifier,
                position as i64,
                Timestamp::now().to_string()
            ],
        )?;
    }
    Ok(())
}

/// Resolve an IANA identifier to a `jiff` time zone, or `None` when the
/// runtime database does not know it.
pub fn resolve_zone(identifier: &str) -> Option<TimeZone> {
    TimeZone::get(identifier.trim()).ok()
}

/// All IANA identifiers known to the bundled time-zone database, sorted.
///
/// Used by the app's add-city picker; the list is stable for the life of the
/// process, so callers may cache it.
pub fn available_zones() -> Vec<String> {
    let mut zones: Vec<String> = jiff::tz::db()
        .available()
        .map(|name| name.as_str().to_string())
        .collect();
    zones.sort_unstable();
    zones.dedup();
    zones
}

/// Case-insensitive match of a query against an identifier, its city segment
/// or its region, with `_` and `/` treated as spaces. An empty query matches
/// everything.
pub fn zone_matches(identifier: &str, query: &str) -> bool {
    let query = normalize_zone_text(query);
    if query.is_empty() {
        return true;
    }
    let region = identifier.split('/').next().unwrap_or(identifier);
    [identifier, &city_name(identifier), region]
        .iter()
        .any(|candidate| normalize_zone_text(candidate).contains(&query))
}

fn normalize_zone_text(value: &str) -> String {
    value.replace(['_', '/'], " ").trim().to_lowercase()
}

/// Last path segment of an IANA identifier, with underscores as spaces:
/// `America/Los_Angeles` → `Los Angeles`.
pub fn city_name(tz_identifier: &str) -> String {
    tz_identifier
        .rsplit('/')
        .next()
        .unwrap_or(tz_identifier)
        .replace('_', " ")
}

/// Format one city at `now`. Pure: no clock or database access.
pub fn format_clock(now: Timestamp, time_zone: &TimeZone, use_24h: bool) -> ClockText {
    let zoned = now.to_zoned(time_zone.clone());
    let (time, period) = if use_24h {
        (zoned.strftime("%H:%M").to_string(), None)
    } else {
        (
            zoned.strftime("%-I:%M").to_string(),
            Some(zoned.strftime("%P").to_string()),
        )
    };
    ClockText {
        time,
        period,
        date: zoned.strftime("%a, %-d %b.").to_string(),
        offset: format_offset(zoned.offset().seconds()),
    }
}

// ---------------------------------------------------------------------------
// Time probe
// ---------------------------------------------------------------------------

/// Why a typed probe time could not be resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeError {
    /// The field was committed with no text.
    Empty,
    /// The text is not a time this parser understands.
    Format,
    /// The time parsed but no such wall clock exists on that date
    /// (`24:00`, `12:60`, `13:00 pm`, `0:00 am`).
    OutOfRange,
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::Empty => f.write_str("Enter a time like 14:30"),
            ProbeError::Format => f.write_str("Enter a time like 14:30"),
            ProbeError::OutOfRange => f.write_str("Enter a time between 00:00 and 23:59"),
        }
    }
}

/// Whether a wall-clock time is unambiguous, skipped or repeated in a zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeAmbiguity {
    /// Exactly one instant matches the wall clock.
    Unique,
    /// Spring forward: the wall clock never occurs on that date.
    Gap,
    /// Fall back: the wall clock occurs twice on that date.
    Fold,
}

/// Today's date in `zone`, derived from the live instant.
pub fn probe_date(now: Timestamp, zone: &TimeZone) -> Date {
    now.to_zoned(zone.clone()).date()
}

/// Parse `text` as a wall-clock time on `date` in `zone`.
///
/// Accepts `H:mm`, `HH:mm`, `H.mm`, `HH.mm` and the 12-hour shapes `H:mm am`,
/// `H am`, `H:mm pm` (the space is optional and the suffix is
/// case-insensitive). 24-hour digits are accepted regardless of the app's
/// preference, so `14:30` always means the same wall clock.
///
/// Ambiguous wall clocks resolve with jiff's `Disambiguation::Compatible`
/// strategy — a gap shifts forward to the first real time after it, a fold
/// takes the earlier occurrence — because that is what `to_zoned` does.
pub fn parse_probe_time(
    text: &str,
    date: Date,
    zone: &TimeZone,
) -> std::result::Result<Timestamp, ProbeError> {
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return Err(ProbeError::Empty);
    }

    let (clock, period) = if let Some(rest) = text.strip_suffix("am") {
        (rest.trim(), Some(false))
    } else if let Some(rest) = text.strip_suffix("pm") {
        (rest.trim(), Some(true))
    } else {
        (text.as_str(), None)
    };

    let (hour_text, minute_text) = match clock.split_once([':', '.']) {
        Some((hour, minute)) => (hour.trim(), Some(minute.trim())),
        None => (clock.trim(), None),
    };
    if hour_text.is_empty() || hour_text.len() > 2 || !hour_text.chars().all(|c| c.is_ascii_digit())
    {
        return Err(ProbeError::Format);
    }
    let hour: i64 = hour_text.parse().map_err(|_| ProbeError::Format)?;
    let minute: i64 = match minute_text {
        Some(minute) => {
            if minute.is_empty() || minute.len() > 2 || !minute.chars().all(|c| c.is_ascii_digit())
            {
                return Err(ProbeError::Format);
            }
            minute.parse().map_err(|_| ProbeError::Format)?
        }
        // `2 pm` means the top of the hour; a bare `2` stays ambiguous.
        None if period.is_some() => 0,
        None => return Err(ProbeError::Format),
    };
    if minute > 59 {
        return Err(ProbeError::OutOfRange);
    }

    let hour = match period {
        Some(pm) => {
            if !(1..=12).contains(&hour) {
                return Err(ProbeError::OutOfRange);
            }
            match (pm, hour) {
                (false, 12) => 0,
                (true, 12) => 12,
                (false, hour) => hour,
                (true, hour) => hour + 12,
            }
        }
        None => {
            if hour > 23 {
                return Err(ProbeError::OutOfRange);
            }
            hour
        }
    };

    let time = jiff::civil::Time::new(hour as i8, minute as i8, 0, 0)
        .map_err(|_| ProbeError::OutOfRange)?;
    date.to_datetime(time)
        .to_zoned(zone.clone())
        .map(|zoned| zoned.timestamp())
        .map_err(|_| ProbeError::OutOfRange)
}

/// Classify a civil time in `zone` before it is disambiguated.
pub fn probe_ambiguity(date: Date, time: jiff::civil::Time, zone: &TimeZone) -> ProbeAmbiguity {
    match zone.to_ambiguous_zoned(date.to_datetime(time)).offset() {
        jiff::tz::AmbiguousOffset::Unambiguous { .. } => ProbeAmbiguity::Unique,
        jiff::tz::AmbiguousOffset::Gap { .. } => ProbeAmbiguity::Gap,
        jiff::tz::AmbiguousOffset::Fold { .. } => ProbeAmbiguity::Fold,
    }
}

/// The zone the app runs in. Captured once when a probe is opened.
pub fn system_zone() -> TimeZone {
    TimeZone::system()
}

/// `0` → `GMT`; otherwise `GMT` plus whole hours and, when present, minutes.
pub fn format_offset(seconds: i32) -> String {
    if seconds == 0 {
        return "GMT".to_string();
    }
    let sign = if seconds < 0 { '-' } else { '+' };
    let total_minutes = seconds.unsigned_abs() / 60;
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    if minutes == 0 {
        format!("GMT{sign}{hours}")
    } else {
        format!("GMT{sign}{hours}:{minutes:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_db;
    use crate::seed::seed;

    fn instant(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn zone(identifier: &str) -> TimeZone {
        resolve_zone(identifier).unwrap()
    }

    fn mem_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("clocks.db")).unwrap();
        crate::db::run_schema(&mut db).unwrap();
        (dir, db)
    }

    #[test]
    fn formatting_follows_spring_forward_in_new_york() {
        let tz = zone("America/New_York");
        let before = format_clock(instant("2026-03-08T06:59:00Z"), &tz, false);
        assert_eq!(before.time, "1:59");
        assert_eq!(before.period.as_deref(), Some("am"));
        assert_eq!(before.offset, "GMT-5");
        assert_eq!(before.date, "Sun, 8 Mar.");

        // 02:00 local does not exist; the clock jumps to 03:00 EDT.
        let after = format_clock(instant("2026-03-08T07:00:00Z"), &tz, false);
        assert_eq!(after.time, "3:00");
        assert_eq!(after.offset, "GMT-4");
    }

    #[test]
    fn formatting_follows_fall_back_in_new_york() {
        let tz = zone("America/New_York");
        let before = format_clock(instant("2026-11-01T05:59:00Z"), &tz, false);
        assert_eq!(before.time, "1:59");
        assert_eq!(before.offset, "GMT-4");

        let after = format_clock(instant("2026-11-01T06:00:00Z"), &tz, false);
        assert_eq!(after.time, "1:00");
        assert_eq!(after.offset, "GMT-5");
    }

    #[test]
    fn formatting_handles_half_hour_offsets_and_24h_mode() {
        let kolkata = format_clock(
            instant("2026-01-01T00:00:00Z"),
            &zone("Asia/Kolkata"),
            false,
        );
        assert_eq!(kolkata.time, "5:30");
        assert_eq!(kolkata.period.as_deref(), Some("am"));
        assert_eq!(kolkata.offset, "GMT+5:30");

        let tokyo = format_clock(instant("2026-01-01T00:00:00Z"), &zone("Asia/Tokyo"), true);
        assert_eq!(tokyo.time, "09:00");
        assert_eq!(tokyo.period, None);
        assert_eq!(tokyo.offset, "GMT+9");
        assert_eq!(tokyo.date, "Thu, 1 Jan.");
    }

    #[test]
    fn formatting_renders_midnight_and_noon_in_both_modes() {
        let utc = zone("UTC");
        let midnight = format_clock(instant("2026-11-05T00:00:00Z"), &utc, false);
        assert_eq!(midnight.time, "12:00");
        assert_eq!(midnight.period.as_deref(), Some("am"));
        let midnight_24h = format_clock(instant("2026-11-05T00:00:00Z"), &utc, true);
        assert_eq!(midnight_24h.time, "00:00");
        assert_eq!(midnight_24h.period, None);

        let noon = format_clock(instant("2026-11-05T12:00:00Z"), &utc, false);
        assert_eq!(noon.time, "12:00");
        assert_eq!(noon.period.as_deref(), Some("pm"));
        let noon_24h = format_clock(instant("2026-11-05T12:00:00Z"), &utc, true);
        assert_eq!(noon_24h.time, "12:00");
        assert_eq!(noon_24h.period, None);
    }

    #[test]
    fn offset_formatting_covers_zero_whole_and_partial_hours() {
        assert_eq!(format_offset(0), "GMT");
        assert_eq!(format_offset(3600), "GMT+1");
        assert_eq!(format_offset(-4 * 3600), "GMT-4");
        assert_eq!(format_offset(5 * 3600 + 1800), "GMT+5:30");
        assert_eq!(format_offset(-(3 * 3600 + 1800)), "GMT-3:30");
    }

    #[test]
    fn add_list_and_remove_keep_a_dense_order() {
        let (_dir, db) = mem_db();
        let tokyo = add_clock(&db, "Asia/Tokyo").unwrap();
        let paris = add_clock(&db, "Europe/Paris").unwrap();
        let la = add_clock(&db, "America/Los_Angeles").unwrap();
        assert_eq!(
            list_clocks(&db)
                .unwrap()
                .iter()
                .map(|c| c.position)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );

        remove_clock(&db, &paris.id).unwrap();
        let rest = list_clocks(&db).unwrap();
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0].id, tokyo.id);
        assert_eq!(rest[0].position, 0);
        assert_eq!(rest[1].id, la.id);
        assert_eq!(rest[1].position, 1);
    }

    #[test]
    fn reorder_requires_a_complete_permutation() {
        let (_dir, db) = mem_db();
        let tokyo = add_clock(&db, "Asia/Tokyo").unwrap();
        let paris = add_clock(&db, "Europe/Paris").unwrap();

        let reversed = vec![paris.id.clone(), tokyo.id.clone()];
        reorder_clocks(&db, &reversed).unwrap();
        let rows = list_clocks(&db).unwrap();
        assert_eq!(rows[0].id, paris.id);
        assert_eq!(rows[1].id, tokyo.id);

        assert!(reorder_clocks(&db, std::slice::from_ref(&tokyo.id)).is_err());
        let duplicate = vec![tokyo.id.clone(), tokyo.id.clone()];
        assert!(reorder_clocks(&db, &duplicate).is_err());
    }

    #[test]
    fn add_rejects_unknown_and_duplicate_zones() {
        let (_dir, db) = mem_db();
        assert!(add_clock(&db, "Mars/Olympus_Mons").is_err());
        assert!(add_clock(&db, "Asia/Tokyo").is_ok());
        assert!(add_clock(&db, "Asia/Tokyo").is_err());
    }

    #[test]
    fn fresh_install_seeds_the_reference_trio() {
        let (_dir, mut db) = mem_db();
        crate::db::run_schema(&mut db).unwrap();
        seed(&db).unwrap();
        let clocks = list_clocks(&db).unwrap();
        assert_eq!(clocks.len(), 3);
        assert_eq!(clocks[0].tz_identifier, "Europe/Kyiv");
        assert_eq!(clocks[1].tz_identifier, "Asia/Tokyo");
        assert_eq!(clocks[2].tz_identifier, "America/Los_Angeles");
    }

    #[test]
    fn existing_database_with_boards_is_not_seeded() {
        let (_dir, mut db) = mem_db();
        crate::db::run_schema(&mut db).unwrap();
        db.execute(
            "INSERT INTO boards (id, title, position) VALUES ('board_custom', 'Custom', 0)",
            [],
        )
        .unwrap();

        seed(&db).unwrap();
        assert!(list_clocks(&db).unwrap().is_empty());
    }

    #[test]
    fn seed_defaults_is_idempotent() {
        let (_dir, mut db) = mem_db();
        crate::db::run_schema(&mut db).unwrap();
        seed_defaults(&db).unwrap();
        seed_defaults(&db).unwrap();
        assert_eq!(list_clocks(&db).unwrap().len(), 3);
    }

    #[test]
    fn people_add_rename_remove_and_keep_a_dense_order() {
        let (_dir, db) = mem_db();
        let tokyo = add_clock(&db, "Asia/Tokyo").unwrap();
        let paris = add_clock(&db, "Europe/Paris").unwrap();
        let ana = add_person(&db, &tokyo.id, "  Ana  ").unwrap();
        let ben = add_person(&db, &tokyo.id, "Ben").unwrap();
        let cleo = add_person(&db, &paris.id, "Cleo").unwrap();
        assert_eq!(ana.name, "Ana");
        assert_eq!(ana.position, 0);
        assert_eq!(ben.position, 1);
        // Positions are per city, not global.
        assert_eq!(cleo.position, 0);
        assert_eq!(list_people(&db, &tokyo.id).unwrap().len(), 2);
        assert_eq!(list_all_people(&db).unwrap().len(), 3);

        rename_person(&db, &ana.id, "Ann").unwrap();
        assert_eq!(list_people(&db, &tokyo.id).unwrap()[0].name, "Ann");

        remove_person(&db, &ana.id).unwrap();
        let rest = list_people(&db, &tokyo.id).unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id, ben.id);
        assert_eq!(rest[0].position, 0);

        assert!(rename_person(&db, &ana.id, "Ghost").is_err());
        assert!(remove_person(&db, &ana.id).is_err());
    }

    #[test]
    fn add_person_rejects_blank_names_and_unknown_clocks() {
        let (_dir, db) = mem_db();
        let tokyo = add_clock(&db, "Asia/Tokyo").unwrap();
        assert!(add_person(&db, &tokyo.id, "   ").is_err());
        assert!(add_person(&db, "clock_missing", "Ana").is_err());
        let ana = add_person(&db, &tokyo.id, "Ana").unwrap();
        assert!(rename_person(&db, &ana.id, " ").is_err());
    }

    #[test]
    fn reorder_people_requires_the_full_city_permutation() {
        let (_dir, db) = mem_db();
        let tokyo = add_clock(&db, "Asia/Tokyo").unwrap();
        let paris = add_clock(&db, "Europe/Paris").unwrap();
        let ana = add_person(&db, &tokyo.id, "Ana").unwrap();
        let ben = add_person(&db, &tokyo.id, "Ben").unwrap();
        let cleo = add_person(&db, &paris.id, "Cleo").unwrap();

        reorder_people(&db, &tokyo.id, &[ben.id.clone(), ana.id.clone()]).unwrap();
        let rows = list_people(&db, &tokyo.id).unwrap();
        assert_eq!(rows[0].id, ben.id);
        assert_eq!(rows[1].id, ana.id);

        assert!(reorder_people(&db, &tokyo.id, std::slice::from_ref(&ana.id)).is_err());
        // A complete but foreign list is rejected too.
        assert!(
            reorder_people(
                &db,
                &paris.id,
                &[ana.id.clone(), ben.id.clone(), cleo.id.clone()]
            )
            .is_err()
        );
    }

    /// `ON DELETE RESTRICT`: a city keeps its residents, so the core guard
    /// refuses the delete until the last person is removed.
    #[test]
    fn removing_a_clock_is_blocked_while_it_has_residents() {
        let (_dir, db) = mem_db();
        let tokyo = add_clock(&db, "Asia/Tokyo").unwrap();
        let ana = add_person(&db, &tokyo.id, "Ana").unwrap();

        let err = remove_clock(&db, &tokyo.id).unwrap_err();
        assert!(err.to_string().contains("Remove them first"));
        assert_eq!(list_clocks(&db).unwrap().len(), 1);

        remove_person(&db, &ana.id).unwrap();
        remove_clock(&db, &tokyo.id).unwrap();
        assert!(list_clocks(&db).unwrap().is_empty());
    }

    #[test]
    fn seeding_clocks_never_seeds_people() {
        let (_dir, mut db) = mem_db();
        crate::db::run_schema(&mut db).unwrap();
        seed(&db).unwrap();
        assert_eq!(list_clocks(&db).unwrap().len(), 3);
        assert!(list_all_people(&db).unwrap().is_empty());
    }

    #[test]
    fn available_zones_are_sorted_deduplicated_and_real() {
        let zones = available_zones();
        assert!(!zones.is_empty());
        assert!(zones.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(zones.iter().any(|z| z == "Asia/Tokyo"));
        assert!(zones.iter().any(|z| z == "America/Los_Angeles"));
    }

    #[test]
    fn zone_query_matches_city_region_and_identifier() {
        assert!(zone_matches("America/Los_Angeles", "los angeles"));
        assert!(zone_matches("America/Los_Angeles", "america"));
        assert!(zone_matches("America/Los_Angeles", "los_angeles"));
        assert!(zone_matches("Asia/Tokyo", "tok"));
        assert!(!zone_matches("Asia/Tokyo", "paris"));
        assert!(zone_matches("Asia/Tokyo", ""));
    }

    #[test]
    fn city_name_uses_the_last_identifier_segment() {
        assert_eq!(city_name("America/Los_Angeles"), "Los Angeles");
        assert_eq!(city_name("Asia/Tokyo"), "Tokyo");
        assert_eq!(city_name("UTC"), "UTC");
    }

    fn now_secs() -> i64 {
        Timestamp::now().as_second()
    }

    fn travel_at(
        id: &str,
        person: &str,
        dest: Option<&str>,
        start: i64,
        end: Option<i64>,
    ) -> Travel {
        Travel {
            id: id.to_string(),
            person_id: person.to_string(),
            origin_clock_id: None,
            destination_clock_id: dest.map(str::to_string),
            starts_at: start,
            ends_at: end,
            cancelled_at: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn placement_picks_the_latest_active_trip_and_expires() {
        let trips = vec![
            travel_at("t1", "p1", Some("c_paris"), 100, Some(200)),
            travel_at("t2", "p1", Some("c_berlin"), 200, Some(400)),
        ];
        assert!(placement_at(&trips, 99).is_none());
        assert_eq!(placement_at(&trips, 100).unwrap().id, "t1");
        // End is exclusive: at 200 the next trip already owns the person.
        assert_eq!(placement_at(&trips, 200).unwrap().id, "t2");
        assert_eq!(placement_at(&trips, 399).unwrap().id, "t2");
        assert!(placement_at(&trips, 400).is_none());

        let mut open = travel_at("t3", "p1", Some("c_paris"), 500, None);
        assert_eq!(
            placement_at(std::slice::from_ref(&open), 9_000).unwrap().id,
            "t3"
        );
        open.cancelled_at = Some(600);
        assert!(placement_at(&[open], 700).is_none());
    }

    #[test]
    fn placing_the_latest_start_wins_when_two_trips_overlap() {
        // The transaction rules prevent this, but the pure resolver is
        // defensive: legacy rows may overlap after an external edit.
        let trips = vec![
            travel_at("early", "p1", Some("c_paris"), 100, Some(400)),
            travel_at("late", "p1", Some("c_berlin"), 200, Some(300)),
        ];
        assert_eq!(placement_at(&trips, 250).unwrap().id, "late");
    }

    #[test]
    fn travel_keeps_home_and_captures_the_origin() {
        let (_dir, db) = mem_db();
        let tokyo = add_clock(&db, "Asia/Tokyo").unwrap();
        let paris = add_clock(&db, "Europe/Paris").unwrap();
        let ana = add_person(&db, &tokyo.id, "Ana").unwrap();

        let now = now_secs();
        let travel = start_travel(&db, &ana.id, &paris.id, now - 60, Some(now + 600)).unwrap();
        assert_eq!(travel.origin_clock_id.as_deref(), Some(tokyo.id.as_str()));
        assert_eq!(
            travel.destination_clock_id.as_deref(),
            Some(paris.id.as_str())
        );
        // Home is untouched by travel.
        assert_eq!(
            list_people(&db, &tokyo.id).unwrap()[0].home_clock_id,
            tokyo.id
        );
        assert_eq!(
            placement_at(&list_travels(&db, &ana.id).unwrap(), now)
                .unwrap()
                .id,
            travel.id
        );
    }

    #[test]
    fn start_travel_validates_window_city_and_overlap() {
        let (_dir, db) = mem_db();
        let tokyo = add_clock(&db, "Asia/Tokyo").unwrap();
        let paris = add_clock(&db, "Europe/Paris").unwrap();
        let berlin = add_clock(&db, "Europe/Berlin").unwrap();
        let ana = add_person(&db, &tokyo.id, "Ana").unwrap();
        let now = now_secs();

        assert!(
            start_travel(&db, &ana.id, &paris.id, now, Some(now))
                .unwrap_err()
                .to_string()
                .contains("after the start")
        );
        assert!(start_travel(&db, &ana.id, &tokyo.id, now, None).is_err());
        assert!(start_travel(&db, "person_missing", &paris.id, now, None).is_err());
        assert!(start_travel(&db, &ana.id, "clock_missing", now, None).is_err());

        // An earlier ended trip blocks an overlapping window and names its
        // city.
        let first = start_travel(&db, &ana.id, &paris.id, now - 100, Some(now + 100)).unwrap();
        let err = start_travel(&db, &ana.id, &berlin.id, now - 50, Some(now + 200)).unwrap_err();
        assert!(err.to_string().contains("Paris"), "{err}");
        // …and travelling to the city already occupied is refused.
        let err = start_travel(&db, &ana.id, &paris.id, now - 1_000, Some(now - 900)).unwrap_err();
        assert!(err.to_string().contains("already in that city"), "{err}");

        // A new trip closes an earlier open-ended one at its start, and
        // travelling back home is still a move from the current city.
        let open = start_travel(&db, &ana.id, &paris.id, now + 700, None).unwrap();
        let later = start_travel(&db, &ana.id, &tokyo.id, now + 900, None).unwrap();
        let rows = list_travels(&db, &ana.id).unwrap();
        let closed = rows.iter().find(|row| row.id == open.id).unwrap();
        assert_eq!(closed.ends_at, Some(now + 900));
        assert_eq!(later.origin_clock_id.as_deref(), Some(paris.id.as_str()));
        assert_eq!(first.ends_at, Some(now + 100));
    }

    #[test]
    fn end_extend_and_remove_travel() {
        let (_dir, db) = mem_db();
        let tokyo = add_clock(&db, "Asia/Tokyo").unwrap();
        let paris = add_clock(&db, "Europe/Paris").unwrap();
        let berlin = add_clock(&db, "Europe/Berlin").unwrap();
        let ana = add_person(&db, &tokyo.id, "Ana").unwrap();
        let now = now_secs();

        let running = start_travel(&db, &ana.id, &paris.id, now - 100, Some(now + 500)).unwrap();
        end_travel(&db, &running.id).unwrap();
        let ended = list_travels(&db, &ana.id)
            .unwrap()
            .into_iter()
            .find(|row| row.id == running.id)
            .unwrap();
        assert!(ended.ends_at.is_some_and(|end| end <= now_secs()));
        assert!(placement_at(&[ended], now).is_none());
        assert!(end_travel(&db, &running.id).is_err());
        assert!(end_travel(&db, "travel_missing").is_err());

        // A future trip cannot be ended early.
        let future = start_travel(&db, &ana.id, &berlin.id, now + 1_000, None).unwrap();
        assert!(end_travel(&db, &future.id).is_err());

        // Extending a planned trip respects the window and other trips. The
        // open-ended Berlin trip is closed at the Paris trip's start.
        let second = start_travel(&db, &ana.id, &paris.id, now + 2_000, Some(now + 3_000)).unwrap();
        extend_travel(&db, &future.id, Some(now + 1_500)).unwrap();
        let err = extend_travel(&db, &future.id, Some(now + 2_500)).unwrap_err();
        assert!(err.to_string().contains("Paris"), "{err}");
        assert!(extend_travel(&db, &second.id, Some(now + 1_000)).is_err());
        extend_travel(&db, &second.id, None).unwrap();

        remove_travel(&db, &second.id).unwrap();
        assert!(remove_travel(&db, &second.id).is_err());
        assert_eq!(list_travels(&db, &ana.id).unwrap().len(), 2);
    }

    #[test]
    fn removing_a_person_cascades_travel_history() {
        let (_dir, db) = mem_db();
        let tokyo = add_clock(&db, "Asia/Tokyo").unwrap();
        let paris = add_clock(&db, "Europe/Paris").unwrap();
        let ana = add_person(&db, &tokyo.id, "Ana").unwrap();
        let now = now_secs();
        start_travel(&db, &ana.id, &paris.id, now - 60, None).unwrap();

        remove_person(&db, &ana.id).unwrap();
        assert!(list_all_travels(&db).unwrap().is_empty());
    }

    /// Residency keeps `RESTRICT`, but a city that is only a travel
    /// destination may be deleted: its active trips are cancelled first and
    /// the history row survives with a NULL destination.
    #[test]
    fn removing_a_destination_clock_cancels_active_visits_and_keeps_history() {
        let (_dir, db) = mem_db();
        let tokyo = add_clock(&db, "Asia/Tokyo").unwrap();
        let paris = add_clock(&db, "Europe/Paris").unwrap();
        let ana = add_person(&db, &tokyo.id, "Ana").unwrap();
        let now = now_secs();
        let travel = start_travel(&db, &ana.id, &paris.id, now - 60, None).unwrap();

        remove_clock(&db, &paris.id).unwrap();

        let rows = list_all_travels(&db).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, travel.id);
        assert!(rows[0].cancelled_at.is_some());
        assert_eq!(rows[0].destination_clock_id, None);
        // The resident is back home and still blocks deleting Tokyo.
        assert!(placement_at(&rows, now).is_none());
        assert!(remove_clock(&db, &tokyo.id).is_err());
    }

    #[test]
    fn relocating_cancels_trips_to_the_old_and_new_home() {
        let (_dir, db) = mem_db();
        let tokyo = add_clock(&db, "Asia/Tokyo").unwrap();
        let paris = add_clock(&db, "Europe/Paris").unwrap();
        let berlin = add_clock(&db, "Europe/Berlin").unwrap();
        let ana = add_person(&db, &tokyo.id, "Ana").unwrap();
        let now = now_secs();

        let paris_trip = start_travel(&db, &ana.id, &paris.id, now - 60, None).unwrap();
        let tokyo_trip =
            start_travel(&db, &ana.id, &tokyo.id, now + 1_000, Some(now + 2_000)).unwrap();

        // Move her to Berlin: the trip back to the old home is pointless, and
        // the one to the new home would be a no-op, so both are cancelled.
        relocate_person(&db, &ana.id, &berlin.id).unwrap();

        let rows = list_travels(&db, &ana.id).unwrap();
        let by_id = |id: &str| rows.iter().find(|row| row.id == id).unwrap();
        assert!(by_id(&tokyo_trip.id).cancelled_at.is_some());
        assert!(by_id(&paris_trip.id).cancelled_at.is_none());
        assert_eq!(list_people(&db, &berlin.id).unwrap()[0].id, ana.id);
        assert!(list_people(&db, &tokyo.id).unwrap().is_empty());
        // Moving to the current home is a no-op.
        relocate_person(&db, &ana.id, &berlin.id).unwrap();
        assert!(relocate_person(&db, &ana.id, "clock_missing").is_err());

        // A trip to the new home is cancelled by the move itself.
        let ana2 = add_person(&db, &berlin.id, "Ben").unwrap();
        let berlin_trip = start_travel(&db, &ana2.id, &paris.id, now - 60, None).unwrap();
        relocate_person(&db, &ana2.id, &paris.id).unwrap();
        let rows = list_travels(&db, &ana2.id).unwrap();
        assert!(
            rows.iter()
                .find(|row| row.id == berlin_trip.id)
                .unwrap()
                .cancelled_at
                .is_some()
        );
    }

    #[test]
    fn travel_day_bounds_resolve_in_the_destination_zone() {
        let new_york = zone("America/New_York");
        let start = date("2026-03-08");
        let end = date("2026-03-08");
        let start_secs = day_start_seconds(start, &new_york).unwrap();
        let end_secs = day_end_seconds(end, &new_york).unwrap();
        // EST midnight; the day ends after the spring-forward jump in EDT.
        assert_eq!(start_secs, instant("2026-03-08T05:00:00Z").as_second());
        assert_eq!(end_secs, instant("2026-03-09T03:59:59Z").as_second());

        let kolkata = zone("Asia/Kolkata");
        assert_eq!(
            day_start_seconds(date("2026-01-01"), &kolkata).unwrap(),
            instant("2025-12-31T18:30:00Z").as_second()
        );
    }

    fn date(value: &str) -> Date {
        value.parse().unwrap()
    }

    fn probe_at(text: &str, day: &str, identifier: &str) -> Timestamp {
        parse_probe_time(text, date(day), &zone(identifier)).expect("parse probe time")
    }

    #[test]
    fn probe_time_accepts_both_clock_shapes_and_rejects_junk() {
        let utc = zone("UTC");
        let day = date("2026-11-05");
        for text in ["9:05", "09:05", "9.05", "2:30 pm", "2:30pm", "2 pm"] {
            assert!(
                parse_probe_time(text, day, &utc).is_ok(),
                "should accept {text}"
            );
        }
        assert_eq!(
            probe_at("2:30 pm", "2026-11-05", "UTC").as_second(),
            instant("2026-11-05T14:30:00Z").as_second()
        );
        assert_eq!(
            probe_at("2 pm", "2026-11-05", "UTC").as_second(),
            instant("2026-11-05T14:00:00Z").as_second()
        );

        for text in [
            "", "  ", "abc", "24:00", "12:60", "0:00 pm", "13:00 pm", "2",
        ] {
            let result = parse_probe_time(text, day, &utc);
            assert!(result.is_err(), "should reject {text:?}: {result:?}");
        }
        assert_eq!(parse_probe_time("", day, &utc), Err(ProbeError::Empty));
        assert_eq!(
            parse_probe_time("24:00", day, &utc),
            Err(ProbeError::OutOfRange)
        );
        assert_eq!(
            parse_probe_time("13:00 pm", day, &utc),
            Err(ProbeError::OutOfRange)
        );
        assert_eq!(parse_probe_time("abc", day, &utc), Err(ProbeError::Format));
    }

    #[test]
    fn probe_date_reads_the_probe_zone_not_the_local_one() {
        let at = instant("2026-03-08T04:30:00Z");
        assert_eq!(probe_date(at, &zone("Europe/Paris")), date("2026-03-08"));
        assert_eq!(
            probe_date(at, &zone("America/Los_Angeles")),
            date("2026-03-07")
        );
    }

    #[test]
    fn probe_resolves_a_dst_gap_forward_and_reports_it() {
        let new_york = zone("America/New_York");
        // 02:30 does not exist on 2026-03-08; the compatible strategy lands on
        // the first real wall clock after the jump.
        assert_eq!(
            probe_at("2:30", "2026-03-08", "America/New_York").as_second(),
            instant("2026-03-08T07:30:00Z").as_second()
        );
        assert_eq!(
            probe_ambiguity(
                date("2026-03-08"),
                jiff::civil::Time::new(2, 30, 0, 0).unwrap(),
                &new_york
            ),
            ProbeAmbiguity::Gap
        );
    }

    #[test]
    fn probe_picks_the_earlier_occurrence_in_a_fold_and_reports_it() {
        let new_york = zone("America/New_York");
        let time = jiff::civil::Time::new(1, 30, 0, 0).unwrap();
        let day = date("2026-11-01");
        assert_eq!(
            probe_at("1:30", "2026-11-01", "America/New_York").as_second(),
            instant("2026-11-01T05:30:00Z").as_second()
        );
        assert_eq!(probe_ambiguity(day, time, &new_york), ProbeAmbiguity::Fold);
        // The second occurrence is exactly one hour later.
        assert_eq!(
            new_york
                .to_ambiguous_zoned(day.to_datetime(time))
                .later()
                .unwrap()
                .timestamp()
                .as_second(),
            instant("2026-11-01T06:30:00Z").as_second()
        );
    }

    #[test]
    fn probe_handles_half_hour_zones_and_round_trips_formatting() {
        assert_eq!(
            probe_at("9:00", "2026-01-01", "Asia/Kolkata").as_second(),
            instant("2026-01-01T03:30:00Z").as_second()
        );
        let at = probe_at("2:30 pm", "2026-11-05", "Asia/Tokyo");
        let text = format_clock(at, &zone("Asia/Tokyo"), false);
        assert_eq!(text.time, "2:30");
        assert_eq!(text.period.as_deref(), Some("pm"));
        let text = format_clock(at, &zone("Asia/Tokyo"), true);
        assert_eq!(text.time, "14:30");
    }
}
