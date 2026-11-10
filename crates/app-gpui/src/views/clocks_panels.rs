//! Clocks overlays: the IANA zone picker, the per-city people inspector, the
//! travel form and the shared confirmation dialog.
//!
//! Every panel renders from [`ClockState`] and sends actions back to
//! [`AppShell`], which owns the [`crate::store::Store`]. No SQL here.

use gpui_kit::component::input::Input;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, IntoElement, KeyDownEvent, TestSupportExt, Window, div,
    px, rgba,
};

use kandrium_core::{Travel, format_offset, resolve_zone};

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::clocks::{
    ClockEntry, ClockState, ConfirmKind, FormKind, PersonStatus, city_label, picker_rows,
    starts_label, until_label, zone_is_added,
};

/// Full-section scrim with a centred panel.
fn overlay(id: &'static str, t: &Tokens, panel: AnyElement) -> AnyElement {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x00000080))
        .child(panel)
        .into_any_element()
}

fn panel_shell(
    id: &'static str,
    width: f32,
    t: &Tokens,
) -> gpui_kit::base::ObservedElement<gpui_kit::Stateful<gpui_kit::Div>> {
    div()
        .id(id)
        .test_support()
        .w(px(width))
        .max_h(px(600.))
        .flex()
        .flex_col()
        .rounded(px(t.radius))
        .bg(t.panel)
        .border_1()
        .border_color(t.border)
}

fn panel_header(title: &str, t: &Tokens) -> AnyElement {
    div()
        .w_full()
        .px(px(16.))
        .py(px(12.))
        .flex()
        .items_center()
        .gap(px(8.))
        .border_b_1()
        .border_color(t.border)
        .child(
            div()
                .text(Text::Heading)
                .text_color(t.text)
                .child(title.to_string()),
        )
        .into_any_element()
}

fn error_banner(message: &str, t: &Tokens) -> AnyElement {
    div()
        .w_full()
        .px(px(12.))
        .py(px(8.))
        .rounded(px(t.radius))
        .border_1()
        .border_color(t.danger)
        .text(Text::Small)
        .text_color(t.danger)
        .child(message.to_string())
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Zone picker
// ---------------------------------------------------------------------------

pub fn picker(
    state: &ClockState,
    t: &Tokens,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    let picker = state.picker.as_ref()?;
    let query = picker.input.read(cx).value().to_string();
    let rows = picker_rows(&state.zones, &query);
    let selected = picker.selected.min(rows.len().saturating_sub(1));

    let mut list = div()
        .id("clock-picker-list")
        .flex_1()
        .min_h_0()
        .max_h(px(400.))
        .overflow_y_scroll()
        .py(px(6.))
        .flex()
        .flex_col();

    if rows.is_empty() {
        list = list.child(
            div()
                .p(px(18.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .text(Text::Body)
                        .text_color(t.text_muted)
                        .child(format!("No time zones match \u{201c}{query}\u{201d}.")),
                )
                .child(
                    div()
                        .id("clock-picker-clear")
                        .cursor_pointer()
                        .text(Text::SmallStrong)
                        .text_color(t.accent_text)
                        .child("Clear search")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.clear_clock_picker_search(window, cx)
                        })),
                ),
        );
    } else {
        for (index, zone) in rows.iter().enumerate() {
            let added = zone_is_added(&state.entries, zone);
            let is_selected = index == selected;
            let city = kandrium_core::city_name(zone);
            let offset = resolve_zone(zone)
                .map(|zone| format_offset(state.now.to_zoned(zone).offset().seconds()))
                .unwrap_or_else(|| "GMT?".to_string());
            let row = div()
                .id(format!("clock-zone-{zone}"))
                .test_support()
                .mx(px(8.))
                .px(px(10.))
                .py(px(7.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(4.))
                .when(is_selected && !added, |el| el.bg(t.selected))
                .when(!added, |el| el.cursor_pointer())
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text(Text::Body)
                                .text_color(if added { t.text_faint } else { t.text })
                                .child(city),
                        )
                        .child(
                            div()
                                .text(Text::Small)
                                .text_color(t.text_faint)
                                .child(zone.clone()),
                        ),
                )
                .when(added, |el| {
                    el.child(
                        div()
                            .px(px(6.))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .bg(t.field)
                            .text(Text::SmallStrong)
                            .text_color(t.text_faint)
                            .child("Added"),
                    )
                })
                .when(!added, |el| {
                    el.child(
                        div()
                            .text(Text::Small)
                            .text_color(t.text_muted)
                            .child(offset),
                    )
                });
            let row = if added {
                row
            } else {
                let zone = zone.clone();
                row.on_click(
                    cx.listener(move |this, _: &ClickEvent, _window, cx| this.pick_zone(&zone, cx)),
                )
            };
            list = list.child(row);
        }
    }

    let panel = panel_shell("clock-picker", 520., t)
        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            this.handle_picker_key(event, window, cx)
        }))
        .child(panel_header("Add a city", t))
        .child(
            div()
                .w_full()
                .px(px(16.))
                .pt(px(12.))
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1().child(Input::new(&picker.input)))
                .when(!query.is_empty(), |el| {
                    el.child(
                        div()
                            .id("clock-picker-clear-inline")
                            .cursor_pointer()
                            .text(Text::Small)
                            .text_color(t.text_muted)
                            .child("Clear")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.clear_clock_picker_search(window, cx)
                            })),
                    )
                }),
        )
        .when_some(picker.error.clone(), |el, message| {
            el.child(
                div()
                    .px(px(16.))
                    .pb(px(8.))
                    .child(error_banner(&message, t)),
            )
        })
        .child(list)
        .child(
            div()
                .w_full()
                .px(px(16.))
                .py(px(10.))
                .flex()
                .items_center()
                .justify_end()
                .gap(px(8.))
                .border_t_1()
                .border_color(t.border)
                .child(
                    div()
                        .id("clock-picker-cancel")
                        .test_support()
                        .px(px(10.))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .rounded(px(t.radius))
                        .cursor_pointer()
                        .text(Text::Button)
                        .text_color(t.text_muted)
                        .child("Cancel")
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.close_clock_picker(cx)
                        })),
                ),
        );

    Some(overlay("clock-picker-overlay", t, panel.into_any_element()))
}

// ---------------------------------------------------------------------------
// People inspector
// ---------------------------------------------------------------------------

pub fn inspector(
    state: &ClockState,
    t: &Tokens,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    let inspector = state.inspector.as_ref()?;
    let entry = state.entry(&inspector.clock_id)?;
    let people = state.city_people(entry.id.as_str());
    let zone = entry.zone.as_ref();

    let mut residents = div().flex().flex_col().gap(px(2.));
    if people.residents.is_empty() {
        residents = residents.child(
            div()
                .p(px(10.))
                .text(Text::Small)
                .text_color(t.text_faint)
                .child("No residents yet."),
        );
    }
    for status in &people.residents {
        residents = residents.child(resident_row(state, entry, status, t, cx));
    }

    let mut visitors = div().flex().flex_col().gap(px(2.));
    for status in &people.visitors {
        visitors = visitors.child(visitor_row(state, status, t, cx));
    }

    let body = div()
        .id("clock-inspector-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(16.))
        .py(px(12.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(
            div()
                .text(Text::Small)
                .text_color(t.text_faint)
                .child(format!(
                    "{} \u{00b7} {} \u{00b7} {} residents",
                    entry.city,
                    entry.tz_identifier,
                    people.residents.len()
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(section_label("Residents", t))
                .child(residents),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1().child(Input::new(&inspector.add)))
                .child(
                    div()
                        .id("clock-inspector-add")
                        .test_support()
                        .h(px(28.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .rounded(px(t.radius))
                        .bg(t.accent_soft)
                        .cursor_pointer()
                        .text(Text::Button)
                        .text_color(t.accent_text)
                        .child(icon(Icon::Plus, 13., t.accent_text))
                        .child("Add")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.add_person_from_inspector(window, cx)
                        })),
                ),
        )
        .when(!people.visitors.is_empty(), |el| {
            el.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(section_label("Visiting", t))
                    .child(visitors),
            )
        })
        .when_some(inspector.error.clone(), |el, message| {
            el.child(error_banner(&message, t))
        });

    let panel = panel_shell("clock-inspector", 560., t)
        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            this.handle_inspector_key(event, window, cx)
        }))
        .child(
            div()
                .w_full()
                .px(px(16.))
                .py(px(12.))
                .flex()
                .items_center()
                .gap(px(8.))
                .border_b_1()
                .border_color(t.border)
                .child(
                    div()
                        .text(Text::Heading)
                        .text_color(t.text)
                        .child(entry.city.clone()),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .id("clock-inspector-close")
                        .test_support()
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|style| style.bg(t.hover))
                        .child(icon(Icon::X, 14., t.text_muted))
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.close_inspector(cx)
                        })),
                ),
        )
        .child(body)
        .child(
            div()
                .px(px(16.))
                .py(px(8.))
                .border_t_1()
                .border_color(t.border)
                .text(Text::Small)
                .text_color(t.text_faint)
                .child(match zone {
                    Some(_) => format!("All times in {}.", entry.city),
                    None => "Unknown time zone.".to_string(),
                }),
        );

    Some(overlay(
        "clock-inspector-overlay",
        t,
        panel.into_any_element(),
    ))
}

fn section_label(label: &str, t: &Tokens) -> AnyElement {
    div()
        .text(Text::SmallStrong)
        .text_color(t.text_faint)
        .child(label.to_string())
        .into_any_element()
}

fn resident_row(
    state: &ClockState,
    entry: &ClockEntry,
    status: &PersonStatus,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let person_id = status.person.id.clone();
    let name = status.person.name.clone();
    let renaming = state
        .inspector
        .as_ref()
        .and_then(|inspector| inspector.rename.as_ref())
        .filter(|(id, _)| *id == person_id)
        .map(|(_, input)| input.clone());

    let mut row = div()
        .id(format!("clock-resident-{person_id}"))
        .test_support()
        .px(px(8.))
        .py(px(7.))
        .flex()
        .flex_col()
        .gap(px(4.))
        .rounded(px(4.))
        .hover(|style| style.bg(t.hover))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .size(px(24.))
                        .rounded(px(12.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(t.field)
                        .border_1()
                        .border_color(if status.away_to.is_some() {
                            t.accent
                        } else {
                            t.divider
                        })
                        .text(Text::SmallStrong)
                        .text_color(if status.away_to.is_some() {
                            t.accent_text
                        } else {
                            t.text_muted
                        })
                        .child(crate::views::clocks::initials(&name)),
                )
                .child(if let Some(input) = renaming {
                    div().flex_1().child(Input::new(&input)).into_any_element()
                } else {
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text(Text::Body)
                                .text_color(t.text)
                                .child(name.clone()),
                        )
                        .child(
                            div()
                                .text(Text::Small)
                                .text_color(t.text_muted)
                                .child(status_line(state, status)),
                        )
                        .into_any_element()
                })
                .child(action_icon(
                    format!("clock-person-rename-{person_id}"),
                    Icon::Pencil,
                    t,
                    {
                        let person_id = person_id.clone();
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.begin_person_rename(&person_id, window, cx)
                        })
                    },
                ))
                .child(action_icon(
                    format!("clock-person-travel-{person_id}"),
                    Icon::Plane,
                    t,
                    {
                        let person_id = person_id.clone();
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_travel_form(&person_id, window, cx)
                        })
                    },
                ))
                .child(action_icon(
                    format!("clock-person-relocate-{person_id}"),
                    Icon::House,
                    t,
                    {
                        let person_id = person_id.clone();
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_relocate_form(&person_id, window, cx)
                        })
                    },
                ))
                .child(action_icon(
                    format!("clock-person-remove-{person_id}"),
                    Icon::Trash2,
                    t,
                    {
                        let person_id = person_id.clone();
                        cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.ask_remove_person(&person_id, cx)
                        })
                    },
                )),
        );

    if let Some(travel) = &status.travel {
        row = row.child(trip_row(state, entry, travel, t, cx));
    } else if let Some(travel) = &status.upcoming {
        row = row.child(trip_row(state, entry, travel, t, cx));
    }
    row.into_any_element()
}

fn trip_row(
    state: &ClockState,
    entry: &ClockEntry,
    travel: &Travel,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let destination = travel
        .destination_clock_id
        .as_deref()
        .map(|id| city_label(&state.entries, id))
        .unwrap_or_else(|| "unknown".to_string());
    let zone = travel
        .destination_clock_id
        .as_deref()
        .and_then(|id| state.entry(id))
        .and_then(|entry| entry.zone.as_ref());
    let label = if travel.starts_at > state.now.as_second() {
        format!(
            "{} \u{00b7} {}",
            destination,
            starts_label(zone, travel.starts_at)
        )
    } else {
        format!("{} {}", destination, until_label(zone, travel.ends_at))
    };
    let running = travel.starts_at <= state.now.as_second();
    let travel_id = travel.id.clone();

    div()
        .id(format!("clock-trip-{}", travel.id))
        .ml(px(32.))
        .flex()
        .items_center()
        .gap(px(6.))
        .text(Text::Small)
        .text_color(t.text_muted)
        .child(
            div()
                .size(px(6.))
                .rounded_full()
                .when(running, |el| el.bg(t.accent))
                .when(!running, |el| el.border_1().border_color(t.accent)),
        )
        .child(div().flex_1().child(label))
        .when(running, |el| {
            el.child(text_action(
                format!("clock-trip-end-{}", travel.id),
                "End now",
                t,
                {
                    let travel_id = travel_id.clone();
                    cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.end_travel_now(&travel_id, cx)
                    })
                },
            ))
        })
        .child(text_action(
            format!("clock-trip-extend-{}", travel.id),
            "Extend",
            t,
            {
                let travel_id = travel_id.clone();
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_extend_form(&travel_id, window, cx)
                })
            },
        ))
        .child(action_icon(
            format!("clock-trip-remove-{}", travel.id),
            Icon::X,
            t,
            {
                let travel_id = travel_id.clone();
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.ask_remove_travel(&travel_id, cx)
                })
            },
        ))
        .into_any_element()
}

fn visitor_row(
    state: &ClockState,
    status: &PersonStatus,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let home = state
        .entries
        .iter()
        .find(|entry| entry.id == status.person.home_clock_id)
        .map(|entry| entry.city.clone())
        .unwrap_or_else(|| "elsewhere".to_string());
    let label = format!(
        "from {} \u{00b7} {}",
        home,
        until_label(
            state
                .entry(&status.person.home_clock_id)
                .and_then(|entry| entry.zone.as_ref()),
            status.until
        )
    );
    let travel_id = status.travel.as_ref().map(|travel| travel.id.clone());
    div()
        .id(format!("clock-visitor-{}", status.person.id))
        .px(px(8.))
        .py(px(6.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(4.))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text(Text::Body)
                        .text_color(t.text)
                        .child(status.person.name.clone()),
                )
                .child(
                    div()
                        .text(Text::Small)
                        .text_color(t.text_muted)
                        .child(label),
                ),
        )
        .when_some(travel_id, |el, travel_id| {
            el.child(text_action(
                format!("clock-visitor-home-{}", status.person.id),
                "Send home",
                t,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.send_home(&travel_id, cx)
                }),
            ))
        })
        .into_any_element()
}

fn status_line(state: &ClockState, status: &PersonStatus) -> String {
    if let Some(destination) = &status.away_to {
        let zone = status
            .travel
            .as_ref()
            .and_then(|travel| travel.destination_clock_id.as_deref())
            .and_then(|id| state.entry(id))
            .and_then(|entry| entry.zone.as_ref());
        return format!(
            "Away in {destination} \u{00b7} {}",
            until_label(zone, status.until)
        );
    }
    if let Some(travel) = &status.upcoming {
        let destination = travel
            .destination_clock_id
            .as_deref()
            .map(|id| city_label(&state.entries, id))
            .unwrap_or_else(|| "unknown".to_string());
        let zone = travel
            .destination_clock_id
            .as_deref()
            .and_then(|id| state.entry(id))
            .and_then(|entry| entry.zone.as_ref());
        return format!(
            "Leaves for {destination} \u{00b7} {}",
            starts_label(zone, travel.starts_at)
        );
    }
    "Here now".to_string()
}

fn action_icon(
    id: String,
    glyph: Icon,
    t: &Tokens,
    onclick: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .cursor_pointer()
        .hover(|style| style.bg(t.hover))
        .child(icon(glyph, 13., t.text_muted))
        .on_click(onclick)
        .into_any_element()
}

fn text_action(
    id: String,
    label: &'static str,
    t: &Tokens,
    onclick: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .px(px(6.))
        .h(px(20.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .cursor_pointer()
        .text(Text::Small)
        .text_color(t.accent_text)
        .hover(|style| style.bg(t.hover))
        .child(label)
        .on_click(onclick)
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Travel form
// ---------------------------------------------------------------------------

pub fn form(
    state: &ClockState,
    t: &Tokens,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    let form = state.form.as_ref()?;
    let title = match form.kind {
        FormKind::Assign => format!("Travel \u{00b7} {}", form.person_name),
        FormKind::Extend => format!("Extend trip \u{00b7} {}", form.person_name),
        FormKind::Relocate => format!("Move home \u{00b7} {}", form.person_name),
    };

    let mut body = div()
        .id("clock-form-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(16.))
        .py(px(12.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(
            div()
                .text(Text::Small)
                .text_color(t.text_faint)
                .child(match form.kind {
                    FormKind::Assign => format!(
                        "Home: {}. Pick the destination and the days there.",
                        form.home_city
                    ),
                    FormKind::Extend => format!(
                        "Current city: {}.",
                        form.current_city
                            .clone()
                            .unwrap_or_else(|| form.home_city.clone())
                    ),
                    FormKind::Relocate => format!(
                        "Home: {}. Relocation ends trips to the old and new home.",
                        form.home_city
                    ),
                }),
        );

    if form.kind != FormKind::Extend {
        let mut destinations = div().flex().flex_col().gap(px(2.));
        for entry in &state.entries {
            let is_home = entry.id == form.home_clock_id;
            let disabled = matches!(form.kind, FormKind::Relocate) && is_home;
            let selected = form.destination.as_deref() == Some(entry.id.as_str());
            let zone_id = entry.id.clone();
            let mut row = div()
                .id(format!("clock-form-dest-{}", entry.id))
                .test_support()
                .px(px(10.))
                .py(px(7.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(4.))
                .when(selected, |el| el.bg(t.selected))
                .when(!disabled, |el| el.cursor_pointer())
                .child(
                    div()
                        .flex_1()
                        .text(Text::Body)
                        .text_color(if disabled { t.text_faint } else { t.text })
                        .child(entry.city.clone()),
                )
                .child(
                    div()
                        .text(Text::Small)
                        .text_color(t.text_faint)
                        .child(entry.tz_identifier.clone()),
                );
            if !disabled {
                row = row.on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.set_form_destination(&zone_id, cx)
                }));
            }
            destinations = destinations.child(row);
        }
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(section_label("Destination", t))
                .child(destinations),
        );
    }

    if form.kind != FormKind::Relocate {
        body = body
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(section_label("Starts", t))
                    .child(Input::new(&form.start))
                    .child(
                        div()
                            .text(Text::Small)
                            .text_color(t.text_faint)
                            .child("First day in the destination, from 00:00 local time."),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(section_label("Ends", t))
                    .when(form.open_ended, |el| {
                        el.child(
                            div()
                                .px(px(10.))
                                .py(px(6.))
                                .rounded(px(t.radius))
                                .bg(t.field)
                                .text(Text::Small)
                                .text_color(t.text_faint)
                                .child("Open-ended"),
                        )
                    })
                    .when(!form.open_ended, |el| el.child(Input::new(&form.end)))
                    .child(
                        div()
                            .id("clock-form-open-ended")
                            .test_support()
                            .mt(px(2.))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .cursor_pointer()
                            .child(
                                div()
                                    .size(px(14.))
                                    .rounded(px(3.))
                                    .border_1()
                                    .border_color(if form.open_ended { t.accent } else { t.border })
                                    .when(form.open_ended, |el| el.bg(t.accent_soft))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(form.open_ended, |el| {
                                        el.child(icon(Icon::Check, 10., t.accent_text))
                                    }),
                            )
                            .child(
                                div()
                                    .text(Text::Small)
                                    .text_color(t.text_muted)
                                    .child("Open-ended (no return day)"),
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.toggle_form_open_ended(cx)
                            })),
                    ),
            );
    }

    if let Some(message) = &form.error {
        body = body.child(error_banner(message, t));
    }

    let panel = panel_shell("clock-form", 540., t)
        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            this.handle_form_key(event, window, cx)
        }))
        .child(panel_header(&title, t))
        .child(body)
        .child(
            div()
                .px(px(16.))
                .py(px(10.))
                .flex()
                .items_center()
                .justify_end()
                .gap(px(8.))
                .border_t_1()
                .border_color(t.border)
                .child(
                    div()
                        .id("clock-form-cancel")
                        .test_support()
                        .px(px(10.))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .rounded(px(t.radius))
                        .cursor_pointer()
                        .text(Text::Button)
                        .text_color(t.text_muted)
                        .child("Cancel")
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.close_clock_form(cx)
                        })),
                )
                .child(
                    div()
                        .id("clock-form-save")
                        .test_support()
                        .px(px(12.))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .rounded(px(t.radius))
                        .bg(t.accent_soft)
                        .cursor_pointer()
                        .text(Text::Button)
                        .text_color(t.accent_text)
                        .child("Save")
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.submit_clock_form(cx)
                        })),
                ),
        );

    Some(overlay("clock-form-overlay", t, panel.into_any_element()))
}

// ---------------------------------------------------------------------------
// Confirmation
// ---------------------------------------------------------------------------

pub fn confirm(state: &ClockState, t: &Tokens, cx: &mut Context<AppShell>) -> Option<AnyElement> {
    let confirm = state.confirm.as_ref()?;
    let title = confirm.title.clone();
    let message = confirm.message.clone();
    let emphasis = match confirm.kind {
        ConfirmKind::RemoveClock { .. } => "Remove city",
        ConfirmKind::RemovePerson { .. } => "Remove person",
        ConfirmKind::RemoveTravel { .. } => "Remove trip",
    };

    let panel = div()
        .id("clock-confirm")
        .test_support()
        .w(px(420.))
        .flex()
        .flex_col()
        .gap(px(10.))
        .rounded(px(t.radius))
        .bg(t.panel)
        .border_1()
        .border_color(t.border)
        .p(px(16.))
        .child(div().text(Text::Heading).text_color(t.text).child(title))
        .child(
            div()
                .text(Text::Body)
                .text_color(t.text_muted)
                .child(message),
        )
        .when_some(confirm.error.clone(), |el, error| {
            el.child(error_banner(&error, t))
        })
        .child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(8.))
                .child(
                    div()
                        .id("clock-confirm-cancel")
                        .test_support()
                        .px(px(10.))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .rounded(px(t.radius))
                        .cursor_pointer()
                        .text(Text::Button)
                        .text_color(t.text_muted)
                        .child("Cancel")
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.cancel_clock_confirm(cx)
                        })),
                )
                .child(
                    div()
                        .id("clock-confirm-ok")
                        .test_support()
                        .px(px(12.))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .rounded(px(t.radius))
                        .bg(t.danger)
                        .cursor_pointer()
                        .text(Text::Button)
                        .text_color(t.accent_text)
                        .child(emphasis)
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.confirm_clock_action(cx)
                        })),
                ),
        );

    Some(overlay(
        "clock-confirm-overlay",
        t,
        panel.into_any_element(),
    ))
}
