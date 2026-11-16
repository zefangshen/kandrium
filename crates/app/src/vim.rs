//! Vim-style modal editing for the card description field.
//!
//! Ported from the egui app's `crates/app/src/widgets/vim.rs`. The command
//! set, the text model (`Vec<char>` + a character-index cursor) and the
//! state machine are the same; only the glue differs.
//!
//! The egui version rewrites the pending input events before the widget sees
//! them. Here the modal layer sits *outside* the textarea instead: the card
//! modal's capture listener sees the key first, applies it to a plain copy of
//! the buffer, writes the result back through the input state and repositions
//! the caret. To keep the textarea from acting on the same key, its state is
//! read-only whenever the mode is not Insert, so its own editing actions are
//! never registered. Insert mode is plain textarea editing, so IME, selection
//! and clipboard keep working.
//!
//! Normal/Visual modes keep the caret on the input but draw it as a filled
//! block ([`block_cursor_overlay`]) over the engine's line caret; Insert mode
//! leaves the line caret alone, so the mode is visible without a badge. The
//! vim cursor is always one character, and Visual mode's inclusive selection
//! is written as a textarea selection (`anchor ..= cursor`) with the caret
//! direction pointing at the moving end.
//!
//! Supported: `h j k l w W b B e E 0 ^ $ gg G { }`, counts,
//! `f F t T ; ,`, `i I a A o O`, `v V`, `.` repeat,
//! `x X dd D yy Y cc C S p P J r ~ u Ctrl-r`, text objects
//! (`iw aw`, `i" a"`, `i( a(`, `i[ a[`, `i{ a{`, `ip ap`), `Esc`, and the
//! ex commands `:w` (save without closing) / `:q`, `:wq`, `:x` (save and
//! close; closing already saves, so `:q!` is reported as unknown).
//! Deliberately absent, unchanged from the egui editor: `/` search, marks,
//! named registers, `%`, `>>`/`<<`, `s`, and `.` repeat of Visual-mode
//! operations. Also absent here: the on/off preference itself — the shell
//! owns the global `vim_mode` setting and passes it in through `set_enabled`
//! — and `.` replay of IME-composed Insert text (only `key_char` presses are
//! recorded).

use std::ops::Range;

use gpui_kit::component::input::{EditorState, RopeExt as _, TextareaState};
use gpui_kit::{
    App, Bounds, Entity, Focusable, Hsla, IntoElement, KeyDownEvent, Keystroke, Pixels,
    SharedString, Styled, TextAlign, TextRun, Window, canvas, fill, font, px,
};

/// The buffer operations vim needs.
///
/// Notes, document tabs and project documents edit through the code-editor
/// state (for the gutter and syntax colours); the description fields still use
/// a textarea. Both are the same engine underneath, so the command layer stays
/// one implementation and only binds these six operations twice.
pub trait VimBuffer: 'static {
    fn vim_text(&self, cx: &App) -> String;
    fn vim_cursor(&self, cx: &App) -> usize;
    fn vim_set_readonly(&self, on: bool, cx: &mut App);
    /// Replace the differing middle of the buffer and move the caret.
    #[allow(clippy::too_many_arguments)]
    fn vim_write(
        &self,
        old: &str,
        new: &str,
        start: usize,
        old_end: usize,
        insert: &str,
        range: Range<usize>,
        reversed: bool,
        window: &mut Window,
        cx: &mut App,
    );
}

macro_rules! impl_vim_buffer {
    ($state:ty) => {
        impl VimBuffer for Entity<$state> {
            fn vim_text(&self, cx: &App) -> String {
                self.read(cx).text().to_string()
            }

            fn vim_cursor(&self, cx: &App) -> usize {
                self.read(cx).cursor()
            }

            fn vim_set_readonly(&self, on: bool, cx: &mut App) {
                self.update(cx, |state, cx| state.set_readonly(on, cx));
            }

            fn vim_write(
                &self,
                old: &str,
                new: &str,
                start: usize,
                old_end: usize,
                insert: &str,
                range: Range<usize>,
                reversed: bool,
                window: &mut Window,
                cx: &mut App,
            ) {
                self.update(cx, |state, cx| {
                    if old != new {
                        state.set_selected_range(start..old_end, cx);
                        state.replace(insert, window, cx);
                    }
                    state.set_selected_range_with_direction(range, reversed, cx);
                });
            }
        }
    };
}

impl_vim_buffer!(TextareaState);
impl_vim_buffer!(EditorState);

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

/// What a `:` command asks the view to do. It carries no paths or text:
/// closing is the view's existing "flush, then remove" path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExCommand {
    /// Flush the current buffer to disk now (`:w`).
    Write,
    /// Flush and close the current tab (`:q`, `:wq`, `:x`).
    Close,
}

/// Parse the text typed after `:`. Closing always flushes first, so `wq`,
/// `x` and `xit` are the same intent as `q`. The error text is what the strip
/// shows for an unknown command.
fn parse_ex(input: &str) -> Result<ExCommand, String> {
    match input.trim() {
        "w" | "write" => Ok(ExCommand::Write),
        "q" | "quit" | "wq" | "x" | "xit" => Ok(ExCommand::Close),
        other => Err(format!("Not a command: :{other}")),
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
    /// Moving end of the visual selection as a vim character index. `None`
    /// outside Visual/VisualLine: the textarea keeps the selection end as an
    /// exclusive boundary, so it cannot be read back from the input.
    visual_cursor: Option<usize>,
    /// Text typed after `:`. `Some` exactly while the command line is open;
    /// it can only become `Some` from Normal mode, so it never coexists with
    /// Insert/Visual.
    ex: Option<String>,
    /// Unknown-command message shown in the mode strip until the next `:`.
    ex_error: Option<String>,
    register: String,
    register_linewise: bool,
    /// Text before the current Insert session, so `u` undoes it in one step.
    inserted_at: Option<Snapshot>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl VimState {
    /// Whether a count, operator or multi-key command is waiting for its
    /// next input.
    fn has_pending(&self) -> bool {
        self.count.is_some()
            || self.pending_op.is_some()
            || self.pending_g
            || self.pending_r
            || self.pending_find.is_some()
            || self.pending_text_obj.is_some()
    }

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
    // `:` opens the command line; it is never part of a `.` change.
    if ch == ':' && state.mode == VimMode::Normal && state.ex.is_none() {
        cancel_command(state);
        state.ex = Some(String::new());
        state.ex_error = None;
        return;
    }
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

/// Leave the current mode for Normal mode, as `Esc` does.
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
    state.ex = None;
    state.reset_pending();
    state.goal_col = None;
    state.cmd_buf.clear();
    state.cmd_start = None;
    state.suppress_record = false;
    b.clamp_normal();
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

/// A key with a name rather than a character (gpui's `Keystroke::key`).
fn is_named_key(key: &str) -> bool {
    matches!(
        key,
        "enter"
            | "backspace"
            | "delete"
            | "tab"
            | "left"
            | "right"
            | "up"
            | "down"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
    )
}

/// Apply a key that is not plain text. Returns false when the textarea should
/// handle the key itself (arrows and Home/End/PageUp/PageDown move the caret).
fn special_key(state: &mut VimState, b: &mut Buf, key: &str, ctrl: bool) -> bool {
    if ctrl {
        return match key {
            "r" => {
                cancel_command(state);
                redo(state, b);
                true
            }
            "z" => {
                cancel_command(state);
                undo(state, b);
                true
            }
            "c" => {
                state.mode = VimMode::Normal;
                cancel_command(state);
                true
            }
            "d" => {
                state.goal_col = None;
                half_page(b, true);
                true
            }
            "u" => {
                state.goal_col = None;
                half_page(b, false);
                true
            }
            "f" => {
                state.goal_col = None;
                page(b, true);
                true
            }
            "b" => {
                state.goal_col = None;
                page(b, false);
                true
            }
            _ => false,
        };
    }
    match key {
        "left" | "right" | "up" | "down" | "home" | "end" | "pageup" | "pagedown" => false,
        "enter" => {
            state.goal_col = None;
            let line = line_index(&b.chars, b.cursor) + 1;
            goto_line(b, line);
            true
        }
        "backspace" | "delete" | "tab" => true,
        _ => true,
    }
}

/// Apply one keystroke to the open command line. `Some(cmd)` means Enter
/// closed it successfully. OS shortcuts (Cmd-…) stay with the app.
fn feed_ex_line(
    ex: &mut Option<String>,
    error: &mut Option<String>,
    keystroke: &Keystroke,
) -> (bool, Option<ExCommand>) {
    if keystroke.modifiers.platform {
        return (false, None);
    }
    let Some(line) = ex.as_mut() else {
        return (true, None);
    };
    match keystroke.key.as_str() {
        "backspace" => {
            line.pop();
        }
        "enter" => {
            let parsed = parse_ex(line);
            *ex = None;
            return match parsed {
                Ok(cmd) => {
                    *error = None;
                    (true, Some(cmd))
                }
                Err(msg) => {
                    *error = Some(msg);
                    (true, None)
                }
            };
        }
        _ => {
            if let Some(text) = keystroke.key_char.as_deref() {
                line.push_str(text);
            }
        }
    }
    (true, None)
}

// ---------------------------------------------------------------------------
// gpui glue
// ---------------------------------------------------------------------------

/// What the caller has to do with the key after vim saw it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyOutcome {
    /// Vim consumed the key; it must not reach the textarea.
    pub handled: bool,
    /// The command line completed with an ex command.
    pub ex: Option<ExCommand>,
}

impl KeyOutcome {
    fn handled() -> Self {
        Self {
            handled: true,
            ex: None,
        }
    }
}

/// One description field's vim state and the glue to a gpui textarea.
pub struct VimSession {
    state: VimState,
    /// Caret (character index) the textarea reported after our last write.
    /// When it changes on its own the input moved (click, arrow), so the next
    /// key adopts its position instead of the stored vim cursor.
    expected_caret: Option<usize>,
    /// The user's vim preference. When off the session is a plain textarea:
    /// no keys are consumed and the buffer is never read-only.
    enabled: bool,
}

impl Default for VimSession {
    fn default() -> Self {
        Self {
            state: VimState::default(),
            expected_caret: None,
            // egui's `SavedState.vim_mode` defaults to on.
            enabled: true,
        }
    }
}

impl VimSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mode(&self) -> VimMode {
        self.state.mode
    }

    /// Whether vim is on for this editor.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Whether the textarea should accept plain typing.
    pub fn is_insert(&self) -> bool {
        !self.enabled || self.state.mode == VimMode::Insert
    }

    /// Whether the caret should read as a vim block. Normal and Visual modes
    /// use the block; Insert keeps the input's line caret. A disabled session
    /// keeps the line caret so it behaves like a plain editor.
    pub fn block_cursor(&self) -> bool {
        self.enabled && self.state.mode != VimMode::Insert
    }

    /// Whether the textarea must be read-only. A disabled session is never
    /// read-only, so typing behaves like a plain editor.
    pub fn readonly(&self) -> bool {
        self.enabled && self.state.mode != VimMode::Insert
    }

    /// Follow the shell's vim preference. Re-enabling starts in Normal mode;
    /// disabling drops the transient command state and hands the buffer back
    /// to plain typing.
    pub fn set_enabled(&mut self, on: bool, input: &impl VimBuffer, cx: &mut App) {
        if self.enabled != on {
            self.enabled = on;
            self.state = VimState::default();
            self.expected_caret = None;
        }
        self.sync_readonly(input, cx);
    }

    /// Label shown in the mode strip. A disabled session shows `vim` so the
    /// badge still reads as the toggle.
    pub fn mode_label(&self) -> &'static str {
        if self.enabled {
            self.state.mode.label()
        } else {
            "vim"
        }
    }

    /// Keys of the command in progress (`d`, `f`, ...), for the strip.
    pub fn pending(&self) -> String {
        if self.enabled {
            self.state.cmd_buf.iter().collect()
        } else {
            String::new()
        }
    }

    /// Text typed after `:` while the command line is open.
    pub fn ex_line(&self) -> Option<&str> {
        if self.enabled {
            self.state.ex.as_deref()
        } else {
            None
        }
    }

    /// Message for an unknown `:` command, shown until the next `:`.
    pub fn ex_error(&self) -> Option<&str> {
        if self.enabled {
            self.state.ex_error.as_deref()
        } else {
            None
        }
    }

    /// Keep the textarea editable only in Insert mode. Read-only keeps focus,
    /// the caret and mouse selection, so Normal/Visual can use the same input
    /// for display and for our own caret writes.
    pub fn sync_readonly(&self, input: &impl VimBuffer, cx: &mut App) {
        let readonly = self.readonly();
        input.vim_set_readonly(readonly, cx);
    }

    /// Apply one key press to the buffer and write the result back.
    ///
    /// `handled: false` on `Esc` in Normal mode means the key did nothing:
    /// Normal already is the resting mode, and the caller leaves the open
    /// document alone.
    pub fn handle_key(
        &mut self,
        event: &KeyDownEvent,
        input: &impl VimBuffer,
        window: &mut Window,
        cx: &mut App,
    ) -> KeyOutcome {
        if !self.enabled {
            return KeyOutcome::default();
        }
        let outcome = self.apply_key(event, input, window, cx);
        self.sync_readonly(input, cx);
        outcome
    }

    fn apply_key(
        &mut self,
        event: &KeyDownEvent,
        input: &impl VimBuffer,
        window: &mut Window,
        cx: &mut App,
    ) -> KeyOutcome {
        let keystroke = &event.keystroke;
        // OS shortcuts stay with the app.
        if keystroke.modifiers.platform {
            return KeyOutcome::default();
        }
        let text = input.vim_text(cx);
        let read_caret = char_index(&text, input.vim_cursor(cx));
        // Visual selections report their exclusive end, so trust the stored
        // vim cursor as long as the input still shows the caret we wrote.
        let caret = if self.state.mode == VimMode::Insert {
            read_caret
        } else if self.expected_caret == Some(read_caret) {
            self.state.visual_cursor.unwrap_or(read_caret)
        } else {
            self.state.visual_cursor = None;
            read_caret
        };

        if is_escape(keystroke) {
            if self.state.ex.is_some() {
                self.state.ex = None;
                self.state.ex_error = None;
                return KeyOutcome::handled();
            }
            if self.state.mode == VimMode::Normal {
                // Nothing to leave. Esc cancels a pending command; with
                // nothing pending it is a no-op, and it never closes the
                // document.
                if !self.state.has_pending() {
                    return KeyOutcome::default();
                }
                self.state.reset_pending();
                return KeyOutcome::handled();
            }
            let mut b = Buf::new(&text, caret);
            escape(&mut self.state, &mut b);
            self.write(&b, &text, input, window, cx);
            return KeyOutcome::handled();
        }

        // The open command line swallows every key except Esc.
        if self.state.ex.is_some() {
            let (consumed, ex) =
                feed_ex_line(&mut self.state.ex, &mut self.state.ex_error, keystroke);
            return KeyOutcome {
                handled: consumed,
                ex,
            };
        }

        if self.state.mode == VimMode::Insert {
            // Plain textarea editing; only track the text for `.` repeat.
            if let Some(typed) = self.state.recording.as_mut() {
                if keystroke.key == "enter" {
                    typed.push('\n');
                } else if let Some(text) = keystroke.key_char.as_deref() {
                    typed.push_str(text);
                }
            }
            return KeyOutcome::default();
        }

        let mut b = Buf::new(&text, caret);
        let handled = if keystroke.modifiers.control {
            special_key(&mut self.state, &mut b, &keystroke.key, true)
        } else if is_named_key(&keystroke.key) {
            special_key(&mut self.state, &mut b, &keystroke.key, false)
        } else if let Some(text) = keystroke.key_char.as_deref() {
            for ch in text.chars() {
                feed(&mut self.state, &mut b, ch);
            }
            true
        } else {
            // Function keys and the like: swallow them in Normal mode.
            true
        };
        if handled || b.changed {
            self.write(&b, &text, input, window, cx);
        }
        KeyOutcome { handled, ex: None }
    }

    /// Write the buffer and the vim caret back into the textarea. Only the
    /// differing middle of the text is replaced, so the input keeps its
    /// scroll position and its own undo history stays meaningful.
    fn write(
        &mut self,
        b: &Buf,
        old: &str,
        input: &impl VimBuffer,
        window: &mut Window,
        cx: &mut App,
    ) {
        let new: String = b.chars.iter().collect();
        let (start, old_end, insert) = diff(old, &new);
        let (range, reversed) = self.selection(b);
        input.vim_write(
            old, &new, start, old_end, &insert, range, reversed, window, cx,
        );
        // Remember the caret the input will report next, and keep the moving
        // end of a visual selection since the input stores it exclusively.
        let read_back = input.vim_cursor(cx);
        self.expected_caret = Some(char_index(&new, read_back));
        self.state.visual_cursor =
            matches!(self.state.mode, VimMode::Visual | VimMode::VisualLine).then_some(b.cursor);
    }

    /// The vim selection as a byte range plus the caret direction.
    fn selection(&self, b: &Buf) -> (std::ops::Range<usize>, bool) {
        let clamp = |i: usize| i.min(b.chars.len());
        match self.state.mode {
            VimMode::Visual | VimMode::VisualLine => {
                let anchor = clamp(self.state.anchor);
                let cursor = clamp(b.cursor);
                let (start, end) = if self.state.mode == VimMode::VisualLine {
                    (
                        line_start(&b.chars, anchor.min(cursor)),
                        next_line_start(&b.chars, anchor.max(cursor)),
                    )
                } else {
                    let lo = anchor.min(cursor);
                    let hi = anchor.max(cursor);
                    (lo, (hi + 1).min(b.chars.len()))
                };
                (
                    byte_index(&b.chars, start)..byte_index(&b.chars, end),
                    cursor < anchor,
                )
            }
            // Normal mode keeps the caret on a character, even when a mouse
            // click placed it past the end of the line.
            VimMode::Normal => {
                let i = clamp(clamp_normal(&b.chars, b.cursor));
                let byte = byte_index(&b.chars, i);
                (byte..byte, false)
            }
            VimMode::Insert => {
                let i = clamp(b.cursor);
                let byte = byte_index(&b.chars, i);
                (byte..byte, false)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Block caret
// ---------------------------------------------------------------------------

/// Tab stop the input renders, in cells: its default `tab_size`, which the app
/// never changes.
const VIM_TAB_SIZE: f32 = 4.0;

/// The caret geometry the block overlay needs from an editor state.
///
/// The code editor and the textarea are aliases of the same engine, but they
/// are distinct types, so this trait keeps the overlay one implementation.
pub trait CaretSource: 'static {
    /// The active caret in window coordinates, with the vertical scroll
    /// offset applied the way the engine applies it when painting its own
    /// line caret.
    fn caret_bounds(&self, cx: &App) -> Option<Bounds<Pixels>>;
    /// Height of the line box the caret sits in. The engine's caret is only
    /// 85 % of it, so the block uses this to fill the whole cell.
    fn caret_line_height(&self, cx: &App) -> Option<Pixels>;
    /// The character under the caret, if the caret is on one.
    fn caret_char(&self, cx: &App) -> Option<char>;
    /// Whether the editor owns keyboard focus right now.
    fn caret_focused(&self, window: &Window, cx: &App) -> bool;
}

macro_rules! impl_caret_source {
    ($state:ty) => {
        impl CaretSource for Entity<$state> {
            fn caret_bounds(&self, cx: &App) -> Option<Bounds<Pixels>> {
                let geometry = self.read(cx).prepaint_caret_geometry()?;
                let mut bounds = geometry.cursor_bounds();
                // The engine stashes the caret before scroll is applied and
                // adds it back when painting; mirror that here.
                bounds.origin.y += geometry.scroll_offset().y;
                Some(bounds)
            }

            fn caret_line_height(&self, cx: &App) -> Option<Pixels> {
                Some(self.read(cx).prepaint_caret_geometry()?.line_height())
            }

            fn caret_char(&self, cx: &App) -> Option<char> {
                let state = self.read(cx);
                state.text().char_at(state.cursor())
            }

            fn caret_focused(&self, window: &Window, cx: &App) -> bool {
                self.read(cx).focus_handle(cx).is_focused(window)
            }
        }
    };
}

impl_caret_source!(TextareaState);
impl_caret_source!(EditorState);

/// Paint the vim block caret over an editor.
///
/// The input keeps painting its own 1 px line caret, but the block starts at
/// the same origin and is filled with `block_color`, so Normal/Visual modes
/// read as a block. The block spans the shaped advance of the character under
/// the caret — one nominal `ch_width` is too narrow for wide glyphs in a
/// proportional font — and the full line height, since the engine's caret is
/// only 85 % of it. The character is then redrawn in `text_color` on top so the
/// glyph reads inverted instead of disappearing under the fill. Insert mode
/// does not call this, which leaves the engine's line caret untouched.
///
/// `family` and `font_size` must match the editor's text, and `text_color` must
/// be the surface behind the block (`Tokens::field`).
///
/// The caller places the returned canvas over the editor (for example
/// `.absolute().inset_0()` inside the editor's relative wrapper); it paints
/// in window coordinates and never takes pointer input.
pub fn block_cursor_overlay(
    source: impl CaretSource,
    family: impl Into<SharedString>,
    font_size: Pixels,
    block_color: Hsla,
    text_color: Hsla,
) -> impl IntoElement + Styled {
    let family = family.into();
    canvas(
        move |_bounds, _window, _cx| (),
        move |_bounds, (), window, cx| {
            if !source.caret_focused(window, cx) {
                return;
            }
            let Some(caret) = source.caret_bounds(cx) else {
                return;
            };
            let ch = source.caret_char(cx);
            let style_font = font(family.clone());
            let font_id = window.text_system().resolve_font(&style_font);
            let ch_width = window
                .text_system()
                .ch_width(font_id, font_size)
                .unwrap_or(font_size * 0.6);
            let shaped = ch.filter(|ch| !ch.is_control()).map(|ch| {
                let text: SharedString = ch.to_string().into();
                let run = TextRun {
                    len: text.len(),
                    font: style_font.clone(),
                    color: text_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = window
                    .text_system()
                    .shape_line(text, font_size, &[run], None);
                (ch, line)
            });
            let advance = shaped
                .as_ref()
                .map(|(_, line)| line.width)
                .unwrap_or(ch_width);
            let width = block_width(ch, ch_width, advance);
            // The engine's caret is vertically centred inside the line box, so
            // grow it back to the full line height around that centre.
            let line_height = source
                .caret_line_height(cx)
                .unwrap_or(caret.size.height)
                .max(caret.size.height);
            let origin = gpui_kit::point(
                caret.origin.x,
                caret.origin.y - (line_height - caret.size.height) / 2.0,
            );
            window.paint_quad(fill(
                Bounds::new(origin, gpui_kit::size(width, line_height)),
                block_color,
            ));
            if let Some((ch, line)) = &shaped
                && !ch.is_whitespace()
            {
                let _ = line.paint(origin, line_height, TextAlign::Left, None, window, cx);
            }
        },
    )
}

/// Width of the block cell at one caret position.
///
/// `shaped` is the shaped advance of the character under the caret. Tabs take
/// the input's tab stop (it renders them as four spaces and the app never
/// changes `tab_size`), and positions with no visible character (end of line,
/// newline) fall back to the nominal cell width.
fn block_width(ch: Option<char>, ch_width: Pixels, shaped: Pixels) -> Pixels {
    match ch {
        Some('\t') => ch_width * VIM_TAB_SIZE,
        Some(ch) if !ch.is_control() => {
            if shaped > px(0.) {
                shaped
            } else {
                ch_width
            }
        }
        _ => ch_width,
    }
}

fn is_escape(keystroke: &Keystroke) -> bool {
    keystroke.key == "escape" || (keystroke.modifiers.control && keystroke.key == "[")
}

fn char_index(text: &str, byte: usize) -> usize {
    let mut end = byte.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].chars().count()
}

fn byte_index(chars: &[char], index: usize) -> usize {
    chars
        .iter()
        .take(index.min(chars.len()))
        .map(|ch| ch.len_utf8())
        .sum()
}

/// The smallest single replacement that turns `old` into `new`:
/// `(start_byte, old_end_byte, inserted_text)`.
fn diff(old: &str, new: &str) -> (usize, usize, String) {
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    let mut prefix = 0;
    while prefix < a.len() && prefix < b.len() && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < a.len() - prefix
        && suffix < b.len() - prefix
        && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix]
    {
        suffix += 1;
    }
    (
        byte_index(&a, prefix),
        byte_index(&a, a.len() - suffix),
        b[prefix..b.len() - suffix].iter().collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run keys like the editor does: text goes through `feed` in
    /// Normal/Visual, and is spliced at the caret in Insert mode.
    fn run(text: &str, cursor: usize, keys: &str) -> (String, usize) {
        let mut state = VimState::default();
        let mut b = Buf::new(text, cursor);
        for ch in keys.chars() {
            if ch == '\u{1b}' {
                escape(&mut state, &mut b);
            } else if state.mode == VimMode::Insert {
                b.splice(b.cursor, b.cursor, &ch.to_string());
                b.cursor += 1;
            } else {
                feed(&mut state, &mut b, ch);
            }
        }
        (b.chars.iter().collect(), b.cursor)
    }

    #[test]
    fn motions_and_counts_move_the_cursor() {
        assert_eq!(run("one two\nthree", 0, "w"), ("one two\nthree".into(), 4));
        assert_eq!(run("one two\nthree", 0, "2w"), ("one two\nthree".into(), 8));
        assert_eq!(run("one two\nthree", 8, "gg"), ("one two\nthree".into(), 0));
        assert_eq!(run("one two\nthree", 0, "$"), ("one two\nthree".into(), 6));
        assert_eq!(run("one two\nthree", 0, "G"), ("one two\nthree".into(), 8));
    }

    #[test]
    fn delete_and_change_families() {
        assert_eq!(run("abc", 0, "x"), ("bc".into(), 0));
        assert_eq!(run("abc", 0, "dd"), ("".into(), 0));
        assert_eq!(run("one two\nthree", 0, "dw"), ("two\nthree".into(), 0));
        assert_eq!(run("abc", 0, "cwX"), ("X".into(), 1));
        assert_eq!(run("abc", 0, "yy"), ("abc".into(), 0));
    }

    #[test]
    fn find_motions_and_repeat() {
        assert_eq!(run("a,b,c", 0, "f,"), ("a,b,c".into(), 1));
        assert_eq!(run("a,b,c", 0, "f,;"), ("a,b,c".into(), 3));
        assert_eq!(run("a,b,c", 3, "F,"), ("a,b,c".into(), 1));
    }

    #[test]
    fn text_objects_work_with_change() {
        assert_eq!(run("one two three", 0, "ciwX"), ("X two three".into(), 1));
        let (text, _) = run("one two three", 4, "diw");
        assert_eq!(text, "one  three");
    }

    #[test]
    fn undo_restores_the_previous_text() {
        let mut state = VimState::default();
        let mut b = Buf::new("abc", 0);
        for ch in "xx".chars() {
            feed(&mut state, &mut b, ch);
        }
        assert_eq!(b.chars.iter().collect::<String>(), "c");
        feed(&mut state, &mut b, 'u');
        assert_eq!(b.chars.iter().collect::<String>(), "bc");
    }

    #[test]
    fn visual_delete() {
        assert_eq!(run("abc", 0, "vlld").0, "");
    }

    #[test]
    fn insert_then_escape_lands_on_the_last_character() {
        let (text, cursor) = run("", 0, "ihey\u{1b}");
        assert_eq!(text, "hey");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn diff_finds_the_changed_middle() {
        assert_eq!(diff("abc", "abd"), (2, 3, "d".into()));
        assert_eq!(diff("abc", "axc"), (1, 2, "x".into()));
        assert_eq!(diff("", "xy"), (0, 0, "xy".into()));
        assert_eq!(diff("abc", ""), (0, 3, String::new()));
    }

    /// Like [`run`], but keeps the modal state so a test can inspect the
    /// register and the mode.
    fn run_state(text: &str, cursor: usize, keys: &str) -> (VimState, Buf) {
        let mut state = VimState::default();
        let mut b = Buf::new(text, cursor);
        for ch in keys.chars() {
            if ch == '\u{1b}' {
                escape(&mut state, &mut b);
            } else if state.mode == VimMode::Insert {
                b.splice(b.cursor, b.cursor, &ch.to_string());
                b.cursor += 1;
            } else {
                feed(&mut state, &mut b, ch);
            }
        }
        (state, b)
    }

    fn body(b: &Buf) -> String {
        b.chars.iter().collect()
    }

    #[test]
    fn ex_parser_accepts_the_documented_commands() {
        assert_eq!(parse_ex("w"), Ok(ExCommand::Write));
        assert_eq!(parse_ex(" write "), Ok(ExCommand::Write));
        assert_eq!(parse_ex("q"), Ok(ExCommand::Close));
        assert_eq!(parse_ex("quit"), Ok(ExCommand::Close));
        assert_eq!(parse_ex("wq"), Ok(ExCommand::Close));
        assert_eq!(parse_ex("x"), Ok(ExCommand::Close));
        assert_eq!(parse_ex("xit"), Ok(ExCommand::Close));
        assert_eq!(parse_ex("q!"), Err("Not a command: :q!".to_string()));
    }

    #[test]
    fn counted_linewise_operations_cover_n_lines() {
        let (_, b) = run_state("a\nb\nc\nd", 0, "3dd");
        assert_eq!(body(&b), "d");
        let (state, b) = run_state("a\nb\nc", 0, "3cc");
        assert_eq!(body(&b), "");
        assert_eq!(state.mode, VimMode::Insert);
        let (state, b) = run_state("a\nb\nc\nd", 0, "2yy");
        assert_eq!(body(&b), "a\nb\nc\nd");
        assert_eq!(state.register, "a\nb\n");
    }

    #[test]
    fn counted_delete_to_end_and_yank() {
        let (_, b) = run_state("abc\ndef\nghi", 1, "2D");
        assert_eq!(body(&b), "a\nghi");
        let (state, b) = run_state("abc\ndef\nghi", 1, "2C");
        assert_eq!(body(&b), "a\nghi");
        assert_eq!(state.mode, VimMode::Insert);
        let (state, _) = run_state("a\nb\nc\nd", 0, "2Y");
        assert_eq!(state.register, "a\nb\n");
    }

    #[test]
    fn j_and_k_keep_the_goal_column() {
        let (_, b) = run_state("abcdef\nxy\nabcdef", 2, "jj");
        assert_eq!(b.cursor, 12);
        let (_, b) = run_state("abcdef\nxy\nabcdef", 2, "jjk");
        assert_eq!(b.cursor, 8);
        let (_, b) = run_state("abcdef\nxy\nabcdef", 2, "jjkk");
        assert_eq!(b.cursor, 2, "the goal column returns after a short line");
    }

    #[test]
    fn find_char_motions_and_repeat() {
        assert_eq!(run("abcxdef", 0, "fx").1, 3);
        assert_eq!(run("abcxdef", 6, "Fx").1, 3);
        assert_eq!(run("a x b x c", 0, "2fx").1, 6);
        assert_eq!(run("a,b,c,d", 0, "f,;").1, 3);
        assert_eq!(run("a,b,c,d", 0, "f,;,").1, 1);
        assert_eq!(run("abc def", 0, "fz").1, 0, "a missing target is a no-op");
    }

    #[test]
    fn find_char_motions_work_under_operators() {
        assert_eq!(run("abcxdef", 0, "dfx").0, "def");
        assert_eq!(run("abcxdef", 0, "dtx").0, "xdef");
        assert_eq!(run("abcxdef", 0, "ctx").0, "xdef");
    }

    #[test]
    fn dot_repeats_normal_commands() {
        assert_eq!(run("abcdef", 0, "x.").0, "cdef");
        assert_eq!(run("foo bar baz", 0, "dw.").0, "baz");
        assert_eq!(run("a\nb\nc\nd\ne\nf", 0, "3dd.").0, "");
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
        assert_eq!(body(&b), "XYabc");
        feed(&mut state, &mut b, '.');
        assert_eq!(body(&b), "XXYYabc");
        feed(&mut state, &mut b, 'u');
        assert_eq!(body(&b), "XYabc", "the replay undoes as one step");
    }

    #[test]
    fn word_text_objects() {
        assert_eq!(run("foo bar baz", 4, "diw").0, "foo  baz");
        let (state, b) = run_state("foo bar", 4, "ciw");
        assert_eq!(body(&b), "foo ");
        assert_eq!(state.mode, VimMode::Insert);
        assert_eq!(run("foo bar baz", 4, "daw").0, "foo baz");
    }

    #[test]
    fn quote_text_objects() {
        let quote = "say \"hi there\"";
        assert_eq!(run(quote, 6, "di\"").0, "say \"\"");
        assert_eq!(run(quote, 6, "da\"").0, "say ");
    }

    #[test]
    fn bracket_text_objects_cross_lines() {
        let (state, b) = run_state("f(a,\nb)", 2, "ci(");
        assert_eq!(body(&b), "f()");
        assert_eq!(state.mode, VimMode::Insert);
    }

    #[test]
    fn paragraph_text_objects() {
        assert_eq!(run("one\ntwo\n\nthree", 1, "dip").0, "\nthree");
        assert_eq!(run("one\ntwo\n\nthree", 1, "dap").0, "three");
    }

    #[test]
    fn block_width_uses_the_shaped_advance_and_the_tab_stop() {
        let ch_width = px(8.);
        assert_eq!(
            block_width(Some('W'), ch_width, px(13.)),
            px(13.),
            "a wide glyph keeps its shaped advance"
        );
        assert_eq!(
            block_width(Some('i'), ch_width, px(3.)),
            px(3.),
            "a narrow glyph keeps its shaped advance"
        );
        assert_eq!(
            block_width(Some('W'), ch_width, px(0.)),
            ch_width,
            "an empty shape falls back to the nominal cell"
        );
        assert_eq!(
            block_width(Some('\t'), ch_width, px(99.)),
            px(32.),
            "a tab spans four cells"
        );
        assert_eq!(
            block_width(Some('\n'), ch_width, px(99.)),
            ch_width,
            "a newline has no cell of its own"
        );
        assert_eq!(block_width(None, ch_width, px(99.)), ch_width);
    }
}
