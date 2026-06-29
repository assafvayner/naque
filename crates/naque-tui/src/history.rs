//! In-memory, session-scoped command history with readline-style navigation.
//!
//! Records every non-empty submitted line and lets the input prompt walk
//! backward (Up) and forward (Down) through it, restoring the in-progress
//! draft when navigation moves past the newest entry. Consecutive duplicates
//! are suppressed (bash `ignoredups`). Nothing is persisted to disk.

#[derive(Debug, Default)]
pub struct History {
    entries: Vec<String>,
    /// None = editing the live draft; Some(i) = currently viewing entries[i].
    pos: Option<usize>,
    /// The in-progress line saved when navigation begins, restored on Down past newest.
    draft: String,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a submitted line and reset navigation state.
    ///
    /// Trims the line, ignores it when empty, and ignores a consecutive
    /// duplicate of the last entry.
    pub fn push(&mut self, line: &str) {
        let trimmed = line.trim();
        if !trimmed.is_empty() && self.entries.last().map(String::as_str) != Some(trimmed) {
            self.entries.push(trimmed.to_string());
        }
        self.pos = None;
        self.draft.clear();
    }

    /// Up: move to an older entry. On the first call (not yet navigating),
    /// `current` is saved as the draft. Returns the text to place in the
    /// input, or None if there is no history (input should stay unchanged).
    pub fn older(&mut self, current: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        match self.pos {
            None => {
                self.draft = current.to_string();
                self.pos = Some(self.entries.len() - 1);
            },
            Some(0) => {},
            Some(i) => self.pos = Some(i - 1),
        }
        self.pos.map(|i| self.entries[i].clone())
    }

    /// Down: move to a newer entry. Returns the entry text, or the restored
    /// draft (possibly empty) when moving past the newest entry, or None if
    /// not currently navigating (input should stay unchanged).
    pub fn newer(&mut self) -> Option<String> {
        match self.pos {
            None => None,
            Some(i) if i + 1 < self.entries.len() => {
                self.pos = Some(i + 1);
                Some(self.entries[i + 1].clone())
            },
            Some(_) => {
                self.pos = None;
                Some(std::mem::take(&mut self.draft))
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_history_older_is_none() {
        let mut h = History::new();
        assert_eq!(h.older("draft"), None);
    }

    #[test]
    fn newer_without_navigating_is_none() {
        let mut h = History::new();
        h.push("a");
        assert_eq!(h.newer(), None);
    }

    #[test]
    fn push_then_older_returns_last() {
        let mut h = History::new();
        h.push("select 1");
        assert_eq!(h.older(""), Some("select 1".to_string()));
    }

    #[test]
    fn older_walks_backward_and_clamps_at_oldest() {
        let mut h = History::new();
        h.push("first");
        h.push("second");
        h.push("third");
        assert_eq!(h.older(""), Some("third".to_string()));
        assert_eq!(h.older(""), Some("second".to_string()));
        assert_eq!(h.older(""), Some("first".to_string()));
        // Clamped at the oldest entry.
        assert_eq!(h.older(""), Some("first".to_string()));
    }

    #[test]
    fn newer_walks_forward() {
        let mut h = History::new();
        h.push("first");
        h.push("second");
        h.push("third");
        h.older(""); // third
        h.older(""); // second
        h.older(""); // first
        assert_eq!(h.newer(), Some("second".to_string()));
        assert_eq!(h.newer(), Some("third".to_string()));
    }

    #[test]
    fn newer_past_newest_restores_draft() {
        let mut h = History::new();
        h.push("entry");
        assert_eq!(h.older("in-progress"), Some("entry".to_string()));
        // Moving forward past the newest entry restores the saved draft.
        assert_eq!(h.newer(), Some("in-progress".to_string()));
        // No longer navigating.
        assert_eq!(h.newer(), None);
    }

    #[test]
    fn newer_past_newest_restores_empty_draft() {
        let mut h = History::new();
        h.push("entry");
        assert_eq!(h.older(""), Some("entry".to_string()));
        assert_eq!(h.newer(), Some(String::new()));
    }

    #[test]
    fn draft_captured_from_current_on_first_older() {
        let mut h = History::new();
        h.push("a");
        h.push("b");
        assert_eq!(h.older("typing this"), Some("b".to_string()));
        h.older(""); // a
        // Down all the way back restores the originally-captured draft.
        h.newer(); // b
        assert_eq!(h.newer(), Some("typing this".to_string()));
    }

    #[test]
    fn consecutive_duplicate_suppressed() {
        let mut h = History::new();
        h.push("same");
        h.push("same");
        assert_eq!(h.older(""), Some("same".to_string()));
        // Only one entry recorded, so navigation clamps here.
        assert_eq!(h.older(""), Some("same".to_string()));
    }

    #[test]
    fn non_consecutive_duplicate_recorded() {
        let mut h = History::new();
        h.push("a");
        h.push("b");
        h.push("a");
        assert_eq!(h.older(""), Some("a".to_string()));
        assert_eq!(h.older(""), Some("b".to_string()));
        assert_eq!(h.older(""), Some("a".to_string()));
    }

    #[test]
    fn empty_and_whitespace_push_ignored() {
        let mut h = History::new();
        h.push("");
        h.push("   ");
        h.push("\t\n");
        assert_eq!(h.older(""), None);
    }

    #[test]
    fn push_trims_the_line() {
        let mut h = History::new();
        h.push("  trimmed  ");
        assert_eq!(h.older(""), Some("trimmed".to_string()));
    }

    #[test]
    fn push_resets_navigation() {
        let mut h = History::new();
        h.push("first");
        h.push("second");
        h.older(""); // second
        h.older(""); // first, now navigating mid-history
        h.push("third");
        // After push, navigation starts fresh from the newest entry.
        assert_eq!(h.older(""), Some("third".to_string()));
    }
}
