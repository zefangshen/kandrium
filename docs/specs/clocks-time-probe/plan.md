# Plan: time probe in the Clocks toolbar

Status: approved; implementation follows the stage order in §15.
Scope: `crates/app-gpui` (the gpui front-end) and the pure helpers in
`crates/core/src/clocks.rs`. The egui app (`crates/app/src/views/wclock.rs`),
the MCP server, the schema and the settings file are untouched.

Decisions taken (approved defaults):

- Q1 commit timing: Enter or blur, never per keystroke.
- Q2 zone switch: re-read the typed wall clock in the new zone.
- Q3 date scope: today in the probe zone only; no date field.
- Q4 titlebar: stays live; the page is the only place that shows the probe.
- Q5 fold: earlier occurrence with a hint; no second-occurrence toggle.
- Q6 placement: evaluated at the probe instant.
- Q7 persistence: none; a restart opens live.
- Q8 repaint tick: unchanged; the minute tick keeps running for the titlebar.
- Q9 probed zone: no accent ring on its card; the pill names the zone.
- Q10 default zone: the system zone captured when the probe opens.
- Q11 error surface: pill border plus tooltip/`aria_label`, no extra row.
- S2: the accent sits on the zone chip, not a wrapping pill, because
  `field_input` already paints its own border and fill.
- S3: the all-zones overlay reuses the existing picker (`clock-picker`); its
  header reads "Probe time zone", saved zones stay selectable, and the current
  zone carries a `Selected` chip.
- S3: a 12/24-hour toggle rewrites an active probe's field from its instant
  (`probe_field_text`), leaving the instant itself untouched.

## 1. Goal

Add a probe to the Clocks page: the user types a wall-clock time and picks the
zone that time is expressed in (local zone by default). While a probe is
active, every city card shows what that city's clock reads **at that one
instant** instead of the live time, so the whole grid can be read as a single
row of a world clock. Leaving the probe returns the page to live time.

Success looks like:

- The control lives in the existing 46 px search row and never wraps it.
- Typing `03:00`, zone `Asia/Shanghai`, makes Tokyo read `04:00` and Los
  Angeles read `11:00` (previous day), with the same date/offset rules the
  cards already use.
- One click on **Now** (or Esc, or clearing the field) restores live time.
- Nothing is written to SQLite or to `gpui-settings.conf`.
- Residents/visitors and the "starts/until" rows follow the probed instant, so
  the page can answer "who was in Tokyo at 03:00 last Tuesday?".

### Non-goals

- No date entry, no timeline scrubbing, no animation of a moving probe.
- No probe in the titlebar clock; the chrome keeps telling live time.
- No per-card probe, no pinning a city to a second instant.
- No persistence of the last probe across restarts.
- No change to travel/residency data or to the reorder drag model.

## 2. What exists today

| Piece | Location |
| --- | --- |
| Toolbar row (46 px, heading, count, search, spacer, units toggle, Add city) | `crates/app-gpui/src/views/clocks.rs` L713–783 |
| `ClockState` (entries, travels, `now`, search, overlays) | `clocks.rs` L161–275 |
| `ClockState::next_wait` (nearer of next minute / next travel boundary) | `clocks.rs` L229–236 |
| `ClockState::city_people` (placement at `self.now`) | `clocks.rs` L264–269 |
| Card render (`format_clock(state.now, zone, use_24h)`) | `clocks.rs` L992–1014 |
| Overlay pattern (absolute panel over the page) + IANA picker | `crates/app-gpui/src/views/clocks_panels.rs` L25–279 |
| Minute tick loop, one `Timestamp::now()` per wake | `crates/app-gpui/src/app.rs` L3335–3360 |
| Clocks section body | `app.rs` L6000 |
| Titlebar clock (always live, every section) | `app.rs` L5783–5839 |
| `format_clock`, `format_offset`, `city_name`, `placement_at`/`placement_for` | `crates/core/src/clocks.rs` L424–456, L795–874 |
| `use_24h` preference (in memory, default `true`) | `app.rs` L168, L3459–3462 |

The important property: every rendered time is already a pure function of one
instant plus a zone (`format_clock`), and every person row is a pure function
of one instant plus the cached travel rows (`placement_for`). A probe is
therefore a second instant plumbed through two existing call sites — no new
data path, no new timer.

## 3. Toolbar placement

Current row: `Clocks` · `N saved` · search (240 px) · `flex_1` · 12h/24h ·
`Add city`, height 46 px, `px(20)`, `gap(10)`.

The probe control goes **immediately after the search field**, before the
`flex_1` spacer, so the left group reads "filter cities · look at a moment".
The right group stays untouched.

```
[ Clocks ]  [ 3 saved ]  [ 🔍 search… ]  [ 🕘 3:00 pm  Shanghai ▾  | Now ]  …spacer…  [12h|24h]  [ + Add city ]
```

Anatomy (all `h(px(28))`, `rounded(t.radius)`, `border_1`):

- container `clocks-probe`, `flex items_center gap(6)`; the accent lives on
  the zone chip (S2 decision): `field_input` already paints its own fill and
  border, and wrapping it in a framed pill would show the duplicate-box
  regression the workspace notes warn about.
- `Icon::Calendar` at 11 px, `t.text_faint` (no new icon asset).
- time field `clocks-probe-time`: `width 92 px`, `Text::Small`; placeholder
  follows the preference (`14:30` vs `2:30 pm`).
- zone trigger `clocks-probe-zone`: city label + `Icon::ChevronDown` at 10 px,
  hover wash; label is `city_name(iana)` / the local zone's city. Live:
  `border t.border`, `bg t.card`; probing: `border t.accent`, `bg
  t.accent_soft`; failed commit: `border t.danger`.
- separator `1 px t.divider` and `clocks-probe-now` (`Icon::RotateCcw` 10 px +
  `Now`), rendered **only** while probing.
- `clocks-probe-error` / `clocks-probe-hint`: the toolbar is a fixed 46 px, so
  both surface on the control's `aria_label` and the zone chip's border rather
  than as a second row. See §11 Q11.

Width budget at the app's 940 px minimum: padding 40 + title ~55 + count ~60 +
search 200 + probe ~230 + units ~70 + add-city ~90 + 7 gaps 70 = 815, leaving
~85 px of slack for the spacer. The search field drops from 240 to 200 px when
the probe is present. Below `window.bounds().width() < 980`, the probe
collapses to a 28 px calendar button (`clocks-probe-compact`) that opens the
same popover with the time field and zone list; the popover is the overlay
pattern from `clocks_panels.rs`, not an inline row.

The control is a sibling of the search input, with its own `InputState`, so
typing in one never steals the other's focus.

## 4. Input behaviour

- Clicking the time field focuses it; the text is plain digits/`:`/space/`am`/
  `pm`, no spinner.
- **Commit on Enter or blur** (not per keystroke): partial text such as `1`
  must not jump the page to 01:00. While the text is unparseable the pill shows
  the error state but the previous probe stays in effect.
- `Esc` in the field clears the text and returns to live (standard "cancel a
  filter" behaviour); `Tab` moves to the zone trigger.
- `Now` clears both the text and the probe, keeps the chosen zone for the next
  probe, and moves focus back to the field.
- A cleared field committed with Enter/blur also returns to live (no error).
- The probe survives `load_clocks()`, a section switch, search edits and drag
  reorder; only `Now`, Esc, an emptied field or an app restart clears it.

## 5. Zone picker

Two-step picker, reusing what the add-city flow already has:

1. The trigger opens a small menu (`clocks-probe-zone-menu`, the overlay
   pattern) listing:
   - `Local time zone` (the system zone) with its resolved offset — always
     first, and the default;
   - each saved city's zone in display order (`clocks-probe-zone-option-{i}`),
     so a probe on "the Tokyo card" is one click;
   - a divider and `Search all zones…`, which opens the existing searchable
     IANA overlay (`available_zones` / `zone_matches` / `picker_rows`) in a
     new "pick a probe zone" mode.
2. Picking a zone re-resolves the typed wall-clock time **in that zone**
   (wall-clock semantics: `09:00` Tokyo → `09:00` New York changes the
   instant). The time text is not rewritten.

`PickerState` gains a `target: PickerTarget { AddCity, ProbeZone }`; the
overlay's confirm handler branches on it. `zone_is_added` / `next_selectable`
keep their behaviour for the AddCity path; the ProbeZone path must not disable
zones that are already saved.

Default zone resolution: `jiff::tz::TimeZone::system()` captured when the probe
is first opened, with `iana_name()` used for the label. If the system zone has
no IANA name (fixed-offset system zones), the label is `Local` and the string
stored on the probe is the system zone's own rendering; if that fails to
resolve, fall back to `UTC`.

## 6. Formatting rules (12/24-hour)

- Parsing accepts both shapes regardless of the preference: `9:05`, `09:05`,
  `9.05`, `2:30 pm`, `2:30pm`, `2 pm`. Digits are unambiguous at 24-hour
  values (`14:30`), so a 12-hour user is never blocked.
- Placeholder follows the preference: `14:30` vs `2:30 pm`.
- After a commit the field keeps the user's typed text verbatim while it
  resolves to the same wall clock, so the caret never fights the user.
- Toggling 12h/24h while a probe is active rewrites the field from the probe
  instant in the new shape (`2:30 pm` ↔ `14:30`); the cards re-render through
  the existing `format_clock(instant, zone, use_24h)`.
- Cards keep their offset (`GMT+8`) and date line, which is what makes the
  probed instant readable across zones; no new strings are introduced.

## 7. Core model: a probed instant that is never stored

The probe is view state, but the timezone math belongs in `kandrium-core`
alongside `format_clock`, so both front-ends can share it. All helpers are
pure and synchronous; none takes a `Connection`.

```rust
// crates/core/src/clocks.rs

/// Why a typed probe time could not be resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeError {
    Empty,
    Format,
    OutOfRange,
}

/// Whether a civil time is unambiguous, skipped, or repeated in `zone`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeAmbiguity {
    Unique,
    Gap,   // spring forward: the wall clock never occurs
    Fold,  // fall back: the wall clock occurs twice
}

/// Today's date in `zone`, taken from the live instant.
pub fn probe_date(now: Timestamp, zone: &TimeZone) -> Date;

/// Parse `text` as a wall-clock time on `date` in `zone`.
/// Uses jiff's `Disambiguation::Compatible` (gap ⇒ shifted forward,
/// fold ⇒ earlier occurrence), matching `civil::DateTime::to_zoned`.
pub fn parse_probe_time(text: &str, date: Date, zone: &TimeZone)
    -> Result<Timestamp, ProbeError>;

/// Same parse plus the DST classification, so the view can render a hint.
pub struct ProbeReading { pub instant: Timestamp, pub ambiguity: ProbeAmbiguity }
pub fn read_probe_time(text: &str, date: Date, zone: &TimeZone)
    -> Result<ProbeReading, ProbeError>;

/// Classify the civil time before disambiguation, for the inline hint.
pub fn probe_ambiguity(date: Date, time: civil::Time, zone: &TimeZone)
    -> ProbeAmbiguity;   // `TimeZone::to_ambiguous_zoned(&dt).offset()`

/// The system zone, resolved once at probe open.
pub fn system_zone() -> TimeZone;      // `TimeZone::system()`
```

View state (`crates/app-gpui/src/views/clocks.rs`):

```rust
pub struct ProbeState {
    pub instant: Timestamp,
    /// IANA identifier, or the system zone's string when it has no name.
    pub zone: String,
    /// Text as typed/committed; kept so the field round-trips.
    pub text: String,
    pub input: Entity<InputState>,
    pub menu_open: bool,
    pub error: Option<String>,
    pub hint: Option<String>,
}

pub struct ClockState {
    // …
    pub probe: Option<ProbeState>,
}

impl ClockState {
    /// The instant every card and every placement reads.
    pub fn instant(&self) -> Timestamp {
        self.probe.as_ref().map_or(self.now, |probe| probe.instant)
    }
}
```

Not persisted anywhere: no schema column, no `settings` key, no session file.
`ClockState::load` must not touch `probe`; restart is live by construction.

## 8. Interaction with the per-frame repaint

- `ensure_clock_tick` (`app.rs` L3335) keeps running exactly as it does now.
  It runs in every section because the titlebar clock is always visible.
- While probing, `now` still advances each wake, but every card renders from
  `state.instant()`, so the visible text does not change between ticks. The
  tick costs one `cx.notify()` per minute and nothing else.
- No new timer, no `request_repaint_after` from the probe: the probe is state,
  not a clock. Editing the field only calls `cx.notify()`.
- `next_wait()` may optionally drop the travel-boundary arm while probing (the
  nearest boundary no longer changes any rendered row). Suggested default for
  v1: **leave it unchanged** — the behaviour is identical and there is one
  fewer branch to get wrong. A database change still arrives through
  `load_clocks()` → `invalidate_clock_tick()`.
- The probe never mutates `now`; `ClockState::set_now` keeps its current
  meaning so the titlebar and `probe_date` stay correct.

## 9. Travel, people and placement

- `city_people` (`clocks.rs` L264–269) and the card's `format_clock` call are
  the only two places that read the instant. Both switch to `self.instant()`.
- `placement_for` / `placement_at` are already pure functions of `(rows,
  instant)`, so visitors, `away_to`, `until` and `upcoming` all follow the
  probe with no new code. Probing the past shows the historical visitor; the
  same row's `until`/`starts` labels stay absolute and truthful.
- `upcoming` is computed as `starts_at > instant`, so a probe before a trip
  shows "starts …", and a probe after it shows the trip as active — this is
  the desired reading and needs no special case.
- Reorder drag, search and the add/remove flows are unaffected: they read
  `entries`/`travels`, never the instant.
- `cancelled_at` still filters trips; probe past a cancellation and the person
  reads as home, which matches the data model.

## 10. Titlebar clock

The titlebar (`app.rs` L5803) keeps rendering `self.clocks.now`, and its city
menu (`format_clock_menu_meta(now, …)`) keeps showing live times. Rationale:
the chrome answers "what time is it" globally, while the page answers "what
did the world read at this moment". The probe is explicit about being a mode
via the accent pill and the `Now` chip, so the two readings cannot be confused.

Suggested micro-affordance (default: skip in v1): a 4 px accent dot before the
titlebar time while a probe is active. It is one `when` and no strings, but it
also risks reading as "the app's clock is wrong", so it stays out unless asked.

## 11. Edge cases

| Case | Behaviour |
| --- | --- |
| Empty field, committed | Back to live; no error |
| Partial text (`1`, `14:`) while typing | No change, no error until commit |
| Unparseable (`abc`) on commit | Red pill + tooltip "Enter a time like 14:30"; previous probe stays; text stays for correction |
| Out of range (`24:00`, `12:60`, `13:00 pm`) | Same as unparseable, message "Enter a time between 00:00 and 23:59" |
| DST gap (2026-03-08 02:30 New York) | Resolves to 03:30 EDT (compatible); hint `02:30 → 03:30` in the pill tooltip |
| DST fold (2026-11-01 01:30 New York) | Earlier occurrence (01:30 EDT, GMT-4); hint "01:30 happens twice; showing the first" |
| Zone with half-hour offset | Nothing special; `format_clock` already prints `GMT+5:30` |
| Fixed-offset / unnamed system zone | Label `Local`, stored as the system zone; `UTC` fallback if it does not resolve |
| Saved city deleted while probing | Probe holds an IANA string, not a clock id, so it keeps working |
| Probe date differs from local date | Cards print their own date line; no special case |
| Search hides the probed zone's card | Filtering is orthogonal; the pill still shows the zone label |
| Window narrower than 980 px | Probe collapses to an icon button; popover keeps both fields |
| System zone changes while the app runs | Captured when the probe opens; not re-resolved per frame |
| Travel written by another process | Watcher → `load_clocks` → placement recomputed at the probe instant |

## 12. Test list

Core unit tests (`crates/core/src/clocks.rs`):

1. `parse_probe_time` accepts `9:05`, `09:05`, `9.05`, `2:30 pm`, `2:30pm`,
   `2 pm`; rejects `""`, `abc`, `24:00`, `12:60`, `0:00 pm`, `13:00 pm`.
2. 24-hour digits parse identically under both preferences.
3. `probe_date` uses the probe zone's date: `2026-03-08T04:30Z` is 8 Mar in
   Paris and 7 Mar in Los Angeles.
4. Spring forward: 2026-03-08 02:30 New York → `07:30Z`, `ProbeAmbiguity::Gap`.
5. Fall back: 2026-11-01 01:30 New York → `05:30Z` (GMT-4), `Fold`; `.later()`
   would be `06:30Z`.
6. Half-hour zone: 09:00 `Asia/Kolkata` → `03:30Z`.
7. Round trip: `format_clock(parse(…), zone, …).time` equals the typed time for
   every non-gap case, in both 12h and 24h shapes.
8. `ClockState::instant()` returns `now` with no probe and the probe otherwise.

Headless UI tests (`crates/app-gpui/tests/clocks.rs`, module-level pure tests
where a window is not needed):

9. Toolbar renders `clocks-probe`, `clocks-probe-time`, `clocks-probe-zone`;
   no `clocks-probe-now` while live.
10. Committing `03:00` with the Tokyo zone sets the probe: `clocks-probe-now`
    appears, the pill gains the accent border, and the card for a second zone
    shows `format_clock(instant, …)` for that instant (assert against a fixed
    `now`, injected as the test does today).
11. The `Now` chip clears the probe and restores live text; Esc does the same.
12. Invalid text leaves the previous probe untouched and sets
    `clocks-probe-error`.
13. Toggling 12h/24h with an active probe rewrites the field text and the
    card time.
14. Probe survives `ClockState::load`, a search edit and a reorder.
15. `city_people` at a probe instant: a 09:00–17:00 trip reads as a visitor at
    10:00 and as home at 18:00.
16. Zone menu lists `Local time zone` first and each saved zone; picking one
    re-resolves the same wall clock in that zone.
17. Layout regression: at 940 px the toolbar has no wrapped/overflowing child
    (assert the probe's right edge is left of the units toggle).

## 13. Open questions (with suggested defaults)

1. **Commit timing** — per keystroke vs Enter/blur. Default: Enter/blur, so a
   half-typed `1` never jumps the page.
2. **Zone switch semantics** — keep the instant and re-render the field, or
   re-read the typed wall clock. Default: wall clock in the new zone (matches
   how the user typed it); keeping the instant is the alternative if the probe
   is meant to be "pin this moment, inspect other zones".
3. **Date scope** — today-in-the-probe-zone only, vs a date field. Default:
   today only; a date row would force the toolbar to grow past 46 px.
4. **Titlebar** — stay live vs follow the probe. Default: stay live.
5. **Fold handling** — always the earlier occurrence, vs a toggle for the
   second one. Default: earlier + hint; a toggle is a later follow-up.
6. **Placement** — evaluate at the probe instant vs always at live now.
   Default: at the probe instant (it is what makes the mode useful).
7. **Persistence** — none vs remember the last probe. Default: none; a probe
   is a transient reading, and a restart should never open in the past.
8. **Tick** — unchanged vs minute-only while probing. Default: unchanged.
9. **Probed zone highlight** — none vs an accent ring on that city's card.
   Default: none; the pill already names the zone.
10. **Default zone** — system zone at open vs the first saved city. Default:
    system zone, per the request; the menu still lists saved cities first.
11. **Error surface** — pill border + tooltip vs a caption row under the
    toolbar. Default: pill + tooltip, because the toolbar is a fixed 46 px and
    a second row would shift the whole grid.

## 14. Files touched

| File | Change |
| --- | --- |
| `crates/core/src/clocks.rs` | `ProbeError`, `ProbeAmbiguity`, `probe_date`, `parse_probe_time`, `probe_ambiguity`, `system_zone`; unit tests |
| `crates/core/src/lib.rs` | Re-export the new helpers if the crate re-exports the rest of `clocks` explicitly |
| `crates/app-gpui/src/views/clocks.rs` | `ProbeState`, `ClockState::probe` + `instant()`, toolbar control, card/`city_people` switch to `instant()`, module tests |
| `crates/app-gpui/src/views/clocks_panels.rs` | `PickerTarget`, probe-zone menu overlay |
| `crates/app-gpui/src/app.rs` | Probe actions (commit, clear, open zone menu, pick zone) and `use_24h` rewrite; no tick/titlebar change |
| `crates/app-gpui/tests/clocks.rs` | Headless cases 9–17 |

## 15. Stage order (one commit per stage)

- **S1 — core math.** Pure helpers + tests; no UI change, no behaviour change.
  Rollback: revert the one commit; the app is unaffected.
- **S2 — control and state.** `ProbeState`, toolbar pill in live mode, Enter/
  blur/Now/Esc, cards render from `instant()`; zone menu with local + saved
  zones. Rollback: the page returns to the previous toolbar.
- **S3 — search-all-zones overlay, 12/24 rewrite, ambiguity hints.** Rollback:
  the probe still works with the quick list only.
- **S4 — placement assertions, narrow-window collapse, captures.** Gates
  (`fmt`, `clippy --all-targets`, tests), rebuild, relaunch, report.
