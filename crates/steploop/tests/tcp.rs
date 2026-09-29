//! Scripted-event tests for `steploop::tcp`: the test plays the reactor,
//! answering each planned action with the event it chooses, and checks what
//! the stage plans next. No sockets, threads or clocks.

use std::io::ErrorKind;
use std::net::SocketAddr;

use steploop::sys::{Action, Event, Ids, Interest, IoError, Progress, Readiness, SockId};
use steploop::tcp::{ACCEPT_BACKOFF, Conn, Listener, READ_CHUNK};
use steploop::time::Time;

const S: SockId = SockId(5);
const OTHER: SockId = SockId(6);

fn plan(c: &mut Conn) -> Vec<Action> {
    let mut actions = Vec::new();
    c.plan(&mut actions);
    actions
}

fn err(kind: ErrorKind) -> IoError {
    IoError::from(kind)
}

fn read(data: &[u8]) -> Event {
    Event::Read {
        sock: S,
        result: Ok(data.to_vec()),
    }
}

fn read_err(kind: ErrorKind) -> Event {
    Event::Read {
        sock: S,
        result: Err(err(kind)),
    }
}

fn wrote(data: &[u8], result: Result<usize, IoError>) -> Event {
    Event::Wrote {
        sock: S,
        data: data.to_vec(),
        result,
    }
}

fn ready(readable: bool, writable: bool) -> Event {
    Event::Ready {
        sock: S,
        result: Ok(Readiness { readable, writable }),
    }
}

fn closed() -> Event {
    Event::Closed {
        sock: S,
        result: Ok(()),
    }
}

fn read_action(max: usize) -> Action {
    Action::Read { sock: S, max }
}

fn write_action(data: &[u8]) -> Action {
    Action::Write {
        sock: S,
        data: data.to_vec(),
    }
}

fn arm(interest: Interest) -> Action {
    Action::Arm { sock: S, interest }
}

fn open(limit: usize) -> Conn {
    let mut c = Conn::new(S);
    c.set_read_limit(limit);
    c
}

// ---------------------------------------------------------------------------
// Conn: reads
// ---------------------------------------------------------------------------

#[test]
fn reads_nothing_until_a_limit_is_set() {
    let mut c = Conn::new(S);
    assert_eq!(plan(&mut c), []);
    c.set_read_limit(10);
    assert_eq!(plan(&mut c), [read_action(10)]);
}

#[test]
fn reads_up_to_the_limit_and_resumes_when_consumed() {
    let mut c = open(10);
    assert_eq!(plan(&mut c), [read_action(10)]);
    assert_eq!(plan(&mut c), [], "one read in flight at a time");
    c.on_event(read(b"abcd"));
    assert_eq!(c.available(), 4);
    assert_eq!(plan(&mut c), [read_action(6)], "only what the limit leaves");
    c.on_event(read(b"efghij"));
    assert_eq!(c.inbound, b"abcdefghij");
    assert_eq!(plan(&mut c), [], "full: backpressure");
    c.inbound.drain(..3);
    assert_eq!(plan(&mut c), [read_action(3)]);
}

#[test]
fn a_read_never_asks_for_more_than_a_chunk() {
    let mut c = open(usize::MAX);
    assert_eq!(plan(&mut c), [read_action(READ_CHUNK)]);
}

#[test]
fn would_block_arms_read_and_readiness_resumes() {
    let mut c = open(100);
    plan(&mut c);
    c.on_event(read_err(ErrorKind::WouldBlock));
    assert_eq!(plan(&mut c), [arm(Interest::READ)]);
    assert_eq!(plan(&mut c), [], "exactly one arm");
    c.on_event(ready(true, false));
    assert_eq!(plan(&mut c), [read_action(100)]);
}

#[test]
fn no_arm_for_reads_nobody_wants() {
    let mut c = open(100);
    plan(&mut c);
    c.on_event(read_err(ErrorKind::WouldBlock));
    c.set_read_limit(0);
    assert_eq!(plan(&mut c), []);
    c.set_read_limit(1);
    assert_eq!(plan(&mut c), [arm(Interest::READ)], "still blocked: arm");
}

#[test]
fn eof_stops_reading_but_not_writing() {
    let mut c = open(100);
    plan(&mut c);
    c.on_event(read(b"hi"));
    plan(&mut c);
    c.on_event(read(b""));
    assert!(c.eof());
    assert_eq!(c.inbound, b"hi", "what was read stays");
    c.send(b"bye".to_vec());
    assert_eq!(plan(&mut c), [write_action(b"bye")]);
}

#[test]
fn a_read_error_stops_everything_until_closed() {
    let mut c = open(100);
    plan(&mut c);
    c.send(b"queued".to_vec());
    c.on_event(read_err(ErrorKind::ConnectionReset));
    assert_eq!(c.error(), Some(err(ErrorKind::ConnectionReset)));
    assert_eq!(c.unsent(), 0, "unsendable bytes are dropped");
    assert_eq!(plan(&mut c), []);
    c.close();
    assert_eq!(plan(&mut c), [Action::Close { sock: S }]);
}

// ---------------------------------------------------------------------------
// Conn: writes
// ---------------------------------------------------------------------------

#[test]
fn short_writes_keep_the_tail_in_front_of_newer_bytes() {
    let mut c = Conn::new(S);
    c.send(b"0123456789".to_vec());
    assert_eq!(plan(&mut c), [write_action(b"0123456789")]);
    assert_eq!(c.unsent(), 10, "the write in flight counts");
    c.send(b"AB".to_vec());
    assert_eq!(plan(&mut c), [], "one write in flight at a time");
    assert_eq!(c.unsent(), 12);
    c.on_event(wrote(b"0123456789", Ok(4)));
    assert_eq!(c.unsent(), 8);
    assert_eq!(plan(&mut c), [write_action(b"456789AB")]);
    c.on_event(wrote(b"456789AB", Ok(8)));
    assert_eq!(c.unsent(), 0);
    assert_eq!(plan(&mut c), []);
}

#[test]
fn write_would_block_arms_write_and_keeps_the_bytes() {
    let mut c = Conn::new(S);
    c.send(b"data".to_vec());
    plan(&mut c);
    c.on_event(wrote(b"data", Err(err(ErrorKind::WouldBlock))));
    assert_eq!(c.unsent(), 4);
    assert_eq!(plan(&mut c), [arm(Interest::WRITE)]);
    c.on_event(ready(false, true));
    assert_eq!(plan(&mut c), [write_action(b"data")]);
}

#[test]
fn a_write_error_drops_the_bytes() {
    let mut c = Conn::new(S);
    c.send(b"data".to_vec());
    plan(&mut c);
    c.on_event(wrote(b"data", Err(err(ErrorKind::BrokenPipe))));
    assert_eq!(c.error(), Some(err(ErrorKind::BrokenPipe)));
    assert_eq!(c.unsent(), 0);
    c.send(b"more".to_vec());
    assert_eq!(c.unsent(), 0, "sending after a failure is dropped");
    assert_eq!(plan(&mut c), []);
}

// ---------------------------------------------------------------------------
// Conn: arms
// ---------------------------------------------------------------------------

#[test]
fn reads_and_writes_share_one_merged_arm() {
    let mut c = open(100);
    c.send(b"out".to_vec());
    assert_eq!(plan(&mut c), [write_action(b"out"), read_action(100)]);
    assert_eq!(plan(&mut c), [], "no arm while syscalls are in flight");
    c.on_event(wrote(b"out", Err(err(ErrorKind::WouldBlock))));
    assert_eq!(plan(&mut c), [], "the read is still in flight");
    c.on_event(read_err(ErrorKind::WouldBlock));
    assert_eq!(plan(&mut c), [arm(Interest::BOTH)]);
    assert_eq!(plan(&mut c), []);
}

#[test]
fn readiness_clears_only_what_it_reports() {
    let mut c = open(100);
    c.send(b"out".to_vec());
    plan(&mut c);
    c.on_event(wrote(b"out", Err(err(ErrorKind::WouldBlock))));
    c.on_event(read_err(ErrorKind::WouldBlock));
    plan(&mut c);
    c.on_event(ready(true, false));
    assert_eq!(plan(&mut c), [read_action(100)]);
    c.on_event(read_err(ErrorKind::WouldBlock));
    assert_eq!(plan(&mut c), [arm(Interest::BOTH)], "write still blocked");
}

#[test]
fn readiness_may_report_more_than_was_asked() {
    let mut c = open(100);
    c.send(b"x".to_vec());
    plan(&mut c);
    c.on_event(wrote(b"x", Err(err(ErrorKind::WouldBlock))));
    c.on_event(read_err(ErrorKind::WouldBlock));
    plan(&mut c);
    // A hang-up reports both.
    c.on_event(ready(true, true));
    assert_eq!(plan(&mut c), [write_action(b"x"), read_action(100)]);
}

#[test]
fn a_need_arising_under_a_pending_arm_waits_for_it() {
    let mut c = open(100);
    plan(&mut c);
    c.on_event(read_err(ErrorKind::WouldBlock));
    assert_eq!(plan(&mut c), [arm(Interest::READ)]);
    c.send(b"late".to_vec());
    assert_eq!(plan(&mut c), [write_action(b"late")], "writes don't wait");
    c.on_event(wrote(b"late", Err(err(ErrorKind::WouldBlock))));
    assert_eq!(plan(&mut c), [], "no second arm while one is pending");
    c.on_event(ready(true, false));
    assert_eq!(plan(&mut c), [read_action(100)]);
    c.on_event(read_err(ErrorKind::WouldBlock));
    assert_eq!(plan(&mut c), [arm(Interest::BOTH)]);
}

#[test]
fn a_rejected_arm_is_an_error() {
    let mut c = open(100);
    plan(&mut c);
    c.on_event(read_err(ErrorKind::WouldBlock));
    plan(&mut c);
    c.on_event(Event::Ready {
        sock: S,
        result: Err(err(ErrorKind::InvalidInput)),
    });
    assert_eq!(c.error(), Some(err(ErrorKind::InvalidInput)));
    assert_eq!(plan(&mut c), []);
}

// ---------------------------------------------------------------------------
// Conn: close
// ---------------------------------------------------------------------------

#[test]
fn close_does_not_wait_for_a_pending_arm() {
    let mut c = open(100);
    plan(&mut c);
    c.on_event(read_err(ErrorKind::WouldBlock));
    assert_eq!(plan(&mut c), [arm(Interest::READ)]);
    c.close();
    assert_eq!(plan(&mut c), [Action::Close { sock: S }]);
    assert_eq!(plan(&mut c), [], "one close");
    assert!(!c.is_closed());
    // The `Closed` completes the arm too.
    c.on_event(closed());
    assert!(c.is_closed());
    c.on_event(ready(true, false));
    assert_eq!(plan(&mut c), []);
}

#[test]
fn close_waits_one_round_for_syscalls_in_flight() {
    let mut c = open(100);
    c.send(b"out".to_vec());
    plan(&mut c);
    c.close();
    assert_eq!(c.unsent(), 3, "the write in flight is still out");
    assert_eq!(plan(&mut c), [], "read and write in flight");
    c.on_event(wrote(b"out", Ok(1)));
    c.on_event(read(b"in"));
    assert_eq!(c.unsent(), 0, "a closing socket keeps no tail");
    assert_eq!(plan(&mut c), [Action::Close { sock: S }]);
    c.on_event(closed());
    assert!(c.is_closed());
    assert_eq!(plan(&mut c), []);
}

#[test]
fn close_drops_queued_bytes_and_later_sends() {
    let mut c = Conn::new(S);
    c.send(b"never".to_vec());
    c.close();
    c.send(b"too late".to_vec());
    assert_eq!(c.unsent(), 0);
    assert_eq!(plan(&mut c), [Action::Close { sock: S }]);
}

// ---------------------------------------------------------------------------
// Conn: stale events
// ---------------------------------------------------------------------------

#[test]
fn events_for_other_sockets_or_nothing_in_flight_are_ignored() {
    let mut c = open(100);
    c.on_event(read(b"unasked"));
    c.on_event(wrote(b"unasked", Ok(7)));
    c.on_event(ready(true, true));
    c.on_event(Event::Connected {
        sock: S,
        result: Ok(Progress::Done),
    });
    assert_eq!(c.available(), 0);
    assert_eq!(plan(&mut c), [read_action(100)]);
    c.on_event(Event::Read {
        sock: OTHER,
        result: Ok(b"not mine".to_vec()),
    });
    c.on_event(Event::Closed {
        sock: OTHER,
        result: Ok(()),
    });
    assert_eq!(plan(&mut c), [], "our read is still in flight");
    assert!(!c.is_closed());
    c.on_event(read(b"mine"));
    assert_eq!(c.inbound, b"mine");
}

// ---------------------------------------------------------------------------
// Conn: connecting
// ---------------------------------------------------------------------------

fn addr() -> SocketAddr {
    "127.0.0.1:9".parse().unwrap()
}

fn connected(result: Result<Progress, IoError>) -> Event {
    Event::Connected { sock: S, result }
}

#[test]
fn connect_in_progress_arms_write_then_finishes() {
    let mut c = Conn::connect(S, addr());
    c.set_read_limit(100);
    assert!(c.is_connecting());
    assert_eq!(
        plan(&mut c),
        [Action::Connect {
            sock: S,
            addr: addr()
        }]
    );
    assert_eq!(plan(&mut c), []);
    c.on_event(connected(Ok(Progress::InProgress)));
    assert_eq!(plan(&mut c), [arm(Interest::WRITE)]);
    c.on_event(ready(false, true));
    assert_eq!(plan(&mut c), [Action::FinishConnect { sock: S }]);
    c.on_event(connected(Ok(Progress::Done)));
    assert!(c.is_open());
    c.send(b"GET".to_vec());
    assert_eq!(plan(&mut c), [write_action(b"GET"), read_action(100)]);
}

#[test]
fn a_spurious_wake_while_connecting_arms_again() {
    let mut c = Conn::connect(S, addr());
    plan(&mut c);
    c.on_event(connected(Ok(Progress::InProgress)));
    plan(&mut c);
    c.on_event(ready(false, true));
    plan(&mut c);
    c.on_event(connected(Ok(Progress::InProgress)));
    assert_eq!(plan(&mut c), [arm(Interest::WRITE)]);
}

#[test]
fn a_failed_connect_leaves_nothing_to_close() {
    let mut c = Conn::connect(S, addr());
    plan(&mut c);
    c.on_event(connected(Err(err(ErrorKind::ConnectionRefused))));
    assert!(c.is_closed());
    assert_eq!(c.error(), Some(err(ErrorKind::ConnectionRefused)));
    c.close();
    assert_eq!(plan(&mut c), []);
}

#[test]
fn a_failed_finish_leaves_a_socket_to_close() {
    let mut c = Conn::connect(S, addr());
    plan(&mut c);
    c.on_event(connected(Ok(Progress::InProgress)));
    plan(&mut c);
    c.on_event(ready(true, true));
    plan(&mut c);
    c.on_event(connected(Err(err(ErrorKind::ConnectionRefused))));
    assert!(!c.is_closed() && !c.is_open());
    assert_eq!(c.error(), Some(err(ErrorKind::ConnectionRefused)));
    assert_eq!(plan(&mut c), []);
    c.close();
    assert_eq!(plan(&mut c), [Action::Close { sock: S }]);
}

#[test]
fn closing_before_the_connect_is_planned_needs_no_syscall() {
    let mut c = Conn::connect(S, addr());
    c.close();
    assert!(c.is_closed());
    assert_eq!(plan(&mut c), []);
}

#[test]
fn closing_mid_connect_waits_for_the_connect() {
    let mut c = Conn::connect(S, addr());
    plan(&mut c);
    c.close();
    assert_eq!(plan(&mut c), []);
    c.on_event(connected(Ok(Progress::Done)));
    assert!(!c.is_open(), "closing wins");
    assert_eq!(plan(&mut c), [Action::Close { sock: S }]);

    let mut c = Conn::connect(S, addr());
    plan(&mut c);
    c.close();
    c.on_event(connected(Err(err(ErrorKind::ConnectionRefused))));
    assert!(c.is_closed(), "no socket, no close");
    assert_eq!(plan(&mut c), []);
}

// ---------------------------------------------------------------------------
// Listener
// ---------------------------------------------------------------------------

const L: SockId = SockId(1);

fn lplan(l: &mut Listener, now: Time, open: usize, ids: &mut Ids) -> Vec<Action> {
    let mut actions = Vec::new();
    l.plan(now, open, ids, &mut actions);
    actions
}

fn accepted(new: u64, result: Result<SocketAddr, IoError>) -> Event {
    Event::Accepted {
        listener: L,
        new: SockId(new),
        result,
    }
}

fn accept(new: u64) -> Action {
    Action::Accept {
        listener: L,
        new: SockId(new),
    }
}

fn lready() -> Event {
    Event::Ready {
        sock: L,
        result: Ok(Readiness {
            readable: true,
            writable: false,
        }),
    }
}

const T0: Time = Time(1_000);

/// An allocator whose next id is 2, the listener having taken 1.
fn ids() -> Ids {
    let mut ids = Ids::new();
    assert_eq!(ids.next_sock(), L);
    ids
}

#[test]
fn accepts_until_would_block_then_arms() {
    let mut ids = ids();
    let mut l = Listener::new(L, 10);
    assert_eq!(lplan(&mut l, T0, 0, &mut ids), [accept(2)]);
    assert_eq!(lplan(&mut l, T0, 0, &mut ids), [], "one accept in flight");
    assert_eq!(l.on_event(T0, accepted(2, Ok(addr()))), Some(SockId(2)));
    assert_eq!(lplan(&mut l, T0, 1, &mut ids), [accept(3)]);
    assert_eq!(
        l.on_event(T0, accepted(3, Err(err(ErrorKind::WouldBlock)))),
        None
    );
    let arm_read = Action::Arm {
        sock: L,
        interest: Interest::READ,
    };
    assert_eq!(lplan(&mut l, T0, 1, &mut ids), [arm_read]);
    assert_eq!(lplan(&mut l, T0, 1, &mut ids), []);
    assert_eq!(l.on_event(T0, lready()), None);
    assert_eq!(
        lplan(&mut l, T0, 1, &mut ids),
        [accept(4)],
        "ids never reused"
    );
}

#[test]
fn stops_at_the_cap_and_resumes_below_it() {
    let mut ids = ids();
    let mut l = Listener::new(L, 2);
    assert_eq!(lplan(&mut l, T0, 2, &mut ids), [], "at the cap");
    assert_eq!(lplan(&mut l, T0, 1, &mut ids), [accept(2)]);
    l.on_event(T0, accepted(2, Err(err(ErrorKind::WouldBlock))));
    assert_eq!(
        lplan(&mut l, T0, 2, &mut ids),
        [],
        "no arm at the cap either"
    );
    let arm_read = Action::Arm {
        sock: L,
        interest: Interest::READ,
    };
    assert_eq!(lplan(&mut l, T0, 1, &mut ids), [arm_read]);
    // Readiness at the cap waits for room without re-arming.
    l.on_event(T0, lready());
    assert_eq!(lplan(&mut l, T0, 2, &mut ids), []);
    assert_eq!(lplan(&mut l, T0, 0, &mut ids), [accept(3)]);
}

#[test]
fn a_failed_accept_backs_off() {
    let mut ids = ids();
    let mut l = Listener::new(L, 10);
    lplan(&mut l, T0, 0, &mut ids);
    l.on_event(T0, accepted(2, Err(err(ErrorKind::ConnectionAborted))));
    let until = T0.after(ACCEPT_BACKOFF);
    assert_eq!(l.deadline(), Some(until));
    assert_eq!(lplan(&mut l, T0, 0, &mut ids), []);
    assert_eq!(lplan(&mut l, Time(until.0 - 1), 0, &mut ids), []);
    assert_eq!(lplan(&mut l, until, 0, &mut ids), [accept(3)]);
    assert_eq!(l.deadline(), None);
    // A persistent error keeps backing off rather than spinning.
    l.on_event(until, accepted(3, Err(err(ErrorKind::Other))));
    assert_eq!(l.deadline(), Some(until.after(ACCEPT_BACKOFF)));
}

#[test]
fn closes_at_once_under_a_pending_arm() {
    let mut ids = ids();
    let mut l = Listener::new(L, 10);
    lplan(&mut l, T0, 0, &mut ids);
    l.on_event(T0, accepted(2, Err(err(ErrorKind::WouldBlock))));
    lplan(&mut l, T0, 0, &mut ids);
    l.close();
    assert_eq!(lplan(&mut l, T0, 0, &mut ids), [Action::Close { sock: L }]);
    assert_eq!(lplan(&mut l, T0, 0, &mut ids), []);
    l.on_event(
        T0,
        Event::Closed {
            sock: L,
            result: Ok(()),
        },
    );
    assert!(l.is_closed());
    assert_eq!(l.on_event(T0, lready()), None, "stale");
    assert_eq!(lplan(&mut l, T0, 0, &mut ids), []);
}

#[test]
fn closing_waits_for_an_accept_in_flight_and_hands_over_its_socket() {
    let mut ids = ids();
    let mut l = Listener::new(L, 10);
    lplan(&mut l, T0, 0, &mut ids);
    l.close();
    assert_eq!(lplan(&mut l, T0, 0, &mut ids), []);
    assert_eq!(l.on_event(T0, accepted(2, Ok(addr()))), Some(SockId(2)));
    assert_eq!(lplan(&mut l, T0, 1, &mut ids), [Action::Close { sock: L }]);
}

#[test]
fn the_listener_ignores_other_and_stale_events() {
    let mut ids = ids();
    let mut l = Listener::new(L, 10);
    assert_eq!(l.on_event(T0, accepted(9, Ok(addr()))), None, "none asked");
    assert_eq!(l.on_event(T0, lready()), None);
    let other = Event::Accepted {
        listener: OTHER,
        new: SockId(2),
        result: Ok(addr()),
    };
    lplan(&mut l, T0, 0, &mut ids);
    assert_eq!(l.on_event(T0, other), None);
    assert_eq!(
        lplan(&mut l, T0, 0, &mut ids),
        [],
        "ours is still in flight"
    );
}
