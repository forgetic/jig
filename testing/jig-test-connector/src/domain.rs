use alloc::boxed::Box;
use skein_lib::{Env, List, Map, Queue, Token, Wall};

use crate::outbox::Outbox;
use crate::{
    Adoption, Class, Classed, Config, Event, Hold, Limits, Named, Origin, Path, Record, RecordKey, Request,
    ResourceRole, ResourceSpec, SystemEvent,
};

/// Four pending judges plus a fact change, drift and a procedure save fit.
pub const MAX_OUT: u32 = 8;

/// A connector's live working set and records.
#[derive(Debug)]
pub struct Domain {
    pub(crate) config: Config,
    pub(crate) tasks: Map<u64, Record>,
    pub(crate) adoptions: Map<(u32, Path), ResourceRole>,
    subscriptions: Map<(u16, u64), (u16, u16)>,
    pools: Map<Path, u32>,
    pub(crate) facts: Map<Path, crate::Fact>,
    pub(crate) judges: Map<Token, crate::requirements::Question>,
    pub(crate) procedures: Map<u64, crate::ProcedureState>,
    pub(crate) results: Map<u64, u16>,
    pub(crate) values: Map<Token, crate::values::Payload>,
    pub(crate) outbox: Outbox,
    restart_settling: bool,
    closing: Map<u64, ()>,
}

impl Domain {
    /// Builds a connector from the world's seed-derived configuration.
    #[must_use]
    pub fn new(config: Config, limits: &Limits) -> Domain {
        assert!(crate::worst_case(limits).is_some(), "connector limits fit");
        assert!(u32::try_from(config.pools.len()).unwrap_or(u32::MAX) <= limits.pools, "pool configuration fits");
        assert!(
            u32::try_from(config.resources.len()).unwrap_or(u32::MAX) <= limits.resources,
            "resource configuration fits"
        );
        assert!(u32::try_from(config.topics.len()).unwrap_or(u32::MAX) <= limits.topics, "topic configuration fits");
        assert!(u32::try_from(config.kinds.len()).unwrap_or(u32::MAX) <= limits.kinds, "kind configuration fits");
        assert!(
            u32::try_from(config.requirements.len()).unwrap_or(u32::MAX) <= limits.requirements,
            "requirements fit"
        );
        assert!(u32::try_from(config.procedures.len()).unwrap_or(u32::MAX) <= limits.procedures, "procedures fit");
        for program in &config.procedures {
            assert!(
                u32::try_from(program.actions.len()).unwrap_or(u32::MAX) <= limits.actions_per_procedure,
                "procedure actions fit"
            );
            assert!(u16::try_from(program.actions.len()).is_ok(), "procedure index fits");
            let mut same = 0_u32;
            for other in &config.procedures {
                if other.number == program.number {
                    same = same.checked_add(1).expect("procedure count fits");
                }
            }
            assert!(same == 1, "procedure numbers are unique");
        }
        for requirement in &config.requirements {
            let mut same = 0_u32;
            for other in &config.requirements {
                if other.number == requirement.number {
                    same = same.checked_add(1).expect("requirement count fits");
                }
            }
            assert!(same == 1, "requirement numbers are unique");
        }
        assert!(valid_path(&config.prefix, limits), "deployment prefix fits");
        for resource in &config.resources {
            assert!(resource.path.under(&config.prefix), "resource uses the deployment prefix");
            assert!(valid_path(&resource.path, limits), "resource path fits");
            let mut same = 0_u32;
            for other in &config.resources {
                if other.path == resource.path {
                    same = same.checked_add(1).expect("resource count fits");
                }
            }
            assert!(same == 1, "resource paths are unique");
        }
        for topic in &config.topics {
            let mut same = 0_u32;
            for other in &config.topics {
                if other.topic == topic.topic {
                    same = same.checked_add(1).expect("topic count fits");
                }
            }
            assert!(same == 1, "topic numbers are unique");
        }
        for kind in &config.kinds {
            let mut same = 0_u32;
            for other in &config.kinds {
                if other.kind == kind.kind {
                    same = same.checked_add(1).expect("kind count fits");
                }
            }
            assert!(same == 1, "effect kinds are unique");
        }
        let mut pools = Map::with_capacity(limits.pools);
        for pool in &config.pools {
            assert!(pool.path.under(&config.prefix), "pool uses the deployment prefix");
            assert!(valid_path(&pool.path, limits), "pool path fits");
            assert!(pools.insert(pool.path.clone(), pool.slots) == Ok(None), "pool paths are unique");
        }
        Domain {
            config,
            closing: Map::with_capacity(limits.tasks),
            tasks: Map::with_capacity(limits.tasks),
            adoptions: Map::with_capacity(limits.adoptions),
            subscriptions: Map::with_capacity(limits.subscriptions),
            pools,
            facts: Map::with_capacity(limits.facts),
            judges: Map::with_capacity(limits.judges),
            procedures: Map::with_capacity(limits.procedures),
            results: Map::with_capacity(limits.procedures),
            values: Map::with_capacity(limits.values),
            outbox: Outbox::new(limits),
            restart_settling: false,
        }
    }

    /// The last known count for a pool, including the configuration's first count.
    #[must_use]
    pub fn slots(&self, pool: &Path) -> Option<u32> {
        self.pools.get(pool).copied()
    }

    /// The number of live task records, for bounds in the world's referee.
    #[must_use]
    pub fn live_tasks(&self) -> u32 {
        self.tasks.len()
    }
}

/// One input from the root or the fake system.
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event, out: &mut Queue<Request>) {
    assert!(out.room() >= MAX_OUT, "parent reserved the connector's maximum output");
    match event {
        Event::Close { task } => {
            domain.closing.insert(task, ()).expect("one closing slot per task");
        }
        Event::EffectDecision { task, made } => crate::procedures::effect_decided(domain, task, made, out),
        Event::Judge { token, requirement, resources, state } => {
            crate::requirements::judge(domain, env, token, requirement, &resources, state, out);
        }
        Event::Procedure { task, number, resource, signal } => {
            crate::procedures::step(domain, env, task, number, resource, signal, out);
        }
        Event::Read { token, read } => crate::values::read(domain, env, token, read, out),
        Event::Gather { token, task, budget } => crate::values::gather(domain, env, token, task, budget, out),
        Event::Cut { token, size } => crate::values::cut(domain, token, size, out),
        Event::Items { token, task } => crate::values::items(domain, env, token, task, out),
        Event::HandOver { token } => crate::values::hand_over(domain, token, out),
        Event::Restart(crate::RestartStep::Records) => {}
        Event::Restart(crate::RestartStep::FreshRead { resource }) => {
            out.push(Request::System(crate::SystemRequest::ReadFact { resource, observed: env.wall }));
        }
        Event::Restart(crate::RestartStep::SettleOutbox) => {
            domain.restart_settling = true;
            crate::outbox::fire(domain, env, out);
        }
        Event::Names { task, project, resources } => names(domain, env, task, project, resources, out),
        Event::Unnamed { task } => unnamed(domain, task, out),
        Event::Adopt { project, resource, role } => adopt(domain, env, project, resource, role, out),
        Event::Subscribe { task, topic, wake_at, keep_at } => {
            subscribe(domain, env, task, topic, wake_at, keep_at, out);
        }
        Event::Unsubscribe { task, topic } => unsubscribe(domain, task, topic, out),
        Event::Describe { token, effect } => crate::outbox::describe(domain, env, token, effect, out),
        Event::DescribeProposal { token, number } => crate::outbox::describe_proposal(domain, env, token, number, out),
        Event::KeepProposal { token, number, task } => crate::outbox::keep_proposal(domain, token, number, task, out),
        Event::DropProposal { number } => crate::outbox::drop_proposal(domain, number, out),
        Event::Keep { token, entry, task, key } => crate::outbox::keep(domain, env, token, entry, task, key, out),
        Event::Drop { token } => {
            crate::outbox::drop_staged(domain, token);
            crate::values::drop_value(domain, token);
            domain.judges.remove(&token);
        }
        Event::Make { entry } => crate::outbox::make(domain, env, entry, out),
        Event::Withdraw { entry } => crate::outbox::withdraw(domain, entry, out),
        Event::Restore { record } => restore(domain, record),
        Event::System(system) => system_event(domain, env, system, out),
    }
    restart_complete(domain, out);
    close_complete(domain, out);
}

/// A closing task can withdraw an unsent entry or finish its acknowledgement.
#[must_use]
pub fn closing_ready(domain: &Domain) -> bool {
    for (task, ()) in &domain.closing {
        if crate::outbox::close_ready(domain, *task) {
            return true;
        }
    }
    false
}

/// Finish at most one ready closing task after reserving the normal output room.
pub fn resume(domain: &mut Domain, out: &mut Queue<Request>) {
    assert!(out.room() >= MAX_OUT, "parent reserved closing output room");
    close_complete(domain, out);
}

fn close_complete(domain: &mut Domain, out: &mut Queue<Request>) {
    let mut ready = None;
    for (task, ()) in &domain.closing {
        if crate::outbox::close_ready(domain, *task) {
            ready = Some(*task);
            break;
        }
    }
    if let Some(task) = ready
        && crate::outbox::close(domain, task, out)
    {
        domain.closing.remove(&task);
        out.push(Request::Closed { task });
    }
}

fn restart_complete(domain: &mut Domain, out: &mut Queue<Request>) {
    if domain.restart_settling && crate::outbox::restart_settled(domain) {
        domain.restart_settling = false;
        out.push(Request::RestartDone);
    }
}

/// The earliest absolute retry deadline of an unsettled entry.
#[must_use]
pub fn next_deadline(domain: &Domain) -> Option<Wall> {
    let first = crate::outbox::next_deadline(domain);
    let other = crate::procedures::next_deadline(domain);
    match (first, other) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, right) => right,
    }
}

/// Advances at most one due outbox entry after the root reserved output room.
pub fn fire(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    assert!(out.room() >= MAX_OUT, "parent reserved the connector's maximum output");
    crate::outbox::fire(domain, env, out);
    restart_complete(domain, out);
}

pub(crate) fn valid_path(path: &Path, limits: &Limits) -> bool {
    if u32::try_from(path.segments().len()).unwrap_or(u32::MAX) > limits.path_segments {
        return false;
    }
    for segment in path.segments() {
        if segment.is_empty() || u32::try_from(segment.len()).unwrap_or(u32::MAX) > limits.segment_bytes {
            return false;
        }
    }
    true
}

#[expect(clippy::manual_find, reason = "step code uses no closures")]
pub(crate) fn resource_spec<'a>(config: &'a Config, path: &Path) -> Option<&'a ResourceSpec> {
    for spec in &config.resources {
        if spec.path == *path {
            return Some(spec);
        }
    }
    None
}

fn topic_known(config: &Config, topic: u16) -> bool {
    for spec in &config.topics {
        if spec.topic == topic {
            return true;
        }
    }
    false
}

fn names(
    domain: &mut Domain,
    env: &Env<Limits>,
    task: u64,
    project: u32,
    resources: Box<[Path]>,
    out: &mut Queue<Request>,
) {
    if u32::try_from(resources.len()).unwrap_or(u32::MAX) > env.limits.resources_per_task
        || domain.tasks.len() >= env.limits.tasks && !domain.tasks.contains_key(&task)
    {
        out.push(Request::Refused { task });
        return;
    }
    let mut named: List<Named> = List::with_capacity(env.limits.resources_per_task);
    for path in &resources {
        for prior in &named {
            if prior.path == *path {
                out.push(Request::Refused { task });
                return;
            }
        }
        if !path.under(&domain.config.prefix) || !valid_path(path, &env.limits) {
            out.push(Request::Unknown { task, resource: path.clone() });
            return;
        }
        let Some(spec) = resource_spec(&domain.config, path) else {
            out.push(Request::Unknown { task, resource: path.clone() });
            return;
        };
        let Some(role) = domain.adoptions.get(&(project, path.clone())) else {
            out.push(Request::Unknown { task, resource: path.clone() });
            return;
        };
        let hold = match spec.hold {
            Hold::Exclusive { mode } => Hold::Exclusive { mode },
            Hold::Pooled { mode } => Hold::Pooled { mode },
            Hold::None => Hold::None,
        };
        named.push(Named { path: path.clone(), role: *role, hold }).expect("names checked against the limit");
    }
    let record = Record::Task { task, project, resources };
    let previous = domain.tasks.insert(task, record.clone());
    assert!(previous.is_ok(), "task capacity checked before mutation");
    out.push(Request::Save { record });
    out.push(Request::Named { task, resources: named.into_boxed() });
}

fn unnamed(domain: &mut Domain, task: u64, out: &mut Queue<Request>) {
    if domain.tasks.remove(&task).is_some() {
        out.push(Request::Erase { key: RecordKey::Task(task) });
    }
    if domain.procedures.remove(&task).is_some() {
        out.push(Request::Erase { key: RecordKey::Procedure(task) });
    }
    if domain.results.remove(&task).is_some() {
        out.push(Request::Erase { key: RecordKey::Result(task) });
    }
}

fn adopt(
    domain: &mut Domain,
    env: &Env<Limits>,
    project: u32,
    resource: Path,
    role: ResourceRole,
    out: &mut Queue<Request>,
) {
    let result = if !resource.under(&domain.config.prefix) || !valid_path(&resource, &env.limits) {
        Adoption::Outside
    } else {
        match resource_spec(&domain.config, &resource) {
            None => Adoption::Unknown,
            Some(spec) => {
                if role != ResourceRole::Context && !spec.writable
                    || domain.adoptions.len() >= env.limits.adoptions
                        && !domain.adoptions.contains_key(&(project, resource.clone()))
                {
                    Adoption::Refused
                } else {
                    let previous = domain.adoptions.insert((project, resource.clone()), role);
                    assert!(previous.is_ok(), "adoption capacity checked before mutation");
                    let record = Record::Adoption { project, path: resource.clone(), role };
                    out.push(Request::Save { record });
                    Adoption::Added
                }
            }
        }
    };
    out.push(Request::Adopted { project, resource, result });
}

fn subscribe(
    domain: &mut Domain,
    env: &Env<Limits>,
    task: u64,
    topic: u16,
    wake_at: u16,
    keep_at: u16,
    out: &mut Queue<Request>,
) {
    if !topic_known(&domain.config, topic) || keep_at > wake_at {
        out.push(Request::Refused { task });
        return;
    }
    let key = (topic, task);
    if !domain.subscriptions.contains_key(&key) {
        let mut count = 0_u32;
        for ((number, _), _) in &domain.subscriptions {
            if *number == topic {
                count = count.checked_add(1).expect("subscription count fits its capacity");
            }
        }
        if count >= env.limits.subscribers_per_topic || domain.subscriptions.len() >= env.limits.subscriptions {
            out.push(Request::Refused { task });
            return;
        }
    }
    let previous = domain.subscriptions.insert(key, (wake_at, keep_at));
    assert!(previous.is_ok(), "subscription capacity checked before mutation");
    out.push(Request::Save { record: Record::Subscription { topic, task, wake_at, keep_at } });
}

fn unsubscribe(domain: &mut Domain, task: u64, topic: u16, out: &mut Queue<Request>) {
    if domain.subscriptions.remove(&(topic, task)).is_some() {
        out.push(Request::Erase { key: RecordKey::Subscription { topic, task } });
    }
}

fn restore(domain: &mut Domain, record: Record) {
    match record {
        Record::Procedure(state) => {
            domain.procedures.insert(state.task, state).expect("restored procedure fits");
        }
        Record::Result { task, code } => {
            domain.results.insert(task, code).expect("restored result fits");
        }
        Record::Task { task, project, resources } => {
            domain.tasks.insert(task, Record::Task { task, project, resources }).expect("restored task fits");
        }
        Record::Adoption { project, path, role } => {
            domain.adoptions.insert((project, path), role).expect("restored adoption fits");
        }
        Record::Subscription { topic, task, wake_at, keep_at } => {
            domain.subscriptions.insert((topic, task), (wake_at, keep_at)).expect("restored subscription fits");
        }
        Record::Pool { path, slots } => {
            domain.pools.insert(path, slots).expect("restored pool fits");
        }
        Record::Proposal { number, task, effect } => {
            assert!(domain.outbox.proposals.insert(number, (task, effect)).is_ok(), "restored proposal capacity");
        }
        Record::Outbox(entry) => crate::outbox::restore_entry(domain, entry),
        Record::Made { key, resources, state } => crate::outbox::restore_made(domain, key, resources, state),
    }
}

fn system_event(domain: &mut Domain, env: &Env<Limits>, event: SystemEvent, out: &mut Queue<Request>) {
    match event {
        SystemEvent::Fact { resource, fact, origin } => {
            crate::requirements::fact(domain, env, resource, fact, origin, out);
        }
        SystemEvent::Pool { path, slots, lost } => {
            if u32::try_from(lost.len()).unwrap_or(u32::MAX) > env.limits.lost_per_pool
                || !domain.pools.contains_key(&path)
            {
                return;
            }
            let previous = domain.pools.insert(path.clone(), slots);
            assert!(previous.is_ok(), "known pool can be replaced");
            out.push(Request::Save { record: Record::Pool { path: path.clone(), slots } });
            out.push(Request::Slots { pool: path.clone(), slots });
            if !lost.is_empty() {
                out.push(Request::Drift { pool: path, tasks: lost });
            }
        }
        SystemEvent::News { topic, importance, origin } => match origin {
            Origin::Own => {}
            Origin::Other => {
                if !topic_known(&domain.config, topic) {
                    return;
                }
                let mut subscribers = List::with_capacity(env.limits.subscribers_per_topic);
                for ((subscribed_topic, task), (wake_at, keep_at)) in &domain.subscriptions {
                    if *subscribed_topic == topic {
                        let class = if importance >= *wake_at {
                            Some(Class::Wake)
                        } else if importance >= *keep_at {
                            Some(Class::Keep)
                        } else {
                            None
                        };
                        if let Some(class) = class {
                            subscribers
                                .push(Classed { task: *task, class })
                                .expect("subscriber limit checked at admission");
                        }
                    }
                }
                if !subscribers.is_empty() {
                    out.push(Request::News { topic, subscribers: subscribers.into_boxed() });
                }
            }
        },
        SystemEvent::Applied { entry, attempt, result } => crate::outbox::applied(domain, entry, attempt, result, out),
        SystemEvent::Looked { entry, looked } => crate::outbox::looked(domain, env, entry, looked, out),
    }
}
