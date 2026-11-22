//! Canonical person records (Network).
//!
//! `persons` owns identity and profile: name, role, organization, expertise,
//! collaboration intent, contact channels, location, time zone, how met,
//! notes, network status and Clocks visibility (`in_clocks`). Clocks owns
//! placement and travel (see [`crate::clocks`]): a colleague on a city card is
//! a `clock_placements` row that references a person.
//!
//! Every function is synchronous and takes `&rusqlite::Connection`, like the
//! rest of `kandrium-core`. Mutating calls run inside the caller's history
//! run (the app's `Store::app_run`); this module never opens one.

use jiff::Timestamp;
use jiff::civil::Date;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::uid;
use crate::error::{KandriumError, Result};

/// Person lifecycle statuses shown in the Network filters.
pub const PERSON_STATUSES: [&str; 3] = ["active", "dormant", "archived"];

/// The canonical person profile. Clocks renders only `name`; everything else
/// belongs to Network.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub id: String,
    pub name: String,
    pub role_title: String,
    pub organization: String,
    pub collaboration: String,
    pub email: String,
    pub url: String,
    pub location: String,
    pub tz_identifier: String,
    pub last_contact_on: Option<String>,
    pub how_met: String,
    pub notes: String,
    pub status: String,
    pub in_clocks: bool,
    pub created_at: String,
    pub updated_at: String,
    /// Expertise labels, ordered by position.
    pub expertise: Vec<String>,
}

/// Input for [`create_person`]. Network-created people start `in_clocks = 0`
/// and have no placement; only `name` is required.
#[derive(Clone, Debug, Default)]
pub struct NewPerson {
    pub name: String,
    pub role_title: String,
    pub organization: String,
    pub collaboration: String,
    pub email: String,
    pub url: String,
    pub location: String,
    pub tz_identifier: String,
    pub last_contact_on: Option<String>,
    pub how_met: String,
    pub notes: String,
    pub expertise: Vec<String>,
}

impl NewPerson {
    /// A person with only a display name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}

/// Partial profile update for [`update_person`].
///
/// `None` leaves a field unchanged. `last_contact_on: Some("")` clears the
/// date; `expertise: Some(vec![])` clears every label.
#[derive(Clone, Debug, Default)]
pub struct PersonPatch {
    pub name: Option<String>,
    pub role_title: Option<String>,
    pub organization: Option<String>,
    pub collaboration: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
    pub location: Option<String>,
    pub tz_identifier: Option<String>,
    pub last_contact_on: Option<String>,
    pub how_met: Option<String>,
    pub notes: Option<String>,
    pub status: Option<String>,
    pub expertise: Option<Vec<String>>,
}

const PERSON_COLUMNS: &str = "id, name, role_title, organization, collaboration, \
     email, url, location, tz_identifier, last_contact_on, how_met, notes, \
     status, in_clocks, created_at, updated_at";

fn person_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Person> {
    Ok(Person {
        id: row.get(0)?,
        name: row.get(1)?,
        role_title: row.get(2)?,
        organization: row.get(3)?,
        collaboration: row.get(4)?,
        email: row.get(5)?,
        url: row.get(6)?,
        location: row.get(7)?,
        tz_identifier: row.get(8)?,
        last_contact_on: row.get(9)?,
        how_met: row.get(10)?,
        notes: row.get(11)?,
        status: row.get(12)?,
        in_clocks: row.get::<_, i64>(13)? != 0,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
        expertise: Vec::new(),
    })
}

/// All people matching `query` and `status`, ordered by name.
///
/// `query` is a case-insensitive substring over name, role, organization,
/// location, collaboration and expertise labels. `status` is `"all"` (or
/// empty) or one of [`PERSON_STATUSES`].
pub fn list_persons(db: &Connection, query: &str, status: &str) -> Result<Vec<Person>> {
    let status = status_filter(status)?;
    let needle = query.trim().to_lowercase();
    let mut stmt = db.prepare(
        "SELECT id, name, role_title, organization, collaboration, \
                email, url, location, tz_identifier, last_contact_on, \
                how_met, notes, status, in_clocks, created_at, updated_at \
         FROM persons \
         WHERE (?1 IS NULL OR status = ?1) \
           AND (?2 = '' \
                OR instr(lower(name), ?2) > 0 \
                OR instr(lower(role_title), ?2) > 0 \
                OR instr(lower(organization), ?2) > 0 \
                OR instr(lower(location), ?2) > 0 \
                OR instr(lower(collaboration), ?2) > 0 \
                OR EXISTS (SELECT 1 FROM person_expertise e \
                           WHERE e.person_id = persons.id \
                             AND instr(lower(e.label), ?2) > 0)) \
         ORDER BY name COLLATE NOCASE, id",
    )?;
    let rows = stmt.query_map(rusqlite::params![status, needle], person_from_row)?;
    let mut people = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    for person in &mut people {
        person.expertise = expertise_for(db, &person.id)?;
    }
    Ok(people)
}

/// One person by id.
pub fn get_person(db: &Connection, id: &str) -> Result<Person> {
    let mut person = db
        .query_row(
            &format!("SELECT {PERSON_COLUMNS} FROM persons WHERE id = ?1"),
            [id],
            person_from_row,
        )
        .map_err(|_| KandriumError::new("Person not found"))?;
    person.expertise = expertise_for(db, id)?;
    Ok(person)
}

/// Create a Network person with `in_clocks = 0`. Trims the name and rejects
/// a blank one.
pub fn create_person(db: &Connection, input: NewPerson) -> Result<Person> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(KandriumError::new("Person name required"));
    }
    let last_contact_on = input
        .last_contact_on
        .as_deref()
        .map(validate_last_contact)
        .transpose()?
        .flatten();
    let expertise = normalize_expertise(&input.expertise);
    let now = Timestamp::now().to_string();
    let id = uid();

    let tx = db.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO persons \
             (id, name, role_title, organization, collaboration, email, url, \
              location, tz_identifier, last_contact_on, how_met, notes, \
              status, in_clocks, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, \
                 'active', 0, ?13, ?13)",
        rusqlite::params![
            id,
            name,
            input.role_title.trim(),
            input.organization.trim(),
            input.collaboration.trim(),
            input.email.trim(),
            input.url.trim(),
            input.location.trim(),
            input.tz_identifier.trim(),
            last_contact_on,
            input.how_met.trim(),
            input.notes,
            now,
        ],
    )?;
    replace_expertise(&tx, &id, &expertise)?;
    tx.commit()?;
    get_person(db, &id)
}

/// Update the supplied profile fields, status and last-contact date, and
/// optionally replace the expertise labels. Touches `updated_at`.
pub fn update_person(db: &Connection, id: &str, patch: PersonPatch) -> Result<Person> {
    let current = get_person(db, id)?;

    let name = match patch.name {
        Some(name) => {
            let name = name.trim();
            if name.is_empty() {
                return Err(KandriumError::new("Person name required"));
            }
            name.to_string()
        }
        None => current.name,
    };
    let status = match patch.status {
        Some(status) => canonical_status(&status)?.to_string(),
        None => current.status,
    };
    let last_contact_on = match patch.last_contact_on {
        Some(value) => validate_last_contact(&value)?,
        None => current.last_contact_on,
    };
    let expertise = patch.expertise.map(|labels| normalize_expertise(&labels));
    let updated_at = Timestamp::now().to_string();

    let tx = db.unchecked_transaction()?;
    tx.execute(
        "UPDATE persons SET name = ?2, role_title = ?3, organization = ?4, \
                collaboration = ?5, email = ?6, url = ?7, location = ?8, \
                tz_identifier = ?9, last_contact_on = ?10, how_met = ?11, \
                notes = ?12, status = ?13, updated_at = ?14 \
         WHERE id = ?1",
        rusqlite::params![
            id,
            name,
            patch.role_title.unwrap_or(current.role_title),
            patch.organization.unwrap_or(current.organization),
            patch.collaboration.unwrap_or(current.collaboration),
            patch.email.unwrap_or(current.email),
            patch.url.unwrap_or(current.url),
            patch.location.unwrap_or(current.location),
            patch.tz_identifier.unwrap_or(current.tz_identifier),
            last_contact_on,
            patch.how_met.unwrap_or(current.how_met),
            patch.notes.unwrap_or(current.notes),
            status,
            updated_at,
        ],
    )?;
    if let Some(labels) = &expertise {
        replace_expertise(&tx, id, labels)?;
    }
    tx.commit()?;
    get_person(db, id)
}

/// Rename a person. Blank names are rejected; Clocks' inline rename
/// delegates here so the name is canonical everywhere.
pub fn set_person_name(db: &Connection, id: &str, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(KandriumError::new("Person name required"));
    }
    let changed = db.execute(
        "UPDATE persons SET name = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![name, Timestamp::now().to_string(), id],
    )?;
    if changed == 0 {
        return Err(KandriumError::new("Person not found"));
    }
    Ok(())
}

/// Show or hide a person on Clocks. Hiding keeps the placement and travels;
/// showing requires a retained placement.
pub fn set_person_in_clocks(db: &Connection, id: &str, on: bool) -> Result<()> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM persons WHERE id = ?1)",
        [id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(KandriumError::new("Person not found"));
    }
    if on {
        let placed: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM clock_placements WHERE person_id = ?1)",
            [id],
            |row| row.get(0),
        )?;
        if !placed {
            return Err(KandriumError::new("Place the person on a city first"));
        }
    }
    db.execute(
        "UPDATE persons SET in_clocks = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![on as i64, Timestamp::now().to_string(), id],
    )?;
    Ok(())
}

/// Delete a person. Placements and travels cascade; affected cities must be
/// renumbered by the caller that owns placements ([`crate::clocks`]).
pub fn delete_person(db: &Connection, id: &str) -> Result<()> {
    let changed = db.execute("DELETE FROM persons WHERE id = ?1", [id])?;
    if changed == 0 {
        return Err(KandriumError::new("Person not found"));
    }
    Ok(())
}

/// Distinct expertise labels, ordered alphabetically, for autocomplete.
pub fn expertise_labels(db: &Connection) -> Result<Vec<String>> {
    let mut stmt = db.prepare(
        "SELECT DISTINCT label FROM person_expertise \
         ORDER BY label COLLATE NOCASE, label",
    )?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
}

fn expertise_for(db: &Connection, person_id: &str) -> Result<Vec<String>> {
    let mut stmt = db.prepare(
        "SELECT label FROM person_expertise \
         WHERE person_id = ?1 \
         ORDER BY position, label COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([person_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
}

fn replace_expertise(db: &Connection, person_id: &str, labels: &[String]) -> Result<()> {
    db.execute(
        "DELETE FROM person_expertise WHERE person_id = ?1",
        [person_id],
    )?;
    for (position, label) in labels.iter().enumerate() {
        db.execute(
            "INSERT INTO person_expertise (person_id, label, position) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![person_id, label, position as i64],
        )?;
    }
    Ok(())
}

fn normalize_expertise(labels: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for label in labels {
        let label = label.trim();
        if label.is_empty() {
            continue;
        }
        if out
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(label))
        {
            continue;
        }
        out.push(label.to_string());
    }
    out
}

fn canonical_status(raw: &str) -> Result<&'static str> {
    let raw = raw.trim();
    PERSON_STATUSES
        .iter()
        .copied()
        .find(|status| status.eq_ignore_ascii_case(raw))
        .ok_or_else(|| KandriumError::new("Person status must be active, dormant or archived"))
}

fn status_filter(raw: &str) -> Result<Option<&'static str>> {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    canonical_status(raw).map(Some)
}

fn validate_last_contact(raw: &str) -> Result<Option<String>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let date: Date = raw
        .parse()
        .map_err(|_| KandriumError::new("Last contact must be a date like 2026-01-31"))?;
    Ok(Some(date.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_db;

    fn mem_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open_db(&dir.path().join("persons.db")).unwrap();
        crate::db::migrations::run_schema(&mut db).unwrap();
        (dir, db)
    }

    fn sample(name: &str) -> NewPerson {
        NewPerson::new(name)
    }

    #[test]
    fn create_get_and_list_people() {
        let (_dir, db) = mem_db();
        let ada = create_person(
            &db,
            NewPerson {
                role_title: "Engineer".to_string(),
                organization: "Acme".to_string(),
                location: "Paris".to_string(),
                last_contact_on: Some("2026-01-05".to_string()),
                expertise: vec!["Rust".to_string(), "  SQLite  ".to_string()],
                ..sample("  Ada Lovelace  ")
            },
        )
        .unwrap();
        assert_eq!(ada.name, "Ada Lovelace");
        assert_eq!(ada.role_title, "Engineer");
        assert_eq!(ada.last_contact_on.as_deref(), Some("2026-01-05"));
        assert_eq!(ada.status, "active");
        assert!(!ada.in_clocks);
        assert_eq!(ada.expertise, vec!["Rust", "SQLite"]);
        assert_eq!(ada.created_at, ada.updated_at);

        create_person(&db, sample("Grace Hopper")).unwrap();
        assert_eq!(get_person(&db, &ada.id).unwrap(), ada);
        assert_eq!(list_persons(&db, "", "all").unwrap().len(), 2);
        assert_eq!(list_persons(&db, "", "").unwrap().len(), 2);
        assert!(get_person(&db, "person_missing").is_err());
    }

    #[test]
    fn list_search_covers_profile_and_expertise() {
        let (_dir, db) = mem_db();
        create_person(
            &db,
            NewPerson {
                role_title: "Researcher".to_string(),
                organization: "Lab".to_string(),
                location: "Kyiv".to_string(),
                collaboration: "advise".to_string(),
                expertise: vec!["virology".to_string()],
                ..sample("Ana")
            },
        )
        .unwrap();
        create_person(&db, sample("Ben")).unwrap();

        assert_eq!(list_persons(&db, "ana", "all").unwrap().len(), 1);
        assert_eq!(list_persons(&db, "RESEARCH", "all").unwrap().len(), 1);
        assert_eq!(list_persons(&db, "lab", "all").unwrap().len(), 1);
        assert_eq!(list_persons(&db, "kyiv", "all").unwrap().len(), 1);
        assert_eq!(list_persons(&db, "advise", "all").unwrap().len(), 1);
        assert_eq!(list_persons(&db, "viro", "all").unwrap().len(), 1);
        assert!(list_persons(&db, "nothing", "all").unwrap().is_empty());
    }

    #[test]
    fn update_replaces_expertise_and_validates_status_and_date() {
        let (_dir, db) = mem_db();
        let ana = create_person(&db, sample("Ana")).unwrap();

        let updated = update_person(
            &db,
            &ana.id,
            PersonPatch {
                organization: Some("Lab".to_string()),
                status: Some("dormant".to_string()),
                last_contact_on: Some("2026-02-03".to_string()),
                expertise: Some(vec!["Rust".to_string(), "rust".to_string()]),
                ..PersonPatch::default()
            },
        )
        .unwrap();
        assert_eq!(updated.organization, "Lab");
        assert_eq!(updated.status, "dormant");
        assert_eq!(updated.last_contact_on.as_deref(), Some("2026-02-03"));
        // Case-insensitive duplicates collapse to one label.
        assert_eq!(updated.expertise, vec!["Rust"]);
        assert_ne!(updated.updated_at, ana.updated_at);

        let cleared = update_person(
            &db,
            &ana.id,
            PersonPatch {
                last_contact_on: Some(String::new()),
                expertise: Some(Vec::new()),
                ..PersonPatch::default()
            },
        )
        .unwrap();
        assert_eq!(cleared.last_contact_on, None);
        assert!(cleared.expertise.is_empty());

        assert!(
            update_person(
                &db,
                &ana.id,
                PersonPatch {
                    status: Some("busy".to_string()),
                    ..PersonPatch::default()
                },
            )
            .is_err()
        );
        assert!(
            update_person(
                &db,
                &ana.id,
                PersonPatch {
                    last_contact_on: Some("5 Jan".to_string()),
                    ..PersonPatch::default()
                },
            )
            .is_err()
        );
        assert!(
            update_person(
                &db,
                &ana.id,
                PersonPatch {
                    name: Some("   ".to_string()),
                    ..PersonPatch::default()
                },
            )
            .is_err()
        );
    }

    #[test]
    fn status_filter_narrows_and_unknown_statuses_are_rejected() {
        let (_dir, db) = mem_db();
        let ana = create_person(&db, sample("Ana")).unwrap();
        create_person(&db, sample("Ben")).unwrap();
        update_person(
            &db,
            &ana.id,
            PersonPatch {
                status: Some("archived".to_string()),
                ..PersonPatch::default()
            },
        )
        .unwrap();

        assert_eq!(list_persons(&db, "", "active").unwrap().len(), 1);
        assert_eq!(list_persons(&db, "", "archived").unwrap().len(), 1);
        assert!(list_persons(&db, "", "dormant").unwrap().is_empty());
        assert!(list_persons(&db, "", "unknown").is_err());
    }

    #[test]
    fn rename_in_clocks_and_delete_follow_the_person() {
        let (_dir, db) = mem_db();
        let ana = create_person(&db, sample("Ana")).unwrap();

        set_person_name(&db, &ana.id, "  Ann  ").unwrap();
        assert_eq!(get_person(&db, &ana.id).unwrap().name, "Ann");
        assert!(set_person_name(&db, &ana.id, " ").is_err());
        assert!(set_person_name(&db, "person_missing", "Ann").is_err());

        // Hiding needs no placement; showing does.
        set_person_in_clocks(&db, &ana.id, false).unwrap();
        assert!(
            set_person_in_clocks(&db, &ana.id, true)
                .unwrap_err()
                .to_string()
                .contains("city")
        );

        delete_person(&db, &ana.id).unwrap();
        assert!(get_person(&db, &ana.id).is_err());
        assert!(delete_person(&db, &ana.id).is_err());
    }

    #[test]
    fn expertise_labels_are_distinct_and_sorted() {
        let (_dir, db) = mem_db();
        create_person(
            &db,
            NewPerson {
                expertise: vec!["Rust".to_string(), "SQLite".to_string()],
                ..sample("Ana")
            },
        )
        .unwrap();
        create_person(
            &db,
            NewPerson {
                expertise: vec!["rust".to_string(), "Maps".to_string()],
                ..sample("Ben")
            },
        )
        .unwrap();

        assert_eq!(
            expertise_labels(&db).unwrap(),
            vec!["Maps", "Rust", "rust", "SQLite"]
        );
    }

    #[test]
    fn create_rejects_blank_names_and_bad_dates() {
        let (_dir, db) = mem_db();
        assert!(create_person(&db, sample("   ")).is_err());
        assert!(
            create_person(
                &db,
                NewPerson {
                    last_contact_on: Some("yesterday".to_string()),
                    ..sample("Ana")
                },
            )
            .is_err()
        );
    }
}
