//! Clocks view — world-clock page ported from Kwora.
//!
//! A single scrollable page of clock cards (Kwora's panel shape) with search,
//! a 12/24-hour toggle, drag-to-reorder, and per-card move/remove actions.
//! Cards keep Kwora's three-across width and wrap onto further rows — there
//! is no paging. Adding a city opens a searchable IANA picker backed by the
//! bundled `jiff` zone database. The inspector hosts each city's residents,
//! its visitors, and the temporary travel/relocation forms.
//!
//! Reorder uses the shared `dnd::GridZone` (row-major insertion index) plus
//! `anim`'s reflow/settle curves; the committed permutation goes through
//! `clocks::reorder_clocks` from `app.rs`.
//!
//! Repaint discipline: one `Timestamp::now()` per frame, then a single
//! `request_repaint_after` aimed just past the next minute boundary or the
//! nearest pending travel start/expiry, whichever comes first. The view
//! never reads or writes the database on that tick — rows, travel windows and
//! resolved `TimeZone` handles are cached in [`WorldClockState`], and every
//! mutation goes back through `kandrium-core` from `app.rs`. A trip expires
//! because placement is evaluated against the frame's `now`, not because the
//! tick queried the database.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{
    Align, Color32, CornerRadius, CursorIcon, Key, Layout, Margin, Rect, RichText, Sense, Stroke,
    TextStyle, Vec2, pos2,
};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use kandrium_core::clocks::{self, Clock, Person, Travel};
use rusqlite::Connection;

use crate::anim;
use crate::dnd;
use crate::theme::{self, Tokens};

/// Height reserved for one card so a row of columns aligns (time, meta, the
/// resident strip and an optional visiting strip).
const CARD_H: f32 = 156.0;
const CARD_GAP: f32 = 12.0;
/// Cards per row at the design width. Narrower windows keep three across and
/// shrink the card to a readable floor instead of re-flowing mid-drag.
const COLUMNS: usize = 3;
const CARD_HPAD: f32 = 14.0;
const CARD_VPAD: f32 = 12.0;
/// Kwora caps the add-city list at 200 matching rows.
const PICKER_MAX_ROWS: usize = 200;

/// A saved clock with its resolved zone handle, cached across frames.
#[derive(Clone)]
pub struct WorldClockEntry {
    pub id: String,
    pub tz_identifier: String,
    pub city: String,
    pub zone: Option<TimeZone>,
    /// Residents of this city, in display order.
    pub people: Vec<Person>,
}

impl WorldClockEntry {
    fn new(clock: Clock, people: Vec<Person>) -> Self {
        Self {
            city: clocks::city_name(&clock.tz_identifier),
            zone: clocks::resolve_zone(&clock.tz_identifier),
            id: clock.id,
            tz_identifier: clock.tz_identifier,
            people,
        }
    }
}

/// Searchable add-city dialog state.
#[derive(Default)]
struct ZonePicker {
    query: String,
    last_query: String,
    selected: usize,
    zones: Vec<String>,
}

/// Inline rename of one person inside the inspector.
struct RenameDraft {
    person_id: String,
    draft: String,
    /// Focus the field once, on the frame the rename starts.
    focus: bool,
}

/// Per-city people inspector (modal state).
#[derive(Default)]
struct Inspector {
    clock_id: String,
    add_draft: String,
    rename: Option<RenameDraft>,
}

/// What the move modal is editing.
enum MoveKind {
    /// A new temporary trip.
    Travel,
    /// Push (or set) the return time of an existing trip.
    Extend { travel_id: String },
    /// Change a person's resident city.
    Relocate,
}

/// Travel/relocate form state (modal).
struct MoveDraft {
    person_id: String,
    person_name: String,
    home_clock_id: String,
    home_city: String,
    /// Clock the person occupies now, excluded from the travel destination
    /// list.
    current_clock_id: Option<String>,
    kind: MoveKind,
    /// Selected destination clock (unused while extending).
    destination: Option<String>,
    start_date: String,
    end_date: String,
    /// No return date: the trip lasts until ended manually.
    open_ended: bool,
    error: Option<String>,
}

/// A clock card lifted by the pointer.
struct ClockDrag {
    clock_id: String,
    /// Pointer offset from the card's top-left at pickup, so the ghost keeps
    /// its grip point while the pointer moves.
    grab: Vec2,
    /// Card width captured at pickup; the ghost keeps the card size stable
    /// even if the grid re-flows underneath it.
    column_w: f32,
}

/// Ghost flight after release: back to the source slot for a cancelled drop,
/// or into the target slot for a committed reorder.
struct SnapBack {
    clock_id: String,
    from: Rect,
    to: Rect,
    start: f64,
    /// `true` when the drop was cancelled (snap-back curve, not reflow).
    cancelled: bool,
}

/// One card's FLIP glide between two slots.
#[derive(Clone, Copy)]
struct Motion {
    from: Rect,
    to: Rect,
    start: f64,
}

/// Slot-change animations for the card grid.
#[derive(Default)]
struct MotionSet {
    items: HashMap<String, Motion>,
}

impl MotionSet {
    /// Rect to draw this frame: glide from the previous slot to `target`.
    /// Retargeting starts from the current on-screen position so an
    /// interrupted glide never jumps.
    fn rect(&mut self, id: &str, target: Rect, time: f64, animate: bool) -> Rect {
        if !animate {
            self.items.remove(id);
            return target;
        }
        match self.items.get(id).copied() {
            Some(motion) if motion.to == target => {
                let progress = progress(motion.start, time);
                if progress >= 1.0 {
                    self.items.remove(id);
                    target
                } else {
                    lerp_rect(motion.from, motion.to, anim::ease_standard(progress))
                }
            }
            Some(motion) => {
                let current = lerp_rect(
                    motion.from,
                    motion.to,
                    anim::ease_standard(progress(motion.start, time)),
                );
                self.items.insert(
                    id.to_string(),
                    Motion {
                        from: current,
                        to: target,
                        start: time,
                    },
                );
                current
            }
            None => {
                self.items.insert(
                    id.to_string(),
                    Motion {
                        from: target,
                        to: target,
                        start: time,
                    },
                );
                target
            }
        }
    }

    /// True while at least one card is still gliding.
    fn animating(&self, time: f64) -> bool {
        self.items
            .values()
            .any(|motion| motion.to != motion.from && progress(motion.start, time) < 1.0)
    }

    fn clear(&mut self) {
        self.items.clear();
    }

    fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.items.retain(|id, _| keep(id));
    }
}

/// Elapsed fraction of a glide, clamped to `0..=1`.
fn progress(start: f64, time: f64) -> f32 {
    ((time - start) / anim::SHIFT_DURATION).clamp(0.0, 1.0) as f32
}

/// Interpolated rect between two slots.
fn lerp_rect(from: Rect, to: Rect, t: f32) -> Rect {
    anim::lerp_rect(from, to, t)
}

/// Row-major card slots for `count` cards at `width`.
struct GridLayout {
    slots: Vec<Rect>,
    column_w: f32,
    height: f32,
}

/// Lay out `count` cards in rows of [`COLUMNS`]. The height covers every
/// row, so the surrounding scroll area reserves space for all of them.
fn grid_layout(count: usize, width: f32) -> GridLayout {
    let column_w = ((width - CARD_GAP * (COLUMNS - 1) as f32) / COLUMNS as f32).max(160.0);
    let stride = CARD_H + CARD_GAP;
    let rows = count.div_ceil(COLUMNS);
    let height = if rows == 0 {
        0.0
    } else {
        rows as f32 * stride - CARD_GAP
    };
    let slots = (0..count)
        .map(|index| {
            let (row, column) = (index / COLUMNS, index % COLUMNS);
            Rect::from_min_size(
                pos2(column as f32 * (column_w + CARD_GAP), row as f32 * stride),
                Vec2::new(column_w, CARD_H),
            )
        })
        .collect();
    GridLayout {
        slots,
        column_w,
        height,
    }
}

/// Complete id order after moving `from` to `to` (remove, then insert).
fn reordered(ids: &[String], from: usize, to: usize) -> Vec<String> {
    let mut next = ids.to_vec();
    if from >= next.len() || to >= next.len() {
        return next;
    }
    let id = next.remove(from);
    next.insert(to, id);
    next
}

/// Final grid slot for a pointer over the static drop zone. `None` when the
/// pointer is outside it; positions past the last card collapse onto the
/// last slot, which is also the clamped `to` of the permutation.
fn grid_slot_at(zone: &dnd::GridZone, pos: egui::Pos2, count: usize) -> Option<usize> {
    zone.contains(pos)
        .then(|| zone.insertion_index(pos).min(count.saturating_sub(1)))
}

/// Preview layout for the grid: the target slot of every card that stays
/// visible, plus the slot left empty for the lifted (or settling) card.
///
/// `from` is the lifted card's index and `hover` its final slot in the
/// reloaded order, so `preview_slots` and [`reordered`] agree on where the
/// card lands. `snap_slot` hides the real card while its ghost flies home.
fn preview_slots(
    count: usize,
    from: Option<usize>,
    hover: Option<usize>,
    snap_slot: Option<usize>,
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
            None => {
                if Some(slot) == snap_slot {
                    placeholder = Some(slot);
                    continue;
                }
                placements.push((slot, slot));
            }
        }
    }
    (placements, placeholder)
}

/// One person as the rendered city sees them right now.
struct PersonStatus {
    person: Person,
    /// City this person is currently in when it is not the rendered city.
    away_to: Option<String>,
    /// End of the active trip (`None` with an active trip = open-ended).
    until: Option<i64>,
    /// The active trip, if any (used by the inspector actions).
    travel: Option<Travel>,
    /// The next planned trip that has not started yet, so it can still be
    /// extended or removed before it begins.
    upcoming: Option<Travel>,
}

/// Resident and visiting people for one city at one instant.
struct CityPeople {
    residents: Vec<PersonStatus>,
    visitors: Vec<PersonStatus>,
}

/// View state. Rows load on section entry and after every mutation; the
/// minute tick only reads this cache. Search, picker, inspector, and the
/// drag/reflow state are session-only.
#[derive(Default)]
pub struct WorldClockState {
    pub clocks: Vec<WorldClockEntry>,
    loaded: bool,
    search: String,
    last_search: String,
    picker: Option<ZonePicker>,
    inspector: Option<Inspector>,
    /// All travel rows, cached so placement and expiry need no DB access.
    travels: Vec<Travel>,
    move_draft: Option<MoveDraft>,
    /// Card currently held by the pointer.
    drag: Option<ClockDrag>,
    /// Ghost flight after release (committed landing or snap-back).
    snap: Option<SnapBack>,
    /// Last frame's drop geometry, in screen coordinates. Resolving the
    /// insertion slot against it (instead of this frame's layout) keeps the
    /// target stable while cards reflow underneath the pointer.
    zone: Option<dnd::GridZone>,
    /// FLIP glide for cards whose slot moved.
    motions: MotionSet,
}

impl WorldClockState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load rows the first time the section is shown.
    pub fn ensure_loaded(&mut self, db: &Arc<Mutex<Connection>>) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if let Ok(conn) = db.lock() {
            self.reload(&conn);
        }
    }

    /// Re-read rows, keeping the previous cache when the query fails.
    pub fn reload(&mut self, db: &Connection) {
        let Ok(rows) = clocks::list_clocks(db) else {
            return;
        };
        let people = clocks::list_all_people(db).unwrap_or_default();
        self.travels = clocks::list_all_travels(db).unwrap_or_default();
        self.clocks = rows
            .into_iter()
            .map(|clock| {
                let residents = people
                    .iter()
                    .filter(|person| person.home_clock_id == clock.id)
                    .cloned()
                    .collect();
                WorldClockEntry::new(clock, residents)
            })
            .collect();
        // A clock removed elsewhere takes its open inspector with it.
        if let Some(inspector) = &self.inspector
            && !self
                .clocks
                .iter()
                .any(|clock| clock.id == inspector.clock_id)
        {
            self.inspector = None;
        }
        // Likewise for a person removed elsewhere while a move form was open.
        if let Some(draft) = &self.move_draft
            && !self.clocks.iter().any(|entry| {
                entry
                    .people
                    .iter()
                    .any(|person| person.id == draft.person_id)
            })
        {
            self.move_draft = None;
        }
        // Drop drag/reflow state for clocks that vanished elsewhere.
        let known = |id: &str| self.clocks.iter().any(|clock| clock.id == id);
        if let Some(drag) = &self.drag
            && !known(&drag.clock_id)
        {
            self.drag = None;
        }
        if let Some(snap) = &self.snap
            && !known(&snap.clock_id)
        {
            self.snap = None;
        }
        self.motions.retain(known);
    }

    fn open_picker(&mut self) {
        if self.picker.is_some() {
            return;
        }
        self.picker = Some(ZonePicker {
            zones: clocks::available_zones(),
            ..Default::default()
        });
    }

    fn open_inspector(&mut self, clock_id: &str) {
        self.inspector = Some(Inspector {
            clock_id: clock_id.to_string(),
            ..Default::default()
        });
    }
}

/// A travel form submission resolved to epoch seconds by the UI.
#[derive(Clone, Debug)]
pub struct TravelRequest {
    pub person_id: String,
    pub destination_clock_id: String,
    pub starts_at: i64,
    pub ends_at: Option<i64>,
}

/// UI intents for the caller to execute through `kandrium-core`.
#[derive(Default)]
pub struct Actions {
    /// New 12/24-hour preference when the toggle was clicked.
    pub set_use_24h: Option<bool>,
    /// IANA identifier chosen in the add-city picker.
    pub add_zone: Option<String>,
    /// Complete id order after a move earlier/later action.
    pub reorder_ids: Option<Vec<String>>,
    /// Clock id + city label for the remove confirmation.
    pub remove_clock: Option<(String, String)>,
    /// Clock id + trimmed name from the inspector's add field.
    pub add_person: Option<(String, String)>,
    /// Person id + new name from the inspector's inline rename.
    pub rename_person: Option<(String, String)>,
    /// Person id + name for the remove confirmation.
    pub remove_person: Option<(String, String)>,
    /// A resolved new trip from the move modal.
    pub start_travel: Option<TravelRequest>,
    /// Travel id to end now.
    pub end_travel: Option<String>,
    /// Travel id + new end (`None` = open-ended) from the extend form.
    pub extend_travel: Option<(String, Option<i64>)>,
    /// Travel id + person name for the delete confirmation.
    pub remove_travel: Option<(String, String)>,
    /// Person id + new home clock id from the relocate form.
    pub relocate_person: Option<(String, String)>,
}

/// Draw the section body. Returns the actions for the caller to apply.
pub fn show(ui: &mut egui::Ui, state: &mut WorldClockState, use_24h: bool) -> Actions {
    let now = Timestamp::now();
    let mut wait = until_next_minute(now);
    // Expire/start a trip on time without polling: only the nearest pending
    // boundary is scheduled, and only while this section is being drawn.
    if let Some(travel_wait) = until_next_travel(&state.travels, now.as_second()) {
        wait = wait.min(travel_wait);
    }
    ui.ctx().request_repaint_after(wait);

    let t = Tokens::get(ui.ctx());
    let mut actions = Actions::default();

    if state.search != state.last_search {
        state.last_search = state.search.clone();
        state.motions.clear();
        // Dragging is only offered on the unfiltered order, so editing the
        // search ends any drag in progress.
        state.drag = None;
        state.snap = None;
    }

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        theme::search_field(ui, &mut state.search, "Search cities or time zones…", 280.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme::pill_button(ui, "Add city", true).clicked() {
                state.open_picker();
            }

            ui.add_space(8.0);
            // Right-to-left: "24h" ends up right of "12h".
            if theme::tab_button(
                ui,
                "24h",
                use_24h,
                TextStyle::Small,
                theme::small_strong(),
                24.0,
            )
            .clicked()
            {
                actions.set_use_24h = Some(true);
            }
            if theme::tab_button(
                ui,
                "12h",
                !use_24h,
                TextStyle::Small,
                theme::small_strong(),
                24.0,
            )
            .clicked()
            {
                actions.set_use_24h = Some(false);
            }

            let filtered: Vec<usize> = state
                .clocks
                .iter()
                .enumerate()
                .filter(|(_, entry)| matches_search(entry, &state.search))
                .map(|(index, _)| index)
                .collect();
            let total = state.clocks.len();
            let label = if state.search.trim().is_empty() {
                format!("{} {}", total, if total == 1 { "CITY" } else { "CITIES" })
            } else {
                format!(
                    "{} OF {} {}",
                    filtered.len(),
                    total,
                    if total == 1 { "CITY" } else { "CITIES" }
                )
            };
            ui.add_space(10.0);
            ui.label(
                RichText::new(label)
                    .text_style(theme::small_strong())
                    .color(t.text_faint),
            );
        });
    });
    ui.add_space(8.0);

    let filtered: Vec<usize> = state
        .clocks
        .iter()
        .enumerate()
        .filter(|(_, entry)| matches_search(entry, &state.search))
        .map(|(index, _)| index)
        .collect();

    let mut empty_add_clicked = false;
    let mut clear_search = false;
    let mut inspect_clock: Option<String> = None;
    let animate = ui.ctx().global_style().animation_time > 0.0;
    let time = ui.input(|i| i.time);
    let pointer = ui.ctx().pointer_latest_pos();
    // Dragging reorders the stored list, so it is only offered while every
    // city is visible (a filtered grid cannot map slots to the full order).
    let draggable = state.search.trim().is_empty();

    // A drag only lives while this section is drawn with the button held.
    if state.drag.is_some()
        && !ui.ctx().input(|i| i.pointer.any_down())
        && !ui.ctx().input(|i| i.pointer.any_released())
    {
        state.drag = None;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .content_margin(Margin {
            left: 0,
            right: crate::widgets::CONTENT_RIGHT_INSET as i8,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if state.clocks.is_empty() {
                empty_add_clicked = empty_state(ui, &t);
                return;
            }
            if filtered.is_empty() {
                clear_search = no_results_state(ui, &t, &state.search);
                return;
            }

            let count = filtered.len();
            let width = ui.available_width();
            let layout = grid_layout(count, width);
            let origin = ui.cursor().min;
            ui.allocate_space(Vec2::new(width, layout.height));

            // Drop geometry comes from the laid-out slots — one per grid
            // position, including the lifted card's own. Resolving against
            // the reflowed preview let a row disappear from the zone once the
            // gap was its only occupant, which flipped the target by a full
            // row (and could oscillate) while the pointer was still there.
            // Static slots also keep the row bands fixed under the pointer.
            // The rect is clipped to the viewport, so rows scrolled out of
            // sight are not valid drop targets.
            let zone = draggable.then(|| dnd::GridZone {
                rect: Rect::from_min_size(origin, Vec2::new(width, layout.height.max(CARD_H)))
                    .intersect(ui.clip_rect()),
                slots: (0..count)
                    .map(|slot| {
                        (
                            state.clocks[filtered[slot]].id.clone(),
                            layout.slots[slot].translate(origin.to_vec2()),
                        )
                    })
                    .collect(),
            });
            state.zone = zone;

            let drag_id = state.drag.as_ref().map(|drag| drag.clock_id.clone());
            let dragged = drag_id.as_ref().and_then(|id| {
                filtered
                    .iter()
                    .position(|&index| state.clocks[index].id == *id)
            });
            // A static index is already the card's final slot after the
            // remove-then-insert permutation, so it is used as the hover
            // index directly; positions past the last card land on it.
            let hover = if draggable && drag_id.is_some() {
                state
                    .zone
                    .as_ref()
                    .and_then(|zone| pointer.and_then(|pos| grid_slot_at(zone, pos, count)))
            } else {
                None
            };
            // Reflow glides belong to drag lifecycle motion only: scrolling
            // or resizing must not leave cards trailing the viewport.
            let animate_cards = animate && (drag_id.is_some() || state.snap.is_some());

            let snap_slot = if drag_id.is_none() {
                state.snap.as_ref().and_then(|snap| {
                    filtered
                        .iter()
                        .position(|&index| state.clocks[index].id == snap.clock_id)
                })
            } else {
                None
            };

            // Target slot per still-visible card, plus the empty slot left by
            // the lifted card (or opened at the drop index).
            let (placements, placeholder) = preview_slots(count, dragged, hover, snap_slot);
            if let Some(slot) = placeholder {
                paint_drop_slot(ui, layout.slots[slot].translate(origin.to_vec2()), &t);
            }

            for (visible_slot, slot) in placements {
                let full_index = filtered[visible_slot];
                let entry = state.clocks[full_index].clone();
                let city = city_people(&state.clocks, &state.travels, &entry, now.as_second());
                let target = layout.slots[slot].translate(origin.to_vec2());
                let rect = state.motions.rect(&entry.id, target, time, animate_cards);
                let hit = ui
                    .interact(
                        rect,
                        ui.id().with(("clock-card-drag", entry.id.as_str())),
                        Sense::click_and_drag(),
                    )
                    .on_hover_cursor(CursorIcon::Grab);
                if draggable
                    && state.drag.is_none()
                    && hit.drag_started()
                    && let Some(pos) = pointer
                {
                    state.drag = Some(ClockDrag {
                        clock_id: entry.id.clone(),
                        grab: pos - rect.min,
                        column_w: layout.column_w,
                    });
                    state.snap = None;
                }

                let inspector_open = state
                    .inspector
                    .as_ref()
                    .is_some_and(|inspector| inspector.clock_id == entry.id);
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::top_down(Align::LEFT)),
                    |ui| {
                        ui.push_id(("clock-card", entry.id.as_str()), |ui| {
                            let card_action = clock_card(
                                ui,
                                &entry,
                                &city,
                                now,
                                use_24h,
                                layout.column_w,
                                full_index > 0,
                                full_index + 1 < state.clocks.len(),
                                inspector_open,
                            );
                            match card_action {
                                Some(CardAction::MoveEarlier) => {
                                    let mut ids: Vec<String> =
                                        state.clocks.iter().map(|clock| clock.id.clone()).collect();
                                    ids.swap(full_index, full_index - 1);
                                    actions.reorder_ids = Some(ids);
                                }
                                Some(CardAction::MoveLater) => {
                                    let mut ids: Vec<String> =
                                        state.clocks.iter().map(|clock| clock.id.clone()).collect();
                                    ids.swap(full_index, full_index + 1);
                                    actions.reorder_ids = Some(ids);
                                }
                                Some(CardAction::Remove) => {
                                    actions.remove_clock =
                                        Some((entry.id.clone(), entry.city.clone()));
                                }
                                Some(CardAction::Inspect) => {
                                    inspect_clock = Some(entry.id.clone());
                                }
                                None => {}
                            }
                        });
                    },
                );
            }

            // Release: commit the permutation, or fly the ghost home.
            if ui.ctx().input(|i| i.pointer.any_released())
                && let Some(drag) = state.drag.take()
            {
                let from = filtered
                    .iter()
                    .position(|&index| state.clocks[index].id == drag.clock_id);
                let ghost = pointer.map(|pos| {
                    Rect::from_min_size(pos - drag.grab, Vec2::new(drag.column_w, CARD_H))
                });
                let commit = matches!((from, hover), (Some(from), Some(hover)) if hover != from);
                let slot = if commit { hover } else { from };
                if let Some(slot) = slot {
                    let to = layout.slots[slot].translate(origin.to_vec2());
                    if commit {
                        let order: Vec<String> =
                            state.clocks.iter().map(|clock| clock.id.clone()).collect();
                        actions.reorder_ids = Some(reordered(&order, from.unwrap_or(slot), slot));
                    }
                    if animate {
                        state.snap = Some(SnapBack {
                            clock_id: drag.clock_id,
                            from: ghost.unwrap_or(to),
                            to,
                            start: time,
                            cancelled: !commit,
                        });
                    }
                }
            }
        });

    if empty_add_clicked {
        state.open_picker();
    }
    if clear_search {
        state.search.clear();
        state.last_search.clear();
    }
    if let Some(clock_id) = inspect_clock {
        state.open_inspector(&clock_id);
    }

    let ghost_animating = drag_ghost(ui.ctx(), state, now, use_24h);
    if ghost_animating || state.motions.animating(ui.input(|i| i.time)) {
        ui.ctx().request_repaint();
    }

    let outcome = picker_modal(ui.ctx(), state, now);
    match outcome {
        PickerOutcome::Choose(zone) => {
            state.picker = None;
            actions.add_zone = Some(zone);
        }
        PickerOutcome::Close => state.picker = None,
        PickerOutcome::Stay => {}
    }

    // Only one modal is drawn per frame. A move form opened from the
    // inspector therefore takes over on the following frame.
    let move_open = state.move_draft.is_some();
    let inspector_outcome = if move_open {
        InspectorOutcome::Stay
    } else {
        let WorldClockState {
            clocks,
            travels,
            inspector,
            ..
        } = state;
        match inspector.as_mut() {
            Some(open) => match clocks.iter().find(|entry| entry.id == open.clock_id) {
                Some(entry) => {
                    inspector_modal(ui.ctx(), open, entry, clocks, travels, now.as_second())
                }
                None => InspectorOutcome::Close,
            },
            None => InspectorOutcome::Stay,
        }
    };
    match inspector_outcome {
        InspectorOutcome::Stay => {}
        InspectorOutcome::Close => state.inspector = None,
        InspectorOutcome::AddPerson(clock_id, name) => {
            actions.add_person = Some((clock_id, name));
        }
        InspectorOutcome::RenamePerson(id, name) => {
            actions.rename_person = Some((id, name));
        }
        InspectorOutcome::RemovePerson(id, name) => {
            actions.remove_person = Some((id, name));
        }
        InspectorOutcome::StartTravel(person_id) => {
            state.move_draft = start_travel_draft(&state.clocks, &state.travels, &person_id, now);
        }
        InspectorOutcome::Relocate(person_id) => {
            state.move_draft = relocate_draft(&state.clocks, &state.travels, &person_id, now);
        }
        InspectorOutcome::EndTravel(travel_id) => {
            actions.end_travel = Some(travel_id);
        }
        InspectorOutcome::ExtendTravel(travel_id) => {
            state.move_draft = extend_travel_draft(&state.clocks, &state.travels, &travel_id, now);
        }
        InspectorOutcome::RemoveTravel(travel_id, name) => {
            actions.remove_travel = Some((travel_id, name));
        }
    }

    if move_open {
        let outcome = {
            let WorldClockState {
                clocks, move_draft, ..
            } = state;
            match move_draft.as_mut() {
                Some(draft) => move_modal(ui.ctx(), draft, clocks),
                None => MoveOutcome::Stay,
            }
        };
        match outcome {
            MoveOutcome::Stay => {}
            MoveOutcome::Close => state.move_draft = None,
            MoveOutcome::Save(submission) => {
                match submission {
                    MoveSubmission::Travel(request) => actions.start_travel = Some(request),
                    MoveSubmission::Extend { travel_id, ends_at } => {
                        actions.extend_travel = Some((travel_id, ends_at));
                    }
                    MoveSubmission::Relocate {
                        person_id,
                        clock_id,
                    } => actions.relocate_person = Some((person_id, clock_id)),
                }
                state.move_draft = None;
            }
        }
    }

    actions
}

/// Search matches the zone text (city, region, identifier) or any resident's
/// name.
fn matches_search(entry: &WorldClockEntry, query: &str) -> bool {
    if clocks::zone_matches(&entry.tz_identifier, query) {
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

enum CardAction {
    MoveEarlier,
    MoveLater,
    Remove,
    Inspect,
}

#[allow(clippy::too_many_arguments)]
fn clock_card(
    ui: &mut egui::Ui,
    entry: &WorldClockEntry,
    city: &CityPeople,
    now: Timestamp,
    use_24h: bool,
    column_w: f32,
    can_move_earlier: bool,
    can_move_later: bool,
    inspector_open: bool,
) -> Option<CardAction> {
    let t = Tokens::get(ui.ctx());
    let mut action = None;
    let frame = egui::Frame::new()
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(t.radius))
        .inner_margin(Margin::symmetric(CARD_HPAD as i8, CARD_VPAD as i8));
    frame.show(ui, |ui| {
        ui.set_min_width(column_w - CARD_HPAD * 2.0);
        ui.set_min_height(CARD_H - CARD_VPAD * 2.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&entry.city)
                    .text_style(theme::small_strong())
                    .color(t.text),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let menu = theme::icon_button(ui, "ellipsis", 22.0, false, "Clock menu");
                let _ = egui::Popup::menu(&menu)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show(|ui| {
                        if can_move_earlier
                            && theme::menu_item(ui, "chevron-left", "Move earlier", false).clicked()
                        {
                            action = Some(CardAction::MoveEarlier);
                        }
                        if can_move_later
                            && theme::menu_item(ui, "chevron-right", "Move later", false).clicked()
                        {
                            action = Some(CardAction::MoveLater);
                        }
                        if city.residents.is_empty() {
                            if theme::menu_item(ui, "trash-2", "Remove clock", true).clicked() {
                                action = Some(CardAction::Remove);
                            }
                        } else {
                            // The city keeps its residents (ON DELETE RESTRICT);
                            // deleting them first is the only way out.
                            ui.add_enabled_ui(false, |ui| {
                                let _ = theme::menu_item(ui, "trash-2", "Remove clock", true);
                            });
                            ui.label(
                                RichText::new("Remove the people first.")
                                    .text_style(TextStyle::Small)
                                    .color(t.text_faint),
                            );
                        }
                    });
                ui.add_space(2.0);
                if theme::icon_button(ui, "users", 22.0, inspector_open, "People").clicked() {
                    action = Some(CardAction::Inspect);
                }
            });
        });
        ui.add_space(4.0);

        let Some(zone) = &entry.zone else {
            ui.label(
                RichText::new("Unknown time zone")
                    .text_style(TextStyle::Small)
                    .color(t.text_faint),
            );
            return;
        };

        let text = clocks::format_clock(now, zone, use_24h);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&text.time)
                    .text_style(theme::clock_time())
                    .color(t.text),
            );
            if let Some(period) = &text.period {
                ui.add_space(2.0);
                ui.label(
                    RichText::new(period)
                        .text_style(TextStyle::Small)
                        .color(t.text_muted),
                );
            }
        });
        ui.add_space(2.0);
        ui.label(
            RichText::new(format!("{}   {}", text.date, text.offset))
                .text_style(TextStyle::Small)
                .color(t.text_faint),
        );
        ui.add_space(6.0);
        people_strips(ui, city, &t);
    });
    action
}

/// Avatar diameter on a clock card and in the inspector.
const AVATAR_D: f32 = 24.0;

/// Residents first, then a labelled visitor strip when someone is in town
/// from another city.
fn people_strips(ui: &mut egui::Ui, city: &CityPeople, t: &Tokens) {
    if !city.residents.is_empty() {
        avatar_strip(ui, &city.residents, t, None);
    }
    if !city.visitors.is_empty() {
        avatar_strip(ui, &city.visitors, t, Some("Visiting"));
    }
}

/// Up to two initials chips plus a `+N` remainder. A person travelling away
/// from their home chip gets the accent ring; visitors are always ringed and
/// sit under their own label.
fn avatar_strip(ui: &mut egui::Ui, people: &[PersonStatus], t: &Tokens, label: Option<&str>) {
    if people.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        if let Some(label) = label {
            ui.label(
                RichText::new(label.to_uppercase())
                    .text_style(TextStyle::Small)
                    .color(t.text_faint),
            );
            ui.add_space(5.0);
        }
        for (index, status) in people.iter().take(2).enumerate() {
            if index > 0 {
                ui.add_space(4.0);
            }
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(AVATAR_D), Sense::hover());
            let marked = label.is_some() || status.away_to.is_some();
            paint_avatar(
                ui,
                rect,
                &initials(&status.person.name),
                t,
                marked.then_some(t.accent),
            );
            let hover = match (&status.away_to, label.is_some()) {
                (Some(city), _) => format!("{} · away in {city}", status.person.name),
                (None, true) => format!("{} · visiting", status.person.name),
                (None, false) => status.person.name.clone(),
            };
            let _ = response.on_hover_text(hover);
        }
        if people.len() > 2 {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("+{}", people.len() - 2))
                    .text_style(TextStyle::Small)
                    .color(t.text_faint),
            );
        }
    });
}

/// Neutral initial chip: first letter of the first two name words. `ring`
/// paints the accent ring used for travelling/visiting people.
fn paint_avatar(
    ui: &egui::Ui,
    rect: egui::Rect,
    text: &str,
    t: &Tokens,
    ring: Option<egui::Color32>,
) {
    let painter = ui.painter();
    let radius = rect.width() * 0.5;
    painter.circle_filled(rect.center(), radius, t.field);
    painter.circle_stroke(
        rect.center(),
        radius,
        Stroke::new(1.0, ring.unwrap_or(t.border)),
    );
    let galley = painter.layout_no_wrap(
        text.to_string(),
        theme::font_id(ui, theme::small_strong()),
        t.text_muted,
    );
    painter.galley(rect.center() - galley.size() * 0.5, galley, t.text_muted);
}

/// Resident and visiting status for one city at `instant`. Pure over the
/// cached rows: an expired trip simply stops matching, so the card and
/// inspector fall back to home without a reload.
fn city_people(
    clocks: &[WorldClockEntry],
    travels: &[Travel],
    entry: &WorldClockEntry,
    instant: i64,
) -> CityPeople {
    let mut residents = Vec::with_capacity(entry.people.len());
    for person in &entry.people {
        let active = clocks::placement_for(travels, &person.id, instant);
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
            let Some(travel) = clocks::placement_for(travels, &person.id, instant) else {
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

fn city_label(clocks: &[WorldClockEntry], clock_id: &str) -> String {
    clocks
        .iter()
        .find(|entry| entry.id == clock_id)
        .map(|entry| entry.city.clone())
        .unwrap_or_else(|| clock_id.to_string())
}

/// "until Sat, 7 Nov. 23:59" in the relevant city, or "no return date".
fn until_label(zone: Option<&TimeZone>, ends_at: Option<i64>) -> String {
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
fn starts_label(zone: Option<&TimeZone>, starts_at: i64) -> String {
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

fn initials(name: &str) -> String {
    name.split_whitespace()
        .take(2)
        .filter_map(|word| word.chars().next())
        .flat_map(char::to_uppercase)
        .collect()
}

fn empty_state(ui: &mut egui::Ui, t: &Tokens) -> bool {
    let mut add_clicked = false;
    let frame = egui::Frame::new()
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(t.radius))
        .inner_margin(Margin::symmetric(16, 18));
    frame.show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.label(
            RichText::new("No cities yet.")
                .text_style(TextStyle::Body)
                .color(t.text_muted),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new("Add your first city to see its local time here.")
                .text_style(TextStyle::Small)
                .color(t.text_faint),
        );
        ui.add_space(10.0);
        if theme::pill_button(ui, "Add a city", true).clicked() {
            add_clicked = true;
        }
    });
    add_clicked
}

fn no_results_state(ui: &mut egui::Ui, t: &Tokens, query: &str) -> bool {
    let mut clear_clicked = false;
    let frame = egui::Frame::new()
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(t.radius))
        .inner_margin(Margin::symmetric(16, 18));
    frame.show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.label(
            RichText::new(format!("No cities match “{}”.", query.trim()))
                .text_style(TextStyle::Body)
                .color(t.text_muted),
        );
        ui.add_space(10.0);
        if theme::pill_button(ui, "Clear search", false).clicked() {
            clear_clicked = true;
        }
    });
    clear_clicked
}

/// Empty slot left by the lifted card, or opened at the drop target.
fn paint_drop_slot(ui: &egui::Ui, rect: Rect, t: &Tokens) {
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(t.radius), t.accent_soft);
    painter.rect_stroke(
        rect,
        CornerRadius::same(t.radius),
        Stroke::new(1.0, t.accent),
        egui::StrokeKind::Inside,
    );
}

/// Draw the card following the pointer, or its settle flight after release.
/// Returns `true` while a flight is still running (drives a repaint).
fn drag_ghost(
    ctx: &egui::Context,
    state: &mut WorldClockState,
    now: Timestamp,
    use_24h: bool,
) -> bool {
    let animate = ctx.global_style().animation_time > 0.0;
    if !animate {
        state.snap = None;
    }
    let time = ctx.input(|i| i.time);

    let dragging = state
        .drag
        .as_ref()
        .map(|drag| (drag.clock_id.clone(), drag.grab, drag.column_w));
    let snapping = state.snap.as_ref().map(|snap| {
        (
            snap.clock_id.clone(),
            snap.from,
            snap.to,
            snap.start,
            snap.cancelled,
        )
    });

    let (clock_id, column_w, rect) = if let Some((clock_id, grab, column_w)) = dragging {
        let Some(pos) = ctx.pointer_latest_pos() else {
            state.drag = None;
            return false;
        };
        (
            clock_id,
            column_w,
            Rect::from_min_size(pos - grab, Vec2::new(column_w, CARD_H)),
        )
    } else if let Some((clock_id, from, to, start, cancelled)) = snapping {
        let elapsed = (time - start) / anim::SETTLE_DURATION;
        let eased = if cancelled {
            anim::ease_snap(elapsed.clamp(0.0, 1.0) as f32)
        } else {
            anim::ease_standard(elapsed.clamp(0.0, 1.0) as f32)
        };
        let rect = anim::lerp_rect(from, to, eased);
        if elapsed >= 1.0 {
            state.snap = None;
        }
        (clock_id, from.width(), rect)
    } else {
        return false;
    };

    let Some(entry) = state.clocks.iter().find(|entry| entry.id == clock_id) else {
        state.drag = None;
        state.snap = None;
        return false;
    };
    let city = city_people(&state.clocks, &state.travels, entry, now.as_second());
    let shadow = egui::epaint::Shadow {
        offset: [0, anim::SHADOW_OFFSET_LIFT as i8],
        blur: anim::SHADOW_BLUR_LIFT as u8,
        spread: 0,
        color: Color32::from_black_alpha((anim::SHADOW_ALPHA_LIFT * 255.0) as u8),
    };
    egui::Area::new(egui::Id::new("clocks-drag-ghost"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_width(column_w);
            ui.set_opacity(anim::DRAG_OPACITY);
            egui::Frame::new().shadow(shadow).show(ui, |ui| {
                let _ = clock_card(
                    ui, entry, &city, now, use_24h, column_w, false, false, false,
                );
            });
        });
    state.drag.is_some() || state.snap.is_some()
}

enum InspectorOutcome {
    Stay,
    Close,
    /// `(clock id, trimmed name)` from the add field.
    AddPerson(String, String),
    /// `(person id, trimmed name)` from the inline rename.
    RenamePerson(String, String),
    /// `(person id, name)` for the remove confirmation.
    RemovePerson(String, String),
    /// Person id whose travel form should open.
    StartTravel(String),
    /// Person id whose relocation form should open.
    Relocate(String),
    /// Travel id to end now.
    EndTravel(String),
    /// Travel id whose extend form should open.
    ExtendTravel(String),
    /// Travel id + person name for the delete confirmation.
    RemoveTravel(String, String),
}

/// People inspector for one city: its residents (with their current
/// placement and active trip) and its visitors. Enter commits (add or
/// rename), Esc closes the modal; Esc in an open rename cancels that rename
/// instead.
fn inspector_modal(
    ctx: &egui::Context,
    inspector: &mut Inspector,
    entry: &WorldClockEntry,
    clocks: &[WorldClockEntry],
    travels: &[Travel],
    instant: i64,
) -> InspectorOutcome {
    let t = Tokens::get(ctx);
    let city = city_people(clocks, travels, entry, instant);
    let mut outcome = InspectorOutcome::Stay;
    let modal = egui::Modal::new(egui::Id::new("clocks-inspector")).frame(theme::dialog_frame(ctx));
    let response = modal.show(ctx, |ui| {
        ui.set_min_width(460.0);
        ui.heading(&entry.city);
        ui.label(
            RichText::new(&entry.tz_identifier)
                .text_style(TextStyle::Small)
                .color(t.text_faint),
        );
        ui.add_space(8.0);

        egui::ScrollArea::vertical()
            .max_height(320.0)
            .auto_shrink([false, true])
            .content_margin(Margin {
                left: 0,
                right: crate::widgets::CONTENT_RIGHT_INSET as i8,
                top: 0,
                bottom: 0,
            })
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!("RESIDENTS · {}", city.residents.len()))
                        .text_style(theme::small_strong())
                        .color(t.text_faint),
                );
                ui.add_space(4.0);
                if city.residents.is_empty() {
                    ui.label(
                        RichText::new("No residents yet.")
                            .text_style(TextStyle::Body)
                            .color(t.text_muted),
                    );
                    ui.label(
                        RichText::new("Add someone and their initials will show on this clock.")
                            .text_style(TextStyle::Small)
                            .color(t.text_faint),
                    );
                } else {
                    for status in &city.residents {
                        resident_row(ui, status, clocks, inspector, &t, &mut outcome);
                    }
                }

                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!("VISITORS · {}", city.visitors.len()))
                        .text_style(theme::small_strong())
                        .color(t.text_faint),
                );
                ui.add_space(4.0);
                if city.visitors.is_empty() {
                    ui.label(
                        RichText::new("Nobody is visiting right now.")
                            .text_style(TextStyle::Small)
                            .color(t.text_faint),
                    );
                } else {
                    for status in &city.visitors {
                        visitor_row(ui, status, entry, &t, &mut outcome);
                    }
                }
            });

        ui.add_space(10.0);
        let mut submitted = false;
        let mut field_lost_focus = false;
        ui.horizontal(|ui| {
            let add_width = (ui.available_width() - 84.0).max(160.0);
            let field = egui::Frame::new()
                .fill(t.field)
                .stroke(Stroke::new(1.0, t.border))
                .corner_radius(CornerRadius::same(t.radius))
                .inner_margin(Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.add_sized(
                        [add_width - 16.0, 20.0],
                        egui::TextEdit::singleline(&mut inspector.add_draft)
                            .frame(egui::Frame::NONE)
                            .hint_text("Add a resident"),
                    )
                })
                .inner;
            field_lost_focus =
                field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            if theme::pill_button(ui, "Add", true).clicked() {
                submitted = true;
            }
        });
        if field_lost_focus || submitted {
            let name = inspector.add_draft.trim().to_string();
            if !name.is_empty() {
                outcome = InspectorOutcome::AddPerson(entry.id.clone(), name);
                inspector.add_draft.clear();
            }
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Enter adds · Esc closes · plane travels · house relocates")
                    .text_style(TextStyle::Small)
                    .color(t.text_faint),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::pill_button(ui, "Done", false).clicked() {
                    outcome = InspectorOutcome::Close;
                }
            });
        });
    });

    match outcome {
        InspectorOutcome::Stay if response.should_close() => InspectorOutcome::Close,
        other => other,
    }
}

fn resident_row(
    ui: &mut egui::Ui,
    status: &PersonStatus,
    clocks: &[WorldClockEntry],
    inspector: &mut Inspector,
    t: &Tokens,
    outcome: &mut InspectorOutcome,
) {
    let person = &status.person;
    let renaming = inspector
        .rename
        .as_ref()
        .is_some_and(|draft| draft.person_id == person.id);

    if renaming {
        // Esc cancels the rename only. Consume the key so the modal does not
        // read it as its own close request.
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            inspector.rename = None;
            return;
        }

        let mut commit = false;
        {
            let rename = inspector.rename.as_mut().expect("rename is open");
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(AVATAR_D), Sense::hover());
                paint_avatar(ui, rect, &initials(&person.name), t, None);
                ui.add_space(6.0);
                let field = ui.add_sized(
                    [(ui.available_width() - 78.0).max(120.0), 22.0],
                    egui::TextEdit::singleline(&mut rename.draft)
                        .id(egui::Id::new(("person-rename", person.id.as_str()))),
                );
                if rename.focus {
                    field.request_focus();
                    rename.focus = false;
                }
                commit = (field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)))
                    || theme::pill_button(ui, "Save", true).clicked();
            });
        }
        if commit {
            let name = inspector
                .rename
                .as_ref()
                .map(|draft| draft.draft.trim().to_string())
                .unwrap_or_default();
            if !name.is_empty() {
                *outcome = InspectorOutcome::RenamePerson(person.id.clone(), name);
            }
            inspector.rename = None;
        }
        return;
    }

    ui.horizontal(|ui| {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(AVATAR_D), Sense::hover());
        let ring = status.away_to.as_ref().map(|_| t.accent);
        paint_avatar(ui, rect, &initials(&person.name), t, ring);
        let _ = response.on_hover_text(&person.name);
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.label(
                RichText::new(&person.name)
                    .text_style(TextStyle::Body)
                    .color(t.text),
            );
            let place = match &status.away_to {
                Some(city) => format!(
                    "Away in {city} · {}",
                    until_label(travel_zone(clocks, status.travel.as_ref()), status.until)
                ),
                None => match &status.upcoming {
                    Some(travel) => {
                        let destination = travel
                            .destination_clock_id
                            .as_deref()
                            .map(|id| city_label(clocks, id))
                            .unwrap_or_else(|| "another city".to_string());
                        format!(
                            "Leaving for {destination} · {}",
                            starts_label(travel_zone(clocks, Some(travel)), travel.starts_at)
                        )
                    }
                    None => "Here now".to_string(),
                },
            };
            ui.label(
                RichText::new(place)
                    .text_style(TextStyle::Small)
                    .color(t.text_faint),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme::icon_button(ui, "trash-2", 22.0, true, "Remove person").clicked() {
                *outcome = InspectorOutcome::RemovePerson(person.id.clone(), person.name.clone());
            }
            if theme::icon_button(ui, "house", 22.0, false, "Change home city").clicked() {
                *outcome = InspectorOutcome::Relocate(person.id.clone());
            }
            if theme::icon_button(ui, "plane", 22.0, status.travel.is_some(), "Travel").clicked() {
                *outcome = InspectorOutcome::StartTravel(person.id.clone());
            }
            if theme::icon_button(ui, "pencil", 22.0, false, "Rename person").clicked() {
                inspector.rename = Some(RenameDraft {
                    person_id: person.id.clone(),
                    draft: person.name.clone(),
                    focus: true,
                });
            }
        });
    });

    if let Some(travel) = &status.travel {
        trip_actions(ui, travel, person, clocks, t, outcome, true);
    } else if let Some(travel) = &status.upcoming {
        trip_actions(ui, travel, person, clocks, t, outcome, false);
    }
    ui.add_space(2.0);
}

/// One line for a person's trip with its management actions. A running trip
/// can be ended now; a planned trip can only be extended or deleted.
fn trip_actions(
    ui: &mut egui::Ui,
    travel: &Travel,
    person: &Person,
    clocks: &[WorldClockEntry],
    t: &Tokens,
    outcome: &mut InspectorOutcome,
    running: bool,
) {
    let destination = travel
        .destination_clock_id
        .as_deref()
        .map(|id| city_label(clocks, id))
        .unwrap_or_else(|| "another city".to_string());
    ui.horizontal(|ui| {
        ui.add_space(AVATAR_D + 6.0);
        let detail = if running {
            format!(
                "In {destination} · {}",
                until_label(travel_zone(clocks, Some(travel)), travel.ends_at)
            )
        } else {
            format!(
                "Next: {destination} · {}",
                starts_label(travel_zone(clocks, Some(travel)), travel.starts_at)
            )
        };
        ui.label(
            RichText::new(detail)
                .text_style(TextStyle::Small)
                .color(t.text_muted),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme::icon_button(ui, "trash-2", 20.0, true, "Delete trip").clicked() {
                *outcome = InspectorOutcome::RemoveTravel(travel.id.clone(), person.name.clone());
            }
            if theme::pill_button(ui, "Extend", false).clicked() {
                *outcome = InspectorOutcome::ExtendTravel(travel.id.clone());
            }
            if running && theme::pill_button(ui, "End now", false).clicked() {
                *outcome = InspectorOutcome::EndTravel(travel.id.clone());
            }
        });
    });
}

fn visitor_row(
    ui: &mut egui::Ui,
    status: &PersonStatus,
    entry: &WorldClockEntry,
    t: &Tokens,
    outcome: &mut InspectorOutcome,
) {
    ui.horizontal(|ui| {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(AVATAR_D), Sense::hover());
        paint_avatar(ui, rect, &initials(&status.person.name), t, Some(t.accent));
        let _ = response.on_hover_text(&status.person.name);
        ui.add_space(6.0);
        ui.vertical(|ui| {
            ui.label(
                RichText::new(&status.person.name)
                    .text_style(TextStyle::Body)
                    .color(t.text),
            );
            ui.label(
                RichText::new(format!(
                    "Here · {}",
                    until_label(entry.zone.as_ref(), status.until)
                ))
                .text_style(TextStyle::Small)
                .color(t.text_faint),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(travel) = &status.travel
                && theme::pill_button(ui, "Send home", false).clicked()
            {
                *outcome = InspectorOutcome::EndTravel(travel.id.clone());
            }
        });
    });
    ui.add_space(2.0);
}

/// Zone of the travel's destination (for formatting its return time).
fn travel_zone<'a>(clocks: &'a [WorldClockEntry], travel: Option<&Travel>) -> Option<&'a TimeZone> {
    let travel = travel?;
    let destination = travel.destination_clock_id.as_deref()?;
    clocks
        .iter()
        .find(|clock| clock.id == destination)
        .and_then(|clock| clock.zone.as_ref())
}

enum MoveOutcome {
    Stay,
    Close,
    Save(MoveSubmission),
}

enum MoveSubmission {
    Travel(TravelRequest),
    Extend {
        travel_id: String,
        ends_at: Option<i64>,
    },
    Relocate {
        person_id: String,
        clock_id: String,
    },
}

fn start_travel_draft(
    clocks: &[WorldClockEntry],
    travels: &[Travel],
    person_id: &str,
    now: Timestamp,
) -> Option<MoveDraft> {
    let (person, home) = find_person(clocks, person_id)?;
    let current_clock_id = clocks::placement_for(travels, person_id, now.as_second())
        .and_then(|travel| travel.destination_clock_id.clone())
        .unwrap_or_else(|| home.id.clone());
    let destination = clocks
        .iter()
        .find(|clock| clock.id != current_clock_id)
        .map(|clock| clock.id.clone());
    let zone = destination
        .as_deref()
        .and_then(|id| clocks.iter().find(|clock| clock.id == id))
        .and_then(|clock| clock.zone.as_ref());
    let (start_date, end_date) = default_dates(zone, now);
    Some(MoveDraft {
        person_id: person.id.clone(),
        person_name: person.name.clone(),
        home_clock_id: home.id.clone(),
        home_city: home.city.clone(),
        current_clock_id: Some(current_clock_id),
        kind: MoveKind::Travel,
        destination,
        start_date,
        end_date,
        open_ended: false,
        error: None,
    })
}

fn extend_travel_draft(
    clocks: &[WorldClockEntry],
    travels: &[Travel],
    travel_id: &str,
    now: Timestamp,
) -> Option<MoveDraft> {
    let travel = travels.iter().find(|travel| travel.id == travel_id)?;
    let (person, home) = find_person(clocks, &travel.person_id)?;
    let zone = travel_zone(clocks, Some(travel));
    let end_date = match travel.ends_at {
        Some(ends) => Timestamp::from_second(ends).ok().map(|timestamp| {
            timestamp
                .to_zoned(zone.cloned().unwrap_or_else(TimeZone::system))
                .date()
                .to_string()
        }),
        None => None,
    }
    .unwrap_or_else(|| default_dates(zone, now).1);
    Some(MoveDraft {
        person_id: person.id.clone(),
        person_name: person.name.clone(),
        home_clock_id: home.id.clone(),
        home_city: home.city.clone(),
        current_clock_id: travel.destination_clock_id.clone(),
        kind: MoveKind::Extend {
            travel_id: travel.id.clone(),
        },
        destination: travel.destination_clock_id.clone(),
        start_date: String::new(),
        end_date,
        open_ended: false,
        error: None,
    })
}

fn relocate_draft(
    clocks: &[WorldClockEntry],
    travels: &[Travel],
    person_id: &str,
    now: Timestamp,
) -> Option<MoveDraft> {
    let (person, home) = find_person(clocks, person_id)?;
    let current_clock_id = clocks::placement_for(travels, person_id, now.as_second())
        .and_then(|travel| travel.destination_clock_id.clone())
        .unwrap_or_else(|| home.id.clone());
    let (start_date, end_date) = default_dates(None, now);
    Some(MoveDraft {
        person_id: person.id.clone(),
        person_name: person.name.clone(),
        home_clock_id: home.id.clone(),
        home_city: home.city.clone(),
        current_clock_id: Some(current_clock_id),
        kind: MoveKind::Relocate,
        destination: clocks
            .iter()
            .find(|clock| clock.id != home.id)
            .map(|clock| clock.id.clone()),
        start_date,
        end_date,
        open_ended: false,
        error: None,
    })
}

fn find_person<'a>(
    clocks: &'a [WorldClockEntry],
    person_id: &str,
) -> Option<(&'a Person, &'a WorldClockEntry)> {
    clocks.iter().find_map(|entry| {
        entry
            .people
            .iter()
            .find(|person| person.id == person_id)
            .map(|person| (person, entry))
    })
}

/// Default wall dates in the destination zone: today and tomorrow.
fn default_dates(zone: Option<&TimeZone>, now: Timestamp) -> (String, String) {
    let today = now
        .to_zoned(zone.cloned().unwrap_or_else(TimeZone::system))
        .date();
    (
        today.to_string(),
        today.tomorrow().unwrap_or(today).to_string(),
    )
}

fn parse_date(text: &str) -> Option<Date> {
    text.trim().parse::<Date>().ok()
}

/// Travel/relocation form. Destination for travel excludes the city the
/// person occupies now; relocation excludes their current home. Enter saves,
/// Esc cancels.
fn move_modal(
    ctx: &egui::Context,
    draft: &mut MoveDraft,
    clocks: &[WorldClockEntry],
) -> MoveOutcome {
    let t = Tokens::get(ctx);
    let mut outcome = MoveOutcome::Stay;
    let title = match draft.kind {
        MoveKind::Travel => format!("Move {}", draft.person_name),
        MoveKind::Extend { .. } => format!("Extend {}'s trip", draft.person_name),
        MoveKind::Relocate => format!("Change {}'s home", draft.person_name),
    };
    let subtitle = match draft.kind {
        MoveKind::Travel => format!(
            "Home stays {}. The move applies for the selected window and reverts automatically.",
            draft.home_city
        ),
        MoveKind::Extend { .. } => {
            "Push the return time later, or set one on an open-ended trip.".to_string()
        }
        MoveKind::Relocate => format!(
            "Home changes from {}. Active trips to the old or new home are cancelled.",
            draft.home_city
        ),
    };

    let modal = egui::Modal::new(egui::Id::new("clocks-move")).frame(theme::dialog_frame(ctx));
    let response = modal.show(ctx, |ui| {
        ui.set_min_width(420.0);
        ui.heading(title);
        ui.label(
            RichText::new(subtitle)
                .text_style(TextStyle::Small)
                .color(t.text_faint),
        );
        ui.add_space(10.0);

        if matches!(draft.kind, MoveKind::Travel | MoveKind::Relocate) {
            ui.label(
                RichText::new("DESTINATION")
                    .text_style(theme::small_strong())
                    .color(t.text_faint),
            );
            ui.add_space(4.0);
            let excluded = match draft.kind {
                MoveKind::Travel => draft.current_clock_id.as_deref(),
                _ => Some(draft.home_clock_id.as_str()),
            };
            let mut any = false;
            for clock in clocks {
                if Some(clock.id.as_str()) == excluded {
                    continue;
                }
                any = true;
                ui.radio_value(
                    &mut draft.destination,
                    Some(clock.id.clone()),
                    clock.city.as_str(),
                );
            }
            if !any {
                ui.label(
                    RichText::new("Add another clock first — there is nowhere else to go.")
                        .text_style(TextStyle::Small)
                        .color(t.text_muted),
                );
            }
            ui.add_space(10.0);
        }

        match draft.kind {
            MoveKind::Travel => {
                ui.label(
                    RichText::new("STARTS")
                        .text_style(theme::small_strong())
                        .color(t.text_faint),
                );
                ui.add_space(4.0);
                date_field(ui, &mut draft.start_date, &t);
                ui.add_space(8.0);
                ui.checkbox(&mut draft.open_ended, "No return date");
                if !draft.open_ended {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("RETURNS")
                            .text_style(theme::small_strong())
                            .color(t.text_faint),
                    );
                    ui.add_space(4.0);
                    date_field(ui, &mut draft.end_date, &t);
                }
                ui.label(
                    RichText::new("Dates are days in the destination city; the trip runs from 00:00 to 23:59.")
                        .text_style(TextStyle::Small)
                        .color(t.text_faint),
                );
            }
            MoveKind::Extend { .. } => {
                ui.label(
                    RichText::new("RETURN DATE")
                        .text_style(theme::small_strong())
                        .color(t.text_faint),
                );
                ui.add_space(4.0);
                date_field(ui, &mut draft.end_date, &t);
                ui.label(
                    RichText::new("The trip ends at 23:59 in the destination city.")
                        .text_style(TextStyle::Small)
                        .color(t.text_faint),
                );
            }
            MoveKind::Relocate => {}
        }

        if let Some(error) = &draft.error {
            ui.add_space(6.0);
            ui.label(
                RichText::new(error)
                    .text_style(TextStyle::Small)
                    .color(ui.visuals().error_fg_color),
            );
        }

        ui.add_space(10.0);
        let mut save = false;
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::pill_button(ui, "Save", true).clicked() {
                    save = true;
                }
                if theme::pill_button(ui, "Cancel", false).clicked() {
                    outcome = MoveOutcome::Close;
                }
            });
        });

        // A text field does not swallow Enter in single-line mode, so this
        // matches the inspector's keyboard flow.
        if save || ui.input(|input| input.key_pressed(Key::Enter)) {
            draft.error = None;
            match build_submission(draft, clocks) {
                Ok(submission) => outcome = MoveOutcome::Save(submission),
                Err(error) => draft.error = Some(error),
            }
        }
    });

    match outcome {
        MoveOutcome::Stay if response.should_close() => MoveOutcome::Close,
        other => other,
    }
}

fn date_field(ui: &mut egui::Ui, value: &mut String, t: &Tokens) {
    egui::Frame::new()
        .fill(t.field)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(t.radius))
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.add_sized(
                [160.0, 20.0],
                egui::TextEdit::singleline(value)
                    .frame(egui::Frame::NONE)
                    .hint_text("2026-11-05"),
            );
        });
}

fn build_submission(
    draft: &MoveDraft,
    clocks: &[WorldClockEntry],
) -> Result<MoveSubmission, String> {
    let destination = draft
        .destination
        .as_deref()
        .ok_or_else(|| "Choose a destination city.".to_string())?;
    let zone = clocks
        .iter()
        .find(|clock| clock.id == destination)
        .and_then(|clock| clock.zone.as_ref())
        .ok_or_else(|| "The destination time zone is unavailable.".to_string())?;

    match draft.kind {
        MoveKind::Travel => {
            let start = parse_date(&draft.start_date)
                .ok_or_else(|| "Use a start date like 2026-11-05.".to_string())?;
            let ends_at = if draft.open_ended {
                None
            } else {
                let end = parse_date(&draft.end_date)
                    .ok_or_else(|| "Use a return date like 2026-11-05.".to_string())?;
                Some(clocks::day_end_seconds(end, zone).map_err(|error| error.to_string())?)
            };
            Ok(MoveSubmission::Travel(TravelRequest {
                person_id: draft.person_id.clone(),
                destination_clock_id: destination.to_string(),
                starts_at: clocks::day_start_seconds(start, zone)
                    .map_err(|error| error.to_string())?,
                ends_at,
            }))
        }
        MoveKind::Extend { .. } => {
            let end = parse_date(&draft.end_date)
                .ok_or_else(|| "Use a return date like 2026-11-05.".to_string())?;
            Ok(MoveSubmission::Extend {
                travel_id: match &draft.kind {
                    MoveKind::Extend { travel_id } => travel_id.clone(),
                    _ => unreachable!(),
                },
                ends_at: Some(
                    clocks::day_end_seconds(end, zone).map_err(|error| error.to_string())?,
                ),
            })
        }
        MoveKind::Relocate => Ok(MoveSubmission::Relocate {
            person_id: draft.person_id.clone(),
            clock_id: destination.to_string(),
        }),
    }
}

/// Nearest future travel start or expiry, with a small epsilon so the
/// repaint lands just after the boundary. `None` when no travel is pending,
/// so an idle page keeps only its minute tick.
fn until_next_travel(travels: &[Travel], now: i64) -> Option<Duration> {
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

enum PickerOutcome {
    Stay,
    Choose(String),
    Close,
}

fn picker_modal(ctx: &egui::Context, state: &mut WorldClockState, now: Timestamp) -> PickerOutcome {
    let WorldClockState { clocks, picker, .. } = state;
    let Some(picker) = picker.as_mut() else {
        return PickerOutcome::Stay;
    };
    let added: HashSet<&str> = clocks
        .iter()
        .map(|entry| entry.tz_identifier.as_str())
        .collect();

    let mut chosen = None;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::new("clocks-add-city")).frame(theme::dialog_frame(ctx));
    let response = modal.show(ctx, |ui| {
        ui.set_min_width(460.0);
        ui.heading("Add a city");
        ui.add_space(6.0);
        ui.label(
            RichText::new("Search by city, region or IANA time-zone identifier.")
                .text_style(TextStyle::Small)
                .color(Tokens::get(ui.ctx()).text_faint),
        );
        ui.add_space(8.0);

        theme::search_field(ui, &mut picker.query, "Search cities or time zones…", 440.0);
        if picker.query != picker.last_query {
            picker.last_query = picker.query.clone();
            picker.selected = 0;
        }

        let matching: Vec<&String> = picker
            .zones
            .iter()
            .filter(|zone| clocks::zone_matches(zone, &picker.query))
            .take(PICKER_MAX_ROWS)
            .collect();
        let selectable: Vec<&String> = matching
            .iter()
            .copied()
            .filter(|zone| !added.contains(zone.as_str()))
            .collect();
        if selectable.is_empty() {
            picker.selected = 0;
        } else {
            picker.selected = picker.selected.min(selectable.len() - 1);
        }

        let (up, down, enter) = ui.input(|input| {
            (
                input.key_pressed(Key::ArrowUp),
                input.key_pressed(Key::ArrowDown),
                input.key_pressed(Key::Enter),
            )
        });
        if down && !selectable.is_empty() {
            picker.selected = (picker.selected + 1).min(selectable.len() - 1);
        }
        if up {
            picker.selected = picker.selected.saturating_sub(1);
        }
        if enter && let Some(zone) = selectable.get(picker.selected) {
            chosen = Some((*zone).clone());
        }

        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .max_height(300.0)
            .auto_shrink([false; 2])
            .content_margin(Margin {
                left: 0,
                right: crate::widgets::CONTENT_RIGHT_INSET as i8,
                top: 0,
                bottom: 0,
            })
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                if matching.is_empty() {
                    ui.label(
                        RichText::new("No matching time zones.")
                            .text_style(TextStyle::Small)
                            .color(Tokens::get(ui.ctx()).text_faint),
                    );
                    return;
                }
                for zone in &matching {
                    let offset = clocks::resolve_zone(zone)
                        .map(|tz| clocks::format_offset(now.to_zoned(tz).offset().seconds()))
                        .unwrap_or_default();
                    let label = format!("{} — {}   {}", clocks::city_name(zone), zone, offset);
                    if added.contains(zone.as_str()) {
                        ui.add_enabled_ui(false, |ui| {
                            let _ = ui.selectable_label(
                                false,
                                RichText::new(format!("{label}   Added"))
                                    .text_style(TextStyle::Small),
                            );
                        });
                    } else {
                        let selected = selectable.get(picker.selected).map(|z| z.as_str())
                            == Some(zone.as_str());
                        if ui
                            .selectable_label(
                                selected,
                                RichText::new(label).text_style(TextStyle::Small),
                            )
                            .clicked()
                        {
                            chosen = Some((*zone).clone());
                        }
                    }
                }
            });

        if matching.len() == PICKER_MAX_ROWS {
            ui.label(
                RichText::new("Showing the first 200 matches — keep typing to narrow.")
                    .text_style(TextStyle::Small)
                    .color(Tokens::get(ui.ctx()).text_faint),
            );
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("↑ ↓ select · Enter add · Esc close")
                    .text_style(TextStyle::Small)
                    .color(Tokens::get(ui.ctx()).text_faint),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::pill_button(ui, "Cancel", false).clicked() {
                    cancel = true;
                }
            });
        });
    });

    if let Some(zone) = chosen {
        PickerOutcome::Choose(zone)
    } else if cancel || response.should_close() {
        PickerOutcome::Close
    } else {
        PickerOutcome::Stay
    }
}

/// Time until 50 ms past the next minute boundary. Clocks change on the
/// minute, so there is no reason to repaint between ticks.
fn until_next_minute(now: Timestamp) -> Duration {
    let elapsed =
        now.as_second().rem_euclid(60) as f64 + now.subsec_nanosecond() as f64 / 1_000_000_000.0;
    Duration::from_secs_f64((60.0 - elapsed + 0.05).max(0.05))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Context, Id, Pos2, RawInput, Rect, UiBuilder};

    fn entry(id: &str, tz: &str) -> WorldClockEntry {
        clock_entry(id, tz, &[])
    }

    fn clock_entry(id: &str, tz: &str, names: &[&str]) -> WorldClockEntry {
        let people = names
            .iter()
            .enumerate()
            .map(|(index, name)| Person {
                id: format!("{id}-p{index}"),
                name: (*name).to_string(),
                home_clock_id: id.to_string(),
                position: index as i64,
                created_at: "2026-01-01T00:00:00Z".to_string(),
            })
            .collect();
        WorldClockEntry::new(
            Clock {
                id: id.to_string(),
                tz_identifier: tz.to_string(),
                position: 0,
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
            people,
        )
    }

    fn fixture() -> WorldClockState {
        WorldClockState {
            clocks: vec![
                entry("c1", "Europe/Kyiv"),
                entry("c2", "Asia/Tokyo"),
                entry("c3", "America/Los_Angeles"),
            ],
            loaded: true,
            ..Default::default()
        }
    }

    fn five_cities() -> WorldClockState {
        WorldClockState {
            clocks: vec![
                entry("c1", "Europe/Kyiv"),
                entry("c2", "Asia/Tokyo"),
                entry("c3", "America/Los_Angeles"),
                entry("c4", "Europe/Paris"),
                entry("c5", "Europe/Berlin"),
            ],
            loaded: true,
            ..Default::default()
        }
    }

    fn twelve_cities() -> WorldClockState {
        let zones = [
            "Europe/Kyiv",
            "Asia/Tokyo",
            "America/Los_Angeles",
            "Europe/Paris",
            "Europe/Berlin",
            "Europe/London",
            "Europe/Madrid",
            "Europe/Rome",
            "Africa/Cairo",
            "Asia/Dubai",
            "Australia/Sydney",
            "Pacific/Auckland",
        ];
        WorldClockState {
            clocks: zones
                .iter()
                .enumerate()
                .map(|(index, zone)| entry(&format!("c{index}"), zone))
                .collect(),
            loaded: true,
            ..Default::default()
        }
    }

    /// Y position of the first text shape containing `needle`.
    fn text_y(output: &egui::FullOutput, needle: &str) -> Option<f32> {
        fn walk(shape: &egui::Shape, needle: &str, found: &mut Option<f32>) {
            match shape {
                egui::Shape::Text(text) => {
                    if found.is_none() && text.galley.text().contains(needle) {
                        *found = Some(text.pos.y);
                    }
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, needle, found);
                    }
                }
                _ => {}
            }
        }
        let mut found = None;
        for clipped in &output.shapes {
            walk(&clipped.shape, needle, &mut found);
        }
        found
    }

    fn shapes_text(output: &egui::FullOutput) -> String {
        fn collect(shape: &egui::Shape, text: &mut String) {
            match shape {
                egui::Shape::Text(t) => {
                    text.push_str(t.galley.text());
                    text.push('\n');
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, text);
                    }
                }
                _ => {}
            }
        }
        let mut text = String::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut text);
        }
        text
    }

    fn render(mut state: WorldClockState, use_24h: bool) -> egui::FullOutput {
        let ctx = Context::default();
        theme::install_fonts(&ctx);
        theme::apply_visuals(&ctx, false);
        // Areas (modals) fade in over their first frames. The first pass lets
        // the area record when it became visible; the second pass draws it at
        // full opacity so its text reaches the shapes.
        let mut out = None;
        for time in [0.0_f64, 1.0] {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(940.0, 600.0))),
                time: Some(time),
                ..Default::default()
            };
            let mut full = ctx.run_ui(input, |ctx| {
                let mut ui = egui::Ui::new(
                    ctx.clone(),
                    Id::new("clocks-page"),
                    UiBuilder::new()
                        .max_rect(Rect::from_min_size(Pos2::ZERO, Vec2::new(940.0, 600.0))),
                );
                let _ = show(&mut ui, &mut state, use_24h);
            });
            // Must happen before the context drops, even when an assertion fails.
            full.textures_delta.clear();
            out = Some(full);
        }
        out.expect("render runs at least one frame")
    }

    /// Frames a drag over several frames so egui can start and finish the
    /// gesture the way the running app does.
    struct DragHarness {
        ctx: Context,
        state: WorldClockState,
        time: f64,
    }

    impl DragHarness {
        fn new(state: WorldClockState) -> Self {
            let ctx = Context::default();
            theme::install_fonts(&ctx);
            theme::apply_visuals(&ctx, false);
            Self {
                ctx,
                state,
                time: 0.0,
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Actions {
            self.time += 1.0 / 60.0;
            let mut actions = None;
            let mut full = self.ctx.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(940.0, 600.0))),
                    time: Some(self.time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    let mut ui = egui::Ui::new(
                        ctx.clone(),
                        Id::new("clocks-page"),
                        UiBuilder::new()
                            .max_rect(Rect::from_min_size(Pos2::ZERO, Vec2::new(940.0, 600.0))),
                    );
                    actions = Some(show(&mut ui, &mut self.state, true));
                },
            );
            full.textures_delta.clear();
            actions.expect("show runs every frame")
        }

        /// Laid-out card slots as recorded by the drop zone.
        fn slots(&self) -> Vec<Rect> {
            self.state
                .zone
                .as_ref()
                .expect("the grid records its drop zone")
                .slots
                .iter()
                .map(|(_, rect)| *rect)
                .collect()
        }

        /// Press on `from`, cross to `to`, release there.
        fn drag(&mut self, from: Pos2, to: Pos2) -> Actions {
            self.frame(vec![
                egui::Event::PointerMoved(from),
                egui::Event::PointerButton {
                    pos: from,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            // Cross egui's click/drag threshold, then move to the target.
            self.frame(vec![egui::Event::PointerMoved(from + Vec2::new(0.0, 8.0))]);
            self.frame(vec![egui::Event::PointerMoved(to)]);
            assert!(self.state.drag.is_some(), "drag did not start");
            self.frame(vec![egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }])
        }
    }

    fn ordered(ids: &[&str]) -> Option<Vec<String>> {
        Some(ids.iter().map(|id| id.to_string()).collect())
    }

    /// End-to-end pointer drag from the first row into the second: the drop
    /// must resolve to the slot under the pointer and commit the matching
    /// permutation (c2 lands in slot 3).
    #[test]
    fn pointer_drag_into_the_next_row_commits_the_pointer_slot() {
        let mut harness = DragHarness::new(five_cities());
        harness.frame(vec![]);
        let slots = harness.slots();

        let actions = harness.drag(slots[1].center(), slots[3].center());
        assert_eq!(
            actions.reorder_ids,
            ordered(&["c1", "c3", "c4", "c2", "c5"])
        );
        assert!(harness.state.drag.is_none(), "release must end the drag");
    }

    /// The direction bugs hide in: drag from the second row back up into the
    /// first. c4 must take the middle of row 1.
    #[test]
    fn pointer_drag_back_up_a_row_commits_the_pointer_slot() {
        let mut harness = DragHarness::new(five_cities());
        harness.frame(vec![]);
        let slots = harness.slots();

        let actions = harness.drag(slots[3].center(), slots[1].center());
        assert_eq!(
            actions.reorder_ids,
            ordered(&["c1", "c4", "c2", "c3", "c5"])
        );
        assert!(harness.state.drag.is_none(), "release must end the drag");
    }

    #[test]
    fn one_row_renders_the_three_city_columns() {
        let out = render(fixture(), true);
        let text = shapes_text(&out);
        assert!(text.contains("Kyiv"), "{text}");
        assert!(text.contains("Tokyo"), "{text}");
        assert!(text.contains("Los Angeles"), "{text}");
        assert!(text.contains("GMT+9"), "{text}");
        assert!(text.contains("Add city"), "{text}");
        assert!(text.contains("3 CITIES"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn twelve_hour_mode_adds_a_lowercase_period() {
        let out = render(fixture(), false);
        let text = shapes_text(&out);
        assert!(text.contains("am") || text.contains("pm"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn five_cities_wrap_onto_a_second_row() {
        let out = render(five_cities(), true);
        let text = shapes_text(&out);
        for city in ["Kyiv", "Tokyo", "Los Angeles", "Paris", "Berlin"] {
            assert!(text.contains(city), "{city} missing: {text}");
        }
        assert!(text.contains("5 CITIES"), "{text}");
        assert!(!text.contains(" / "), "no pager remains: {text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn many_cities_fill_rows_inside_one_scroll_area() {
        let out = render(twelve_cities(), true);
        let text = shapes_text(&out);
        assert!(text.contains("12 CITIES"), "{text}");
        assert!(!text.contains(" / "), "no pager remains: {text}");
        for city in ["Auckland", "Cairo", "Sydney", "Rome"] {
            assert!(text.contains(city), "{city} missing: {text}");
        }
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");

        // Wrapping is uniform: the second and fourth rows sit one and three
        // strides below the first, so the whole grid lives in the scroll
        // content rather than a paged viewport.
        let first = text_y(&out, "Kyiv").expect("Kyiv renders");
        let second = text_y(&out, "Paris").expect("Paris renders");
        let fourth = text_y(&out, "Dubai").expect("Dubai renders");
        assert!((second - first - (CARD_H + CARD_GAP)).abs() < 1.0);
        assert!((fourth - first - 3.0 * (CARD_H + CARD_GAP)).abs() < 1.0);
    }

    #[test]
    fn search_filters_the_grid_and_reports_the_match_count() {
        let mut state = five_cities();
        state.search = "par".to_string();
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("Paris"), "{text}");
        assert!(!text.contains("Kyiv"), "{text}");
        assert!(!text.contains("Tokyo"), "{text}");
        assert!(text.contains("1 OF 5 CITIES"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn grid_layout_keeps_three_across_and_wraps() {
        let one_row = grid_layout(3, 940.0);
        assert_eq!(one_row.slots.len(), 3);
        assert!((one_row.column_w - (940.0 - 2.0 * CARD_GAP) / 3.0).abs() < 0.01);
        assert!(one_row.slots.iter().all(|slot| slot.top() == 0.0));
        assert!((one_row.height - CARD_H).abs() < 0.01);

        let two_rows = grid_layout(5, 940.0);
        assert!((two_rows.slots[3].top() - (CARD_H + CARD_GAP)).abs() < 0.01);
        assert_eq!(two_rows.slots[3].left(), two_rows.slots[0].left());
        assert!((two_rows.height - (2.0 * CARD_H + CARD_GAP)).abs() < 0.01);

        // Narrow windows keep three across down to the readable floor.
        let narrow = grid_layout(4, 300.0);
        assert_eq!(narrow.column_w, 160.0);
        assert!((narrow.slots[3].top() - (CARD_H + CARD_GAP)).abs() < 0.01);
    }

    #[test]
    fn reorder_moves_one_id_to_the_drop_index() {
        let ids: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|id| id.to_string())
            .collect();
        assert_eq!(reordered(&ids, 0, 2), ["b", "c", "a", "d", "e"]);
        assert_eq!(reordered(&ids, 4, 0), ["e", "a", "b", "c", "d"]);
        assert_eq!(reordered(&ids, 2, 2), ids);
        // Out-of-range input is returned untouched rather than panicking.
        assert_eq!(reordered(&ids, 9, 0), ids);
    }

    /// Drop zone built from the laid-out slots, the way the view builds it.
    fn static_zone(count: usize) -> dnd::GridZone {
        let layout = grid_layout(count, 940.0);
        dnd::GridZone {
            rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(940.0, layout.height.max(CARD_H))),
            slots: layout
                .slots
                .iter()
                .enumerate()
                .map(|(slot, rect)| (format!("s{slot}"), *rect))
                .collect(),
        }
    }

    /// The row a pointer lands in must not depend on vertical position within
    /// a card, and a drop index must always resolve to the slot under the
    /// pointer in both directions. This is the cross-row seam: the old rule
    /// flipped rows at each row's centre, and the zone lost a row entirely
    /// once the insertion gap was its only occupant.
    #[test]
    fn cross_row_drop_resolves_the_slot_under_the_pointer() {
        let layout = grid_layout(5, 940.0);
        let zone = static_zone(5);

        // Row 1 -> row 2 and back, resolved by the actual slot centres.
        assert_eq!(grid_slot_at(&zone, layout.slots[1].center(), 5), Some(1));
        assert_eq!(grid_slot_at(&zone, layout.slots[3].center(), 5), Some(3));
        assert_eq!(grid_slot_at(&zone, layout.slots[4].center(), 5), Some(4));
        assert_eq!(grid_slot_at(&zone, layout.slots[0].center(), 5), Some(0));

        // The lower half of a first-row card still belongs to the first row.
        let lower = pos2(layout.slots[1].center().x, layout.slots[1].bottom() - 8.0);
        assert_eq!(grid_slot_at(&zone, lower, 5), Some(1));

        // A row only disappears when the whole grid does: every slot stays
        // in the zone regardless of what the preview does.
        assert_eq!(zone.slots.len(), 5);

        // Outside the grid resolves nothing, which the caller turns into a
        // snap-back.
        assert_eq!(grid_slot_at(&zone, pos2(-20.0, -20.0), 5), None);
    }

    /// The preview target of every card and the committed permutation must
    /// agree, including when the drop crosses a row in either direction.
    #[test]
    fn cross_row_preview_matches_the_committed_order() {
        let ids: Vec<String> = ["c1", "c2", "c3", "c4", "c5"]
            .iter()
            .map(|id| id.to_string())
            .collect();

        // Down: lift c2 (slot 1) and drop it on the first slot of row 2.
        let (placements, gap) = preview_slots(5, Some(1), Some(3), None);
        assert_eq!(gap, Some(3));
        assert_eq!(placements, vec![(0, 0), (2, 1), (3, 2), (4, 4)]);
        let down = reordered(&ids, 1, 3);
        assert_eq!(down, ["c1", "c3", "c4", "c2", "c5"]);
        assert_eq!(down[3], "c2", "the lifted card must land in the gap slot");
        // The other cards keep their order and shift by at most one slot.
        for (visible, target) in &placements {
            assert!(visible.abs_diff(*target) <= 1, "{visible} -> {target}");
        }

        // Up: lift c4 (slot 3) and drop it on the middle slot of row 1.
        let (placements, gap) = preview_slots(5, Some(3), Some(1), None);
        assert_eq!(gap, Some(1));
        assert_eq!(placements, vec![(0, 0), (1, 2), (2, 3), (4, 4)]);
        let up = reordered(&ids, 3, 1);
        assert_eq!(up, ["c1", "c4", "c2", "c3", "c5"]);
        assert_eq!(up[1], "c4", "the lifted card must land in the gap slot");

        // Hovering the lifted card's own slot is a no-op (snap-back path).
        let (placements, gap) = preview_slots(5, Some(1), Some(1), None);
        assert_eq!(gap, Some(1));
        assert!(placements.iter().all(|(visible, target)| visible == target));
    }

    /// A cross-row reflow glides each card from where it is now to its new
    /// slot: no teleport on the retarget frame, monotone travel in both axes,
    /// and an exact landing after `SHIFT_DURATION`.
    #[test]
    fn cross_row_reflow_glides_from_the_current_position() {
        let layout = grid_layout(5, 940.0);

        // Down: c4 travels from slot 3 (row 2) up to slot 2 (row 1).
        let mut motions = MotionSet::default();
        let before = motions.rect("c4", layout.slots[3], 0.0, true);
        assert_eq!(before, layout.slots[3]);
        let retarget = motions.rect("c4", layout.slots[2], 0.05, true);
        assert_eq!(retarget, before, "retarget frame teleported the card");
        let middle = motions.rect(
            "c4",
            layout.slots[2],
            0.05 + anim::SHIFT_DURATION * 0.5,
            true,
        );
        assert!(middle.top() < before.top() && middle.top() > layout.slots[2].top());
        assert!(middle.left() > before.left() && middle.left() < layout.slots[2].left());
        assert!(motions.animating(0.05 + anim::SHIFT_DURATION * 0.5));
        let landed = motions.rect(
            "c4",
            layout.slots[2],
            0.05 + anim::SHIFT_DURATION + 0.001,
            true,
        );
        assert_eq!(landed, layout.slots[2]);
        assert!(!motions.animating(0.05 + anim::SHIFT_DURATION + 0.001));

        // Up: c3 travels from slot 2 (row 1) down to slot 3 (row 2).
        let mut motions = MotionSet::default();
        let before = motions.rect("c3", layout.slots[2], 0.0, true);
        let retarget = motions.rect("c3", layout.slots[3], 0.05, true);
        assert_eq!(retarget, before, "retarget frame teleported the card");
        let middle = motions.rect(
            "c3",
            layout.slots[3],
            0.05 + anim::SHIFT_DURATION * 0.5,
            true,
        );
        assert!(middle.top() > before.top() && middle.top() < layout.slots[3].top());
        assert!(middle.left() < before.left() && middle.left() > layout.slots[3].left());
        assert_eq!(
            motions.rect(
                "c3",
                layout.slots[3],
                0.05 + anim::SHIFT_DURATION + 0.001,
                true
            ),
            layout.slots[3]
        );

        // Reduced motion still teleports with no animation state left behind.
        let mut motions = MotionSet::default();
        assert_eq!(
            motions.rect("c3", layout.slots[2], 0.0, false),
            layout.slots[2]
        );
        assert_eq!(
            motions.rect("c3", layout.slots[3], 0.0, false),
            layout.slots[3]
        );
        assert!(!motions.animating(0.0));
    }

    #[test]
    fn motion_retargets_from_the_current_position() {
        let from = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(100.0, 100.0));
        let to = Rect::from_min_size(pos2(200.0, 0.0), Vec2::new(100.0, 100.0));
        let third = Rect::from_min_size(pos2(400.0, 0.0), Vec2::new(100.0, 100.0));
        let mut motions = MotionSet::default();
        assert_eq!(motions.rect("a", from, 0.0, true), from);

        // Retargeting starts where the card is (still at rest), then glides.
        let start = anim::SHIFT_DURATION * 0.5;
        assert_eq!(motions.rect("a", to, start, true), from);
        let middle = motions.rect("a", to, start + anim::SHIFT_DURATION * 0.5, true);
        assert!(middle.left() > 0.0 && middle.left() < 200.0);

        // Interrupting mid-glide continues from the on-screen position.
        let interrupted = motions.rect("a", third, start + anim::SHIFT_DURATION * 0.51, true);
        assert!((interrupted.left() - middle.left()).abs() < 10.0);
        assert!(motions.animating(start + anim::SHIFT_DURATION * 0.51));

        // Reduced motion teleports and leaves nothing to animate.
        assert_eq!(motions.rect("a", third, 0.0, false), third);
        assert!(!motions.animating(0.0));
    }

    #[test]
    fn no_results_state_offers_to_clear_the_search() {
        let mut state = fixture();
        state.search = "zzzz".to_string();
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("No cities match"), "{text}");
        assert!(text.contains("Clear search"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn empty_state_invites_adding_the_first_city() {
        let out = render(WorldClockState::default(), true);
        let text = shapes_text(&out);
        assert!(text.contains("No cities yet."), "{text}");
        assert!(text.contains("Add your first city"), "{text}");
        assert!(text.contains("Add a city"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn picker_lists_matching_zones_and_marks_added_ones() {
        let mut state = fixture();
        state.picker = Some(ZonePicker {
            query: "tok".to_string(),
            last_query: "tok".to_string(),
            selected: 0,
            zones: clocks::available_zones(),
        });
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("Add a city"), "{text}");
        assert!(text.contains("Asia/Tokyo"), "{text}");
        assert!(text.contains("Added"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn card_strip_shows_two_initials_and_a_remainder() {
        let mut state = fixture();
        state.clocks[1] = clock_entry("c2", "Asia/Tokyo", &["Ana", "Ben Carter", "Cleo"]);
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("A\n"), "{text}");
        assert!(text.contains("BC"), "{text}");
        assert!(text.contains("+1"), "{text}");
        assert!(!text.contains("Cleo"), "only two chips fit: {text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn initials_come_from_the_first_two_words() {
        assert_eq!(initials("Ben Carter"), "BC");
        assert_eq!(initials("ana"), "A");
        assert_eq!(initials("Mary Jane Watson"), "MJ");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn inspector_lists_people_and_explains_the_empty_case() {
        let mut state = fixture();
        state.clocks[1] = clock_entry("c2", "Asia/Tokyo", &["Ana", "Ben Carter"]);
        state.open_inspector("c2");
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("Tokyo"), "{text}");
        assert!(text.contains("RESIDENTS · 2"), "{text}");
        assert!(text.contains("VISITORS · 0"), "{text}");
        assert!(text.contains("Ana"), "{text}");
        assert!(text.contains("Ben Carter"), "{text}");
        assert!(text.contains("Add a resident"), "{text}");
        assert!(text.contains("Done"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");

        let mut empty = fixture();
        empty.open_inspector("c1");
        let out = render(empty, true);
        let text = shapes_text(&out);
        assert!(text.contains("RESIDENTS · 0"), "{text}");
        assert!(text.contains("No residents yet."), "{text}");
        assert!(text.contains("Add someone and their initials"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    fn trip(id: &str, person: &str, destination: &str, start: i64, end: Option<i64>) -> Travel {
        Travel {
            id: id.to_string(),
            person_id: person.to_string(),
            origin_clock_id: None,
            destination_clock_id: Some(destination.to_string()),
            starts_at: start,
            ends_at: end,
            cancelled_at: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn card_marks_away_residents_and_lists_visitors_separately() {
        let now = Timestamp::now().as_second();
        let mut state = fixture();
        state.clocks[0] = clock_entry("c1", "Europe/Kyiv", &["Ana"]);
        state.clocks[1] = clock_entry("c2", "Asia/Tokyo", &["Ben"]);
        state.travels = vec![trip("t1", "c1-p0", "c2", now - 60, None)];
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("VISITING"), "{text}");
        assert!(text.contains("Kyiv"), "{text}");
        assert!(text.contains("Tokyo"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn inspector_distinguishes_away_visiting_and_expired_people() {
        let now = Timestamp::now().as_second();
        let mut state = fixture();
        state.clocks[0] = clock_entry("c1", "Europe/Kyiv", &["Ana"]);
        state.clocks[1] = clock_entry("c2", "Asia/Tokyo", &["Ben"]);
        let travels = vec![trip("t1", "c1-p0", "c2", now - 60, Some(now + 600))];
        state.travels = travels.clone();
        state.open_inspector("c1");
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("RESIDENTS · 1"), "{text}");
        assert!(text.contains("Away in Tokyo · until"), "{text}");
        assert!(text.contains("VISITORS · 0"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");

        let mut destination = fixture();
        destination.clocks[0] = clock_entry("c1", "Europe/Kyiv", &["Ana"]);
        destination.clocks[1] = clock_entry("c2", "Asia/Tokyo", &["Ben"]);
        destination.travels = travels;
        destination.open_inspector("c2");
        let out = render(destination, true);
        let text = shapes_text(&out);
        assert!(text.contains("RESIDENTS · 1"), "{text}");
        assert!(text.contains("VISITORS · 1"), "{text}");
        assert!(text.contains("Here · until"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");

        // An expired trip renders as home again without any reload: placement
        // is evaluated against this frame's now.
        let mut expired = fixture();
        expired.clocks[0] = clock_entry("c1", "Europe/Kyiv", &["Ana"]);
        expired.clocks[1] = clock_entry("c2", "Asia/Tokyo", &["Ben"]);
        expired.travels = vec![trip("t1", "c1-p0", "c2", now - 120, Some(now - 60))];
        expired.open_inspector("c1");
        let out = render(expired, true);
        let text = shapes_text(&out);
        assert!(text.contains("Here now"), "{text}");
        assert!(!text.contains("Away in Tokyo"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn inspector_shows_a_planned_trip_before_it_starts() {
        let now = Timestamp::now().as_second();
        let mut state = fixture();
        state.clocks[0] = clock_entry("c1", "Europe/Kyiv", &["Ana"]);
        state.clocks[1] = clock_entry("c2", "Asia/Tokyo", &["Ben"]);
        state.travels = vec![trip("t1", "c1-p0", "c2", now + 600, Some(now + 1200))];
        state.open_inspector("c1");
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("Leaving for Tokyo · starts"), "{text}");
        assert!(text.contains("Next: Tokyo · starts"), "{text}");
        assert!(text.contains("Extend"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn travel_repaint_targets_the_nearest_pending_boundary() {
        let now = Timestamp::now().as_second();
        assert!(until_next_travel(&[], now).is_none());

        let running = trip("t1", "p", "c", now - 60, Some(now + 30));
        let wait = until_next_travel(&[running], now).unwrap();
        assert!((wait.as_secs_f64() - 30.05).abs() < 0.01, "{wait:?}");

        let mut cancelled = trip("t2", "p", "c", now - 60, Some(now + 10));
        cancelled.cancelled_at = Some(now - 10);
        assert!(until_next_travel(&[cancelled], now).is_none());

        let starts_soon = trip("t3", "p", "c", now + 50, Some(now + 100));
        let ends_later = trip("t4", "p", "c", now - 60, Some(now + 200));
        let wait = until_next_travel(&[starts_soon, ends_later], now).unwrap();
        assert!((wait.as_secs_f64() - 50.05).abs() < 0.01, "{wait:?}");
    }

    #[test]
    fn travel_form_renders_the_destination_and_window() {
        let now = Timestamp::now();
        let mut state = fixture();
        state.clocks[0] = clock_entry("c1", "Europe/Kyiv", &["Ana"]);
        state.clocks[1] = clock_entry("c2", "Asia/Tokyo", &[]);
        let draft = start_travel_draft(&state.clocks, &state.travels, "c1-p0", now)
            .expect("a second city is available");
        assert_eq!(draft.destination.as_deref(), Some("c2"));
        state.move_draft = Some(draft);
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("Move Ana"), "{text}");
        assert!(text.contains("DESTINATION"), "{text}");
        assert!(text.contains("STARTS"), "{text}");
        assert!(text.contains("No return date"), "{text}");
        assert!(text.contains("Save"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn extend_form_opens_on_the_current_destination() {
        let now = Timestamp::now();
        let mut state = fixture();
        state.clocks[0] = clock_entry("c1", "Europe/Kyiv", &["Ana"]);
        state.clocks[1] = clock_entry("c2", "Asia/Tokyo", &[]);
        state.travels = vec![trip(
            "t1",
            "c1-p0",
            "c2",
            now.as_second() - 60,
            Some(now.as_second() + 600),
        )];
        let draft = extend_travel_draft(&state.clocks, &state.travels, "t1", now)
            .expect("the travel exists");
        assert_eq!(draft.destination.as_deref(), Some("c2"));
        state.move_draft = Some(draft);
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("Extend Ana's trip"), "{text}");
        assert!(text.contains("RETURN DATE"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn search_matches_a_residents_name() {
        let mut state = fixture();
        state.clocks[1] = clock_entry("c2", "Asia/Tokyo", &["Ben Carter"]);
        state.search = "carter".to_string();
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("Tokyo"), "{text}");
        assert!(!text.contains("Kyiv"), "{text}");
        assert!(!text.contains("Los Angeles"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn one_city_renders_a_single_card_and_count() {
        let state = WorldClockState {
            clocks: vec![entry("c1", "Europe/Kyiv")],
            loaded: true,
            ..Default::default()
        };
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("Kyiv"), "{text}");
        assert!(text.contains("1 CITY"), "{text}");
        assert!(!text.contains(" / "), "a single city has no pager: {text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn unknown_zone_renders_a_readable_label_instead_of_panicking() {
        let out = render(
            WorldClockState {
                clocks: vec![entry("c1", "Mars/Olympus")],
                loaded: true,
                ..Default::default()
            },
            true,
        );
        let text = shapes_text(&out);
        assert!(text.contains("Olympus"), "{text}");
        assert!(text.contains("Unknown time zone"), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");

        // The inspector still opens and stays readable with no zone handle.
        let mut state = WorldClockState {
            clocks: vec![entry("c1", "Mars/Olympus")],
            loaded: true,
            ..Default::default()
        };
        state.open_inspector("c1");
        let out = render(state, true);
        let text = shapes_text(&out);
        assert!(text.contains("RESIDENTS · 0"), "{text}");
        assert!(text.contains("No residents yet."), "{text}");
        assert!(!text.contains("use of"), "duplicate widget ids: {text}");
    }

    #[test]
    fn until_label_formats_the_end_of_a_day_in_the_destination_zone() {
        let tokyo = clocks::resolve_zone("Asia/Tokyo").expect("bundled zone resolves");
        let ends = clocks::day_end_seconds("2026-11-07".parse().expect("valid date"), &tokyo)
            .expect("day bounds resolve");
        assert_eq!(
            until_label(Some(&tokyo), Some(ends)),
            "until Sat, 7 Nov. 23:59"
        );
        assert_eq!(until_label(None, Some(ends)), "until an unknown time");
    }

    #[test]
    fn next_minute_repaint_lands_just_past_the_boundary() {
        let now: Timestamp = "2026-01-01T00:00:30.250Z".parse().unwrap();
        let wait = until_next_minute(now);
        assert!(
            (wait.as_secs_f64() - 29.8).abs() < 0.001,
            "expected ~29.8 s, got {wait:?}"
        );
        let at_boundary: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
        assert!((until_next_minute(at_boundary).as_secs_f64() - 60.05).abs() < 0.001);
    }
}
