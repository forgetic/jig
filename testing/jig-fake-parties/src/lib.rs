//! Independent parties at `domain/people.md`, 11 and `domain/testing.md`, 7.
//! A party keeps its sign-in, last task, visible waiting work and request keys.
//! One script action waits for its reply before the next; duplicate sends reuse
//! the original key and payload. Nothing depends on core or people vocabulary.
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use skein_lib::{Duration, Rng, Time};
use std::collections::VecDeque;

/// The role under which a random script acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// May steer all project work.
    Owner,
    /// May accept work and steer the project.
    Maintainer,
    /// May chat and steer their own work.
    Member,
    /// May observe but cannot start a chat.
    Observer,
}

/// A script's reference to a task without core-owned state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    /// Stable scenario-issued task number.
    Number(u64),
    /// Most recently started or released task this party saw.
    Last,
    /// The visible person task waiting for this party.
    Waiting,
}

/// Requests in the client's independent vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Start one chat.
    Chat { words: Box<[u8]> },
    /// Ask for tracked work.
    Goal { words: Box<[u8]>, budget: u64, priority: u32 },
    /// Add words to a task.
    Say { task: Task, words: Box<[u8]> },
    /// Stop a current run.
    Stop { task: Task },
    /// Release a held task.
    Release { task: Task },
    /// Claim visible role-addressed work.
    Take { task: Task },
    /// Answer one visible report choice.
    Choose { task: Task, code: u32, words: Box<[u8]> },
    /// Decide the visible proposal.
    Decide { accept: bool },
    /// A resolved proposal retained with the keyed request for later duplicates.
    Decision { proposer: u64, proposal: u64, accept: bool },
}

/// One action of a client's scenario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Authenticate with one provider's independent identity.
    SignIn { provider: u16, subject: Box<[u8]> },
    /// Send a keyed request and wait for its reply.
    Request { key: [u8; 16], ask: Ask },
    /// Choose the visible report after taking it.
    AnswerWaiting { code: u32, words: Box<[u8]> },
    /// Decide one visible proposal.
    Decide { accept: bool },
    /// Send the previous request again with its original key and payload.
    Twice,
    /// Let other actors progress for this interval.
    Pause { for_: Duration },
}

/// A waiting item independently presented by the world's observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waiting {
    /// One action needing this party's authority.
    Proposal { proposer: u64, proposal: u64 },
    /// One person task asking for a result.
    Choice { task: u64 },
}

/// A reply a client can see, translated by the world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Authentication established a party and a sign-in.
    SignedIn { person: u64, sign_in: u64 },
    /// A new chat or goal was made.
    Started { task: u64 },
    /// A goal request awaits another party.
    Proposed { proposal: u64 },
    /// A stop or release was kept.
    Changed { task: u64 },
    /// The numbered request finished.
    Done,
    /// The request was refused.
    Refused,
}

/// One request at the party's independent client face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Up {
    /// Authentication before any keyed request.
    SignIn { to: u64, provider: u16, subject: Box<[u8]> },
    /// One request using the session this party received.
    Request { to: u64, sign_in: u64, project: u32, key: [u8; 16], ask: Ask },
}

/// A client with a deterministic script and its own reply and waiting ledger.
#[derive(Debug)]
pub struct Party {
    pub project: u32,
    pub role: Role,
    pub person: Option<u64>,
    pub sign_in: Option<u64>,
    pub last_task: Option<u64>,
    pub waiting: Option<Waiting>,
    pub replies: Vec<(u64, Reply)>,
    script: VecDeque<Act>,
    next_to: u64,
    in_flight: Option<u64>,
    last: Option<Up>,
    pause: Option<Time>,
}

impl Party {
    /// Request numbers start in a scenario-selected disjoint range.
    #[must_use]
    pub fn new(project: u32, role: Role, first_to: u64, script: Box<[Act]>) -> Self {
        Self {
            project,
            role,
            person: None,
            sign_in: None,
            last_task: None,
            waiting: None,
            replies: Vec::new(),
            script: script.into_vec().into(),
            next_to: first_to,
            in_flight: None,
            last: None,
            pause: None,
        }
    }

    /// Present or withdraw one waiting item after the store kept its decision.
    pub fn waiting(&mut self, waiting: Option<Waiting>) {
        self.waiting = waiting;
    }

    /// Accept the reply to a sent request; every flight is answered once.
    pub fn reply(&mut self, to: u64, reply: Reply) {
        assert_eq!(self.in_flight.take(), Some(to), "one terminal reply to the party's flight");
        match &reply {
            Reply::SignedIn { person, sign_in } => {
                self.person = Some(*person);
                self.sign_in = Some(*sign_in);
            }
            Reply::Started { task } | Reply::Changed { task } => self.last_task = Some(*task),
            Reply::Proposed { .. } | Reply::Done | Reply::Refused => {}
        }
        self.replies.push((to, reply));
    }

    /// Add a later scenario phase without replacing outstanding work.
    pub fn extend(&mut self, acts: Box<[Act]>) {
        self.script.extend(acts);
    }

    /// Send at most one action, waiting for dependencies and the preceding reply.
    #[must_use]
    pub fn tick(&mut self, now: Time) -> Option<Up> {
        if self.in_flight.is_some() || self.pause.is_some_and(|until| now < until) {
            return None;
        }
        self.pause = None;
        let act = self.script.front()?.clone();
        let to = self.next_to;
        let up = match act {
            Act::SignIn { provider, subject } => Up::SignIn { to, provider, subject },
            Act::Pause { for_ } => {
                self.script.pop_front();
                self.pause = Some(now.saturating_add(for_));
                return None;
            }
            Act::Twice => {
                let Some(Up::Request { sign_in, project, key, ask, .. }) = self.last.clone() else {
                    panic!("duplicate follows a keyed request");
                };
                Up::Request { to, sign_in, project, key, ask }
            }
            Act::Request { key, ask } => {
                let ask = self.resolve(ask)?;
                Up::Request { to, sign_in: self.sign_in?, project: self.project, key, ask }
            }
            Act::Decide { accept } => {
                if !matches!(self.waiting, Some(Waiting::Proposal { .. })) {
                    return None;
                }
                Up::Request {
                    to,
                    sign_in: self.sign_in?,
                    project: self.project,
                    key: request_key(to),
                    ask: self.resolve(Ask::Decide { accept })?,
                }
            }
            Act::AnswerWaiting { code, words } => {
                let Some(Waiting::Choice { task }) = self.waiting else { return None };
                Up::Request {
                    to,
                    sign_in: self.sign_in?,
                    project: self.project,
                    key: request_key(to),
                    ask: Ask::Choose { task: Task::Number(task), code, words },
                }
            }
        };
        self.script.pop_front();
        self.next_to = to.checked_add(1).expect("party request number fits");
        self.in_flight = Some(to);
        self.last = Some(up.clone());
        Some(up)
    }

    fn task(&self, task: Task) -> Option<Task> {
        let number = match task {
            Task::Number(number) => number,
            Task::Last => self.last_task?,
            Task::Waiting => match self.waiting? {
                Waiting::Choice { task } => task,
                Waiting::Proposal { .. } => return None,
            },
        };
        Some(Task::Number(number))
    }

    fn resolve(&self, ask: Ask) -> Option<Ask> {
        Some(match ask {
            Ask::Say { task, words } => Ask::Say { task: self.task(task)?, words },
            Ask::Stop { task } => Ask::Stop { task: self.task(task)? },
            Ask::Release { task } => Ask::Release { task: self.task(task)? },
            Ask::Take { task } => Ask::Take { task: self.task(task)? },
            Ask::Choose { task, code, words } => Ask::Choose { task: self.task(task)?, code, words },
            Ask::Decide { accept } => {
                let Some(Waiting::Proposal { proposer, proposal }) = self.waiting else { return None };
                Ask::Decision { proposer, proposal, accept }
            }
            other @ (Ask::Chat { .. } | Ask::Goal { .. } | Ask::Decision { .. }) => other,
        })
    }

    /// Whether its script and flights have ended.
    #[must_use]
    pub fn quiescent(&self) -> bool {
        self.script.is_empty() && self.in_flight.is_none()
    }
}

fn request_key(number: u64) -> [u8; 16] {
    let mut key = [0; 16];
    key[8..].copy_from_slice(&number.to_be_bytes());
    key
}

/// Generate requests within a role's steering powers, for the fuzzy harness.
#[must_use]
pub fn random_acts(seed: u64, role: Role, count: u32) -> Box<[Act]> {
    let mut rng = Rng::new(seed);
    (0..count)
        .map(|number| {
            let ask = match role {
                Role::Owner | Role::Maintainer if rng.below(2) == 0 => {
                    Ask::Goal { words: Box::from(&b"goal"[..]), budget: 10, priority: 1 }
                }
                Role::Owner | Role::Maintainer | Role::Member => Ask::Chat { words: Box::from(&b"hello"[..]) },
                Role::Observer => return Act::Pause { for_: Duration::from_millis(1) },
            };
            Act::Request { key: request_key(u64::from(number) + 1), ask }
        })
        .collect()
}
