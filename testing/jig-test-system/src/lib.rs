//! A fake system that receives the test connector's calls and reports what it
//! saw in its own terms (domain/testing.md, section 7).
//!
//! Its keyed operations, guarded states and late copies are independent of
//! the connector's records. Worlds may preload unrelated history and compare
//! call counts to show that idle load does not follow system history.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};

use jig_test_connector::{
    ApplyResult, Effect, Fact, Form, Key, Looked, Origin, Path, Recovery, SystemEvent, SystemRequest,
};

/// A resource name recorded as bytes by this fake system.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Name(pub Vec<Vec<u8>>);

impl Name {
    /// Copies a connector path at the system's boundary.
    #[must_use]
    pub fn from_path(path: &Path) -> Name {
        let mut parts = Vec::new();
        for part in path.segments() {
            parts.push(part.to_vec());
        }
        Name(parts)
    }
}

/// A key as this system observes it, without a connector's types.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SystemKey {
    pub attempt: u64,
    pub completion: u32,
    pub position: u32,
    /// Deployment that made the request.
    pub deployment: [u8; 16],
    /// Asking task.
    pub task: u64,
    /// Purpose in that task.
    pub purpose: u64,
}

impl From<Key> for SystemKey {
    fn from(key: Key) -> SystemKey {
        SystemKey {
            deployment: key.deployment,
            task: key.task,
            purpose: key.purpose,
            attempt: key.attempt,
            completion: key.completion,
            position: key.position,
        }
    }
}

/// Which copy of a write the system received.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Copy {
    /// First attempt delivered on time.
    First,
    /// An attempt arrived after its caller timed out.
    Late,
    /// The connector sent the effect again.
    Retried,
}

/// One effect this fake received, including rejected copies.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Observed {
    /// Numbered effect kind.
    pub kind: u16,
    /// Deployment-scoped operation key.
    pub key: SystemKey,
    /// Every resource the write reached.
    pub resources: Vec<Name>,
    /// Starting state checked by the system, if any.
    pub condition: Option<u64>,
    /// State requested by this copy.
    pub target: u64,
    /// Values and owners immediately before this copy arrived.
    pub before: Vec<ValueObserved>,
    /// Whether this copy changed the target.
    pub applied: bool,
    /// Delivery class of this copy.
    pub copy: Copy,
}

/// One system value, including whether this deployment made it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ValueObserved {
    pub state: Option<u64>,
    pub owner: Option<SystemKey>,
}

/// One fact read, as observed beneath the connector.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ReadObserved {
    pub resource: Name,
    pub state: Option<u64>,
    pub at: u64,
}

/// How a fake call's answer behaves.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Fault {
    /// Deliver and answer normally.
    None,
    /// Fail before the write reaches this system.
    BeforeSend,
    /// Apply the write, then lose the answer.
    AfterApply,
    /// Hold this copy until the world delivers it late.
    Late,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Value {
    state: u64,
    owner: Option<SystemKey>,
}

#[derive(Debug)]
struct Pending {
    entry: u64,
    attempt: u32,
    key: Key,
    effect: Effect,
    form: Form,
    recovery: Recovery,
}

/// A deterministic system with keyed writes, conditions and scheduled late copies.
#[derive(Debug, Default)]
pub struct System {
    values: BTreeMap<Name, Value>,
    keyed: BTreeMap<SystemKey, u64>,
    pending: VecDeque<Pending>,
    observed: Vec<Observed>,
    reads: Vec<ReadObserved>,
    history: Vec<u64>,
    calls: u64,
}

impl System {
    /// A new empty fake system.
    #[must_use]
    pub fn new() -> System {
        System::default()
    }

    /// Effects this system received, in arrival order and in its own terms.
    #[must_use]
    pub fn observed(&self) -> &[Observed] {
        &self.observed
    }

    /// Fact reads received, independently of connector verdicts.
    #[must_use]
    pub fn reads(&self) -> &[ReadObserved] {
        &self.reads
    }

    /// Calls received, including lookups, but excluding preloaded history.
    #[must_use]
    pub const fn calls(&self) -> u64 {
        self.calls
    }

    /// Copies still waiting to arrive late.
    #[must_use]
    pub fn late_copies(&self) -> usize {
        self.pending.len()
    }

    /// Past unrelated objects; only live names are consulted by calls.
    pub fn preload_history(&mut self, count: u32) {
        for number in 0..count {
            self.history.push(u64::from(number));
        }
    }

    /// Another hand writes a target without this deployment's key.
    pub fn other_hand(&mut self, path: &Path, state: u64) {
        self.values.insert(Name::from_path(path), Value { state, owner: None });
    }

    /// State now visible at one resource.
    #[must_use]
    pub fn state(&self, path: &Path) -> Option<u64> {
        self.values.get(&Name::from_path(path)).map(|value| value.state)
    }

    /// Execute one released connector request under the chosen fault.
    pub fn answer(&mut self, request: SystemRequest, fault: Fault) -> SystemEvent {
        self.calls = self.calls.checked_add(1).expect("test call count fits");
        match request {
            SystemRequest::ReadFact { resource, observed } => {
                let value = self.values.get(&Name::from_path(&resource));
                self.reads.push(ReadObserved {
                    resource: Name::from_path(&resource),
                    state: value.map(|row| row.state),
                    at: observed.as_nanos(),
                });
                SystemEvent::Fact {
                    resource,
                    fact: Fact { state: value.map(|row| row.state), observed, pending: false },
                    origin: if value.is_some_and(|row| row.owner.is_some()) { Origin::Own } else { Origin::Other },
                }
            }
            SystemRequest::Apply { entry, attempt, key, effect, form, recovery } => {
                if fault == Fault::BeforeSend {
                    return SystemEvent::Applied { entry, attempt, result: ApplyResult::Transient };
                }
                if fault == Fault::Late {
                    self.pending.push_back(Pending { entry, attempt, key, effect, form, recovery });
                    return SystemEvent::Applied { entry, attempt, result: ApplyResult::Uncertain };
                }
                let copy = if attempt > 1 { Copy::Retried } else { Copy::First };
                let result = self.apply(key, &effect, form, recovery, copy);
                if fault == Fault::AfterApply && matches!(result, ApplyResult::Made { .. }) {
                    SystemEvent::Applied { entry, attempt, result: ApplyResult::Uncertain }
                } else {
                    SystemEvent::Applied { entry, attempt, result }
                }
            }
            SystemRequest::Look { entry, key, effect, recovery } => {
                let system_key = SystemKey::from(key);
                let first = effect.resources.first().map(Name::from_path);
                let value = first.and_then(|name| self.values.get(&name)).copied();
                let looked = Looked {
                    state: value.map(|row| row.state),
                    owner: value.and_then(|row| row.owner).map(to_connector_key),
                    found_key: recovery == Recovery::Keyed && self.keyed.contains_key(&system_key),
                };
                SystemEvent::Looked { entry, looked }
            }
        }
    }

    /// Deliver one copy after its caller already heard uncertainty.
    pub fn deliver_late(&mut self) -> Option<SystemEvent> {
        let pending = self.pending.pop_front()?;
        let result = self.apply(pending.key, &pending.effect, pending.form, pending.recovery, Copy::Late);
        Some(SystemEvent::Applied { entry: pending.entry, attempt: pending.attempt, result })
    }

    fn apply(&mut self, key: Key, effect: &Effect, form: Form, recovery: Recovery, copy: Copy) -> ApplyResult {
        let system_key = SystemKey::from(key);
        let mut resources = Vec::new();
        for path in &effect.resources {
            resources.push(Name::from_path(path));
        }
        let before: Vec<_> = resources
            .iter()
            .map(|name| {
                let value = self.values.get(name);
                ValueObserved { state: value.map(|row| row.state), owner: value.and_then(|row| row.owner) }
            })
            .collect();
        if recovery == Recovery::Keyed
            && let Some(state) = self.keyed.get(&system_key)
        {
            self.observed.push(Observed {
                kind: effect.kind,
                key: system_key,
                resources,
                condition: effect.condition,
                target: effect.target,
                before,
                applied: false,
                copy,
            });
            return ApplyResult::Made { state: *state };
        }
        for name in &resources {
            let old = self.values.get(name).map(|row| row.state);
            if let Some(expected) = effect.condition
                && old.unwrap_or(0) != expected
            {
                self.observe(effect, system_key, resources, before, false, copy);
                return ApplyResult::Conflict;
            }
            if form == Form::Creation && old.is_some() {
                self.observe(effect, system_key, resources, before, false, copy);
                return ApplyResult::Conflict;
            }
        }
        for name in &resources {
            self.values.insert(name.clone(), Value { state: effect.target, owner: Some(system_key) });
        }
        if recovery == Recovery::Keyed {
            self.keyed.insert(system_key, effect.target);
        }
        self.observe(effect, system_key, resources, before, true, copy);
        ApplyResult::Made { state: effect.target }
    }

    fn observe(
        &mut self,
        effect: &Effect,
        key: SystemKey,
        resources: Vec<Name>,
        before: Vec<ValueObserved>,
        applied: bool,
        copy: Copy,
    ) {
        self.observed.push(Observed {
            kind: effect.kind,
            key,
            resources,
            condition: effect.condition,
            target: effect.target,
            before,
            applied,
            copy,
        });
    }
}

fn to_connector_key(key: SystemKey) -> Key {
    Key {
        deployment: key.deployment,
        task: key.task,
        purpose: key.purpose,
        attempt: key.attempt,
        completion: key.completion,
        position: key.position,
    }
}

#[cfg(test)]
mod tests;
