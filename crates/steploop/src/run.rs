//! The loop driver and the three traits it drives.
//!
//! The traits are statically dispatched (no `dyn`): nothing is stored behind a
//! vtable and no callback is passed into pure code. [`Host`] is the one place
//! where impure embedder code (logs, locks, closures) meets the loop. See
//! `docs/explanation/sans-io-shell.md` §4.11 and §5.3.

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
