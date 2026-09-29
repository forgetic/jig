//! `tcp::Conn` against a real reactor and a real peer: the arm widening that
//! keeps a client from deadlocking (§5.4, item 2). Scripted tests
//! (`tcp.rs`) cover the planning; this checks it against the kernel.
//!
//! The peer is a blocking std socket on its own thread. No test sleeps: the
//! loop polls with a generous timeout, and a poll that times out with
//! nothing to show is the deadlock this guards against.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use steploop::reactor::Reactor;
use steploop::sys::{Action, Event, Interest, SockId};
use steploop::tcp::Conn;

const S: SockId = SockId(1);
const LONG: Duration = Duration::from_secs(20);
/// Well over what the loopback socket buffers on both ends hold, so the
/// writes must block until the peer reads.
const BODY: usize = 32 << 20;
const HEAD: &[u8] = b"PUT /big HTTP/1.1\r\n\r\n";
const REPLY: &[u8] = b"got it all";

/// Drives a `Conn` the way `run` would: plan, perform, poll (without
/// waiting while `perform` reported something), and feed the events back.
struct Driver {
    reactor: Reactor,
    conn: Conn,
    events: Vec<Event>,
    /// An arm was accepted and neither its `Ready` nor a `Closed` has come.
    arm_pending: bool,
    /// Arms planned while one was pending, with the interest they asked.
    widenings: Vec<Interest>,
}

impl Driver {
    /// Plan and perform. Returns the actions planned.
    fn act(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        self.conn.plan(&mut actions);
        let planned = actions.clone();
        for action in &planned {
            if let Action::Arm { interest, .. } = action {
                if self.arm_pending {
                    self.widenings.push(*interest);
                }
                self.arm_pending = true;
            }
        }
        self.reactor.perform(&mut actions, &mut self.events);
        planned
    }

    /// Poll (without waiting if `perform` reported something) and feed the
    /// events to the `Conn`.
    fn wait(&mut self) {
        let wait = if self.events.is_empty() {
            LONG
        } else {
            Duration::ZERO
        };
        self.reactor.poll(&mut self.events, Some(wait)).unwrap();
        assert!(
            !self.events.is_empty(),
            "stuck: nothing happened for {LONG:?}"
        );
        for event in self.events.drain(..) {
            if matches!(event, Event::Ready { .. } | Event::Closed { .. }) {
                self.arm_pending = false;
            }
            self.conn.on_event(event);
        }
    }
}

/// A client waits for the reply (a read arm pending) and then sends a body
/// larger than the socket buffers to a peer that reads all of it before
/// replying. The body's writes block under the pending read arm; unless the
/// `Conn` widens that arm, it never hears the socket drain, and the two sides
/// wait on each other forever.
#[test]
fn a_blocked_write_under_a_pending_read_arm_widens_it() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (go, gate) = mpsc::channel::<()>();
    let peer = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(LONG)).unwrap();
        // Hold off reading until the client's writes have blocked, so they
        // surely do.
        gate.recv().unwrap();
        let mut got = vec![0; HEAD.len() + BODY];
        s.read_exact(&mut got).unwrap();
        s.write_all(REPLY).unwrap();
        got
    });

    let mut d = Driver {
        reactor: Reactor::new().unwrap(),
        conn: Conn::connect(S, addr),
        events: Vec::new(),
        arm_pending: false,
        widenings: Vec::new(),
    };
    d.conn.set_read_limit(1024);
    d.conn.send(HEAD.to_vec());
    // Connect, write the head, and find nothing to read: a read arm waits.
    loop {
        let planned = d.act();
        if planned.contains(&Action::Arm {
            sock: S,
            interest: Interest::READ,
        }) {
            break;
        }
        d.wait();
    }
    assert_eq!(d.conn.unsent(), 0, "the head is out");

    let body: Vec<u8> = (0..BODY).map(|i| (i % 251) as u8).collect();
    d.conn.send(body.clone());
    let mut gate_open = false;
    while !d.conn.eof() {
        let planned = d.act();
        let wants_write = planned
            .iter()
            .any(|a| matches!(a, Action::Arm { interest, .. } if interest.write));
        if wants_write && !gate_open {
            go.send(()).unwrap();
            gate_open = true;
        }
        d.wait();
    }
    assert_eq!(d.conn.inbound, REPLY);
    assert!(
        d.widenings.contains(&Interest::BOTH),
        "the read arm was widened: {:?}",
        d.widenings
    );

    let got = peer.join().unwrap();
    assert!(got[..HEAD.len()] == *HEAD);
    assert!(got[HEAD.len()..] == body[..], "the body arrived whole");

    d.conn.close();
    while !d.conn.is_closed() {
        d.act();
        d.wait();
    }
}
