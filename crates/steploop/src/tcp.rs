//! The TCP stages: [`Conn`], one socket's byte pipe and lifecycle, and
//! [`Listener`], which accepts sockets. See
//! `docs/explanation/sans-io-shell.md` §5.5.
//!
//! Both are pure planners. They take the [`Event`]s their owner routes to
//! them by [`SockId`] (an owner whose lookup misses drops the event) and plan
//! [`Action`]s. Upper stages (TLS, HTTP) work on a `Conn`'s buffers and say
//! what they want: how much to read, what to write, when to close. The `Conn`
//! turns that into syscalls and keeps the reactor's rules, so no upper stage
//! has to:
//!
//! - **One arm per socket.** The `Conn` arms only when no read or write is in
//!   flight, so every direction's need is known, and merges them into one
//!   `Arm`. A pending arm can't be widened (there is no re-arm action), so a
//!   need that arises while one is pending waits for its readiness. An upper
//!   stage that stops reading before it starts writing, as a server does,
//!   never meets this.
//! - **Backpressure.** It reads only while `inbound` is below the limit the
//!   upper stage sets, and never more than that limit leaves room for, so
//!   an upper stage that doesn't consume stops the reads.
//! - **Close** waits for synchronous actions still in flight (one round, since
//!   `perform` is synchronous) but not for a pending arm, which the `Close`
//!   completes.

use std::mem;
use std::net::SocketAddr;
use std::time::Duration;

use crate::sys::{Action, Event, Ids, Interest, IoError, Progress, SockId};
use crate::time::Time;

/// The most one `Read` asks for.
pub const READ_CHUNK: usize = 64 * 1024;

/// How long a [`Listener`] waits after a failed accept before trying again.
/// Short, since most failures (a peer that reset before the accept) are
/// transient, but long enough that a persistent one (out of fds) doesn't spin
/// the loop.
pub const ACCEPT_BACKOFF: Duration = Duration::from_millis(10);

/// One socket's byte pipe and lifecycle. See the module docs.
#[derive(Clone, Debug)]
pub struct Conn {
    sock: SockId,
    /// Bytes read and not yet consumed. The upper stage takes them from the
    /// front.
    pub inbound: Vec<u8>,
    /// Bytes to write, appended by the upper stage ([`Conn::send`] or
    /// directly). A `Write` in flight has taken its bytes from here;
    /// [`Conn::unsent`] counts both.
    pub outbound: Vec<u8>,
    read_limit: usize,
    life: Life,
    flight: Flight,
    /// Directions that hit `WouldBlock` and wait for readiness.
    blocked: Interest,
    eof: bool,
    error: Option<IoError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Life {
    /// To connect to this address; nothing planned yet.
    Dial(SocketAddr),
    /// `Connect` or `FinishConnect` in flight, or waiting for writability
    /// before `FinishConnect`.
    Connecting,
    Open,
    /// `close` was called: `Close` goes out once no read, write or connect is
    /// in flight.
    Closing,
    Closed,
}

/// What the `Conn` waits on. Synchronous actions last until the next `reap`;
/// an arm lasts until readiness or `Closed`.
#[derive(Clone, Copy, Debug, Default)]
struct Flight {
    read: bool,
    /// The length of the `Write` in flight; zero when there is none (empty
    /// writes are never planned).
    write: usize,
    op: Option<Op>,
    armed: bool,
}

/// A synchronous action other than a read or write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Connect,
    FinishConnect,
    Close,
}

impl Conn {
    /// An accepted, connected socket. It reads nothing until the upper stage
    /// sets a read limit.
    pub fn new(sock: SockId) -> Conn {
        Conn::with_life(sock, Life::Open)
    }

    /// A socket to connect to `addr`, named `sock`. The first `plan` issues
    /// the `Connect`; [`Conn::is_open`] turns true once it has succeeded. A
    /// failure shows as [`Conn::error`], and the `Conn` is closed at once if
    /// no socket was left behind; otherwise the upper stage closes it.
    pub fn connect(sock: SockId, addr: SocketAddr) -> Conn {
        Conn::with_life(sock, Life::Dial(addr))
    }

    fn with_life(sock: SockId, life: Life) -> Conn {
        Conn {
            sock,
            inbound: Vec::new(),
            outbound: Vec::new(),
            read_limit: 0,
            life,
            flight: Flight::default(),
            blocked: Interest::default(),
            eof: false,
            error: None,
        }
    }

    pub fn sock(&self) -> SockId {
        self.sock
    }

    /// Read while `inbound` holds fewer than `limit` bytes. Zero stops
    /// reading; a read already in flight still completes.
    pub fn set_read_limit(&mut self, limit: usize) {
        self.read_limit = limit;
    }

    /// Queue `bytes` for writing. Dropped once the socket has failed or is
    /// closing, since they could never be written.
    pub fn send(&mut self, bytes: Vec<u8>) {
        if self.error.is_some() || matches!(self.life, Life::Closing | Life::Closed) {
            return;
        }
        if self.outbound.is_empty() {
            self.outbound = bytes;
        } else {
            self.outbound.extend_from_slice(&bytes);
        }
    }

    /// Close as soon as no synchronous action is in flight, dropping unsent
    /// bytes. A pending arm doesn't delay it: `Close` completes the arm.
    pub fn close(&mut self) {
        match self.life {
            Life::Dial(_) => self.life = Life::Closed,
            Life::Closing | Life::Closed => {}
            Life::Connecting | Life::Open => self.life = Life::Closing,
        }
        self.outbound.clear();
    }

    /// Bytes read and not yet consumed.
    pub fn available(&self) -> usize {
        self.inbound.len()
    }

    /// The peer has finished sending. Writing may go on.
    pub fn eof(&self) -> bool {
        self.eof
    }

    /// The first read, write or connect error. The `Conn` stops reading and
    /// writing; the upper stage should close it.
    pub fn error(&self) -> Option<IoError> {
        self.error
    }

    /// Bytes queued or being written, not yet taken by the kernel: the
    /// outbound backpressure an upper stage watches.
    pub fn unsent(&self) -> usize {
        self.outbound.len() + self.flight.write
    }

    /// Connected and not closing.
    pub fn is_open(&self) -> bool {
        self.life == Life::Open
    }

    pub fn is_connecting(&self) -> bool {
        matches!(self.life, Life::Dial(_) | Life::Connecting)
    }

    /// The socket is gone: its `Closed` arrived, or its connect failed
    /// without leaving one.
    pub fn is_closed(&self) -> bool {
        self.life == Life::Closed
    }

    /// Take an event for this socket. Events for other sockets, and stale ones
    /// (for nothing in flight), are ignored.
    pub fn on_event(&mut self, event: Event) {
        if event.sock() != Some(self.sock) {
            return;
        }
        match event {
            Event::Read { result, .. } if self.flight.read => {
                self.flight.read = false;
                match result {
                    Ok(data) if data.is_empty() => self.eof = true,
                    Ok(data) if self.inbound.is_empty() => self.inbound = data,
                    Ok(data) => self.inbound.extend_from_slice(&data),
                    Err(e) if e.is_would_block() => self.blocked.read = true,
                    Err(e) => self.fail(e),
                }
            }
            Event::Wrote { data, result, .. } if self.flight.write > 0 => {
                self.flight.write = 0;
                match result {
                    Ok(n) => self.requeue(data, n),
                    Err(e) if e.is_would_block() => {
                        self.blocked.write = true;
                        self.requeue(data, 0);
                    }
                    Err(e) => self.fail(e),
                }
            }
            Event::Ready { result, .. } if self.flight.armed => {
                self.flight.armed = false;
                match result {
                    // Readiness may report more than was asked (a hang-up
                    // sets both); the next syscall tells the truth.
                    Ok(ready) => {
                        self.blocked.read &= !ready.readable;
                        self.blocked.write &= !ready.writable;
                    }
                    Err(e) => self.fail(e),
                }
            }
            Event::Connected { result, .. } => {
                let Some(op @ (Op::Connect | Op::FinishConnect)) = self.flight.op else {
                    return;
                };
                self.flight.op = None;
                match result {
                    Ok(Progress::Done) if self.life == Life::Connecting => self.life = Life::Open,
                    Ok(Progress::Done) => {}
                    Ok(Progress::InProgress) => self.blocked.write = true,
                    // A failed `Connect` leaves no socket to close.
                    Err(e) if op == Op::Connect => {
                        self.error.get_or_insert(e);
                        self.life = Life::Closed;
                    }
                    Err(e) => self.fail(e),
                }
            }
            // The socket is gone, and so is anything pending on it.
            Event::Closed { .. } => {
                self.life = Life::Closed;
                self.flight = Flight::default();
            }
            _ => {}
        }
    }

    /// Plan the actions the socket needs now.
    pub fn plan(&mut self, actions: &mut Vec<Action>) {
        let sock = self.sock;
        match self.life {
            Life::Dial(addr) => {
                actions.push(Action::Connect { sock, addr });
                self.flight.op = Some(Op::Connect);
                self.life = Life::Connecting;
            }
            Life::Connecting if self.flight.op.is_none() && self.error.is_none() => {
                if !self.blocked.write {
                    actions.push(Action::FinishConnect { sock });
                    self.flight.op = Some(Op::FinishConnect);
                } else if !self.flight.armed {
                    actions.push(Action::Arm {
                        sock,
                        interest: Interest::WRITE,
                    });
                    self.flight.armed = true;
                }
            }
            Life::Closing if !self.busy() => {
                actions.push(Action::Close { sock });
                self.flight.op = Some(Op::Close);
            }
            Life::Open if self.error.is_none() => self.plan_io(actions),
            _ => {}
        }
    }

    fn plan_io(&mut self, actions: &mut Vec<Action>) {
        let sock = self.sock;
        if self.flight.write == 0 && !self.blocked.write && !self.outbound.is_empty() {
            let data = mem::take(&mut self.outbound);
            self.flight.write = data.len();
            actions.push(Action::Write { sock, data });
        }
        let room = self.room();
        if !self.flight.read && !self.blocked.read && room > 0 {
            actions.push(Action::Read {
                sock,
                max: room.min(READ_CHUNK),
            });
            self.flight.read = true;
        }
        if !self.busy() && !self.flight.armed {
            let interest = Interest {
                read: self.blocked.read && room > 0,
                write: self.blocked.write && !self.outbound.is_empty(),
            };
            if !interest.is_empty() {
                actions.push(Action::Arm { sock, interest });
                self.flight.armed = true;
            }
        }
    }

    /// How many more bytes the upper stage wants read.
    fn room(&self) -> usize {
        if self.eof {
            0
        } else {
            self.read_limit.saturating_sub(self.inbound.len())
        }
    }

    /// A synchronous action is in flight.
    fn busy(&self) -> bool {
        self.flight.read || self.flight.write > 0 || self.flight.op.is_some()
    }

    /// Put a write's unwritten tail back in front of what was queued since
    /// (unless the socket is closing, and the tail can go).
    fn requeue(&mut self, mut data: Vec<u8>, written: usize) {
        data.drain(..written.min(data.len()));
        if !data.is_empty() && self.life == Life::Open {
            data.extend_from_slice(&self.outbound);
            self.outbound = data;
        }
    }

    fn fail(&mut self, e: IoError) {
        self.error.get_or_insert(e);
        self.outbound.clear();
    }
}

/// Accepts sockets. See the module docs.
#[derive(Clone, Debug)]
pub struct Listener {
    sock: SockId,
    /// Open sockets at most, counted by the owner; the kernel's backlog holds
    /// the rest.
    cap: usize,
    phase: Phase,
    /// An `Accept` or `Close` is in flight.
    busy: bool,
    armed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Accept until `WouldBlock`.
    Accepting,
    /// Accepting would block: wait for readability.
    Waiting,
    /// An accept failed: try again at this time.
    Backoff(Time),
    Closing,
    Closed,
}

impl Listener {
    /// A listener the embedder adopted under `sock`, accepting while the
    /// owner has fewer than `cap` sockets open.
    pub fn new(sock: SockId, cap: usize) -> Listener {
        Listener {
            sock,
            cap,
            phase: Phase::Accepting,
            busy: false,
            armed: false,
        }
    }

    pub fn sock(&self) -> SockId {
        self.sock
    }

    /// Take an event for the listener. Returns the id of a newly accepted
    /// socket, which the owner now owns (and must close), even if the
    /// listener is closing.
    pub fn on_event(&mut self, now: Time, event: Event) -> Option<SockId> {
        if event.sock() != Some(self.sock) {
            return None;
        }
        match event {
            Event::Accepted { new, result, .. } if self.busy => {
                self.busy = false;
                match result {
                    Ok(_) => return Some(new),
                    Err(e) if e.is_would_block() => self.settle(Phase::Waiting),
                    Err(_) => self.settle(Phase::Backoff(now.after(ACCEPT_BACKOFF))),
                }
            }
            Event::Ready { result, .. } if self.armed => {
                self.armed = false;
                if self.phase == Phase::Waiting {
                    self.phase = match result {
                        Ok(_) => Phase::Accepting,
                        Err(_) => Phase::Backoff(now.after(ACCEPT_BACKOFF)),
                    };
                }
            }
            Event::Closed { .. } => {
                self.phase = Phase::Closed;
                self.busy = false;
                self.armed = false;
            }
            _ => {}
        }
        None
    }

    /// Plan the next accept, arm or close. `open` is how many sockets the
    /// owner has open; new ids come from `ids`.
    pub fn plan(&mut self, now: Time, open: usize, ids: &mut Ids, actions: &mut Vec<Action>) {
        if self.busy {
            return;
        }
        if matches!(self.phase, Phase::Backoff(at) if at <= now) {
            self.phase = Phase::Accepting;
        }
        let listener = self.sock;
        let room = open < self.cap;
        match self.phase {
            Phase::Accepting if room => {
                let new = ids.next_sock();
                actions.push(Action::Accept { listener, new });
                self.busy = true;
            }
            Phase::Waiting if room && !self.armed => {
                actions.push(Action::Arm {
                    sock: listener,
                    interest: Interest::READ,
                });
                self.armed = true;
            }
            Phase::Closing => {
                actions.push(Action::Close { sock: listener });
                self.busy = true;
            }
            _ => {}
        }
    }

    /// Stop accepting and close the listener. A pending arm doesn't delay it.
    pub fn close(&mut self) {
        if self.phase != Phase::Closed {
            self.phase = Phase::Closing;
        }
    }

    /// When a backoff ends.
    pub fn deadline(&self) -> Option<Time> {
        match self.phase {
            Phase::Backoff(at) => Some(at),
            _ => None,
        }
    }

    pub fn is_closed(&self) -> bool {
        self.phase == Phase::Closed
    }

    /// Move to `next` after an accept, unless closing overtook it.
    fn settle(&mut self, next: Phase) {
        if self.phase == Phase::Accepting {
            self.phase = next;
        }
    }
}
