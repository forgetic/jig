//! The recorder core: client requests in; upstream fetches, relayed bytes and
//! captures out. Pure, like jig-server's provider core: no sockets, clock or
//! locks, so every behaviour is tested by feeding it completions (design
//! `docs/explanation/sans-io-shell.md` §5.7).
//!
//! It speaks the planners' own vocabulary, wrapped: the HTTP/1 server's
//! events and commands for the client side, and the HTTP/1 client's for the
//! upstream side. [`crate::io::RecorderIo`] composes the two planners.
//!
//! Per exchange, a routable request becomes a fetch, and the upstream's
//! response is relayed as raw bytes, head and body exactly as they arrive, so
//! SSE framing and timing reach the client untouched. Credit keeps the relay
//! from buffering: the client planner reads nothing more from the upstream
//! until the core acks the last `Body`, and the core acks it on the next
//! `Flushed`, once the client socket has taken it. The I/O step hands over
//! the server's events before the client's, which is what makes a `Flushed`
//! cover every byte relayed before it (§5.5).
//!
//! A fetch is named after the exchange it serves: request ids are never
//! reused, so neither are fetch ids, and no second map is needed.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use steploop::http1::client::{ClientCmd, ClientEvent, Target};
use steploop::http1::server::{ServerCmd, ServerEvent};
use steploop::http1::{Body, FetchId, ReqId, Request, Response};
use steploop::run::Core;
use steploop::time::Time;

use crate::proxy::{ClientRequest, UpstreamResponse, build_upstream_request_head};
use crate::redact::Header;
use crate::route::Route;

/// One captured routable exchange: the client request, the upstream response,
/// and the route it was forwarded on.
pub type Exchange = (ClientRequest, UpstreamResponse, Route);

/// How long relays in flight may take to finish once the recorder stops.
pub const GRACE: Duration = Duration::from_secs(1);

/// How many exchanges a recorder captures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// The first routable exchange to end is the recording: captured if it
    /// completed, and either way the recorder then shuts down. That is what
    /// `jig record` wants, and a failure ends it at once instead of leaving
    /// it waiting.
    #[default]
    Once,
    /// Capture every routable exchange until stopped, for clients that make
    /// several (the capture examples).
    Pump,
}

/// Where fetches go instead of the route's `host:443` (a test hook). TLS, if
/// on, still names the server by the route's host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpstreamOverride {
    pub addr: SocketAddr,
    pub tls: bool,
}

/// Inputs to the core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Comp {
    /// From the server planner. Signals arrive as [`Comp::Stop`] instead.
    Server(ServerEvent),
    /// From the client planner.
    Client(ClientEvent),
    /// The embedder wants the recorder to stop.
    Stop,
}

/// Requests to the I/O step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IoReq {
    Server(ServerCmd),
    Client(ClientCmd),
}

/// Requests to the embedder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostReq {
    Captured(Exchange),
    /// A line for the operator.
    Log(String),
}

/// The pure recorder core. See the module docs.
#[derive(Clone, Debug)]
pub struct RecorderCore {
    mode: Mode,
    upstream_host: Option<String>,
    upstream: Option<UpstreamOverride>,
    relays: BTreeMap<ReqId, Relay>,
    captured: usize,
    /// `Shutdown` has been issued.
    stopping: bool,
}

/// A routable exchange in flight.
#[derive(Clone, Debug)]
struct Relay {
    request: ClientRequest,
    route: Route,
    stage: Stage,
}

#[derive(Clone, Debug)]
enum Stage {
    /// The fetch is out; the upstream's head hasn't come.
    Fetching,
    /// Relaying the body, and capturing it in `response`. `owed`: a relayed
    /// `Body` awaits the `Flushed` that acks it.
    Streaming {
        response: UpstreamResponse,
        owed: bool,
    },
}

impl RecorderCore {
    /// A core forwarding to each route's upstream, or to `upstream_host` in
    /// its place, or to `upstream`'s address.
    pub fn new(
        mode: Mode,
        upstream_host: Option<String>,
        upstream: Option<UpstreamOverride>,
    ) -> Self {
        RecorderCore {
            mode,
            upstream_host,
            upstream,
            relays: BTreeMap::new(),
            captured: 0,
            stopping: false,
        }
    }

    fn on_request(&mut self, req: ReqId, request: Request, io: &mut Vec<IoReq>) {
        let request = ClientRequest {
            method: request.method,
            target: request.target,
            headers: headers(request.headers),
            body: request.body,
        };
        let route = Route::resolve(request.path()).map(|route| match &self.upstream_host {
            Some(host) => route.with_upstream_host(host.clone()),
            None => route,
        });
        // Not a dialect route: a connectivity preflight such as Claude Code's
        // `HEAD /`. A bare 204 lets the client go on to its real request.
        let Some(route) = route else {
            let response = Response {
                status: 204,
                reason: "No Content".to_owned(),
                headers: vec![
                    ("Content-Length".to_owned(), "0".to_owned()),
                    ("Connection".to_owned(), "close".to_owned()),
                ],
                body: Body::Empty,
            };
            return io.push(IoReq::Server(ServerCmd::Respond { req, response }));
        };
        let (tls, addr) = match self.upstream {
            Some(o) => (o.tls, Some(o.addr)),
            None => (true, None),
        };
        io.push(IoReq::Client(ClientCmd::Fetch {
            fetch: fetch(req),
            target: Target {
                host: route.upstream_host.clone(),
                port: route.upstream_port,
                tls,
                addr,
            },
            head: build_upstream_request_head(&request, &route).into_bytes(),
            body: request.body.clone(),
        }));
        let stage = Stage::Fetching;
        self.relays.insert(
            req,
            Relay {
                request,
                route,
                stage,
            },
        );
    }

    fn on_upstream(&mut self, event: ClientEvent, io: &mut Vec<IoReq>, host: &mut Vec<HostReq>) {
        let req = ReqId(event.fetch().0);
        let Some(mut relay) = self.relays.remove(&req) else {
            return;
        };
        match (relay.stage, event) {
            (
                Stage::Fetching,
                ClientEvent::Head {
                    status,
                    headers: h,
                    raw: head,
                    ..
                },
            ) => {
                io.push(IoReq::Server(ServerCmd::RawStart { req }));
                io.push(IoReq::Server(ServerCmd::RawBytes { req, bytes: head }));
                let body = Vec::new();
                let response = UpstreamResponse {
                    status,
                    headers: headers(h),
                    body,
                };
                relay.stage = Stage::Streaming {
                    response,
                    owed: false,
                };
            }
            (
                Stage::Streaming {
                    mut response,
                    owed: false,
                },
                ClientEvent::Body { bytes, .. },
            ) => {
                response.body.extend_from_slice(&bytes);
                io.push(IoReq::Server(ServerCmd::RawBytes { req, bytes }));
                relay.stage = Stage::Streaming {
                    response,
                    owed: true,
                };
            }
            (Stage::Streaming { response, .. }, ClientEvent::End { .. }) => {
                io.push(IoReq::Server(ServerCmd::RawEnd { req }));
                self.capture((relay.request, response, relay.route), host);
                return self.settle(io);
            }
            (stage, ClientEvent::Failed { error, .. }) => {
                // With nothing relayed yet, an empty raw response closes the
                // client's connection without one, as dropping it used to.
                if let Stage::Fetching = stage {
                    io.push(IoReq::Server(ServerCmd::RawStart { req }));
                }
                io.push(IoReq::Server(ServerCmd::RawEnd { req }));
                host.push(HostReq::Log(format!("connection error: {error}")));
                return self.settle(io);
            }
            // Out of order: the planner never does this, so it is ignored.
            (stage, _) => relay.stage = stage,
        }
        self.relays.insert(req, relay);
    }

    /// Hand the exchange to the embedder, unless once mode has already ended.
    fn capture(&mut self, exchange: Exchange, host: &mut Vec<HostReq>) {
        if self.mode == Mode::Once && self.stopping {
            return;
        }
        let (request, response, _) = &exchange;
        host.push(HostReq::Log(format!(
            "captured exchange #{} {} {} -> {} ({} body bytes)",
            self.captured,
            request.method,
            request.path(),
            response.status,
            response.body.len()
        )));
        host.push(HostReq::Captured(exchange));
        self.captured += 1;
    }

    /// A routable exchange is over, however it ended. In once mode, that was
    /// the recording.
    fn settle(&mut self, io: &mut Vec<IoReq>) {
        if self.mode == Mode::Once {
            self.shut_down(io);
        }
    }

    /// Stop taking requests. Relays in flight get [`GRACE`] to finish, then
    /// the server cuts them and says so with `Gone`.
    fn shut_down(&mut self, io: &mut Vec<IoReq>) {
        if !self.stopping {
            self.stopping = true;
            io.push(IoReq::Server(ServerCmd::Shutdown { grace: GRACE }));
        }
    }
}

impl Core for RecorderCore {
    type Comp = Comp;
    type IoReq = IoReq;
    type HostReq = HostReq;

    fn step(
        &mut self,
        _now: Time,
        comps: &mut Vec<Comp>,
        io: &mut Vec<IoReq>,
        host: &mut Vec<HostReq>,
    ) {
        for comp in comps.drain(..) {
            match comp {
                Comp::Server(ServerEvent::Request { req, request }) => {
                    self.on_request(req, request, io);
                }
                Comp::Server(ServerEvent::Flushed { req }) => {
                    if let Some(Relay {
                        stage:
                            Stage::Streaming {
                                owed: owed @ true, ..
                            },
                        ..
                    }) = self.relays.get_mut(&req)
                    {
                        *owed = false;
                        io.push(IoReq::Client(ClientCmd::Ack { fetch: fetch(req) }));
                    }
                }
                // The client left, or the grace ran out: stop fetching for it.
                Comp::Server(ServerEvent::Gone { req }) => {
                    if self.relays.remove(&req).is_some() {
                        io.push(IoReq::Client(ClientCmd::Cancel { fetch: fetch(req) }));
                        let why = "connection error: the response to the client was cut short";
                        host.push(HostReq::Log(why.to_owned()));
                        self.settle(io);
                    }
                }
                Comp::Server(ServerEvent::Signal { .. }) => {}
                Comp::Client(event) => self.on_upstream(event, io, host),
                Comp::Stop => self.shut_down(io),
            }
        }
    }

    /// The core keeps no timers: the grace period is the I/O step's.
    fn deadline(&self) -> Option<Time> {
        None
    }

    fn done(&self) -> bool {
        self.stopping && self.relays.is_empty()
    }
}

fn fetch(req: ReqId) -> FetchId {
    FetchId(req.0)
}

fn headers(pairs: Vec<(String, String)>) -> Vec<Header> {
    pairs.into_iter().map(|(n, v)| Header::new(n, v)).collect()
}

#[cfg(test)]
mod tests;
