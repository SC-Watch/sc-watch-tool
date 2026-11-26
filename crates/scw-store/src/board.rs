//! What the watcher is doing that the page shows but no database holds -
//! the mining overlay's state. One process only (`sc-watch`): the watcher
//! writes it, the page's live stream reads it, and a change wakes the
//! stream as a commit does. Not in the Python, whose page and watcher are
//! two programs; the page without a board (`sc-watch-ui`) is unchanged.

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::db::Changes;

#[derive(Debug, Default)]
pub struct Board {
    /// A number that goes up with each change, and the value (None: nothing
    /// to show, e.g. the mining overlay is off).
    state: Mutex<(u64, Option<Value>)>,
    changes: Option<Arc<Changes>>,
}

impl Board {
    /// A board whose changes wake whoever waits on `changes`.
    pub fn new(changes: Option<Arc<Changes>>) -> Self {
        Board {
            state: Mutex::new((0, None)),
            changes,
        }
    }

    /// Shows `value` (None: nothing); a value equal to the one shown is no
    /// change.
    pub fn set(&self, value: Option<Value>) {
        {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if s.1 == value {
                return;
            }
            s.0 += 1;
            s.1 = value;
        }
        if let Some(c) = &self.changes {
            c.bump();
        }
    }

    /// The change number and the value.
    pub fn get(&self) -> (u64, Option<Value>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_change_counts_and_wakes_an_equal_one_does_not() {
        let changes = Arc::new(Changes::default());
        let board = Board::new(Some(changes.clone()));
        assert_eq!(board.get(), (0, None));
        board.set(None);
        assert_eq!((board.get().0, changes.seen()), (0, 0));
        board.set(Some(json!({"state": "looking"})));
        assert_eq!(board.get(), (1, Some(json!({"state": "looking"}))));
        assert_eq!(changes.seen(), 1);
        board.set(Some(json!({"state": "looking"})));
        assert_eq!((board.get().0, changes.seen()), (1, 1));
        board.set(None);
        assert_eq!(board.get(), (2, None));
        assert_eq!(changes.seen(), 2);
    }
}
