//! Where you have been, and how to get back there.
//!
//! Two questions with different answers, which is why this is one type rather
//! than two lists. *Back* and *forward* walk a path that branches: going back
//! and then somewhere new discards what was ahead, the way a browser does.
//! *Recent* is the set of places visited, most recent first and each appearing
//! once, which is what a switcher should offer.

use gpui::SharedString;

/// The most conversations remembered. Past this the oldest are dropped: a
/// recents list nobody scrolls does not need to be unbounded.
const CAPACITY: usize = 50;

#[derive(Debug, Default)]
pub struct History {
    /// Visited conversations in order, oldest first.
    entries: Vec<SharedString>,
    /// Where in `entries` the reader currently is.
    cursor: usize,
}

impl History {
    /// Record a visit.
    ///
    /// Re-selecting the current conversation is not a visit, so it does not
    /// grow the path — otherwise Back would walk through repeats of one place.
    pub fn visit(&mut self, id: SharedString) {
        if self.current() == Some(&id) {
            return;
        }

        // Anything ahead of the cursor is a path not taken any more.
        if !self.entries.is_empty() {
            self.entries.truncate(self.cursor + 1);
        }

        self.entries.push(id);
        if self.entries.len() > CAPACITY {
            self.entries.remove(0);
        }
        self.cursor = self.entries.len() - 1;
    }

    pub fn current(&self) -> Option<&SharedString> {
        self.entries.get(self.cursor)
    }

    pub fn can_go_back(&self) -> bool {
        self.cursor > 0 && !self.entries.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.entries.is_empty() && self.cursor + 1 < self.entries.len()
    }

    /// Step back, returning where to go.
    pub fn back(&mut self) -> Option<SharedString> {
        if !self.can_go_back() {
            return None;
        }
        self.cursor -= 1;
        self.current().cloned()
    }

    pub fn forward(&mut self) -> Option<SharedString> {
        if !self.can_go_forward() {
            return None;
        }
        self.cursor += 1;
        self.current().cloned()
    }

    /// Visited conversations, most recent first, each once.
    pub fn recent(&self) -> Vec<SharedString> {
        let mut seen = Vec::new();
        for id in self.entries.iter().rev() {
            if !seen.contains(id) {
                seen.push(id.clone());
            }
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(ids: &[&str]) -> History {
        let mut history = History::default();
        for id in ids {
            history.visit(SharedString::from(id.to_string()));
        }
        history
    }

    #[test]
    fn a_fresh_history_goes_nowhere() {
        let mut history = History::default();
        assert!(!history.can_go_back());
        assert!(!history.can_go_forward());
        assert_eq!(history.back(), None);
        assert_eq!(history.forward(), None);
    }

    #[test]
    fn back_and_forward_walk_the_path() {
        let mut history = history(&["A", "B", "C"]);

        assert_eq!(history.current().map(|s| s.as_ref()), Some("C"));
        assert_eq!(history.back().as_deref(), Some("B"));
        assert_eq!(history.back().as_deref(), Some("A"));
        assert!(!history.can_go_back());
        assert_eq!(history.forward().as_deref(), Some("B"));
    }

    #[test]
    fn going_somewhere_new_discards_what_was_ahead() {
        let mut history = history(&["A", "B", "C"]);
        history.back();
        history.visit("D".into());

        // The path is now A → B → D, standing on D.
        assert!(!history.can_go_forward(), "C is no longer reachable");
        assert_eq!(history.back().as_deref(), Some("B"));
        assert_eq!(history.back().as_deref(), Some("A"));
    }

    #[test]
    fn reselecting_the_same_conversation_is_not_a_visit() {
        let mut history = history(&["A", "B"]);
        history.visit("B".into());

        assert_eq!(history.back().as_deref(), Some("A"));
        assert!(!history.can_go_back());
    }

    #[test]
    fn recent_lists_each_place_once_newest_first() {
        let history = history(&["A", "B", "A", "C"]);
        let recent: Vec<String> = history.recent().iter().map(|s| s.to_string()).collect();
        assert_eq!(recent, vec!["C", "A", "B"]);
    }

    #[test]
    fn recent_reflects_where_you_are_after_going_back() {
        let mut history = history(&["A", "B", "C"]);
        history.back();
        history.visit("D".into());

        let recent: Vec<String> = history.recent().iter().map(|s| s.to_string()).collect();
        assert_eq!(recent, vec!["D", "B", "A"]);
    }

    #[test]
    fn the_path_stays_bounded() {
        let ids: Vec<String> = (0..CAPACITY + 10).map(|i| i.to_string()).collect();
        let mut history = History::default();
        for id in &ids {
            history.visit(SharedString::from(id.clone()));
        }

        assert_eq!(history.recent().len(), CAPACITY);
        assert_eq!(
            history.current().map(|s| s.to_string()),
            ids.last().cloned()
        );
    }
}
