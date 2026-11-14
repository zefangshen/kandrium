//! The debounced-save protocol shared by every editable surface.
//!
//! A draft records the value confirmed on disk (`saved`), the last value
//! observed from the editor (`seen`), whether the two differ (`dirty`) and a
//! generation counter. A real change bumps the generation; a timer captures it
//! and bows out through [`Draft::is_current`] once a newer edit has landed, so
//! a stale write can never overwrite fresh text.

use std::time::Duration;

/// Debounce for every text autosave (notes, documents, project docs, title,
/// description).
pub const AUTOSAVE: Duration = Duration::from_millis(400);

#[derive(Clone, Debug)]
pub struct Draft<T = String> {
    saved: T,
    seen: T,
    dirty: bool,
    generation: u64,
}

impl<T: PartialEq + Clone> Draft<T> {
    /// A clean draft whose value is already on disk.
    pub fn new(saved: T) -> Self {
        Self {
            seen: saved.clone(),
            saved,
            dirty: false,
            generation: 0,
        }
    }

    /// Record an editor notification. Returns the new generation for a real
    /// change, and `None` for a caret/selection notification or unchanged
    /// text.
    pub fn observe(&mut self, value: T) -> Option<u64> {
        if value == self.saved {
            self.seen = value;
            self.dirty = false;
            return None;
        }
        if self.dirty && value == self.seen {
            return None;
        }
        self.seen = value;
        self.dirty = true;
        self.generation = self.generation.wrapping_add(1);
        Some(self.generation)
    }

    /// Mark the draft dirty without an editor value (e.g. annotation edits).
    pub fn touch(&mut self) -> u64 {
        self.dirty = true;
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// True when no newer edit has superseded `generation`.
    pub fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }

    /// Record a successful write.
    pub fn mark_saved(&mut self, value: T) {
        self.seen = value.clone();
        self.saved = value;
        self.dirty = false;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn saved(&self) -> &T {
        &self.saved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_only_reports_real_changes() {
        let mut draft = Draft::new("a".to_string());
        assert_eq!(draft.observe("a".to_string()), None);
        assert!(!draft.is_dirty());

        let generation = draft.observe("b".to_string()).unwrap();
        assert!(draft.is_dirty());
        assert!(draft.is_current(generation));

        // A caret move reports the same buffer again: no new generation.
        assert_eq!(draft.observe("b".to_string()), None);
        assert!(draft.is_current(generation));

        // Reverting to the saved text clears the dirty flag.
        assert_eq!(draft.observe("a".to_string()), None);
        assert!(!draft.is_dirty());
    }

    #[test]
    fn mark_saved_clean_and_is_current_guards_stale_timers() {
        let mut draft = Draft::new("a".to_string());
        let first = draft.observe("b".to_string()).unwrap();
        let second = draft.observe("c".to_string()).unwrap();
        assert!(!draft.is_current(first));
        assert!(draft.is_current(second));

        draft.mark_saved("c".to_string());
        assert!(!draft.is_dirty());
        assert_eq!(draft.saved(), "c");
    }

    #[test]
    fn touch_bumps_without_an_editor_value() {
        let mut draft: Draft<Vec<u8>> = Draft::new(vec![]);
        let generation = draft.touch();
        assert!(draft.is_current(generation));
        assert!(draft.is_dirty());
        assert_eq!(draft.observe(vec![]), None);
    }
}
