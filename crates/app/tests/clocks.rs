//! Clocks flows through the real `Store` and `kandrium-core`.
//!
//! These tests are headless: they boot a seeded temp database, then drive the
//! same wrappers the shell uses. Placement, day windows and the restricted
//! removal message all come from core, so a failure here means the shell and
//! the data layer disagree.

use std::path::{Path, PathBuf};

use gpui::TestAppContext;
use gpui_kit::test::TestWindowExt;

use kandrium_app::app::AppShell;
use kandrium_app::store::Store;
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

/// The civil date currently in effect in `zone`.
///
/// A test that builds its window from the process-local date breaks after
/// 23:00 in zones behind the destination: "today" locally is already
/// "tomorrow" there, so the window has ended. Deriving the date in the zone
/// that will own the window keeps the trip active at `now` at every hour.
fn today_in(zone: &str) -> jiff::civil::Date {
    let zone = resolve_zone(zone).expect("known zone");
    jiff::Timestamp::now().to_zoned(zone).date()
}

/// The civil day containing `now` in `zone`, as a half-open second range.
///
/// The end is the next day's start rather than 23:59:59 so `now` stays
/// strictly inside the window in its final second and `end_travel` cannot
/// race the midnight boundary.
fn active_window(zone: &str) -> (i64, i64) {
    let zone = resolve_zone(zone).expect("known zone");
    let today = jiff::Timestamp::now().to_zoned(zone.clone()).date();
    let tomorrow = today.tomorrow().expect("next day");
    (
        day_start_seconds(today, &zone).expect("start"),
        day_start_seconds(tomorrow, &zone).expect("next start"),
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
    // Removing only the placement empties the roster; the canonical profile
    // survives, hidden, and can be shown again.
    assert!(store.clock_data().expect("clocks").people.is_empty());
    let hidden = store.get_person(&person.id).expect("profile stays");
    assert_eq!(hidden.name, "Ada Lovelace");
    assert!(!hidden.in_clocks);
    assert_eq!(
        store.list_persons("", "all").expect("people").len(),
        1,
        "the person stays in the Network list"
    );

    store
        .add_existing_person(&clock_id, &person.id)
        .expect("place again");
    let restored = store.clock_data().expect("clocks").people;
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].id, person.id);
    assert_eq!(restored[0].position, 0, "the same city slot is available");
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

    let today = today_in(&destination.tz_identifier);
    let (starts_at, ends_at) = active_window(&destination.tz_identifier);
    // The trip must be running *now*: the old process-local date built a
    // window that had already closed late in the evening.
    let now = jiff::Timestamp::now().as_second();
    assert!(
        starts_at <= now && now < ends_at,
        "the destination's window must contain now"
    );
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

    let (starts_at, ends_at) = active_window(&new_home.tz_identifier);
    let now = jiff::Timestamp::now().as_second();
    assert!(
        starts_at <= now && now < ends_at,
        "the new home's window must contain now"
    );
    store
        .start_travel(&person_id, &new_home.id, starts_at, Some(ends_at))
        .expect("trip to new home");
    // A planned trip that begins only after the new-home window ends, so the
    // two windows never compete for "now". Deriving the date from the first
    // window's end keeps that true even when the two zones are on different
    // civil days; if that instant still falls inside the unrelated zone's
    // civil day, that day's window starts before the new-home window ends, so
    // move to the next one.
    let unrelated_zone = resolve_zone(&elsewhere.tz_identifier).expect("known zone");
    let mut unrelated_date = jiff::Timestamp::from_second(ends_at + 1)
        .expect("valid instant")
        .to_zoned(unrelated_zone.clone())
        .date();
    if day_window(&elsewhere.tz_identifier, unrelated_date).0 <= ends_at {
        unrelated_date = unrelated_date.tomorrow().expect("next day");
    }
    let (starts_at, ends_at) = day_window(&elsewhere.tz_identifier, unrelated_date);
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

// ---------------------------------------------------------------------------
// Overlay behaviour
// ---------------------------------------------------------------------------

type ClocksUi<'a> = &'a mut gpui::VisualTestContext;

fn open_clocks(cx: ClocksUi) {
    cx.update(|window, cx| window.click("nav-clocks", cx));
    cx.update(|window, cx| window.render_frame(cx));
}

fn open_picker(cx: ClocksUi) {
    cx.update(|window, cx| window.click("clocks-add-city", cx));
    cx.update(|window, cx| window.render_frame(cx));
}

fn picker_open(cx: ClocksUi) -> bool {
    cx.update(|window, _cx| window.try_find("clock-picker").is_some())
}

/// The add-city overlay must behave like every other modal in the app: it
/// covers the page so clicks cannot reach the cards behind it, `Esc` closes
/// it, and so does a press outside the panel.
#[gpui::test]
fn add_city_picker_blocks_the_page_and_closes(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let db = seeded(dir.path());
    cx.update(gpui_kit::init);
    let (_shell, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(db)));
    open_clocks(cx);

    open_picker(cx);
    assert!(picker_open(cx), "the add-city picker should open");
    let backdrop = cx.update(|window, _cx| window.try_find("clock-picker-backdrop"));
    let backdrop = backdrop.expect("the picker needs a full-size backdrop");
    let page = cx.update(|window, _cx| window.find("clocks-view").bounds().size);
    let size = backdrop.bounds().size;
    assert_eq!(
        (size.width, size.height),
        (page.width, page.height),
        "the backdrop must cover the page"
    );

    cx.update(|window, cx| window.press("escape", cx));
    assert!(!picker_open(cx), "Esc should close the picker");

    open_picker(cx);
    assert!(picker_open(cx));
    // The panel is centred, so a click near the top-left corner lands on the
    // backdrop rather than on the panel.
    cx.update(|window, cx| {
        window.click_at(
            "clock-picker-backdrop",
            gpui::point(gpui::px(4.), gpui::px(4.)),
            cx,
        );
    });
    assert!(
        !picker_open(cx),
        "a press outside the panel should close it"
    );
}

fn open_inspector(cx: ClocksUi, clock_id: &str) {
    let card = format!("clock-card-{clock_id}");
    cx.update(|window, cx| window.click(card, cx));
    cx.update(|window, cx| window.render_frame(cx));
}

fn inspector_open(cx: ClocksUi) -> bool {
    cx.update(|window, _cx| window.try_find("clock-inspector").is_some())
}

/// The per-city inspector opened from a card is a modal too: the cards behind
/// it must neither hover nor activate, `Esc` closes it, and so does a press
/// outside the panel.
#[gpui::test]
fn clock_inspector_blocks_the_page_and_closes(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_path(seeded(dir.path()));
    let clock_id = store.clock_data().expect("clocks").clocks[0].id.clone();
    cx.update(gpui_kit::init);
    let (_shell, cx) = cx.add_window_view(|_window, _cx| AppShell::with_store(store));
    open_clocks(cx);

    open_inspector(cx, &clock_id);
    assert!(inspector_open(cx), "the inspector should open");
    let backdrop = cx.update(|window, _cx| window.try_find("clock-inspector-backdrop"));
    let backdrop = backdrop.expect("the inspector needs a full-size backdrop");
    let page = cx.update(|window, _cx| window.find("clocks-view").bounds().size);
    let size = backdrop.bounds().size;
    assert_eq!(
        (size.width, size.height),
        (page.width, page.height),
        "the backdrop must cover the page"
    );

    cx.update(|window, cx| window.press("escape", cx));
    assert!(!inspector_open(cx), "Esc should close the inspector");

    open_inspector(cx, &clock_id);
    assert!(inspector_open(cx));
    // The panel is centred, so a click near the top-left corner lands on the
    // backdrop rather than on the panel.
    cx.update(|window, cx| {
        window.click_at(
            "clock-inspector-backdrop",
            gpui::point(gpui::px(4.), gpui::px(4.)),
            cx,
        );
    });
    assert!(
        !inspector_open(cx),
        "a press outside the panel should close it"
    );
}

fn open_people_tab(cx: ClocksUi) {
    cx.update(|window, cx| window.click("clocks-tab-colleagues", cx));
    cx.update(|window, cx| window.render_frame(cx));
}

fn open_probe_menu(cx: ClocksUi) {
    cx.update(|window, cx| window.click("clocks-probe-zone", cx));
    cx.update(|window, cx| window.render_frame(cx));
}

fn probe_menu_open(cx: ClocksUi) -> bool {
    cx.update(|window, _cx| window.try_find("clock-probe-zone-menu").is_some())
}

/// The probe-zone menu is modal like the picker and the city inspector: the
/// content behind it must neither hover nor activate, `Esc` closes it, and so
/// does a press outside the panel.
#[gpui::test]
fn probe_zone_menu_blocks_the_page_and_closes(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(gpui_kit::init);
    let (_shell, cx) = cx
        .add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(seeded(dir.path()))));
    open_clocks(cx);

    open_probe_menu(cx);
    assert!(probe_menu_open(cx), "the probe menu should open");
    let backdrop = cx
        .update(|window, _cx| window.try_find("clock-probe-zone-backdrop"))
        .expect("the menu needs a full-size backdrop");
    let page = cx.update(|window, _cx| window.find("clocks-view").bounds().size);
    let size = backdrop.bounds().size;
    assert_eq!(
        (size.width, size.height),
        (page.width, page.height),
        "the backdrop must cover the page"
    );

    cx.update(|window, cx| window.press("escape", cx));
    assert!(!probe_menu_open(cx), "Esc should close the menu");

    open_probe_menu(cx);
    assert!(probe_menu_open(cx));
    // The panel is centred, so a click near the top-left corner lands on the
    // backdrop rather than on the panel.
    cx.update(|window, cx| {
        window.click_at(
            "clock-probe-zone-backdrop",
            gpui::point(gpui::px(4.), gpui::px(4.)),
            cx,
        );
    });
    assert!(
        !probe_menu_open(cx),
        "a press outside the panel should close it"
    );

    // A row still works: the local zone closes the menu and re-reads the
    // probe in it.
    open_probe_menu(cx);
    assert!(probe_menu_open(cx));
    cx.update(|window, cx| window.click("clock-probe-zone-local", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        !probe_menu_open(cx),
        "picking the local zone should close the menu"
    );

    // Same again after picking a city, which is the state the menu is usually
    // reopened from.
    open_probe_menu(cx);
    cx.update(|window, cx| window.click("clock-probe-zone-1", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(!probe_menu_open(cx), "picking a city should close the menu");
    open_probe_menu(cx);
    assert!(probe_menu_open(cx));
    cx.update(|window, cx| window.click("clock-probe-zone-local", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        !probe_menu_open(cx),
        "the local row must still activate after a city was picked"
    );
}
/// The add and rename forms split a name into first, middle (optional) and
/// last fields, while the stored person keeps a single display name.
#[gpui::test]
fn inspector_forms_take_three_name_fields(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_path(seeded(dir.path()));
    let clock_id = store.clock_data().expect("clocks").clocks[0].id.clone();
    store.add_person(&clock_id, "Ada Lovelace").expect("person");
    let person_id = store.clock_data().expect("clocks").people[0].id.clone();
    cx.update(gpui_kit::init);
    let (_shell, cx) = cx.add_window_view(|_window, _cx| AppShell::with_store(store));
    open_clocks(cx);
    open_inspector(cx, &clock_id);

    for id in [
        "clock-inspector-add-first",
        "clock-inspector-add-middle",
        "clock-inspector-add-last",
    ] {
        assert!(
            cx.update(|window, _cx| window.try_find(id).is_some()),
            "the add form is missing {id}"
        );
    }

    let rename = format!("clock-person-rename-{person_id}");
    cx.update(move |window, cx| window.click(rename, cx));
    cx.update(|window, cx| window.render_frame(cx));
    for id in [
        "clock-person-rename-first",
        "clock-person-rename-middle",
        "clock-person-rename-last",
    ] {
        assert!(
            cx.update(|window, _cx| window.try_find(id).is_some()),
            "the rename form is missing {id}"
        );
    }
}

fn person_form_open(cx: ClocksUi) -> bool {
    cx.update(|window, _cx| window.try_find("person-form").is_some())
}

/// The Colleagues tab's action adds a person to a city that has no clock card
/// yet: the modal creates that city on submit and the roster picks the person
/// up straight away.
#[gpui::test]
fn colleagues_tab_adds_a_person_to_a_new_city(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(gpui_kit::init);
    let (_shell, cx) = cx
        .add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(seeded(dir.path()))));
    open_clocks(cx);
    open_people_tab(cx);
    assert!(
        cx.update(|window, _cx| window.try_find("clocks-add-colleague").is_some()),
        "the Colleagues tab needs its own add action"
    );

    cx.update(|window, _cx| {
        let tab = window.find("clocks-tab-colleagues").bounds();
        let count = window.find("clocks-tab-count").bounds();
        let search = window.find("clocks-search").bounds();
        assert!(
            tab.right() <= count.left() && count.right() <= search.left(),
            "the active tab's count sits left of the search: tab {tab:?}, count {count:?}, search {search:?}"
        );
    });

    cx.update(|window, cx| window.click("clocks-add-colleague", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(person_form_open(cx), "the add-person modal should open");
    for id in [
        "person-form-first",
        "person-form-middle",
        "person-form-last",
        "person-form-city",
    ] {
        assert!(
            cx.update(|window, _cx| window.try_find(id).is_some()),
            "the person form is missing {id}"
        );
    }

    cx.update(|window, cx| window.click("person-form-first", cx));
    cx.update(|window, cx| window.input("Ada", cx));
    cx.update(|window, cx| window.click("person-form-last", cx));
    cx.update(|window, cx| window.input("Lovelace", cx));
    cx.update(|window, cx| window.click("person-form-city", cx));
    cx.update(|window, cx| window.input("Seoul", cx));
    cx.update(|window, cx| window.render_frame(cx));

    cx.update(|window, cx| window.click("person-form-zone-0", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("person-form-submit", cx));
    cx.update(|window, cx| window.render_frame(cx));

    assert!(!person_form_open(cx), "submitting closes the modal");
    assert!(
        cx.update(|window, _cx| window.try_find("clocks-colleagues").is_some()),
        "the roster should list the new person"
    );
    assert!(
        cx.update(|window, _cx| window.try_find("clocks-colleagues-empty").is_none()),
        "the empty state should be gone once someone exists"
    );
}

/// Clicking a colleague's name opens the edit modal with the stored name and
/// city; one save renames the person, moves the home city and starts a trip.
#[gpui::test]
fn clicking_a_colleague_name_edits_name_and_city(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_path(seeded(dir.path()));
    let data = store.clock_data().expect("clocks");
    // The seed holds Kyiv, Tokyo and Los Angeles.
    let kyiv = data.clocks[0].id.clone();
    let tokyo = data.clocks[1].id.clone();
    let los_angeles = data.clocks[2].id.clone();
    store
        .add_person(&kyiv, "Grace Brewster Hopper")
        .expect("grace");
    let person_id = store
        .clock_data()
        .expect("clocks")
        .people
        .into_iter()
        .find(|person| person.name == "Grace Brewster Hopper")
        .expect("grace")
        .id;

    cx.update(gpui_kit::init);
    let (_shell, cx) = cx.add_window_view(|_window, _cx| AppShell::with_store(store));
    open_clocks(cx);
    open_people_tab(cx);

    cx.update(|window, cx| window.click(format!("clock-person-open-{person_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    for id in [
        "person-edit-first",
        "person-edit-middle",
        "person-edit-last",
        "person-edit-home-trigger",
        "person-edit-travel-trigger",
        "person-edit-save",
        "person-edit-cancel",
    ] {
        assert!(
            cx.update(|window, _cx| window.try_find(id).is_some()),
            "the edit modal is missing {id}"
        );
    }

    // The footer must be reachable by a hit-tested click, not only by id.
    cx.update(|window, cx| {
        let size = window.find("person-edit-cancel").bounds().size;
        window.click_at(
            "person-edit-cancel",
            gpui::point(size.width / 2., size.height / 2.),
            cx,
        );
    });
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        cx.update(|window, _cx| window.try_find("person-edit").is_none()),
        "a hit-tested click on Cancel must reach the button"
    );
    cx.update(|window, cx| window.click(format!("clock-person-open-{person_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));

    // Opening a list floats it over the modal: the panel height is unchanged
    // and the list renders at the app root, outside the scroll body.
    let modal_height = cx.update(|window, _cx| window.find("person-edit").bounds().size.height);
    // The footer must sit inside the panel: a footer painted past the card's
    // hitbox is unreachable by the pointer.
    cx.update(|window, _cx| {
        let panel = window.find("person-edit").bounds();
        let footer = window.find("person-edit-footer").bounds();
        assert!(
            footer.origin.y >= panel.origin.y
                && footer.origin.y + footer.size.height <= panel.origin.y + panel.size.height,
            "the footer must stay inside the panel card: panel {panel:?}, footer {footer:?}"
        );
    });
    cx.update(|window, cx| window.click("person-edit-home-trigger", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.find("person-edit-home-menu").visible(),
            "the home list floats over the modal"
        );
        assert!(
            window.try_find("person-edit-menu-layer").is_some(),
            "the list is rendered at the app root"
        );
        assert_eq!(
            window.find("person-edit").bounds().size.height,
            modal_height,
            "an open list must not change the modal height"
        );
    });
    // Esc dismisses the list first, leaving the modal open.
    cx.update(|window, cx| window.press("escape", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, _cx| {
        assert!(
            window.try_find("person-edit-home-menu").is_none(),
            "Esc closes the floating list"
        );
        assert!(
            window.try_find("person-edit").is_some(),
            "the modal stays open while a list is dismissed"
        );
    });

    // Esc closes the modal without saving.
    cx.update(|window, cx| window.click("person-edit-first", cx));
    cx.update(|window, cx| window.press("escape", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        cx.update(|window, _cx| window.try_find("person-edit").is_none()),
        "Esc closes the edit modal"
    );
    assert_eq!(
        Store::open_path(dir.path().join("kandrium.db"))
            .clock_data()
            .expect("clocks")
            .people[0]
            .name,
        "Grace Brewster Hopper",
        "Esc must not save"
    );

    // Reopen: edit the surname, pick a new home from the dropdown and pick a
    // trip destination from the other dropdown, then Save once.
    cx.update(|window, cx| window.click(format!("clock-person-open-{person_id}"), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("person-edit-last", cx));
    cx.update(|window, cx| window.press("cmd-a", cx));
    cx.update(|window, cx| window.input("Hopper-Smith", cx));
    cx.update(|window, cx| window.click("person-edit-home-trigger", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("person-edit-home-search", cx));
    cx.update(|window, cx| window.input("Tokyo", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("person-edit-home-option-0", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("person-edit-travel-trigger", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("person-edit-travel-search", cx));
    cx.update(|window, cx| window.input("Los Angeles", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("person-edit-travel-option-0", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("person-edit-save", cx));
    cx.update(|window, cx| window.render_frame(cx));

    assert!(
        cx.update(|window, _cx| window.try_find("person-edit").is_none()),
        "saving closes the edit modal"
    );
    let data = Store::open_path(dir.path().join("kandrium.db"))
        .clock_data()
        .expect("clocks");
    let edited = data
        .people
        .into_iter()
        .find(|person| person.id == person_id)
        .expect("the person survives the edit");
    assert_eq!(edited.name, "Grace Brewster Hopper-Smith");
    assert_eq!(
        edited.home_clock_id, tokyo,
        "the picked city becomes the home clock"
    );
    assert_ne!(edited.home_clock_id, kyiv);
    let travel = data
        .travels
        .into_iter()
        .find(|travel| travel.person_id == person_id)
        .expect("the trip is started in the same save");
    assert_eq!(
        travel.destination_clock_id.as_deref(),
        Some(los_angeles.as_str())
    );
    assert_eq!(travel.origin_clock_id.as_deref(), Some(tokyo.as_str()));
}

/// The toolbar search filters the People roster by name and by city, and
/// clearing it brings every row back.
#[gpui::test]
fn people_search_filters_the_roster(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_path(seeded(dir.path()));
    let data = store.clock_data().expect("clocks");
    // The seed holds Kyiv, Tokyo and Los Angeles.
    let kyiv = data.clocks[0].id.clone();
    let tokyo = data.clocks[1].id.clone();
    store.add_person(&kyiv, "Grace Hopper").expect("grace");
    store.add_person(&tokyo, "Ada Lovelace").expect("ada");
    let people = store.clock_data().expect("clocks").people;
    let grace = people
        .iter()
        .find(|person| person.name == "Grace Hopper")
        .expect("grace")
        .id
        .clone();
    let ada = people
        .iter()
        .find(|person| person.name == "Ada Lovelace")
        .expect("ada")
        .id
        .clone();
    let grace_row = format!("clock-person-{grace}-{kyiv}");
    let ada_row = format!("clock-person-{ada}-{tokyo}");

    cx.update(gpui_kit::init);
    let (_shell, cx) = cx.add_window_view(|_window, _cx| AppShell::with_store(store));
    open_clocks(cx);
    open_people_tab(cx);
    assert!(cx.update(|window, _cx| window.try_find(grace_row.clone()).is_some()));
    assert!(cx.update(|window, _cx| window.try_find(ada_row.clone()).is_some()));

    cx.update(|window, cx| window.click("clocks-search", cx));
    cx.update(|window, cx| window.input("grace", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        cx.update(|window, _cx| window.try_find(grace_row.clone()).is_some()),
        "a name match keeps its row"
    );
    assert!(
        cx.update(|window, _cx| window.try_find(ada_row.clone()).is_none()),
        "a non-matching row is filtered out"
    );

    for _ in 0..5 {
        cx.update(|window, cx| window.press("backspace", cx));
    }
    cx.update(|window, cx| window.input("kyiv", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        cx.update(|window, _cx| window.try_find(grace_row.clone()).is_some()),
        "a city match keeps its row"
    );
    assert!(
        cx.update(|window, _cx| window.try_find(ada_row.clone()).is_none()),
        "a different city is filtered out"
    );

    for _ in 0..4 {
        cx.update(|window, cx| window.press("backspace", cx));
    }
    cx.update(|window, cx| window.render_frame(cx));
    assert!(cx.update(|window, _cx| window.try_find(grace_row.clone()).is_some()));
    assert!(cx.update(|window, _cx| window.try_find(ada_row.clone()).is_some()));
}

/// A legacy single-word name can be opened and saved through the rename form;
/// the add-person form still demands a first and a last name.
#[gpui::test]
fn rename_accepts_a_single_word_name(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_path(seeded(dir.path()));
    let clock_id = store.clock_data().expect("clocks").clocks[0].id.clone();
    store.add_person(&clock_id, "Ada").expect("person");
    let person_id = store.clock_data().expect("clocks").people[0].id.clone();

    cx.update(gpui_kit::init);
    let (_shell, cx) = cx.add_window_view(|_window, _cx| AppShell::with_store(store));
    open_clocks(cx);
    open_inspector(cx, &clock_id);

    let rename = format!("clock-person-rename-{person_id}");
    cx.update(move |window, cx| window.click(rename, cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(cx.update(|window, _cx| window.try_find("clock-person-rename-first").is_some()));

    cx.update(|window, cx| window.press("enter", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        cx.update(|window, _cx| window.try_find("clock-person-rename-first").is_none()),
        "Enter saves the split name instead of holding the form open"
    );
}

/// The People tab is a cross-city roster; with nobody attached to a clock it
/// explains itself rather than showing an empty table.
#[gpui::test]
fn people_tab_shows_its_empty_state(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(gpui_kit::init);
    let (_shell, cx) = cx
        .add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(seeded(dir.path()))));
    open_clocks(cx);
    assert!(
        cx.update(|window, _cx| window.try_find("clocks-grid").is_some()),
        "the city grid stays the default view"
    );

    open_people_tab(cx);
    assert!(
        cx.update(|window, _cx| window.try_find("clocks-colleagues-empty").is_some()),
        "an empty roster needs its own empty state"
    );
    assert!(cx.update(|window, _cx| window.try_find("clocks-grid").is_none()));
}

/// A person attached to a city appears in the roster with that city's time;
/// the row id pairs the person and the clock so duplicates stay distinct.
#[gpui::test]
fn people_tab_lists_every_person_with_their_city(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_path(seeded(dir.path()));
    let clock_id = store.clock_data().expect("clocks").clocks[0].id.clone();
    store.add_person(&clock_id, "Ada Lovelace").expect("person");
    let person_id = store.clock_data().expect("clocks").people[0].id.clone();
    cx.update(gpui_kit::init);
    let (_shell, cx) = cx.add_window_view(|_window, _cx| AppShell::with_store(store));
    open_clocks(cx);

    open_people_tab(cx);
    let row = format!("clock-person-{person_id}-{clock_id}");
    let found = cx.update({
        let row = row.clone();
        move |window, _cx| window.try_find(row).is_some()
    });
    assert!(found, "the roster needs a row for {row}");
}

/// Hiding from the edit modal removes the colleague from the roster and the
/// city counts while the profile and its slot stay in place; showing them
/// again restores the same position.
#[gpui::test]
fn hiding_from_the_edit_modal_keeps_the_profile(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = seeded(dir.path());
    let store = Store::open_path(db_path.clone());
    let kyiv = store.clock_data().expect("clocks").clocks[0].id.clone();
    store.add_person(&kyiv, "Ada Lovelace").expect("ada");
    store.add_person(&kyiv, "Grace Hopper").expect("grace");
    let people = store.clock_data().expect("clocks").people;
    let ada = people
        .iter()
        .find(|person| person.name == "Ada Lovelace")
        .expect("ada")
        .clone();
    let grace = people
        .iter()
        .find(|person| person.name == "Grace Hopper")
        .expect("grace")
        .clone();
    assert_eq!(ada.position, 0);
    assert_eq!(grace.position, 1);
    let ada_row = format!("clock-person-{}-{kyiv}", ada.id);
    let grace_row = format!("clock-person-{}-{kyiv}", grace.id);

    cx.update(gpui_kit::init);
    let (_shell, cx) = cx.add_window_view(|_window, _cx| AppShell::with_store(store));
    open_clocks(cx);
    open_people_tab(cx);
    assert!(cx.update({
        let row = ada_row.clone();
        move |window, _cx| window.try_find(row).is_some()
    }));

    cx.update({
        let open = format!("clock-person-open-{}", ada.id);
        move |window, cx| window.click(open, cx)
    });
    cx.update(|window, cx| window.render_frame(cx));
    assert!(
        cx.update(|window, _cx| window.try_find("person-edit-hide").is_some()),
        "the edit modal needs the hide action"
    );
    cx.update(|window, cx| window.click("person-edit-hide", cx));
    cx.update(|window, cx| window.render_frame(cx));

    assert!(
        cx.update(|window, _cx| window.try_find("person-edit").is_none()),
        "hiding closes the edit modal"
    );
    assert!(
        cx.update({
            let row = ada_row.clone();
            move |window, _cx| window.try_find(row).is_none()
        }),
        "a hidden person leaves the Clocks roster"
    );
    assert!(
        cx.update({
            let row = grace_row.clone();
            move |window, _cx| window.try_find(row).is_some()
        }),
        "the other colleague stays on the roster"
    );

    let reopened = Store::open_path(db_path);
    let visible = reopened.clock_data().expect("clocks").people;
    assert_eq!(visible.len(), 1, "hidden people are not in Clocks counts");
    assert_eq!(visible[0].id, grace.id);
    assert_eq!(visible[0].position, 1, "hiding keeps the retained slot");
    let profile = reopened.get_person(&ada.id).expect("profile stays");
    assert_eq!(profile.name, "Ada Lovelace");
    assert!(!profile.in_clocks, "the person is hidden, not deleted");
    assert_eq!(
        reopened.list_persons("", "all").expect("people").len(),
        2,
        "the hidden profile stays in the Network list"
    );

    reopened.set_person_in_clocks(&ada.id, true).expect("show");
    let restored = reopened.clock_data().expect("clocks").people;
    let ada_again = restored
        .iter()
        .find(|person| person.id == ada.id)
        .expect("the person is back");
    assert_eq!(ada_again.position, 0, "showing restores the same slot");
}
