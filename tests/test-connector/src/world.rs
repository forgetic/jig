use std::collections::{BTreeMap, BTreeSet, VecDeque};

use jig_test_connector::{
    self as connector, Config, Domain, EffectPhase, Event, Form, Hold, HoldMode, Key, KindSpec, Limits, Path, PoolSpec,
    ProcedureAction, ProcedureResult, ProcedureSpec, Record, RecordKey, Recovery, Request, RequirementSpec,
    ResourceSpec, RestartStep, SystemRequest, TopicSpec,
};
use jig_test_system::{Fault, System};
use skein_lib::{Duration, Env, List, Queue, Time, Wall};

/// Bounds shared by the seeded scenarios. Every vector they own fits here.
pub const LIMITS: Limits = Limits {
    tasks: 8,
    adoptions: 8,
    subscriptions: 8,
    pools: 1,
    resources: 2,
    topics: 1,
    kinds: 4,
    requirements: 2,
    procedures: 2,
    actions_per_procedure: 5,
    facts: 2,
    judges: 4,
    values: 8,
    value_bytes: 18,
    staged: 4,
    entries: 4,
    made: 8,
    resources_per_effect: 1,
    max_attempts: 3,
    write_lifetime: Duration::from_nanos(5),
    clock_margin: Duration::from_nanos(1),
    resources_per_task: 2,
    subscribers_per_topic: 8,
    lost_per_pool: 8,
    path_segments: 2,
    segment_bytes: 16,
};

/// One seeded resource path; zero is the deployment prefix.
#[must_use]
pub fn path(seed: u8, name: u8) -> Path {
    let mut segments = List::with_capacity(2);
    segments.push(Box::from([seed])).expect("one-byte seed fits");
    if name != 0 {
        segments.push(Box::from([name])).expect("one-byte resource fits");
    }
    Path::new(segments, LIMITS.segment_bytes).expect("seeded path fits")
}

/// Number of one of four effect kinds in this deployment.
#[must_use]
pub const fn kind(seed: u8, local: u16) -> u16 {
    (seed as u16) * 4 + local
}

/// Connector configuration drawn from a seed, including its procedure and judge.
#[must_use]
pub fn config(seed: u8) -> Config {
    Config {
        deployment: (u128::from(seed) + 1).to_be_bytes(),
        prefix: path(seed, 0),
        resources: Box::from([
            ResourceSpec { path: path(seed, 1), hold: Hold::Exclusive { mode: HoldMode::Wait }, writable: true },
            ResourceSpec { path: path(seed, 2), hold: Hold::Pooled { mode: HoldMode::Wait }, writable: true },
        ]),
        pools: Box::from([PoolSpec { path: path(seed, 2), slots: 2 }]),
        topics: Box::from([TopicSpec { topic: kind(seed, 1) }]),
        kinds: Box::from([
            KindSpec { kind: kind(seed, 1), form: Form::Creation, recovery: Recovery::Keyed, price: Some(3) },
            KindSpec { kind: kind(seed, 2), form: Form::Transition, recovery: Recovery::Conditional, price: Some(4) },
            KindSpec { kind: kind(seed, 3), form: Form::Set, recovery: Recovery::Idempotent, price: Some(2) },
            KindSpec { kind: kind(seed, 4), form: Form::Transition, recovery: Recovery::Unrecoverable, price: Some(1) },
        ]),
        requirements: Box::from([
            RequirementSpec { number: kind(seed, 1), guarded: true, freshness: Duration::from_nanos(4) },
            RequirementSpec { number: kind(seed, 2), guarded: false, freshness: Duration::from_nanos(4) },
        ]),
        procedures: Box::from([ProcedureSpec {
            number: kind(seed, 1),
            actions: Box::from([
                ProcedureAction::Effect { kind: kind(seed, 1), purpose: 8, target: 5 },
                ProcedureAction::Delegate { kinds: Box::from([kind(seed, 2)]) },
                ProcedureAction::Propose { number: 7 },
                ProcedureAction::Wait { state: 5 },
                ProcedureAction::Finish { result: ProcedureResult::Local { code: 9 } },
            ]),
            max_steps: 5,
            stall: Duration::from_nanos(9),
        }]),
    }
}

/// The root's journal, connector and system for one seeded run.
#[derive(Debug)]
pub struct World {
    pub seed: u8,
    pub wall: u64,
    pub domain: Domain,
    pub system: System,
    pub records: BTreeMap<RecordKey, Record>,
    pub seen: Vec<Request>,
    pub commits: u32,
    pub released: u32,
}

impl World {
    /// Start a deployment with a fresh system and store.
    #[must_use]
    pub fn new(seed: u8) -> World {
        World {
            seed,
            wall: 0,
            domain: Domain::new(config(seed), &LIMITS),
            system: System::new(),
            records: BTreeMap::new(),
            seen: Vec::new(),
            commits: 0,
            released: 0,
        }
    }

    fn env(&self) -> Env<Limits> {
        Env { now: Time::ZERO, wall: Wall::from_nanos(self.wall), limits: LIMITS }
    }

    fn collected(out: &mut Queue<Request>) -> Vec<Request> {
        let mut requests = Vec::new();
        while let Some(request) = out.pop() {
            requests.push(request);
        }
        requests
    }

    fn commit(&mut self, requests: &[Request]) {
        // A journal turn commits all records before any output can be released.
        for request in requests {
            match request {
                Request::Save { record } => {
                    self.records.insert(record.key(), record.clone());
                    self.commits += 1;
                }
                Request::Erase { key } => {
                    self.records.remove(key);
                    self.commits += 1;
                }
                Request::Verdict { .. }
                | Request::Step { .. }
                | Request::Answer { .. }
                | Request::Ready { .. }
                | Request::Section { .. }
                | Request::Workspace { .. }
                | Request::DriftResource { .. }
                | Request::Changed { .. }
                | Request::RestartDone
                | Request::Described { .. }
                | Request::EffectBusy { .. }
                | Request::EffectRefused { .. }
                | Request::Make { .. }
                | Request::Outcome { .. }
                | Request::System(..)
                | Request::Named { .. }
                | Request::Unknown { .. }
                | Request::Refused { .. }
                | Request::Adopted { .. }
                | Request::Slots { .. }
                | Request::Drift { .. }
                | Request::News { .. }
                | Request::Closed { .. } => {}
            }
        }
    }

    /// One connector step followed by its journal commit. Returned requests
    /// are still behind the release door.
    pub fn event(&mut self, event: Event) -> Vec<Request> {
        let mut out = Queue::with_capacity(connector::MAX_OUT);
        let env = self.env();
        connector::step(&mut self.domain, &env, event, &mut out);
        let requests = Self::collected(&mut out);
        self.commit(&requests);
        self.seen.extend(requests.iter().cloned());
        requests
    }

    /// One timer turn, committed before its system calls are released.
    pub fn fire(&mut self) -> Vec<Request> {
        let mut out = Queue::with_capacity(connector::MAX_OUT);
        let env = self.env();
        connector::fire(&mut self.domain, &env, &mut out);
        let requests = Self::collected(&mut out);
        self.commit(&requests);
        self.seen.extend(requests.iter().cloned());
        requests
    }

    /// Release already committed requests, answering system calls in order.
    /// A caller can restart before this method to cut at a durable edge.
    pub fn release(&mut self, requests: &[Request], faults: &mut VecDeque<Fault>) -> Vec<Request> {
        let mut pending: VecDeque<Request> = requests.iter().cloned().collect();
        let mut all = requests.to_vec();
        let mut turns = 0_u32;
        while let Some(request) = pending.pop_front() {
            turns += 1;
            assert!(turns < 100, "script has a finite cascade");
            let generated = match request {
                Request::Make { entry } => {
                    self.released += 1;
                    self.event(Event::Make { entry })
                }
                Request::System(call) => {
                    if let SystemRequest::Apply { key, attempt, .. } = &call {
                        let Some(Record::Outbox(entry)) = self.records.get(&RecordKey::Outbox(*key)) else {
                            panic!("apply was released before its outbox record committed");
                        };
                        assert_eq!(entry.phase, EffectPhase::Sent);
                        assert_eq!(entry.attempt.map(|attempt| attempt.number), Some(*attempt));
                    }
                    self.released += 1;
                    let reply = self.system.answer(call, faults.pop_front().unwrap_or(Fault::None));
                    self.event(Event::System(reply))
                }
                Request::Verdict { .. }
                | Request::Step { .. }
                | Request::Answer { .. }
                | Request::Ready { .. }
                | Request::Section { .. }
                | Request::Workspace { .. }
                | Request::DriftResource { .. }
                | Request::Changed { .. }
                | Request::RestartDone
                | Request::Described { .. }
                | Request::EffectBusy { .. }
                | Request::EffectRefused { .. }
                | Request::Outcome { .. }
                | Request::Named { .. }
                | Request::Unknown { .. }
                | Request::Refused { .. }
                | Request::Adopted { .. }
                | Request::Slots { .. }
                | Request::Drift { .. }
                | Request::News { .. }
                | Request::Save { .. }
                | Request::Erase { .. }
                | Request::Closed { .. } => Vec::new(),
            };
            pending.extend(generated.iter().cloned());
            all.extend(generated);
        }
        all
    }

    /// Rebuild from committed records, then refresh each live resource before
    /// a procedure can step. The outbox remains to be settled by the caller.
    pub fn cold_restart(&mut self) {
        let mut live = BTreeSet::new();
        for record in self.records.values() {
            match record {
                Record::Task { resources, .. } | Record::Made { resources, .. } => {
                    live.extend(resources.iter().cloned());
                }
                Record::Adoption { path, .. } | Record::Pool { path, .. } => {
                    live.insert(path.clone());
                }
                Record::Outbox(entry) => {
                    live.extend(entry.effect.resources.iter().cloned());
                }
                Record::Procedure(state) => {
                    live.insert(state.resource.clone());
                }
                Record::Proposal { .. } | Record::Subscription { .. } | Record::Result { .. } => {}
            }
        }
        self.domain = Domain::new(config(self.seed), &LIMITS);
        for record in self.records.values().cloned().collect::<Vec<_>>() {
            let restored = self.event(Event::Restore { record });
            assert!(restored.is_empty(), "restoration makes no external call");
        }
        let records = self.event(Event::Restart(RestartStep::Records));
        assert!(records.is_empty(), "record phase makes no external call");
        for resource in live {
            let calls = self.event(Event::Restart(RestartStep::FreshRead { resource }));
            self.release(&calls, &mut VecDeque::new());
        }
    }

    /// Ask the connector to settle one more outbox step after fresh reads.
    pub fn settle_restart(&mut self, faults: &mut VecDeque<Fault>) -> Vec<Request> {
        let requests = self.event(Event::Restart(RestartStep::SettleOutbox));
        self.release(&requests, faults)
    }

    /// A stable trace of decisions and calls for deterministic replay checks.
    #[must_use]
    pub fn trace(&self) -> &[Request] {
        &self.seen
    }

    /// A key valid in this deployment for the given task and purpose.
    #[must_use]
    pub fn key(&self, task: u64, purpose: u64) -> Key {
        Key {
            deployment: (u128::from(self.seed) + 1).to_be_bytes(),
            task,
            purpose,
            attempt: 0,
            completion: 0,
            position: 0,
        }
    }
}
