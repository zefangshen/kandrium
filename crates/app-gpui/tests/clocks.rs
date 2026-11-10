//! Clocks flows through the real `Store` and `kandrium-core`.
//!
//! These tests are headless: they boot a seeded temp database, then drive the
//! same wrappers the shell uses. Placement, day windows and the restricted
//! removal message all come from core, so a failure here means the shell and
//! the data layer disagree.

use std::path::{Path, PathBuf};

use kandrium_app_gpui::store::Store;
use kandrium_core::clocks::{day_end_seconds, day_start_seconds, resolve_zone};

/// A bootstrapped database (schema + seed) in the temp directory.
fn seeded(dir: &Path) -> PathBuf {
    let path = dir.join("kandrium.db");
    let mut db = kandrium_core::open_db(&path).expect("open temp db");
    kandrium_core::bootstrap(&mut db, &path).expect("bootstrap temp db");
    path
}

fn zones(store: &Store) -> Vec<String> {
    store
        .clock_data()
        .expect("clocks")
        .clocks
        .into_iter()
        .map(|clock| clock.tz_identifier)
        .collect()
}

/// Whole-day window for `date` in `zone`.
fn day_window(zone: &str, date: jiff::civil::Date) -> (i64, i64) {
    let zone = resolve_zone(zone).expect("known zone");
    (
        day_start_seconds(date, &zone).expect("start"),
        day_end_seconds(date, &zone).expect("end"),
    )
}

fn travel_for(store: &Store, person_id: &str) -> Vec<kandrium_core::Travel> {
    store
        .clock_data()
        .expect("clocks")
        .travels
        .into_iter()
        .filter(|travel| travel.person_id == person_id)
        .collect()
}

#[test]
fn adding_removing_and_reordering_cities_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_path(seeded(dir.path()));

    store.add_clock("Asia/Seoul").expect("add city");
    assert_eq!(zones(&store).last().unwrap(), "Asia/Seoul");

    let mut ids: Vec<String> = store
        .clock_data()
        .expect("clocks")
        .clocks
        .into_iter()
        .map(|clock| clock.id)
        .collect();
    ids.reverse();
    store.reorder_clocks(&ids).expect("reorder");
    let ordered: Vec<String> = store
        .clock_data()
        .expect("clocks")
        .clocks
        .into_iter()
        .map(|clock| clock.id)
        .collect();
    assert_eq!(ordered, ids);

    let seoul = ordered[0].clone();
    store.remove_clock(&seoul).expect("remove city");
    assert!(!zones(&store).contains(&"Asia/Seoul".to_string()));
    let positions: Vec<i64> = store
        .clock_data()
        .expect("clocks")
        .clocks
        .into_iter()
        .map(|clock| clock.position)
        .collect();
    assert_eq!(positions, (0..positions.len() as i64).collect::<Vec<_>>());
}

#[test]
fn a_city_with_residents_refuses_removal() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_path(seeded(dir.path()));
    let data = store.clock_data().expect("clocks");
    let clock_id = data.clocks[0].id.clone();
    let city = data.clocks[0].tz_identifier.clone();

    store.add_person(&clock_id, "Ada Lovelace").expect("person");

    let error = store.remove_clock(&clock_id).expect_err("blocked");
    assert!(
        error.contains("still has 1 person"),
        "unexpected message: {error}"
    );
    assert!(zones(&store).contains(&city));

    let person_id = store.clock_data().expect("clocks").people[0].id.clone();
    store.remove_person(&person_id).expect("remove person");
    store.remove_clock(&clock_id).expect("city is free now");
}

#[test]
fn people_are_added_renamed_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_path(seeded(dir.path()));
    let clock_id = store.clock_data().expect("clocks").clocks[0].id.clone();

    store.add_person(&clock_id, "Ada").expect("add");
    let person = store.clock_data().expect("clocks").people[0].clone();
    assert_eq!(person.name, "Ada");
    assert_eq!(person.home_clock_id, clock_id);

    store
        .rename_person(&person.id, "Ada Lovelace")
        .expect("rename");
    assert_eq!(
        store.clock_data().expect("clocks").people[0].name,
        "Ada Lovelace"
    );

    store.remove_person(&person.id).expect("remove");
    assert!(store.clock_data().expect("clocks").people.is_empty());
}

#[test]
fn a_trip_can_be_assigned_extended_ended_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_path(seeded(dir.path()));
    let data = store.clock_data().expect("clocks");
    let home = data.clocks[0].id.clone();
    let destination = data.clocks[1].clone();
    store.add_person(&home, "Ada").expect("person");
    let person_id = store.clock_data().expect("clocks").people[0].id.clone();

    let today = jiff::Zoned::now().date();
    let (starts_at, ends_at) = day_window(&destination.tz_identifier, today);
    store
        .start_travel(&person_id, &destination.id, starts_at, Some(ends_at))
        .expect("start trip");

    let trips = travel_for(&store, &person_id);
    assert_eq!(trips.len(), 1);
    assert_eq!(trips[0].ends_at, Some(ends_at));

    store.extend_travel(&trips[0].id, None).expect("open ended");
    assert_eq!(travel_for(&store, &person_id)[0].ends_at, None);

    store.end_travel(&trips[0].id).expect("end early");
    let ended = &travel_for(&store, &person_id)[0];
    assert!(
        ended.ends_at.is_some(),
        "ending early moves the return time to now"
    );
    assert!(ended.cancelled_at.is_none());

    let tomorrow = today.tomorrow().expect("tomorrow");
    let (starts_at, ends_at) = day_window(&destination.tz_identifier, tomorrow);
    store
        .start_travel(&person_id, &destination.id, starts_at, Some(ends_at))
        .expect("planned trip");
    let planned = travel_for(&store, &person_id)
        .into_iter()
        .find(|travel| travel.cancelled_at.is_none())
        .expect("planned");
    store.remove_travel(&planned.id).expect("remove trip");
    assert_eq!(travel_for(&store, &person_id).len(), 1);
}

#[test]
fn relocation_cancels_trips_to_both_homes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_path(seeded(dir.path()));
    let data = store.clock_data().expect("clocks");
    let old_home = data.clocks[0].clone();
    let new_home = data.clocks[1].clone();
    let elsewhere = data.clocks[2].clone();
    store.add_person(&old_home.id, "Ada").expect("person");
    let person_id = store.clock_data().expect("clocks").people[0].id.clone();

    let today = jiff::Zoned::now().date();
    let (starts_at, ends_at) = day_window(&new_home.tz_identifier, today);
    store
        .start_travel(&person_id, &new_home.id, starts_at, Some(ends_at))
        .expect("trip to new home");
    let (starts_at, ends_at) = day_window(
        &elsewhere.tz_identifier,
        today.tomorrow().expect("tomorrow"),
    );
    store
        .start_travel(&person_id, &elsewhere.id, starts_at, Some(ends_at))
        .expect("unrelated trip");

    store
        .relocate_person(&person_id, &new_home.id)
        .expect("relocate");

    let trips = travel_for(&store, &person_id);
    let to_new_home = trips
        .iter()
        .find(|travel| travel.destination_clock_id.as_deref() == Some(new_home.id.as_str()))
        .expect("trip to new home");
    assert!(
        to_new_home.cancelled_at.is_some(),
        "a trip to the new home must end"
    );
    let unrelated = trips
        .iter()
        .find(|travel| travel.destination_clock_id.as_deref() == Some(elsewhere.id.as_str()))
        .expect("unrelated trip");
    assert!(
        unrelated.cancelled_at.is_none(),
        "an unrelated trip must survive"
    );
    assert_eq!(
        store.clock_data().expect("clocks").people[0].home_clock_id,
        new_home.id
    );
}
