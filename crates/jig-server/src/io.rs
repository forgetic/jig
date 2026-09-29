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
                // An undecided request's decision may never come, and the
                // core must not wait for it past the exchange's end.
                ServerEvent::Gone { req } => comps.push(Comp::Gone { req }),
                // The core never relays, so flushes mean nothing to it.
                ServerEvent::Signal { .. } | ServerEvent::Flushed { .. } => {}
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use steploop::http1::ReqId;
    use steploop::http1::server::Config;
    use steploop::sys::{Ids, SockId};

    use super::*;

    const STOP: SignalId = SignalId(u64::MAX - 1);

    fn step(io: &mut ServerIo, now: Time, reqs: Vec<IoReq>) -> (Vec<Comp>, Vec<Action>) {
        let (mut reqs, mut comps, mut actions) = (reqs, Vec::new(), Vec::new());
        io.step(now, &mut reqs, &mut comps, &mut actions);
        (comps, actions)
    }

    fn reap(io: &mut ServerIo, events: Vec<Event>) -> Vec<Comp> {
        let (mut events, mut comps) = (events, Vec::new());
        io.reap(Time::ZERO, &mut events, &mut comps);
        comps
    }

    #[test]
    fn an_exchange_cut_at_the_grace_reaches_the_core_as_gone() {
        let mut io = ServerIo::new(Server::new(Ids::new(), Config::default()), STOP);
        let (listener, conn) = (SockId(1), SockId(2));
        step(&mut io, Time::ZERO, vec![]);
        let peer = "127.0.0.1:4000".parse().unwrap();
        let accepted = Event::Accepted {
            listener,
            new: conn,
            result: Ok(peer),
        };
        assert_eq!(reap(&mut io, vec![accepted]), []);
        step(&mut io, Time::ZERO, vec![]);
        let request = Event::Read {
            sock: conn,
            result: Ok(b"GET /x HTTP/1.1\r\n\r\n".to_vec()),
        };
        let comps = reap(&mut io, vec![request]);
        assert!(matches!(&comps[..], [Comp::Request { .. }]), "{comps:?}");

        // No response comes (a decision nobody answers), and the grace ends.
        let grace = Duration::from_secs(1);
        let (comps, _) = step(&mut io, Time::ZERO, vec![IoReq::Shutdown { grace }]);
        assert_eq!(comps, []);
        let (comps, actions) = step(&mut io, Time::ZERO.after(grace), vec![]);
        assert_eq!(comps, [Comp::Gone { req: ReqId(2) }]);
        assert!(actions.contains(&Action::Close { sock: conn }));
    }

    #[test]
    fn only_the_stop_signal_reaches_the_core() {
        let mut io = ServerIo::new(Server::new(Ids::new(), Config::default()), STOP);
        let other = SignalId(u64::MAX - 2);
        let signals = vec![
            Event::Signal { signal: other },
            Event::Signal { signal: STOP },
        ];
        assert_eq!(reap(&mut io, signals), [Comp::Stop]);
    }
}
