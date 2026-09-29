//! The provider core's I/O step: steploop's HTTP/1 server planner, spoken to
//! in the core's vocabulary.
//!
//! The planner is generic (no jig types), and the core should not have to
//! know about raw relays or flushes it never uses, so this adapter translates
//! and does nothing else. It is pure, like the planner it wraps, and public so
//! that an embedder can drive the provider from a loop of its own. See
//! `docs/explanation/sans-io-shell.md` §5.6.

use steploop::http1::server::{Server, ServerCmd, ServerEvent};
use steploop::run::IoStep;
use steploop::sys::{Action, Event, SignalId};
use steploop::time::Time;

use crate::provider::{Comp, IoReq};

/// [`Server`] as the provider's [`IoStep`]. See the module docs.
#[derive(Debug)]
pub struct ServerIo {
    server: Server,
    /// The signal that means [`Comp::Stop`]; others are not the core's.
    stop: SignalId,
    /// The planner's output before translation, reused across calls.
    events: Vec<ServerEvent>,
}

impl ServerIo {
    /// Wrap `server`, whose listener the embedder adopts under
    /// [`Server::listener`]. Raising `stop` shuts the provider down.
    pub fn new(server: Server, stop: SignalId) -> Self {
        ServerIo {
            server,
            stop,
            events: Vec::new(),
        }
    }

    fn translate(&mut self, comps: &mut Vec<Comp>) {
        for event in self.events.drain(..) {
            match event {
                ServerEvent::Request { req, request } => comps.push(Comp::Request { req, request }),
                ServerEvent::Signal { id } if id == self.stop => comps.push(Comp::Stop),
                // The core never relays, so flushes mean nothing to it, and it
                // keeps no state an early end could free: an undecided
                // request is let go when its decision arrives, and the
                // server ignores the response that follows.
                ServerEvent::Signal { .. }
                | ServerEvent::Gone { .. }
                | ServerEvent::Flushed { .. } => {}
            }
        }
    }
}

impl IoStep for ServerIo {
    type Comp = Comp;
    type Req = IoReq;

    fn reap(&mut self, now: Time, events: &mut Vec<Event>, comps: &mut Vec<Comp>) {
        self.server.reap(now, events, &mut self.events);
        self.translate(comps);
    }

    fn step(
        &mut self,
        now: Time,
        reqs: &mut Vec<IoReq>,
        comps: &mut Vec<Comp>,
        actions: &mut Vec<Action>,
    ) {
        for req in reqs.drain(..) {
            let cmd = match req {
                IoReq::Respond { req, response } => ServerCmd::Respond { req, response },
                IoReq::Shutdown { grace } => ServerCmd::Shutdown { grace },
            };
            self.server.command(now, cmd);
        }
        self.server.plan(now, &mut self.events, actions);
        self.translate(comps);
    }

    fn deadline(&self) -> Option<Time> {
        self.server.deadline()
    }

    fn idle(&self) -> bool {
        self.server.idle()
    }
}
