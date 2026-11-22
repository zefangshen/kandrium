//! Clocks overlays: the IANA zone picker, the per-city people inspector, the
//! travel form and the shared confirmation dialog.
//!
//! Every panel renders from [`ClockState`] and sends actions back to
//! [`AppShell`], which owns the [`crate::store::Store`]. No SQL here.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, Context, Entity, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, Pixels, TestSupportExt, Window, canvas, div, px, rgba,
};

use kandrium_core::{Travel, format_offset, resolve_zone};

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::clocks::{
    ClockState, ConfirmKind, FormKind, PersonStatus, PickerTarget, city_label, picker_rows,
    saved_first_rows, starts_label, until_label, zone_is_added,
};

/// Full-section scrim with a centred panel.
fn overlay(id: &'static str, panel: AnyElement) -> AnyElement {
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
    let target = picker.target;
    // The probe picker must reach saved zones too, so it never disables a row.
    let probe_target = target == PickerTarget::ProbeZone;
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
            let added = !probe_target && zone_is_added(&state.entries, zone);
            let is_selected = index == selected;
            let city = kandrium_core::city_name(zone);
            let offset = resolve_zone(zone)
                .map(|zone| format_offset(state.instant().to_zoned(zone).offset().seconds()))
                .unwrap_or_else(|| "GMT?".to_string());
            let current = probe_target && state.probe_zone.as_deref() == Some(zone.as_str());
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
                .when(current, |el| {
                    el.child(
                        div()
                            .px(px(6.))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .bg(t.field)
                            .text(Text::SmallStrong)
                            .text_color(t.accent_text)
                            .child("Selected"),
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
                match target {
                    PickerTarget::AddCity => {
                        row.on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.pick_zone(&zone, cx)
                        }))
                    }
                    PickerTarget::ProbeZone => {
                        row.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.pick_probe_zone(&zone, window, cx)
                        }))
                    }
                }
            };
            list = list.child(row);
        }
    }

    let panel = panel_shell("clock-picker", 520., t)
        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            this.handle_picker_key(event, window, cx)
        }))
        .child(panel_header(
            if probe_target {
                "Probe time zone"
            } else {
                "Add a city"
            },
            t,
        ))
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

    // Same shape as the app's other modal layers: a press-catching backdrop
    // sits behind a sibling panel layer. Both occlude the pointer so the cards
    // underneath stop being hovered too — a plain scrim only blocks clicks,
    // while occlude() also clears their hover styles and tooltips.
    let layer = div()
        .id("clock-picker-layer")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .id("clock-picker-backdrop")
                .occlude()
                .test_support()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000080))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                        this.close_clock_picker(cx)
                    }),
                ),
        )
        .child(div().occlude().child(panel));

    Some(layer.into_any_element())
}

// ---------------------------------------------------------------------------
// Probe zone menu
// ---------------------------------------------------------------------------

/// Zone menu for the toolbar probe: the local zone first, then every saved
/// city. Picking a row re-reads the typed wall clock in that zone.
pub fn probe_zone_menu(
    state: &ClockState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    if !state.probe_menu {
        return None;
    }

    let mut list = div()
        .id("clock-probe-zone-list")
        .w_full()
        .max_h(px(360.))
        .overflow_y_scroll()
        .py(px(6.))
        .flex()
        .flex_col();

    let local_detail = state
        .local_zone
        .iana_name()
        .unwrap_or("system time zone")
        .to_string();
    let local_offset = format_offset(
        state
            .instant()
            .to_zoned(state.local_zone.clone())
            .offset()
            .seconds(),
    );
    list = list.child(probe_zone_row(
        "clock-probe-zone-local".to_string(),
        "Local time zone",
        &format!(
            "{} \u{00b7} {local_detail} \u{00b7} {local_offset}",
            state.probe_zone_label()
        ),
        state.probe_zone.is_none(),
        None,
        t,
        cx,
    ));

    for (index, entry) in state.entries.iter().enumerate() {
        let Some(zone) = entry.zone.clone() else {
            continue;
        };
        let offset = format_offset(state.instant().to_zoned(zone).offset().seconds());
        let selected = state.probe_zone.as_deref() == Some(entry.tz_identifier.as_str());
        list = list.child(probe_zone_row(
            format!("clock-probe-zone-{index}"),
            &entry.city,
            &format!("{} \u{00b7} {offset}", entry.tz_identifier),
            selected,
            Some(entry.tz_identifier.clone()),
            t,
            cx,
        ));
    }

    let mut panel = panel_shell("clock-probe-zone-menu", 360., t);
    if let Some(focus) = state.probe_menu_focus.clone() {
        panel = panel.track_focus(&focus);
    }
    let panel = panel
        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                this.close_clock_probe_menu(cx);
            }
        }))
        .child(panel_header("Probe time zone", t))
        .child(list)
        .child(
            div()
                .id("clock-probe-zone-search")
                .test_support()
                .mx(px(8.))
                .px(px(10.))
                .py(px(7.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|style| style.bg(t.hover))
                .child(icon(Icon::Search, 11., t.text_muted))
                .child(
                    div()
                        .text(Text::Body)
                        .text_color(t.text_muted)
                        .child("Search all zones\u{2026}"),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.open_probe_zone_picker(window, cx)
                })),
        )
        .child(
            div()
                .w_full()
                .px(px(16.))
                .py(px(10.))
                .flex()
                .items_center()
                .justify_end()
                .border_t_1()
                .border_color(t.border)
                .child(
                    div()
                        .id("clock-probe-zone-cancel")
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
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.toggle_clock_probe_menu(window, cx)
                        })),
                ),
        );

    // Same modal layer as the picker and inspector: a press-catching backdrop
    // behind a sibling panel layer, both occluding the pointer so the toolbar
    // and cards under them stop hovering as well as stop taking clicks.
    let layer = div()
        .id("clock-probe-zone-layer")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .id("clock-probe-zone-backdrop")
                .occlude()
                .test_support()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000080))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                        this.close_clock_probe_menu(cx)
                    }),
                ),
        )
        .child(div().occlude().child(panel));

    Some(layer.into_any_element())
}

fn probe_zone_row(
    id: String,
    city: &str,
    detail: &str,
    selected: bool,
    zone: Option<String>,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .mx(px(8.))
        .px(px(10.))
        .py(px(7.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(4.))
        .cursor_pointer()
        .when(selected, |el| el.bg(t.selected))
        .hover(|style| style.bg(t.hover))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text(Text::Body)
                        .text_color(t.text)
                        .child(city.to_string()),
                )
                .child(
                    div()
                        .text(Text::Small)
                        .text_color(t.text_faint)
                        .child(detail.to_string()),
                ),
        )
        .when(selected, |el| {
            el.child(
                div()
                    .px(px(6.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .bg(t.field)
                    .text(Text::SmallStrong)
                    .text_color(t.accent_text)
                    .child("Selected"),
            )
        })
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.set_clock_probe_zone(zone.as_deref(), window, cx)
        }))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Add-colleague form (Colleagues tab)
// ---------------------------------------------------------------------------

/// Add a colleague from the Colleagues tab: three name fields plus a searchable city.
/// The city may be one that has no clock card yet; submitting creates that
/// clock first, so the person always lands on a real city.
pub fn person_form(
    state: &ClockState,
    t: &Tokens,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    let form = state.person_form.as_ref()?;
    let query = form.city.read(cx).value().to_string();
    // Saved zones lead the list, so the cities already on the page are the
    // first thing offered; search still filters both groups.
    let rows = saved_first_rows(&state.entries, &state.zones, &query);

    let mut list = div()
        .id("person-form-city-list")
        .test_support()
        .h(px(180.))
        .overflow_y_scroll()
        .flex()
        .flex_col();
    if rows.is_empty() {
        list = list.child(
            div()
                .p(px(12.))
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(format!("No time zones match \u{201c}{query}\u{201d}.")),
        );
    } else {
        for (index, zone) in rows.iter().enumerate() {
            let saved = zone_is_added(&state.entries, zone);
            let chosen = form.zone.as_deref() == Some(zone.as_str());
            let row = div()
                .id(format!("person-form-zone-{index}"))
                .test_support()
                .mx(px(6.))
                .px(px(10.))
                .py(px(6.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(4.))
                .cursor_pointer()
                .when(chosen, |el| el.bg(t.selected))
                .hover(|style| style.bg(t.hover))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text(Text::Body)
                                .text_color(t.text)
                                .child(kandrium_core::city_name(zone)),
                        )
                        .child(
                            div()
                                .text(Text::Small)
                                .text_color(t.text_faint)
                                .child(zone.clone()),
                        ),
                )
                .when(saved, |el| {
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
                            .child("Saved"),
                    )
                });
            let zone = zone.clone();
            list = list.child(row.on_click(cx.listener(
                move |this, _: &ClickEvent, _window, cx| this.choose_person_zone(&zone, cx),
            )));
        }
    }

    let panel = panel_shell("person-form", 520., t)
        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                this.close_person_form(cx);
            }
        }))
        .child(panel_header("Add a colleague", t))
        .child(
            div()
                .w_full()
                .px(px(16.))
                .pt(px(12.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(name_field("person-form-first", &form.first))
                        .child(name_field("person-form-middle", &form.middle))
                        .child(name_field("person-form-last", &form.last)),
                )
                .child(section_label("City", t))
                .child(
                    div()
                        .id("person-form-city")
                        .test_support()
                        .child(Input::new(&form.city)),
                )
                .when_some(form.zone.clone(), |el, zone| {
                    el.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text(Text::Small)
                            .text_color(t.text_muted)
                            .child("Home city:")
                            .child(
                                div()
                                    .text(Text::SmallStrong)
                                    .text_color(t.accent_text)
                                    .child(kandrium_core::city_name(&zone)),
                            ),
                    )
                }),
        )
        // The zone list is the only scrolling region and has a fixed height, so
        // the footer below it is a plain sibling: it can never be pushed past
        // the panel's max height and lose its hitbox.
        .child(div().w_full().px(px(16.)).child(list))
        .when_some(form.error.clone(), |el, message| {
            el.child(
                div()
                    .px(px(16.))
                    .pt(px(8.))
                    .child(error_banner(&message, t)),
            )
        })
        .child(
            div()
                .id("person-form-footer")
                .test_support()
                .w_full()
                .flex_shrink_0()
                .mt(px(10.))
                .px(px(16.))
                .py(px(12.))
                .border_t_1()
                .border_color(t.divider)
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1())
                .child(
                    div()
                        .id("person-form-cancel")
                        .test_support()
                        .h(px(28.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(t.radius))
                        .cursor_pointer()
                        .text(Text::Button)
                        .text_color(t.text_muted)
                        .hover(|style| style.bg(t.hover))
                        .child("Cancel")
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.close_person_form(cx)
                        })),
                )
                .child(
                    div()
                        .id("person-form-submit")
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
                        .child("Add colleague")
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.add_person_from_form(cx)
                        })),
                ),
        );

    let layer = div()
        .id("person-form-layer")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .id("person-form-backdrop")
                .occlude()
                .test_support()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000080))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _window, cx| this.close_person_form(cx)),
                ),
        )
        .child(div().occlude().child(panel));

    Some(layer.into_any_element())
}

// ---------------------------------------------------------------------------
// Edit-colleague form (Colleagues tab)
// ---------------------------------------------------------------------------

/// One location dropdown trigger: the current pick plus a canvas that
/// publishes its window bounds. The list is a root-level floating layer
/// (`person_edit_menu`), so opening it never changes the modal's height and
/// the modal's scroll body cannot clip it.
struct LocationDropdown<'a> {
    prefix: &'static str,
    label: &'static str,
    placeholder: &'static str,
    selected: Option<&'a str>,
    anchor: &'a Rc<RefCell<Option<Bounds<Pixels>>>>,
    helper: &'static str,
}

fn location_dropdown(
    dd: LocationDropdown<'_>,
    state: &ClockState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
    toggle: fn(&mut AppShell, &mut Window, &mut Context<AppShell>),
) -> AnyElement {
    let label = dd
        .selected
        .map(|zone| {
            let city = state
                .entries
                .iter()
                .find(|entry| entry.tz_identifier == zone)
                .map(|entry| entry.city.clone())
                .unwrap_or_else(|| kandrium_core::city_name(zone));
            format!("{city} \u{00b7} {zone}")
        })
        .unwrap_or_else(|| dd.placeholder.to_string());
    let has_selection = dd.selected.is_some();
    let anchor = dd.anchor.clone();

    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(section_label(dd.label, t))
        .child(
            div()
                .id(format!("{}-trigger", dd.prefix))
                .test_support()
                .relative()
                .h(px(28.))
                .px(px(8.))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .rounded(px(t.radius))
                .border_1()
                .border_color(t.border)
                .bg(t.field)
                .cursor_pointer()
                .text(Text::Body)
                .text_color(if has_selection { t.text } else { t.text_faint })
                .child(div().flex_1().min_w(px(0.)).truncate().child(label))
                .child(icon(Icon::ChevronDown, 12., t.text_muted))
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, window, cx| toggle(this, window, cx)),
                )
                .child(
                    canvas(
                        move |bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut App| {
                            *anchor.borrow_mut() = Some(bounds);
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                ),
        )
        .child(
            div()
                .text(Text::Small)
                .text_color(t.text_faint)
                .child(dd.helper),
        )
        .into_any_element()
}

/// The floating location list for the edit modal, rendered at the app root so
/// it paints above the modal and outside its scroll region. Four catcher
/// strips around the trigger + list union dismiss on an outside press (the
/// trigger stays clickable so it toggles), and Esc closes just the list.
pub fn person_edit_menu(
    state: &ClockState,
    window: &Window,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    let edit = state.person_edit.as_ref()?;
    let choose: fn(&mut AppShell, &str, &mut Window, &mut Context<AppShell>) = if edit.home_open {
        AppShell::choose_person_edit_zone
    } else if edit.travel_open {
        AppShell::choose_person_edit_travel
    } else {
        return None;
    };
    let (prefix, anchor, search, selected) = if edit.home_open {
        (
            "person-edit-home",
            (*edit.home_anchor.borrow())?,
            &edit.city,
            edit.zone.as_deref(),
        )
    } else {
        (
            "person-edit-travel",
            (*edit.travel_anchor.borrow())?,
            &edit.travel_search,
            edit.travel.as_deref(),
        )
    };
    let viewport = window.viewport_size();
    let width = anchor.size.width;
    // Search field + gap + 120 px list + padding and border.
    let height = px(176.);
    let x = anchor
        .origin
        .x
        .min(viewport.width - width - px(8.))
        .max(px(8.));
    let below = anchor.origin.y + anchor.size.height;
    let y = if below + height > viewport.height - px(8.) {
        (anchor.origin.y - height - px(4.)).max(px(8.))
    } else {
        below
    };

    let query = search.read(cx).value().to_string();
    let rows = saved_first_rows(&state.entries, &state.zones, &query);
    let mut list = div()
        .id(format!("{prefix}-list"))
        .test_support()
        .h(px(120.))
        .overflow_y_scroll()
        .flex()
        .flex_col();
    if rows.is_empty() {
        list = list.child(
            div()
                .p(px(10.))
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(format!("No time zones match \u{201c}{query}\u{201d}.")),
        );
    } else {
        for (index, zone) in rows.iter().enumerate() {
            let saved = zone_is_added(&state.entries, zone);
            let chosen = selected == Some(zone.as_str());
            let row = div()
                .id(format!("{prefix}-option-{index}"))
                .test_support()
                .px(px(8.))
                .py(px(6.))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .when(chosen, |el| el.bg(t.selected))
                .hover(|style| style.bg(t.hover))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text(Text::Body)
                                .text_color(t.text)
                                .child(kandrium_core::city_name(zone)),
                        )
                        .child(
                            div()
                                .text(Text::Small)
                                .text_color(t.text_faint)
                                .child(zone.clone()),
                        ),
                )
                .when(saved, |el| {
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
                            .child("Saved"),
                    )
                });
            let zone = zone.clone();
            list = list.child(row.on_click(cx.listener(
                move |this, _: &ClickEvent, window, cx| choose(this, &zone, window, cx),
            )));
        }
    }

    let menu = div()
        .id(format!("{prefix}-menu"))
        .test_support()
        .absolute()
        .left(x)
        .top(y)
        .w(width)
        .occlude()
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(6.))
        .rounded(px(t.radius))
        .bg(t.surface_2)
        .border_1()
        .border_color(t.border)
        .child(
            div()
                .id(format!("{prefix}-search"))
                .test_support()
                .child(Input::new(search)),
        )
        .child(list);

    let union_left = x.min(anchor.origin.x);
    let union_right = (x + width).max(anchor.origin.x + anchor.size.width);
    let union_top = y.min(anchor.origin.y);
    let union_bottom = (y + height).max(anchor.origin.y + anchor.size.height);

    Some(
        div()
            .id("person-edit-menu-layer")
            .test_support()
            .absolute()
            .inset_0()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close_person_edit_menu(window, cx);
                }
            }))
            .children(
                [
                    (px(0.), px(0.), viewport.width, union_top),
                    (
                        px(0.),
                        union_bottom,
                        viewport.width,
                        viewport.height - union_bottom,
                    ),
                    (px(0.), union_top, union_left, union_bottom - union_top),
                    (
                        union_right,
                        union_top,
                        viewport.width - union_right,
                        union_bottom - union_top,
                    ),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (left, top, width, height))| {
                    div()
                        .id(("person-edit-menu-catcher", index))
                        .test_support()
                        .absolute()
                        .left(left)
                        .top(top)
                        .w(width)
                        .h(height)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                                this.close_person_edit_menu(window, cx)
                            }),
                        )
                }),
            )
            .child(menu)
            .into_any_element(),
    )
}

/// Edit a colleague from the roster: name, home city and an optional new trip,
/// all in one save. The home city and the trip destination each use a
/// searchable dropdown with saved zones first; a different home moves the
/// person (creating the clock when needed), and a trip reuses the same rules
/// as the travel form (`start_travel`).
pub fn person_edit(
    state: &ClockState,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    let edit = state.person_edit.as_ref()?;

    let panel = panel_shell("person-edit", 520., t)
        .track_focus(&edit.focus)
        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                this.close_person_edit(cx);
            }
        }))
        .child(panel_header("Edit colleague", t))
        // Header and footer stay put. The body needs no scroll region: the
        // location lists float at the app root, so content height is fixed and
        // the footer always stays inside the panel's hitbox.
        .child(
            div()
                .id("person-edit-body")
                .test_support()
                .w_full()
                .px(px(16.))
                .py(px(12.))
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(section_label("Name", t))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(name_field("person-edit-first", &edit.first))
                        .child(name_field("person-edit-middle", &edit.middle))
                        .child(name_field("person-edit-last", &edit.last)),
                )
                .child(location_dropdown(
                    LocationDropdown {
                        prefix: "person-edit-home",
                        label: "Home city",
                        placeholder: "Choose the home city",
                        selected: edit.zone.as_deref(),
                        anchor: &edit.home_anchor,
                        helper: "Saving a different city moves the colleague to that city's card.",
                    },
                    state,
                    t,
                    cx,
                    AppShell::toggle_person_edit_home,
                ))
                .child(div().mt(px(2.)).border_t_1().border_color(t.divider))
                .child(section_label("Start a trip", t))
                .child(
                    div()
                        .text(Text::Small)
                        .text_color(t.text_faint)
                        .child(
                            "Optional: pick a destination and the first day there. Leave it on \
                             \"No trip\" to only edit the colleague.",
                        ),
                )
                .child(location_dropdown(
                    LocationDropdown {
                        prefix: "person-edit-travel",
                        label: "Destination",
                        placeholder: "No trip",
                        selected: edit.travel.as_deref(),
                        anchor: &edit.travel_anchor,
                        helper: "Dates are destination-local; existing trips and their overlap rules apply.",
                    },
                    state,
                    t,
                    cx,
                    AppShell::toggle_person_edit_travel,
                ))
                .when(edit.travel.is_some(), |el| {
                    el.child(
                        div()
                            .flex()
                            // Top-align: Starts carries a hint line under its
                            // field, and centring would push Ends down.
                            .items_start()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(section_label("Starts", t))
                                    .child(Input::new(&edit.trip_start))
                                    .child(
                                        div()
                                            .text(Text::Small)
                                            .text_color(t.text_faint)
                                            .child("First day in the destination, from 00:00."),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(section_label("Ends", t))
                                    .when(edit.trip_open_ended, |el| {
                                        el.child(
                                            div()
                                                .h(px(28.))
                                                .px(px(8.))
                                                .flex()
                                                .items_center()
                                                .rounded(px(t.radius))
                                                .bg(t.field)
                                                .text(Text::Small)
                                                .text_color(t.text_faint)
                                                .child("Open-ended"),
                                        )
                                    })
                                    .when(!edit.trip_open_ended, |el| {
                                        el.child(Input::new(&edit.trip_end))
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("person-edit-trip-open-ended")
                            .test_support()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .cursor_pointer()
                            .child(
                                div()
                                    .size(px(14.))
                                    .rounded(px(3.))
                                    .border_1()
                                    .border_color(if edit.trip_open_ended {
                                        t.accent
                                    } else {
                                        t.border
                                    })
                                    .when(edit.trip_open_ended, |el| el.bg(t.accent_soft))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(edit.trip_open_ended, |el| {
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
                                this.toggle_person_edit_trip_open_ended(cx)
                            })),
                    )
                })
                .when_some(edit.error.clone(), |el, message| el.child(error_banner(&message, t))),
        )
        .child(
            div()
                .id("person-edit-footer")
                .test_support()
                .w_full()
                .flex_shrink_0()
                .mt(px(10.))
                .px(px(16.))
                .py(px(12.))
                .border_t_1()
                .border_color(t.divider)
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1())
                .child(
                    div()
                        .id("person-edit-cancel")
                        .test_support()
                        .h(px(28.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(t.radius))
                        .cursor_pointer()
                        .text(Text::Button)
                        .text_color(t.text_muted)
                        .hover(|style| style.bg(t.hover))
                        .child("Cancel")
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.close_person_edit(cx)
                        })),
                )
                .child(
                    div()
                        .id("person-edit-save")
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
                        .child("Save")
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.save_person_edit(cx)
                        })),
                ),
        );

    let layer = div()
        .id("person-edit-layer")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .id("person-edit-backdrop")
                .occlude()
                .test_support()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000080))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _window, cx| this.close_person_edit(cx)),
                ),
        )
        .child(div().occlude().child(panel));

    Some(layer.into_any_element())
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
                .child("No colleagues yet."),
        );
    }
    for status in &people.residents {
        residents = residents.child(resident_row(state, status, t, cx));
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
                    "{} \u{00b7} {} \u{00b7} {} {}",
                    entry.city,
                    entry.tz_identifier,
                    people.residents.len(),
                    if people.residents.len() == 1 {
                        "colleague"
                    } else {
                        "colleagues"
                    }
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(section_label("Colleagues", t))
                .child(residents),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(name_field(
                    "clock-inspector-add-first",
                    &inspector.add_first,
                ))
                .child(name_field(
                    "clock-inspector-add-middle",
                    &inspector.add_middle,
                ))
                .child(name_field("clock-inspector-add-last", &inspector.add_last))
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

    // Same modal layer as the picker: a press-catching backdrop sits behind a
    // sibling panel layer, and both occlude the pointer so the cards under
    // them stop hovering as well as stop taking clicks.
    let layer = div()
        .id("clock-inspector-layer")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .id("clock-inspector-backdrop")
                .occlude()
                .test_support()
                .absolute()
                .inset_0()
                .bg(rgba(0x00000080))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _window, cx| this.close_inspector(cx)),
                ),
        )
        .child(div().occlude().child(panel));

    Some(layer.into_any_element())
}

fn section_label(label: &str, t: &Tokens) -> AnyElement {
    div()
        .text(Text::SmallStrong)
        .text_color(t.text_faint)
        .child(label.to_string())
        .into_any_element()
}

/// One name field in the add or rename form. The three share the row equally
/// and never shrink below zero, so a long typed name scrolls inside its box
/// instead of squeezing its siblings.
fn name_field(id: &'static str, input: &Entity<InputState>) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .flex_1()
        .min_w(px(0.))
        .child(Input::new(input))
        .into_any_element()
}

fn resident_row(
    state: &ClockState,
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
        .filter(|rename| rename.person_id == person_id)
        .cloned();

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
                .child(if let Some(rename) = renaming {
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(name_field("clock-person-rename-first", &rename.first))
                        .child(name_field("clock-person-rename-middle", &rename.middle))
                        .child(name_field("clock-person-rename-last", &rename.last))
                        .into_any_element()
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
        row = row.child(trip_row(state, travel, t, cx));
    } else if let Some(travel) = &status.upcoming {
        row = row.child(trip_row(state, travel, t, cx));
    }
    row.into_any_element()
}

fn trip_row(
    state: &ClockState,
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

    Some(overlay("clock-form-overlay", panel.into_any_element()))
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
        ConfirmKind::RemovePerson { .. } => "Remove colleague",
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

    Some(overlay("clock-confirm-overlay", panel.into_any_element()))
}
