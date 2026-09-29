//! The syscall vocabulary between the pure I/O step and the reactor:
//! [`Action`]s in, one [`Event`] per action out. See
//! `docs/explanation/sans-io-shell.md` §5.4.
//!
//! These are plain owned records so that they can cross the step boundary
//! (house rule 2), be recorded by the tap and be compared on replay. Buffers
//! move: a [`Action::Write`] hands its `Vec` to the reactor and the matching
//! [`Event::Wrote`] hands it back with the count written, so the planner keeps
//! ownership of any remainder without copying.
//!
//! **The contract** (owned by [`crate::reactor::Reactor`], checked by its
//! tests):
//!
//! 1. Every action completes with exactly one event naming the action's id.
//!    All but [`Action::Arm`] complete within the same `perform` call; a
//!    `WouldBlock` is an ordinary `Err` result.
//! 2. An `Arm` that is accepted completes later, with one [`Event::Ready`]
//!    from `poll`, or with the [`Event::Closed`] of its socket if the socket is
//!    closed first. At most one arm may be pending per socket: a second one is
//!    rejected at once with `Ready { result: Err(InvalidInput) }`, leaving the
//!    pending one in place. An arm with no interest is rejected the same way.
//! 3. An action naming an unknown [`SockId`] completes with a `NotFound`
//!    error, and one naming the wrong kind of resource (reading a listener,
//!    say) with `InvalidInput`. Nothing panics.
//! 4. `Write`'s buffer always comes back in `Wrote`, whatever the outcome.
//! 5. Signals are the reactor's own resources: it re-arms and drains them
//!    itself, and they never appear in actions.
//!
//! A resource exists from a successful `Accept` or `Connect` (or from
//! `Reactor::adopt_listener`) until its `Close`; a failed `Connect` leaves
//! nothing behind, while a failed `FinishConnect` leaves the socket in place
//! for the planner to close.

use std::fmt;
use std::io;
use std::net::SocketAddr;

/// Names one OS socket. Allocated by the pure side from a counter it owns and
/// never reused, so a late event for a forgotten id simply misses its lookup
/// (§4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SockId(pub u64);

/// Names one cross-thread signal. Allocated by the reactor
/// (`Reactor::signal`), since signals are created by the embedder at setup
/// time rather than planned by the pure side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignalId(pub u64);

/// Which readiness an [`Action::Arm`] waits for. The TCP stage merges its read
/// and write needs into one interest, since only one arm may be pending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Interest {
    pub read: bool,
    pub write: bool,
}

impl Interest {
    pub const READ: Interest = Interest {
        read: true,
        write: false,
    };
    pub const WRITE: Interest = Interest {
        read: false,
        write: true,
    };
    pub const BOTH: Interest = Interest {
        read: true,
        write: true,
    };

    /// Both interests combined.
    pub fn merge(self, other: Interest) -> Interest {
        Interest {
            read: self.read || other.read,
            write: self.write || other.write,
        }
    }

    /// Neither read nor write: arming this would never complete.
    pub fn is_empty(self) -> bool {
        !self.read && !self.write
    }
}

/// What an armed socket became ready for. It may report more than was asked:
/// a hang-up or a socket error sets both flags, and the next `Read` or
/// `Write` then reports what happened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Readiness {
    pub readable: bool,
    pub writable: bool,
}

/// How far a non-blocking connect got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Progress {
    /// Connected: the socket is a stream now.
    Done,
    /// Still connecting: arm write interest, then `FinishConnect`.
    InProgress,
}

/// A syscall to perform. Every variant names the id its event will carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Accept one connection from `listener` and name it `new`.
    Accept { listener: SockId, new: SockId },
    /// Start a non-blocking connect of a new socket named `sock`.
    Connect { sock: SockId, addr: SocketAddr },
    /// After writable readiness: check the connect's outcome (`SO_ERROR`).
    FinishConnect { sock: SockId },
    /// Read at most `max` bytes (`max` must be non-zero).
    Read { sock: SockId, max: usize },
    /// Write from `data`; the buffer comes back in [`Event::Wrote`].
    Write { sock: SockId, data: Vec<u8> },
    /// Ask for one readiness event (oneshot).
    Arm { sock: SockId, interest: Interest },
    /// Close the socket; completes a pending arm too.
    Close { sock: SockId },
    /// Resolve `host:port` with a blocking `getaddrinfo` (§4.7).
    Resolve { query: u64, host: String, port: u16 },
}

/// The outcome of an [`Action`], or a signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The peer's address on success.
    Accepted {
        listener: SockId,
        new: SockId,
        result: Result<SocketAddr, IoError>,
    },
    /// From both `Connect` and `FinishConnect`.
    Connected {
        sock: SockId,
        result: Result<Progress, IoError>,
    },
    /// `Ok` with an empty buffer is end of file.
    Read {
        sock: SockId,
        result: Result<Vec<u8>, IoError>,
    },
    /// The written buffer, whole, and how much of it went out.
    Wrote {
        sock: SockId,
        data: Vec<u8>,
        result: Result<usize, IoError>,
    },
    /// An arm completed (`Ok`) or was rejected (`Err`, at once).
    Ready {
        sock: SockId,
        result: Result<Readiness, IoError>,
    },
    /// The socket is gone (even on `Err`, which reports a failed
    /// deregistration or an unknown id). Also completes a pending arm.
    Closed {
        sock: SockId,
        result: Result<(), IoError>,
    },
    Resolved {
        query: u64,
        result: Result<Vec<SocketAddr>, IoError>,
    },
    /// The signal was raised at least once since the last `Signal` (raises
    /// coalesce), or every sender was dropped.
    Signal { signal: SignalId },
}

/// An `io::Error` reduced to comparable, cloneable data. `WouldBlock` is just
/// a kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IoError {
    pub kind: io::ErrorKind,
    /// The OS error number, when there was one.
    pub os: Option<i32>,
}

impl IoError {
    pub fn is_would_block(&self) -> bool {
        self.kind == io::ErrorKind::WouldBlock
    }
}

impl From<io::ErrorKind> for IoError {
    fn from(kind: io::ErrorKind) -> IoError {
        IoError { kind, os: None }
    }
}

impl From<&io::Error> for IoError {
    fn from(e: &io::Error) -> IoError {
        IoError {
            kind: e.kind(),
            os: e.raw_os_error(),
        }
    }
}

impl From<io::Error> for IoError {
    fn from(e: io::Error) -> IoError {
        IoError::from(&e)
    }
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.os {
            // std renders the OS message and number, e.g. "Connection
            // refused (os error 111)".
            Some(code) => io::Error::from_raw_os_error(code).fmt(f),
            None => self.kind.fmt(f),
        }
    }
}

impl std::error::Error for IoError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_keeps_kind_and_os_code() {
        let e = io::Error::from_raw_os_error(111);
        let ioe = IoError::from(&e);
        assert_eq!(ioe.os, Some(111));
        assert_eq!(ioe.kind, e.kind());
        assert_eq!(ioe.to_string(), e.to_string());
    }

    #[test]
    fn io_error_from_kind_displays_the_kind() {
        let ioe = IoError::from(io::ErrorKind::WouldBlock);
        assert!(ioe.is_would_block());
        assert_eq!(ioe.os, None);
        assert_eq!(ioe.to_string(), io::ErrorKind::WouldBlock.to_string());
        assert!(!IoError::from(io::ErrorKind::NotFound).is_would_block());
    }

    #[test]
    fn interest_merges() {
        assert_eq!(Interest::READ.merge(Interest::WRITE), Interest::BOTH);
        assert!(Interest::default().is_empty());
        assert!(!Interest::READ.is_empty());
    }
}
