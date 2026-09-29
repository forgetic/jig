//! The HTTP/1.1 client planner: upstream fetches, one request per
//! connection, reported to a core as [`ClientEvent`]s and driven by its
//! [`ClientCmd`]s. See `docs/explanation/sans-io-shell.md` §4.7 and §5.5.
//!
//! A fetch resolves its host (§4.7's blocking `Resolve`, unless the caller
//! gave an address), tries each address in turn, runs TLS over the socket if
//! asked, writes the caller's request bytes verbatim, parses the response
//! head and then streams the body. The vocabulary has no jig types: the
//! caller builds the request head, and gets the response head back as parsed
//! fields and as the raw bytes, for relaying verbatim.
//!
//! Each fetch is a `Phase` enum whose variants hold that phase's data, as in
//! the server planner: every transition is one `match` arm that moves owned
//! data on, and every reap and step sweeps all fetches through it. Beside the
//! phase sits the connection's stages (`Pipe`), once the fetch has a socket:
//! a [`Conn`] and, optionally, the TLS stage between it and the HTTP parser.
//! Sockets a fetch no longer uses (a failed attempt's, a finished fetch's)
//! are retired to a list until their `Closed` comes, so a fetch only ever
//! holds its current socket.
//!
//! # Until EOF
//!
//! The body ends when the peer closes: a TCP FIN, or TLS closure (see
//! [`crate::tls`] on a FIN without close_notify). The codec's
//! [`Framing`](super::codec::Framing) is not used to end it. The recorder
//! sends `Connection: close` upstream and relays and captures the raw bytes,
//! chunk framing included, so reading to EOF is both enough and what it
//! wants.
//!
//! A framing-aware mode, for temper's model client, would add a flag to
//! `Fetch` and carry the head's framing into the `Body` state. A `Length`
//! body then ends after that many bytes, and a `Chunked` one feeds a
//! [`ChunkedDecoder`](super::codec::ChunkedDecoder) and ends when the
//! decoder is done; either way `End` comes without waiting for the peer to
//! close. Reusing the connection for another request would come after that.
//!
//! # Interim responses
//!
//! A `1xx` response other than `101` (`100 Continue`, `103 Early Hints`) is
//! dropped where it stands: the core gets the `Head` of the final response
//! that follows it. The request goes out whole, head and body at once, so a
//! `100 Continue` releases nothing.
//!
//! # Early responses
//!
//! An upstream may answer before it has read the whole request (a 413, 401
//! or 429) and close, so the rest of the upload fails with `EPIPE` or a
//! reset. That write error stops only the writing (see [`crate::tcp`]): the
//! response is read as usual, and the fetch fails with the write error only
//! if the peer ends before a head arrives.
//!
//! # Credit
//!
//! Backpressure across connections is credit from the core (§5.5): at most
//! one [`ClientEvent::Body`] is outstanding per fetch, and nothing is read
//! from that upstream until the core's [`ClientCmd::Ack`]. The `Conn`'s read
//! limit does the stopping. `End` or `Failed` may follow a `Body` before its
//! `Ack`; an `Ack` or `Cancel` after them is ignored.
//!
//! # Composing with the server planner
//!
//! The recorder runs a [`Server`](super::server::Server) and a `Client` in
//! one `IoStep`. Give the client a disjoint id range (`Ids::split`), let
//! [`Client::reap`] take the events it owns out of the batch (it leaves the
//! rest in order, for the server), and in each batch hand the core the
//! server's events before the client's. That order makes a `Flushed` the core
//! sees cover every `RawBytes` it had sent before, so acknowledging a relayed
//! `Body` on the next `Flushed` never releases credit early.

use std::collections::BTreeMap;
use std::fmt;
use std::mem;
use std::net::SocketAddr;
use std::vec;

#[cfg(feature = "tls")]
use std::sync::Arc;

#[cfg(feature = "tls")]
use rustls_pki_types::ServerName;

use super::FetchId;
use super::codec::{self, HeadError, Limits};
use crate::run::IoStep;
use crate::sys::{Action, Event, Ids, IoError};
use crate::tcp::{Conn, READ_CHUNK};
use crate::time::Time;
#[cfg(feature = "tls")]
use crate::tls::{DEFAULT_BUFFER_LIMIT, StageBufs, TlsClient, TlsError, TlsStatus};

/// Where a fetch goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The name to resolve and, with TLS, to send as SNI and check the
    /// certificate against.
    pub host: String,
    pub port: u16,
    pub tls: bool,
    /// Connect here instead of resolving `host` (a test hook). TLS still
    /// names the server by `host`.
    pub addr: Option<SocketAddr>,
}

/// What the core tells the client. Commands for unknown or finished fetches
/// are ignored, and so is a `Fetch` whose id is still in use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientCmd {
    /// Send `head` then `body`, both verbatim, to `target`. The head should
    /// say `Connection: close`: the body is read until the peer closes.
    Fetch {
        fetch: FetchId,
        target: Target,
        head: Vec<u8>,
        body: Vec<u8>,
    },
    /// The last `Body` has been dealt with: read on.
    Ack { fetch: FetchId },
    /// Give up: the fetch fails with [`ClientError::Cancelled`] at once.
    Cancel { fetch: FetchId },
}

/// What the client tells its core. Each fetch gets exactly one terminal
/// event, `End` or `Failed`, and nothing after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientEvent {
    /// The response head: parsed, and `raw` exactly as received.
    Head {
        fetch: FetchId,
        status: u16,
        headers: Vec<(String, String)>,
        raw: Vec<u8>,
    },
    /// Body bytes, raw (chunk framing included). The next one waits for an
    /// `Ack`.
    Body {
        fetch: FetchId,
        bytes: Vec<u8>,
    },
    /// The peer closed after the head: every body byte has been delivered.
    End {
        fetch: FetchId,
    },
    Failed {
        fetch: FetchId,
        error: ClientError,
    },
}

impl ClientEvent {
    pub fn fetch(&self) -> FetchId {
        match self {
            ClientEvent::Head { fetch, .. }
            | ClientEvent::Body { fetch, .. }
            | ClientEvent::End { fetch }
            | ClientEvent::Failed { fetch, .. } => *fetch,
        }
    }

    /// `End` or `Failed`: the fetch is over.
    pub fn is_terminal(&self) -> bool {
        matches!(self, ClientEvent::End { .. } | ClientEvent::Failed { .. })
    }
}

/// Why a fetch failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientError {
    /// The host did not resolve (`Some`), or resolved to no address.
    Resolve(Option<IoError>),
    /// No address accepted the connection: the last one's error.
    Connect(IoError),
    /// The TLS stage failed (a certificate, the handshake, a truncated
    /// stream), or TLS was asked for without a config. The text is
    /// [`crate::tls::TlsError`]'s where the stage found the problem.
    Tls(String),
    /// Reading the connection failed (a reset, say), or writing it did and
    /// the peer then closed without a response head. A write error after
    /// the head changes nothing: the body is read to EOF as usual.
    Io(IoError),
    /// The response head was malformed or too large, or the peer closed
    /// before it ended.
    Protocol(HeadError),
    /// The core cancelled the fetch.
    Cancelled,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Resolve(Some(e)) => write!(f, "resolving the upstream host failed: {e}"),
            ClientError::Resolve(None) => f.write_str("the upstream host resolved to no address"),
            ClientError::Connect(e) => write!(f, "connecting upstream failed: {e}"),
            ClientError::Tls(why) => write!(f, "upstream TLS failed: {why}"),
            ClientError::Io(e) => write!(f, "the upstream connection failed: {e}"),
            ClientError::Protocol(e) => write!(f, "bad upstream response: {e}"),
            ClientError::Cancelled => f.write_str("the fetch was cancelled"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Why a response is cut short before its head ends.
const EOF_IN_HEAD: HeadError =
    HeadError::Malformed("the connection closed before the response head ended");

/// The HTTP/1.1 client planner. See the module docs.
#[derive(Debug)]
pub struct Client {
    fetches: BTreeMap<FetchId, Fetch>,
    shared: Shared,
    /// `reap`'s scratch: the events that are not the client's, handed back.
    foreign: Vec<Event>,
}

/// What every fetch draws on.
#[derive(Debug)]
struct Shared {
    ids: Ids,
    limits: Limits,
    /// Sockets no fetch uses any more, kept until their `Closed` arrives.
    retired: Vec<Conn>,
    #[cfg(feature = "tls")]
    tls: Option<Arc<rustls::ClientConfig>>,
}

#[derive(Debug)]
struct Fetch {
    phase: Phase,
    /// The current attempt's stages: none while resolving, and none once
    /// done (the socket is retired).
    pipe: Option<Pipe>,
}

#[derive(Debug)]
enum Phase {
    /// Waiting for the `Resolved` of `query`, with the stages' link.
    Resolving {
        query: u64,
        request: Vec<u8>,
        link: Link,
    },
    /// Connecting to one address, with `rest` to try if it fails. The request
    /// waits here: a failed attempt's `Conn` drops what it was given.
    Connecting {
        rest: vec::IntoIter<SocketAddr>,
        request: Vec<u8>,
    },
    /// Sending the request and reading the response head. `plain_in[..scanned]`
    /// has no newline the parser hasn't seen.
    Head { scanned: usize },
    /// Relaying the body. `owed`: a `Body` is out and its `Ack` hasn't come.
    Body { owed: bool },
    /// The terminal event is out and the socket retired: forgotten at the
    /// end of the sweep.
    Done,
}

/// A connection's stages, socket side first.
#[derive(Debug)]
struct Pipe {
    tcp: Conn,
    link: Link,
}

#[derive(Debug)]
enum Link {
    Plain,
    #[cfg(feature = "tls")]
    Tls(Tls),
}

#[cfg(feature = "tls")]
#[derive(Debug)]
struct Tls {
    stage: TlsClient,
    /// Holds the plaintext for good; the ciphertext lives in the `Conn`'s
    /// buffers and is swapped in only for a pump.
    bufs: StageBufs,
    /// The last pump's report.
    status: TlsStatus,
}

impl Client {
    /// A client that names its sockets and resolve queries from `ids`, which
    /// must not overlap those of any other planner on the same reactor (see
    /// `Ids::split`). It speaks plain HTTP until given a TLS config.
    pub fn new(ids: Ids) -> Client {
        Client {
            fetches: BTreeMap::new(),
            shared: Shared {
                ids,
                limits: Limits::default(),
                retired: Vec::new(),
                #[cfg(feature = "tls")]
                tls: None,
            },
            foreign: Vec::new(),
        }
    }

    /// Fetches with `tls: true` use `config`, typically from
    /// [`crate::tls::client_config`]. Without one they fail with
    /// [`ClientError::Tls`].
    #[cfg(feature = "tls")]
    pub fn with_tls(mut self, config: Arc<rustls::ClientConfig>) -> Client {
        self.shared.tls = Some(config);
        self
    }

    /// Head limits for responses, instead of the codec's defaults
    /// (`max_body` is not used: bodies stream).
    pub fn with_limits(mut self, limits: Limits) -> Client {
        self.shared.limits = limits;
        self
    }

    /// The event is for one of the client's sockets or resolve queries.
    pub fn owns(&self, event: &Event) -> bool {
        self.owner(event).is_some()
    }

    /// Take the events the client owns out of `events` and leave the others
    /// there, in order, for whichever planner shares the reactor.
    pub fn reap(&mut self, _now: Time, events: &mut Vec<Event>, out: &mut Vec<ClientEvent>) {
        for event in events.drain(..) {
            match self.owner(&event) {
                Some(Owner::Fetch(id)) => {
                    if let Some(fetch) = self.fetches.get_mut(&id) {
                        on_event(id, fetch, event, &mut self.shared, out);
                    }
                }
                Some(Owner::Retired(i)) => {
                    if let Some(tcp) = self.shared.retired.get_mut(i) {
                        tcp.on_event(event);
                    }
                }
                None => self.foreign.push(event),
            }
        }
        mem::swap(events, &mut self.foreign);
        self.sweep(out);
    }

    /// Apply one command. A `Fetch` that has to resolve plans its `Resolve`
    /// here; a `Cancel` reports its `Failed` here.
    pub fn command(
        &mut self,
        _now: Time,
        cmd: ClientCmd,
        out: &mut Vec<ClientEvent>,
        actions: &mut Vec<Action>,
    ) {
        match cmd {
            ClientCmd::Fetch {
                fetch,
                target,
                head,
                body,
            } => self.start(fetch, target, head, body, out, actions),
            ClientCmd::Ack { fetch } => {
                if let Some(Fetch {
                    phase: Phase::Body { owed },
                    ..
                }) = self.fetches.get_mut(&fetch)
                {
                    *owed = false;
                }
            }
            ClientCmd::Cancel { fetch } => {
                if let Some(cancelled) = self.fetches.remove(&fetch) {
                    if let Some(pipe) = cancelled.pipe {
                        self.shared.retire(pipe.tcp, false);
                    }
                    let error = ClientError::Cancelled;
                    out.push(ClientEvent::Failed { fetch, error });
                }
            }
        }
    }

    /// Move every fetch along and plan the actions its socket needs, and
    /// close retired sockets.
    pub fn plan(&mut self, _now: Time, out: &mut Vec<ClientEvent>, actions: &mut Vec<Action>) {
        self.sweep(out);
        for pipe in self.fetches.values_mut().filter_map(|f| f.pipe.as_mut()) {
            pipe.tcp.plan(actions);
        }
        for tcp in &mut self.shared.retired {
            // Planning first gives a TLS alert left in `outbound` its one
            // write; `close` then drops whatever it didn't take.
            tcp.plan(actions);
            tcp.close();
        }
    }

    /// No timeouts yet.
    pub fn deadline(&self) -> Option<Time> {
        None
    }

    /// Every fetch has had its terminal event and every socket is closed.
    pub fn idle(&self) -> bool {
        self.fetches.is_empty() && self.shared.retired.is_empty()
    }

    fn start(
        &mut self,
        fetch: FetchId,
        target: Target,
        head: Vec<u8>,
        body: Vec<u8>,
        out: &mut Vec<ClientEvent>,
        actions: &mut Vec<Action>,
    ) {
        if self.fetches.contains_key(&fetch) {
            return;
        }
        let link = match self.shared.link(&target) {
            Ok(link) => link,
            Err(error) => return out.push(ClientEvent::Failed { fetch, error }),
        };
        let mut request = head;
        request.extend_from_slice(&body);
        let state = match target.addr {
            Some(addr) => self
                .shared
                .dial(addr, Vec::new().into_iter(), request, link),
            None => {
                let query = self.shared.ids.next_id();
                let (host, port) = (target.host, target.port);
                actions.push(Action::Resolve { query, host, port });
                let phase = Phase::Resolving {
                    query,
                    request,
                    link,
                };
                Fetch { phase, pipe: None }
            }
        };
        self.fetches.insert(fetch, state);
    }

    fn owner(&self, event: &Event) -> Option<Owner> {
        let sock = event.sock();
        let owns = |f: &Fetch| match (&f.phase, event) {
            (Phase::Resolving { query, .. }, Event::Resolved { query: q, .. }) => query == q,
            _ => sock.is_some() && f.pipe.as_ref().map(|pipe| pipe.tcp.sock()) == sock,
        };
        if let Some((&fetch, _)) = self.fetches.iter().find(|(_, f)| owns(f)) {
            return Some(Owner::Fetch(fetch));
        }
        let sock = sock?;
        let mut retired = self.shared.retired.iter();
        retired
            .position(|tcp| tcp.sock() == sock)
            .map(Owner::Retired)
    }

    /// Move every fetch along, and forget the finished ones and the closed
    /// sockets.
    fn sweep(&mut self, out: &mut Vec<ClientEvent>) {
        for (&id, fetch) in &mut self.fetches {
            let phase = mem::replace(&mut fetch.phase, Phase::Done);
            fetch.phase = next(phase, &mut fetch.pipe, id, &mut self.shared, out);
        }
        self.fetches
            .retain(|_, fetch| !matches!(fetch.phase, Phase::Done));
        self.shared.retired.retain(|tcp| !tcp.is_closed());
    }
}

impl IoStep for Client {
    type Comp = ClientEvent;
    type Req = ClientCmd;

    /// Standalone, the client drops the events it doesn't own (signals).
    fn reap(&mut self, now: Time, events: &mut Vec<Event>, comps: &mut Vec<ClientEvent>) {
        Client::reap(self, now, events, comps);
        events.clear();
    }

    fn step(
        &mut self,
        now: Time,
        reqs: &mut Vec<ClientCmd>,
        comps: &mut Vec<ClientEvent>,
        actions: &mut Vec<Action>,
    ) {
        for cmd in reqs.drain(..) {
            self.command(now, cmd, comps, actions);
        }
        self.plan(now, comps, actions);
    }

    fn deadline(&self) -> Option<Time> {
        Client::deadline(self)
    }

    fn idle(&self) -> bool {
        Client::idle(self)
    }
}

/// Whose an event is.
enum Owner {
    Fetch(FetchId),
    /// An index into `Shared::retired`.
    Retired(usize),
}

/// Hand an event to the fetch it belongs to.
fn on_event(
    id: FetchId,
    fetch: &mut Fetch,
    event: Event,
    shared: &mut Shared,
    out: &mut Vec<ClientEvent>,
) {
    let Event::Resolved { result, .. } = event else {
        if let Some(pipe) = &mut fetch.pipe {
            pipe.tcp.on_event(event);
        }
        return;
    };
    match (mem::replace(&mut fetch.phase, Phase::Done), result) {
        (Phase::Resolving { request, link, .. }, Ok(addrs)) => {
            let mut rest = addrs.into_iter();
            *fetch = match rest.next() {
                Some(addr) => shared.dial(addr, rest, request, link),
                None => fail(id, ClientError::Resolve(None), out),
            };
        }
        (Phase::Resolving { .. }, Err(e)) => *fetch = fail(id, ClientError::Resolve(Some(e)), out),
        (phase, _) => fetch.phase = phase,
    }
}

/// The phase after `phase`, given the socket in `slot`: one `match` arm per
/// phase. A phase without a socket stays as it is.
fn next(
    phase: Phase,
    slot: &mut Option<Pipe>,
    fetch: FetchId,
    shared: &mut Shared,
    out: &mut Vec<ClientEvent>,
) -> Phase {
    let Some(pipe) = slot else {
        return phase;
    };
    match phase {
        Phase::Connecting { mut rest, request } => {
            if pipe.tcp.is_open() {
                pipe.send(request);
                return next(Phase::Head { scanned: 0 }, slot, fetch, shared, out);
            }
            let Some(error) = pipe.tcp.error().cloned() else {
                return Phase::Connecting { rest, request };
            };
            match rest.next() {
                Some(addr) => {
                    let failed = mem::replace(&mut pipe.tcp, shared.connect(addr));
                    shared.retire(failed, false);
                    Phase::Connecting { rest, request }
                }
                None => shared.end(fetch, slot, Err(ClientError::Connect(error)), out),
            }
        }
        Phase::Head { mut scanned } => {
            if let Err(error) = pipe.flow() {
                return shared.end(fetch, slot, Err(error), out);
            }
            let limits = shared.limits;
            let parsed = loop {
                match codec::reparse(
                    pipe.plain_in(),
                    &mut scanned,
                    &limits,
                    codec::parse_response_head,
                ) {
                    // An interim response (100 Continue, 103 Early Hints)
                    // comes before the real one and has no body; the core
                    // relays and captures only the real one. 101 is final:
                    // the connection is the upgraded protocol's from here.
                    Ok(Some(head)) if (100..200).contains(&head.status) && head.status != 101 => {
                        pipe.plain_in().drain(..head.head_len);
                        scanned = 0;
                    }
                    other => break other,
                }
            };
            let plain = pipe.plain_in();
            match parsed {
                Ok(Some(head)) => {
                    let body = plain.split_off(head.head_len);
                    let raw = mem::replace(plain, body);
                    out.push(ClientEvent::Head {
                        fetch,
                        status: head.status,
                        headers: head.headers,
                        raw,
                    });
                    next(Phase::Body { owed: false }, slot, fetch, shared, out)
                }
                // A write error counts only once no head can come: a peer that
                // stopped taking the request may have answered it first.
                Ok(None) if pipe.ended() => {
                    let error = match pipe.tcp.error() {
                        Some(e) => ClientError::Io(e.clone()),
                        None => ClientError::Protocol(EOF_IN_HEAD),
                    };
                    shared.end(fetch, slot, Err(error), out)
                }
                Ok(None) => {
                    pipe.want(limits.max_head);
                    Phase::Head { scanned }
                }
                Err(e) => shared.end(fetch, slot, Err(ClientError::Protocol(e)), out),
            }
        }
        Phase::Body { mut owed } => {
            if let Err(error) = pipe.flow() {
                return shared.end(fetch, slot, Err(error), out);
            }
            let plain = pipe.plain_in();
            if !owed && !plain.is_empty() {
                let bytes = mem::take(plain);
                out.push(ClientEvent::Body { fetch, bytes });
                owed = true;
            }
            if pipe.plain_in().is_empty() && pipe.ended() {
                return shared.end(fetch, slot, Ok(()), out);
            }
            pipe.want(if owed { 0 } else { READ_CHUNK });
            Phase::Body { owed }
        }
        Phase::Resolving { .. } | Phase::Done => phase,
    }
}

/// Report a failure that left no socket behind.
fn fail(fetch: FetchId, error: ClientError, out: &mut Vec<ClientEvent>) -> Fetch {
    out.push(ClientEvent::Failed { fetch, error });
    Fetch {
        phase: Phase::Done,
        pipe: None,
    }
}

impl Shared {
    /// The link a target asks for.
    fn link(&self, target: &Target) -> Result<Link, ClientError> {
        if target.tls {
            self.tls_link(&target.host)
        } else {
            Ok(Link::Plain)
        }
    }

    #[cfg(feature = "tls")]
    fn tls_link(&self, host: &str) -> Result<Link, ClientError> {
        let Some(config) = self.tls.clone() else {
            return Err(ClientError::Tls("no TLS config was given".to_owned()));
        };
        let name = ServerName::try_from(host.to_owned())
            .map_err(|e| ClientError::Tls(format!("{host:?} is not a server name: {e}")))?;
        // `plain_in` must hold a whole head, or a long one would stall
        // rather than fail at the limit.
        let stage = TlsClient::new(config, name)
            .map_err(tls_error)?
            .with_buffer_limit(self.limits.max_head.max(DEFAULT_BUFFER_LIMIT));
        Ok(Link::Tls(Tls {
            stage,
            bufs: StageBufs::default(),
            status: TlsStatus {
                handshaking: true,
                peer_closed: false,
                wants_read: true,
                wants_write: false,
                close_sent: false,
            },
        }))
    }

    #[cfg(not(feature = "tls"))]
    fn tls_link(&self, _host: &str) -> Result<Link, ClientError> {
        Err(ClientError::Tls(
            "steploop was built without its `tls` feature".to_owned(),
        ))
    }

    /// A fetch connecting to `addr` first.
    fn dial(
        &mut self,
        addr: SocketAddr,
        rest: vec::IntoIter<SocketAddr>,
        request: Vec<u8>,
        link: Link,
    ) -> Fetch {
        let tcp = self.connect(addr);
        Fetch {
            phase: Phase::Connecting { rest, request },
            pipe: Some(Pipe { tcp, link }),
        }
    }

    fn connect(&mut self, addr: SocketAddr) -> Conn {
        Conn::connect(self.ids.next_sock(), addr)
    }

    /// Report the fetch's end and retire its socket. A TLS failure leaves an
    /// alert in `outbound` telling the server why; it gets one write.
    fn end(
        &mut self,
        fetch: FetchId,
        slot: &mut Option<Pipe>,
        outcome: Result<(), ClientError>,
        out: &mut Vec<ClientEvent>,
    ) -> Phase {
        let alert = matches!(outcome, Err(ClientError::Tls(_)));
        if let Some(pipe) = slot.take() {
            self.retire(pipe.tcp, alert);
        }
        out.push(match outcome {
            Ok(()) => ClientEvent::End { fetch },
            Err(error) => ClientEvent::Failed { fetch, error },
        });
        Phase::Done
    }

    /// Keep `tcp` until it is closed: at once, or after one write of what it
    /// holds (`flush`).
    fn retire(&mut self, mut tcp: Conn, flush: bool) {
        if flush {
            tcp.set_read_limit(0);
        } else {
            tcp.close();
        }
        if !tcp.is_closed() {
            self.retired.push(tcp);
        }
    }
}

impl Pipe {
    /// Queue the request: onto the socket, or into TLS, which holds it until
    /// the handshake is done.
    fn send(&mut self, request: Vec<u8>) {
        match &mut self.link {
            Link::Plain => self.tcp.send(request),
            #[cfg(feature = "tls")]
            Link::Tls(tls) => tls.bufs.plain_out = request,
        }
    }

    /// Move bytes between the socket's buffers and the plaintext ones, and
    /// report a failure of either. A write error alone is no failure here:
    /// the response may still arrive (see `crate::tcp`).
    fn flow(&mut self) -> Result<(), ClientError> {
        if let Some(e) = self.tcp.read_error() {
            return Err(ClientError::Io(e.clone()));
        }
        match &mut self.link {
            Link::Plain => Ok(()),
            #[cfg(feature = "tls")]
            Link::Tls(tls) => tls.pump(&mut self.tcp).map_err(tls_error),
        }
    }

    /// Response bytes received and not yet handed out.
    fn plain_in(&mut self) -> &mut Vec<u8> {
        match &mut self.link {
            Link::Plain => &mut self.tcp.inbound,
            #[cfg(feature = "tls")]
            Link::Tls(tls) => &mut tls.bufs.plain_in,
        }
    }

    /// The peer's stream has ended and everything in it is in `plain_in`.
    fn ended(&self) -> bool {
        match &self.link {
            Link::Plain => self.tcp.eof(),
            #[cfg(feature = "tls")]
            Link::Tls(tls) => tls.status.peer_closed,
        }
    }

    /// Read while `plain_in` holds fewer than `limit` bytes (zero: stop).
    /// Through TLS the socket carries ciphertext, so it reads a chunk at a
    /// time while the stage wants more and `limit` allows any.
    fn want(&mut self, limit: usize) {
        let limit = match &self.link {
            Link::Plain => limit,
            #[cfg(feature = "tls")]
            Link::Tls(tls) if limit > 0 && tls.status.wants_read => READ_CHUNK,
            #[cfg(feature = "tls")]
            Link::Tls(_) => 0,
        };
        self.tcp.set_read_limit(limit);
    }
}

#[cfg(feature = "tls")]
impl Tls {
    /// One pump, with the socket's buffers standing in as the ciphertext
    /// ones. `pump` appends to `cipher_out`; bytes behind a write in flight
    /// are kept behind its remainder by the `Conn`.
    fn pump(&mut self, tcp: &mut Conn) -> Result<(), TlsError> {
        if tcp.eof() {
            self.stage.peer_eof();
        }
        mem::swap(&mut tcp.inbound, &mut self.bufs.cipher_in);
        mem::swap(&mut tcp.outbound, &mut self.bufs.cipher_out);
        let result = self.stage.pump(&mut self.bufs);
        mem::swap(&mut tcp.inbound, &mut self.bufs.cipher_in);
        mem::swap(&mut tcp.outbound, &mut self.bufs.cipher_out);
        self.status = result?;
        Ok(())
    }
}

#[cfg(feature = "tls")]
fn tls_error(e: TlsError) -> ClientError {
    ClientError::Tls(e.to_string())
}
