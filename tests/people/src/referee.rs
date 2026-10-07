//! Expectations over client replies and durable task creations, never state.
use jig_core_people::{RequestKey, Role};
use skein_lib::Duration;
use skein_world::domain::{Expectations, Judge};
use std::collections::BTreeSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Seen {
    Called { call: u64 },
    Abandoned { call: u64 },
    Routed { role: Role },
    Durable { commit: u64 },
    Created { key: RequestKey },
    Replied { call: u64, after: u64 },
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Name {
    Reply(u64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stimulus {
    Restart,
}

#[derive(Default, Debug)]
pub struct People {
    durable: u64,
    replied: BTreeSet<u64>,
    created: BTreeSet<RequestKey>,
}

impl Expectations for People {
    type Seen = Seen;

    type Name = Name;

    type Stimulus = Stimulus;

    fn observe(&mut self, seen: Seen, judge: &mut Judge<Name, Stimulus>) {
        match seen {
            Seen::Called { call } => judge.expect(Name::Reply(call), Duration::from_secs(10)),
            Seen::Abandoned { call } => {
                judge.meet(&Name::Reply(call));
            }
            Seen::Routed { role } => {
                if role == Role::Observer {
                    judge.fail("observer request routed");
                }
            }
            Seen::Durable { commit } => self.durable = commit,
            Seen::Created { key } => {
                if !self.created.insert(key) {
                    judge.fail("key made a task twice");
                }
            }
            Seen::Replied { call, after } => {
                if after > self.durable {
                    judge.fail("reply before its commit is durable");
                }
                if !self.replied.insert(call) {
                    judge.fail("call replied twice");
                }
                judge.meet(&Name::Reply(call));
            }
        }
    }
}
