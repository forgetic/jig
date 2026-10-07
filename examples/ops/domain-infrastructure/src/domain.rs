use alloc::boxed::Box;
use skein_lib::{Env, List, Map, Queue, Time, Token};

use crate::{
    ApplyResult, Backend, Description, Effect, Entry, Environment, EnvironmentFact, Event, Form, Hold, Key, Limits,
    Looked, Named, Outcome, Phase, Pool, Procedure, ProcedurePhase, ProcedureSignal, ProcedureState, Record, RecordKey,
    Recovery, Request, Resource, Service, ServiceFact, StepDecision, SystemEvent, SystemRequest,
};

/// One fact may report drift to sixteen tasks, plus a change and a save.
pub const MAX_OUT: u32 = 40;

/// Infrastructure's bounded working set and durable records.
#[derive(Debug)]
pub struct Domain {
    backend: Backend,
    services: Map<Service, ServiceFact>,
    environments: Map<Environment, Option<EnvironmentFact>>,
    pools: Map<Pool, (u32, u32)>,
    reliance: Map<u64, Box<[Resource]>>,
    procedures: Map<u64, ProcedureState>,
    staged: Map<Token, Effect>,
    effects: Map<Key, Entry>,
    made: Map<Environment, Key>,
}

impl Domain {
    /// Allocates the connector's tables and chooses the backend's recovery class.
    #[must_use]
    pub fn new(backend: Backend, limits: &Limits) -> Self {
        assert!(crate::worst_case(limits).is_some(), "connector limits fit");
        assert!(
            limits.tasks <= 16 && limits.effects <= 16 && limits.resources_per_task <= 16,
            "MAX_OUT covers one step"
        );
        Self {
            backend,
            services: Map::with_capacity(limits.services),
            environments: Map::with_capacity(limits.environments),
            pools: Map::with_capacity(limits.pools),
            reliance: Map::with_capacity(limits.tasks),
            procedures: Map::with_capacity(limits.procedures),
            staged: Map::with_capacity(limits.staged),
            effects: Map::with_capacity(limits.effects),
            made: Map::with_capacity(limits.made),
        }
    }
}

fn wall_seconds(env: &Env<Limits>) -> u64 {
    env.wall.as_nanos() / 1_000_000_000
}

fn bytes_valid(bytes: &[u8], limits: &Limits) -> bool {
    !bytes.is_empty() && u32::try_from(bytes.len()).unwrap_or(u32::MAX) <= limits.name_bytes
}

fn resource_valid(resource: &Resource, limits: &Limits) -> bool {
    match resource {
        Resource::Service(service) => bytes_valid(&service.environment, limits) && bytes_valid(&service.name, limits),
        Resource::Environment(environment) => {
            bytes_valid(&environment.pool, limits) && bytes_valid(&environment.name, limits)
        }
        Resource::Pool(pool) | Resource::Quota(pool) => bytes_valid(&pool.0, limits),
    }
}

fn hold(resource: &Resource) -> Hold {
    match resource {
        Resource::Service(..) | Resource::Environment(..) => Hold::ExclusiveWait,
        Resource::Pool(..) => Hold::PooledWait,
        Resource::Quota(..) => Hold::None,
    }
}

fn effect_valid(effect: &Effect, limits: &Limits) -> bool {
    match effect {
        Effect::Restart { service, .. } | Effect::Scale { service, .. } => {
            resource_valid(&Resource::Service(service.clone()), limits)
        }
        Effect::Rollback { service, from, to } => {
            resource_valid(&Resource::Service(service.clone()), limits)
                && bytes_valid(from, limits)
                && bytes_valid(to, limits)
        }
        Effect::CreateEnvironment { environment, .. } | Effect::TearDown { environment, .. } => {
            resource_valid(&Resource::Environment(environment.clone()), limits)
        }
    }
}

fn describe(effect: &Effect, backend: Backend) -> Description {
    match effect {
        Effect::Restart { service, operation } => Description {
            kind: 1,
            resources: Box::from([Resource::Service(service.clone())]),
            condition: false,
            form: Form::Transition,
            recovery: if backend == Backend::OperationIds { Recovery::Keyed } else { Recovery::Unrecoverable },
            price: None,
            state: *operation,
        },
        Effect::Scale { service, from: _, to } => Description {
            kind: 2,
            resources: Box::from([Resource::Service(service.clone())]),
            condition: true,
            form: Form::Set,
            recovery: Recovery::Conditional,
            price: None,
            state: u64::from(*to),
        },
        Effect::Rollback { service, from: _, to: _ } => Description {
            kind: 3,
            resources: Box::from([Resource::Service(service.clone())]),
            condition: true,
            form: Form::Transition,
            recovery: Recovery::Conditional,
            price: None,
            state: 0,
        },
        Effect::CreateEnvironment { environment, until, price } => Description {
            kind: 4,
            resources: Box::from([
                Resource::Environment(environment.clone()),
                Resource::Pool(Pool(environment.pool.clone())),
            ]),
            condition: false,
            form: Form::Creation,
            recovery: Recovery::Keyed,
            price: Some(*price),
            state: *until,
        },
        Effect::TearDown { environment, created_by: _ } => Description {
            kind: 5,
            resources: Box::from([Resource::Environment(environment.clone())]),
            condition: true,
            form: Form::Transition,
            recovery: Recovery::Conditional,
            price: None,
            state: 0,
        },
    }
}

fn names(domain: &mut Domain, env: &Env<Limits>, task: u64, resources: Box<[Resource]>, out: &mut Queue<Request>) {
    if u32::try_from(resources.len()).unwrap_or(u32::MAX) > env.limits.resources_per_task
        || domain.reliance.len() >= env.limits.tasks && !domain.reliance.contains_key(&task)
    {
        return;
    }
    for resource in &resources {
        if !resource_valid(resource, &env.limits) {
            return;
        }
    }
    let mut named = List::with_capacity(env.limits.resources_per_task);
    for resource in &resources {
        named.push(Named { resource: resource.clone(), hold: hold(resource) }).expect("resource count checked");
    }
    domain.reliance.insert(task, resources.clone()).expect("task capacity checked");
    out.push(Request::Save { record: Record::Rely { task, resources } });
    out.push(Request::Named { task, resources: named.into_boxed() });
}

fn drift(domain: &Domain, resource: Resource, out: &mut Queue<Request>) {
    for (task, resources) in &domain.reliance {
        if resources.contains(&resource) {
            out.push(Request::Drift { task: *task, resource: resource.clone() });
        }
    }
}

fn fresh_service(
    domain: &mut Domain,
    env: &Env<Limits>,
    service: Service,
    fact: ServiceFact,
    other_hand: bool,
    out: &mut Queue<Request>,
) {
    if !resource_valid(&Resource::Service(service.clone()), &env.limits)
        || domain.services.len() >= env.limits.services && !domain.services.contains_key(&service)
    {
        return;
    }
    let old = domain.services.insert(service.clone(), fact.clone()).expect("service capacity checked");
    if old != Some(fact.clone()) {
        out.push(Request::Changed { resource: Resource::Service(service.clone()) });
    }
    let revised = match old {
        Some(before) => before.revision != fact.revision,
        None => false,
    };
    if other_hand && revised {
        drift(domain, Resource::Service(service), out);
    }
}

fn fresh_environment(
    domain: &mut Domain,
    env: &Env<Limits>,
    environment: Environment,
    fact: Option<EnvironmentFact>,
    other_hand: bool,
    out: &mut Queue<Request>,
) {
    if !resource_valid(&Resource::Environment(environment.clone()), &env.limits)
        || domain.environments.len() >= env.limits.environments && !domain.environments.contains_key(&environment)
    {
        return;
    }
    let old = domain.environments.insert(environment.clone(), fact).expect("environment capacity checked");
    if old != Some(fact) {
        out.push(Request::Changed { resource: Resource::Environment(environment.clone()) });
    }
    if other_hand && fact.is_none() && domain.made.contains_key(&environment) {
        drift(domain, Resource::Environment(environment), out);
    }
}

fn fresh_pool(domain: &mut Domain, env: &Env<Limits>, pool: Pool, quota: u32, used: u32, out: &mut Queue<Request>) {
    if !resource_valid(&Resource::Pool(pool.clone()), &env.limits)
        || domain.pools.len() >= env.limits.pools && !domain.pools.contains_key(&pool)
    {
        return;
    }
    let old = domain.pools.insert(pool.clone(), (quota, used)).expect("pool capacity checked");
    if old != Some((quota, used)) {
        out.push(Request::Slots { pool: pool.clone(), quota, used });
        out.push(Request::Changed { resource: Resource::Quota(pool) });
    }
}

fn restore(domain: &mut Domain, record: Record) {
    match record {
        Record::Rely { task, resources } => {
            domain.reliance.insert(task, resources).expect("restored task fits");
        }
        Record::Procedure(state) => {
            domain.procedures.insert(state.task, state).expect("restored procedure fits");
        }
        Record::Outbox(entry) => {
            domain.effects.insert(entry.key, entry).expect("restored effect fits");
        }
        Record::Made { environment, key } => {
            domain.made.insert(environment, key).expect("restored owner fits");
        }
    }
}

fn make(domain: &mut Domain, env: &Env<Limits>, key: Key, out: &mut Queue<Request>) {
    if let Some(entry) = domain.effects.get_mut(&key)
        && entry.phase == Phase::Kept
    {
        entry.phase = Phase::Sent;
        entry.attempt = entry.attempt.checked_add(1).expect("attempt fits");
        entry.deadline = wall_seconds(env).checked_add(env.limits.retry_after_seconds).expect("deadline fits");
        out.push(Request::Save { record: Record::Outbox(entry.clone()) });
        out.push(Request::System(SystemRequest::Apply { key, attempt: entry.attempt, effect: entry.effect.clone() }));
    }
}

fn settled(domain: &mut Domain, key: Key, outcome: Outcome, out: &mut Queue<Request>) {
    let Some(entry) = domain.effects.remove(&key) else {
        return;
    };
    out.push(Request::Erase { key: RecordKey::Outbox(key) });
    if outcome == Outcome::Made {
        match entry.effect {
            Effect::CreateEnvironment { environment, .. } => {
                domain.made.insert(environment.clone(), key).expect("made capacity reserved");
                out.push(Request::Save { record: Record::Made { environment, key } });
            }
            Effect::TearDown { environment, .. } => {
                domain.made.remove(&environment);
                out.push(Request::Erase { key: RecordKey::Made(environment) });
            }
            Effect::Restart { .. } | Effect::Scale { .. } | Effect::Rollback { .. } => {}
        }
    }
    out.push(Request::Outcome { key, outcome });
}

fn applied(domain: &mut Domain, key: Key, attempt: u32, result: ApplyResult, out: &mut Queue<Request>) {
    let Some(entry) = domain.effects.get_mut(&key) else {
        return;
    };
    if attempt > entry.attempt || entry.phase == Phase::Kept || entry.phase == Phase::Held {
        return;
    }
    match result {
        ApplyResult::Made => settled(domain, key, Outcome::Made, out),
        ApplyResult::Conflict | ApplyResult::Missing | ApplyResult::Full | ApplyResult::Error => {
            if attempt == entry.attempt && entry.phase == Phase::Sent {
                settled(domain, key, Outcome::Failed, out);
            }
        }
        ApplyResult::Uncertain => {
            if attempt == entry.attempt && entry.phase == Phase::Sent {
                let recovery = describe(&entry.effect, domain.backend).recovery;
                if recovery == Recovery::Unrecoverable {
                    entry.phase = Phase::Held;
                    out.push(Request::Save { record: Record::Outbox(entry.clone()) });
                    out.push(Request::Outcome { key, outcome: Outcome::Uncertain });
                } else {
                    entry.phase = Phase::Uncertain;
                    out.push(Request::Save { record: Record::Outbox(entry.clone()) });
                    out.push(Request::System(SystemRequest::Look { key, effect: entry.effect.clone() }));
                }
            }
        }
    }
}

fn looked(domain: &mut Domain, env: &Env<Limits>, key: Key, result: Looked, out: &mut Queue<Request>) {
    let Some(entry) = domain.effects.get_mut(&key) else {
        return;
    };
    if entry.phase != Phase::Sent && entry.phase != Phase::Uncertain {
        return;
    }
    match result {
        Looked::Made => settled(domain, key, Outcome::Made, out),
        Looked::Ambiguous => {
            entry.phase = Phase::Held;
            out.push(Request::Save { record: Record::Outbox(entry.clone()) });
            out.push(Request::Outcome { key, outcome: Outcome::Uncertain });
        }
        Looked::CanRetry => {
            if wall_seconds(env) >= entry.deadline {
                if entry.attempt >= env.limits.max_attempts {
                    entry.phase = Phase::Held;
                    out.push(Request::Save { record: Record::Outbox(entry.clone()) });
                    out.push(Request::Outcome { key, outcome: Outcome::Uncertain });
                } else {
                    entry.phase = Phase::Kept;
                    out.push(Request::Save { record: Record::Outbox(entry.clone()) });
                    out.push(Request::Make { key });
                }
            } else {
                entry.phase = Phase::Uncertain;
                out.push(Request::Save { record: Record::Outbox(entry.clone()) });
            }
        }
    }
}

fn start_procedure(domain: &mut Domain, env: &Env<Limits>, task: u64, procedure: Procedure, out: &mut Queue<Request>) {
    if domain.procedures.len() >= env.limits.procedures {
        return;
    }
    let state = ProcedureState { task, procedure, phase: ProcedurePhase::Active };
    domain.procedures.insert(task, state.clone()).expect("procedure capacity checked");
    out.push(Request::Save { record: Record::Procedure(state) });
}

fn procedure(domain: &mut Domain, env: &Env<Limits>, task: u64, signal: ProcedureSignal, out: &mut Queue<Request>) {
    let Some(mut state) = domain.procedures.get(&task).cloned() else {
        return;
    };
    let before = state.clone();
    let now = wall_seconds(env);
    match signal {
        ProcedureSignal::Step => {}
        ProcedureSignal::EffectMade => {
            if state.phase == ProcedurePhase::EffectOutstanding {
                state.phase = ProcedurePhase::WaitingCondition;
            }
        }
        ProcedureSignal::EffectFailed => {
            state.phase = ProcedurePhase::Held;
        }
        ProcedureSignal::Release => match &state.procedure {
            Procedure::Provision { environment, .. } => {
                let Some(key) = domain.made.get(environment) else {
                    return;
                };
                state.procedure = Procedure::TearDown {
                    environment: environment.clone(),
                    created_by: *key,
                    deadline: now.saturating_add(30),
                };
                state.phase = ProcedurePhase::Active;
            }
            Procedure::Remediate { .. } | Procedure::TearDown { .. } => {
                state.phase = ProcedurePhase::Done;
            }
        },
    }
    let (phase, decision, read) = decide(domain, now, &state);
    state.phase = phase;
    if state.phase == ProcedurePhase::Done {
        domain.procedures.remove(&task);
        out.push(Request::Erase { key: RecordKey::Procedure(task) });
    } else {
        domain.procedures.insert(task, state.clone()).expect("procedure remains within capacity");
        if state != before {
            out.push(Request::Save { record: Record::Procedure(state) });
        }
    }
    if let Some(read) = read {
        out.push(Request::System(read));
    }
    if let Some(decision) = decision {
        out.push(Request::Step { task, decision });
    }
}

#[expect(clippy::too_many_lines, reason = "three procedures share one level-triggered decision point")]
fn decide(
    domain: &Domain,
    now: u64,
    state: &ProcedureState,
) -> (ProcedurePhase, Option<StepDecision>, Option<SystemRequest>) {
    match &state.procedure {
        Procedure::Remediate { service, operation, deadline } => {
            if state.phase == ProcedurePhase::Held || state.phase == ProcedurePhase::Done {
                return (state.phase, None, None);
            }
            if now > *deadline {
                return (ProcedurePhase::Held, Some(StepDecision::Hold { reason: 1 }), None);
            }
            let Some(fact) = domain.services.get(service) else {
                return (
                    state.phase,
                    Some(StepDecision::Wait { until: *deadline }),
                    Some(SystemRequest::Service { service: service.clone() }),
                );
            };
            if fact.healthy {
                return (ProcedurePhase::Done, Some(StepDecision::Finish), None);
            }
            match state.phase {
                ProcedurePhase::Active => (
                    ProcedurePhase::EffectOutstanding,
                    Some(StepDecision::Effect(Effect::Restart { service: service.clone(), operation: *operation })),
                    None,
                ),
                ProcedurePhase::EffectOutstanding => (state.phase, Some(StepDecision::Wait { until: *deadline }), None),
                ProcedurePhase::WaitingCondition => (
                    state.phase,
                    Some(StepDecision::Wait { until: *deadline }),
                    Some(SystemRequest::Service { service: service.clone() }),
                ),
                ProcedurePhase::Holding | ProcedurePhase::Done | ProcedurePhase::Held => (state.phase, None, None),
            }
        }
        Procedure::Provision { environment, until, price, deadline } => {
            if state.phase == ProcedurePhase::Holding || state.phase == ProcedurePhase::Held {
                return (state.phase, None, None);
            }
            if now > *deadline {
                return (ProcedurePhase::Held, Some(StepDecision::Hold { reason: 2 }), None);
            }
            let fact = domain.environments.get(environment);
            match fact {
                Some(Some(found)) if found.ready_at <= now => {
                    (ProcedurePhase::Holding, Some(StepDecision::Finish), None)
                }
                Some(Some(_)) => (
                    ProcedurePhase::WaitingCondition,
                    Some(StepDecision::Wait { until: *deadline }),
                    Some(SystemRequest::Environment { environment: environment.clone() }),
                ),
                Some(None) => match state.phase {
                    ProcedurePhase::Active => (
                        ProcedurePhase::EffectOutstanding,
                        Some(StepDecision::Effect(Effect::CreateEnvironment {
                            environment: environment.clone(),
                            until: *until,
                            price: *price,
                        })),
                        None,
                    ),
                    ProcedurePhase::EffectOutstanding => {
                        (state.phase, Some(StepDecision::Wait { until: *deadline }), None)
                    }
                    ProcedurePhase::WaitingCondition => (
                        state.phase,
                        Some(StepDecision::Wait { until: *deadline }),
                        Some(SystemRequest::Environment { environment: environment.clone() }),
                    ),
                    ProcedurePhase::Holding | ProcedurePhase::Done | ProcedurePhase::Held => (state.phase, None, None),
                },
                None => (
                    state.phase,
                    Some(StepDecision::Wait { until: *deadline }),
                    Some(SystemRequest::Environment { environment: environment.clone() }),
                ),
            }
        }
        Procedure::TearDown { environment, created_by, deadline } => {
            if state.phase == ProcedurePhase::Held || state.phase == ProcedurePhase::Done {
                return (state.phase, None, None);
            }
            if now > *deadline {
                return (ProcedurePhase::Held, Some(StepDecision::Hold { reason: 3 }), None);
            }
            match domain.environments.get(environment) {
                Some(None) => (ProcedurePhase::Done, Some(StepDecision::Finish), None),
                Some(Some(_)) => match state.phase {
                    ProcedurePhase::Active => (
                        ProcedurePhase::EffectOutstanding,
                        Some(StepDecision::Effect(Effect::TearDown {
                            environment: environment.clone(),
                            created_by: *created_by,
                        })),
                        None,
                    ),
                    ProcedurePhase::EffectOutstanding => {
                        (state.phase, Some(StepDecision::Wait { until: *deadline }), None)
                    }
                    ProcedurePhase::WaitingCondition => (
                        state.phase,
                        Some(StepDecision::Wait { until: *deadline }),
                        Some(SystemRequest::Environment { environment: environment.clone() }),
                    ),
                    ProcedurePhase::Holding | ProcedurePhase::Done | ProcedurePhase::Held => (state.phase, None, None),
                },
                None => (
                    state.phase,
                    Some(StepDecision::Wait { until: *deadline }),
                    Some(SystemRequest::Environment { environment: environment.clone() }),
                ),
            }
        }
    }
}

/// Routes one event with room reserved for its worst case.
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event, out: &mut Queue<Request>) {
    assert!(out.room() >= MAX_OUT, "the root reserves MAX_OUT slots");
    match event {
        Event::Names { task, resources } => names(domain, env, task, resources, out),
        Event::Unnamed { task } => {
            if domain.reliance.remove(&task).is_some() {
                out.push(Request::Erase { key: RecordKey::Rely(task) });
            }
        }
        Event::StartProcedure { task, procedure } => start_procedure(domain, env, task, procedure, out),
        Event::Procedure { task, signal } => procedure(domain, env, task, signal, out),
        Event::Describe { token, effect } => {
            if effect_valid(&effect, &env.limits) && domain.staged.len() < env.limits.staged {
                let description = describe(&effect, domain.backend);
                domain.staged.insert(token, effect).expect("stage capacity checked");
                out.push(Request::Described { token, description });
            } else {
                out.push(Request::Refused { token });
            }
        }
        Event::Keep { token, key } => {
            if let Some(effect) = domain.staged.remove(&token) {
                let owner_room = match &effect {
                    Effect::CreateEnvironment { environment, .. } => {
                        domain.made.len() < env.limits.made || domain.made.contains_key(environment)
                    }
                    Effect::Restart { .. }
                    | Effect::Scale { .. }
                    | Effect::Rollback { .. }
                    | Effect::TearDown { .. } => true,
                };
                if domain.effects.len() < env.limits.effects && !domain.effects.contains_key(&key) && owner_room {
                    let entry = Entry { key, effect, attempt: 0, deadline: 0, phase: Phase::Kept };
                    domain.effects.insert(key, entry.clone()).expect("outbox capacity checked");
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
                    Phase::Sent | Phase::Uncertain => {
                        out.push(Request::System(SystemRequest::Look { key: *key, effect: entry.effect.clone() }));
                    }
                    Phase::Held => {}
                }
            }
        }
        Event::System(system) => match system {
            SystemEvent::Service { service, fact, other_hand } => {
                fresh_service(domain, env, service, fact, other_hand, out);
            }
            SystemEvent::Environment { environment, fact, other_hand } => {
                fresh_environment(domain, env, environment, fact, other_hand, out);
            }
            SystemEvent::Pool { pool, quota, used } => fresh_pool(domain, env, pool, quota, used, out),
            SystemEvent::Applied { key, attempt, result } => applied(domain, key, attempt, result, out),
            SystemEvent::Looked { key, result } => looked(domain, env, key, result, out),
        },
    }
}

/// Rechecks uncertain entries once their absolute retry deadline passes.
pub fn fire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    assert!(out.room() >= MAX_OUT, "the root reserves MAX_OUT slots");
    for (key, entry) in &domain.effects {
        if entry.phase == Phase::Uncertain && wall_seconds(env) >= entry.deadline {
            out.push(Request::System(SystemRequest::Look { key: *key, effect: entry.effect.clone() }));
        }
    }
}

/// Earliest absolute retry deadline in the world's clock domain.
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
