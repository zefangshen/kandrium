//! Clocks: saved cities, residents and travel windows.
//!
//! Data lives in SQLite and is reached only through `kandrium_core::clocks`
//! (via [`crate::store::Store`]). This module caches one snapshot and renders
//! from it: the minute tick rewrites `now` and nothing else, so it never takes
//! a database lock.
//!
//! # Drag reordering
//!
//! The grid is a fixed three-column layout of `CARD_H` cards with `GAP`
//! between them. Insertion targets are computed from that **static** layout,
//! never from the reflowed preview: once the preview changes order, slot i no
//! longer contains card i, and using the preview to derive the target makes
//! the target chase the pointer.
//!
//! Row bands are half-open intervals bounded by the midpoint of the gap
//! between rows (`row_bottom + GAP / 2`), not by the row's own centre. A
//! pointer inside the gap therefore belongs to the upper row until the gap's
//! midpoint and to the lower row afterwards, which keeps a cross-row drag
//! stable while the preview is reflowing.
//!
//! Motion is reduced to the lift of the ghost card: the reflow itself is
//! instantaneous, so there is nothing to animate and reduced-motion layouts
//! behave identically. The egui snap-back flight has no gpui counterpart and
//! is deliberately omitted.

use std::time::Duration;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    Animation, AnimationExt, AnyElement, ClickEvent, Context, DragMoveEvent, Entity, FocusHandle,
    IntoElement, KeyDownEvent, Render, TestSupportExt, Window, div, px,
};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use kandrium_core::clocks::{available_zones, zone_matches};
use kandrium_core::{
    ClockText, Person, ProbeAmbiguity, ProbeReading, Travel, city_name, format_clock,
    placement_for, resolve_zone, system_zone,
};

use crate::anim::{LIFT, ease_standard};
use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::store::ClockData;
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::clocks_panels;

/// Height of one city card.
pub const CARD_H: f32 = 156.0;
/// Gap between cards, horizontally and between rows.
pub const GAP: f32 = 12.0;
/// Cards per row.
pub const COLUMNS: usize = 3;
/// Cards never shrink below this width; narrower windows scroll instead.
pub const MIN_COL_W: f32 = 160.0;
/// The narrowest grid that still fits three minimum-width cards.
const MIN_GRID_W: f32 = COLUMNS as f32 * MIN_COL_W + (COLUMNS - 1) as f32 * GAP;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One saved city with its resolved zone and residents.
pub struct ClockEntry {
    pub id: String,
    pub tz_identifier: String,
    pub city: String,
    pub zone: Option<TimeZone>,
    pub people: Vec<Person>,
}

/// Resident or visitor status at one instant.
#[derive(Clone)]
pub struct PersonStatus {
    pub person: Person,
    /// City this person is currently visiting, when it is not home.
    pub away_to: Option<String>,
    /// End of the active trip (`None` when open-ended).
    pub until: Option<i64>,
    /// The active trip, when the person is away.
    pub travel: Option<Travel>,
    /// Earliest planned trip when the person is home.
    pub upcoming: Option<Travel>,
}

/// Residents and visitors for one city.
#[derive(Default)]
pub struct CityPeople {
    pub residents: Vec<PersonStatus>,
    pub visitors: Vec<PersonStatus>,
}

/// What the IANA picker is choosing for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerTarget {
    /// Append the zone to the Clocks page.
    AddCity,
    /// Set the zone the probe reads its wall clock in.
    ProbeZone,
}

/// Add-city picker.
pub struct PickerState {
    pub input: Entity<InputState>,
    /// Index into the filtered zone list.
    pub selected: usize,
    /// Last failure reported by core while adding a zone.
    pub error: Option<String>,
    /// Whether the chosen zone is added as a city or becomes the probe zone.
    pub target: PickerTarget,
}

/// Per-city people inspector.
pub struct InspectorState {
    pub clock_id: String,
    pub add: Entity<InputState>,
    /// Inline rename: `(person_id, input)`.
    pub rename: Option<(String, Entity<InputState>)>,
    pub error: Option<String>,
}

/// What the travel form is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormKind {
    Assign,
    Extend,
    Relocate,
}

/// Destination / dates form.
pub struct FormState {
    pub kind: FormKind,
    pub travel_id: Option<String>,
    pub person_id: String,
    pub person_name: String,
    pub home_clock_id: String,
    pub home_city: String,
    pub current_city: Option<String>,
    pub destination: Option<String>,
    pub start: Entity<InputState>,
    pub end: Entity<InputState>,
    pub open_ended: bool,
    pub error: Option<String>,
}

/// Confirmation target.
#[derive(Clone, Debug)]
pub enum ConfirmKind {
    RemoveClock { clock_id: String },
    RemovePerson { person_id: String },
    RemoveTravel { travel_id: String },
}

/// Confirmation target.
#[derive(Clone, Debug)]
pub struct ConfirmState {
    pub kind: ConfirmKind,
    pub title: String,
    pub message: String,
    pub error: Option<String>,
}

/// The in-flight reorder.
#[derive(Clone, Debug)]
pub struct DragState {
    pub clock_id: String,
    pub from: usize,
}

/// A committed probe: the instant every card reads at.
pub struct ProbeState {
    pub instant: Timestamp,
    /// How the typed wall clock resolved (DST gap/fold note).
    pub ambiguity: ProbeAmbiguity,
    /// The text the user committed, kept verbatim while it still resolves.
    pub text: String,
}

/// Everything the Clocks section renders.
pub struct ClockState {
    pub entries: Vec<ClockEntry>,
    pub travels: Vec<Travel>,
    pub now: Timestamp,
    /// The active probe; `None` means live time. Never persisted.
    pub probe: Option<ProbeState>,
    /// Zone the probe reads in. `None` follows [`ClockState::local_zone`].
    pub probe_zone: Option<String>,
    /// The zone captured when the section was created; the probe's default.
    pub local_zone: TimeZone,
    /// Text field shared by every probe, created on first render.
    pub probe_input: Option<Entity<InputState>>,
    /// Placeholder currently installed on `probe_input`, so the 12/24-hour
    /// preference only rewrites it when the shape really changed.
    pub probe_placeholder: String,
    /// Whether the probe-zone menu is open.
    pub probe_menu: bool,
    /// Focus parked on the probe-zone menu while it is open, so `Esc` reaches
    /// the panel even though it holds no text field.
    pub probe_menu_focus: Option<FocusHandle>,
    /// Last commit failure, shown on the control and in its `aria_label`.
    pub probe_error: Option<String>,
    /// DST note for the active probe ("02:30 does not exist here").
    pub probe_hint: Option<String>,
    pub search: String,
    pub search_input: Option<Entity<InputState>>,
    pub zones: Vec<String>,
    pub picker: Option<PickerState>,
    pub inspector: Option<InspectorState>,
    pub form: Option<FormState>,
    pub confirm: Option<ConfirmState>,
    pub drag: Option<DragState>,
    pub hover: Option<usize>,
    pub error: Option<String>,
}

impl ClockState {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            travels: Vec::new(),
            now: Timestamp::now(),
            probe: None,
            probe_zone: None,
            local_zone: system_zone(),
            probe_input: None,
            probe_placeholder: String::new(),
            probe_menu: false,
            probe_menu_focus: None,
            probe_error: None,
            probe_hint: None,
            search: String::new(),
            search_input: None,
            zones: Vec::new(),
            picker: None,
            inspector: None,
            form: None,
            confirm: None,
            drag: None,
            hover: None,
            error: None,
        }
    }

    /// Replace the cached snapshot. `now` is owned by the render loop.
    pub fn load(&mut self, data: ClockData) {
        self.entries = data
            .clocks
            .into_iter()
            .map(|clock| {
                let people = data
                    .people
                    .iter()
                    .filter(|person| person.home_clock_id == clock.id)
                    .cloned()
                    .collect();
                ClockEntry {
                    city: city_name(&clock.tz_identifier),
                    zone: resolve_zone(&clock.tz_identifier),
                    tz_identifier: clock.tz_identifier,
                    id: clock.id,
                    people,
                }
            })
            .collect();
        self.travels = data.travels;
    }

    pub fn set_now(&mut self, now: Timestamp) {
        self.now = now;
    }

    /// The instant every card and person row reads: the probe when one is
    /// active, the live clock otherwise.
    pub fn instant(&self) -> Timestamp {
        self.probe.as_ref().map_or(self.now, |probe| probe.instant)
    }

    /// The zone the probe reads its wall clock in.
    pub fn probe_zone_value(&self) -> TimeZone {
        self.probe_zone
            .as_deref()
            .and_then(resolve_zone)
            .unwrap_or_else(|| self.local_zone.clone())
    }

    /// Label on the zone trigger. The local default names the system city
    /// when the platform reports one.
    pub fn probe_zone_label(&self) -> String {
        match &self.probe_zone {
            Some(identifier) => city_name(identifier),
            None => self
                .local_zone
                .iana_name()
                .map(city_name)
                .unwrap_or_else(|| "Local".to_string()),
        }
    }

    pub fn clear_drag(&mut self) {
        self.drag = None;
        self.hover = None;
    }

    /// Milliseconds until the next minute or the nearest travel boundary.
    pub fn next_wait(&self) -> Duration {
        let minute = until_next_minute(self.now);
        match until_next_travel(&self.travels, self.now.as_second()) {
            Some(travel) => minute.min(travel),
            None => minute,
        }
    }

    /// Load the IANA list once, on first open of the picker.
    pub fn ensure_zones(&mut self) {
        if self.zones.is_empty() {
            self.zones = available_zones();
        }
    }

    /// Entry indices matching the search, in display order.
    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.search.trim().is_empty() {
            return (0..self.entries.len()).collect();
        }
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| matches_search(entry, &self.search))
            .map(|(index, _)| index)
            .collect()
    }

    /// Entry by id.
    pub fn entry(&self, clock_id: &str) -> Option<&ClockEntry> {
        self.entries.iter().find(|entry| entry.id == clock_id)
    }

    /// Resident and visitor status for one city at `now`.
    pub fn city_people(&self, clock_id: &str) -> CityPeople {
        let Some(entry) = self.entry(clock_id) else {
            return CityPeople::default();
        };
        city_people(
            &self.entries,
            &self.travels,
            entry,
            self.instant().as_second(),
        )
    }

    /// Display order of clock ids.
    pub fn order(&self) -> Vec<String> {
        self.entries.iter().map(|entry| entry.id.clone()).collect()
    }
}

impl Default for ClockState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Pure layout and behaviour helpers
// ---------------------------------------------------------------------------

/// Static three-column geometry of the grid in window coordinates.
#[derive(Clone, Copy, Debug)]
pub struct StaticGrid {
    pub origin_x: f32,
    pub origin_y: f32,
    pub width: f32,
    pub count: usize,
}

impl StaticGrid {
    pub fn new(origin_x: f32, origin_y: f32, width: f32, count: usize) -> Self {
        Self {
            origin_x,
            origin_y,
            width,
            count,
        }
    }

    /// Width of one column. Mirrors `flex_1` cells separated by `GAP`; the
    /// floor matches the card minimum, and the grid container is at least
    /// [`MIN_GRID_W`] wide so the two never disagree.
    pub fn col_w(&self) -> f32 {
        ((self.width - (COLUMNS - 1) as f32 * GAP) / COLUMNS as f32).max(MIN_COL_W)
    }

    pub fn rows(&self) -> usize {
        self.count.div_ceil(COLUMNS)
    }

    pub fn height(&self) -> f32 {
        if self.count == 0 {
            return 0.0;
        }
        self.rows() as f32 * (CARD_H + GAP) - GAP
    }

    /// Top-left corner of a static slot.
    pub fn slot_origin(&self, slot: usize) -> (f32, f32) {
        let column = slot % COLUMNS;
        let row = slot / COLUMNS;
        (
            self.origin_x + column as f32 * (self.col_w() + GAP),
            self.origin_y + row as f32 * (CARD_H + GAP),
        )
    }

    /// Final index for a pointer at window coordinates, using half-open row
    /// bands bounded by the gap midpoint and, within a row, the number of
    /// slot centres left of the pointer. `None` outside the grid.
    pub fn slot_at(&self, x: f32, y: f32) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        let rel_x = x - self.origin_x;
        let rel_y = y - self.origin_y;
        if rel_x < 0.0 || rel_y < 0.0 || rel_x > self.width || rel_y > self.height() {
            return None;
        }
        let stride = CARD_H + GAP;
        let rows = self.rows();
        let mut row = rows - 1;
        for candidate in 0..rows {
            let boundary = if candidate + 1 < rows {
                candidate as f32 * stride + CARD_H + GAP * 0.5
            } else {
                f32::INFINITY
            };
            if rel_y < boundary {
                row = candidate;
                break;
            }
        }
        let start = row * COLUMNS;
        let end = (start + COLUMNS).min(self.count);
        let col_w = self.col_w();
        let before = (start..end)
            .filter(|slot| {
                let column = (*slot % COLUMNS) as f32;
                column * (col_w + GAP) + col_w * 0.5 < rel_x
            })
            .count();
        Some((start + before).min(self.count - 1))
    }
}

/// Complete id order after moving `from` to `to` (remove, then insert).
pub fn reordered(ids: &[String], from: usize, to: usize) -> Vec<String> {
    let mut next = ids.to_vec();
    if from >= next.len() || to >= next.len() {
        return next;
    }
    let id = next.remove(from);
    next.insert(to, id);
    next
}

/// Preview layout: the target slot of every card that stays visible, plus the
/// slot left empty for the lifted card.
pub fn preview_slots(
    count: usize,
    from: Option<usize>,
    hover: Option<usize>,
) -> (Vec<(usize, usize)>, Option<usize>) {
    let mut placements = Vec::with_capacity(count);
    let mut placeholder = None;
    for slot in 0..count {
        match from {
            Some(from) => {
                if slot == from {
                    placeholder = Some(hover.unwrap_or(from));
                    continue;
                }
                // Rank in the list with the lifted card removed.
                let rank = if slot < from { slot } else { slot - 1 };
                let target = match hover {
                    Some(hover) if rank < hover => rank,
                    Some(_) => rank + 1,
                    None => slot,
                };
                placements.push((slot, target));
            }
            None => placements.push((slot, slot)),
        }
    }
    (placements, placeholder)
}

/// Search matches the zone text (city, region, identifier) or any resident's
/// name.
pub fn matches_search(entry: &ClockEntry, query: &str) -> bool {
    if zone_matches(&entry.tz_identifier, query) {
        return true;
    }
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    entry
        .people
        .iter()
        .any(|person| person.name.to_lowercase().contains(&needle))
}

/// Residents and visitors for one city at `instant`.
pub fn city_people(
    clocks: &[ClockEntry],
    travels: &[Travel],
    entry: &ClockEntry,
    instant: i64,
) -> CityPeople {
    let mut residents = Vec::with_capacity(entry.people.len());
    for person in &entry.people {
        let active = placement_for(travels, &person.id, instant);
        let away_clock = active
            .and_then(|travel| travel.destination_clock_id.as_deref())
            .filter(|destination| *destination != entry.id);
        let upcoming = if away_clock.is_none() {
            travels
                .iter()
                .filter(|travel| {
                    travel.person_id == person.id
                        && travel.cancelled_at.is_none()
                        && travel.starts_at > instant
                })
                .min_by_key(|travel| travel.starts_at)
                .cloned()
        } else {
            None
        };
        residents.push(PersonStatus {
            person: person.clone(),
            away_to: away_clock.map(|id| city_label(clocks, id)),
            until: away_clock.and_then(|_| active.and_then(|travel| travel.ends_at)),
            travel: if away_clock.is_some() {
                active.cloned()
            } else {
                None
            },
            upcoming,
        });
    }

    let mut visitors = Vec::new();
    for other in clocks {
        if other.id == entry.id {
            continue;
        }
        for person in &other.people {
            let Some(travel) = placement_for(travels, &person.id, instant) else {
                continue;
            };
            if travel.destination_clock_id.as_deref() != Some(entry.id.as_str()) {
                continue;
            }
            visitors.push(PersonStatus {
                person: person.clone(),
                away_to: None,
                until: travel.ends_at,
                travel: Some(travel.clone()),
                upcoming: None,
            });
        }
    }
    visitors.sort_by_key(|status| status.travel.as_ref().map(|travel| travel.starts_at));

    CityPeople {
        residents,
        visitors,
    }
}

pub fn city_label(clocks: &[ClockEntry], clock_id: &str) -> String {
    clocks
        .iter()
        .find(|entry| entry.id == clock_id)
        .map(|entry| entry.city.clone())
        .unwrap_or_else(|| clock_id.to_string())
}

/// "until Sat, 7 Nov. 23:59" in the relevant city, or "no return date".
pub fn until_label(zone: Option<&TimeZone>, ends_at: Option<i64>) -> String {
    let Some(ends_at) = ends_at else {
        return "no return date".to_string();
    };
    let Some(zone) = zone else {
        return "until an unknown time".to_string();
    };
    match Timestamp::from_second(ends_at) {
        Ok(timestamp) => format!(
            "until {}",
            timestamp
                .to_zoned(zone.clone())
                .strftime("%a, %-d %b. %H:%M")
        ),
        Err(_) => "until an unknown time".to_string(),
    }
}

/// "starts Sat, 7 Nov. 00:00" in the destination city.
pub fn starts_label(zone: Option<&TimeZone>, starts_at: i64) -> String {
    let Some(zone) = zone else {
        return "starts at an unknown time".to_string();
    };
    match Timestamp::from_second(starts_at) {
        Ok(timestamp) => format!(
            "starts {}",
            timestamp
                .to_zoned(zone.clone())
                .strftime("%a, %-d %b. %H:%M")
        ),
        Err(_) => "starts at an unknown time".to_string(),
    }
}

/// One-line DST note for a committed probe, or `None` when unambiguous.
pub fn probe_hint(reading: ProbeReading, zone: &TimeZone, use_24h: bool) -> Option<String> {
    let shown = probe_field_text(reading.instant, zone, use_24h);
    match reading.ambiguity {
        ProbeAmbiguity::Unique => None,
        ProbeAmbiguity::Gap => Some(format!("{shown} \u{00b7} skipped hour")),
        ProbeAmbiguity::Fold => Some(format!("{shown} \u{00b7} occurs twice, showing the first")),
    }
}

/// The time field's text for an instant: the same shape the cards print.
pub fn probe_field_text(instant: Timestamp, zone: &TimeZone, use_24h: bool) -> String {
    let text = format_clock(instant, zone, use_24h);
    match text.period {
        Some(period) => format!("{} {period}", text.time),
        None => text.time,
    }
}

/// First character of the first two words, uppercased.
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .take(2)
        .filter_map(|word| word.chars().next())
        .flat_map(char::to_uppercase)
        .collect()
}

/// Time until 50 ms past the next minute boundary.
pub fn until_next_minute(now: Timestamp) -> Duration {
    let elapsed =
        now.as_second().rem_euclid(60) as f64 + now.subsec_nanosecond() as f64 / 1_000_000_000.0;
    Duration::from_secs_f64((60.0 - elapsed + 0.05).max(0.05))
}

/// Time until the nearest travel start or expiry, plus 50 ms so the repaint
/// lands just after the boundary.
pub fn until_next_travel(travels: &[Travel], now: i64) -> Option<Duration> {
    let mut nearest: Option<i64> = None;
    for travel in travels {
        if travel.cancelled_at.is_some() {
            continue;
        }
        for instant in [Some(travel.starts_at), travel.ends_at]
            .into_iter()
            .flatten()
        {
            if instant > now {
                nearest = Some(nearest.map_or(instant, |current| current.min(instant)));
            }
        }
    }
    nearest.map(|instant| Duration::from_secs((instant - now) as u64) + Duration::from_millis(50))
}

/// Zones matching the picker query, capped for a responsive list.
pub fn picker_rows(zones: &[String], query: &str) -> Vec<String> {
    zones
        .iter()
        .filter(|zone| zone_matches(zone, query))
        .take(200)
        .cloned()
        .collect()
}

pub fn zone_is_added(entries: &[ClockEntry], zone: &str) -> bool {
    entries.iter().any(|entry| entry.tz_identifier == zone)
}

/// Next selectable index in `rows`, skipping zones already added.
pub fn next_selectable(
    entries: &[ClockEntry],
    rows: &[String],
    from: usize,
    forward: bool,
) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let len = rows.len();
    let mut index = from.min(len - 1);
    for _ in 0..len {
        index = if forward {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        };
        if !zone_is_added(entries, &rows[index]) {
            return index;
        }
    }
    from.min(len - 1)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Drag payload carried by gpui's session.
#[derive(Clone)]
pub struct ClockDrag {
    pub clock_id: String,
    pub city: String,
    pub from: usize,
}

pub fn clocks_view(
    state: &mut ClockState,
    use_24h: bool,
    t: &Tokens,
    window: &mut Window,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let search_input = ensure_search_input(state, window, cx);
    let probe_input = ensure_probe_input(state, window, cx);
    sync_probe_placeholder(state, &probe_input, use_24h, window, cx);
    let filtered = state.filtered_indices();
    let reduced = cx.reduce_motion();
    // Below the plan's 980 px threshold the probe drops its glyphs and labels
    // so the row never overflows; the controls keep working. gpui reports the
    // viewport in device pixels, so divide by the display scale first.
    let logical_w = f32::from(window.viewport_size().width) / window.scale_factor();
    let compact = logical_w < 980.0;

    let mut page = div()
        .id("clocks-view")
        .test_support()
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .child(toolbar(
            state,
            &search_input,
            &probe_input,
            use_24h,
            compact,
            t,
            cx,
        ));

    if let Some(error) = &state.error {
        page = page.child(
            div()
                .mx(px(16.))
                .mb(px(8.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(t.radius))
                .border_1()
                .border_color(t.danger)
                .text(Text::Small)
                .text_color(t.danger)
                .child(error.clone()),
        );
    }

    let body = if state.entries.is_empty() {
        empty_state(t, cx)
    } else if filtered.is_empty() {
        no_results(state, t, cx)
    } else {
        grid(state, &filtered, use_24h, t, reduced, cx)
    };
    page = page.child(body);

    if let Some(panel) = clocks_panels::picker(state, t, window, cx) {
        page = page.child(panel);
    }
    if let Some(panel) = clocks_panels::inspector(state, t, window, cx) {
        page = page.child(panel);
    }
    if let Some(panel) = clocks_panels::form(state, t, window, cx) {
        page = page.child(panel);
    }
    if let Some(panel) = clocks_panels::confirm(state, t, cx) {
        page = page.child(panel);
    }
    if let Some(panel) = clocks_panels::probe_zone_menu(state, t, cx) {
        page = page.child(panel);
    }
    page.into_any_element()
}

fn ensure_search_input(
    state: &mut ClockState,
    window: &mut Window,
    cx: &mut Context<AppShell>,
) -> Entity<InputState> {
    if let Some(input) = &state.search_input {
        return input.clone();
    }
    let input =
        cx.new(|cx| InputState::new(window, cx).placeholder("Search cities, zones or people"));
    // The grid filters from `ClockState::search`, so mirror every keystroke.
    cx.observe(&input, |this: &mut AppShell, input, cx| {
        let value = input.read(cx).value().to_string();
        if this.clocks.search != value {
            this.clocks.search = value;
            this.clocks.clear_drag();
            cx.notify();
        }
    })
    .detach();
    state.search_input = Some(input.clone());
    input
}

fn ensure_probe_input(
    state: &mut ClockState,
    window: &mut Window,
    cx: &mut Context<AppShell>,
) -> Entity<InputState> {
    if let Some(input) = &state.probe_input {
        return input.clone();
    }
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(""));
    // Commit on Enter and on blur, never per keystroke: a half-typed `1`
    // must not jump the page to 01:00.
    cx.subscribe_in(
        &input,
        window,
        |this: &mut AppShell, _input, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_clock_probe(window, cx),
            InputEvent::Change => this.clear_clock_probe_error(cx),
            _ => {}
        },
    )
    .detach();
    state.probe_input = Some(input.clone());
    input
}

/// The placeholder follows the 12/24-hour preference; rewrite it only when
/// the shape really changed so the field is not touched every frame.
fn sync_probe_placeholder(
    state: &mut ClockState,
    input: &Entity<InputState>,
    use_24h: bool,
    window: &mut Window,
    cx: &mut Context<AppShell>,
) {
    let desired = if use_24h { "14:30" } else { "2:30 pm" };
    if state.probe_placeholder == desired {
        return;
    }
    state.probe_placeholder = desired.to_string();
    let desired = desired.to_string();
    input.update(cx, |input, cx| {
        input.set_placeholder(desired, window, cx);
    });
}

fn toolbar(
    state: &ClockState,
    search_input: &Entity<InputState>,
    probe_input: &Entity<InputState>,
    use_24h: bool,
    compact: bool,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let count = state.entries.len();
    div()
        .id("clocks-toolbar")
        .test_support()
        .w_full()
        .flex_shrink_0()
        .h(px(46.))
        .px(px(20.))
        .flex()
        .items_center()
        .gap(px(10.))
        .border_b_1()
        .border_color(t.border)
        .child(div().text(Text::Heading).text_color(t.text).child("Clocks"))
        .child(
            div()
                .text(Text::Micro)
                .text_color(t.text_faint)
                .child(format!("{count} saved")),
        )
        .child(
            div()
                .id("clocks-search")
                .test_support()
                .w(px(if compact { 150. } else { 200. }))
                .ml(px(14.))
                .child(crate::views::field_input(search_input, t).prefix(icon(
                    Icon::Search,
                    11.,
                    t.text_faint,
                ))),
        )
        .child(probe_control(state, probe_input, compact, t, cx))
        .child(div().flex_1())
        .child(units_toggle(use_24h, t, cx))
        .child(
            div()
                .id("clocks-add-city")
                .test_support()
                .flex()
                .items_center()
                .gap(px(5.))
                .h(px(28.))
                .px(px(10.))
                .rounded(px(t.radius))
                .border_1()
                .border_color(t.border)
                .bg(t.card)
                .cursor_pointer()
                .text(Text::Small)
                .text_color(t.text_muted)
                .hover(|style| {
                    style
                        .bg(t.surface_2)
                        .border_color(t.border_hover)
                        .text_color(t.text)
                })
                .child(icon(Icon::Plus, 11., t.text_muted))
                .child("Add city")
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.open_clock_picker(window, cx)
                })),
        )
        .into_any_element()
}

/// The time + zone control: a calendar glyph, the time field, the zone
/// trigger, and — while a probe is active — the `Now` chip.
///
/// The field keeps `field_input`'s own fill and border, so the accent lives on
/// the zone chip instead of a wrapping pill; framing the field twice is the
/// known duplicate-box regression.
fn probe_control(
    state: &ClockState,
    input: &Entity<InputState>,
    compact: bool,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let probing = state.probe.is_some();
    let invalid = state.probe_error.is_some();
    let label = state
        .probe_error
        .clone()
        .or_else(|| state.probe_hint.clone())
        .or_else(|| {
            state.probe.as_ref().map(|probe| {
                format!(
                    "Probing {} \u{00b7} {}",
                    probe.text,
                    state.probe_zone_label()
                )
            })
        })
        .unwrap_or_else(|| "Read every city at one time".to_string());
    let zone_label = state.probe_zone_label();
    let tokens = *t;
    let zone_icon = if probing { t.accent_text } else { t.text_muted };

    div()
        .id("clocks-probe")
        .test_support()
        .aria_label(label)
        .h(px(28.))
        .flex()
        .items_center()
        .gap(px(6.))
        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
            this.handle_clock_probe_key(event, window, cx)
        }))
        .when(!compact, |el| {
            el.child(icon(
                Icon::Calendar,
                11.,
                if probing { t.accent_text } else { t.text_faint },
            ))
        })
        .child(
            div()
                .id("clocks-probe-time")
                .test_support()
                .w(px(92.))
                .child(crate::views::field_input(input, t)),
        )
        .child(
            div()
                .id("clocks-probe-zone")
                .test_support()
                .h(px(28.))
                .px(px(if compact { 6. } else { 8. }))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(t.radius))
                .border_1()
                .border_color(if invalid {
                    t.danger
                } else if probing {
                    t.accent
                } else {
                    t.border
                })
                .bg(if probing { t.accent_soft } else { t.card })
                .cursor_pointer()
                .text(Text::Small)
                .text_color(if probing { t.accent_text } else { t.text_muted })
                .hover(move |style| style.bg(tokens.surface_2).border_color(tokens.border_hover))
                .when(compact, |el| el.child(icon(Icon::Globe, 11., zone_icon)))
                .when(!compact, |el| el.child(zone_label))
                .child(icon(
                    Icon::ChevronDown,
                    10.,
                    if probing { t.accent_text } else { t.text_muted },
                ))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.toggle_clock_probe_menu(window, cx)
                })),
        )
        .when(probing, |el| {
            el.child(
                div()
                    .id("clocks-probe-now")
                    .test_support()
                    .aria_label("Now")
                    .h(px(28.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .rounded(px(t.radius))
                    .bg(t.accent_soft)
                    .cursor_pointer()
                    .text(Text::SmallStrong)
                    .text_color(t.accent_text)
                    .hover(move |style| style.bg(tokens.surface_2))
                    .child(icon(Icon::RotateCcw, 10., t.accent_text))
                    .when(!compact, |el| el.child("Now"))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.clear_clock_probe(window, cx)
                    })),
            )
        })
        .into_any_element()
}

fn units_toggle(use_24h: bool, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    div()
        .id("clocks-units")
        .test_support()
        .flex()
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(t.radius))
        .bg(t.hover)
        .cursor_pointer()
        .on_click(
            cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_clock_units(window, cx)),
        )
        .child(unit_chip("12h", !use_24h, t))
        .child(unit_chip("24h", use_24h, t))
        .into_any_element()
}

fn unit_chip(label: &'static str, selected: bool, t: &Tokens) -> AnyElement {
    div()
        .px(px(8.))
        .h(px(22.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .text(Text::Micro)
        .text_color(if selected { t.text } else { t.text_ghost })
        .when(selected, |el| el.bg(t.surface_2))
        .child(label)
        .into_any_element()
}

fn empty_state(t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    div()
        .id("clocks-empty")
        .test_support()
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .p(px(28.))
                .rounded(px(t.radius))
                .bg(t.card)
                .border_1()
                .border_color(t.border)
                .child(icon(Icon::Globe, 28., t.text_faint))
                .child(
                    div()
                        .text(Text::Strong)
                        .text_color(t.text)
                        .child("No saved cities yet"),
                )
                .child(
                    div()
                        .text(Text::Small)
                        .text_color(t.text_muted)
                        .child("Add a city to see its time, residents and travel."),
                )
                .child(
                    div()
                        .id("clocks-empty-add")
                        .test_support()
                        .mt(px(4.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(28.))
                        .px(px(10.))
                        .rounded(px(t.radius))
                        .bg(t.accent_soft)
                        .cursor_pointer()
                        .text(Text::Button)
                        .text_color(t.accent_text)
                        .child(icon(Icon::Plus, 14., t.accent_text))
                        .child("Add city")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.open_clock_picker(window, cx)
                        })),
                ),
        )
        .into_any_element()
}

fn no_results(state: &ClockState, t: &Tokens, cx: &mut Context<AppShell>) -> AnyElement {
    let query = state.search.clone();
    div()
        .id("clocks-no-results")
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .child(
            div()
                .text(Text::Body)
                .text_color(t.text_muted)
                .child(format!("No cities match \u{201c}{query}\u{201d}.")),
        )
        .child(
            div()
                .id("clocks-clear-search")
                .cursor_pointer()
                .text(Text::SmallStrong)
                .text_color(t.accent_text)
                .child("Clear search")
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.clear_clock_search(window, cx)
                })),
        )
        .into_any_element()
}

fn grid(
    state: &ClockState,
    display: &[usize],
    use_24h: bool,
    t: &Tokens,
    reduced: bool,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let count = display.len();
    let drag_from = state.drag.as_ref().map(|drag| drag.from);
    let (placements, placeholder) = if drag_from.is_some() && state.search.trim().is_empty() {
        preview_slots(count, drag_from, state.hover)
    } else {
        preview_slots(count, None, None)
    };
    let mut at_slot: Vec<Option<usize>> = vec![None; count];
    for (source, target) in placements {
        if let Some(slot) = at_slot.get_mut(target) {
            *slot = Some(source);
        }
    }

    let shell = cx.entity().downgrade();
    let rows = count.div_ceil(COLUMNS);
    let mut grid = div()
        .id("clocks-grid")
        .test_support()
        .relative()
        .w_full()
        .min_w(px(MIN_GRID_W))
        .flex()
        .flex_col()
        .gap(px(GAP))
        .on_drag_move::<ClockDrag>(cx.listener(
            |this, event: &DragMoveEvent<ClockDrag>, _window, cx| this.hover_clock(event, cx),
        ))
        .on_drop::<ClockDrag>(
            cx.listener(|this, drag: &ClockDrag, _window, cx| this.drop_clock(drag, cx)),
        );

    for row in 0..rows {
        let mut row_div = div().flex().gap(px(GAP)).h(px(CARD_H));
        for column in 0..COLUMNS {
            let slot = row * COLUMNS + column;
            let cell = div()
                .id(format!("clocks-slot-{slot}"))
                .flex_1()
                .min_w_0()
                .h(px(CARD_H));
            if slot >= count {
                row_div = row_div.child(cell);
                continue;
            }
            if placeholder == Some(slot) {
                row_div = row_div.child(
                    cell.rounded(px(t.radius))
                        .border_1()
                        .border_color(t.accent)
                        .bg(t.accent_soft),
                );
                continue;
            }
            let Some(source) = at_slot[slot] else {
                row_div = row_div.child(cell);
                continue;
            };
            let entry_index = display[source];
            let entry = &state.entries[entry_index];
            row_div = row_div
                .child(cell.child(card(state, entry, source, use_24h, t, reduced, &shell, cx)));
        }
        grid = grid.child(row_div);
    }

    div()
        .id("clocks-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(20.))
        .pt(px(16.))
        .pb(px(26.))
        .child(grid)
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn card(
    state: &ClockState,
    entry: &ClockEntry,
    from: usize,
    use_24h: bool,
    t: &Tokens,
    reduced: bool,
    shell: &gpui_kit::WeakEntity<AppShell>,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    // `on_drag` needs a `'static` payload, so the handle is cloned in.
    let drag_handle = shell.clone();
    let text = entry
        .zone
        .as_ref()
        .map(|zone| format_clock(state.instant(), zone, use_24h))
        .unwrap_or_else(|| ClockText {
            time: "--:--".into(),
            period: None,
            date: "unknown zone".into(),
            offset: "GMT?".into(),
        });
    let people = state.city_people(&entry.id);
    let away = people
        .residents
        .iter()
        .any(|status| status.away_to.is_some());
    let visiting = people.visitors.len();
    let visitors_until = people
        .visitors
        .iter()
        .filter_map(|status| status.travel.as_ref().map(|travel| travel.ends_at))
        .min()
        .flatten();
    let planned = people
        .residents
        .iter()
        .filter_map(|status| status.upcoming.clone())
        .min_by_key(|travel| travel.starts_at);

    let clock_id = entry.id.clone();
    let drag_id = entry.id.clone();
    let drag_city = entry.city.clone();
    let ghost_tokens = *t;
    let remove_id = entry.id.clone();
    let remove_city = entry.city.clone();

    let mut el = div()
        .id(format!("clock-card-{}", entry.id))
        .test_support()
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .gap(px(4.))
        .rounded(px(t.radius_lg))
        .bg(t.card)
        .border_1()
        .border_color(if away { t.accent_line } else { t.border })
        .px(px(12.))
        .pt(px(11.))
        .pb(px(11.))
        .overflow_hidden()
        .cursor_pointer()
        .hover(move |style| {
            style
                .bg(ghost_tokens.surface_2)
                .border_color(ghost_tokens.border_hover)
        })
        .when(
            state
                .drag
                .as_ref()
                .is_some_and(|drag| drag.clock_id == entry.id),
            |el| el.opacity(0.55),
        )
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(6.))
                .child(
                    div()
                        .text(Text::Strong)
                        .text_color(t.text)
                        .truncate()
                        .child(entry.city.clone()),
                )
                .child(
                    div()
                        .text(Text::Micro)
                        .text_color(t.text_ghost)
                        .child(text.offset.clone()),
                ),
        )
        .child(
            div()
                .flex()
                .items_baseline()
                .gap(px(5.))
                .child(
                    div()
                        .text(Text::ClockTime)
                        .text_color(t.text)
                        .child(text.time.clone()),
                )
                .when_some(text.period.clone(), |el, period| {
                    el.child(
                        div()
                            .text(Text::Small)
                            .text_color(t.text_muted)
                            .child(period),
                    )
                }),
        )
        .child(
            div()
                .text(Text::Micro)
                .text_color(t.text_faint)
                .child(text.date.clone()),
        )
        .child(div().flex_1())
        .child(people_row(&people, t));

    if visiting > 0 {
        el = el.child(
            div()
                .id(format!("clock-visiting-{}", entry.id))
                .px(px(6.))
                .h(px(19.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .bg(t.accent_soft)
                .text(Text::Micro)
                .text_color(t.accent_text)
                .child(format!(
                    "VISITING {visiting} \u{00b7} {}",
                    until_label(entry.zone.as_ref(), visitors_until)
                )),
        );
    } else if let Some(travel) = planned {
        let destination = travel
            .destination_clock_id
            .as_deref()
            .map(|id| city_label(&state.entries, id))
            .unwrap_or_default();
        let zone = travel
            .destination_clock_id
            .as_deref()
            .and_then(|id| state.entry(id))
            .and_then(|entry| entry.zone.as_ref());
        el = el.child(
            div()
                .id(format!("clock-planned-{}", entry.id))
                .flex()
                .items_center()
                .gap(px(6.))
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .border_1()
                        .border_color(t.accent),
                )
                .child(format!(
                    "{destination} \u{00b7} {}",
                    starts_label(zone, travel.starts_at)
                )),
        );
    }

    el.child(
        div()
            .id(format!("clock-remove-{}", remove_id))
            .test_support()
            .absolute()
            .top(px(8.))
            .right(px(8.))
            .size(px(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|style| style.bg(t.hover))
            .child(icon(Icon::Trash2, 12., t.text_faint))
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                this.ask_remove_clock(&remove_id, &remove_city, cx);
            })),
    )
    .on_click({
        let clock_id = clock_id.clone();
        cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.open_inspector(&clock_id, window, cx)
        })
    })
    .on_drag(
        ClockDrag {
            clock_id: drag_id,
            city: drag_city,
            from,
        },
        move |drag: &ClockDrag, _offset, _window, cx| {
            let from = drag.from;
            let clock_id = drag.clock_id.clone();
            if let Some(shell) = drag_handle.upgrade() {
                shell.update(cx, |this, cx| this.start_clock_drag(from, &clock_id, cx));
            }
            cx.new(|_| ClockGhost {
                city: drag.city.clone(),
                tokens: ghost_tokens,
                reduced,
            })
        },
    )
    .into_any_element()
}

fn people_row(people: &CityPeople, t: &Tokens) -> AnyElement {
    let extra = people.residents.len().saturating_sub(2);
    div()
        .id("clock-people")
        .flex()
        .items_center()
        .gap(px(4.))
        .pt(px(7.))
        .border_t_1()
        .border_color(t.divider)
        .when(people.residents.is_empty(), |el| {
            el.child(
                div()
                    .text(Text::Micro)
                    .text_color(t.text_ghost)
                    .child("No residents"),
            )
        })
        .children(
            people
                .residents
                .iter()
                .take(2)
                .map(|status| {
                    let away = status.away_to.is_some();
                    div()
                        .size(px(22.))
                        .rounded(px(11.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(t.field)
                        .border_1()
                        .border_color(if away { t.accent_line } else { t.divider })
                        .text(Text::Micro)
                        .text_color(if away { t.accent_text } else { t.text_muted })
                        .child(initials(&status.person.name))
                })
                .collect::<Vec<_>>(),
        )
        .when(extra > 0, |el| {
            el.child(
                div()
                    .size(px(22.))
                    .rounded(px(11.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(t.field)
                    .border_1()
                    .border_color(t.divider)
                    .text(Text::Micro)
                    .text_color(t.text_faint)
                    .child(format!("+{extra}")),
            )
        })
        .into_any_element()
}

/// The card-shaped ghost that follows the pointer.
struct ClockGhost {
    city: String,
    tokens: Tokens,
    reduced: bool,
}

impl Render for ClockGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.tokens;
        let base = div()
            .id("clock-drag-ghost")
            .w(px(276.))
            .h(px(CARD_H))
            .flex()
            .flex_col()
            .justify_between()
            .rounded(px(t.radius))
            .bg(t.card)
            .border_1()
            .border_color(t.accent)
            .p(px(12.))
            .child(
                div()
                    .text(Text::Strong)
                    .text_color(t.text)
                    .truncate()
                    .child(self.city.clone()),
            )
            .child(
                div()
                    .text(Text::Small)
                    .text_color(t.text_muted)
                    .child("Drop to reorder"),
            );
        if self.reduced {
            base.opacity(0.92).into_any_element()
        } else {
            base.with_animation(
                "clock-drag-lift",
                Animation::new(LIFT).with_easing(ease_standard),
                |el, delta| el.opacity(0.55 + 0.37 * delta),
            )
            .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|index| format!("c{index}")).collect()
    }

    fn entry(id: &str, tz: &str, people: &[&str]) -> ClockEntry {
        ClockEntry {
            id: id.into(),
            tz_identifier: tz.into(),
            city: city_name(tz),
            zone: resolve_zone(tz),
            people: people
                .iter()
                .map(|name| Person {
                    id: format!("p-{id}-{name}"),
                    name: (*name).into(),
                    home_clock_id: id.into(),
                    position: 0,
                    created_at: String::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn slot_centres_round_trip() {
        let grid = StaticGrid::new(0.0, 0.0, 940.0, 5);
        for slot in 0..5 {
            let (x, y) = grid.slot_origin(slot);
            let cx = x + grid.col_w() * 0.5;
            let cy = y + CARD_H * 0.5;
            assert_eq!(grid.slot_at(cx, cy), Some(slot), "slot {slot}");
        }
    }

    #[test]
    fn row_bands_split_at_the_gap_midpoint() {
        let grid = StaticGrid::new(0.0, 0.0, 940.0, 6);
        // Just past the first row's bottom edge, still inside the upper band.
        assert_eq!(grid.slot_at(1.0, CARD_H + 3.0), Some(0));
        // Past the gap's midpoint the pointer belongs to the next row.
        assert_eq!(grid.slot_at(1.0, CARD_H + 9.0), Some(3));
    }

    #[test]
    fn vertical_centre_of_the_gap_is_the_boundary() {
        let grid = StaticGrid::new(0.0, 0.0, 940.0, 6);
        assert_eq!(grid.slot_at(1.0, CARD_H + GAP * 0.5), Some(3));
        assert_eq!(grid.slot_at(1.0, CARD_H + GAP * 0.5 - 0.1), Some(0));
    }

    #[test]
    fn pointer_outside_is_none() {
        let grid = StaticGrid::new(0.0, 0.0, 940.0, 5);
        assert_eq!(grid.slot_at(-1.0, 10.0), None);
        assert_eq!(grid.slot_at(10.0, grid.height() + 1.0), None);
    }

    #[test]
    fn cross_row_move_is_a_complete_permutation() {
        let grid = StaticGrid::new(0.0, 0.0, 940.0, 5);
        let (x, y) = grid.slot_origin(3);
        let hover = grid.slot_at(x + 1.0, y + 10.0).expect("inside");
        assert_eq!(hover, 3);
        let order = reordered(&ids(5), 1, hover);
        assert_eq!(order, vec!["c0", "c2", "c3", "c1", "c4"]);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(sorted, ids(5));
    }

    #[test]
    fn preview_placeholder_is_the_static_hover_slot() {
        let (placements, placeholder) = preview_slots(5, Some(1), Some(3));
        assert_eq!(placeholder, Some(3));
        let mut by_target = [None; 5];
        for (source, target) in placements {
            by_target[target] = Some(source);
        }
        // The lifted card is absent from every placement.
        assert!(by_target.iter().flatten().all(|source| *source != 1));
        assert_eq!(by_target[0], Some(0));
        assert_eq!(by_target[1], Some(2));
        assert_eq!(by_target[2], Some(3));
        assert_eq!(by_target[3], None);
        assert_eq!(by_target[4], Some(4));
    }

    #[test]
    fn reordered_is_complete_and_identity_when_unchanged() {
        let order = ids(4);
        assert_eq!(reordered(&order, 2, 2), order);
        let moved = reordered(&order, 3, 0);
        assert_eq!(moved, vec!["c3", "c0", "c1", "c2"]);
    }

    #[test]
    fn initials_come_from_the_first_two_words() {
        assert_eq!(initials("Ada Lovelace"), "AL");
        assert_eq!(initials("ada"), "A");
        assert_eq!(initials("  grace   brewster murray  "), "GB");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn search_matches_zone_or_resident() {
        let entries = [entry("a", "Europe/Kyiv", &["Ada Lovelace"])];
        assert!(matches_search(&entries[0], "kyiv"));
        assert!(matches_search(&entries[0], "europe"));
        assert!(matches_search(&entries[0], "ada"));
        assert!(matches_search(&entries[0], ""));
        assert!(!matches_search(&entries[0], "tokyo"));
    }

    #[test]
    fn picker_skips_added_zones() {
        let entries = vec![entry("a", "Europe/Kyiv", &[])];
        let rows = vec![
            "Europe/Kyiv".to_string(),
            "Europe/Lisbon".to_string(),
            "Europe/London".to_string(),
        ];
        let next = next_selectable(&entries, &rows, 0, true);
        assert_eq!(next, 1);
        let previous = next_selectable(&entries, &rows, 1, false);
        assert_eq!(previous, 2);
    }

    #[test]
    fn instant_falls_back_to_now_and_follows_the_probe() {
        let mut state = ClockState::new();
        let now = Timestamp::from_second(1_700_000_000).expect("valid");
        state.set_now(now);
        assert_eq!(state.instant(), now);
        let probe = Timestamp::from_second(1_600_000_000).expect("valid");
        state.probe = Some(ProbeState {
            instant: probe,
            ambiguity: ProbeAmbiguity::Unique,
            text: "03:00".into(),
        });
        assert_eq!(state.instant(), probe);
    }

    #[test]
    fn probe_zone_label_names_the_default_zone() {
        let mut state = ClockState::new();
        assert!(!state.probe_zone_label().is_empty());
        state.probe_zone = Some("Asia/Tokyo".into());
        assert_eq!(state.probe_zone_label(), "Tokyo");
    }

    #[test]
    fn a_probe_instant_moves_people_between_cities() {
        let person = Person {
            id: "p1".into(),
            name: "Ada".into(),
            home_clock_id: "home".into(),
            position: 0,
            created_at: String::new(),
        };
        let clock = |id: &str, people: Vec<Person>| ClockEntry {
            id: id.to_string(),
            tz_identifier: "UTC".into(),
            city: id.to_string(),
            zone: resolve_zone("UTC"),
            people,
        };
        let travels = vec![Travel {
            id: "t1".into(),
            person_id: "p1".into(),
            origin_clock_id: Some("home".into()),
            destination_clock_id: Some("away".into()),
            starts_at: 9 * 3600,
            ends_at: Some(17 * 3600),
            cancelled_at: None,
            created_at: String::new(),
        }];

        let at_ten = city_people(
            &[clock("home", vec![person.clone()]), clock("away", vec![])],
            &travels,
            &clock("away", vec![]),
            10 * 3600,
        );
        assert_eq!(at_ten.visitors.len(), 1);
        let home_at_ten = city_people(
            &[clock("home", vec![person.clone()]), clock("away", vec![])],
            &travels,
            &clock("home", vec![person.clone()]),
            10 * 3600,
        );
        assert_eq!(home_at_ten.residents[0].away_to.as_deref(), Some("away"));

        let at_eighteen = city_people(
            &[clock("home", vec![person.clone()]), clock("away", vec![])],
            &travels,
            &clock("away", vec![]),
            18 * 3600,
        );
        assert!(at_eighteen.visitors.is_empty());
        let home_at_eighteen = city_people(
            &[clock("home", vec![person.clone()]), clock("away", vec![])],
            &travels,
            &clock("home", vec![person]),
            18 * 3600,
        );
        assert!(home_at_eighteen.residents[0].away_to.is_none());
    }

    #[test]
    fn probe_field_text_follows_the_clock_preference() {
        let zone = resolve_zone("Asia/Tokyo").expect("known zone");
        let at: Timestamp = "2026-11-05T05:30:00Z".parse().expect("timestamp");
        assert_eq!(probe_field_text(at, &zone, true), "14:30");
        assert_eq!(probe_field_text(at, &zone, false), "2:30 pm");
    }

    #[test]
    fn probe_hint_reports_only_gap_and_fold() {
        use kandrium_core::{probe_date, read_probe_time};
        let zone = resolve_zone("America/New_York").expect("known zone");
        let now = Timestamp::from_second(1_772_000_000).expect("valid");
        let unique = read_probe_time("12:00", probe_date(now, &zone), &zone).expect("parse");
        assert_eq!(probe_hint(unique, &zone, true), None);

        let gap = read_probe_time("2:30", jiff::civil::date(2026, 3, 8), &zone).expect("parse");
        assert!(
            probe_hint(gap, &zone, true)
                .expect("hint")
                .contains("skipped hour")
        );
        let fold = read_probe_time("1:30", jiff::civil::date(2026, 11, 1), &zone).expect("parse");
        assert!(
            probe_hint(fold, &zone, false)
                .expect("hint")
                .contains("occurs twice")
        );
    }

    #[test]
    fn until_next_minute_lands_past_the_boundary() {
        let at = Timestamp::from_second(1_700_000_010).expect("valid");
        let wait = until_next_minute(at);
        // The instant sits 30 s past the minute; the wake adds the 50 ms grace.
        assert!(wait > Duration::from_secs(29));
        assert!(wait < Duration::from_secs(31));
    }

    #[test]
    fn until_next_travel_picks_the_nearest_future_boundary() {
        let now = 1_000;
        let travels = vec![
            Travel {
                id: "t1".into(),
                person_id: "p".into(),
                origin_clock_id: None,
                destination_clock_id: None,
                starts_at: now - 10,
                ends_at: Some(now + 30),
                cancelled_at: None,
                created_at: String::new(),
            },
            Travel {
                id: "t2".into(),
                person_id: "p".into(),
                origin_clock_id: None,
                destination_clock_id: None,
                starts_at: now + 20,
                ends_at: None,
                cancelled_at: None,
                created_at: String::new(),
            },
            Travel {
                id: "gone".into(),
                person_id: "p".into(),
                origin_clock_id: None,
                destination_clock_id: None,
                starts_at: now + 1,
                ends_at: None,
                cancelled_at: Some(now),
                created_at: String::new(),
            },
        ];
        assert_eq!(
            until_next_travel(&travels, now),
            Some(Duration::from_secs(20) + Duration::from_millis(50))
        );
    }
}
