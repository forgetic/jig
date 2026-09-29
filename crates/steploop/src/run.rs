//! The loop driver and the three traits it drives.
//!
//! The traits are statically dispatched (no `dyn`): nothing is stored behind a
//! vtable and no callback is passed into pure code. [`Host`] is the one place
//! where impure embedder code (logs, locks, closures) meets the loop. See
//! `docs/explanation/sans-io-shell.md` §4.11 and §5.3.
//!
//! [`run`] is the loop; [`Tap`] records what the pure steps saw during a run
//! and [`replay`] feeds the same inputs to fresh steps, so any real run
//! (including a failing one) can be reproduced deterministically without a
//! reactor (§5.8). [`Count`] only counts iterations, so a test can tell a
//! loop that sleeps when it has nothing to do from one that spins.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::Instant;

use crate::reactor::Reactor;
use crate::sys::Action;
use crate::sys::Event;
use crate::time::Time;

/// Business logic. Pure: no I/O, no clock, no randomness, no locks.
pub trait Core {
    /// Completions and events from the I/O step (and answers from the host).
    type Comp;
    /// Requests to the I/O step.
    type IoReq;
    /// Requests to the embedder: records, captures, decisions.
    type HostReq;

    /// Drain `comps`; append to `io` and `host`. May be called with `comps`
    /// empty (time passed): expire deadlines here.
    fn step(
        &mut self,
        now: Time,
        comps: &mut Vec<Self::Comp>,
        io: &mut Vec<Self::IoReq>,
        host: &mut Vec<Self::HostReq>,
    );

    /// The earliest time this core wants to be stepped again, if any.
    fn deadline(&self) -> Option<Time>;

    /// The core has finished (e.g. shut down) and wants the loop to end once
    /// the I/O step is idle.
    fn done(&self) -> bool;
}

/// I/O planning. Pure: it plans syscalls ([`Action`]) and consumes their
/// results ([`Event`]), but never performs them.
pub trait IoStep {
    type Comp;
    type Req;

    /// Drain reactor events; update planner state; append completions for the
    /// core.
    fn reap(&mut self, now: Time, events: &mut Vec<Event>, comps: &mut Vec<Self::Comp>);

    /// Drain core requests; plan actions for every resource that needs
    /// progress. Completions that need no syscall go straight to `comps`.
    fn step(
        &mut self,
        now: Time,
        reqs: &mut Vec<Self::Req>,
        comps: &mut Vec<Self::Comp>,
        actions: &mut Vec<Action>,
    );

    /// The earliest time this step wants to run again, if any.
    fn deadline(&self) -> Option<Time>;

    /// Nothing open and nothing in flight: safe to end the loop.
    fn idle(&self) -> bool;
}

/// The embedder. Shell code: it may lock, log or call closures. Answers it
/// pushes into `comps` are inputs to the pure side and are recorded by the tap.
pub trait Host<C: Core> {
    fn handle(&mut self, now: Time, reqs: &mut Vec<C::HostReq>, comps: &mut Vec<C::Comp>);
}

/// A host for cores that need no embedder: it discards every host request
/// and never answers. Fits a core whose `HostReq` is uninhabited (such as
/// `std::convert::Infallible`) as well as one whose requests (log lines, say)
/// may be ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoHost;

impl<C: Core> Host<C> for NoHost {
    fn handle(&mut self, _now: Time, reqs: &mut Vec<C::HostReq>, _comps: &mut Vec<C::Comp>) {
        reqs.clear();
    }
}

/// Drive `core` and `io` until the core is done, the I/O step is idle and no
/// actions remain. `reactor.poll` is the only blocking call.
///
/// Each iteration:
/// 1. polls, with a zero timeout while `perform` results are pending (and on
///    the first pass, so the steps can plan their first actions before
///    anything is awaited), otherwise until the earliest deadline;
/// 2. reads the clock once;
/// 3. reaps the events into completions;
/// 4. steps to quiescence: `core.step`, `host.handle`, `io.step`, repeated
///    until a round leaves no completions;
/// 5. performs the planned actions, whose events feed the next iteration.
///
/// `tap` sees every input and output of the pure steps; pass [`NoTap`] to
/// record nothing.
pub fn run<C, I, H, T>(
    core: &mut C,
    io: &mut I,
    host: &mut H,
    reactor: &mut Reactor,
    tap: &mut T,
) -> io::Result<()>
where
    C: Core,
    I: IoStep<Comp = C::Comp, Req = C::IoReq>,
    H: Host<C>,
    T: Observe<C::Comp, C::HostReq>,
{
    // Allocated once and reused: steps drain their inputs and append outputs.
    let mut events = Vec::new();
    let mut comps = Vec::new();
    let mut io_reqs = Vec::new();
    let mut host_reqs = Vec::new();
    let mut actions = Vec::new();
    let start = Instant::now();
    let mut now = Time::ZERO;
    let mut first = true;
    loop {
        // The deadline is measured from the previous iteration's `now`, stale
        // only by the time `perform` took.
        let timeout = if first || !events.is_empty() {
            Some(Duration::ZERO)
        } else {
            earliest(core.deadline(), io.deadline()).map(|at| now.until(at))
        };
        first = false;
        reactor.poll(&mut events, timeout)?;
        now = Time::ZERO.after(start.elapsed());
        tap.polled(now, &events);
        io.reap(now, &mut events, &mut comps);
        loop {
            let mark = host_reqs.len();
            core.step(now, &mut comps, &mut io_reqs, &mut host_reqs);
            tap.host_reqs(host_reqs.get(mark..).unwrap_or_default());
            let mark = comps.len();
            host.handle(now, &mut host_reqs, &mut comps);
            tap.host_answers(comps.get(mark..).unwrap_or_default());
            io.step(now, &mut io_reqs, &mut comps, &mut actions);
            if comps.is_empty() {
                break;
            }
        }
        tap.actions(&actions);
        if core.done() && io.idle() && actions.is_empty() {
            return Ok(());
        }
        reactor.perform(&mut actions, &mut events);
    }
}

fn earliest(a: Option<Time>, b: Option<Time>) -> Option<Time> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// What [`run`] shows its tap. Implemented by [`Tap`], which records, and by
/// [`NoTap`], which doesn't; the split keeps `Clone` bounds off `run` and the
/// step traits.
pub trait Observe<Comp, HostReq> {
    /// A new iteration begins: its `now` and every event `reap` will see.
    fn polled(&mut self, now: Time, events: &[Event]);
    /// The host requests the core made in one round of the quiescence loop.
    fn host_reqs(&mut self, reqs: &[HostReq]);
    /// The host's answers in the same round (often none).
    fn host_answers(&mut self, answers: &[Comp]);
    /// The iteration's actions, just before they are performed.
    fn actions(&mut self, actions: &[Action]);
}

/// Records nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoTap;

impl<Comp, HostReq> Observe<Comp, HostReq> for NoTap {
    fn polled(&mut self, _now: Time, _events: &[Event]) {}
    fn host_reqs(&mut self, _reqs: &[HostReq]) {}
    fn host_answers(&mut self, _answers: &[Comp]) {}
    fn actions(&mut self, _actions: &[Action]) {}
}

/// Counts loop iterations and records nothing else. A loop with nothing to
/// do blocks in `poll`, so over an idle stretch the count should barely
/// move; one that spins climbs by the thousand every millisecond, which no
/// other check notices. Clones share the count, so a test keeps one and
/// reads it from another thread while `run` holds the other.
#[derive(Clone, Debug, Default)]
pub struct Count(Arc<AtomicU64>);

impl Count {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many times `run` has returned from `poll` so far.
    pub fn iterations(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

impl<Comp, HostReq> Observe<Comp, HostReq> for Count {
    fn polled(&mut self, _now: Time, _events: &[Event]) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn host_reqs(&mut self, _reqs: &[HostReq]) {}
    fn host_answers(&mut self, _answers: &[Comp]) {}
    fn actions(&mut self, _actions: &[Action]) {}
}

/// A recording of a run: per iteration, the pure steps' inputs (`now`, the
/// reactor events, the host's answers) and their outputs (host requests and
/// actions). The inputs are everything the steps receive, so [`replay`] can
/// reproduce the outputs; recording the outputs lets a test check that it
/// did. It grows with the run, so it suits tests and bounded captures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tap<Comp, HostReq> {
    pub iterations: Vec<Iteration<Comp, HostReq>>,
}

/// One loop iteration, as recorded by [`Tap`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Iteration<Comp, HostReq> {
    pub now: Time,
    /// Everything `reap` was given.
    pub events: Vec<Event>,
    /// The host's answers, one entry per round of the quiescence loop.
    pub answers: Vec<Vec<Comp>>,
    pub outputs: Outputs<HostReq>,
}

/// What the pure steps produced in one iteration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outputs<HostReq> {
    /// Host requests, in order, across all rounds.
    pub host_reqs: Vec<HostReq>,
    /// The actions handed to `perform`.
    pub actions: Vec<Action>,
}

impl<HostReq> Default for Outputs<HostReq> {
    fn default() -> Self {
        Outputs {
            host_reqs: Vec::new(),
            actions: Vec::new(),
        }
    }
}

impl<Comp, HostReq> Default for Tap<Comp, HostReq> {
    fn default() -> Self {
        Tap {
            iterations: Vec::new(),
        }
    }
}

impl<Comp, HostReq> Tap<Comp, HostReq> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<Comp, HostReq: Clone> Tap<Comp, HostReq> {
    /// The recorded outputs, to compare with [`replay`]'s.
    pub fn outputs(&self) -> Vec<Outputs<HostReq>> {
        self.iterations
            .iter()
            .map(|it| it.outputs.clone())
            .collect()
    }
}

impl<Comp: Clone, HostReq: Clone> Observe<Comp, HostReq> for Tap<Comp, HostReq> {
    fn polled(&mut self, now: Time, events: &[Event]) {
        self.iterations.push(Iteration {
            now,
            events: events.to_vec(),
            answers: Vec::new(),
            outputs: Outputs::default(),
        });
    }

    fn host_reqs(&mut self, reqs: &[HostReq]) {
        if let Some(it) = self.iterations.last_mut() {
            it.outputs.host_reqs.extend_from_slice(reqs);
        }
    }

    fn host_answers(&mut self, answers: &[Comp]) {
        if let Some(it) = self.iterations.last_mut() {
            it.answers.push(answers.to_vec());
        }
    }

    fn actions(&mut self, actions: &[Action]) {
        if let Some(it) = self.iterations.last_mut() {
            it.outputs.actions = actions.to_vec();
        }
    }
}

/// Feed fresh steps the inputs recorded in `tap`, iteration by iteration and
/// round by round, with no reactor and no host, and return their outputs.
/// For deterministic steps these equal `tap.outputs()`. Plain-TCP runs replay
/// byte for byte; TLS runs do not, since rustls draws its own randomness
/// (§4.5).
///
/// `core` and `io` must start in the state the recorded run's did (built by
/// the same constructors, with listeners and signals named alike).
pub fn replay<C, I>(
    core: &mut C,
    io: &mut I,
    tap: &Tap<C::Comp, C::HostReq>,
) -> Vec<Outputs<C::HostReq>>
where
    C: Core,
    I: IoStep<Comp = C::Comp, Req = C::IoReq>,
    C::Comp: Clone,
{
    let mut events = Vec::new();
    let mut comps = Vec::new();
    let mut io_reqs = Vec::new();
    let mut host_reqs = Vec::new();
    let mut actions = Vec::new();
    let mut outputs = Vec::with_capacity(tap.iterations.len());
    for it in &tap.iterations {
        events.clear();
        events.extend_from_slice(&it.events);
        io.reap(it.now, &mut events, &mut comps);
        let mut out = Outputs::default();
        let mut answers = it.answers.iter();
        loop {
            core.step(it.now, &mut comps, &mut io_reqs, &mut host_reqs);
            out.host_reqs.append(&mut host_reqs);
            comps.extend(answers.next().into_iter().flatten().cloned());
            io.step(it.now, &mut io_reqs, &mut comps, &mut actions);
            if comps.is_empty() {
                break;
            }
        }
        out.actions.append(&mut actions);
        outputs.push(out);
    }
    outputs
}
