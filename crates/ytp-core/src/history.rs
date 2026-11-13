//! Undo/redo for Studio edits: a bounded list of [`Project`] snapshots with a
//! cursor naming the one that is current.
//!
//! [`Project`] is plain data, so a snapshot is a clone; no diffing is involved
//! and nothing here knows what an edit is. [`History::apply`] pushes whatever
//! the caller produced, [`History::undo`] and [`History::redo`] move the cursor
//! and hand back the snapshot to show.
//!
//! The list is bounded: once it holds [`History::capacity`] snapshots, the
//! oldest is dropped, so a long session cannot grow without limit. A capacity
//! of 0 or 1 keeps only the current state, i.e. no history.

use crate::timeline::Project;

/// Snapshots kept by [`History::new`] unless [`History::set_capacity`] says
/// otherwise.
pub const DEFAULT_CAPACITY: usize = 200;

/// A bounded undo/redo stack of project snapshots.
#[derive(Debug, Clone)]
pub struct History {
    /// What the caller asked for; 0 means "current state only".
    capacity: usize,
    /// Snapshots, oldest first. Never empty.
    states: Vec<Project>,
    /// Index in `states` of the snapshot that is current.
    cursor: usize,
}

impl History {
    /// A history holding `project` as its only, current, state.
    pub fn new(project: Project) -> History {
        History {
            capacity: DEFAULT_CAPACITY,
            states: vec![project],
            cursor: 0,
        }
    }

    /// The configured capacity; 0 and 1 both mean no history.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Change how many snapshots are kept. Shrinking drops the oldest ones but
    /// never drops the current state; 0 is treated as 1.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
        let cap = self.keep();
        if self.states.len() <= cap {
            return;
        }
        // Drop the oldest states, but stop at the cursor so the current
        // snapshot survives.
        let excess = self.states.len() - cap;
        let drop = excess.min(self.cursor);
        self.states.drain(0..drop);
        self.cursor -= drop;
        // If the cursor was near the front, the tail is what is left to trim.
        let cap = self.keep();
        if self.states.len() > cap {
            self.states.truncate(cap);
            self.cursor = self.cursor.min(self.states.len() - 1);
        }
    }

    /// How many snapshots are actually kept: at least one, so `current` always
    /// has something to return.
    fn keep(&self) -> usize {
        self.capacity.max(1)
    }

    /// Record `new` as the newest state and make it current. Anything that
    /// could have been redone is discarded. When the stack is full the oldest
    /// snapshot is dropped.
    pub fn apply(&mut self, new: Project) {
        self.states.truncate(self.cursor + 1);
        self.states.push(new);
        let cap = self.keep();
        if self.states.len() > cap {
            let excess = self.states.len() - cap;
            self.states.drain(0..excess);
        }
        self.cursor = self.states.len() - 1;
    }

    /// The snapshot one edit back, made current, or `None` at the oldest state.
    pub fn undo(&mut self) -> Option<&Project> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        Some(&self.states[self.cursor])
    }

    /// The snapshot one edit forward, made current, or `None` at the newest.
    pub fn redo(&mut self) -> Option<&Project> {
        if self.cursor + 1 >= self.states.len() {
            return None;
        }
        self.cursor += 1;
        Some(&self.states[self.cursor])
    }

    /// Is there an older snapshot to undo to?
    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    /// Is there a newer snapshot to redo to?
    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.states.len()
    }

    /// The snapshot the cursor names.
    pub fn current(&self) -> &Project {
        &self.states[self.cursor]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> Project {
        Project::new(name)
    }

    #[test]
    fn undo_and_redo_roundtrip() {
        let mut h = History::new(project("a"));
        assert_eq!(h.current().name, "a");
        assert!(!h.can_undo());
        assert!(!h.can_redo());
        assert!(h.undo().is_none());
        assert!(h.redo().is_none());

        h.apply(project("b"));
        h.apply(project("c"));
        assert_eq!(h.current().name, "c");
        assert!(h.can_undo());
        assert!(!h.can_redo());

        assert_eq!(h.undo().map(|p| p.name.as_str()), Some("b"));
        assert_eq!(h.undo().map(|p| p.name.as_str()), Some("a"));
        assert!(h.undo().is_none());
        assert_eq!(h.current().name, "a");

        assert_eq!(h.redo().map(|p| p.name.as_str()), Some("b"));
        assert_eq!(h.redo().map(|p| p.name.as_str()), Some("c"));
        assert!(h.redo().is_none());
        assert_eq!(h.current().name, "c");
    }

    #[test]
    fn stack_never_exceeds_capacity() {
        let mut h = History::new(project("s0"));
        assert_eq!(h.capacity(), DEFAULT_CAPACITY);
        h.set_capacity(3);
        for i in 1..=6 {
            h.apply(project(&format!("s{i}")));
        }
        assert_eq!(h.current().name, "s6");

        // Seven states were produced; only the newest three survive, so the
        // furthest back undo reaches is s4.
        let mut names = Vec::new();
        while let Some(p) = h.undo() {
            names.push(p.name.clone());
        }
        assert_eq!(names, vec!["s5".to_string(), "s4".to_string()]);
        assert_eq!(h.current().name, "s4");
        assert!(!h.can_undo());
        assert!(h.can_redo());
    }

    #[test]
    fn redo_after_undo_reaches_newest_then_stops() {
        let mut h = History::new(project("a"));
        h.apply(project("b"));
        h.apply(project("c"));

        assert_eq!(h.undo().map(|p| p.name.as_str()), Some("b"));
        assert_eq!(h.undo().map(|p| p.name.as_str()), Some("a"));
        assert_eq!(h.redo().map(|p| p.name.as_str()), Some("b"));
        assert_eq!(h.redo().map(|p| p.name.as_str()), Some("c"));
        assert!(h.redo().is_none());
        assert_eq!(h.current().name, "c");
        assert!(!h.can_redo());
    }

    #[test]
    fn apply_after_undo_clears_redo() {
        let mut h = History::new(project("a"));
        h.apply(project("b"));
        h.apply(project("c"));

        assert_eq!(h.undo().map(|p| p.name.as_str()), Some("b"));
        h.apply(project("d"));
        assert_eq!(h.current().name, "d");
        assert!(!h.can_redo());
        assert!(h.redo().is_none());

        // The abandoned branch (c) is gone; undo walks the kept one.
        assert_eq!(h.undo().map(|p| p.name.as_str()), Some("b"));
        assert_eq!(h.undo().map(|p| p.name.as_str()), Some("a"));
        assert!(h.undo().is_none());
    }

    #[test]
    fn current_follows_the_cursor() {
        let mut h = History::new(project("a"));
        h.apply(project("b"));
        h.apply(project("c"));

        assert_eq!(h.current().name, "c");
        h.undo();
        assert_eq!(h.current().name, "b");
        h.undo();
        assert_eq!(h.current().name, "a");
        h.redo();
        assert_eq!(h.current().name, "b");
        h.apply(project("e"));
        assert_eq!(h.current().name, "e");
    }

    #[test]
    fn tiny_capacity_keeps_only_the_current_state() {
        for cap in [0, 1] {
            let mut h = History::new(project("a"));
            h.set_capacity(cap);
            assert_eq!(h.capacity(), cap);
            h.apply(project("b"));
            h.apply(project("c"));
            assert_eq!(h.current().name, "c");
            assert!(!h.can_undo());
            assert!(!h.can_redo());
            assert!(h.undo().is_none());
            assert!(h.redo().is_none());
            assert_eq!(h.current().name, "c");
        }
    }

    #[test]
    fn shrinking_capacity_keeps_the_current_state() {
        let mut h = History::new(project("a"));
        h.apply(project("b"));
        h.apply(project("c"));
        assert_eq!(h.undo().map(|p| p.name.as_str()), Some("b"));

        h.set_capacity(1);
        assert_eq!(h.current().name, "b");
        assert!(!h.can_undo());
        assert!(!h.can_redo());
        assert!(h.undo().is_none());
    }
}
