//! The views child driven through its public steps by a parent and watchers.

use jig_core_views::{self as views, Domain, Event, Limits, Request, Subject};
use skein_lib::{Env, Queue, Time, Token, Wall};

use crate::referee::Referee;

pub const LIMITS: Limits = Limits { runs: 3, watchers: 4, backlog: 2, report_bytes: 32, snapshot_bytes: 64, facts: 8 };

#[derive(Debug)]
pub struct World {
    pub domain: Domain,
    pub referee: Referee,
    pub now: Time,
    pub limits: Limits,
    pub log: Vec<String>,
}

impl Default for World {
    fn default() -> Self {
        Self::new(LIMITS)
    }
}

impl World {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self { domain: Domain::new(&limits), referee: Referee::default(), now: Time::ZERO, limits, log: Vec::new() }
    }

    pub fn step(&mut self, event: Event) -> Vec<Request> {
        self.referee.before(&event);
        self.log.push(format!("in {event:?}"));
        let env = Env { now: self.now, wall: Wall::EPOCH, limits: self.limits };
        let mut out = Queue::with_capacity(views::max_out(&self.limits));
        views::step(&mut self.domain, &env, event, &mut out);
        let mut requests = Vec::new();
        while let Some(request) = out.pop() {
            self.log.push(format!("out {request:?}"));
            requests.push(request);
        }
        self.referee.saw(&requests);
        requests
    }

    pub fn start(&mut self, task: u64, attempt: u64) -> Vec<Request> {
        self.step(Event::Started { task: Token::new(task), attempt: Token::new(attempt) })
    }

    pub fn watch(&mut self, watcher: u64, subject: Subject) -> Vec<Request> {
        self.step(Event::Watch { watcher: Token::new(watcher), subject, snapshot: b"snapshot".as_slice().into() })
    }

    pub fn delivered(&mut self, watcher: u64, done: bool) -> Vec<Request> {
        self.step(Event::Delivered { watcher: Token::new(watcher), done })
    }

    pub fn reclaim(&mut self) {
        self.domain.reclaim();
    }
}
