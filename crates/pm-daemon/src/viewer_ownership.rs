use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::watch;

#[derive(Clone, Default)]
pub(crate) struct Ownership {
    pub revision: u64,
    pub owner: u64,
    pub cols: u16,
    pub rows: u16,
    requests: HashMap<u64, u64>,
}

impl Ownership {
    pub fn acknowledgment(&self, viewer: u64) -> u64 {
        self.requests.get(&viewer).copied().unwrap_or_default()
    }
}

#[derive(Default)]
pub(crate) struct ViewerOwners {
    terminals: Mutex<HashMap<(u64, u64), watch::Sender<Ownership>>>,
}

impl ViewerOwners {
    pub fn subscribe(&self, terminal: u64, generation: u64) -> watch::Receiver<Ownership> {
        self.terminals
            .lock()
            .unwrap()
            .entry((terminal, generation))
            .or_insert_with(|| watch::channel(Ownership::default()).0)
            .subscribe()
    }

    pub fn claim(
        &self,
        terminal: u64,
        generation: u64,
        viewer: u64,
        request: u64,
        size: (u16, u16),
        resize: impl FnOnce(),
    ) {
        let mut terminals = self.terminals.lock().unwrap();
        let sender = terminals
            .entry((terminal, generation))
            .or_insert_with(|| watch::channel(Ownership::default()).0);
        let mut next = sender.borrow().clone();
        if request != 0 && request <= next.acknowledgment(viewer) {
            return;
        }
        resize();
        next.revision += 1;
        next.owner = viewer;
        next.cols = size.0;
        next.rows = size.1;
        if request != 0 {
            next.requests.insert(viewer, request);
        }
        sender.send_replace(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn duplicate_and_superseded_claims_do_not_reclaim_another_viewer() {
        let owners = ViewerOwners::default();
        let feed = owners.subscribe(1, 1);
        let calls = Cell::new(0);
        let resized = || calls.set(calls.get() + 1);
        owners.claim(1, 1, 10, 2, (120, 40), resized);
        owners.claim(1, 1, 20, 1, (80, 24), resized);
        owners.claim(1, 1, 10, 1, (100, 30), resized);
        owners.claim(1, 1, 10, 2, (120, 40), resized);
        assert_eq!(calls.get(), 2);
        let state = feed.borrow();
        assert_eq!(state.owner, 20);
        assert_eq!(state.revision, 2);
        assert_eq!(state.acknowledgment(10), 2);
        assert_eq!((state.cols, state.rows), (80, 24));
    }

    #[test]
    fn coalesced_ownership_keeps_each_viewers_acknowledgment() {
        let owners = ViewerOwners::default();
        let feed = owners.subscribe(1, 1);
        owners.claim(1, 1, 10, 1, (120, 40), || {});
        owners.claim(1, 1, 20, 1, (80, 24), || {});
        owners.claim(1, 1, 10, 2, (120, 40), || {});
        let state = feed.borrow();
        assert_eq!(state.owner, 10);
        assert_eq!(state.acknowledgment(10), 2);
        assert_eq!(state.acknowledgment(20), 1);
        assert_eq!(state.revision, 3);
    }

    #[test]
    fn generations_and_terminals_have_independent_ownership() {
        let owners = ViewerOwners::default();
        let resumed = owners.subscribe(1, 2);
        let other = owners.subscribe(2, 1);
        owners.claim(1, 1, 10, 1, (120, 40), || {});
        assert_eq!(resumed.borrow().revision, 0);
        assert_eq!(other.borrow().revision, 0);
    }
}
