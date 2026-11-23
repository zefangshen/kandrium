//! Network flows through the real `Store` and `kandrium-core`.
//!
//! These tests are headless: they boot a seeded temp database, then drive the
//! section the same way the shell does. Person validation, placement and the
//! delete cascade all come from core.

use std::path::{Path, PathBuf};

use gpui::TestAppContext;
use gpui_kit::test::TestWindowExt;

use kandrium_app::app::{AppShell, Section};
use kandrium_app::settings::Settings;
use kandrium_app::store::Store;
use kandrium_core::persons::{NewPerson, PersonPatch};

/// A bootstrapped database (schema + seed) in the temp directory.
fn seeded(dir: &Path) -> PathBuf {
    let path = dir.join("kandrium.db");
    let mut db = kandrium_core::open_db(&path).expect("open temp db");
    kandrium_core::bootstrap(&mut db, &path).expect("bootstrap temp db");
    path
}

type NetworkUi<'a> = &'a mut gpui::VisualTestContext;

fn open_network(cx: NetworkUi) {
    cx.update(|window, cx| window.click("nav-network", cx));
    cx.update(|window, cx| window.render_frame(cx));
}

fn type_into(cx: NetworkUi, id: impl Into<String>, value: &str) {
    let id = id.into();
    cx.update(|window, cx| window.click(id.clone(), cx));
    cx.update(|window, cx| window.press("cmd-a", cx));
    cx.update(|window, cx| window.input(value, cx));
}

#[gpui::test]
fn network_section_renders_empty_and_restores_its_setting(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded(dir.path());
    cx.update(gpui_kit::init);

    let (_shell, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(cx.update(|window, _cx| window.try_find("nav-network").is_some()));

    open_network(cx);
    assert!(
        cx.update(|window, _cx| window.find("network-view").visible()),
        "the Network section renders its body"
    );
    assert!(
        cx.update(|window, _cx| window.find("network-empty").visible()),
        "an empty address book shows the empty state"
    );

    let settings = dir.path().join("gpui-settings.conf");
    let stored = Settings::load(&settings);
    assert_eq!(
        stored.section(),
        Section::Network,
        "section=network is saved"
    );

    let again = AppShell::with_store(Store::open_path(path));
    assert_eq!(again.section(), Section::Network);
}

#[gpui::test]
fn toolbar_search_takes_the_slack(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded(dir.path());
    cx.update(gpui_kit::init);

    let (_shell, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path.clone())));
    cx.update(|window, cx| window.render_frame(cx));
    open_network(cx);

    cx.update(|window, _cx| {
        let toolbar = window.find("network-toolbar").bounds();
        let search = window.find("network-search").bounds();
        let status = window.find("network-status-trigger").bounds();
        let add = window.find("network-add-person").bounds();
        let width = |bounds: gpui::Bounds<gpui::Pixels>| f32::from(bounds.size.width);
        let centre = |bounds: gpui::Bounds<gpui::Pixels>| {
            f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.0
        };
        let right =
            |bounds: gpui::Bounds<gpui::Pixels>| f32::from(bounds.origin.x + bounds.size.width);

        assert!((width(toolbar) - 299.0).abs() < 1.0);
        assert!(
            (width(search) - 227.0).abs() < 2.0,
            "the search takes the slack: {}",
            width(search)
        );
        assert!(
            (right(toolbar) - right(add) - 8.0).abs() < 1.5,
            "the add action sits at the row's right edge"
        );
        assert!(
            (f32::from(status.origin.x) - right(search) - 4.0).abs() < 1.5,
            "search right {} status x {}",
            right(search),
            f32::from(status.origin.x)
        );
        assert!((centre(search) - centre(status)).abs() < 1.0);
        assert!((centre(search) - centre(add)).abs() < 1.0);
    });
}

#[gpui::test]
fn create_edit_and_filter_people(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded(dir.path());
    let store = Store::open_path(path.clone());
    let ana = store
        .create_person(NewPerson {
            role_title: "Analyst".to_string(),
            organization: "Lab".to_string(),
            ..NewPerson::new("Ana Torres")
        })
        .expect("ana");
    let ben = store
        .create_person(NewPerson {
            role_title: "Designer".to_string(),
            ..NewPerson::new("Ben Okafor")
        })
        .expect("ben");
    store
        .update_person(
            &ben.id,
            PersonPatch {
                status: Some("dormant".to_string()),
                ..PersonPatch::default()
            },
        )
        .expect("dormant");

    cx.update(gpui_kit::init);
    let (_shell, cx) = cx.add_window_view(|_window, _cx| AppShell::with_store(store));
    open_network(cx);
    assert!(cx.update(|window, _cx| { window.try_find("network-expertise-trigger").is_none() }));
    assert!(cx.update(|window, _cx| { window.try_find("network-status-trigger").is_some() }));
    cx.update(|window, cx| window.click("network-add-person", cx));
    cx.update(|window, cx| window.render_frame(cx));

    type_into(cx, "network-editor-first", "Grace");
    type_into(cx, "network-editor-last", "Hopper");
    type_into(cx, "network-editor-role", "Engineer");
    type_into(cx, "network-editor-organization", "Navy");
    type_into(cx, "network-editor-expertise", "Rust");
    cx.update(|window, cx| window.press("enter", cx));
    cx.update(|window, cx| window.click("network-editor-save", cx));
    cx.update(|window, cx| window.render_frame(cx));

    let reopened = Store::open_path(path.clone());
    let grace = reopened
        .list_persons("grace", "all")
        .expect("people")
        .into_iter()
        .next()
        .expect("grace exists");
    assert_eq!(grace.role_title, "Engineer");
    assert_eq!(grace.organization, "Navy");
    assert_eq!(grace.expertise, vec!["Rust"]);
    assert!(
        !grace.in_clocks,
        "Network-created people start hidden from Clocks"
    );

    // Edit the role through the detail header.
    cx.update(|window, cx| window.click(format!("network-person-{}", grace.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("network-edit", cx));
    cx.update(|window, cx| window.render_frame(cx));
    type_into(cx, "network-editor-role", "Admiral");
    cx.update(|window, cx| window.click("network-editor-save", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert_eq!(
        reopened.get_person(&grace.id).expect("grace").role_title,
        "Admiral"
    );

    // Search narrows the rail without touching the detail selection.
    type_into(cx, "network-search", "ana");
    cx.update(|window, cx| window.render_frame(cx));
    assert!(cx.update(|window, _cx| {
        window
            .try_find(format!("network-person-{}", ana.id))
            .is_some()
    }));
    assert!(cx.update(|window, _cx| {
        window
            .try_find(format!("network-person-{}", ben.id))
            .is_none()
    }));

    // Clear the search, then narrow by status.
    cx.update(|window, cx| window.click("network-search", cx));
    cx.update(|window, cx| window.press("cmd-a", cx));
    cx.update(|window, cx| window.press("backspace", cx));
    cx.update(|window, cx| window.click("network-status-trigger", cx));
    cx.update(|window, cx| window.click("network-status-dormant", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(cx.update(|window, _cx| {
        window
            .try_find(format!("network-person-{}", ben.id))
            .is_some()
    }));
    assert!(cx.update(|window, _cx| {
        window
            .try_find(format!("network-person-{}", ana.id))
            .is_none()
    }));
}

#[gpui::test]
fn place_hide_and_show_on_clocks(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded(dir.path());
    let store = Store::open_path(path.clone());
    let person = store
        .create_person(NewPerson::new("Network Only"))
        .expect("person");
    let clock = store.clock_data().expect("clocks").clocks[0].id.clone();

    cx.update(gpui_kit::init);
    let (_shell, cx) = cx.add_window_view(|_window, _cx| AppShell::with_store(store));
    open_network(cx);
    cx.update(|window, cx| window.click(format!("network-person-{}", person.id), cx));
    cx.update(|window, cx| window.render_frame(cx));

    assert!(
        cx.update(|window, _cx| window.try_find("network-place").is_some()),
        "an unplaced person offers the placement action"
    );
    cx.update(|window, cx| window.click("network-place", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("network-place-city-{clock}"), cx));
    cx.update(|window, cx| window.render_frame(cx));

    let reopened = Store::open_path(path.clone());
    let placed = reopened.clock_data().expect("clocks").people;
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].id, person.id);
    let position = placed[0].position;

    // Hide from Clocks: gone from the roster, still in Network.
    cx.update(|window, cx| window.click("network-clocks-toggle", cx));
    cx.update(|window, cx| window.render_frame(cx));
    let hidden = reopened.clock_data().expect("clocks").people;
    assert!(
        hidden.is_empty(),
        "a hidden person is not a Clocks colleague"
    );
    let profile = reopened.get_person(&person.id).expect("profile stays");
    assert!(!profile.in_clocks);
    assert_eq!(reopened.list_persons("", "all").expect("people").len(), 1);
    assert!(
        cx.update(|window, _cx| window
            .try_find(format!("network-person-{}", person.id))
            .is_some()),
        "a hidden person stays in the Network list"
    );

    // Show again: restored in the same slot.
    cx.update(|window, cx| window.click("network-clocks-toggle", cx));
    cx.update(|window, cx| window.render_frame(cx));
    let restored = reopened.clock_data().expect("clocks").people;
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].id, person.id);
    assert_eq!(
        restored[0].position, position,
        "the retained slot is reused"
    );
}

#[gpui::test]
fn clocks_can_open_a_colleague_in_network(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded(dir.path());
    let store = Store::open_path(path.clone());
    let person = store
        .create_person(NewPerson::new("Reachable Person"))
        .expect("person");
    let clock = store.clock_data().expect("clocks").clocks[0].id.clone();
    store
        .add_existing_person(&clock, &person.id)
        .expect("place the colleague");

    cx.update(gpui_kit::init);
    let (_shell, cx) =
        cx.add_window_view(|_window, _cx| AppShell::with_store(Store::open_path(path)));
    cx.update(|window, cx| window.click("nav-clocks", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("clocks-tab-colleagues", cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click(format!("clock-person-open-{}", person.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("person-edit-open-network", cx));
    cx.update(|window, cx| window.render_frame(cx));

    assert!(cx.update(|window, _cx| window.find("network-view").visible()));
    assert!(
        cx.update(|window, _cx| window
            .try_find(format!("network-person-{}", person.id))
            .is_some()),
        "the Clocks modal selects the same person in Network"
    );
}

#[gpui::test]
fn deleting_a_person_asks_first(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = seeded(dir.path());
    let store = Store::open_path(path.clone());
    let person = store
        .create_person(NewPerson::new("Temporary Contact"))
        .expect("person");

    cx.update(gpui_kit::init);
    let (_shell, cx) = cx.add_window_view(|_window, _cx| AppShell::with_store(store));
    open_network(cx);
    cx.update(|window, cx| window.click(format!("network-person-{}", person.id), cx));
    cx.update(|window, cx| window.render_frame(cx));
    cx.update(|window, cx| window.click("network-delete", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(cx.update(|window, _cx| window.find("network-confirm").visible()));

    cx.update(|window, cx| window.click("network-confirm-cancel", cx));
    cx.update(|window, cx| window.render_frame(cx));
    assert!(cx.update(|window, _cx| window.try_find("network-confirm").is_none()));

    cx.update(|window, cx| window.click("network-delete", cx));
    cx.update(|window, cx| window.click("network-confirm-delete", cx));
    cx.update(|window, cx| window.render_frame(cx));

    let reopened = Store::open_path(path);
    assert!(reopened.get_person(&person.id).is_err());
    assert!(reopened.list_persons("", "all").expect("people").is_empty());
}
