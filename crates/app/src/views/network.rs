//! Network: the people address book.
//!
//! Every person lives here. Clocks only renders a person when
//! `persons.in_clocks = 1`; a Network contact can be placed on a saved city
//! card, which creates the retained placement and shows them on Clocks.
//!
//! The view is a fixed 300 px alphabetical rail plus a detail pane. Editors,
//! the placement picker and the delete confirmation are absolute layers inside
//! the section, following the Clocks panels.

use std::collections::HashMap;

use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, TestSupportExt, Window, div, px, rgba,
};

use kandrium_core::city_name;
use kandrium_core::clocks::available_zones;
use kandrium_core::persons::{PERSON_STATUSES, Person};

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::clocks::{initials, picker_rows};
use crate::views::{field_input, field_textarea};

/// Width of the fixed alphabetical rail (no drag resize in v1).
pub const RAIL_W: f32 = 300.0;

/// A saved city the picker can place a person on.
#[derive(Clone, Debug)]
pub struct NetworkCity {
    pub id: String,
    pub zone: String,
    pub city: String,
}

/// Where a person is placed, including placements retained while hidden from
/// Clocks.
#[derive(Clone, Debug)]
pub struct NetworkPlacement {
    pub clock_id: String,
    pub city: String,
}

/// Everything the Network section renders.
pub struct NetworkState {
    pub people: Vec<Person>,
    pub selected: Option<String>,
    /// Search field, created on first render so the shell owns the buffer.
    pub search: Option<Entity<InputState>>,
    pub query: String,
    pub status_filter: String,
    pub expertise_filter: Option<String>,
    pub expertise_labels: Vec<String>,
    pub error: Option<String>,
    pub editor: Option<NetworkEditor>,
    pub place_picker: Option<PlacePicker>,
    pub confirm: Option<NetworkConfirm>,
    pub status_menu: bool,
    pub expertise_menu: bool,
    /// Saved cities, in their Clocks order.
    pub cities: Vec<NetworkCity>,
    /// Person → city placement for every person, hidden ones included.
    pub placements: HashMap<String, NetworkPlacement>,
    /// Saved zones first, then every IANA zone, for the pickers.
    pub zones: Vec<String>,
}

impl NetworkState {
    pub fn new() -> Self {
        Self {
            people: Vec::new(),
            selected: None,
            search: None,
            query: String::new(),
            status_filter: "all".to_string(),
            expertise_filter: None,
            expertise_labels: Vec::new(),
            error: None,
            editor: None,
            place_picker: None,
            confirm: None,
            status_menu: false,
            expertise_menu: false,
            cities: Vec::new(),
            placements: HashMap::new(),
            zones: Vec::new(),
        }
    }
}

impl Default for NetworkState {
    fn default() -> Self {
        Self::new()
    }
}

/// The create/edit modal's fields.
pub struct NetworkEditor {
    /// `None` creates a person; `Some` edits their profile.
    pub person_id: Option<String>,
    pub focus: FocusHandle,
    pub first: Entity<InputState>,
    pub middle: Entity<InputState>,
    pub last: Entity<InputState>,
    pub role: Entity<InputState>,
    pub organization: Entity<InputState>,
    pub collaboration: Entity<InputState>,
    pub email: Entity<InputState>,
    pub url: Entity<InputState>,
    pub location: Entity<InputState>,
    pub last_contact: Entity<InputState>,
    pub how_met: Entity<InputState>,
    pub expertise_input: Entity<InputState>,
    pub expertise: Vec<String>,
    pub notes: Entity<TextareaState>,
    pub status: String,
    pub zone: Option<String>,
    pub zone_input: Entity<InputState>,
    pub zone_open: bool,
    pub error: Option<String>,
}

/// The "Place on a city card" picker.
pub struct PlacePicker {
    pub person_id: String,
    pub person_name: String,
    pub focus: FocusHandle,
    pub search: Entity<InputState>,
    pub zone_input: Entity<InputState>,
    pub new_city_open: bool,
    pub error: Option<String>,
}

/// The delete confirmation layer.
pub struct NetworkConfirm {
    pub person_id: String,
    pub person_name: String,
    pub focus: FocusHandle,
    pub error: Option<String>,
}

pub fn network_view(
    state: &mut NetworkState,
    t: &Tokens,
    window: &mut Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let search = ensure_search_input(state, window, cx);
    let expertise_filter = state.expertise_filter.clone();
    let filtered: Vec<&Person> = state
        .people
        .iter()
        .filter(|person| match &expertise_filter {
            None => true,
            Some(label) => person
                .expertise
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(label)),
        })
        .collect();
    let selected = state
        .selected
        .as_deref()
        .and_then(|id| state.people.iter().find(|person| person.id == id));

    let mut page = div()
        .id("network-view")
        .test_support()
        .relative()
        .size_full()
        .flex()
        .child(rail(state, &filtered, &search, t, cx))
        .child(detail(state, selected, t, cx));

    if let Some(layer) = editor_layer(state, t, window, cx) {
        page = page.child(layer);
    }
    if let Some(layer) = place_picker_layer(state, t, cx) {
        page = page.child(layer);
    }
    if let Some(layer) = confirm_layer(state, t, cx) {
        page = page.child(layer);
    }
    page.into_any_element()
}

fn ensure_search_input(
    state: &mut NetworkState,
    window: &mut Window,
    cx: &mut Context<AppShell>,
) -> Entity<InputState> {
    if let Some(input) = &state.search {
        return input.clone();
    }
    let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search people"));
    cx.observe(&input, |this: &mut AppShell, input, cx| {
        let value = input.read(cx).value().to_string();
        if this.network.query != value {
            this.network.query = value;
            this.reload_network_people();
            cx.notify();
        }
    })
    .detach();
    state.search = Some(input.clone());
    input
}

fn rail(
    state: &NetworkState,
    people: &[&Person],
    search: &Entity<InputState>,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let mut toolbar = div()
        .id("network-toolbar")
        .test_support()
        .flex_none()
        .h(px(38.))
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(8.))
        .border_b_1()
        .border_color(t.divider);
    toolbar = toolbar
        .child(
            div()
                .id("network-search")
                .test_support()
                .w(px(200.))
                .child(field_input(search, t)),
        )
        .child(toolbar_button(
            "network-status-trigger",
            "Status filter",
            false,
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| this.toggle_network_status_menu(cx)),
        ))
        .child(toolbar_button(
            "network-expertise-trigger",
            "Expertise filter",
            false,
            t,
            cx.listener(|this, _: &ClickEvent, _window, cx| this.toggle_network_expertise_menu(cx)),
        ))
        .child(
            div()
                .id("network-add-person")
                .test_support()
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .cursor_pointer()
                .bg(t.accent_soft)
                .text_color(t.accent_text)
                .hover(|style| style.bg(t.selected))
                .child(icon(Icon::Plus, 13., t.accent_text))
                .tooltip({
                    let label = "Add person";
                    move |_window, cx| {
                        cx.new(|_| NameTooltip {
                            label: label.to_string(),
                        })
                        .into()
                    }
                })
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.open_network_editor(None, window, cx)
                })),
        );

    let mut rail = div()
        .id("network-rail")
        .test_support()
        .w(px(RAIL_W))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(t.divider)
        .bg(t.card)
        .child(toolbar);

    if state.status_menu {
        let mut menu = div()
            .id("network-status-menu")
            .test_support()
            .flex_none()
            .border_b_1()
            .border_color(t.divider)
            .py(px(4.));
        for option in ["all", "active", "dormant", "archived"] {
            let label = status_filter_label(option);
            let active = state.status_filter == option;
            menu = menu.child(
                div()
                    .id(format!("network-status-{option}"))
                    .test_support()
                    .mx(px(4.))
                    .px(px(8.))
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(active, |el| el.bg(t.selected))
                    .hover(|style| style.bg(t.hover))
                    .text(Text::Small)
                    .text_color(if active { t.text } else { t.text_muted })
                    .child(label.to_string())
                    .when(active, |el| el.child(icon(Icon::Check, 11., t.accent_text)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.set_network_status_filter(option, cx)
                    })),
            );
        }
        rail = rail.child(menu);
    }

    if state.expertise_menu {
        let mut menu = div()
            .id("network-expertise-menu")
            .test_support()
            .flex_none()
            .max_h(px(180.))
            .overflow_y_scroll()
            .border_b_1()
            .border_color(t.divider)
            .py(px(4.));
        menu = menu.child(
            div()
                .id("network-expertise-all")
                .test_support()
                .mx(px(4.))
                .px(px(8.))
                .h(px(26.))
                .flex()
                .items_center()
                .justify_between()
                .rounded(px(4.))
                .cursor_pointer()
                .when(state.expertise_filter.is_none(), |el| el.bg(t.selected))
                .hover(|style| style.bg(t.hover))
                .text(Text::Small)
                .text_color(if state.expertise_filter.is_none() {
                    t.text
                } else {
                    t.text_muted
                })
                .child("All expertise")
                .when(state.expertise_filter.is_none(), |el| {
                    el.child(icon(Icon::Check, 11., t.accent_text))
                })
                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                    this.set_network_expertise_filter(None, cx)
                })),
        );
        if state.expertise_labels.is_empty() {
            menu = menu.child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .text(Text::Small)
                    .text_color(t.text_faint)
                    .child("No expertise labels yet."),
            );
        }
        for (index, label) in state.expertise_labels.iter().enumerate() {
            let active = state.expertise_filter.as_deref() == Some(label.as_str());
            let value = label.clone();
            menu = menu.child(
                div()
                    .id(format!("network-expertise-filter-{index}"))
                    .test_support()
                    .mx(px(4.))
                    .px(px(8.))
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(active, |el| el.bg(t.selected))
                    .hover(|style| style.bg(t.hover))
                    .text(Text::Small)
                    .text_color(if active { t.text } else { t.text_muted })
                    .child(label.clone())
                    .when(active, |el| el.child(icon(Icon::Check, 11., t.accent_text)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.set_network_expertise_filter(Some(value.clone()), cx)
                    })),
            );
        }
        rail = rail.child(menu);
    }

    let mut list = div()
        .id("network-list")
        .test_support()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .py(px(4.));

    if state.people.is_empty() {
        list = list.child(empty_message(
            "network-empty",
            "No people yet",
            "Add a person to start your network.",
            t,
        ));
    } else if people.is_empty() {
        list = list.child(empty_message(
            "network-no-matches",
            "No matches",
            "Try a different search or filter.",
            t,
        ));
    } else {
        let mut last_header: Option<char> = None;
        for person in people {
            let header = initial_of(&person.name);
            if last_header != Some(header) {
                last_header = Some(header);
                list = list.child(
                    div()
                        .px(px(12.))
                        .pt(px(8.))
                        .pb(px(2.))
                        .text(Text::Label)
                        .text_color(t.text_faint)
                        .child(header.to_string()),
                );
            }
            list = list.child(person_row(state, person, t, cx));
        }
    }
    rail = rail.child(list);
    rail.into_any_element()
}

fn toolbar_button(
    id: &'static str,
    tooltip: &'static str,
    _active: bool,
    t: &Tokens,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .cursor_pointer()
        .text_color(t.text_muted)
        .hover(|style| style.bg(t.hover))
        .child(icon(Icon::ChevronDown, 13., t.text_muted))
        .tooltip(move |_window, cx| {
            cx.new(|_| NameTooltip {
                label: tooltip.to_string(),
            })
            .into()
        })
        .on_click(handler)
        .into_any_element()
}

fn person_row(
    state: &NetworkState,
    person: &Person,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let id = person.id.clone();
    let selected = state.selected.as_deref() == Some(person.id.as_str());
    let placed = state.placements.get(&person.id);
    let in_clocks = person.in_clocks && placed.is_some();
    let meta = role_line(person);
    let status = status_dot(person, t);

    div()
        .id(format!("network-person-{}", person.id))
        .test_support()
        .mx(px(4.))
        .px(px(8.))
        .py(px(6.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(5.))
        .cursor_pointer()
        .when(selected, |el| el.bg(t.selected))
        .hover(|style| style.bg(t.hover))
        .child(
            div()
                .size(px(24.))
                .flex_none()
                .rounded_full()
                .bg(t.surface_2)
                .flex()
                .items_center()
                .justify_center()
                .text(Text::Micro)
                .text_color(t.text_muted)
                .child(initials(&person.name)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text(Text::CardTitle)
                        .text_color(t.text)
                        .child(person.name.clone()),
                )
                .when(!meta.is_empty(), |el| {
                    el.child(
                        div()
                            .truncate()
                            .text(Text::Small)
                            .text_color(t.text_faint)
                            .child(meta),
                    )
                }),
        )
        .when(in_clocks, |el| {
            el.child(
                div()
                    .flex_none()
                    .px(px(5.))
                    .h(px(16.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .bg(t.accent_soft)
                    .text(Text::Micro)
                    .text_color(t.accent_text)
                    .child("In Clocks"),
            )
        })
        .child(status)
        .on_click(
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.select_network_person(&id, cx)
            }),
        )
        .into_any_element()
}

fn status_dot(person: &Person, t: &Tokens) -> AnyElement {
    let color = match person.status.as_str() {
        "active" => t.accent,
        "dormant" => t.text_muted,
        _ => t.text_ghost,
    };
    div()
        .size(px(6.))
        .flex_none()
        .rounded_full()
        .bg(color)
        .into_any_element()
}

fn detail(
    state: &NetworkState,
    person: Option<&Person>,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let Some(person) = person else {
        return div()
            .id("network-detail")
            .test_support()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.))
                    .child(icon(Icon::Users, 28., t.text_ghost))
                    .child(
                        div()
                            .text(Text::Body)
                            .text_color(t.text_muted)
                            .child("Select a person"),
                    )
                    .child(
                        div()
                            .text(Text::Small)
                            .text_color(t.text_faint)
                            .child("Their profile and Clocks placement show here."),
                    ),
            )
            .into_any_element();
    };

    let placement = state.placements.get(&person.id);
    let status_label = capitalize(&person.status);
    let meta = role_line(person);
    let person_id = person.id.clone();

    let header = div()
        .flex_none()
        .px(px(24.))
        .py(px(18.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .border_b_1()
        .border_color(t.divider)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text(Text::Heading)
                        .text_color(t.text)
                        .child(person.name.clone()),
                )
                .child(status_pill(&status_label, &person.status, t))
                .child(small_button(
                    "network-edit",
                    "Edit",
                    false,
                    t,
                    cx.listener({
                        let id = person_id.clone();
                        move |this, _: &ClickEvent, window, cx| {
                            this.open_network_editor(Some(&id), window, cx)
                        }
                    }),
                ))
                .child(small_button(
                    "network-delete",
                    "Delete",
                    true,
                    t,
                    cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_network_delete(window, cx)
                    }),
                )),
        )
        .when(!meta.is_empty(), |el| {
            el.child(div().text(Text::Body).text_color(t.text_muted).child(meta))
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .when(person.in_clocks && placement.is_some(), |el| {
                    el.child(small_button(
                        "network-clocks-toggle",
                        "Hide from Clocks",
                        false,
                        t,
                        cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.toggle_network_clocks(cx)
                        }),
                    ))
                })
                .when(!person.in_clocks && placement.is_some(), |el| {
                    el.child(small_button(
                        "network-clocks-toggle",
                        "Show on Clocks",
                        false,
                        t,
                        cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.toggle_network_clocks(cx)
                        }),
                    ))
                })
                .when(placement.is_none(), |el| {
                    el.child(small_button(
                        "network-place",
                        "Place on a city card",
                        false,
                        t,
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.open_network_place_picker(window, cx)
                        }),
                    ))
                })
                .when(placement.is_some(), |el| {
                    el.child(small_button(
                        "network-open-in-clocks",
                        "Open in Clocks",
                        false,
                        t,
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.open_network_person_in_clocks(window, cx)
                        }),
                    ))
                }),
        );

    let mut body = div()
        .id("network-detail-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(24.))
        .py(px(16.))
        .flex()
        .flex_col()
        .gap(px(18.));

    body = body.child(detail_section(
        "Expertise",
        expertise_chips(person, t, cx),
        t,
    ));
    body = body.child(detail_section(
        "Collaboration",
        detail_text(&person.collaboration, "No collaboration notes.", t),
        t,
    ));
    body = body.child(detail_section("Contact", contact_block(person, t), t));
    body = body.child(detail_section("Location", location_block(person, t), t));
    body = body.child(detail_section(
        "Network",
        network_block(person, placement, t),
        t,
    ));
    body = body.child(detail_section("Notes", notes_block(person, t), t));

    div()
        .id("network-detail")
        .test_support()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .flex()
        .flex_col()
        .child(header)
        .child(body)
        .into_any_element()
}

fn detail_section(title: &'static str, body: AnyElement, t: &Tokens) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text(Text::Label)
                .text_color(t.text_faint)
                .child(title.to_uppercase()),
        )
        .child(body)
        .into_any_element()
}

fn detail_text(value: &str, empty: &str, t: &Tokens) -> AnyElement {
    let (text, color) = if value.trim().is_empty() {
        (empty.to_string(), t.text_faint)
    } else {
        (value.to_string(), t.text_muted)
    };
    div()
        .text(Text::Body)
        .text_color(color)
        .child(text)
        .into_any_element()
}

fn expertise_chips(person: &Person, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    if person.expertise.is_empty() {
        return div()
            .text(Text::Small)
            .text_color(t.text_faint)
            .child("No expertise yet.")
            .into_any_element();
    }
    let mut chips = div().flex().flex_wrap().gap(px(6.));
    for (index, label) in person.expertise.iter().enumerate() {
        let value = label.clone();
        chips = chips.child(
            div()
                .id(format!("network-expertise-{index}"))
                .test_support()
                .px(px(7.))
                .h(px(22.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .bg(t.surface_2)
                .cursor_pointer()
                .hover(|style| style.bg(t.hover))
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(label.clone())
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.set_network_expertise_filter(Some(value.clone()), cx)
                })),
        );
    }
    chips.into_any_element()
}

fn contact_block(person: &Person, t: &Tokens) -> AnyElement {
    let mut rows = div().flex().flex_col().gap(px(3.));
    rows = rows.child(labeled_value("Email", &person.email, t));
    rows = rows.child(labeled_value("URL", &person.url, t));
    rows.into_any_element()
}

fn location_block(person: &Person, t: &Tokens) -> AnyElement {
    let mut rows = div().flex().flex_col().gap(px(3.));
    rows = rows.child(labeled_value("Location", &person.location, t));
    let zone = if person.tz_identifier.is_empty() {
        "None".to_string()
    } else {
        person.tz_identifier.clone()
    };
    rows = rows.child(labeled_value("Time zone", &zone, t));
    rows.into_any_element()
}

fn network_block(person: &Person, placement: Option<&NetworkPlacement>, t: &Tokens) -> AnyElement {
    let last = person
        .last_contact_on
        .clone()
        .unwrap_or_else(|| "Never".to_string());
    let how_met = if person.how_met.trim().is_empty() {
        "—".to_string()
    } else {
        person.how_met.clone()
    };
    let placed = match placement {
        Some(placed) if person.in_clocks => format!("On Clocks · {}", placed.city),
        Some(placed) => format!("Hidden from Clocks · {}", placed.city),
        None => "Not on Clocks".to_string(),
    };
    let mut rows = div().flex().flex_col().gap(px(3.));
    rows = rows.child(labeled_value("Last contact", &last, t));
    rows = rows.child(labeled_value("How met", &how_met, t));
    rows = rows.child(labeled_value("Clocks", &placed, t));
    rows.into_any_element()
}

fn notes_block(person: &Person, t: &Tokens) -> AnyElement {
    if person.notes.trim().is_empty() {
        return div()
            .text(Text::Small)
            .text_color(t.text_faint)
            .child("No notes yet.")
            .into_any_element();
    }
    div()
        .text(Text::Reading)
        .text_color(t.text_muted)
        .child(person.notes.clone())
        .into_any_element()
}

fn labeled_value(label: &'static str, value: &str, t: &Tokens) -> AnyElement {
    let (text, color) = if value.trim().is_empty() || value == "—" {
        ("—".to_string(), t.text_faint)
    } else {
        (value.to_string(), t.text_muted)
    };
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .w(px(96.))
                .flex_none()
                .text(Text::Small)
                .text_color(t.text_faint)
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text(Text::Small)
                .text_color(color)
                .child(text),
        )
        .into_any_element()
}

fn status_pill(label: &str, status: &str, t: &Tokens) -> AnyElement {
    let (bg, fg) = match status {
        "active" => (t.accent_soft, t.accent_text),
        "dormant" => (t.surface_2, t.text_muted),
        _ => (t.hover, t.text_faint),
    };
    div()
        .px(px(7.))
        .h(px(20.))
        .flex_none()
        .flex()
        .items_center()
        .rounded(px(4.))
        .bg(bg)
        .text(Text::Small)
        .text_color(fg)
        .child(label.to_string())
        .into_any_element()
}

fn small_button(
    id: &'static str,
    label: &'static str,
    danger: bool,
    t: &Tokens,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .h(px(26.))
        .px(px(9.))
        .flex()
        .items_center()
        .rounded(px(t.radius))
        .cursor_pointer()
        .text(Text::Button)
        .text_color(if danger { t.danger } else { t.text_muted })
        .hover(|style| style.bg(t.hover))
        .child(label)
        .on_click(handler)
        .into_any_element()
}

fn empty_message(id: &'static str, title: &str, detail: &str, t: &Tokens) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .p(px(18.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .child(
            div()
                .text(Text::Body)
                .text_color(t.text_muted)
                .child(title.to_string()),
        )
        .child(
            div()
                .text(Text::Small)
                .text_color(t.text_faint)
                .child(detail.to_string()),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Create / edit modal
// ---------------------------------------------------------------------------

fn editor_layer(
    state: &NetworkState,
    t: &Tokens,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    let editor = state.editor.as_ref()?;
    let zone_query = editor.zone_input.read(cx).value();
    let zone_rows = zone_rows(&state.zones, zone_query.as_ref());
    let query = editor.expertise_input.read(cx).value().to_string();
    let suggestions: Vec<String> = state
        .expertise_labels
        .iter()
        .filter(|label| {
            !editor
                .expertise
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(label))
                && (query.trim().is_empty()
                    || label.to_lowercase().contains(&query.trim().to_lowercase()))
        })
        .take(5)
        .cloned()
        .collect();

    let mut body = div()
        .id("network-editor-body")
        .test_support()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(16.))
        .py(px(12.))
        .flex()
        .flex_col()
        .gap(px(10.));

    body = body
        .child(
            div()
                .flex()
                .gap(px(8.))
                .child(field_block(
                    "network-editor-first",
                    "First name",
                    field_input(&editor.first, t),
                    t,
                ))
                .child(field_block(
                    "network-editor-middle",
                    "Middle name",
                    field_input(&editor.middle, t),
                    t,
                ))
                .child(field_block(
                    "network-editor-last",
                    "Last name",
                    field_input(&editor.last, t),
                    t,
                )),
        )
        .child(
            div()
                .flex()
                .gap(px(8.))
                .child(field_block(
                    "network-editor-role",
                    "Role",
                    field_input(&editor.role, t),
                    t,
                ))
                .child(field_block(
                    "network-editor-organization",
                    "Organization",
                    field_input(&editor.organization, t),
                    t,
                )),
        )
        .child(field_block(
            "network-editor-collaboration",
            "Collaboration",
            field_input(&editor.collaboration, t),
            t,
        ))
        .child(
            div()
                .flex()
                .gap(px(8.))
                .child(field_block(
                    "network-editor-email",
                    "Email",
                    field_input(&editor.email, t),
                    t,
                ))
                .child(field_block(
                    "network-editor-url",
                    "URL",
                    field_input(&editor.url, t),
                    t,
                )),
        )
        .child(
            div()
                .flex()
                .gap(px(8.))
                .child(field_block(
                    "network-editor-location",
                    "Location",
                    field_input(&editor.location, t),
                    t,
                ))
                .child(field_block(
                    "network-editor-last-contact",
                    "Last contact",
                    field_input(&editor.last_contact, t),
                    t,
                )),
        )
        .child(field_block(
            "network-editor-how-met",
            "How met",
            field_input(&editor.how_met, t),
            t,
        ));

    // Expertise chips plus the tag input and its autocomplete.
    let mut expertise = div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(field_input(&editor.expertise_input, t));
    if !editor.expertise.is_empty() {
        let mut chips = div().flex().flex_wrap().gap(px(5.));
        for (index, label) in editor.expertise.iter().enumerate() {
            chips = chips.child(
                div()
                    .id(format!("network-editor-expertise-chip-{index}"))
                    .test_support()
                    .px(px(7.))
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .rounded(px(4.))
                    .bg(t.surface_2)
                    .text(Text::Small)
                    .text_color(t.text_muted)
                    .child(label.clone())
                    .child(
                        div()
                            .id(format!("network-editor-expertise-remove-{index}"))
                            .test_support()
                            .size(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(|style| style.bg(t.pressed))
                            .child(icon(Icon::X, 9., t.text_faint))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                this.remove_network_expertise(index, cx)
                            })),
                    ),
            );
        }
        expertise = expertise.child(chips);
    }
    if !suggestions.is_empty() {
        let mut list = div()
            .flex()
            .flex_col()
            .rounded(px(4.))
            .border_1()
            .border_color(t.border)
            .bg(t.field);
        for (index, label) in suggestions.iter().enumerate() {
            let value = label.clone();
            list = list.child(
                div()
                    .id(format!("network-editor-expertise-suggestion-{index}"))
                    .test_support()
                    .px(px(8.))
                    .py(px(4.))
                    .cursor_pointer()
                    .text(Text::Small)
                    .text_color(t.text_muted)
                    .hover(|style| style.bg(t.hover))
                    .child(label.clone())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.pick_network_expertise(value.clone(), window, cx)
                    })),
            );
        }
        expertise = expertise.child(list);
    }
    body = body.child(field_block(
        "network-editor-expertise",
        "Expertise",
        expertise.into_any_element(),
        t,
    ));

    // Zone dropdown: saved zones first, then every IANA zone.
    let zone_label = editor
        .zone
        .as_deref()
        .map(|zone| format!("{} · {zone}", city_name(zone)))
        .unwrap_or_else(|| "Choose a time zone".to_string());
    let mut zone_block = div().flex().flex_col().gap(px(4.)).child(
        div()
            .id("network-editor-zone-trigger")
            .test_support()
            .h(px(26.))
            .px(px(8.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .rounded(px(5.))
            .border_1()
            .border_color(t.border)
            .bg(t.field)
            .cursor_pointer()
            .text(Text::Small)
            .text_color(if editor.zone.is_some() {
                t.text
            } else {
                t.text_faint
            })
            .child(div().flex_1().min_w(px(0.)).truncate().child(zone_label))
            .child(icon(Icon::ChevronDown, 12., t.text_muted))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.toggle_network_editor_zone(window, cx)
            })),
    );
    if editor.zone_open {
        let mut list = div()
            .id("network-editor-zone-list")
            .test_support()
            .max_h(px(140.))
            .overflow_y_scroll()
            .rounded(px(4.))
            .border_1()
            .border_color(t.border)
            .bg(t.field)
            .py(px(2.))
            .child(field_input(&editor.zone_input, t));
        if zone_rows.is_empty() {
            list = list.child(
                div()
                    .p(px(8.))
                    .text(Text::Small)
                    .text_color(t.text_faint)
                    .child("No time zones match."),
            );
        }
        for (index, zone) in zone_rows.iter().enumerate() {
            let value = zone.clone();
            list = list.child(
                div()
                    .id(format!("network-editor-zone-option-{index}"))
                    .test_support()
                    .px(px(8.))
                    .py(px(5.))
                    .cursor_pointer()
                    .text(Text::Small)
                    .text_color(t.text_muted)
                    .hover(|style| style.bg(t.hover))
                    .child(format!("{} · {zone}", city_name(zone)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.choose_network_editor_zone(&value, window, cx)
                    })),
            );
        }
        zone_block = zone_block.child(list);
    }
    body = body.child(field_block(
        "network-editor-zone",
        "Time zone",
        zone_block.into_any_element(),
        t,
    ));

    // Status: active / dormant / archived.
    let mut status = div().flex().items_center().gap(px(4.));
    for option in PERSON_STATUSES {
        let active = editor.status == option;
        status = status.child(
            div()
                .id(format!("network-editor-status-{option}"))
                .test_support()
                .h(px(24.))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .when(active, |el| el.bg(t.surface_2))
                .hover(|style| style.bg(t.hover))
                .text(Text::Small)
                .text_color(if active { t.text } else { t.text_muted })
                .child(capitalize(option))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    if let Some(editor) = &mut this.network.editor {
                        editor.status = option.to_string();
                        cx.notify();
                    }
                })),
        );
    }
    body = body.child(field_block(
        "network-editor-status",
        "Status",
        status.into_any_element(),
        t,
    ));

    body = body.child(field_block(
        "network-editor-notes",
        "Notes",
        div()
            .child(field_textarea(&editor.notes, t).h(px(96.)))
            .into_any_element(),
        t,
    ));

    if let Some(error) = &editor.error {
        body = body.child(
            div()
                .px(px(10.))
                .py(px(6.))
                .rounded(px(t.radius))
                .border_1()
                .border_color(t.danger)
                .text(Text::Small)
                .text_color(t.danger)
                .child(error.clone()),
        );
    }

    let title = if editor.person_id.is_some() {
        "Edit person"
    } else {
        "Add person"
    };
    let panel = div()
        .id("network-editor")
        .test_support()
        .w(px(560.))
        .max_h(px(640.))
        .flex()
        .flex_col()
        .rounded(px(t.radius))
        .bg(t.panel)
        .border_1()
        .border_color(t.border)
        .track_focus(&editor.focus)
        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                if this
                    .network
                    .editor
                    .as_ref()
                    .is_some_and(|editor| editor.zone_open)
                {
                    this.toggle_network_editor_zone(window, cx);
                } else {
                    this.close_network_editor(cx);
                }
            }
        }))
        .child(
            div()
                .w_full()
                .px(px(16.))
                .py(px(12.))
                .flex()
                .items_center()
                .border_b_1()
                .border_color(t.border)
                .text(Text::Heading)
                .text_color(t.text)
                .child(title),
        )
        .child(body)
        .child(
            div()
                .id("network-editor-footer")
                .test_support()
                .w_full()
                .flex_shrink_0()
                .px(px(16.))
                .py(px(12.))
                .border_t_1()
                .border_color(t.border)
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1())
                .child(small_button(
                    "network-editor-cancel",
                    "Cancel",
                    false,
                    t,
                    cx.listener(|this, _: &ClickEvent, _window, cx| this.close_network_editor(cx)),
                ))
                .child(
                    div()
                        .id("network-editor-save")
                        .test_support()
                        .h(px(26.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(t.radius))
                        .bg(t.accent_soft)
                        .cursor_pointer()
                        .text(Text::Button)
                        .text_color(t.accent_text)
                        .child("Save")
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.save_network_editor(cx)
                        })),
                ),
        );

    Some(overlay(
        "network-editor-layer",
        panel.into_any_element(),
        div()
            .id("network-editor-backdrop")
            .test_support()
            .occlude()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000080))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| this.close_network_editor(cx)),
            )
            .into_any_element(),
    ))
}

fn field_block(
    id: &'static str,
    label: &'static str,
    field: impl IntoElement,
    t: &Tokens,
) -> AnyElement {
    div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .text(Text::Small)
                .text_color(t.text_faint)
                .child(label),
        )
        .child(div().id(id).test_support().w_full().child(field))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Placement picker
// ---------------------------------------------------------------------------

fn place_picker_layer(
    state: &NetworkState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    let picker = state.place_picker.as_ref()?;
    let query = picker.search.read(cx).value().to_lowercase();
    let rows: Vec<&NetworkCity> = state
        .cities
        .iter()
        .filter(|city| {
            query.is_empty()
                || city.city.to_lowercase().contains(&query)
                || city.zone.to_lowercase().contains(&query)
        })
        .collect();
    let zone_query = picker.zone_input.read(cx).value().to_string();
    let zone_rows = picker_rows(&state.zones, &zone_query);

    let mut list = div()
        .id("network-place-list")
        .test_support()
        .max_h(px(240.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .py(px(4.));
    if rows.is_empty() {
        list = list.child(
            div()
                .px(px(12.))
                .py(px(8.))
                .text(Text::Small)
                .text_color(t.text_faint)
                .child("No saved city matches. Add a new city below."),
        );
    }
    for city in rows {
        let clock_id = city.id.clone();
        list = list.child(
            div()
                .id(format!("network-place-city-{}", city.id))
                .test_support()
                .px(px(12.))
                .py(px(7.))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .hover(|style| style.bg(t.hover))
                .child(icon(Icon::Globe, 13., t.text_muted))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text(Text::Body)
                                .text_color(t.text)
                                .child(city.city.clone()),
                        )
                        .child(
                            div()
                                .text(Text::Small)
                                .text_color(t.text_faint)
                                .child(city.zone.clone()),
                        ),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.place_network_person(&clock_id, cx)
                })),
        );
    }

    let mut new_city = div()
        .id("network-place-new-city")
        .test_support()
        .px(px(12.))
        .py(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .border_t_1()
        .border_color(t.divider)
        .cursor_pointer()
        .hover(|style| style.bg(t.hover))
        .child(icon(Icon::Plus, 12., t.text_muted))
        .child(
            div()
                .text(Text::Small)
                .text_color(t.text_muted)
                .child("New city"),
        );
    if picker.new_city_open {
        let mut zones = div()
            .id("network-place-zones")
            .test_support()
            .max_h(px(180.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(field_input(&picker.zone_input, t));
        if zone_rows.is_empty() {
            zones = zones.child(
                div()
                    .p(px(6.))
                    .text(Text::Small)
                    .text_color(t.text_faint)
                    .child("No time zones match."),
            );
        }
        for (index, zone) in zone_rows.iter().enumerate() {
            let value = zone.clone();
            zones = zones.child(
                div()
                    .id(format!("network-place-zone-{index}"))
                    .test_support()
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text(Text::Small)
                    .text_color(t.text_muted)
                    .hover(|style| style.bg(t.hover))
                    .child(format!("{} · {zone}", city_name(zone)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.place_network_person_new_city(&value, cx)
                    })),
            );
        }
        new_city = new_city.child(zones);
    }
    new_city = new_city.on_click(
        cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_network_new_city(window, cx)),
    );

    let mut body = div()
        .px(px(14.))
        .py(px(10.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(field_input(&picker.search, t))
        .child(
            div()
                .text(Text::Body)
                .text_color(t.text_muted)
                .child(format!("Place {} on a city card.", picker.person_name)),
        )
        .child(list)
        .child(new_city);
    if let Some(error) = &picker.error {
        body = body.child(
            div()
                .text(Text::Small)
                .text_color(t.danger)
                .child(error.clone()),
        );
    }

    let panel = div()
        .id("network-place-picker")
        .test_support()
        .w(px(420.))
        .max_h(px(560.))
        .flex()
        .flex_col()
        .rounded(px(t.radius))
        .bg(t.panel)
        .border_1()
        .border_color(t.border)
        .track_focus(&picker.focus)
        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                this.close_network_place_picker(cx);
            }
        }))
        .child(
            div()
                .w_full()
                .px(px(16.))
                .py(px(12.))
                .border_b_1()
                .border_color(t.border)
                .text(Text::Heading)
                .text_color(t.text)
                .child("Place on a city card"),
        )
        .child(body)
        .child(
            div()
                .w_full()
                .flex_shrink_0()
                .px(px(16.))
                .py(px(10.))
                .border_t_1()
                .border_color(t.border)
                .flex()
                .items_center()
                .child(div().flex_1())
                .child(small_button(
                    "network-place-cancel",
                    "Cancel",
                    false,
                    t,
                    cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.close_network_place_picker(cx)
                    }),
                )),
        );

    Some(overlay(
        "network-place-layer",
        panel.into_any_element(),
        div()
            .id("network-place-backdrop")
            .test_support()
            .occlude()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000080))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                    this.close_network_place_picker(cx)
                }),
            )
            .into_any_element(),
    ))
}

// ---------------------------------------------------------------------------
// Delete confirmation
// ---------------------------------------------------------------------------

fn confirm_layer(
    state: &NetworkState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    let confirm = state.confirm.as_ref()?;
    let message = format!(
        "Delete {}? Their Clocks placement and trips are removed too. Other people and cities stay.",
        confirm.person_name
    );
    let panel = div()
        .id("network-confirm")
        .test_support()
        .w(px(420.))
        .flex()
        .flex_col()
        .rounded(px(t.radius))
        .bg(t.panel)
        .border_1()
        .border_color(t.border)
        .track_focus(&confirm.focus)
        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                this.cancel_network_delete(cx);
            }
        }))
        .child(
            div()
                .px(px(16.))
                .py(px(12.))
                .border_b_1()
                .border_color(t.border)
                .text(Text::Heading)
                .text_color(t.text)
                .child("Delete person"),
        )
        .child(
            div()
                .px(px(16.))
                .py(px(12.))
                .text(Text::Body)
                .text_color(t.text_muted)
                .child(message),
        )
        .when_some(confirm.error.clone(), |el, error| {
            el.child(
                div()
                    .px(px(16.))
                    .pb(px(8.))
                    .text(Text::Small)
                    .text_color(t.danger)
                    .child(error),
            )
        })
        .child(
            div()
                .px(px(16.))
                .py(px(12.))
                .border_t_1()
                .border_color(t.border)
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1())
                .child(small_button(
                    "network-confirm-cancel",
                    "Cancel",
                    false,
                    t,
                    cx.listener(|this, _: &ClickEvent, _window, cx| this.cancel_network_delete(cx)),
                ))
                .child(
                    div()
                        .id("network-confirm-delete")
                        .test_support()
                        .h(px(26.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(t.radius))
                        .bg(t.danger)
                        .cursor_pointer()
                        .text(Text::Button)
                        .text_color(t.text)
                        .child("Delete")
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.confirm_network_delete(cx)
                        })),
                ),
        );

    Some(overlay(
        "network-confirm-layer",
        panel.into_any_element(),
        div()
            .id("network-confirm-backdrop")
            .test_support()
            .occlude()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000080))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| this.cancel_network_delete(cx)),
            )
            .into_any_element(),
    ))
}

fn overlay(layer_id: &'static str, panel: AnyElement, backdrop: AnyElement) -> AnyElement {
    div()
        .id(layer_id)
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(backdrop)
        .child(div().occlude().child(panel))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Zones with saved cities first, then every IANA zone, filtered by `query`.
pub fn zone_rows(saved_first: &[String], query: &str) -> Vec<String> {
    let needle = query.trim().to_lowercase();
    let mut rows: Vec<String> = Vec::new();
    for zone in saved_first {
        if (needle.is_empty() || zone_matches_local(zone, &needle)) && !rows.contains(zone) {
            rows.push(zone.clone());
        }
    }
    for zone in available_zones() {
        if rows.contains(&zone) {
            continue;
        }
        if needle.is_empty() || zone_matches_local(&zone, &needle) {
            rows.push(zone);
        }
    }
    rows
}

fn zone_matches_local(zone: &str, needle: &str) -> bool {
    zone.to_lowercase().contains(needle) || city_name(zone).to_lowercase().contains(needle)
}

fn status_filter_label(value: &str) -> &'static str {
    match value {
        "active" => "Active",
        "dormant" => "Dormant",
        "archived" => "Archived",
        _ => "All",
    }
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn initial_of(name: &str) -> char {
    name.trim()
        .chars()
        .next()
        .map(|ch| ch.to_ascii_uppercase())
        .filter(|ch| ch.is_ascii_alphabetic())
        .unwrap_or('#')
}

/// `role · organization`, or whichever is present.
pub fn role_line(person: &Person) -> String {
    match (person.role_title.trim(), person.organization.trim()) {
        ("", "") => String::new(),
        ("", org) => org.to_string(),
        (role, "") => role.to_string(),
        (role, org) => format!("{role} · {org}"),
    }
}

/// A one-line tooltip, shared by the rail buttons.
struct NameTooltip {
    label: String,
}

impl Render for NameTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = Tokens::dark();
        div()
            .px(px(8.))
            .py(px(4.))
            .rounded(px(4.))
            .bg(t.panel)
            .border_1()
            .border_color(t.border)
            .text(Text::Small)
            .text_color(t.text_muted)
            .child(self.label.clone())
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_line_joins_the_present_parts() {
        let mut person = Person {
            id: "p".into(),
            name: "Ada".into(),
            role_title: String::new(),
            organization: String::new(),
            collaboration: String::new(),
            email: String::new(),
            url: String::new(),
            location: String::new(),
            tz_identifier: String::new(),
            last_contact_on: None,
            how_met: String::new(),
            notes: String::new(),
            status: "active".into(),
            in_clocks: false,
            created_at: String::new(),
            updated_at: String::new(),
            expertise: Vec::new(),
        };
        assert_eq!(role_line(&person), "");
        person.role_title = "Engineer".into();
        assert_eq!(role_line(&person), "Engineer");
        person.organization = "Acme".into();
        assert_eq!(role_line(&person), "Engineer · Acme");
        person.role_title.clear();
        assert_eq!(role_line(&person), "Acme");
    }

    #[test]
    fn initial_headers_group_by_first_letter() {
        assert_eq!(initial_of("Ada"), 'A');
        assert_eq!(initial_of("  grace"), 'G');
        assert_eq!(initial_of("42"), '#');
        assert_eq!(initial_of(""), '#');
    }
}
