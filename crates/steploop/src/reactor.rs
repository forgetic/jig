//! The reactor: the one part of the loop that talks to the kernel.
//!
//! It owns every fd the loop uses and a [`polling::Poller`] in oneshot mode,
//! and does two things: [`Reactor::poll`] (the loop's only blocking call) and
//! [`Reactor::perform`] (planned non-blocking syscalls, one [`Event`] per
//! [`Action`]). It makes no decisions: the pure I/O step plans, the reactor
//! executes and reports. The contract it keeps is spelled out in
//! [`crate::sys`] and checked by `tests/reactor.rs`.
//!
//! Why oneshot: arming interest yields exactly one readiness event and must
//! then be re-armed, so an `Arm` is an operation with exactly one completion,
//! like every other action. Why own the wrapper rather than use mio or skein's
//! reactor: that is where the model's choices live (ids, fd ownership, one
//! completion per action, signals as events). See
//! `docs/explanation/sans-io-shell.md` §4.1.
//!
//! Every fd is registered with no interest when it arrives, and `Arm` turns
//! into `modify` with oneshot interest. epoll reports hang-ups and errors even
//! with no interest; the reactor drops such reports for unarmed sockets, which
//! is safe because oneshot has already disabled the fd and the next arm
//! re-reports the (persistent) condition.
//!
//! An `Arm` on a socket whose arm is still pending widens it: `modify` again
//! with the union of both interests, and the pending arm's one `Ready` covers
//! both. Rejecting it instead would leave the planner waiting on the first
//! arm's readiness, which may never come: a client waiting to read the
//! response while its request body is blocked on a full socket waits for a
//! server that is waiting for the rest of the body.
//!
//! Unix only: signals are `UnixStream` pairs.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use polling::{Events, Poller};
use socket2::{Domain, Protocol, Socket, Type};

use crate::sys::{Action, Event, Interest, IoError, Progress, Readiness, SignalId, SockId};

/// Signals take their ids from the top of the id space, counting down, so
/// they never meet the pure side's ids, which count up. A resource's poller
/// key is its id, and `polling` reserves `usize::MAX`.
const FIRST_SIGNAL: u64 = usize::MAX as u64 - 1;

enum Resource {
    Listener(TcpListener),
    Stream(TcpStream),
    /// A non-blocking connect in progress; becomes a `Stream` once
    /// `FinishConnect` sees it connected.
    Connecting(Socket),
    /// The read end of a signal's socket pair.
    Signal(UnixStream),
}

impl Resource {
    fn fd(&self) -> BorrowedFd<'_> {
        match self {
            Resource::Listener(s) => s.as_fd(),
            Resource::Stream(s) => s.as_fd(),
            Resource::Connecting(s) => s.as_fd(),
            Resource::Signal(s) => s.as_fd(),
        }
    }
}

struct Entry {
    res: Resource,
    /// The pending arm's interest, widened by any later arm: the socket's
    /// next readiness is its completion.
    armed: Option<Interest>,
}

/// Owns the loop's fds and the poller. See the module docs.
pub struct Reactor {
    poller: Poller,
    /// Reused across polls. Never zero-capacity: skein's `Events::default()`
    /// is, and silently drops every event, losing oneshot registrations.
    ready: Events,
    entries: BTreeMap<SockId, Entry>,
    next_signal: u64,
}

/// The raising half of a signal: `Send`, cheap to clone, usable from any
/// thread.
#[derive(Clone, Debug)]
pub struct SignalSender(Arc<UnixStream>);

impl SignalSender {
    /// Wake the loop with an [`Event::Signal`]. Raises coalesce until the
    /// loop sees them, so a full socket buffer (one is already pending) is not
    /// an error. Once the reactor is gone this fails with `BrokenPipe` (Rust
    /// programs ignore `SIGPIPE` by default).
    pub fn raise(&self) -> io::Result<()> {
        match (&*self.0).write(&[1]) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            r => r.map(drop),
        }
    }
}

impl Reactor {
    pub fn new() -> io::Result<Reactor> {
        Ok(Reactor {
            poller: Poller::new()?,
            ready: Events::new(),
            entries: BTreeMap::new(),
            next_signal: FIRST_SIGNAL,
        })
    }

    /// Own a bound listener under `id` (the pure side's name for it). It is
    /// made non-blocking. Fails with `AlreadyExists` if `id` is taken.
    pub fn adopt_listener(&mut self, id: SockId, listener: TcpListener) -> io::Result<()> {
        listener.set_nonblocking(true)?;
        self.insert(id, Resource::Listener(listener), false)
    }

    /// A new signal: raising the sender delivers [`Event::Signal`] from
    /// `poll`. Dropping every sender delivers one last `Signal`, after which
    /// the signal is never armed again.
    pub fn signal(&mut self) -> io::Result<(SignalId, SignalSender)> {
        let (rx, tx) = UnixStream::pair()?;
        rx.set_nonblocking(true)?;
        tx.set_nonblocking(true)?;
        let id = self.next_signal;
        self.insert(SockId(id), Resource::Signal(rx), true)?;
        self.next_signal -= 1;
        Ok((SignalId(id), SignalSender(Arc::new(tx))))
    }

    /// Block until readiness, a signal or `timeout` (`None`: no limit), and
    /// append a `Ready` for each armed socket that became ready and a
    /// `Signal` for each raised signal. May return with nothing appended.
    pub fn poll(&mut self, out: &mut Vec<Event>, timeout: Option<Duration>) -> io::Result<()> {
        self.ready.clear();
        // `polling` already retries EINTR internally, keeping its deadline;
        // this guards against a backend letting one through.
        while let Err(e) = self.poller.wait(&mut self.ready, timeout) {
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
        let mut failed = None;
        for ev in self.ready.iter() {
            let id = SockId(ev.key as u64);
            let Some(entry) = self.entries.get_mut(&id) else {
                continue; // closed since: nothing to report
            };
            match &mut entry.res {
                Resource::Signal(rx) => {
                    // Drain, so that the next raise makes it readable again.
                    // At EOF every sender is gone, and re-arming would wake
                    // the loop forever.
                    let mut buf = [0u8; 64];
                    let open = loop {
                        match rx.read(&mut buf) {
                            Ok(0) => break false,
                            Ok(_) => {}
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                            Err(_) => break true,
                        }
                    };
                    if open {
                        let rearm = polling::Event::readable(ev.key);
                        if let Err(e) = self.poller.modify(&*rx, rearm) {
                            failed.get_or_insert(e);
                        }
                    }
                    out.push(Event::Signal {
                        signal: SignalId(id.0),
                    });
                }
                _ if entry.armed.is_some() => {
                    entry.armed = None;
                    let readiness = Readiness {
                        readable: ev.readable,
                        writable: ev.writable,
                    };
                    out.push(Event::Ready {
                        sock: id,
                        result: Ok(readiness),
                    });
                }
                // Unarmed: a hang-up or error epoll reports regardless of
                // interest. The next arm reports it again.
                _ => {}
            }
        }
        failed.map_or(Ok(()), Err)
    }

    /// Drain `actions`, performing each in order, and append its event to
    /// `out`: exactly one per action, except that an accepted `Arm` completes
    /// later and a widening one joins the pending arm (see [`crate::sys`]).
    pub fn perform(&mut self, actions: &mut Vec<Action>, out: &mut Vec<Event>) {
        for action in actions.drain(..) {
            out.extend(self.perform_one(action));
        }
    }

    fn perform_one(&mut self, action: Action) -> Option<Event> {
        let event = match action {
            Action::Accept { listener, new } => Event::Accepted {
                listener,
                new,
                result: self.accept(listener, new).map_err(IoError::from),
            },
            Action::Connect { sock, addr } => Event::Connected {
                sock,
                result: self.connect(sock, addr).map_err(IoError::from),
            },
            Action::FinishConnect { sock } => Event::Connected {
                sock,
                result: self.finish_connect(sock).map_err(IoError::from),
            },
            Action::Read { sock, max } => Event::Read {
                sock,
                result: self.read(sock, max).map_err(IoError::from),
            },
            Action::Write { sock, data } => {
                let result = self.write(sock, &data).map_err(IoError::from);
                Event::Wrote { sock, data, result }
            }
            Action::Arm { sock, interest } => match self.arm(sock, interest) {
                Ok(()) => return None,
                Err(e) => Event::Ready {
                    sock,
                    result: Err(e.into()),
                },
            },
            Action::Close { sock } => Event::Closed {
                sock,
                result: self.close(sock).map_err(IoError::from),
            },
            Action::Resolve { query, host, port } => Event::Resolved {
                query,
                result: resolve(&host, port).map_err(IoError::from),
            },
        };
        Some(event)
    }

    fn accept(&mut self, listener: SockId, new: SockId) -> io::Result<SocketAddr> {
        self.vacant(new)?; // before accepting, so a bad id loses no connection
        let Resource::Listener(l) = &lookup(&mut self.entries, listener)?.res else {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        let (stream, addr) = retry(|| l.accept())?;
        // Streams are non-blocking, since the loop must never block, and have
        // Nagle off: planners write whole messages, and Nagle would hold back
        // a message's small tail (an SSE chunk, say) until the peer acks.
        stream.set_nonblocking(true)?;
        stream.set_nodelay(true)?;
        self.insert(new, Resource::Stream(stream), false)?;
        Ok(addr)
    }

    fn connect(&mut self, sock: SockId, addr: SocketAddr) -> io::Result<Progress> {
        self.vacant(sock)?;
        let s = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
        s.set_nonblocking(true)?;
        s.set_tcp_nodelay(true)?; // as for accepted streams
        let (res, progress) = match s.connect(&addr.into()) {
            Ok(()) => (Resource::Stream(s.into()), Progress::Done),
            // `ErrorKind::InProgress` is unstable, hence the errno. An
            // interrupted non-blocking connect carries on in the background.
            Err(e)
                if e.raw_os_error() == Some(libc::EINPROGRESS)
                    || e.kind() == io::ErrorKind::Interrupted =>
            {
                (Resource::Connecting(s), Progress::InProgress)
            }
            Err(e) => return Err(e),
        };
        self.insert(sock, res, false)?;
        Ok(progress)
    }

    /// Reports a failed connect once (reading `SO_ERROR` clears it).
    fn finish_connect(&mut self, sock: SockId) -> io::Result<Progress> {
        let Resource::Connecting(s) = &lookup(&mut self.entries, sock)?.res else {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        if let Some(e) = s.take_error()? {
            return Err(e);
        }
        match s.peer_addr() {
            Err(e) if e.kind() == io::ErrorKind::NotConnected => return Ok(Progress::InProgress),
            r => r.map(drop)?,
        }
        // Same fd, so the poller registration and any pending arm carry over.
        if let Some(mut entry) = self.entries.remove(&sock) {
            entry.res = match entry.res {
                Resource::Connecting(s) => Resource::Stream(s.into()),
                other => other,
            };
            self.entries.insert(sock, entry);
        }
        Ok(Progress::Done)
    }

    fn read(&mut self, sock: SockId, max: usize) -> io::Result<Vec<u8>> {
        let s = stream(&mut self.entries, sock)?;
        if max == 0 {
            // `Ok(empty)` would read as end of file.
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut buf = vec![0; max];
        let n = retry(|| s.read(&mut buf))?;
        buf.truncate(n);
        Ok(buf)
    }

    fn write(&mut self, sock: SockId, data: &[u8]) -> io::Result<usize> {
        let s = stream(&mut self.entries, sock)?;
        retry(|| s.write(data))
    }

    /// Arm, or widen the pending arm. `Ok` means no event now: the (joint)
    /// arm completes later. `Err` completes it at once, so on a failed
    /// widening the pending arm is over too.
    fn arm(&mut self, sock: SockId, interest: Interest) -> io::Result<()> {
        let entry = lookup(&mut self.entries, sock)?;
        let pending = entry.armed.unwrap_or_default();
        let wanted = pending.merge(interest);
        if wanted == pending {
            // Nothing new: a no-op under a pending arm, and an arm that could
            // never complete otherwise.
            return match entry.armed {
                Some(_) => Ok(()),
                None => Err(io::ErrorKind::InvalidInput.into()),
            };
        }
        entry.armed = None;
        let ev = polling::Event::new(key(sock)?, wanted.read, wanted.write);
        self.poller.modify(entry.res.fd(), ev)?;
        entry.armed = Some(wanted);
        Ok(())
    }

    fn close(&mut self, sock: SockId) -> io::Result<()> {
        lookup(&mut self.entries, sock)?; // signals are not closable
        match self.entries.remove(&sock) {
            Some(entry) => release(&self.poller, entry.res),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }

    fn vacant(&self, id: SockId) -> io::Result<()> {
        if self.entries.contains_key(&id) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        Ok(())
    }

    /// Register `res` (with read interest, or none) and own it under `id`.
    fn insert(&mut self, id: SockId, res: Resource, readable: bool) -> io::Result<()> {
        self.vacant(id)?;
        let ev = polling::Event::new(key(id)?, readable, false);
        // SAFETY: `add` requires the source to be deleted from the poller
        // before its fd is closed. The fd is owned by `res`, which moves into
        // `self.entries` below and leaves it only through `close` or `Drop`.
        // Both go through `release`, which deletes before dropping and leaks
        // the fd if the delete fails. If `add` fails, nothing is registered.
        unsafe { self.poller.add(&res.fd(), ev)? };
        self.entries.insert(id, Entry { res, armed: None });
        Ok(())
    }
}

impl Drop for Reactor {
    fn drop(&mut self) {
        for (_, entry) in std::mem::take(&mut self.entries) {
            let _ = release(&self.poller, entry.res);
        }
    }
}

/// A socket resource (never a signal), or `NotFound`.
fn lookup(entries: &mut BTreeMap<SockId, Entry>, id: SockId) -> io::Result<&mut Entry> {
    let entry = entries
        .get_mut(&id)
        .filter(|e| !matches!(e.res, Resource::Signal(_)));
    entry.ok_or_else(|| io::ErrorKind::NotFound.into())
}

fn stream(entries: &mut BTreeMap<SockId, Entry>, id: SockId) -> io::Result<&mut TcpStream> {
    match &mut lookup(entries, id)?.res {
        Resource::Stream(s) => Ok(s),
        _ => Err(io::ErrorKind::InvalidInput.into()),
    }
}

fn key(id: SockId) -> io::Result<usize> {
    usize::try_from(id.0).map_err(|_| io::ErrorKind::InvalidInput.into())
}

/// Deregister, then drop (closing the fd). If deregistering fails, leak the fd
/// instead: closing a registered fd is what `Poller::add`'s contract forbids.
fn release(poller: &Poller, res: Resource) -> io::Result<()> {
    let result = poller.delete(res.fd());
    if result.is_err() {
        std::mem::forget(res);
    }
    result
}

/// Non-blocking syscalls can still be interrupted before doing anything.
fn retry<T>(mut f: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match f() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            r => return r,
        }
    }
}

/// Blocks the loop thread for the lookup: the one documented exception to
/// "never block but in `poll`" (§4.7).
fn resolve(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    Ok((host, port).to_socket_addrs()?.collect())
}
