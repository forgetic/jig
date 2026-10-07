//! Turns are opaque parent payloads. The fleet holds only their admission
//! and committed prefix (domain/engine.md, 7.2 and 8; domain/worker.md, section 8).
//! A stray's body stays here until adoption; every body leaves exactly once
//! in `Turned` or `Drop`. Handed admissions own no payload. Cleanup and
//! adoption release one held body per resume, so each emits at most two
//! requests, including an acknowledgement or busy notice and a drop.

use skein_lib::{Id, Queue, Token};

use crate::attempt::{Attempt, State, Where};
use crate::boundary::Request;
use crate::channel;
use crate::domain::Domain;
use crate::facts::Fact;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Pending {
    Held { body: Token },
    Handed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Face {
    Live { channel: Option<Token> },
    Stray { channel: Option<Token> },
    Fenced { channel: Option<Token> },
    Unplaced,
}

fn contact(domain: &Domain, at: Where) -> Option<Token> {
    match at {
        Where::On(id) => Some(channel::token(&domain.channels, id)),
        Where::Adrift { .. } => None,
    }
}

fn face(domain: &Domain, entry: &Attempt) -> Face {
    match &entry.state {
        State::Claimed { at, .. } | State::Handed { at } => Face::Live { channel: contact(domain, *at) },
        State::Stray { at, .. } | State::Kept { at, .. } => Face::Stray { channel: contact(domain, *at) },
        State::Cancelled { at, .. } | State::Fenced { at } => Face::Fenced { channel: contact(domain, *at) },
        State::Acknowledged { .. } | State::Closed => Face::Fenced { channel: None },
        State::Waiting { .. } | State::Adopted { .. } => Face::Unplaced,
    }
}

fn drop(domain: &mut Domain, body: Token, out: &mut Queue<Request>) {
    domain.facts.push(Fact::Dropped);
    out.push(Request::Drop { payload: body });
}

fn acknowledged(channel: Token, run: Token, attempt: Token, turn: u32, out: &mut Queue<Request>) {
    out.push(Request::AcknowledgeTurn { channel, run, attempt, turn });
}

pub(crate) fn received(
    domain: &mut Domain,
    channel: Token,
    run: Token,
    attempt: Token,
    turn: u32,
    body: Token,
    out: &mut Queue<Request>,
) {
    if !domain.tokens.contains_key(&channel) || turn == 0 {
        drop(domain, body, out);
        return;
    }
    let Some(&id) = domain.names.get(&(run, attempt)) else {
        acknowledged(channel, run, attempt, turn, out);
        drop(domain, body, out);
        return;
    };
    let entry = domain.attempts.get(id).expect("a named attempt is tracked");
    let held = entry.kept;
    let pending = match face(domain, entry) {
        Face::Live { channel: Some(owned) } if owned == channel => Pending::Handed,
        Face::Stray { channel: Some(owned) } if owned == channel => Pending::Held { body },
        Face::Fenced { channel: Some(owned) } if owned == channel => {
            acknowledged(channel, run, attempt, turn, out);
            drop(domain, body, out);
            return;
        }
        Face::Live { .. } | Face::Stray { .. } | Face::Fenced { .. } | Face::Unplaced => {
            drop(domain, body, out);
            return;
        }
    };
    if turn <= held {
        acknowledged(channel, run, attempt, turn, out);
        drop(domain, body, out);
        return;
    }
    if domain.turns.contains_key(&(id, turn)) {
        drop(domain, body, out);
        return;
    }
    match domain.turns.insert((id, turn), pending) {
        Ok(None) => match pending {
            Pending::Handed => out.push(Request::Turned { run, attempt, turn, body }),
            Pending::Held { .. } => {}
        },
        Ok(Some(_)) => unreachable!("a duplicate admission was checked above"),
        Err(_) => {
            out.push(Request::TurnBusy { channel, run, attempt, turn });
            drop(domain, body, out);
        }
    }
}

/// The parent commits a contiguous prefix, one turn at a time. A stale
/// completion after an attempt retires changes nothing. The admission is
/// released even while its worker is away; replay uses the kept prefix.
pub(crate) fn kept(domain: &mut Domain, run: Token, attempt: Token, turn: u32, out: &mut Queue<Request>) {
    let Some(&id) = domain.names.get(&(run, attempt)) else { return };
    let entry = domain.attempts.get_mut(id).expect("a named attempt is tracked");
    if turn <= entry.kept {
        return;
    }
    assert!(entry.kept.checked_add(1) == Some(turn), "the parent commits turns consecutively");
    entry.kept = turn;
    // Handed metadata owns no payload. Held bodies are dropped by resume.
    match domain.turns.get(&(id, turn)) {
        Some(Pending::Handed) => {
            domain.turns.remove(&(id, turn));
        }
        Some(Pending::Held { .. }) => domain.turning = true,
        None => {}
    }
    let entry = domain.attempts.get(id).expect("a named attempt is tracked");
    match face(domain, entry) {
        Face::Live { channel: Some(channel) }
        | Face::Stray { channel: Some(channel) }
        | Face::Fenced { channel: Some(channel) } => acknowledged(channel, run, attempt, turn, out),
        Face::Live { channel: None }
        | Face::Stray { channel: None }
        | Face::Fenced { channel: None }
        | Face::Unplaced => {}
    }
}

pub(crate) fn busy(domain: &mut Domain, run: Token, attempt: Token, turn: u32, out: &mut Queue<Request>) {
    let Some(&id) = domain.names.get(&(run, attempt)) else { return };
    match domain.turns.get(&(id, turn)) {
        Some(Pending::Handed) => {
            domain.turns.remove(&(id, turn));
        }
        Some(Pending::Held { .. }) | None => return,
    }
    let entry = domain.attempts.get(id).expect("a named attempt is tracked");
    match face(domain, entry) {
        Face::Live { channel: Some(channel) } => out.push(Request::TurnBusy { channel, run, attempt, turn }),
        Face::Live { channel: None } | Face::Stray { .. } | Face::Fenced { .. } | Face::Unplaced => {}
    }
}

/// One admission's next action, selected by a bounded scan. Held stray
/// bodies wait; handed metadata waits for the parent. No ready spin while
/// either waits. Every attempt transition schedules another scan.
pub(crate) fn resume(domain: &mut Domain, out: &mut Queue<Request>) {
    let mut ready: Option<(Id<Attempt>, u32)> = None;
    for (&key, pending) in &domain.turns {
        let action = match domain.attempts.get(key.0) {
            None => true,
            Some(entry) => match face(domain, entry) {
                Face::Fenced { .. } => true,
                Face::Live { .. } => match pending {
                    Pending::Held { .. } => true,
                    Pending::Handed => key.1 <= entry.kept,
                },
                Face::Stray { .. } => key.1 <= entry.kept,
                Face::Unplaced => false,
            },
        };
        if action {
            ready = Some(key);
            break;
        }
    }
    let Some(key) = ready else {
        domain.turning = false;
        return;
    };
    let pending = domain.turns.remove(&key).expect("selected above");
    match pending {
        Pending::Handed => {}
        Pending::Held { body } => {
            let Some(entry) = domain.attempts.get(key.0) else {
                drop(domain, body, out);
                return;
            };
            let (run, attempt, turn) = (entry.run, entry.token, key.1);
            let kept = turn <= entry.kept;
            match face(domain, entry) {
                Face::Live { .. } if !kept => {
                    let room = domain.turns.insert(key, Pending::Handed);
                    assert!(room == Ok(None), "removing the held body made room");
                    out.push(Request::Turned { run, attempt, turn, body });
                }
                Face::Live { channel } | Face::Stray { channel } | Face::Fenced { channel } => {
                    if let Some(channel) = channel {
                        acknowledged(channel, run, attempt, turn, out);
                    }
                    drop(domain, body, out);
                }
                Face::Unplaced => unreachable!("unplaced bodies are not ready"),
            }
        }
    }
}
