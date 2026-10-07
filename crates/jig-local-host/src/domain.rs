//! Slot admission, Smith composition and acknowledgement retention
//! (domain/hosts.md, sections 2 and 5). A task and attempt fence every later
//! input. An answered run keeps its slot until the core acknowledges it.

use alloc::boxed::Box;
use skein_lib::{Env, List, Map, Queue, ReplyTo, Time, Token};
use smith_domain as smith;

use crate::boundary::{Assignment, Completion, Event, MessageRefusal, Refusal, Request};
use crate::charter;
use crate::limits::{self, Limits};

/// One turn retained until its durable acknowledgement.
#[derive(Debug)]
pub(crate) struct Retained {
    pub(crate) bytes: u64,
    pub(crate) turn: smith::Turn,
}

#[derive(Debug)]
pub(crate) struct Relay {
    pub(crate) name: smith::run::RelayName,
}

/// One occupied engine slot, including an answer awaiting acknowledgement.
#[derive(Debug)]
pub(crate) struct Hosted {
    pub(crate) task: u64,
    pub(crate) attempt: u32,
    pub(crate) id: Token,
    pub(crate) smith_run: Option<Token>,
    pub(crate) smith: Option<smith::Domain>,
    pub(crate) completions: Map<Token, bool>,
    pub(crate) relays: Map<Token, Relay>,
    pub(crate) turns: Map<u32, Retained>,
    pub(crate) turn_bytes: u64,
    pub(crate) answer: Option<smith::run::Answer>,
    pub(crate) stopped: bool,
    pub(crate) cancel_deadline: Option<Time>,
}

/// Configured slots and endpoint names, with one Smith domain per live run.
#[derive(Debug)]
pub struct Host {
    slots: Map<u32, Hosted>,
    endpoints: Box<[smith::run::charter::Endpoint]>,
    lower: Queue<smith::Request>,
    capacity: u32,
    next: u64,
    seed: u64,
}

impl Host {
    /// Build a host whose Smith limits and turn window have a checked bound.
    #[must_use]
    pub fn new(limits: &Limits, endpoints: Box<[smith::run::charter::Endpoint]>, seed: u64) -> Host {
        assert!(limits::worst_case(limits).is_some(), "local host limits have a worst case");
        assert!(
            endpoints.len() <= usize::try_from(limits.smith.endpoints).expect("u32 endpoint cap"),
            "configured endpoints fit Smith's count limit"
        );
        for (index, endpoint) in endpoints.iter().enumerate() {
            for other in endpoints.get(index.saturating_add(1)..).unwrap_or_default() {
                assert!(endpoint != other, "configured endpoint names are unique");
            }
        }
        Host {
            slots: Map::with_capacity(limits.slots),
            endpoints,
            lower: Queue::with_capacity(smith::max_out(&limits.smith)),
            capacity: limits.slots,
            next: 1,
            seed,
        }
    }

    /// Occupied slots, answered runs included until their acknowledgement.
    #[must_use]
    pub fn hosted(&self) -> u32 {
        self.slots.len()
    }

    /// Whether this exact task and attempt still occupy a slot.
    #[must_use]
    pub fn is_hosting(&self, task: u64, attempt: u32) -> bool {
        find(self, task, attempt).is_some()
    }

    /// The concrete turn kept for retry until the core acknowledges it.
    #[must_use]
    pub fn retained_turn(&self, task: u64, attempt: u32, number: u32) -> Option<&smith::Turn> {
        let slot = find(self, task, attempt)?;
        let run = self.slots.get(&slot)?;
        Some(&run.turns.get(&number)?.turn)
    }

    /// Drain one best-effort Smith fact for a live assignment.
    pub fn pop_fact(&mut self, task: u64, attempt: u32) -> Option<smith::Fact> {
        let slot = find(self, task, attempt)?;
        let run = self.slots.get_mut(&slot)?;
        match &mut run.smith {
            Some(domain) => domain.pop_fact(),
            None => None,
        }
    }

    /// Drain one bounded Smith content observation for a live assignment.
    pub fn pop_content(&mut self, task: u64, attempt: u32) -> Option<smith::Content> {
        let slot = find(self, task, attempt)?;
        let run = self.slots.get_mut(&slot)?;
        match &mut run.smith {
            Some(domain) => domain.pop_content(),
            None => None,
        }
    }
}

/// The largest number of outputs from one host entry point.
#[must_use]
pub const fn max_out(limits: &Limits) -> u32 {
    smith::max_out(&limits.smith)
        .saturating_add(limits.smith.run.conversations)
        .saturating_add(limits.smith.run.calls)
        .saturating_add(1)
}

fn find(host: &Host, task: u64, attempt: u32) -> Option<u32> {
    for (slot, run) in &host.slots {
        if run.task == task && run.attempt == attempt {
            return Some(*slot);
        }
    }
    None
}

fn child_env(env: &Env<Limits>) -> Env<smith::Limits> {
    Env { now: env.now, wall: env.wall, limits: env.limits.smith }
}

fn smith_step(host: &mut Host, env: &Env<Limits>, slot: u32, event: smith::Event, out: &mut Queue<Request>) {
    let run = host.slots.get_mut(&slot).expect("a Smith event has a hosted slot");
    let domain = run.smith.as_mut().expect("a live run has its Smith domain");
    smith::step(domain, &child_env(env), event, &mut host.lower);
    route(host, env, slot, out);
}

fn assign(host: &mut Host, env: &Env<Limits>, slot: u32, assignment: Box<Assignment>, out: &mut Queue<Request>) {
    let task = assignment.task;
    let attempt = assignment.attempt;
    if slot >= env.limits.slots || attempt == 0 {
        out.push(Request::Refused { task, attempt, why: Refusal::Invalid });
        return;
    }
    if host.slots.contains_key(&slot) {
        out.push(Request::Refused { task, attempt, why: Refusal::Busy });
        return;
    }
    for (_, run) in &host.slots {
        if run.task == task {
            out.push(Request::Refused { task, attempt, why: Refusal::Duplicate });
            return;
        }
    }
    let Some(id) = host.next.checked_add(1) else {
        out.push(Request::Refused { task, attempt, why: Refusal::Invalid });
        return;
    };
    let Some(charter::Start { charter, transcript, answered, grants }) = charter::start(*assignment) else {
        out.push(Request::Refused { task, attempt, why: Refusal::Invalid });
        return;
    };
    let host_run = Token::new(host.next);
    host.next = id;
    let smith = smith::Domain::new(
        &env.limits.smith,
        smith::Config { endpoints: host.endpoints.clone() },
        host.seed ^ host_run.raw(),
    );
    let hosted = Hosted {
        task,
        attempt,
        id: host_run,
        smith_run: None,
        smith: Some(smith),
        completions: Map::with_capacity(env.limits.smith.run.conversations),
        relays: Map::with_capacity(env.limits.smith.run.calls),
        turns: Map::with_capacity(env.limits.window.turns),
        turn_bytes: 0,
        answer: None,
        stopped: false,
        cancel_deadline: None,
    };
    let replaced = host.slots.insert(slot, hosted).expect("a free configured slot fits");
    assert!(replaced.is_none(), "a free slot has no run");
    smith_step(
        host,
        env,
        slot,
        smith::Event::Start {
            reply_to: ReplyTo::new(host_run),
            host_run,
            activation: u64::from(attempt),
            window: env.limits.window,
            charter,
            workspace: None,
            transcript,
            answered,
            grants,
        },
        out,
    );
}

#[expect(clippy::too_many_arguments, reason = "the boundary supplies one named message with two owned text parts")]
fn message(
    host: &mut Host,
    env: &Env<Limits>,
    task: u64,
    attempt: u32,
    name: u64,
    label: Box<[u8]>,
    words: Box<[u8]>,
    out: &mut Queue<Request>,
) {
    let Some(slot) = find(host, task, attempt) else {
        out.push(Request::Bounced { task, attempt, name, why: MessageRefusal::NoRun });
        return;
    };
    let run = host.slots.get(&slot).expect("found slot is occupied");
    if run.answer.is_some() || run.stopped {
        out.push(Request::Bounced { task, attempt, name, why: MessageRefusal::NoRun });
        return;
    }
    let Some(smith_run) = run.smith_run else {
        out.push(Request::Bounced { task, attempt, name, why: MessageRefusal::NoRun });
        return;
    };
    if label.is_empty() {
        out.push(Request::Bounced { task, attempt, name, why: MessageRefusal::Invalid });
        return;
    }
    let prefix = label.len().checked_add(2);
    let len = match prefix {
        Some(prefix) => prefix.checked_add(words.len()),
        None => None,
    };
    let len = match len {
        Some(len) if len <= usize::try_from(env.limits.smith.run.message_bytes).expect("u32 byte cap") => len,
        Some(_) | None => {
            out.push(Request::Bounced { task, attempt, name, why: MessageRefusal::TooLarge });
            return;
        }
    };
    let mut text = List::with_capacity(u32::try_from(len).expect("fits Smith byte cap"));
    for byte in label {
        text.push(byte).expect("precounted label");
    }
    text.push(b':').expect("precounted separator");
    text.push(b' ').expect("precounted separator");
    for byte in words {
        text.push(byte).expect("precounted words");
    }
    smith_step(
        host,
        env,
        slot,
        smith::Event::Message { run: smith_run, name: Token::new(name), text: text.into_boxed() },
        out,
    );
}

/// Handle one root event and emit the resulting bounded requests.
pub fn step(host: &mut Host, env: &Env<Limits>, event: Event, out: &mut Queue<Request>) {
    match event {
        Event::Assign { slot, assignment } => assign(host, env, slot, assignment, out),
        Event::Message { task, attempt, name, label, words } => {
            message(host, env, task, attempt, name, label, words, out);
        }
        Event::Answered { task, attempt, relay, reply } => {
            if let Some(slot) = find(host, task, attempt) {
                let run = host.slots.get_mut(&slot).expect("found slot is occupied");
                if run.relays.remove(&relay.owner).is_some() && run.smith.is_some() && run.answer.is_none() {
                    smith_step(host, env, slot, smith::Event::HostReturned { relay, reply }, out);
                }
            }
        }
        Event::AcknowledgeTurn { task, attempt, turn } => acknowledge_turn(host, env, task, attempt, turn, out),
        Event::AcknowledgeAnswer { task, attempt } => {
            if let Some(slot) = find(host, task, attempt) {
                let run = host.slots.get(&slot).expect("found slot is occupied");
                if run.answer.is_some() || run.stopped {
                    host.slots.remove(&slot);
                }
            }
        }
        Event::Grant { task, attempt, grant } => {
            if let Some(slot) = find(host, task, attempt) {
                let run = host.slots.get(&slot).expect("found slot is occupied");
                if run.smith.is_none() || run.answer.is_some() {
                    return;
                }
                let event = smith::Event::Grant {
                    grant: smith::Grant {
                        name: smith::GrantName { account: grant.account, generation: grant.generation },
                        valid: grant.valid,
                    },
                };
                smith_step(host, env, slot, event, out);
            }
        }
        Event::Cancel { task, attempt } => cancel(host, env, task, attempt, out),
        Event::Completion { task, attempt, terminal } => {
            if let Some(slot) = find(host, task, attempt) {
                let owner = match &terminal {
                    Completion::Completed { owner, .. }
                    | Completion::Failed { owner, .. }
                    | Completion::Cancelled { owner } => *owner,
                };
                let run = host.slots.get_mut(&slot).expect("found slot is occupied");
                if run.completions.remove(&owner).is_none() || run.smith.is_none() || run.answer.is_some() {
                    return;
                }
                let event = match terminal {
                    Completion::Completed { owner, completion } => smith::Event::Completed { owner, completion },
                    Completion::Failed { owner, failure, evidence, detail } => {
                        smith::Event::Failed { owner, failure, evidence, detail }
                    }
                    Completion::Cancelled { owner } => smith::Event::Cancelled { owner },
                };
                smith_step(host, env, slot, event, out);
            }
        }
    }
}

fn acknowledge_turn(host: &mut Host, env: &Env<Limits>, task: u64, attempt: u32, turn: u32, out: &mut Queue<Request>) {
    let Some(slot) = find(host, task, attempt) else { return };
    let run = host.slots.get_mut(&slot).expect("found slot is occupied");
    let mut numbers = List::with_capacity(env.limits.window.turns);
    for (number, _) in &run.turns {
        if *number <= turn {
            numbers.push(*number).expect("no more retained turns than window");
        }
    }
    for number in &numbers {
        let retained = run.turns.remove(number).expect("number was retained");
        run.turn_bytes = run.turn_bytes.checked_sub(retained.bytes).expect("retained bytes were counted");
    }
    let smith_run = run.smith_run;
    match smith_run {
        Some(smith_run) if run.answer.is_none() => {
            smith_step(host, env, slot, smith::Event::Acknowledge { run: smith_run, turn }, out);
        }
        Some(_) | None => {}
    }
}

fn cancel(host: &mut Host, env: &Env<Limits>, task: u64, attempt: u32, out: &mut Queue<Request>) {
    let Some(slot) = find(host, task, attempt) else { return };
    let run = host.slots.get_mut(&slot).expect("found slot is occupied");
    if run.answer.is_some() || run.stopped || run.cancel_deadline.is_some() {
        return;
    }
    run.cancel_deadline = Some(env.now.saturating_add(env.limits.cancel_grace));
    let smith_run = run.smith_run;
    if let Some(smith_run) = smith_run {
        smith_step(host, env, slot, smith::Event::Cancel { run: smith_run }, out);
    }
}

fn copy_answer(answer: &smith::run::Answer) -> smith::run::Answer {
    match answer {
        smith::run::Answer::Parked { spent, turns } => smith::run::Answer::Parked { spent: *spent, turns: *turns },
        smith::run::Answer::Refused(reason) => smith::run::Answer::Refused(*reason),
        smith::run::Answer::Failed { failure, spent, turns } => {
            smith::run::Answer::Failed { failure: *failure, spent: *spent, turns: *turns }
        }
        smith::run::Answer::Accepted { outcome, spent, turns } => {
            let outcome = match outcome {
                smith::run::outcome::Declared::Report(value) => smith::run::outcome::Declared::Report(value.clone()),
                smith::run::outcome::Declared::Verdict(value) => smith::run::outcome::Declared::Verdict(value.clone()),
                smith::run::outcome::Declared::Failure(value) => smith::run::outcome::Declared::Failure(value.clone()),
                smith::run::outcome::Declared::Change(value) => smith::run::outcome::Declared::Change(value.clone()),
            };
            smith::run::Answer::Accepted { outcome, spent: *spent, turns: *turns }
        }
    }
}

#[expect(clippy::manual_map, reason = "the step subset uses total matches in place of closure-based Option::map")]
#[expect(clippy::too_many_lines, reason = "the Smith request vocabulary is handled together as one total match")]
fn route(host: &mut Host, env: &Env<Limits>, slot: u32, out: &mut Queue<Request>) {
    for _ in 0..smith::max_out(&env.limits.smith) {
        let Some(request) = host.lower.pop() else { break };
        let run = host.slots.get(&slot).expect("Smith output has a hosted slot");
        let task = run.task;
        let attempt = run.attempt;
        match request {
            smith::Request::Admitted { host_run, run } => {
                let hosted = host.slots.get_mut(&slot).expect("hosted slot");
                assert_eq!(hosted.id, host_run, "admission belongs to this slot");
                hosted.smith_run = Some(run);
                out.push(Request::Admitted { task, attempt });
            }
            smith::Request::Turn { host_run, number, position, read, spent, turn } => {
                let hosted = host.slots.get_mut(&slot).expect("hosted slot");
                assert_eq!(hosted.id, host_run, "turn belongs to this slot");
                let bytes = smith::max_turn_bytes(&env.limits.smith).expect("one bounded Smith turn");
                let new_bytes = hosted.turn_bytes.checked_add(bytes).expect("bounded retained turn bytes");
                assert!(new_bytes <= env.limits.window.bytes, "Smith obeys its turn window");
                let prior = hosted
                    .turns
                    .insert(number, Retained { bytes, turn: turn.clone() })
                    .expect("Smith obeys its turn window count");
                assert!(prior.is_none(), "turn numbers are unique within an activation");
                hosted.turn_bytes = new_bytes;
                out.push(Request::Turn {
                    task,
                    attempt,
                    number,
                    position,
                    read: match read {
                        Some(name) => Some(name.raw()),
                        None => None,
                    },
                    spent: spent.units,
                    turn,
                });
            }
            smith::Request::HostCall { host_run, relay, name, tool, effect, input, deadline } => {
                assert_eq!(run.id, host_run, "call belongs to this slot");
                let hosted = host.slots.get_mut(&slot).expect("hosted slot");
                assert!(
                    hosted
                        .relays
                        .insert(relay.owner, Relay { name: relay })
                        .expect("Smith's call count fits")
                        .is_none(),
                    "relay owners are unique while in flight"
                );
                out.push(Request::Call { task, attempt, relay, name, tool, effect, input, deadline });
            }
            smith::Request::WithdrawHost { relay } => {
                let hosted = host.slots.get_mut(&slot).expect("hosted slot");
                hosted.relays.remove(&relay.owner);
                out.push(Request::WithdrawCall { task, attempt, relay });
            }
            smith::Request::Waiting { host_run, read } => {
                assert_eq!(run.id, host_run, "wait belongs to this slot");
                out.push(Request::Waiting {
                    task,
                    attempt,
                    read: match read {
                        Some(name) => Some(name.raw()),
                        None => None,
                    },
                });
            }
            smith::Request::Answer { to, answer } => {
                let hosted = host.slots.get_mut(&slot).expect("hosted slot");
                assert_eq!(hosted.id, to.into_token(), "answer belongs to this slot");
                assert!(hosted.answer.is_none(), "one Smith answer per assignment");
                hosted.answer = Some(copy_answer(&answer));
                out.push(Request::Answer { task, attempt, answer });
            }
            request @ smith::Request::Complete { owner, .. } => {
                let hosted = host.slots.get_mut(&slot).expect("hosted slot");
                assert!(
                    hosted.completions.insert(owner, false).expect("Smith's completion count fits").is_none(),
                    "completion owners are unique while in flight"
                );
                out.push(Request::Protocol { task, attempt, request });
            }
            request @ smith::Request::Cancel { owner } => {
                let hosted = host.slots.get_mut(&slot).expect("hosted slot");
                if let Some(cancelled) = hosted.completions.get_mut(&owner) {
                    *cancelled = true;
                }
                out.push(Request::Protocol { task, attempt, request });
            }
            request @ (smith::Request::Rejected { .. } | smith::Request::Exhausted { .. }) => {
                out.push(Request::Protocol { task, attempt, request });
            }
            smith::Request::Checking { .. }
            | smith::Request::ChecksEnded { .. }
            | smith::Request::Deliver { .. }
            | smith::Request::Io { .. }
            | smith::Request::CancelIo { .. }
            | smith::Request::Read { .. }
            | smith::Request::Probe { .. }
            | smith::Request::Check { .. }
            | smith::Request::Abort { .. } => {
                unreachable!("a local charter grants no workspace tools or delivery");
            }
        }
    }
}

/// The earlier of a held deadline and a new candidate.
fn earlier(current: Option<Time>, candidate: Option<Time>) -> Option<Time> {
    match candidate {
        Some(candidate) => match current {
            Some(current) => Some(current.min(candidate)),
            None => Some(candidate),
        },
        None => current,
    }
}

/// Earliest Smith alarm or cancellation grace among occupied slots.
#[must_use]
pub fn next_deadline(host: &Host) -> Option<Time> {
    let mut earliest: Option<Time> = None;
    for (_, run) in &host.slots {
        if run.answer.is_none() && !run.stopped {
            let smith = match &run.smith {
                Some(domain) => domain.next_deadline(),
                None => None,
            };
            earliest = earlier(earliest, smith);
            earliest = earlier(earliest, run.cancel_deadline);
        }
    }
    earliest
}

/// Fire one due Smith alarm, then a due grace if Smith still has not answered.
pub fn fire(host: &mut Host, env: &Env<Limits>, out: &mut Queue<Request>) {
    for slot in 0..host.capacity {
        let run = match host.slots.get(&slot) {
            Some(run) if run.answer.is_none() && !run.stopped => run,
            Some(_) | None => continue,
        };
        if let Some(domain) = &run.smith
            && domain.is_due(env.now)
        {
            let domain = host.slots.get_mut(&slot).expect("occupied slot").smith.as_mut().expect("live Smith domain");
            smith::fire(domain, &child_env(env), &mut host.lower);
            route(host, env, slot, out);
            return;
        }
        if let Some(deadline) = run.cancel_deadline
            && deadline <= env.now
        {
            let run = host.slots.get_mut(&slot).expect("occupied slot");
            for (owner, cancelled) in &run.completions {
                if !cancelled {
                    out.push(Request::Protocol {
                        task: run.task,
                        attempt: run.attempt,
                        request: smith::Request::Cancel { owner: *owner },
                    });
                }
            }
            for (_, relay) in &run.relays {
                out.push(Request::WithdrawCall { task: run.task, attempt: run.attempt, relay: relay.name });
            }
            run.smith = None;
            run.stopped = true;
            out.push(Request::Stopped { task: run.task, attempt: run.attempt });
            return;
        }
    }
}

/// Advance one Smith deferred handoff on the first ready slot.
pub fn resume(host: &mut Host, env: &Env<Limits>, out: &mut Queue<Request>) {
    for slot in 0..host.capacity {
        let ready = match host.slots.get(&slot) {
            Some(run) if run.answer.is_none() && !run.stopped => match &run.smith {
                Some(domain) => domain.is_ready(),
                None => false,
            },
            Some(_) | None => false,
        };
        if ready {
            let domain = host.slots.get_mut(&slot).expect("occupied slot").smith.as_mut().expect("live Smith domain");
            smith::resume(domain, &child_env(env), &mut host.lower);
            route(host, env, slot, out);
            return;
        }
    }
}

/// Complete every Smith domain's end-of-iteration reclamation.
pub fn reclaim(host: &mut Host) {
    for slot in 0..host.capacity {
        if let Some(run) = host.slots.get_mut(&slot)
            && let Some(domain) = &mut run.smith
        {
            domain.reclaim();
        }
    }
}
