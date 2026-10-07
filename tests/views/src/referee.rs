//! An outside check of watch admission, initial snapshots, and flow control.

use jig_core_views::{Chunk, Event, Request};
use skein_lib::Token;
use std::collections::BTreeMap;

/// What the scripted parent can observe without inspecting domain state.
#[derive(Debug, Default)]
pub struct Referee {
    open: BTreeMap<Token, bool>,
    pub deliveries: u64,
    pub missed: u64,
    pub ended: u64,
}

impl Referee {
    pub fn before(&mut self, event: &Event) {
        match event {
            Event::Delivered { watcher, .. } => {
                let flight = self.open.get_mut(watcher).expect("delivery belongs to an open watch");
                assert!(*flight, "delivery has an outstanding request");
                *flight = false;
            }
            Event::Started { .. }
            | Event::Reported { .. }
            | Event::Turn { .. }
            | Event::Finished { .. }
            | Event::TaskPhase { .. }
            | Event::Inbox { .. }
            | Event::Watch { .. }
            | Event::Unwatch { .. } => {}
        }
    }

    pub fn saw(&mut self, requests: &[Request]) {
        let mut newly_watching = None;
        for request in requests {
            match request {
                Request::Watching { watcher } => {
                    assert!(self.open.insert(*watcher, false).is_none(), "one admission per name");
                    newly_watching = Some(*watcher);
                }
                Request::Refused { watcher, .. } => {
                    assert!(!self.open.contains_key(watcher), "refusal does not open a watch");
                }
                Request::Deliver { watcher, missed, chunks } => {
                    let flight = self.open.get_mut(watcher).expect("delivery belongs to a watch");
                    assert!(!*flight, "one delivery in flight");
                    *flight = true;
                    if newly_watching == Some(*watcher) {
                        assert!(matches!(chunks.as_ref(), [Chunk::Snapshot { .. }]), "snapshot first");
                    }
                    self.deliveries += 1;
                    self.missed += missed;
                }
                Request::Ended { watcher, .. } => {
                    assert_eq!(self.open.remove(watcher), Some(false), "end follows delivery terminal");
                    self.ended += 1;
                }
            }
        }
        if let Some(watcher) = newly_watching {
            assert_eq!(self.open.get(&watcher), Some(&true), "admission delivers snapshot");
        }
    }

    #[must_use]
    pub fn live(&self) -> usize {
        self.open.len()
    }
}
