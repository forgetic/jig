use alloc::boxed::Box;
use skein_lib::{Env, List, Map, Queue, Time, Token};

use crate::{
    Alert, Class, Classed, Effect, Entry, Event, Fact, Key, Limits, Outcome, Phase, Read, Record, RecordKey, Request,
    Requirement, Service, SystemEvent, SystemRequest, Topic, Verdict, Watch,
};

/// One fact refresh can answer sixteen waiting judges and announce a change.
pub const MAX_OUT: u32 = 32;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Question {
    requirement: Requirement,
    service: Service,
    freshness: u64,
}

/// Observability's bounded working set.
#[derive(Debug)]
pub struct Domain {
    facts: Map<Service, Fact>,
    subscriptions: Map<(Topic, u64), (u8, u8)>,
    watches: Map<u64, Watch>,
    staged: Map<Token, Effect>,
    effects: Map<Key, Entry>,
    judges: Map<Token, Question>,
    reads: Map<Token, u32>,
}

impl Domain {
    /// Allocates the configured working set before a step runs.
    #[must_use]
    pub fn new(limits: &Limits) -> Self {
        assert!(crate::worst_case(limits).is_some(), "connector limits fit");
        assert!(
            limits.judges <= 16 && limits.effects <= 16 && limits.services_per_watch <= 16,
            "one step's outputs fit MAX_OUT"
        );
        Self {
            facts: Map::with_capacity(limits.services),
            subscriptions: Map::with_capacity(limits.subscriptions),
            watches: Map::with_capacity(limits.watches),
            staged: Map::with_capacity(limits.staged),
            effects: Map::with_capacity(limits.effects),
            judges: Map::with_capacity(limits.judges),
            reads: Map::with_capacity(limits.reads),
        }
    }
}

fn wall_seconds(env: &Env<Limits>) -> u64 {
    env.wall.as_nanos() / 1_000_000_000
}

fn service_valid(service: &Service, limits: &Limits) -> bool {
    !service.environment.is_empty()
        && !service.name.is_empty()
        && u32::try_from(service.environment.len()).unwrap_or(u32::MAX) <= limits.name_bytes
        && u32::try_from(service.name.len()).unwrap_or(u32::MAX) <= limits.name_bytes
}

fn read_valid(read: &Read, limits: &Limits) -> bool {
    match read {
        Read::Logs { service, window, filter, max_bytes } => {
            service_valid(service, limits)
                && window.from <= window.through
                && u32::try_from(filter.len()).unwrap_or(u32::MAX) <= limits.answer_bytes
                && *max_bytes <= limits.answer_bytes
        }
        Read::Series { service, window, max_bytes } | Read::Fired { service, window, max_bytes } => {
            service_valid(service, limits) && window.from <= window.through && *max_bytes <= limits.answer_bytes
        }
    }
}

fn verdict(fact: Option<&Fact>, question: &Question, now: u64) -> Verdict {
    let Some(fact) = fact else {
        return Verdict::Wait;
    };
    if fact.observed > now || now.saturating_sub(fact.observed) > question.freshness {
        return Verdict::Wait;
    }
    match question.requirement {
        Requirement::HealthyReplicaElsewhere => {
            if fact.healthy_replicas >= 2 {
                Verdict::Met { observed: fact.observed }
            } else {
                Verdict::Refuse
            }
        }
        Requirement::LoadBelow { percent, for_seconds } => {
            if percent == 0 || percent > 100 {
                return Verdict::Refuse;
            }
            let start = now.saturating_sub(for_seconds);
            let mut reached_start = false;
            for sample in &fact.load {
                if sample.at <= start {
                    reached_start = true;
                }
                if sample.at >= start && sample.at <= now && sample.percent >= percent {
                    return Verdict::Refuse;
                }
            }
            if !reached_start || fact.load_percent >= percent {
                Verdict::Wait
            } else {
                Verdict::Met { observed: fact.observed }
            }
        }
    }
}

fn question(
    domain: &mut Domain,
    env: &Env<Limits>,
    token: Token,
    requirement: Requirement,
    service: Service,
    freshness: u64,
    out: &mut Queue<Request>,
) {
    if !service_valid(&service, &env.limits) {
        out.push(Request::Verdict { token, verdict: Verdict::Refuse });
        return;
    }
    let question = Question { requirement, service: service.clone(), freshness };
    let result = verdict(domain.facts.get(&service), &question, wall_seconds(env));
    out.push(Request::Verdict { token, verdict: result });
    if result == Verdict::Wait {
        if domain.judges.contains_key(&token) || domain.judges.len() < env.limits.judges {
            domain.judges.insert(token, question).expect("judge capacity checked");
            out.push(Request::System(SystemRequest::Facts { service }));
        }
    } else {
        domain.judges.remove(&token);
    }
}

fn fresh_fact(domain: &mut Domain, env: &Env<Limits>, service: Service, fact: Fact, out: &mut Queue<Request>) {
    if !service_valid(&service, &env.limits)
        || u32::try_from(fact.load.len()).unwrap_or(u32::MAX) > env.limits.samples_per_service
        || domain.facts.len() >= env.limits.services && !domain.facts.contains_key(&service)
    {
        return;
    }
    let old = domain.facts.insert(service.clone(), fact.clone()).expect("fact capacity checked");
    if old != Some(fact.clone()) {
        out.push(Request::Changed { service: service.clone() });
    }
    let health_changed = match &old {
        Some(prior) => prior.healthy_replicas != fact.healthy_replicas,
        None => false,
    };
    if health_changed {
        health_news(domain, env, &service, fact.healthy_replicas > 0, out);
    }
    let mut answered = List::with_capacity(env.limits.judges);
    for (token, pending) in &domain.judges {
        if pending.service == service {
            let result = verdict(domain.facts.get(&service), pending, wall_seconds(env));
            if result != Verdict::Wait {
                out.push(Request::Verdict { token: *token, verdict: result });
                answered.push(*token).expect("judge count fits");
            }
        }
    }
    for token in &answered {
        domain.judges.remove(token);
    }
}

fn news(domain: &Domain, env: &Env<Limits>, alert: Alert, own: bool, out: &mut Queue<Request>) {
    if own {
        return;
    }
    let mut subscribers = List::with_capacity(env.limits.subscribers_per_alert);
    for ((topic, task), (wake_at, keep_at)) in &domain.subscriptions {
        if topic == &Topic::Alerts(alert.service.clone()) {
            let class = if alert.severity >= *wake_at {
                Some(Class::Wake)
            } else if alert.severity >= *keep_at {
                Some(Class::Keep)
            } else {
                None
            };
            if let Some(class) = class {
                subscribers.push(Classed { task: *task, class }).expect("subscriber count fits");
            }
        }
    }
    if !subscribers.is_empty() {
        out.push(Request::News { alert, subscribers: subscribers.into_boxed() });
    }
}

fn health_news(domain: &Domain, env: &Env<Limits>, service: &Service, healthy: bool, out: &mut Queue<Request>) {
    let mut subscribers = List::with_capacity(env.limits.subscribers_per_alert);
    for ((topic, task), (wake_at, keep_at)) in &domain.subscriptions {
        if topic == &Topic::Health(service.clone()) {
            let severity = if healthy { 1 } else { 10 };
            let class = if severity >= *wake_at {
                Some(Class::Wake)
            } else if severity >= *keep_at {
                Some(Class::Keep)
            } else {
                None
            };
            if let Some(class) = class {
                subscribers.push(Classed { task: *task, class }).expect("subscriber count fits");
            }
        }
    }
    if !subscribers.is_empty() {
        out.push(Request::Health { service: service.clone(), healthy, subscribers: subscribers.into_boxed() });
    }
}

fn subscribe(
    domain: &mut Domain,
    env: &Env<Limits>,
    task: u64,
    topic: Topic,
    wake_at: u8,
    keep_at: u8,
    out: &mut Queue<Request>,
) {
    let service = match &topic {
        Topic::Alerts(service) | Topic::Health(service) => service,
    };
    if wake_at < keep_at || !service_valid(service, &env.limits) {
        return;
    }
    let key = (topic.clone(), task);
    if domain.subscriptions.contains_key(&key) || domain.subscriptions.len() < env.limits.subscriptions {
        domain.subscriptions.insert(key, (wake_at, keep_at)).expect("subscription fits");
        out.push(Request::Save { record: Record::Subscription { topic, task, wake_at, keep_at } });
    }
}

fn start_watch(
    domain: &mut Domain,
    env: &Env<Limits>,
    task: u64,
    services: Box<[Service]>,
    template: u16,
    out: &mut Queue<Request>,
) {
    let count = u32::try_from(services.len()).unwrap_or(u32::MAX);
    if count == 0
        || count > env.limits.services_per_watch
        || domain.watches.len() >= env.limits.watches
        || domain.subscriptions.len().saturating_add(count) > env.limits.subscriptions
    {
        return;
    }
    for service in &services {
        if !service_valid(service, &env.limits) {
            return;
        }
    }
    for service in &services {
        subscribe(domain, env, task, Topic::Alerts(service.clone()), 1, 0, out);
    }
    let watch = Watch { task, services, template, last_batch: None };
    domain.watches.insert(task, watch.clone()).expect("watch fits");
    out.push(Request::Save { record: Record::Watch(watch) });
}

fn stop_watch(domain: &mut Domain, task: u64, out: &mut Queue<Request>) {
    let Some(watch) = domain.watches.remove(&task) else {
        return;
    };
    for service in &watch.services {
        let topic = Topic::Alerts(service.clone());
        if domain.subscriptions.remove(&(topic.clone(), task)).is_some() {
            out.push(Request::Erase { key: RecordKey::Subscription(topic, task) });
        }
    }
    out.push(Request::Erase { key: RecordKey::Watch(task) });
}

fn wake_watch(
    domain: &mut Domain,
    env: &Env<Limits>,
    task: u64,
    batch: u64,
    alerts: Box<[u64]>,
    out: &mut Queue<Request>,
) {
    if u32::try_from(alerts.len()).unwrap_or(u32::MAX) > env.limits.alerts_per_batch {
        return;
    }
    let Some(watch) = domain.watches.get_mut(&task) else {
        return;
    };
    if watch.last_batch == Some(batch) {
        return;
    }
    watch.last_batch = Some(batch);
    out.push(Request::Save { record: Record::Watch(watch.clone()) });
    out.push(Request::Triage { watch: task, template: watch.template, batch, alerts });
}

fn restore(domain: &mut Domain, record: Record) {
    match record {
        Record::Subscription { topic, task, wake_at, keep_at } => {
            domain.subscriptions.insert((topic, task), (wake_at, keep_at)).expect("restored subscription fits");
        }
        Record::Watch(watch) => {
            domain.watches.insert(watch.task, watch).expect("restored watch fits");
        }
        Record::Outbox(entry) => {
            domain.effects.insert(entry.key, entry).expect("restored outbox fits");
        }
    }
}

fn applied(domain: &mut Domain, key: Key, attempt: u32, outcome: Outcome, out: &mut Queue<Request>) {
    let Some(entry) = domain.effects.get_mut(&key) else {
        return;
    };
    if attempt > entry.attempt || entry.phase == Phase::Kept || entry.phase == Phase::Held {
        return;
    }
    match outcome {
        Outcome::Made => {
            domain.effects.remove(&key);
            out.push(Request::Erase { key: RecordKey::Outbox(key) });
            out.push(Request::Outcome { key, outcome });
        }
        Outcome::Failed => {
            if attempt == entry.attempt && entry.phase == Phase::Sent {
                domain.effects.remove(&key);
                out.push(Request::Erase { key: RecordKey::Outbox(key) });
                out.push(Request::Outcome { key, outcome });
            }
        }
        Outcome::Uncertain => {
            if attempt == entry.attempt && entry.phase == Phase::Sent {
                entry.phase = Phase::Uncertain;
                let record = Record::Outbox(entry.clone());
                let rule = entry.effect.rule.clone();
                out.push(Request::Save { record });
                out.push(Request::System(SystemRequest::FindSilence { key, rule }));
            }
        }
    }
}

fn found(domain: &mut Domain, env: &Env<Limits>, key: Key, found: bool, out: &mut Queue<Request>) {
    let Some(entry) = domain.effects.get_mut(&key) else {
        return;
    };
    if entry.phase != Phase::Uncertain && entry.phase != Phase::Sent {
        return;
    }
    if found {
        domain.effects.remove(&key);
        out.push(Request::Erase { key: RecordKey::Outbox(key) });
        out.push(Request::Outcome { key, outcome: Outcome::Made });
    } else if wall_seconds(env) >= entry.deadline {
        if entry.attempt >= env.limits.max_attempts {
            entry.phase = Phase::Held;
            out.push(Request::Save { record: Record::Outbox(entry.clone()) });
            out.push(Request::Outcome { key, outcome: Outcome::Uncertain });
            return;
        }
        entry.phase = Phase::Kept;
        out.push(Request::Save { record: Record::Outbox(entry.clone()) });
        out.push(Request::Make { key });
    } else {
        entry.phase = Phase::Uncertain;
        out.push(Request::Save { record: Record::Outbox(entry.clone()) });
    }
}

fn make(domain: &mut Domain, env: &Env<Limits>, key: Key, out: &mut Queue<Request>) {
    if let Some(entry) = domain.effects.get_mut(&key)
        && entry.phase == Phase::Kept
    {
        entry.phase = Phase::Sent;
        entry.attempt = entry.attempt.checked_add(1).expect("attempt count fits");
        entry.deadline = wall_seconds(env).checked_add(env.limits.retry_after_seconds).expect("retry deadline fits");
        out.push(Request::Save { record: Record::Outbox(entry.clone()) });
        out.push(Request::System(SystemRequest::Silence { key, attempt: entry.attempt, effect: entry.effect.clone() }));
    }
}

/// Handles one routed event after the root reserved output room.
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event, out: &mut Queue<Request>) {
    assert!(out.room() >= MAX_OUT, "the root reserves MAX_OUT slots");
    match event {
        Event::Judge { token, requirement, service, freshness } => {
            question(domain, env, token, requirement, service, freshness, out);
        }
        Event::Read { token, read } => {
            let max = match &read {
                Read::Logs { max_bytes, .. } | Read::Series { max_bytes, .. } | Read::Fired { max_bytes, .. } => {
                    *max_bytes
                }
            };
            if read_valid(&read, &env.limits) && domain.reads.len() < env.limits.reads {
                domain.reads.insert(token, max).expect("read capacity checked");
                out.push(Request::System(SystemRequest::Read { token, read }));
            } else {
                out.push(Request::Refused { token });
            }
        }
        Event::Subscribe { task, topic, wake_at, keep_at } => {
            subscribe(domain, env, task, topic, wake_at, keep_at, out);
        }
        Event::Unsubscribe { task, topic } => {
            if domain.subscriptions.remove(&(topic.clone(), task)).is_some() {
                out.push(Request::Erase { key: RecordKey::Subscription(topic, task) });
            }
        }
        Event::StartWatch { task, services, template } => start_watch(domain, env, task, services, template, out),
        Event::StopWatch { task } => stop_watch(domain, task, out),
        Event::WakeWatch { task, batch, alerts } => wake_watch(domain, env, task, batch, alerts, out),
        Event::Describe { token, effect } => {
            if !effect.rule.is_empty()
                && u32::try_from(effect.rule.len()).unwrap_or(u32::MAX) <= env.limits.name_bytes
                && domain.staged.len() < env.limits.staged
            {
                domain.staged.insert(token, effect.clone()).expect("stage fits");
                out.push(Request::Described { token, effect });
            } else {
                out.push(Request::Refused { token });
            }
        }
        Event::Keep { token, key } => {
            if let Some(effect) = domain.staged.remove(&token) {
                if domain.effects.len() < env.limits.effects {
                    let entry = Entry { key, effect, attempt: 0, deadline: 0, phase: Phase::Kept };
                    domain.effects.insert(key, entry.clone()).expect("effect fits");
                    out.push(Request::Save { record: Record::Outbox(entry) });
                    out.push(Request::Make { key });
                } else {
                    out.push(Request::Refused { token });
                }
            }
        }
        Event::Drop { token } => {
            domain.staged.remove(&token);
        }
        Event::Make { key } => make(domain, env, key, out),
        Event::Restore { record } => restore(domain, record),
        Event::Restart => {
            for (key, entry) in &domain.effects {
                match entry.phase {
                    Phase::Kept => out.push(Request::Make { key: *key }),
                    Phase::Sent | Phase::Uncertain => out.push(Request::System(SystemRequest::FindSilence {
                        key: *key,
                        rule: entry.effect.rule.clone(),
                    })),
                    Phase::Held => {}
                }
            }
        }
        Event::System(system) => match system {
            SystemEvent::Fact { service, fact } => fresh_fact(domain, env, service, fact, out),
            SystemEvent::Alert { alert, own } => news(domain, env, alert, own, out),
            SystemEvent::ReadDone { token, bytes } => {
                if let Some(max) = domain.reads.remove(&token) {
                    let limit = usize::try_from(max).expect("u32 fits usize");
                    let size = bytes.len().min(limit);
                    let body = Box::<[u8]>::from(bytes.get(..size).expect("size within answer"));
                    out.push(Request::Answer { token, bytes: body });
                }
            }
            SystemEvent::Applied { key, attempt, outcome } => applied(domain, key, attempt, outcome, out),
            SystemEvent::Found { key, found: found_key } => found(domain, env, key, found_key, out),
        },
    }
}

/// Rechecks uncertain keyed effects when their absolute retry deadline passes.
pub fn fire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    assert!(out.room() >= MAX_OUT, "the root reserves MAX_OUT slots");
    for (key, entry) in &domain.effects {
        if entry.phase == Phase::Uncertain && wall_seconds(env) >= entry.deadline {
            out.push(Request::System(SystemRequest::FindSilence { key: *key, rule: entry.effect.rule.clone() }));
        }
    }
}

/// Earliest absolute deadline in the world's clock domain.
#[must_use]
#[expect(clippy::manual_map, reason = "step code uses no closure-taking methods")]
pub fn next_deadline(domain: &Domain) -> Option<Time> {
    let mut soonest = None;
    for (_, entry) in &domain.effects {
        if entry.phase == Phase::Uncertain {
            let at = entry.deadline.checked_mul(1_000_000_000)?;
            soonest = Some(match soonest {
                Some(earlier) if earlier < at => earlier,
                Some(_) | None => at,
            });
        }
    }
    match soonest {
        Some(at) => Some(Time::from_nanos(at)),
        None => None,
    }
}
