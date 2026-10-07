//! Expectations over client replies and durable task creations, never state.
use jig_core_people::{RequestKey, Role};
use skein_lib::Duration;
use skein_world::domain::{Expectations, Judge};
use std::collections::BTreeSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RouteKind {
    Chat,
    Goal,
    Service,
    Proposal,
    TakePerson,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Seen {
    Called { call: u64 },
    Abandoned { call: u64 },
    Routed { role: Role, kind: RouteKind, service: bool },
    RoleWaitingOpened { task: u64, both_present: bool },
    RoleWaitingResolved { task: u64, both_absent: bool },
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
    role_waiting: BTreeSet<u64>,
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
            Seen::Routed { role, kind, service } => match kind {
                RouteKind::Chat if service || role == Role::Observer => judge.fail("chat beyond party role or kind"),
                RouteKind::Goal | RouteKind::Proposal | RouteKind::TakePerson if role == Role::Observer => {
                    judge.fail("observer request routed");
                }
                RouteKind::Service if role != Role::Owner => judge.fail("service creation beyond owner role"),
                RouteKind::Chat
                | RouteKind::Goal
                | RouteKind::Service
                | RouteKind::Proposal
                | RouteKind::TakePerson => {}
            },
            Seen::RoleWaitingOpened { task, both_present } => {
                if !both_present || !self.role_waiting.insert(task) {
                    judge.fail("role request was not in every holder inbox");
                }
            }
            Seen::RoleWaitingResolved { task, both_absent } => {
                if !both_absent || !self.role_waiting.remove(&task) {
                    judge.fail("acted role request remained in a holder inbox");
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
