//! An echo server driven by `run` over real sockets, then replayed from its
//! tap.
//!
//! The core and the I/O step are written by hand directly against
//! `Action`/`Event` (the real TCP planners come later), and kept small. The
//! host takes part too: it upper-cases each chunk, so its answers are inputs
//! the replay has to get from the tap. Clients are std sockets on their own
//! threads; the server stops on a signal.

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use steploop::reactor::Reactor;
use steploop::run::{Core, Host, IoStep, NoHost, NoTap, Tap, replay, run};
use steploop::sys::{Action, Event, Interest, SockId};
use steploop::time::Time;

const LISTENER: SockId = SockId(0);
const READ_MAX: usize = 16 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Comp {
    Data(SockId, Vec<u8>),
    Eof(SockId),
    Stop,
    /// The host's answer to `HostReq::Transform`.
    Transformed(SockId, Vec<u8>),
}

#[derive(Debug)]
enum IoReq {
    Send(SockId, Vec<u8>),
    Close(SockId),
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HostReq {
    Transform(SockId, Vec<u8>),
    Served(SockId),
}

/// Echo what the host transforms; close on EOF; shut down on `Stop`.
#[derive(Default)]
struct EchoCore {
    stopping: bool,
}

impl Core for EchoCore {
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
                Comp::Data(s, data) => host.push(HostReq::Transform(s, data)),
                Comp::Transformed(s, data) => io.push(IoReq::Send(s, data)),
                Comp::Eof(s) => {
                    io.push(IoReq::Close(s));
                    host.push(HostReq::Served(s));
                }
                Comp::Stop if !self.stopping => {
                    self.stopping = true;
                    io.push(IoReq::Shutdown);
                }
                Comp::Stop => {}
            }
        }
    }

    fn deadline(&self) -> Option<Time> {
        None
    }

    fn done(&self) -> bool {
        self.stopping
    }
}

/// Planner state for the listener. `busy`: an action is planned and its event
/// not yet reaped (with a synchronous `perform`, at most one round trip).
struct Listener {
    busy: bool,
    armed: bool,
    blocked: bool,
    close: bool,
}

#[derive(Default)]
struct Conn {
    /// Chunks to write, oldest first; a short write's remainder goes back in
    /// front.
    out: VecDeque<Vec<u8>>,
    busy: bool,
    armed: bool,
    read_blocked: bool,
    write_blocked: bool,
    eof: bool,
    close: bool,
}

struct EchoIo {
    listener: Option<Listener>,
    next_id: u64,
    conns: BTreeMap<SockId, Conn>,
    shutting_down: bool,
}

impl EchoIo {
    fn new() -> EchoIo {
        EchoIo {
            listener: Some(Listener {
                busy: false,
                armed: false,
                blocked: false,
                close: false,
            }),
            next_id: 1,
            conns: BTreeMap::new(),
            shutting_down: false,
        }
    }
}

impl IoStep for EchoIo {
    type Comp = Comp;
    type Req = IoReq;

    fn reap(&mut self, _now: Time, events: &mut Vec<Event>, comps: &mut Vec<Comp>) {
        for ev in events.drain(..) {
            match ev {
                Event::Accepted { new, result, .. } => {
                    let Some(l) = &mut self.listener else {
                        continue;
                    };
                    l.busy = false;
                    match result {
                        Ok(_) => {
                            let conn = Conn {
                                close: self.shutting_down,
                                ..Conn::default()
                            };
                            self.conns.insert(new, conn);
                        }
                        Err(e) if e.is_would_block() => l.blocked = true,
                        Err(_) => {} // e.g. reset before accept: try the next
                    }
                }
                Event::Ready {
                    sock,
                    result: Ok(_),
                } if sock == LISTENER => {
                    if let Some(l) = &mut self.listener {
                        l.armed = false;
                        l.blocked = false;
                    }
                }
                Event::Ready {
                    sock,
                    result: Ok(_),
                } => {
                    if let Some(c) = self.conns.get_mut(&sock) {
                        // Retry both directions; the syscalls tell the truth.
                        c.armed = false;
                        c.read_blocked = false;
                        c.write_blocked = false;
                    }
                }
                Event::Read { sock, result } => {
                    let Some(c) = self.conns.get_mut(&sock) else {
                        continue;
                    };
                    c.busy = false;
                    match result {
                        Ok(data) if !data.is_empty() => comps.push(Comp::Data(sock, data)),
                        Err(e) if e.is_would_block() => c.read_blocked = true,
                        Ok(_) | Err(_) => {
                            c.eof = true;
                            comps.push(Comp::Eof(sock));
                        }
                    }
                }
                Event::Wrote {
                    sock,
                    mut data,
                    result,
                } => {
                    let Some(c) = self.conns.get_mut(&sock) else {
                        continue;
                    };
                    c.busy = false;
                    match result {
                        Ok(n) => {
                            data.drain(..n.min(data.len()));
                        }
                        Err(e) if e.is_would_block() => c.write_blocked = true,
                        Err(_) => data.clear(), // the peer is gone
                    }
                    if !data.is_empty() {
                        c.out.push_front(data);
                    }
                }
                Event::Closed { sock, .. } => {
                    if sock == LISTENER {
                        self.listener = None;
                    }
                    self.conns.remove(&sock);
                }
                Event::Signal { .. } => comps.push(Comp::Stop),
                // Rejected arms (never planned here), connects, resolves.
                _ => {}
            }
        }
    }

    fn step(
        &mut self,
        _now: Time,
        reqs: &mut Vec<IoReq>,
        _comps: &mut Vec<Comp>,
        actions: &mut Vec<Action>,
    ) {
        for req in reqs.drain(..) {
            match req {
                IoReq::Send(s, data) => {
                    if let Some(c) = self.conns.get_mut(&s) {
                        c.out.push_back(data);
                    }
                }
                IoReq::Close(s) => {
                    if let Some(c) = self.conns.get_mut(&s) {
                        c.close = true;
                    }
                }
                IoReq::Shutdown => {
                    self.shutting_down = true;
                    if let Some(l) = &mut self.listener {
                        l.close = true;
                    }
                    for c in self.conns.values_mut() {
                        c.close = true;
                    }
                }
            }
        }

        if let Some(l) = self.listener.as_mut().filter(|l| !l.busy) {
            if l.close {
                // Completes a pending arm too.
                actions.push(Action::Close { sock: LISTENER });
                l.busy = true;
            } else if !l.blocked {
                let new = SockId(self.next_id);
                self.next_id += 1;
                actions.push(Action::Accept {
                    listener: LISTENER,
                    new,
                });
                l.busy = true;
            } else if !l.armed {
                actions.push(Action::Arm {
                    sock: LISTENER,
                    interest: Interest::READ,
                });
                l.armed = true;
            }
        }

        for (&sock, c) in &mut self.conns {
            if c.busy {
                continue;
            }
            let want_read = !c.eof && !c.close;
            let next = if c.write_blocked {
                None
            } else {
                c.out.pop_front()
            };
            if let Some(data) = next {
                actions.push(Action::Write { sock, data });
                c.busy = true;
            } else if want_read && !c.read_blocked {
                actions.push(Action::Read {
                    sock,
                    max: READ_MAX,
                });
                c.busy = true;
            } else if c.close && c.out.is_empty() {
                actions.push(Action::Close { sock });
                c.busy = true;
            } else if !c.armed {
                let interest = Interest {
                    read: want_read && c.read_blocked,
                    write: c.write_blocked && !c.out.is_empty(),
                };
                if !interest.is_empty() {
                    actions.push(Action::Arm { sock, interest });
                    c.armed = true;
                }
            }
        }
    }

    fn deadline(&self) -> Option<Time> {
        None
    }

    fn idle(&self) -> bool {
        self.listener.is_none() && self.conns.is_empty()
    }
}

/// Upper-cases chunks and remembers which connections were served.
#[derive(Default)]
struct EchoHost {
    served: Vec<SockId>,
}

impl Host<EchoCore> for EchoHost {
    fn handle(&mut self, _now: Time, reqs: &mut Vec<HostReq>, comps: &mut Vec<Comp>) {
        for req in reqs.drain(..) {
            match req {
                HostReq::Transform(s, data) => {
                    comps.push(Comp::Transformed(s, data.to_ascii_uppercase()))
                }
                HostReq::Served(s) => self.served.push(s),
            }
        }
    }
}

/// Connect, send `msg` in two writes, half-close, and read the echo to EOF.
/// A small receive buffer (set before connecting, so the window stays small)
/// makes the server's writes block.
fn client(
    addr: std::net::SocketAddr,
    msg: Vec<u8>,
    small_rcvbuf: bool,
) -> thread::JoinHandle<(Vec<u8>, Vec<u8>)> {
    thread::spawn(move || {
        let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
        if small_rcvbuf {
            s.set_recv_buffer_size(4096).unwrap();
        }
        s.connect(&addr.into()).unwrap();
        let mut c = TcpStream::from(s);
        c.set_read_timeout(Some(TIMEOUT)).unwrap();
        c.set_write_timeout(Some(TIMEOUT)).unwrap();
        let (a, b) = msg.split_at(msg.len() / 2);
        c.write_all(a).unwrap();
        c.write_all(b).unwrap();
        c.shutdown(Shutdown::Write).unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).unwrap();
        (msg, got)
    })
}

#[test]
fn echo_server_serves_several_connections_and_replays_exactly() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut reactor = Reactor::new().unwrap();
    reactor.adopt_listener(LISTENER, listener).unwrap();
    let (_, stop) = reactor.signal().unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut core, mut io, mut host, mut tap) = (
            EchoCore::default(),
            EchoIo::new(),
            EchoHost::default(),
            Tap::new(),
        );
        let result = run(&mut core, &mut io, &mut host, &mut reactor, &mut tap);
        let _ = done_tx.send(());
        (result, host.served, tap)
    });

    // Concurrent clients; the big one outruns the socket buffers, so the
    // server sees short writes and `WouldBlock` and has to arm.
    let mut msgs: Vec<Vec<u8>> = (0..8)
        .map(|i| {
            format!("hello from client {i}; ")
                .repeat(i + 1)
                .into_bytes()
        })
        .collect();
    msgs.push((0..4 << 20).map(|i| b"abcdefghij"[i % 10]).collect());
    let clients: Vec<_> = msgs
        .iter()
        .map(|m| client(addr, m.clone(), m.len() > 1 << 20))
        .collect();
    for c in clients {
        let (msg, got) = c.join().unwrap();
        assert!(
            got == msg.to_ascii_uppercase(),
            "echo mismatch ({} bytes sent, {} back)",
            msg.len(),
            got.len()
        );
    }

    stop.raise().unwrap();
    done_rx
        .recv_timeout(TIMEOUT)
        .expect("the loop ends after the stop signal");
    let (result, served, tap) = server.join().unwrap();
    result.unwrap();
    assert_eq!(served.len(), msgs.len());

    let events = || tap.iterations.iter().flat_map(|it| &it.events);
    assert!(events().any(|e| matches!(e, Event::Signal { .. })));
    assert!(
        tap.iterations
            .iter()
            .any(|it| it.answers.iter().any(|a| !a.is_empty())),
        "the host's answers are recorded"
    );
    assert!(
        events().any(|e| matches!(e, Event::Wrote { result: Err(e), .. } if e.is_would_block())),
        "the big client made a write block"
    );
    assert!(
        tap.outputs()
            .iter()
            .flat_map(|o| &o.actions)
            .any(|a| matches!(a, Action::Arm { interest, .. } if interest.write)),
        "and the server armed for writing"
    );

    // Fresh steps, the recorded inputs, no reactor and no host: the same
    // outputs, byte for byte.
    let replayed = replay(&mut EchoCore::default(), &mut EchoIo::new(), &tap);
    assert_eq!(replayed.len(), tap.iterations.len());
    assert!(replayed == tap.outputs(), "replay diverged");

    // Replay is sensitive to its inputs: a different host answer shows.
    let mut tampered = tap.clone();
    let answer = tampered
        .iterations
        .iter_mut()
        .flat_map(|it| it.answers.iter_mut().flatten())
        .find_map(|c| match c {
            Comp::Transformed(_, data) => Some(data),
            _ => None,
        })
        .unwrap();
    answer.push(b'!');
    assert!(replay(&mut EchoCore::default(), &mut EchoIo::new(), &tampered) != tap.outputs());
}

#[test]
fn stop_before_any_client_with_no_host_and_no_tap() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut reactor = Reactor::new().unwrap();
    reactor.adopt_listener(LISTENER, listener).unwrap();
    let (_, stop) = reactor.signal().unwrap();
    // Raised before the loop starts: the first poll sees it.
    stop.raise().unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let result = run(
            &mut EchoCore::default(),
            &mut EchoIo::new(),
            &mut NoHost,
            &mut reactor,
            &mut NoTap,
        );
        let _ = done_tx.send(result);
    });
    done_rx
        .recv_timeout(TIMEOUT)
        .expect("the loop ends")
        .unwrap();
}
