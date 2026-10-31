//! Vim-style modal editing for the shared markdown editor.
//!
//! The editor stays an [`egui::TextEdit`]: this module only rewrites the input
//! events before the widget sees them, mutates the buffer for Normal/Visual
//! commands, and moves the caret through [`egui::TextEditState`]. Insert mode
//! is plain egui editing, so IME, selection, and clipboard keep working.
//!
//! Supported: `h j k l w W b B e E 0 ^ $ gg G { }`, counts,
//! `f F t T ; ,`, `i I a A o O`, `v V`, `.` repeat,
//! `x X dd D yy Y cc C S p P J r ~ u Ctrl-r`, text objects
//! (`iw aw`, `i" a"`, `i( a(`, `i[ a[`, `i{ a{`, `ip ap`), and `Esc`.
//! Deliberately absent for now: `/` search, `:` ex-commands, marks,
//! registers beyond the unnamed one, `%`, `>>`/`<<`, `s`, and `.` repeat of
//! Visual-mode operations.
//!
//! Text is manipulated as a `Vec<char>` per frame; markdown files are small
//! enough that byte/char bookkeeping is not worth the extra risk.

use egui::text::{CCursor, CCursorRange};
use egui::widgets::text_edit::{TextEditOutput, TextEditState};
use egui::{Event, Id, Key, Modifiers};

use crate::theme::Tokens;

/// Preference key: vim editing is on by default.
fn pref_id() -> Id {
    Id::new("kandrium-vim-enabled")
}

/// Whether vim keys are active (default: yes).
pub fn enabled(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(pref_id())).unwrap_or(true)
}

/// Turn vim keys on or off for this session (persisted by the app).
pub fn set_enabled(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(pref_id(), on));
}

/// Modal state of one editor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VimMode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
}

impl VimMode {
    fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Visual => "VISUAL",
            Self::VisualLine => "V-LINE",
        }
    }
}

/// A text + caret pair used for undo/redo.
#[derive(Clone, Default)]
struct Snapshot {
    text: String,
    cursor: usize,
}

/// The last change, replayed by `.`.
#[derive(Clone, Debug)]
enum Change {
    /// A Normal-mode command (already including any count and pending keys).
    Keys(Vec<char>),
    /// A command that entered Insert mode, plus everything typed until `Esc`.
    InsertSession { keys: Vec<char>, typed: String },
}

/// Everything one editor remembers between frames.
#[derive(Clone, Default)]
struct VimState {
    mode: VimMode,
    count: Option<u32>,
    /// `d`, `y` or `c` waiting for a motion.
    pending_op: Option<char>,
    /// First `g` of `gg`.
    pending_g: bool,
    /// `r` waiting for its replacement character.
    pending_r: bool,
    /// `f`/`F`/`t`/`T` waiting for its target character: (forward, till).
    pending_find: Option<(bool, bool)>,
    /// `i`/`a` after an operator or in Visual mode, waiting for an object.
    pending_text_obj: Option<char>,
    /// Last `f`/`F`/`t`/`T` for `;` and `,`: (target, forward, till).
    last_find: Option<(char, bool, bool)>,
    /// Desired column for a run of `j`/`k` presses.
    goal_col: Option<usize>,
    /// Keys typed for the command in progress, and the text before it.
    cmd_buf: Vec<char>,
    cmd_start: Option<String>,
    /// Set when the completed command must not replace `last_change`.
    suppress_record: bool,
    /// While true, `feed` executes keys without recording anything.
    replaying: bool,
    /// Command currently being replayed by `.`.
    last_change: Option<Change>,
    /// Text typed during the current Insert session.
    recording: Option<String>,
    /// Set by `j`/`k` so `feed` keeps `goal_col`.
    keep_goal: bool,
    /// Visual-mode anchor (character index).
    anchor: usize,
    register: String,
    register_linewise: bool,
    /// Text before the current Insert session, so `u` undoes it in one step.
    inserted_at: Option<Snapshot>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl VimState {
    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1).max(1) as usize
    }

    fn reset_pending(&mut self) {
        self.count = None;
        self.pending_op = None;
        self.pending_g = false;
        self.pending_r = false;
        self.pending_find = None;
        self.pending_text_obj = None;
    }
}

/// Working copy of the edited text for the current frame.
struct Buf {
    chars: Vec<char>,
    cursor: usize,
    changed: bool,
}

impl Buf {
    fn new(text: &str, cursor: usize) -> Self {
        let chars: Vec<char> = text.chars().collect();
        Self {
            cursor: cursor.min(chars.len()),
            chars,
            changed: false,
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.chars.iter().collect(),
            cursor: self.cursor,
        }
    }

    fn splice(&mut self, start: usize, end: usize, insert: &str) {
        let start = start.min(self.chars.len());
        let end = end.min(self.chars.len()).max(start);
        self.chars.splice(start..end, insert.chars());
        self.changed = true;
    }

    fn restore(&mut self, snapshot: &Snapshot) {
        self.chars = snapshot.text.chars().collect();
        self.cursor = snapshot.cursor.min(self.chars.len());
        self.changed = true;
    }

    fn clamp_normal(&mut self) {
        self.cursor = clamp_normal(&self.chars, self.cursor);
    }
}

fn ensure<'a>(buf: &'a mut Option<Buf>, text: &str, cursor: usize) -> &'a mut Buf {
    buf.get_or_insert_with(|| Buf::new(text, cursor))
}

// ---------------------------------------------------------------------------
// Text helpers (character indices)
// ---------------------------------------------------------------------------

fn line_start(c: &[char], i: usize) -> usize {
    let mut i = i.min(c.len());
    while i > 0 && c[i - 1] != '\n' {
        i -= 1;
    }
    i
}

/// Index of the line's `\n`, or `c.len()` when it is the last line.
fn line_end(c: &[char], i: usize) -> usize {
    let mut i = i.min(c.len());
    while i < c.len() && c[i] != '\n' {
        i += 1;
    }
    i
}

fn next_line_start(c: &[char], i: usize) -> usize {
    (line_end(c, i) + 1).min(c.len())
}

fn line_index(c: &[char], i: usize) -> usize {
    c[..i.min(c.len())].iter().filter(|&&ch| ch == '\n').count()
}

fn line_count(c: &[char]) -> usize {
    c.iter().filter(|&&ch| ch == '\n').count() + 1
}

fn line_start_n(c: &[char], n: usize) -> usize {
    let mut line = 0;
    let mut i = 0;
    while i < c.len() {
        if line == n {
            return i;
        }
        if c[i] == '\n' {
            line += 1;
        }
        i += 1;
    }
    c.len()
}

fn first_non_blank(c: &[char], start: usize) -> usize {
    let end = line_end(c, start);
    let mut i = start;
    while i < end && (c[i] == ' ' || c[i] == '\t') {
        i += 1;
    }
    if end > start { i.min(end - 1) } else { start }
}

/// Clamp a caret to the character under it (Normal-mode semantics).
fn clamp_normal(c: &[char], i: usize) -> usize {
    if c.is_empty() {
        return 0;
    }
    let i = i.min(c.len() - 1);
    let start = line_start(c, i);
    let end = line_end(c, i);
    if end > start { i.min(end - 1) } else { start }
}

fn is_space(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\n'
}

fn class_of(c: char, big: bool) -> u8 {
    if is_space(c) {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn word_forward(c: &[char], mut i: usize, big: bool) -> usize {
    let n = c.len();
    if i >= n {
        return n;
    }
    let class = class_of(c[i], big);
    while i < n && !is_space(c[i]) && class_of(c[i], big) == class {
        i += 1;
    }
    while i < n && is_space(c[i]) {
        i += 1;
    }
    i
}

fn word_back(c: &[char], mut i: usize, big: bool) -> usize {
    if i == 0 || c.is_empty() {
        return 0;
    }
    i = i.min(c.len()) - 1;
    while i > 0 && is_space(c[i]) {
        i -= 1;
    }
    let class = class_of(c[i], big);
    while i > 0 && !is_space(c[i - 1]) && class_of(c[i - 1], big) == class {
        i -= 1;
    }
    i
}

fn word_end(c: &[char], mut i: usize, big: bool) -> usize {
    let n = c.len();
    if n == 0 {
        return 0;
    }
    if i + 1 >= n {
        return n - 1;
    }
    i += 1;
    while i < n && is_space(c[i]) {
        i += 1;
    }
    if i >= n {
        return n - 1;
    }
    let class = class_of(c[i], big);
    while i + 1 < n && !is_space(c[i + 1]) && class_of(c[i + 1], big) == class {
        i += 1;
    }
    i
}

fn paragraph_forward(c: &[char], from: usize) -> usize {
    let mut i = line_end(c, from);
    while i < c.len() {
        let next = i + 1;
        if next >= c.len() {
            break;
        }
        let end = line_end(c, next);
        if end == next {
            return next;
        }
        i = end;
    }
    c.len().saturating_sub(1)
}

fn paragraph_back(c: &[char], from: usize) -> usize {
    let mut i = line_start(c, from);
    while i > 0 {
        let prev_start = line_start(c, i - 1);
        let prev_end = line_end(c, prev_start);
        if prev_end == prev_start {
            return prev_start;
        }
        i = prev_start;
    }
    0
}

// ---------------------------------------------------------------------------
// Motions
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum MotionKind {
    Exclusive,
    Inclusive,
    Linewise,
}

struct Motion {
    index: usize,
    kind: MotionKind,
}

fn motion_for(
    c: &[char],
    cursor: usize,
    ch: char,
    count: usize,
    goal: Option<usize>,
) -> Option<Motion> {
    if c.is_empty() {
        return None;
    }
    let cursor = cursor.min(c.len() - 1);
    let start = line_start(c, cursor);
    let end = line_end(c, cursor);
    let last = if end > start { end - 1 } else { start };
    let motion = |index, kind| Some(Motion { index, kind });
    match ch {
        'h' => motion(
            cursor.saturating_sub(count).max(start),
            MotionKind::Exclusive,
        ),
        'l' => motion((cursor + count).min(last), MotionKind::Inclusive),
        '0' => motion(start, MotionKind::Exclusive),
        '^' => motion(first_non_blank(c, start), MotionKind::Exclusive),
        '$' => motion(last, MotionKind::Inclusive),
        'j' | 'k' => {
            let col = goal.unwrap_or(cursor - start);
            let line = line_index(c, cursor);
            let target = if ch == 'j' {
                (line + count).min(line_count(c) - 1)
            } else {
                line.saturating_sub(count)
            };
            let tstart = line_start_n(c, target);
            let tend = line_end(c, tstart);
            let tlast = if tend > tstart { tend - 1 } else { tstart };
            motion(tstart + col.min(tlast - tstart), MotionKind::Linewise)
        }
        'w' | 'W' => {
            let big = ch == 'W';
            let mut i = cursor;
            for _ in 0..count {
                i = word_forward(c, i, big);
            }
            motion(i.min(c.len().saturating_sub(1)), MotionKind::Exclusive)
        }
        'b' | 'B' => {
            let big = ch == 'B';
            let mut i = cursor;
            for _ in 0..count {
                i = word_back(c, i, big);
            }
            motion(i, MotionKind::Exclusive)
        }
        'e' | 'E' => {
            let big = ch == 'E';
            let mut i = cursor;
            for _ in 0..count {
                i = word_end(c, i, big);
            }
            motion(i, MotionKind::Inclusive)
        }
        '{' => {
            let mut i = cursor;
            for _ in 0..count {
                i = paragraph_back(c, i);
            }
            motion(i, MotionKind::Exclusive)
        }
        '}' => {
            let mut i = cursor;
            for _ in 0..count {
                i = paragraph_forward(c, i);
            }
            motion(i, MotionKind::Exclusive)
        }
        _ => None,
    }
}

/// `f`/`F`/`t`/`T` motion. Searches the current line only, like Vim.
fn find_motion(
    c: &[char],
    cursor: usize,
    target: char,
    forward: bool,
    till: bool,
    count: usize,
) -> Option<Motion> {
    let cursor = cursor.min(c.len().checked_sub(1)?);
    let start = line_start(c, cursor);
    let end = line_end(c, cursor);
    let count = count.max(1);
    if forward {
        let mut at = cursor;
        for _ in 0..count {
            let from = at + 1;
            if from >= end {
                return None;
            }
            at = from + c[from..end].iter().position(|&x| x == target)?;
        }
        if till {
            if at <= cursor + 1 {
                return None;
            }
            Some(Motion {
                index: at - 1,
                kind: MotionKind::Inclusive,
            })
        } else {
            Some(Motion {
                index: at,
                kind: MotionKind::Inclusive,
            })
        }
    } else {
        let mut at = cursor;
        for _ in 0..count {
            if at <= start {
                return None;
            }
            at = start + c[start..at].iter().rposition(|&x| x == target)?;
        }
        if till {
            if at + 1 >= cursor {
                return None;
            }
            Some(Motion {
                index: at + 1,
                kind: MotionKind::Inclusive,
            })
        } else {
            Some(Motion {
                index: at,
                kind: MotionKind::Inclusive,
            })
        }
    }
}

/// Which find motion a key starts: (forward, till).
fn find_prefix(ch: char) -> Option<(bool, bool)> {
    match ch {
        'f' => Some((true, false)),
        'F' => Some((false, false)),
        't' => Some((true, true)),
        'T' => Some((false, true)),
        _ => None,
    }
}

fn goto_line(b: &mut Buf, line: usize) {
    let line = line.min(line_count(&b.chars).saturating_sub(1));
    let start = line_start_n(&b.chars, line);
    b.cursor = first_non_blank(&b.chars, start);
}

// ---------------------------------------------------------------------------
// Text objects
// ---------------------------------------------------------------------------

/// Resolve an `i`/`a` text object at `cursor`, returning `[start, end)`.
fn text_object(c: &[char], cursor: usize, around: bool, obj: char) -> Option<(usize, usize)> {
    match obj {
        'w' => word_object(c, cursor, around),
        '"' => quote_object(c, cursor, around, '"'),
        '\'' => quote_object(c, cursor, around, '\''),
        '`' => quote_object(c, cursor, around, '`'),
        '(' | ')' | 'b' => bracket_object(c, cursor, around, '(', ')'),
        '[' | ']' => bracket_object(c, cursor, around, '[', ']'),
        '{' | '}' | 'B' => bracket_object(c, cursor, around, '{', '}'),
        '<' | '>' => bracket_object(c, cursor, around, '<', '>'),
        'p' => paragraph_object(c, cursor, around),
        _ => None,
    }
}

fn word_object(c: &[char], cursor: usize, around: bool) -> Option<(usize, usize)> {
    if c.is_empty() {
        return None;
    }
    let i = cursor.min(c.len() - 1);
    let mut start = i;
    let mut end = i + 1;
    if c[i] == '\n' {
        return Some((i, i + 1));
    }
    if c[i] == ' ' || c[i] == '\t' {
        while start > 0 && (c[start - 1] == ' ' || c[start - 1] == '\t') {
            start -= 1;
        }
        while end < c.len() && (c[end] == ' ' || c[end] == '\t') {
            end += 1;
        }
    } else {
        let class = class_of(c[i], false);
        while start > 0 && !is_space(c[start - 1]) && class_of(c[start - 1], false) == class {
            start -= 1;
        }
        while end < c.len() && !is_space(c[end]) && class_of(c[end], false) == class {
            end += 1;
        }
    }
    if around {
        let mut trailing = end;
        while trailing < c.len() && (c[trailing] == ' ' || c[trailing] == '\t') {
            trailing += 1;
        }
        if trailing > end {
            end = trailing;
        } else {
            while start > 0 && (c[start - 1] == ' ' || c[start - 1] == '\t') {
                start -= 1;
            }
        }
    }
    Some((start, end))
}

fn quote_object(c: &[char], cursor: usize, around: bool, q: char) -> Option<(usize, usize)> {
    if c.is_empty() {
        return None;
    }
    let cursor = cursor.min(c.len() - 1);
    let start = line_start(c, cursor);
    let end = line_end(c, cursor);
    let quotes: Vec<usize> = (start..end).filter(|&i| c[i] == q).collect();
    let mut k = 0;
    while k + 1 < quotes.len() {
        let (open, close) = (quotes[k], quotes[k + 1]);
        if cursor >= open && cursor <= close {
            return Some(if around {
                (open, close + 1)
            } else {
                (open + 1, close)
            });
        }
        k += 2;
    }
    None
}

fn bracket_object(
    c: &[char],
    cursor: usize,
    around: bool,
    open: char,
    close: char,
) -> Option<(usize, usize)> {
    if c.is_empty() {
        return None;
    }
    let cursor = cursor.min(c.len() - 1);
    let mut depth = 0usize;
    let mut k = cursor;
    let open_idx = loop {
        if c[k] == close {
            depth += 1;
        } else if c[k] == open {
            if depth == 0 {
                break k;
            }
            depth -= 1;
        }
        if k == 0 {
            return None;
        }
        k -= 1;
    };
    let mut depth = 0usize;
    let mut j = open_idx + 1;
    while j < c.len() {
        if c[j] == open {
            depth += 1;
        } else if c[j] == close {
            if depth == 0 {
                return Some(if around {
                    (open_idx, j + 1)
                } else {
                    (open_idx + 1, j)
                });
            }
            depth -= 1;
        }
        j += 1;
    }
    None
}

fn paragraph_object(c: &[char], cursor: usize, around: bool) -> Option<(usize, usize)> {
    if c.is_empty() {
        return None;
    }
    let total = line_count(c);
    let line = line_index(c, cursor.min(c.len() - 1));
    let blank = |n: usize| {
        let s = line_start_n(c, n);
        line_end(c, s) == s
    };
    let mut first = line;
    let mut last = line;
    if blank(line) {
        while first > 0 && blank(first - 1) {
            first -= 1;
        }
        while last + 1 < total && blank(last + 1) {
            last += 1;
        }
    } else {
        while first > 0 && !blank(first - 1) {
            first -= 1;
        }
        while last + 1 < total && !blank(last + 1) {
            last += 1;
        }
    }
    let mut after = last + 1;
    if around {
        if after < total && blank(after) {
            after += 1;
        } else if first > 0 && blank(first - 1) {
            first -= 1;
        }
    }
    let start = line_start_n(c, first);
    let end = if after >= total {
        c.len()
    } else {
        line_start_n(c, after)
    };
    Some((start, end))
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn push_snapshot(state: &mut VimState, b: &Buf) {
    state.undo.push(b.snapshot());
    state.redo.clear();
}

/// True when no command prefix is waiting for more input.
fn idle(state: &VimState) -> bool {
    state.count.is_none()
        && state.pending_op.is_none()
        && !state.pending_g
        && !state.pending_r
        && state.pending_find.is_none()
        && state.pending_text_obj.is_none()
}

fn feed(state: &mut VimState, b: &mut Buf, ch: char) {
    if !state.replaying {
        if state.cmd_start.is_none() {
            state.cmd_start = Some(b.snapshot().text);
        }
        state.cmd_buf.push(ch);
    }
    state.keep_goal = false;
    feed_keys(state, b, ch);
    finish_command(state, b);
    if idle(state) && !state.keep_goal {
        state.goal_col = None;
    }
}

/// Remember the command that just completed as the `.` target.
fn finish_command(state: &mut VimState, b: &Buf) {
    if state.replaying || !idle(state) {
        return;
    }
    let Some(start) = state.cmd_start.take() else {
        state.cmd_buf.clear();
        state.suppress_record = false;
        return;
    };
    let keys = std::mem::take(&mut state.cmd_buf);
    let suppress = std::mem::take(&mut state.suppress_record);
    if keys.is_empty() || suppress {
        return;
    }
    if state.mode == VimMode::Insert {
        state.last_change = Some(Change::InsertSession {
            keys,
            typed: String::new(),
        });
        state.recording = Some(String::new());
        return;
    }
    if state.mode == VimMode::Normal && b.snapshot().text != start {
        state.last_change = Some(Change::Keys(keys));
    }
}

/// Re-execute the last change `count` times.
fn replay(state: &mut VimState, b: &mut Buf, change: &Change, count: usize) {
    for _ in 0..count.max(1) {
        match change {
            Change::Keys(keys) => {
                state.replaying = true;
                for &ch in keys {
                    feed(state, b, ch);
                }
                state.replaying = false;
            }
            Change::InsertSession { keys, typed } => {
                state.replaying = true;
                for &ch in keys {
                    feed(state, b, ch);
                }
                state.replaying = false;
                if state.mode != VimMode::Insert {
                    continue;
                }
                if !typed.is_empty() {
                    let at = b.cursor.min(b.chars.len());
                    b.splice(at, at, typed);
                }
                // Same effect as pressing `Esc` after the insert session.
                let start = line_start(&b.chars, b.cursor);
                b.cursor = b.cursor.saturating_sub(1).max(start);
                state.mode = VimMode::Normal;
                state.reset_pending();
                if let Some(snapshot) = state.inserted_at.take() {
                    let current = b.snapshot();
                    let already = state
                        .undo
                        .last()
                        .is_some_and(|entry| entry.text == snapshot.text);
                    if current.text != snapshot.text && !already {
                        state.undo.push(snapshot);
                        state.redo.clear();
                    }
                }
                b.clamp_normal();
            }
        }
    }
}

/// Apply `;` (repeat) or `,` (repeat reversed) to the stored find.
fn repeat_find(state: &mut VimState, b: &mut Buf, ch: char, count: usize) {
    let Some((target, forward, till)) = state.last_find else {
        state.pending_op = None;
        return;
    };
    let forward = if ch == ',' { !forward } else { forward };
    match find_motion(&b.chars, b.cursor, target, forward, till, count) {
        Some(m) => {
            if let Some(op) = state.pending_op.take() {
                operate_motion(state, b, op, m);
            } else {
                b.cursor = m.index;
                b.clamp_normal();
            }
        }
        None => state.pending_op = None,
    }
}

fn feed_keys(state: &mut VimState, b: &mut Buf, ch: char) {
    // `r` consumes the next character verbatim.
    if state.pending_r {
        state.pending_r = false;
        let count = state.take_count();
        replace_chars(state, b, ch, count);
        return;
    }

    // `f`/`F`/`t`/`T` consume their target character here, so that digits
    // after them stay valid targets rather than counts.
    if let Some((forward, till)) = state.pending_find {
        state.pending_find = None;
        let count = state.take_count();
        state.last_find = Some((ch, forward, till));
        let found = find_motion(&b.chars, b.cursor, ch, forward, till, count);
        match (found, state.pending_op.take()) {
            (Some(m), Some(op)) => operate_motion(state, b, op, m),
            (Some(m), None) => {
                b.cursor = m.index;
                b.clamp_normal();
            }
            (None, _) => {}
        }
        return;
    }

    // Count prefix (`0` starts a count only when one is already open).
    if ch.is_ascii_digit() && (ch != '0' || state.count.is_some()) {
        let digit = ch as u32 - '0' as u32;
        state.count = Some(
            state
                .count
                .unwrap_or(0)
                .saturating_mul(10)
                .saturating_add(digit),
        );
        return;
    }

    // Visual mode: text-object prefixes and operators.
    if state.mode != VimMode::Normal {
        if let Some(prefix) = state.pending_text_obj.take() {
            if let Some((start, end)) = text_object(&b.chars, b.cursor, prefix == 'a', ch) {
                state.anchor = start;
                b.cursor = end.saturating_sub(1).max(start);
                b.clamp_normal();
            }
            return;
        }
        match ch {
            'i' | 'a' => {
                state.pending_text_obj = Some(ch);
                return;
            }
            'd' | 'y' | 'c' => {
                // Repeated Visual operations are out of scope for `.`.
                state.suppress_record = true;
                visual_operate(state, b, ch);
                return;
            }
            'o' => {
                std::mem::swap(&mut state.anchor, &mut b.cursor);
                b.clamp_normal();
                return;
            }
            'v' => {
                state.mode = if state.mode == VimMode::Visual {
                    VimMode::Normal
                } else {
                    VimMode::Visual
                };
                state.reset_pending();
                return;
            }
            'V' => {
                state.mode = if state.mode == VimMode::VisualLine {
                    VimMode::Normal
                } else {
                    VimMode::VisualLine
                };
                state.reset_pending();
                return;
            }
            _ => {}
        }
    }

    // `gg`: either `g` goes to a line number or the first line.
    if state.pending_g {
        state.pending_g = false;
        if ch == 'g' {
            let line = state.count.unwrap_or(1).max(1) as usize - 1;
            goto_line(b, line);
        }
        state.count = None;
        return;
    }

    // Pending operator: text objects, find motions, repeats, or a motion.
    if let Some(op) = state.pending_op {
        if state.pending_text_obj.is_none() && (ch == 'i' || ch == 'a') {
            state.pending_text_obj = Some(ch);
            return;
        }
        if let Some(prefix) = state.pending_text_obj.take() {
            state.pending_op = None;
            state.take_count();
            if let Some((start, end)) = text_object(&b.chars, b.cursor, prefix == 'a', ch) {
                apply_range(state, b, op, start, end);
            }
            return;
        }
        if let Some((forward, till)) = find_prefix(ch) {
            state.pending_find = Some((forward, till));
            return;
        }
        if ch == ';' || ch == ',' {
            let count = state.take_count();
            repeat_find(state, b, ch, count);
            return;
        }
        state.pending_op = None;
        let count = state.take_count();
        operate(state, b, op, ch, count);
        return;
    }

    match ch {
        'h' | 'j' | 'k' | 'l' | 'w' | 'W' | 'b' | 'B' | 'e' | 'E' | '0' | '^' | '$' | '{' | '}' => {
            let count = state.take_count();
            let col = state
                .goal_col
                .unwrap_or_else(|| b.cursor.saturating_sub(line_start(&b.chars, b.cursor)));
            if let Some(motion) = motion_for(&b.chars, b.cursor, ch, count, Some(col)) {
                b.cursor = motion.index;
                b.clamp_normal();
                if ch == 'j' || ch == 'k' {
                    state.goal_col = Some(col);
                    state.keep_goal = true;
                }
            }
        }
        'g' => state.pending_g = true,
        'G' => {
            let target = match state.count.take() {
                Some(n) => (n.max(1) as usize).saturating_sub(1),
                None => line_count(&b.chars).saturating_sub(1),
            };
            goto_line(b, target);
        }
        'd' | 'y' | 'c' => state.pending_op = Some(ch),
        'D' => {
            let count = state.take_count();
            operate_to_line_end(state, b, 'd', count);
        }
        'Y' => {
            let count = state.take_count();
            yank_lines(state, b, count);
        }
        'C' => {
            let count = state.take_count();
            operate_to_line_end(state, b, 'c', count);
        }
        'S' => {
            let count = state.take_count();
            operate_linewise(state, b, 'c', count);
        }
        'x' => {
            let count = state.take_count();
            delete_chars(state, b, count);
        }
        'X' => {
            let count = state.take_count();
            delete_before(state, b, count);
        }
        'p' | 'P' => {
            let count = state.take_count();
            paste(state, b, ch == 'P', count);
        }
        'J' => {
            let count = state.take_count();
            join_lines(state, b, count);
        }
        'r' => state.pending_r = true,
        '~' => {
            let count = state.take_count();
            toggle_case(state, b, count);
        }
        'f' | 'F' | 't' | 'T' => {
            state.pending_find = find_prefix(ch);
        }
        ';' | ',' => {
            let count = state.take_count();
            repeat_find(state, b, ch, count);
        }
        '.' => {
            let count = state.take_count();
            state.suppress_record = true;
            if let Some(change) = state.last_change.clone() {
                replay(state, b, &change, count);
            }
        }
        'u' => {
            state.suppress_record = true;
            undo(state, b);
        }
        'i' | 'I' | 'a' | 'A' | 'o' | 'O' => {
            let count = state.take_count();
            enter_insert(state, b, ch, count);
        }
        'v' | 'V' => {
            state.reset_pending();
            state.anchor = b.cursor;
            state.mode = if ch == 'v' {
                VimMode::Visual
            } else {
                VimMode::VisualLine
            };
        }
        _ => state.count = None,
    }
}

fn enter_insert(state: &mut VimState, b: &mut Buf, ch: char, count: usize) {
    state.inserted_at = Some(b.snapshot());
    let start = line_start(&b.chars, b.cursor);
    let end = line_end(&b.chars, b.cursor);
    let indent: String = b.chars[start..end]
        .iter()
        .take_while(|&&ch| ch == ' ' || ch == '\t')
        .collect();
    match ch {
        'i' => {}
        'I' => b.cursor = first_non_blank(&b.chars, start),
        'a' => {
            b.cursor = if b.cursor < end { b.cursor + 1 } else { end };
        }
        'A' => b.cursor = end,
        'o' | 'O' => {
            let at = if ch == 'o' { end } else { start };
            let mut line = String::from("\n");
            line.push_str(&indent);
            let payload = line.repeat(count.max(1));
            b.splice(at, at, &payload);
            b.cursor = at + 1 + indent.chars().count();
        }
        _ => {}
    }
    state.mode = VimMode::Insert;
    state.goal_col = None;
    state.reset_pending();
}

fn replace_chars(state: &mut VimState, b: &mut Buf, ch: char, count: usize) {
    let end = line_end(&b.chars, b.cursor);
    let stop = (b.cursor + count).min(end);
    if stop <= b.cursor {
        return;
    }
    push_snapshot(state, b);
    for slot in &mut b.chars[b.cursor..stop] {
        *slot = ch;
    }
    b.changed = true;
}

fn toggle_case(state: &mut VimState, b: &mut Buf, count: usize) {
    let end = line_end(&b.chars, b.cursor);
    let stop = (b.cursor + count).min(end);
    if stop <= b.cursor {
        return;
    }
    push_snapshot(state, b);
    for slot in &mut b.chars[b.cursor..stop] {
        *slot = if slot.is_uppercase() {
            slot.to_lowercase().next().unwrap_or(*slot)
        } else if slot.is_lowercase() {
            slot.to_uppercase().next().unwrap_or(*slot)
        } else {
            *slot
        };
    }
    b.changed = true;
    b.cursor = stop.saturating_sub(1).max(line_start(&b.chars, b.cursor));
    b.clamp_normal();
}

fn delete_chars(state: &mut VimState, b: &mut Buf, count: usize) {
    let end = line_end(&b.chars, b.cursor);
    let stop = (b.cursor + count).min(end);
    if stop <= b.cursor {
        return;
    }
    push_snapshot(state, b);
    let cut: String = b.chars[b.cursor..stop].iter().collect();
    state.register = cut;
    state.register_linewise = false;
    b.splice(b.cursor, stop, "");
    b.clamp_normal();
}

fn delete_before(state: &mut VimState, b: &mut Buf, count: usize) {
    let start = line_start(&b.chars, b.cursor);
    let from = b.cursor.saturating_sub(count).max(start);
    if from >= b.cursor {
        return;
    }
    push_snapshot(state, b);
    let cut: String = b.chars[from..b.cursor].iter().collect();
    state.register = cut;
    state.register_linewise = false;
    b.splice(from, b.cursor, "");
    b.cursor = from;
    b.clamp_normal();
}

fn yank_lines(state: &mut VimState, b: &mut Buf, count: usize) {
    let line = line_index(&b.chars, b.cursor);
    let start = line_start_n(&b.chars, line);
    let end = line_start_n(&b.chars, line.saturating_add(count.max(1)));
    state.register = b.chars[start..end].iter().collect();
    state.register_linewise = true;
    b.cursor = start;
}

fn operate(state: &mut VimState, b: &mut Buf, op: char, motion: char, count: usize) {
    if motion == op {
        operate_linewise(state, b, op, count);
        return;
    }
    // Vim changes `cw` into `ce` so the trailing space survives.
    let motion = match (op, motion) {
        ('c', 'w') => 'e',
        ('c', 'W') => 'E',
        _ => motion,
    };
    let Some(m) = motion_for(&b.chars, b.cursor, motion, count, None) else {
        return;
    };
    operate_motion(state, b, op, m);
}

fn operate_motion(state: &mut VimState, b: &mut Buf, op: char, m: Motion) {
    let cursor = b.cursor;
    let mut start;
    let mut end;
    let linewise = m.kind == MotionKind::Linewise;
    if linewise {
        start = line_start(&b.chars, cursor.min(m.index));
        end = next_line_start(&b.chars, cursor.max(m.index));
    } else {
        let (lo, hi, inclusive) = if m.index >= cursor {
            (cursor, m.index, m.kind == MotionKind::Inclusive)
        } else {
            (m.index, cursor, m.kind == MotionKind::Inclusive)
        };
        start = lo;
        end = if inclusive {
            (hi + 1).min(b.chars.len())
        } else {
            hi
        };
        // `dw` on the last word must not swallow the line break.
        if !inclusive && end > start && end <= b.chars.len() && end > 0 && b.chars[end - 1] == '\n'
        {
            end -= 1;
            while end > start && (b.chars[end - 1] == ' ' || b.chars[end - 1] == '\t') {
                end -= 1;
            }
        }
    }
    start = start.min(b.chars.len());
    end = end.min(b.chars.len()).max(start);

    push_snapshot(state, b);
    let cut: String = b.chars[start..end].iter().collect();
    match op {
        'y' => {
            state.register = cut;
            state.register_linewise = linewise;
            b.cursor = start;
        }
        'd' | 'c' => {
            state.register = cut;
            state.register_linewise = linewise;
            b.splice(start, end, "");
            b.cursor = start;
            if op == 'c' {
                state.mode = VimMode::Insert;
                state.inserted_at = state.undo.last().cloned();
            } else {
                b.clamp_normal();
            }
        }
        _ => {}
    }
}

fn operate_linewise(state: &mut VimState, b: &mut Buf, op: char, count: usize) {
    let line = line_index(&b.chars, b.cursor);
    let start = line_start_n(&b.chars, line);
    let end = line_start_n(
        &b.chars,
        line.saturating_add(count.max(1)).min(line_count(&b.chars)),
    );
    push_snapshot(state, b);
    let cut: String = b.chars[start..end].iter().collect();
    state.register = cut;
    state.register_linewise = true;
    match op {
        'y' => b.cursor = start,
        'd' => {
            b.splice(start, end, "");
            b.cursor = start;
            b.clamp_normal();
        }
        'c' => {
            b.splice(start, end, "");
            b.cursor = start;
            state.mode = VimMode::Insert;
            state.inserted_at = state.undo.last().cloned();
        }
        _ => {}
    }
}

/// `D`/`C`: with a count, reach the end of the line `count - 1` lines down.
fn operate_to_line_end(state: &mut VimState, b: &mut Buf, op: char, count: usize) {
    if count <= 1 {
        operate(state, b, op, '$', count);
        return;
    }
    let line = line_index(&b.chars, b.cursor);
    let start_of_target = line_start_n(&b.chars, line.saturating_add(count - 1));
    let end = line_end(&b.chars, start_of_target);
    let start = b.cursor;
    if end <= start {
        return;
    }
    apply_range(state, b, op, start, end);
}

/// Operate on an explicit characterwise `[start, end)` range (text objects).
fn apply_range(state: &mut VimState, b: &mut Buf, op: char, start: usize, end: usize) {
    let start = start.min(b.chars.len());
    let end = end.min(b.chars.len()).max(start);
    push_snapshot(state, b);
    let cut: String = b.chars[start..end].iter().collect();
    match op {
        'y' => {
            state.register = cut;
            state.register_linewise = false;
            b.cursor = start;
            b.clamp_normal();
        }
        'd' | 'c' => {
            state.register = cut;
            state.register_linewise = false;
            b.splice(start, end, "");
            b.cursor = start;
            if op == 'c' {
                state.mode = VimMode::Insert;
                state.inserted_at = state.undo.last().cloned();
            } else {
                b.clamp_normal();
            }
        }
        _ => {}
    }
}

fn visual_operate(state: &mut VimState, b: &mut Buf, op: char) {
    let cursor = b.cursor;
    let anchor = state.anchor;
    let linewise = state.mode == VimMode::VisualLine;
    let (start, end) = if linewise {
        (
            line_start(&b.chars, cursor.min(anchor)),
            next_line_start(&b.chars, cursor.max(anchor)),
        )
    } else {
        let lo = cursor.min(anchor);
        let hi = cursor.max(anchor);
        (lo, (hi + 1).min(b.chars.len()))
    };
    push_snapshot(state, b);
    let cut: String = b.chars[start..end.min(b.chars.len())].iter().collect();
    state.register = cut;
    state.register_linewise = linewise;
    state.mode = VimMode::Normal;
    match op {
        'y' => {
            b.cursor = start;
            b.clamp_normal();
        }
        'd' => {
            b.splice(start, end, "");
            b.cursor = start;
            b.clamp_normal();
        }
        'c' => {
            b.splice(start, end, "");
            b.cursor = start;
            state.mode = VimMode::Insert;
            state.inserted_at = state.undo.last().cloned();
        }
        _ => {}
    }
}

fn paste(state: &mut VimState, b: &mut Buf, before: bool, count: usize) {
    if state.register.is_empty() {
        return;
    }
    push_snapshot(state, b);
    let repeated = state.register.repeat(count);
    if state.register_linewise {
        let payload = repeated.trim_end_matches('\n').to_string();
        if before {
            let at = line_start(&b.chars, b.cursor);
            let mut insert = payload.clone();
            insert.push('\n');
            b.splice(at, at, &insert);
            b.cursor = at;
        } else {
            let end = line_end(&b.chars, b.cursor);
            if end < b.chars.len() {
                let at = end + 1;
                let mut insert = payload.clone();
                insert.push('\n');
                b.splice(at, at, &insert);
                b.cursor = at;
            } else {
                let at = end;
                let mut insert = String::from("\n");
                insert.push_str(&payload);
                b.splice(at, at, &insert);
                b.cursor = at + 1;
            }
        }
    } else {
        let at = if before {
            b.cursor
        } else {
            (b.cursor + 1).min(b.chars.len())
        };
        b.splice(at, at, &repeated);
        b.cursor = at + repeated.chars().count().saturating_sub(1);
    }
    b.clamp_normal();
}

fn join_lines(state: &mut VimState, b: &mut Buf, count: usize) {
    push_snapshot(state, b);
    for _ in 0..count.max(1) {
        let end = line_end(&b.chars, b.cursor);
        if end >= b.chars.len() {
            break;
        }
        let mut next = end + 1;
        while next < b.chars.len() && (b.chars[next] == ' ' || b.chars[next] == '\t') {
            next += 1;
        }
        let space = if end > 0 && (b.chars[end - 1] == ' ' || b.chars[end - 1] == '\t') {
            ""
        } else {
            " "
        };
        b.splice(end, next, space);
    }
    b.cursor = line_end(&b.chars, b.cursor).saturating_sub(1);
    b.clamp_normal();
}

fn undo(state: &mut VimState, b: &mut Buf) {
    let Some(previous) = state.undo.pop() else {
        return;
    };
    state.redo.push(b.snapshot());
    b.restore(&previous);
}

fn redo(state: &mut VimState, b: &mut Buf) {
    let Some(next) = state.redo.pop() else {
        return;
    };
    state.undo.push(b.snapshot());
    b.restore(&next);
}

// ---------------------------------------------------------------------------
// Input plumbing
// ---------------------------------------------------------------------------

fn is_escape(key: Key, mods: Modifiers) -> bool {
    key == Key::Escape || (mods.ctrl && key == Key::OpenBracket)
}

fn escape(state: &mut VimState, b: &mut Buf) {
    match state.mode {
        VimMode::Insert => {
            if let Some(snapshot) = state.inserted_at.take() {
                let current = b.snapshot();
                let already = state
                    .undo
                    .last()
                    .is_some_and(|entry| entry.text == snapshot.text);
                if current.text != snapshot.text && !already {
                    state.undo.push(snapshot);
                    state.redo.clear();
                }
            }
            let start = line_start(&b.chars, b.cursor);
            b.cursor = b.cursor.saturating_sub(1).max(start);
        }
        VimMode::Visual | VimMode::VisualLine => {
            b.cursor = b.cursor.min(state.anchor);
        }
        VimMode::Normal => {}
    }
    // Finalize the `.` target for an Insert session.
    if let Some(typed) = state.recording.take()
        && let Some(Change::InsertSession { typed: stored, .. }) = state.last_change.as_mut()
    {
        *stored = typed;
    }
    state.mode = VimMode::Normal;
    state.reset_pending();
    state.goal_col = None;
    state.cmd_buf.clear();
    state.cmd_start = None;
    state.suppress_record = false;
    b.clamp_normal();
}

fn special_key(state: &mut VimState, b: &mut Buf, key: Key, mods: Modifiers) -> bool {
    match key {
        Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown => false,
        Key::Home | Key::End | Key::PageUp | Key::PageDown => false,
        Key::Enter => {
            state.goal_col = None;
            let line = line_index(&b.chars, b.cursor) + 1;
            goto_line(b, line);
            true
        }
        Key::R if mods.ctrl => {
            cancel_command(state);
            redo(state, b);
            true
        }
        Key::Z if mods.ctrl => {
            cancel_command(state);
            undo(state, b);
            true
        }
        Key::C if mods.ctrl => {
            state.mode = VimMode::Normal;
            cancel_command(state);
            true
        }
        Key::D if mods.ctrl => {
            state.goal_col = None;
            half_page(b, true);
            true
        }
        Key::U if mods.ctrl => {
            state.goal_col = None;
            half_page(b, false);
            true
        }
        Key::F if mods.ctrl => {
            state.goal_col = None;
            page(b, true);
            true
        }
        Key::B if mods.ctrl => {
            state.goal_col = None;
            page(b, false);
            true
        }
        Key::Backspace | Key::Delete | Key::Tab => true,
        _ => true,
    }
}

/// Drop the command being assembled (it is not a `.` candidate).
fn cancel_command(state: &mut VimState) {
    state.reset_pending();
    state.cmd_buf.clear();
    state.cmd_start = None;
    state.suppress_record = false;
}

fn half_page(b: &mut Buf, down: bool) {
    let target = if down {
        line_index(&b.chars, b.cursor) + 10
    } else {
        line_index(&b.chars, b.cursor).saturating_sub(10)
    };
    goto_line(b, target);
}

fn page(b: &mut Buf, down: bool) {
    let target = if down {
        line_index(&b.chars, b.cursor) + 20
    } else {
        line_index(&b.chars, b.cursor).saturating_sub(20)
    };
    goto_line(b, target);
}

/// Intercept keys before the `TextEdit` runs. Returns true when the buffer
/// changed (so autosave can pick it up).
pub fn pre_edit(ctx: &egui::Context, id: Id, text: &mut String) -> bool {
    if !enabled(ctx) || !ctx.memory(|m| m.has_focus(id)) {
        return false;
    }
    let Some(mut edit_state) = TextEditState::load(ctx, id) else {
        return false;
    };
    let mut state: VimState = ctx
        .data(|d| d.get_temp::<VimState>(id.with("vim")))
        .unwrap_or_default();
    let range = edit_state.cursor.char_range();
    let mut cursor = range.map(|r| r.primary.index.0).unwrap_or(0);
    let mut buf: Option<Buf> = None;
    let mut changed = false;

    let events = ctx.input_mut(|i| std::mem::take(&mut i.events));
    let mut keep: Vec<Event> = Vec::with_capacity(events.len());
    for event in events {
        if let Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } = &event
            && is_escape(*key, *modifiers)
        {
            let b = ensure(&mut buf, text, cursor);
            escape(&mut state, b);
            cursor = b.cursor;
            continue;
        }

        if state.mode == VimMode::Insert {
            if let Some(typed) = state.recording.as_mut() {
                match &event {
                    Event::Text(text) | Event::Paste(text) => typed.push_str(text),
                    Event::Key {
                        key: Key::Enter,
                        pressed: true,
                        ..
                    } => typed.push('\n'),
                    _ => {}
                }
            }
            keep.push(event);
            continue;
        }

        let keep_event = match &event {
            Event::Text(s) => {
                if let Some(ch) = s.chars().next() {
                    let b = ensure(&mut buf, text, cursor);
                    feed(&mut state, b, ch);
                    cursor = b.cursor;
                }
                false
            }
            // Cmd+V pastes the system clipboard: Normal pastes after the
            // caret, Visual replaces the selection (like Vim). Insert mode
            // is handled by egui above.
            Event::Paste(clip) => {
                let b = ensure(&mut buf, text, cursor);
                let visual = matches!(state.mode, VimMode::Visual | VimMode::VisualLine);
                if visual {
                    visual_operate(&mut state, b, 'd');
                }
                state.register = clip.clone();
                state.register_linewise = clip.ends_with('\n');
                // In Visual mode the register replaces the selection.
                paste(&mut state, b, visual, 1);
                cursor = b.cursor;
                false
            }
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                // Leave OS-level shortcuts (Cmd-C/V/Q, …) to egui and the app.
                if modifiers.command {
                    true
                } else {
                    let b = ensure(&mut buf, text, cursor);
                    let handled = special_key(&mut state, b, *key, *modifiers);
                    cursor = b.cursor;
                    !handled
                }
            }
            _ => true,
        };
        if keep_event {
            keep.push(event);
        }
    }
    ctx.input_mut(|i| i.events = keep);

    if let Some(b) = buf {
        if b.changed {
            *text = b.chars.iter().collect();
            edit_state.clear_undoer();
            changed = true;
        }
        cursor = b.cursor;
    }

    let chars: Vec<char> = text.chars().collect();
    let clamp = |i: usize| i.min(chars.len());
    // Never fight a mouse drag: the user is selecting text, not editing.
    let drag_selecting = ctx.input(|i| i.pointer.any_down());
    let anchor = state.anchor;
    let selection = match state.mode {
        VimMode::Visual | VimMode::VisualLine => {
            let (start, end) = if state.mode == VimMode::VisualLine {
                (
                    line_start(&chars, anchor.min(cursor)),
                    next_line_start(&chars, anchor.max(cursor)),
                )
            } else {
                let lo = anchor.min(cursor);
                let hi = anchor.max(cursor);
                (lo, (hi + 1).min(chars.len()))
            };
            CCursorRange {
                primary: CCursor::new(clamp(end)),
                secondary: CCursor::new(clamp(start)),
                h_pos: None,
            }
        }
        _ => CCursorRange::one(CCursor::new(clamp(cursor))),
    };
    if !drag_selecting {
        edit_state.cursor.set_char_range(Some(selection));
        edit_state.store(ctx, id);
    }
    ctx.data_mut(|d| d.insert_temp(id.with("vim"), state));
    changed
}

/// Paint the block caret for Normal/Visual modes. Call after the widget.
pub fn paint(ui: &egui::Ui, id: Id, output: &TextEditOutput) {
    if !enabled(ui.ctx()) {
        return;
    }
    let Some(state) = ctx_state(ui.ctx(), id) else {
        return;
    };
    if state.mode == VimMode::Insert {
        return;
    }
    let Some(range) = output.cursor_range else {
        return;
    };
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    let rect = egui::text_selection::text_cursor_state::cursor_rect(
        &output.galley,
        &range.primary,
        row_height,
    )
    .translate(output.galley_pos.to_vec2());
    let t = Tokens::get(ui.ctx());
    ui.painter()
        .rect_filled(rect, 1.0, t.accent.gamma_multiply(0.35));
    ui.painter().rect_stroke(
        rect,
        1.0,
        egui::Stroke::new(1.0, t.accent),
        egui::StrokeKind::Inside,
    );
}

fn ctx_state(ctx: &egui::Context, id: Id) -> Option<VimState> {
    ctx.data(|d| d.get_temp::<VimState>(id.with("vim")))
}

/// Height reserved for the mode strip below the editor.
pub const BAR_H: f32 = 16.0;

/// Mode strip: a clickable mode badge plus pending operator/count. Doubles as
/// the on/off switch (vim is on by default; click the badge to turn it off).
pub fn status_bar(ui: &mut egui::Ui, id: Id) {
    let t = Tokens::get(ui.ctx());
    let on = enabled(ui.ctx());
    let state = ctx_state(ui.ctx(), id);
    ui.allocate_ui(egui::vec2(ui.available_width(), BAR_H), |ui| {
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            let mode = state.as_ref().map(|s| s.mode).unwrap_or_default();
            let (label, color) = if on {
                (
                    mode.label(),
                    if mode == VimMode::Insert {
                        t.accent_text
                    } else {
                        t.text_muted
                    },
                )
            } else {
                ("vim", t.text_faint)
            };
            let badge = egui::Label::new(
                egui::RichText::new(label)
                    .text_style(crate::theme::small_strong())
                    .color(color),
            )
            .selectable(false)
            .sense(egui::Sense::click());
            let resp = ui
                .add(badge)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(if on {
                    "Vim keys are on — click to turn them off"
                } else {
                    "Vim keys are off — click to turn them on"
                });
            if resp.clicked() {
                set_enabled(ui.ctx(), !on);
            }
            if on {
                if let Some(state) = &state {
                    let mut pending = String::new();
                    if let Some(count) = state.count {
                        pending.push_str(&count.to_string());
                    }
                    if let Some(op) = state.pending_op {
                        pending.push(op);
                    }
                    if state.pending_g {
                        pending.push('g');
                    }
                    if state.pending_r {
                        pending.push('r');
                    }
                    if state.pending_find.is_some() {
                        pending.push('f');
                    }
                    if let Some(prefix) = state.pending_text_obj {
                        pending.push(prefix);
                    }
                    if !pending.is_empty() {
                        ui.label(
                            egui::RichText::new(pending)
                                .text_style(crate::theme::small_strong())
                                .color(t.accent_text),
                        );
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("esc normal · i insert")
                            .text_style(egui::TextStyle::Small)
                            .color(t.text_faint),
                    );
                });
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn run(initial: &str, cursor: usize, keys: &str) -> (VimState, Buf) {
        let mut state = VimState::default();
        let mut b = Buf::new(initial, cursor);
        for ch in keys.chars() {
            feed(&mut state, &mut b, ch);
        }
        (state, b)
    }

    fn text(b: &Buf) -> String {
        b.chars.iter().collect()
    }

    #[test]
    fn dd_removes_the_current_line() {
        let (_, b) = run("one\ntwo\nthree", 4, "dd");
        assert_eq!(text(&b), "one\nthree");
        assert_eq!(b.cursor, 4);
    }

    #[test]
    fn dw_stops_at_the_line_break() {
        let (_, b) = run("foo bar", 0, "dw");
        assert_eq!(text(&b), "bar");
        let (_, b) = run("foo\nbar", 0, "dw");
        assert_eq!(text(&b), "\nbar");
    }

    #[test]
    fn x_and_count() {
        let (_, b) = run("abcdef", 1, "x");
        assert_eq!(text(&b), "acdef");
        let (_, b) = run("abcdef", 0, "3x");
        assert_eq!(text(&b), "def");
    }

    #[test]
    fn yank_and_paste_linewise() {
        let (_, b) = run("a\nb", 0, "yyp");
        assert_eq!(text(&b), "a\na\nb");
        let (_, b) = run("a\nb", 2, "yyP");
        assert_eq!(text(&b), "a\nb\nb");
    }

    #[test]
    fn undo_restores_before_normal_edit() {
        let (state, b) = run("abc", 0, "xu");
        assert!(state.undo.is_empty());
        assert_eq!(text(&b), "abc");
    }

    #[test]
    fn counts_move_lines_and_clamp() {
        let (_, b) = run("a\nb\nc", 0, "3j");
        assert_eq!(b.cursor, 4);
        let (_, b) = run("a\nb\nc", 0, "9j");
        assert_eq!(b.cursor, 4);
        let (_, b) = run("a\nbb\nc", 0, "j");
        assert_eq!(b.cursor, 2);
    }

    #[test]
    fn dollar_lands_on_the_last_character() {
        let (_, b) = run("abc\ndef", 0, "$");
        assert_eq!(b.cursor, 2);
        let (_, b) = run("abc\ndef", 0, "G$");
        assert_eq!(b.cursor, 6);
    }

    #[test]
    fn gg_and_g_go_to_line_starts() {
        let (_, b) = run("a\nb\nc", 4, "gg");
        assert_eq!(b.cursor, 0);
        let (_, b) = run("a\nb\nc", 0, "G");
        assert_eq!(b.cursor, 4);
        let (_, b) = run("a\nb\nc", 0, "2gg");
        assert_eq!(b.cursor, 2);
    }

    #[test]
    fn open_line_enters_insert_with_indent() {
        let (state, b) = run("  foo", 2, "o");
        assert_eq!(state.mode, VimMode::Insert);
        assert_eq!(text(&b), "  foo\n  ");
    }

    #[test]
    fn visual_delete_moves_the_selection_into_the_register() {
        let (state, b) = run("abcdef", 1, "vlld");
        assert_eq!(text(&b), "aef");
        assert_eq!(state.register, "bcd");
        assert_eq!(state.mode, VimMode::Normal);
    }

    #[test]
    fn visual_line_delete_removes_whole_lines() {
        let (_, b) = run("a\nb\nc", 2, "Vd");
        assert_eq!(text(&b), "a\nc");
    }

    #[test]
    fn join_merges_lines_with_a_single_space() {
        let (_, b) = run("a\n   b", 0, "J");
        assert_eq!(text(&b), "a b");
    }

    #[test]
    fn cw_changes_word_and_enters_insert() {
        let (state, b) = run("foo bar", 0, "cw");
        assert_eq!(state.mode, VimMode::Insert);
        assert_eq!(text(&b), " bar");
    }

    #[test]
    fn escape_leaves_visual_and_insert() {
        let mut state = VimState::default();
        let mut b = Buf::new("abc", 1);
        feed(&mut state, &mut b, 'v');
        assert_eq!(state.mode, VimMode::Visual);
        escape(&mut state, &mut b);
        assert_eq!(state.mode, VimMode::Normal);

        state.mode = VimMode::Insert;
        state.inserted_at = Some(b.snapshot());
        b.chars.push('x');
        escape(&mut state, &mut b);
        assert_eq!(state.mode, VimMode::Normal);
        assert_eq!(state.undo.len(), 1, "insert session is one undo step");
    }

    #[test]
    fn replace_char_and_toggle_case() {
        let (_, b) = run("abc", 0, "rZ");
        assert_eq!(text(&b), "Zbc");
        let (_, b) = run("abc", 0, "~");
        assert_eq!(text(&b), "Abc");
    }

    #[test]
    fn word_motions_land_on_boundaries() {
        let (_, b) = run("foo bar baz", 0, "w");
        assert_eq!(b.cursor, 4);
        let (_, b) = run("foo bar baz", 8, "b");
        assert_eq!(b.cursor, 4);
        let (_, b) = run("foo bar", 0, "e");
        assert_eq!(b.cursor, 2);
    }

    #[test]
    fn redo_replays_an_undone_edit() {
        let mut state = VimState::default();
        let mut b = Buf::new("abc", 0);
        for ch in "xu".chars() {
            feed(&mut state, &mut b, ch);
        }
        redo(&mut state, &mut b);
        assert_eq!(text(&b), "bc");
    }

    fn key(key: Key) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    /// End-to-end: events are intercepted before the `TextEdit`, the buffer
    /// shrinks, and Insert mode still types normally.
    #[test]
    fn pre_edit_rewrites_input_for_a_live_text_edit() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        set_enabled(&ctx, true);
        let id = Id::new("vim-test-editor");
        let mut text = String::from("one\ntwo\nthree");

        // Frame 1: create the widget + its persisted state.
        let mut first = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = egui::TextEdit::multiline(&mut text).id(id).show(ui);
        });
        first.textures_delta.clear();
        ctx.memory_mut(|m| m.request_focus(id));

        // Frame 2: `dd` in Normal mode.
        let input = egui::RawInput {
            events: vec![
                key(Key::D),
                Event::Text("d".into()),
                key(Key::D),
                Event::Text("d".into()),
            ],
            ..Default::default()
        };
        let mut changed = false;
        let mut output = ctx.run_ui(input, |ui| {
            changed = pre_edit(ui.ctx(), id, &mut text);
            let _ = egui::TextEdit::multiline(&mut text).id(id).show(ui);
        });
        output.textures_delta.clear();
        assert!(changed);
        assert_eq!(text, "two\nthree");

        // Frame 3: `i` enters Insert; the following text is typed by egui.
        let input = egui::RawInput {
            events: vec![
                key(Key::I),
                Event::Text("i".into()),
                key(Key::X),
                Event::Text("x".into()),
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            pre_edit(ui.ctx(), id, &mut text);
            let _ = egui::TextEdit::multiline(&mut text).id(id).show(ui);
        });
        output.textures_delta.clear();
        assert_eq!(ctx_state(&ctx, id).unwrap().mode, VimMode::Insert);
        assert_eq!(text, "xtwo\nthree");
    }

    #[test]
    fn counted_linewise_operations_cover_n_lines() {
        let (_, b) = run("a\nb\nc\nd", 0, "3dd");
        assert_eq!(text(&b), "d");
        let (_, b) = run("a\nb\nc", 0, "3cc");
        assert_eq!(text(&b), "");
        let (state, b) = run("a\nb\nc\nd", 0, "2yy");
        assert_eq!(text(&b), "a\nb\nc\nd");
        assert_eq!(state.register, "a\nb\n");
    }

    #[test]
    fn counted_delete_to_end_and_yank() {
        let (_, b) = run("abc\ndef\nghi", 1, "2D");
        assert_eq!(text(&b), "a\nghi");
        let (state, b) = run("abc\ndef\nghi", 1, "2C");
        assert_eq!(text(&b), "a\nghi");
        assert_eq!(state.mode, VimMode::Insert);
        let (state, _) = run("a\nb\nc\nd", 0, "2Y");
        assert_eq!(state.register, "a\nb\n");
    }

    #[test]
    fn j_and_k_keep_the_goal_column() {
        let (_, b) = run("abcdef\nxy\nabcdef", 2, "jj");
        assert_eq!(b.cursor, 12);
        let (_, b) = run("abcdef\nxy\nabcdef", 2, "jjk");
        assert_eq!(b.cursor, 8);
        let (_, b) = run("abcdef\nxy\nabcdef", 2, "jjkk");
        assert_eq!(b.cursor, 2, "goal column returns after a short line");
    }

    #[test]
    fn find_char_motions_and_repeat() {
        let (_, b) = run("abcxdef", 0, "fx");
        assert_eq!(b.cursor, 3);
        let (_, b) = run("abcxdef", 6, "Fx");
        assert_eq!(b.cursor, 3);
        let (_, b) = run("a x b x c", 0, "2fx");
        assert_eq!(b.cursor, 6);
        let (_, b) = run("a,b,c,d", 0, "f,;");
        assert_eq!(b.cursor, 3);
        let (_, b) = run("a,b,c,d", 0, "f,;,");
        assert_eq!(b.cursor, 1);
        let (_, b) = run("abc def", 0, "fz");
        assert_eq!(b.cursor, 0, "a missing target is a no-op");
    }

    #[test]
    fn find_char_motions_work_under_operators() {
        let (_, b) = run("abcxdef", 0, "dfx");
        assert_eq!(text(&b), "def");
        let (_, b) = run("abcxdef", 0, "dtx");
        assert_eq!(text(&b), "xdef");
        let (_, b) = run("abcxdef", 0, "ctx");
        assert_eq!(text(&b), "xdef");
    }

    #[test]
    fn dot_repeats_normal_commands() {
        let (_, b) = run("abcdef", 0, "x.");
        assert_eq!(text(&b), "cdef");
        let (_, b) = run("foo bar baz", 0, "dw.");
        assert_eq!(text(&b), "baz");
        let (_, b) = run("a\nb\nc\nd\ne\nf", 0, "3dd.");
        assert_eq!(text(&b), "");
    }

    #[test]
    fn dot_repeats_an_insert_session_in_one_undo_step() {
        let mut state = VimState::default();
        let mut b = Buf::new("abc", 0);
        feed(&mut state, &mut b, 'i');
        if let Some(rec) = state.recording.as_mut() {
            rec.push_str("XY");
        }
        b.splice(b.cursor, b.cursor, "XY");
        b.cursor = 2;
        escape(&mut state, &mut b);
        assert_eq!(text(&b), "XYabc");
        feed(&mut state, &mut b, '.');
        assert_eq!(text(&b), "XXYYabc");
        feed(&mut state, &mut b, 'u');
        assert_eq!(text(&b), "XYabc", "replay undoes as one step");
    }

    #[test]
    fn word_text_objects() {
        let (_, b) = run("foo bar baz", 4, "diw");
        assert_eq!(text(&b), "foo  baz");
        let (state, b) = run("foo bar", 4, "ciw");
        assert_eq!(text(&b), "foo ");
        assert_eq!(state.mode, VimMode::Insert);
        let (_, b) = run("foo bar baz", 4, "daw");
        assert_eq!(text(&b), "foo baz");
    }

    #[test]
    fn quote_text_objects() {
        let quote = "say \"hi there\"";
        let (_, b) = run(quote, 6, "di\"");
        assert_eq!(text(&b), "say \"\"");
        let (_, b) = run(quote, 6, "da\"");
        assert_eq!(text(&b), "say ");
    }

    #[test]
    fn bracket_text_objects_cross_lines() {
        let (state, b) = run("f(a,\nb)", 2, "ci(");
        assert_eq!(text(&b), "f()");
        assert_eq!(state.mode, VimMode::Insert);
    }

    #[test]
    fn paragraph_text_objects() {
        let (_, b) = run("one\ntwo\n\nthree", 1, "dip");
        assert_eq!(text(&b), "\nthree");
        let (_, b) = run("one\ntwo\n\nthree", 1, "dap");
        assert_eq!(text(&b), "three");
    }

    #[test]
    fn visual_text_object_extends_the_selection() {
        let (state, b) = run("foo bar", 0, "viw");
        assert_eq!(state.mode, VimMode::Visual);
        assert_eq!(state.anchor, 0);
        assert_eq!(b.cursor, 2);
        assert_eq!(text(&b), "foo bar");
    }

    #[test]
    fn pre_edit_pastes_the_clipboard_in_normal_and_visual() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        set_enabled(&ctx, true);
        let id = Id::new("vim-paste-editor");
        let mut text = String::from("abc");

        let mut first = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = egui::TextEdit::multiline(&mut text).id(id).show(ui);
        });
        first.textures_delta.clear();
        ctx.memory_mut(|m| m.request_focus(id));

        // Normal mode: paste after the caret.
        let input = egui::RawInput {
            events: vec![Event::Paste("Z".into())],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            pre_edit(ui.ctx(), id, &mut text);
            let _ = egui::TextEdit::multiline(&mut text).id(id).show(ui);
        });
        output.textures_delta.clear();
        assert_eq!(text, "aZbc");

        // Visual mode: paste replaces the selection.
        let input = egui::RawInput {
            events: vec![
                Event::Text("v".into()),
                Event::Text("l".into()),
                Event::Paste("Q".into()),
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            pre_edit(ui.ctx(), id, &mut text);
            let _ = egui::TextEdit::multiline(&mut text).id(id).show(ui);
        });
        output.textures_delta.clear();
        assert_eq!(text, "aQc");
    }

    /// The Visual anchor must survive across frames (it lives in `VimState`,
    /// not in the per-frame scratch of `pre_edit`).
    #[test]
    fn visual_anchor_persists_across_frames() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply_visuals(&ctx, false);
        set_enabled(&ctx, true);
        let id = Id::new("vim-visual-anchor-editor");
        let mut text = String::from("abc");

        let mut first = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = egui::TextEdit::multiline(&mut text).id(id).show(ui);
        });
        first.textures_delta.clear();
        ctx.memory_mut(|m| m.request_focus(id));

        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![Event::Text("l".into()), Event::Text("v".into())],
                ..Default::default()
            },
            |ui| {
                pre_edit(ui.ctx(), id, &mut text);
                let _ = egui::TextEdit::multiline(&mut text).id(id).show(ui);
            },
        );
        output.textures_delta.clear();
        assert_eq!(ctx_state(&ctx, id).unwrap().anchor, 1);

        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![Event::Text("l".into()), Event::Text("d".into())],
                ..Default::default()
            },
            |ui| {
                pre_edit(ui.ctx(), id, &mut text);
                let _ = egui::TextEdit::multiline(&mut text).id(id).show(ui);
            },
        );
        output.textures_delta.clear();
        assert_eq!(text, "a", "selection ran from anchor 1 to cursor 2");
    }
}
