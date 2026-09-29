//! The recorder core's I/O step: steploop's HTTP/1 server planner (the client
//! side) and client planner (the upstream side) on one reactor.
//!
//! The composition is the recipe of design §5.5, and nothing else: the client
//! gets a disjoint id range, takes its own events out of each batch and
//! leaves the rest to the server, and the core gets the server's completions
//! before the client's, so a `Flushed` covers every relayed byte before it.
//! Pure, like the planners, and public so an embedder can run the recorder
//! from a loop of its own.

use std::sync::Arc;

use steploop::http1::client::{Client, ClientEvent};
use steploop::http1::server::{Config, Server, ServerEvent};
use steploop::run::IoStep;
use steploop::sys::{Action, Event, Ids, SignalId, SockId};
use steploop::time::Time;

use crate::relay::{Comp, IoReq};

/// [`Server`] and [`Client`] as the recorder core's [`IoStep`]. See the
/// module docs.
#[derive(Debug)]
pub struct RecorderIo {
    server: Server,
    client: Client,
    /// The signal that means [`Comp::Stop`].
    stop: SignalId,
    /// The planners' output before translation, reused across calls.
    server_out: Vec<ServerEvent>,
    client_out: Vec<ClientEvent>,
}

impl RecorderIo {
    /// Planners whose fetches trust `tls` (see `steploop::tls::client_config`).
    /// The embedder adopts the listener under [`RecorderIo::listener`];
    /// raising `stop` stops the recorder.
    pub fn new(tls: Arc<rustls::ClientConfig>, stop: SignalId) -> Self {
        let mut ids = Ids::new();
        let client = Client::new(ids.split()).with_tls(tls);
        RecorderIo {
            server: Server::new(ids, Config::default()),
            client,
            stop,
            server_out: Vec::new(),
            client_out: Vec::new(),
        }
    }

    /// The listener's id, for `Reactor::adopt_listener`.
    pub fn listener(&self) -> SockId {
        self.server.listener()
    }

    fn hand_over(&mut self, comps: &mut Vec<Comp>) {
        for event in self.server_out.drain(..) {
            match event {
                ServerEvent::Signal { id } if id == self.stop => comps.push(Comp::Stop),
                ServerEvent::Signal { .. } => {}
                event => comps.push(Comp::Server(event)),
            }
        }
        comps.extend(self.client_out.drain(..).map(Comp::Client));
    }
}

impl IoStep for RecorderIo {
    type Comp = Comp;
    type Req = IoReq;

    fn reap(&mut self, now: Time, events: &mut Vec<Event>, comps: &mut Vec<Comp>) {
        self.client.reap(now, events, &mut self.client_out);
        self.server.reap(now, events, &mut self.server_out);
        self.hand_over(comps);
    }

    fn step(
        &mut self,
        now: Time,
        reqs: &mut Vec<IoReq>,
        comps: &mut Vec<Comp>,
        actions: &mut Vec<Action>,
    ) {
        for req in reqs.drain(..) {
            match req {
                IoReq::Server(cmd) => self.server.command(now, cmd),
                IoReq::Client(cmd) => {
                    self.client.command(now, cmd, &mut self.client_out, actions);
                }
            }
        }
        self.server.plan(now, &mut self.server_out, actions);
        self.client.plan(now, &mut self.client_out, actions);
        self.hand_over(comps);
    }

    fn deadline(&self) -> Option<Time> {
        match (self.server.deadline(), self.client.deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn idle(&self) -> bool {
        self.server.idle() && self.client.idle()
    }
}
