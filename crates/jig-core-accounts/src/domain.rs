use skein_lib::{Deadlines, Duration, Env, Map, Queue, Time};

use crate::{Event, Fact, Failure, Grant, Limits, Request, State};

/// A grant, availability change, and the next refresh or write.
pub const MAX_OUT: u32 = 3;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Phase {
    Fresh { due: Time },
    Refreshing { generation: u64, keeping: bool },
    Retrying { due: Time, generation: u64, keeping: bool },
    Revoked,
    Closing { generation: u64 },
}

#[derive(PartialEq, Eq, Hash, Debug)]
pub(crate) struct Account {
    generation: u64,
    expires: Option<Time>,
    phase: Phase,
    last_refresh: Time,
    backoff: Duration,
    spent: Option<Time>,
    usable: bool,
    unsaved: bool,
    spent_attention: bool,
}

#[derive(Debug)]
pub struct Domain {
    accounts: Map<u32, Account>,
    alarms: Deadlines<u32>,
    facts: Queue<Fact>,
    lost: u64,
}

impl Domain {
    #[must_use]
    pub fn new(limits: &Limits) -> Domain {
        assert!(crate::worst_case(limits).is_some(), "account limits are valid");
        Domain {
            accounts: Map::with_capacity(limits.accounts),
            alarms: Deadlines::with_capacity(limits.accounts),
            facts: Queue::with_capacity(limits.facts),
            lost: 0,
        }
    }

    #[must_use]
    pub fn usable(&self, account: u32) -> bool {
        match self.accounts.get(&account) {
            Some(entry) => entry.usable,
            None => false,
        }
    }

    #[must_use]
    pub fn grant(&self, account: u32, now: Time) -> Option<Grant> {
        let entry = self.accounts.get(&account)?;
        let expires = entry.expires?;
        if !entry.usable || expires <= now {
            return None;
        }
        Some(Grant { account, generation: entry.generation, valid: expires.saturating_since(now) })
    }

    #[must_use]
    pub fn next_deadline(&self) -> Option<Time> {
        self.alarms.next()
    }

    #[must_use]
    pub fn is_due(&self, now: Time) -> bool {
        match self.alarms.next() {
            Some(at) => at <= now,
            None => false,
        }
    }

    /// Root completion fence: true while a refresh, keep or cancellation awaits its terminal.
    /// Future refresh/retry timers alone are idle; this query neither cancels IO nor changes
    /// grants.
    #[must_use]
    pub fn waiting(&self) -> bool {
        for (_, account) in &self.accounts {
            match account.phase {
                Phase::Refreshing { .. } | Phase::Closing { .. } => return true,
                Phase::Fresh { .. } | Phase::Retrying { .. } | Phase::Revoked => {}
            }
        }
        false
    }

    #[must_use]
    pub fn accounts(&self) -> u32 {
        self.accounts.len()
    }

    pub fn pop_fact(&mut self) -> Option<Fact> {
        self.facts.pop()
    }

    #[must_use]
    pub const fn facts_lost(&self) -> u64 {
        self.lost
    }
}

pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event, out: &mut Queue<Request>) {
    let account = match event {
        Event::Add { account, generation, valid } => {
            if !add(domain, env, account, generation, valid, out) {
                return;
            }
            account
        }
        Event::Refreshed { account, generation, valid } => {
            let Some(entry) = domain.accounts.get_mut(&account) else {
                return;
            };
            if closing(entry, account, generation, out) {
                return remove(domain, account);
            }
            if !matches_flight(entry, generation) {
                return;
            }
            entry.generation = generation;
            let expires = env.now.saturating_add(valid);
            entry.expires = Some(expires);
            entry.phase = Phase::Fresh { due: refresh_due(expires, env) };
            entry.backoff = env.limits.backoff_base;
            entry.unsaved = false;
            out.push(Request::Granted { grant: Grant { account, generation, valid } });
            account
        }
        Event::Failed { account, generation, failure } => {
            let Some(entry) = domain.accounts.get_mut(&account) else {
                return;
            };
            if closing(entry, account, generation, out) {
                return remove(domain, account);
            }
            if !matches_flight(entry, generation) {
                return;
            }
            failed(entry, env, generation, failure);
            account
        }
        Event::Rejected { account, generation } => {
            let Some(entry) = domain.accounts.get_mut(&account) else {
                return;
            };
            if generation != entry.generation
                || env.now.saturating_since(entry.last_refresh) < env.limits.rejected_interval
            {
                return;
            }
            match entry.phase {
                Phase::Fresh { .. } => advance(entry, env, account, out),
                Phase::Refreshing { .. } | Phase::Retrying { .. } | Phase::Revoked | Phase::Closing { .. } => return,
            }
            account
        }
        Event::Exhausted { account, retry_after } => {
            let Some(entry) = domain.accounts.get_mut(&account) else {
                return;
            };
            let until = env.now.saturating_add(retry_after);
            entry.spent = Some(match entry.spent {
                Some(old) => old.max(until),
                None => until,
            });
            entry.spent_attention = entry.spent_attention || retry_after >= env.limits.spent_attention;
            account
        }
        Event::Close { account } => {
            let Some(entry) = domain.accounts.get_mut(&account) else {
                return;
            };
            match entry.phase {
                Phase::Refreshing { generation, .. } => {
                    entry.phase = Phase::Closing { generation };
                    out.push(Request::Cancel { account, generation });
                }
                Phase::Closing { .. } => return,
                Phase::Fresh { .. } | Phase::Retrying { .. } | Phase::Revoked => {
                    if entry.usable {
                        out.push(Request::Availability { account, usable: false });
                    }
                    out.push(Request::Closed { account });
                    return remove(domain, account);
                }
            }
            account
        }
    };
    changed(domain, env, account, out);
}

/// One expired account, after inputs so a refresh completing now wins.
pub fn fire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    let Some(account) = domain.alarms.expire(env.now) else {
        return;
    };
    let entry = domain.accounts.get_mut(&account).expect("an alarm belongs to a configured account");
    if let Some(until) = entry.spent
        && until <= env.now
    {
        entry.spent = None;
        entry.spent_attention = false;
    }
    match entry.phase {
        Phase::Fresh { due } if due <= env.now => advance(entry, env, account, out),
        Phase::Retrying { due, generation, keeping } if due <= env.now => {
            refresh(entry, env, account, generation, keeping, out);
        }
        Phase::Fresh { .. }
        | Phase::Refreshing { .. }
        | Phase::Retrying { .. }
        | Phase::Revoked
        | Phase::Closing { .. } => {}
    }
    changed(domain, env, account, out);
}

fn advance(entry: &mut Account, env: &Env<Limits>, account: u32, out: &mut Queue<Request>) {
    match entry.generation.checked_add(1) {
        Some(generation) => refresh(entry, env, account, generation, false, out),
        None => entry.phase = Phase::Revoked,
    }
}

fn refresh_due(expires: Time, env: &Env<Limits>) -> Time {
    let margin = env.limits.refresh_margin.as_nanos();
    Time::from_nanos(expires.as_nanos().saturating_sub(margin))
        .max(env.now.saturating_add(env.limits.rejected_interval.max(env.limits.backoff_base)))
}

fn matches_flight(entry: &Account, wanted: u64) -> bool {
    match entry.phase {
        Phase::Refreshing { generation, .. } => generation == wanted,
        Phase::Fresh { .. } | Phase::Retrying { .. } | Phase::Revoked | Phase::Closing { .. } => false,
    }
}

fn closing(entry: &Account, account: u32, generation: u64, out: &mut Queue<Request>) -> bool {
    match entry.phase {
        Phase::Closing { generation: expected } if expected == generation => {
            out.push(Request::Closed { account });
            true
        }
        Phase::Fresh { .. }
        | Phase::Refreshing { .. }
        | Phase::Retrying { .. }
        | Phase::Revoked
        | Phase::Closing { .. } => false,
    }
}

fn remove(domain: &mut Domain, account: u32) {
    let removed = domain.accounts.remove(&account);
    assert!(removed.is_some(), "a closed account was configured");
    domain.alarms.cancel(account);
}

fn refresh(
    entry: &mut Account,
    env: &Env<Limits>,
    account: u32,
    generation: u64,
    keeping: bool,
    out: &mut Queue<Request>,
) {
    entry.phase = Phase::Refreshing { generation, keeping };
    if keeping {
        out.push(Request::Keep { account, generation });
    } else {
        entry.last_refresh = env.now;
        out.push(Request::Refresh { account, generation });
    }
}

fn failed(entry: &mut Account, env: &Env<Limits>, generation: u64, failure: Failure) {
    let mut keeping = entry.unsaved;
    let floor = match failure {
        Failure::Refused => {
            entry.phase = Phase::Revoked;
            return;
        }
        Failure::Unsaved { valid: _ } => {
            entry.unsaved = true;
            keeping = true;
            Duration::ZERO
        }
        Failure::RateLimited { retry_after } => retry_after,
        Failure::Unavailable | Failure::TimedOut | Failure::Cancelled => Duration::ZERO,
    };
    let delay = entry.backoff.max(floor);
    entry.phase = Phase::Retrying { due: env.now.saturating_add(delay), generation, keeping };
    entry.backoff = entry.backoff.saturating_mul(2).min(env.limits.backoff_max);
}

fn changed(domain: &mut Domain, env: &Env<Limits>, account: u32, out: &mut Queue<Request>) {
    let entry = domain.accounts.get_mut(&account).expect("the event keeps its account");
    let live = match entry.phase {
        Phase::Fresh { .. } | Phase::Refreshing { .. } | Phase::Retrying { .. } => true,
        Phase::Revoked | Phase::Closing { .. } => false,
    };
    let unspent = match entry.spent {
        Some(until) => until <= env.now,
        None => true,
    };
    let valid = match entry.expires {
        Some(until) => until > env.now,
        None => false,
    };
    let usable = live && unspent && valid;
    if entry.usable != usable {
        entry.usable = usable;
        out.push(Request::Availability { account, usable });
    }
    let state = match entry.phase {
        Phase::Fresh { .. } => State::Fresh,
        Phase::Refreshing { .. } => State::Refreshing,
        Phase::Retrying { .. } => State::Retrying,
        Phase::Revoked => State::Revoked,
        Phase::Closing { .. } => State::Closing,
    };
    let fact = Fact {
        account,
        state,
        generation: entry.generation,
        usable,
        unsaved: entry.unsaved,
        spent_until: entry.spent,
        attention: entry.unsaved || (entry.spent_attention && !unspent) || state == State::Revoked,
    };
    if domain.facts.try_push(fact).is_err() {
        domain.lost = domain.lost.saturating_add(1);
    }
    let mut alarm = match entry.phase {
        Phase::Fresh { due } | Phase::Retrying { due, .. } => Some(due),
        Phase::Refreshing { .. } | Phase::Revoked | Phase::Closing { .. } => None,
    };
    for at in [entry.expires, entry.spent].into_iter().flatten() {
        if at > env.now {
            alarm = Some(match alarm {
                Some(old) => old.min(at),
                None => at,
            });
        }
    }
    match alarm {
        Some(at) => domain.alarms.arm(account, at).expect("one alarm for each configured account"),
        None => domain.alarms.cancel(account),
    }
}

fn add(
    domain: &mut Domain,
    env: &Env<Limits>,
    account: u32,
    generation: u64,
    valid: Option<Duration>,
    out: &mut Queue<Request>,
) -> bool {
    if domain.accounts.contains_key(&account) || domain.accounts.len() >= env.limits.accounts {
        out.push(Request::Refused { account });
        return false;
    }
    let Some(next_generation) = generation.checked_add(1) else {
        out.push(Request::Refused { account });
        return false;
    };
    let mut expires = None;
    if let Some(valid) = valid {
        expires = Some(env.now.saturating_add(valid));
    }
    let phase = match expires {
        Some(expires) => Phase::Fresh { due: refresh_due(expires, env) },
        None => Phase::Refreshing { generation: next_generation, keeping: false },
    };
    let entry = Account {
        generation,
        expires,
        phase,
        last_refresh: env.now,
        backoff: env.limits.backoff_base,
        spent: None,
        usable: false,
        unsaved: false,
        spent_attention: false,
    };
    let inserted = domain.accounts.insert(account, entry);
    assert!(inserted == Ok(None), "startup account has room and a unique name");
    match valid {
        Some(valid) => out.push(Request::Granted { grant: Grant { account, generation, valid } }),
        None => out.push(Request::Refresh { account, generation: next_generation }),
    }
    true
}
