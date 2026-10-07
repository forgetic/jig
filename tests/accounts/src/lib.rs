//! A tiny domain world: the parent scripts token and write completions and
//! checks that retries never spend a rotated refresh token a second time.
use jig_core_accounts::{self as accounts, Domain, Event, Limits, Request};
use skein_lib::{Duration, Env, Queue, Time, Wall};

pub const LIMITS: Limits = Limits {
    accounts: 2,
    refresh_margin: Duration::from_secs(10),
    backoff_base: Duration::from_secs(2),
    backoff_max: Duration::from_secs(8),
    rejected_interval: Duration::from_secs(3),
    spent_attention: Duration::from_secs(30),
    facts: 16,
};

#[derive(Debug)]
pub struct World {
    pub domain: Domain,
    pub now: Time,
}

impl Default for World {
    fn default() -> World {
        World { domain: Domain::new(&LIMITS), now: Time::ZERO }
    }
}

impl World {
    pub fn step(&mut self, event: Event) -> Vec<Request> {
        let mut out = Queue::with_capacity(accounts::MAX_OUT);
        let env = Env { now: self.now, wall: Wall::EPOCH, limits: LIMITS };
        accounts::step(&mut self.domain, &env, event, &mut out);
        drain(&mut out)
    }

    pub fn fire_at(&mut self, seconds: u64) -> Vec<Request> {
        self.now = Time::ZERO.saturating_add(Duration::from_secs(seconds));
        let mut out = Queue::with_capacity(accounts::MAX_OUT);
        let env = Env { now: self.now, wall: Wall::EPOCH, limits: LIMITS };
        accounts::fire(&mut self.domain, &env, &mut out);
        drain(&mut out)
    }
}

fn drain(out: &mut Queue<Request>) -> Vec<Request> {
    let mut result = Vec::new();
    while let Some(request) = out.pop() {
        result.push(request);
    }
    result
}
