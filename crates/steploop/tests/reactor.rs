//! The reactor's contract (`steploop::sys`, items 1–5) over real loopback
//! sockets on one thread. A fake here would only encode our own assumptions
//! about epoll; real sockets check the real semantics (§4.2).
//!
//! Waits use `poll` with a generous timeout, never sleeps; the short polls
//! assert that nothing more arrives.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use steploop::reactor::Reactor;
use steploop::sys::{Action, Event, Interest, IoError, Progress, Readiness, SockId};

const L: SockId = SockId(1);
const LONG: Duration = Duration::from_secs(10);
const SHORT: Duration = Duration::from_millis(50);

/// Perform one action and return its events (exactly one, unless it is an
/// accepted `Arm`).
fn perform(r: &mut Reactor, action: Action) -> Vec<Event> {
    let mut actions = vec![action];
    let mut events = Vec::new();
    r.perform(&mut actions, &mut events);
    assert!(actions.is_empty(), "perform drains its actions");
    events
}

fn perform1(r: &mut Reactor, action: Action) -> Event {
    let mut events = perform(r, action);
    assert_eq!(events.len(), 1, "one event per action: {events:?}");
    events.remove(0)
}

fn poll(r: &mut Reactor, timeout: Duration) -> Vec<Event> {
    let mut events = Vec::new();
    r.poll(&mut events, Some(timeout)).unwrap();
    events
}

fn err(kind: ErrorKind) -> IoError {
    IoError::from(kind)
}

fn fail<T>(kind: ErrorKind) -> Result<T, IoError> {
    Err(err(kind))
}

/// As the kernel reports it: the errno is kept.
fn would_block<T>() -> Result<T, IoError> {
    Err(IoError::from(&std::io::Error::from_raw_os_error(
        libc::EAGAIN,
    )))
}

fn listener(r: &mut Reactor) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    r.adopt_listener(L, l).unwrap();
    addr
}

fn client(addr: SocketAddr) -> TcpStream {
    let c = TcpStream::connect(addr).unwrap();
    c.set_read_timeout(Some(LONG)).unwrap();
    c
}

/// A client connected to the reactor's listener, accepted as `id`.
fn accepted(r: &mut Reactor, addr: SocketAddr, id: SockId) -> TcpStream {
    let c = client(addr);
    // The connection is in the backlog once `connect` returns, but readiness
    // is how a planner learns that; go through it.
    assert!(
        perform(
            r,
            Action::Arm {
                sock: L,
                interest: Interest::READ
            }
        )
        .is_empty()
    );
    assert_eq!(poll(r, LONG).len(), 1);
    let ev = perform1(
        r,
        Action::Accept {
            listener: L,
            new: id,
        },
    );
    assert!(
        matches!(ev, Event::Accepted { result: Ok(_), .. }),
        "{ev:?}"
    );
    c
}

fn ready(sock: SockId, readable: bool, writable: bool) -> Event {
    Event::Ready {
        sock,
        result: Ok(Readiness { readable, writable }),
    }
}

#[test]
fn accept_would_block_then_arm_ready_accepted() {
    let mut r = Reactor::new().unwrap();
    let addr = listener(&mut r);
    let new = SockId(2);

    let ev = perform1(&mut r, Action::Accept { listener: L, new });
    assert_eq!(
        ev,
        Event::Accepted {
            listener: L,
            new,
            result: would_block()
        }
    );
    // An accepted arm completes later, from `poll`.
    assert!(
        perform(
            &mut r,
            Action::Arm {
                sock: L,
                interest: Interest::READ
            }
        )
        .is_empty()
    );
    assert!(poll(&mut r, SHORT).is_empty(), "nothing to accept yet");

    let c = client(addr);
    assert_eq!(poll(&mut r, LONG), vec![ready(L, true, false)]);
    let ev = perform1(&mut r, Action::Accept { listener: L, new });
    assert_eq!(
        ev,
        Event::Accepted {
            listener: L,
            new,
            result: Ok(c.local_addr().unwrap())
        }
    );

    // The accepted stream is usable, and its id is taken now.
    let ev = perform1(&mut r, Action::Accept { listener: L, new });
    assert_eq!(
        ev,
        Event::Accepted {
            listener: L,
            new,
            result: Err(err(ErrorKind::AlreadyExists))
        }
    );
}

#[test]
fn read_write_and_eof() {
    let mut r = Reactor::new().unwrap();
    let addr = listener(&mut r);
    let s = SockId(2);
    let mut c = accepted(&mut r, addr, s);

    let ev = perform1(&mut r, Action::Read { sock: s, max: 1024 });
    assert_eq!(
        ev,
        Event::Read {
            sock: s,
            result: would_block()
        }
    );

    c.write_all(b"ping").unwrap();
    assert!(
        perform(
            &mut r,
            Action::Arm {
                sock: s,
                interest: Interest::READ
            }
        )
        .is_empty()
    );
    assert_eq!(poll(&mut r, LONG), vec![ready(s, true, false)]);
    let ev = perform1(&mut r, Action::Read { sock: s, max: 1024 });
    assert_eq!(
        ev,
        Event::Read {
            sock: s,
            result: Ok(b"ping".to_vec())
        }
    );

    let ev = perform1(
        &mut r,
        Action::Write {
            sock: s,
            data: b"pong".to_vec(),
        },
    );
    assert_eq!(
        ev,
        Event::Wrote {
            sock: s,
            data: b"pong".to_vec(),
            result: Ok(4)
        }
    );
    let mut buf = [0u8; 4];
    c.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"pong");

    c.shutdown(Shutdown::Write).unwrap();
    assert!(
        perform(
            &mut r,
            Action::Arm {
                sock: s,
                interest: Interest::READ
            }
        )
        .is_empty()
    );
    assert_eq!(poll(&mut r, LONG).len(), 1);
    let ev = perform1(&mut r, Action::Read { sock: s, max: 1024 });
    assert_eq!(
        ev,
        Event::Read {
            sock: s,
            result: Ok(Vec::new())
        },
        "EOF"
    );

    // A zero-byte read would be indistinguishable from EOF.
    let ev = perform1(&mut r, Action::Read { sock: s, max: 0 });
    assert_eq!(
        ev,
        Event::Read {
            sock: s,
            result: Err(err(ErrorKind::InvalidInput))
        }
    );

    let ev = perform1(&mut r, Action::Close { sock: s });
    assert_eq!(
        ev,
        Event::Closed {
            sock: s,
            result: Ok(())
        }
    );
    let mut rest = Vec::new();
    c.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty(), "the client sees the close");
}

#[test]
fn write_returns_its_buffer_and_the_count() {
    let mut r = Reactor::new().unwrap();
    let addr = listener(&mut r);
    let s = SockId(2);
    let _c = accepted(&mut r, addr, s); // never reads

    // Larger than any loopback socket buffer: the write must be short.
    let big: Vec<u8> = (0..32 << 20).map(|i| i as u8).collect();
    let Event::Wrote {
        sock,
        data,
        result: Ok(n),
    } = perform1(
        &mut r,
        Action::Write {
            sock: s,
            data: big.clone(),
        },
    )
    else {
        panic!("expected a successful write");
    };
    assert_eq!(sock, s);
    assert_eq!(data, big, "the buffer comes back whole");
    assert!(n > 0 && n < big.len(), "short write: {n}");

    // The peer isn't reading, so the remainder eventually would block; the
    // buffer still comes back.
    let mut rest = data[n..].to_vec();
    loop {
        match perform1(
            &mut r,
            Action::Write {
                sock: s,
                data: rest,
            },
        ) {
            Event::Wrote {
                data,
                result: Ok(n),
                ..
            } => rest = data[n..].to_vec(),
            Event::Wrote {
                data,
                result: Err(e),
                ..
            } => {
                assert!(e.is_would_block(), "{e}");
                assert!(!data.is_empty());
                break;
            }
            ev => panic!("unexpected {ev:?}"),
        }
    }
    // Writable interest then completes once the peer drains... which it
    // never does; the arm stays pending until the close completes it.
    assert!(
        perform(
            &mut r,
            Action::Arm {
                sock: s,
                interest: Interest::WRITE
            }
        )
        .is_empty()
    );
    assert!(poll(&mut r, SHORT).is_empty());
    assert_eq!(
        perform1(&mut r, Action::Close { sock: s }),
        Event::Closed {
            sock: s,
            result: Ok(())
        }
    );

    // Unknown id: the buffer still comes back.
    let ev = perform1(
        &mut r,
        Action::Write {
            sock: s,
            data: b"x".to_vec(),
        },
    );
    assert_eq!(
        ev,
        Event::Wrote {
            sock: s,
            data: b"x".to_vec(),
            result: Err(err(ErrorKind::NotFound))
        }
    );
}

#[test]
fn oneshot_one_ready_per_arm() {
    let mut r = Reactor::new().unwrap();
    let addr = listener(&mut r);
    let s = SockId(2);
    let mut c = accepted(&mut r, addr, s);

    // An arm that could never complete is rejected at once.
    let ev = perform1(
        &mut r,
        Action::Arm {
            sock: s,
            interest: Interest::default(),
        },
    );
    assert_eq!(
        ev,
        Event::Ready {
            sock: s,
            result: Err(err(ErrorKind::InvalidInput))
        }
    );
    assert!(
        perform(
            &mut r,
            Action::Arm {
                sock: s,
                interest: Interest::READ
            }
        )
        .is_empty()
    );
    // Under a pending arm, one that adds nothing is a no-op (even an empty
    // one): no event now, and still one `Ready` later.
    for interest in [Interest::READ, Interest::default()] {
        assert!(perform(&mut r, Action::Arm { sock: s, interest }).is_empty());
    }
    assert!(poll(&mut r, SHORT).is_empty());

    c.write_all(b"one").unwrap();
    assert_eq!(poll(&mut r, LONG), vec![ready(s, true, false)]);
    c.write_all(b"two").unwrap();
    assert!(
        poll(&mut r, SHORT).is_empty(),
        "no event until the next arm"
    );

    // Re-arming with the data still unread reports it at once.
    assert!(
        perform(
            &mut r,
            Action::Arm {
                sock: s,
                interest: Interest::READ
            }
        )
        .is_empty()
    );
    assert_eq!(poll(&mut r, LONG), vec![ready(s, true, false)]);
    assert!(poll(&mut r, SHORT).is_empty());

    // A write arm on an idle stream fires at once, and reports only what was
    // asked for: the unread data is not mentioned.
    assert!(
        perform(
            &mut r,
            Action::Arm {
                sock: s,
                interest: Interest::WRITE
            }
        )
        .is_empty()
    );
    assert_eq!(poll(&mut r, LONG), vec![ready(s, false, true)]);
}

#[test]
fn a_second_arm_widens_the_pending_one() {
    let mut r = Reactor::new().unwrap();
    let addr = listener(&mut r);
    let s = SockId(2);
    let mut c = accepted(&mut r, addr, s);
    let arm = |r: &mut Reactor, interest| perform(r, Action::Arm { sock: s, interest });

    // Waiting to read; nothing arrives.
    assert!(arm(&mut r, Interest::READ).is_empty());
    assert!(poll(&mut r, SHORT).is_empty());
    // Widening with write interest has no event of its own; the pending arm
    // completes once, for the writability that is already there.
    assert!(arm(&mut r, Interest::WRITE).is_empty());
    assert_eq!(poll(&mut r, LONG), vec![ready(s, false, true)]);
    c.write_all(b"late").unwrap();
    assert!(
        poll(&mut r, SHORT).is_empty(),
        "one Ready for the joint arm, then none until the next arm"
    );

    // Readiness for the first interest completes a widened arm just the
    // same, and reports what is ready.
    assert!(arm(&mut r, Interest::READ).is_empty());
    assert!(arm(&mut r, Interest::BOTH).is_empty());
    assert_eq!(poll(&mut r, LONG), vec![ready(s, true, true)]);
    assert!(poll(&mut r, SHORT).is_empty());

    // A Close completes a widened arm too.
    assert!(arm(&mut r, Interest::WRITE).is_empty());
    assert!(arm(&mut r, Interest::READ).is_empty());
    assert_eq!(
        perform1(&mut r, Action::Close { sock: s }),
        Event::Closed {
            sock: s,
            result: Ok(())
        }
    );
    assert!(poll(&mut r, SHORT).is_empty());
}

#[test]
fn unarmed_hang_up_is_reported_by_the_next_arm() {
    let mut r = Reactor::new().unwrap();
    let addr = listener(&mut r);
    let s = SockId(2);
    let c = accepted(&mut r, addr, s);

    // Hang-ups are reported by epoll even without interest; unarmed, the
    // reactor must swallow them rather than emit an event nobody asked for.
    socket2::SockRef::from(&c)
        .set_linger(Some(Duration::ZERO))
        .unwrap(); // close with RST
    drop(c);
    assert!(poll(&mut r, SHORT).is_empty());
    assert!(
        perform(
            &mut r,
            Action::Arm {
                sock: s,
                interest: Interest::READ
            }
        )
        .is_empty()
    );
    let evs = poll(&mut r, LONG);
    assert!(
        matches!(evs.as_slice(), [Event::Ready { result: Ok(rd), .. }] if rd.readable),
        "{evs:?}"
    );
}

#[test]
fn close_completes_a_pending_arm() {
    let mut r = Reactor::new().unwrap();
    let addr = listener(&mut r);
    let s = SockId(2);
    let mut c = accepted(&mut r, addr, s);

    assert!(
        perform(
            &mut r,
            Action::Arm {
                sock: s,
                interest: Interest::READ
            }
        )
        .is_empty()
    );
    // The Closed is the only event: it completes the Close and the arm.
    assert_eq!(
        perform1(&mut r, Action::Close { sock: s }),
        Event::Closed {
            sock: s,
            result: Ok(())
        }
    );
    let _ = c.write_all(b"late"); // may fail: the server end is gone
    assert!(poll(&mut r, SHORT).is_empty(), "no Ready after Close");
    assert_eq!(
        perform1(&mut r, Action::Read { sock: s, max: 8 }),
        Event::Read {
            sock: s,
            result: Err(err(ErrorKind::NotFound))
        }
    );

    // The listener too.
    assert!(
        perform(
            &mut r,
            Action::Arm {
                sock: L,
                interest: Interest::READ
            }
        )
        .is_empty()
    );
    assert_eq!(
        perform1(&mut r, Action::Close { sock: L }),
        Event::Closed {
            sock: L,
            result: Ok(())
        }
    );
    assert!(TcpStream::connect(addr).is_err(), "the port is closed");
}

#[test]
fn unknown_ids_and_wrong_kinds_are_errors_not_panics() {
    let mut r = Reactor::new().unwrap();
    listener(&mut r);
    let x = SockId(999);
    fn nf<T>() -> Result<T, IoError> {
        fail(ErrorKind::NotFound)
    }

    assert_eq!(
        perform1(
            &mut r,
            Action::Accept {
                listener: x,
                new: SockId(3)
            }
        ),
        Event::Accepted {
            listener: x,
            new: SockId(3),
            result: nf()
        }
    );
    assert_eq!(
        perform1(&mut r, Action::FinishConnect { sock: x }),
        Event::Connected {
            sock: x,
            result: nf()
        }
    );
    assert_eq!(
        perform1(&mut r, Action::Read { sock: x, max: 8 }),
        Event::Read {
            sock: x,
            result: nf()
        }
    );
    assert_eq!(
        perform1(
            &mut r,
            Action::Write {
                sock: x,
                data: vec![1]
            }
        ),
        Event::Wrote {
            sock: x,
            data: vec![1],
            result: nf()
        }
    );
    assert_eq!(
        perform1(
            &mut r,
            Action::Arm {
                sock: x,
                interest: Interest::READ
            }
        ),
        Event::Ready {
            sock: x,
            result: nf()
        }
    );
    assert_eq!(
        perform1(&mut r, Action::Close { sock: x }),
        Event::Closed {
            sock: x,
            result: nf()
        }
    );

    // A listener is not a stream, nor a connecting socket.
    assert_eq!(
        perform1(&mut r, Action::Read { sock: L, max: 8 }),
        Event::Read {
            sock: L,
            result: fail(ErrorKind::InvalidInput)
        }
    );
    assert_eq!(
        perform1(&mut r, Action::FinishConnect { sock: L }),
        Event::Connected {
            sock: L,
            result: fail(ErrorKind::InvalidInput)
        }
    );
    // Its id is taken.
    let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
    assert_eq!(
        perform1(&mut r, Action::Connect { sock: L, addr }),
        Event::Connected {
            sock: L,
            result: fail(ErrorKind::AlreadyExists)
        }
    );
    let l2 = TcpListener::bind("127.0.0.1:0").unwrap();
    assert_eq!(
        r.adopt_listener(L, l2).unwrap_err().kind(),
        ErrorKind::AlreadyExists
    );

    // A signal is not a socket: actions never reach it.
    let (sig, _tx) = r.signal().unwrap();
    let as_sock = SockId(sig.0);
    assert_eq!(
        perform1(&mut r, Action::Close { sock: as_sock }),
        Event::Closed {
            sock: as_sock,
            result: nf()
        }
    );
}

#[test]
fn several_actions_complete_in_order() {
    let mut r = Reactor::new().unwrap();
    listener(&mut r);
    let mut actions = vec![
        Action::Accept {
            listener: L,
            new: SockId(2),
        },
        Action::Arm {
            sock: L,
            interest: Interest::READ,
        },
        Action::Read {
            sock: SockId(9),
            max: 1,
        },
        Action::Close { sock: SockId(9) },
    ];
    let mut events = Vec::new();
    r.perform(&mut actions, &mut events);
    assert!(actions.is_empty());
    let ids: Vec<_> = events
        .iter()
        .map(|e| match e {
            Event::Accepted { new, .. } => new.0,
            Event::Read { sock, .. } | Event::Closed { sock, .. } => sock.0,
            e => panic!("unexpected {e:?}"),
        })
        .collect();
    assert_eq!(ids, vec![2, 9, 9], "the arm completes later");
}

#[test]
fn signal_raised_from_another_thread_wakes_poll() {
    let mut r = Reactor::new().unwrap();
    let (sig, tx) = r.signal().unwrap();

    let t = {
        let tx = tx.clone();
        std::thread::spawn(move || tx.raise().unwrap())
    };
    let mut events = Vec::new();
    r.poll(&mut events, None).unwrap();
    t.join().unwrap();
    assert_eq!(events, vec![Event::Signal { signal: sig }]);

    // Raises coalesce, and the reactor re-arms and drains by itself.
    tx.raise().unwrap();
    tx.raise().unwrap();
    assert_eq!(poll(&mut r, LONG), vec![Event::Signal { signal: sig }]);
    assert!(poll(&mut r, SHORT).is_empty(), "drained");
    tx.raise().unwrap();
    assert_eq!(poll(&mut r, LONG), vec![Event::Signal { signal: sig }]);

    // Two signals are told apart.
    let (sig2, tx2) = r.signal().unwrap();
    assert_ne!(sig, sig2);
    tx2.raise().unwrap();
    assert_eq!(poll(&mut r, LONG), vec![Event::Signal { signal: sig2 }]);

    // Dropping every sender delivers one last signal, then nothing: an EOF
    // must not wake the loop forever.
    drop(tx);
    assert_eq!(poll(&mut r, LONG), vec![Event::Signal { signal: sig }]);
    assert!(poll(&mut r, SHORT).is_empty());

    // Raising after the reactor is gone fails rather than blocking.
    drop(r);
    assert!(tx2.raise().is_err());
}

#[test]
fn connect_to_a_closed_port_is_refused_on_finish() {
    let mut r = Reactor::new().unwrap();
    // Bind, then close, to find a port nothing listens on.
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let s = SockId(5);

    match perform1(&mut r, Action::Connect { sock: s, addr }) {
        Event::Connected {
            result: Ok(Progress::InProgress),
            ..
        } => {}
        // Loopback may refuse synchronously; then nothing is left to close.
        Event::Connected { result: Err(e), .. } => {
            assert_eq!(e.kind, ErrorKind::ConnectionRefused);
            return;
        }
        ev => panic!("unexpected {ev:?}"),
    }
    assert!(
        perform(
            &mut r,
            Action::Arm {
                sock: s,
                interest: Interest::WRITE
            }
        )
        .is_empty()
    );
    assert_eq!(poll(&mut r, LONG).len(), 1);
    let ev = perform1(&mut r, Action::FinishConnect { sock: s });
    let Event::Connected {
        sock,
        result: Err(e),
    } = ev
    else {
        panic!("expected a refusal, got {ev:?}");
    };
    assert_eq!(sock, s);
    assert_eq!(e.kind, ErrorKind::ConnectionRefused);
    assert!(e.os.is_some());
    // The failed socket stays until the planner closes it.
    assert_eq!(
        perform1(&mut r, Action::Close { sock: s }),
        Event::Closed {
            sock: s,
            result: Ok(())
        }
    );
}

#[test]
fn connect_then_finish_gives_a_stream() {
    let mut r = Reactor::new().unwrap();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let s = SockId(5);

    let ev = perform1(&mut r, Action::Connect { sock: s, addr });
    let Event::Connected {
        result: Ok(progress),
        ..
    } = ev
    else {
        panic!("unexpected {ev:?}");
    };
    // Before it is a stream, it cannot be read.
    if progress == Progress::InProgress {
        assert_eq!(
            perform1(&mut r, Action::Read { sock: s, max: 8 }),
            Event::Read {
                sock: s,
                result: Err(err(ErrorKind::InvalidInput))
            }
        );
        assert!(
            perform(
                &mut r,
                Action::Arm {
                    sock: s,
                    interest: Interest::WRITE
                }
            )
            .is_empty()
        );
        let evs = poll(&mut r, LONG);
        assert!(
            matches!(evs.as_slice(), [Event::Ready { result: Ok(rd), .. }] if rd.writable),
            "{evs:?}"
        );
        assert_eq!(
            perform1(&mut r, Action::FinishConnect { sock: s }),
            Event::Connected {
                sock: s,
                result: Ok(Progress::Done)
            }
        );
    }

    let (mut peer, _) = l.accept().unwrap();
    peer.set_read_timeout(Some(LONG)).unwrap();
    assert_eq!(
        perform1(
            &mut r,
            Action::Write {
                sock: s,
                data: b"hi".to_vec()
            }
        ),
        Event::Wrote {
            sock: s,
            data: b"hi".to_vec(),
            result: Ok(2)
        }
    );
    let mut buf = [0u8; 2];
    peer.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"hi");
}

#[test]
fn resolve_localhost() {
    let mut r = Reactor::new().unwrap();
    let ev = perform1(
        &mut r,
        Action::Resolve {
            query: 7,
            host: "localhost".to_string(),
            port: 80,
        },
    );
    let Event::Resolved {
        query: 7,
        result: Ok(addrs),
    } = ev
    else {
        panic!("unexpected {ev:?}");
    };
    assert!(!addrs.is_empty());
    assert!(
        addrs.iter().all(|a| a.port() == 80 && a.ip().is_loopback()),
        "{addrs:?}"
    );
}

#[test]
fn a_failed_lookup_keeps_its_text() {
    let mut r = Reactor::new().unwrap();
    let ev = perform1(
        &mut r,
        Action::Resolve {
            query: 8,
            host: "nonexistent.invalid".to_string(),
            port: 80,
        },
    );
    let Event::Resolved {
        query: 8,
        result: Err(e),
    } = ev
    else {
        panic!("unexpected {ev:?}");
    };
    // std reports getaddrinfo's own errors without an OS code, as
    // "failed to lookup address information: ...".
    let text = e.to_string();
    assert!(text.contains("lookup"), "{text}");
    assert_ne!(text, e.kind.to_string(), "more than the kind");
}

#[test]
fn reactor_is_send() {
    fn is_send<T: Send>() {}
    is_send::<Reactor>();
    is_send::<steploop::reactor::SignalSender>();
}
