//! What the fleet's scenarios expect, held by a referee (testing.md,
//! 5.2) that sees what the scripted workers see and do, what goes down their
//! channels, and what the scripted parent asks and hears; never the fleet's
//! state.
//!
//! Safety, checked on every observation:
//!
//! - no worker is assigned more runs than its slots, the answers it holds
//!   for the engine counted;
//! - never two live attempts of a run's workstream: no worker is assigned an
//!   attempt of a run while any worker hosts another of it, and no attempt
//!   is assigned again once a worker admitted it (a busy refusal admits
//!   nothing, and the attempt is assigned again);
//! - nothing reaches a worker for an attempt the parent cancelled, or that
//!   ended (answered, presumed lost or withdrawn), but a cancel;
//! - every attempt the parent starts ends with the answer its worker gave,
//!   or presumed lost, withdrawn or refused; and once its answer is durable
//!   it ends no more (before, a restart may hand it on again);
//! - a worker forgets an answer only once the parent has made it durable,
//!   or no longer claims its attempt.
//!
//! Liveness, as deadlines of its own: every attempt the parent starts ends
//! within a bound the world sets.
//!
//! It injects what belongs to no fake: a worker's channel dropping, for a
//! while or for good, and the engine restarting.

use std::collections::{BTreeMap, BTreeSet};

use skein_lib::Duration;
use skein_world::domain::{Expectations, Judge};

/// How a worker's run answered, as the referee compares it: its kind and the
/// nonce that tells one answer from another.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Said {
    pub kind: Kind,
    pub nonce: u64,
}

/// How a worker answered an assignment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Ended,
    Parked,
    Failed,
    Busy,
    Invalid,
}

/// How an attempt ended, as the parent heard it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum End {
    Answered(Said),
    Lost,
    Cancelled,
    Replaced,
    Refused,
}

/// What went down a worker's channel about an attempt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Down {
    Assign,
    Inbound,
    Relayed,
    Cancel,
}

/// What the referee observes. Runs and attempts are the world's names.
#[derive(Debug)]
pub enum Seen {
    /// The parent started the attempt: it must end.
    Started { run: u64, attempt: u64 },
    /// The parent cancelled the attempt.
    Cancelled { run: u64, attempt: u64 },
    /// The parent heard the attempt end.
    Ended { run: u64, attempt: u64, end: End },
    /// The fleet sent `down` about the attempt to a worker.
    Sent { run: u64, attempt: u64, down: Down },
    /// The worker `worker`, with `slots`, hosting `hosting` runs and holding
    /// answers, received the assignment of the attempt, and admitted it or
    /// not.
    Assigned { worker: usize, slots: u32, hosting: u32, run: u64, attempt: u64, admitted: bool },
    /// A worker answered the attempt, once, its run gone if it was hosted.
    Answered { run: u64, attempt: u64, said: Said },
    /// The parent made the attempt's answer durable.
    Durable { run: u64, attempt: u64 },
    /// A worker forgot the attempt's answer, acknowledged.
    Forgot { run: u64, attempt: u64 },
}

/// What the referee expects to happen.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Expected {
    /// The attempt the parent started ends.
    End { run: u64, attempt: u64 },
}

/// What the referee injects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stimulus {
    /// The channel of the worker `worker` drops; it dials again after
    /// `back`, or never.
    Drop { worker: usize, back: Option<Duration> },
    /// The engine restarts: a new fleet, which knows nothing.
    Restart,
}

/// The expectations of the fleet's world.
#[derive(Debug)]
pub struct Fleet {
    /// How long an attempt may take to end.
    within: Duration,
    /// The live attempt of each run on the workers, from its admission to
    /// its answer.
    live: BTreeMap<u64, u64>,
    /// Attempts a worker admitted so far.
    admitted: BTreeSet<(u64, u64)>,
    /// What each attempt's worker answered.
    said: BTreeMap<(u64, u64), Said>,
    /// Attempts the parent cancelled; those that ended for good (lost,
    /// withdrawn, refused, or answered durably); and those its record
    /// claims.
    cancelled: BTreeSet<(u64, u64)>,
    ended: BTreeSet<(u64, u64)>,
    claimed: BTreeSet<(u64, u64)>,
    /// Ends and messages judged.
    pub ends: u64,
    pub sent: u64,
}

impl Fleet {
    /// Expectations under which every attempt ends `within` its start.
    #[must_use]
    pub fn new(within: Duration) -> Fleet {
        Fleet {
            within,
            live: BTreeMap::new(),
            admitted: BTreeSet::new(),
            said: BTreeMap::new(),
            cancelled: BTreeSet::new(),
            ended: BTreeSet::new(),
            claimed: BTreeSet::new(),
            ends: 0,
            sent: 0,
        }
    }
}

impl Expectations for Fleet {
    type Seen = Seen;
    type Name = Expected;
    type Stimulus = Stimulus;

    fn observe(&mut self, seen: Seen, judge: &mut Judge<Expected, Stimulus>) {
        match seen {
            Seen::Started { run, attempt } => {
                self.claimed.insert((run, attempt));
                judge.expect(Expected::End { run, attempt }, self.within);
            }
            Seen::Cancelled { run, attempt } => {
                self.cancelled.insert((run, attempt));
            }
            Seen::Ended { run, attempt, end } => {
                self.ends += 1;
                judge.meet(&Expected::End { run, attempt });
                let after = self.ended.contains(&(run, attempt));
                judge.check(
                    !after,
                    format_args!("attempt {attempt} of run {run} ends no more once ended, ending {end:?}"),
                );
                match end {
                    End::Answered(said) => {
                        let given = self.said.get(&(run, attempt));
                        judge.check(
                            given == Some(&said),
                            format_args!(
                                "attempt {attempt} of run {run} ends with what its worker answered: {said:?}, not {given:?}"
                            ),
                        );
                    }
                    End::Lost | End::Cancelled | End::Replaced | End::Refused => {
                        self.ended.insert((run, attempt));
                        self.claimed.remove(&(run, attempt));
                    }
                }
            }
            Seen::Sent { run, attempt, down } => {
                self.sent += 1;
                match down {
                    Down::Cancel => {}
                    Down::Assign | Down::Inbound | Down::Relayed => {
                        let fenced = self.cancelled.contains(&(run, attempt)) || self.ended.contains(&(run, attempt));
                        judge.check(
                            !fenced,
                            format_args!("no {down:?} reaches a worker for attempt {attempt} of run {run}, fenced"),
                        );
                    }
                }
            }
            Seen::Assigned { worker, slots, hosting, run, attempt, admitted } => {
                judge.check(
                    hosting < slots,
                    format_args!("worker {worker} is assigned no more runs than its {slots} slots"),
                );
                let fresh = !self.admitted.contains(&(run, attempt));
                judge.check(fresh, format_args!("attempt {attempt} of run {run} is not assigned once admitted"));
                let other = self.live.get(&run).copied();
                judge.check(
                    other.is_none(),
                    format_args!("attempt {attempt} of run {run} is assigned while attempt {other:?} is live"),
                );
                if admitted {
                    self.admitted.insert((run, attempt));
                    self.live.insert(run, attempt);
                }
            }
            Seen::Answered { run, attempt, said } => {
                if self.live.get(&run) == Some(&attempt) {
                    self.live.remove(&run);
                }
                let first = self.said.insert((run, attempt), said).is_none();
                judge.check(first, format_args!("a worker answers attempt {attempt} of run {run} once"));
            }
            Seen::Durable { run, attempt } => {
                self.ended.insert((run, attempt));
                self.claimed.remove(&(run, attempt));
            }
            Seen::Forgot { run, attempt } => {
                judge.check(
                    !self.claimed.contains(&(run, attempt)),
                    format_args!("a worker forgets attempt {attempt} of run {run}'s answer only once it is durable"),
                );
            }
        }
    }
}
