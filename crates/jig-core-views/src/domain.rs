//! The views state and its steps (domain/engine.md, 11).

use alloc::boxed::Box;

use skein_lib::{Env, Id, Map, Queue, Slab, Time, Token};

use crate::boundary::{Event, Kind, Request, Subject};
use crate::facts::{Dropped, Fact, Facts, Loss, Lost};
use crate::limits::{self, Limits};
use crate::watch::{self, Head, Watcher};

/// Bounded live streams. Nothing here is restored.
#[derive(Debug)]
pub struct Domain {
    pub(crate) runs: Map<Token, Token>,
    pub(crate) unfollowed: Map<Token, Token>,
    pub(crate) watchers: Slab<Watcher>,
    pub(crate) names: Map<Token, Id<Watcher>>,
    pub(crate) facts: Facts,
}

impl Domain {
    /// A fresh set of live views.
    #[must_use]
    pub fn new(limits: &Limits) -> Domain {
        let slots = limits::slots(limits).expect("worst_case accepted the limits");
        Domain {
            runs: Map::with_capacity(limits.runs),
            unfollowed: Map::with_capacity(limits.runs),
            watchers: Slab::with_capacity(slots),
            names: Map::with_capacity(limits.watchers),
            facts: Facts::with_capacity(limits.facts),
        }
    }

    /// Runs followed.
    #[must_use]
    pub fn runs(&self) -> u32 {
        self.runs.len()
    }

    /// Watches open, including those ending before reclaim.
    #[must_use]
    pub fn watchers(&self) -> u32 {
        self.watchers.len()
    }

    /// What the views lost since startup.
    #[must_use]
    pub fn lost(&self) -> Lost {
        self.facts.lost()
    }

    /// Views have no timers.
    #[must_use]
    pub const fn next_deadline(&self) -> Option<Time> {
        None
    }

    /// Views have no timers.
    #[must_use]
    pub const fn is_due(&self, _now: Time) -> bool {
        false
    }

    /// The oldest best effort fact.
    pub fn pop_fact(&mut self) -> Option<Fact> {
        self.facts.pop()
    }

    /// Count of facts dropped.
    #[must_use]
    pub fn facts_lost(&self) -> u64 {
        self.facts.lost().facts
    }

    /// Free watches that ended in this iteration.
    pub fn reclaim(&mut self) {
        self.watchers.reclaim();
    }
}

/// At most one request per watcher, plus admission.
#[must_use]
pub const fn max_out(limits: &Limits) -> u32 {
    limits.watchers.saturating_add(1)
}

/// Apply one event.
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event, out: &mut Queue<Request>) {
    match event {
        Event::Started { task, attempt } => started(domain, task, attempt, out),
        Event::Reported { task, attempt, kind, content } => reported(domain, env, task, attempt, kind, content, out),
        Event::Turn { task, attempt, number } => turned(domain, env, task, attempt, number, out),
        Event::Finished { task } => finished(domain, task, out),
        Event::TaskPhase { task, trees, project, phase, priority } => {
            task_changed(domain, env, task, &trees, project, phase, priority, out);
        }
        Event::Inbox { party, content } => inbox(domain, env, party, content, out),
        Event::Watch { watcher, subject, snapshot } => watch::watch(domain, env, watcher, subject, snapshot, out),
        Event::Unwatch { watcher } => watch::unwatch(domain, watcher, out),
        Event::Delivered { watcher, done } => watch::delivered(domain, watcher, done, out),
    }
}

/// Views have no deadline; retained for child-domain shape.
pub fn fire(_domain: &mut Domain, _env: &Env<Limits>, _out: &mut Queue<Request>) {}

fn started(domain: &mut Domain, task: Token, attempt: Token, out: &mut Queue<Request>) {
    if let Some(&prior) = domain.runs.get(&task)
        && prior != attempt
    {
        watch::finish(domain, Subject::Run { task, attempt: prior }, out);
    }
    if domain.runs.insert(task, attempt).is_ok() {
        domain.unfollowed.remove(&task);
        domain.facts.push(Fact::Followed);
    } else {
        if domain.unfollowed.contains_key(&task) || domain.unfollowed.len() < domain.unfollowed.capacity() {
            domain.unfollowed.insert(task, attempt).expect("room for a run turned away");
        }
        domain.facts.lose(Loss::Runs, 1);
        domain.facts.push(Fact::Unfollowed);
    }
}

fn reported(
    domain: &mut Domain,
    env: &Env<Limits>,
    task: Token,
    attempt: Token,
    kind: Kind,
    content: Box<[u8]>,
    out: &mut Queue<Request>,
) {
    let within = match u32::try_from(content.len()) {
        Ok(len) => len <= env.limits.report_bytes,
        Err(_) => false,
    };
    if domain.runs.get(&task) == Some(&attempt) {
        let run = Subject::Run { task, attempt };
        let tree = Subject::Tree { task };
        if within {
            let head = Head::Report { task, attempt, kind, at: env.now };
            let watchers = watch::offer(domain, run, tree, head, &content, out);
            domain.facts.push(Fact::Reported { watchers });
            return;
        }
        watch::miss(domain, run, tree);
    } else if domain.unfollowed.get(&task) == Some(&attempt) {
        watch::miss(domain, Subject::Tree { task }, Subject::Tree { task });
    }
    domain.facts.lose(Loss::Reports, 1);
    let dropped = if within { Dropped::Unfollowed } else { Dropped::Oversized };
    domain.facts.push(Fact::Dropped { dropped });
}

fn turned(domain: &mut Domain, env: &Env<Limits>, task: Token, attempt: Token, number: u32, out: &mut Queue<Request>) {
    if domain.runs.get(&task) != Some(&attempt) || number == 0 {
        return;
    }
    let head = Head::Report { task, attempt, kind: Kind::Progress, at: env.now };
    let watchers =
        watch::offer(domain, Subject::Run { task, attempt }, Subject::Tree { task }, head, &number.to_be_bytes(), out);
    domain.facts.push(Fact::Turn { watchers });
}

fn finished(domain: &mut Domain, task: Token, out: &mut Queue<Request>) {
    if let Some(attempt) = domain.runs.remove(&task) {
        watch::finish(domain, Subject::Run { task, attempt }, out);
    } else {
        domain.unfollowed.remove(&task);
    }
}

#[expect(clippy::too_many_arguments, reason = "one task change names its trees and goal")]
fn task_changed(
    domain: &mut Domain,
    env: &Env<Limits>,
    task: Token,
    trees: &[Token],
    project: u32,
    phase: u32,
    priority: Option<u32>,
    out: &mut Queue<Request>,
) {
    let mut watchers = 0_u32;
    let head = Head::Phase { task, phase, at: env.now };
    for tree in trees {
        let subject = Subject::Tree { task: *tree };
        watchers = watchers.saturating_add(watch::offer(domain, subject, subject, head, &[], out));
    }
    if let Some(priority) = priority {
        let head = Head::Report { task, attempt: Token::new(0), kind: Kind::Progress, at: env.now };
        let a = phase.to_be_bytes();
        let b = priority.to_be_bytes();
        let content = [a[0], a[1], a[2], a[3], b[0], b[1], b[2], b[3]];
        let subject = Subject::Goals { project };
        watchers = watchers.saturating_add(watch::offer(domain, subject, subject, head, &content, out));
    }
    domain.facts.push(Fact::Changed { watchers });
}

fn inbox(domain: &mut Domain, env: &Env<Limits>, party: u64, content: Box<[u8]>, out: &mut Queue<Request>) {
    let within = match u32::try_from(content.len()) {
        Ok(len) => len <= env.limits.report_bytes,
        Err(_) => false,
    };
    if !within {
        domain.facts.lose(Loss::Reports, 1);
        domain.facts.push(Fact::Dropped { dropped: Dropped::Oversized });
        return;
    }
    let subject = Subject::Inbox { party };
    let head = Head::Inbox { party, at: env.now };
    let watchers = watch::offer(domain, subject, subject, head, &content, out);
    domain.facts.push(Fact::Changed { watchers });
}
