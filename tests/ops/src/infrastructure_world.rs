use std::collections::{BTreeMap, BTreeSet, VecDeque};

use jig_ops_domain_infrastructure as infra;
use jig_ops_fake_production as production;
use skein_lib::{Env, Queue, Time, Wall};

/// Small bounds for infrastructure's world.
#[must_use]
pub fn infrastructure_limits() -> infra::Limits {
    infra::Limits {
        services: 4,
        environments: 4,
        pools: 2,
        tasks: 4,
        procedures: 4,
        staged: 4,
        effects: 4,
        made: 4,
        resources_per_task: 4,
        name_bytes: 64,
        max_attempts: 3,
        retry_after_seconds: 30,
    }
}

/// The service shared by the ops worlds, in infrastructure's terms.
#[must_use]
pub fn infra_service() -> infra::Service {
    infra::Service::new(Box::from(*b"production"), Box::from(*b"checkout"))
}

/// One staging environment used by the ops worlds.
#[must_use]
pub fn infra_environment() -> infra::Environment {
    infra::Environment::new(Box::from(*b"staging"), Box::from(*b"payments"))
}

fn service_name(service: &infra::Service) -> production::ServiceName {
    production::ServiceName::new(
        std::str::from_utf8(&service.environment).expect("world environment is UTF-8"),
        std::str::from_utf8(&service.name).expect("world service is UTF-8"),
    )
}

fn environment_name(environment: &infra::Environment) -> production::EnvironmentName {
    production::EnvironmentName::new(
        std::str::from_utf8(&environment.pool).expect("world pool is UTF-8"),
        std::str::from_utf8(&environment.name).expect("world name is UTF-8"),
    )
}

fn production_key(key: infra::Key) -> production::Key {
    production::Key { deployment: key.deployment, task: key.task, purpose: key.purpose }
}

fn infra_key(key: production::Key) -> infra::Key {
    infra::Key { deployment: key.deployment, task: key.task, purpose: key.purpose }
}

/// Infrastructure beneath a scripted root, on the same production as observability.
#[derive(Debug)]
pub struct InfrastructureWorld {
    /// Shared production fake, reporting effects in its own terms.
    pub production: production::Production,
    /// Seconds on the world clock.
    pub now: u64,
    /// Every output the scripted root saw.
    pub seen: Vec<infra::Request>,
    domain: infra::Domain,
    backend: infra::Backend,
    records: BTreeMap<infra::RecordKey, infra::Record>,
    /// Commits containing a record change.
    pub commits: u64,
}

impl InfrastructureWorld {
    /// Builds one connector with a service, pool and shared fake production.
    #[must_use]
    pub fn new(seed: u64, backend: infra::Backend) -> Self {
        let fake_backend = match backend {
            infra::Backend::OperationIds => production::Backend::Fake,
            infra::Backend::NoOperationIds => production::Backend::NoOperationIds,
        };
        let mut production = production::Production::new(seed, fake_backend);
        production.add_service(production::ServiceName::new("production", "checkout"), "v2", 3);
        production.set_pool_quota("staging", 2);
        Self {
            production,
            now: 0,
            seen: Vec::new(),
            domain: infra::Domain::new(backend, &infrastructure_limits()),
            backend,
            records: BTreeMap::new(),
            commits: 0,
        }
    }

    /// One step with its writes committed before any output is released.
    pub fn event(&mut self, input: infra::Event) -> Vec<infra::Request> {
        let wall = self.now.checked_mul(1_000_000_000).expect("world clock fits");
        let env =
            Env { now: Time::from_nanos(self.now), wall: Wall::from_nanos(wall), limits: infrastructure_limits() };
        let mut out = Queue::with_capacity(infra::MAX_OUT);
        infra::step(&mut self.domain, &env, input, &mut out);
        let mut requests = Vec::new();
        while let Some(request) = out.pop() {
            requests.push(request);
        }
        let mut written = false;
        for request in &requests {
            match request {
                infra::Request::Save { record } => {
                    self.records.insert(record.key(), record.clone());
                    written = true;
                }
                infra::Request::Erase { key } => {
                    self.records.remove(key);
                    written = true;
                }
                infra::Request::Named { .. }
                | infra::Request::Slots { .. }
                | infra::Request::Drift { .. }
                | infra::Request::Changed { .. }
                | infra::Request::Described { .. }
                | infra::Request::Refused { .. }
                | infra::Request::Make { .. }
                | infra::Request::Outcome { .. }
                | infra::Request::Step { .. }
                | infra::Request::System(..) => {}
            }
        }
        if written {
            self.commits = self.commits.checked_add(1).expect("commit count fits");
        }
        self.seen.extend(requests.iter().cloned());
        requests
    }

    /// Release the requests after the decision's commit.
    pub fn release(&mut self, requests: &[infra::Request]) -> Vec<infra::Request> {
        let mut pending: VecDeque<infra::Request> = requests.iter().cloned().collect();
        let mut all = requests.to_vec();
        let mut turns = 0_u32;
        while let Some(request) = pending.pop_front() {
            turns = turns.checked_add(1).expect("turn count fits");
            assert!(turns < 100, "scripted root's cascade is finite");
            let generated = match request {
                infra::Request::Make { key } => self.event(infra::Event::Make { key }),
                infra::Request::System(call) => {
                    let reply = self.system(call);
                    self.event(infra::Event::System(reply))
                }
                infra::Request::Save { .. }
                | infra::Request::Erase { .. }
                | infra::Request::Named { .. }
                | infra::Request::Slots { .. }
                | infra::Request::Drift { .. }
                | infra::Request::Changed { .. }
                | infra::Request::Described { .. }
                | infra::Request::Refused { .. }
                | infra::Request::Outcome { .. }
                | infra::Request::Step { .. } => Vec::new(),
            };
            pending.extend(generated.iter().cloned());
            all.extend(generated);
        }
        all
    }

    /// One fake production call, translated from infrastructure's own vocabulary.
    fn system(&mut self, call: infra::SystemRequest) -> infra::SystemEvent {
        match call {
            infra::SystemRequest::Service { service } => {
                let fact = self.production.read_service(&service_name(&service)).expect("world service exists");
                let other_hand = fact.revision > 1 && fact.last_change.is_none();
                infra::SystemEvent::Service {
                    service,
                    fact: infra::ServiceFact {
                        version: fact.version.into_bytes().into_boxed_slice(),
                        replicas: fact.replicas,
                        healthy: fact.errors == 0 && fact.healthy_replicas == fact.replicas,
                        revision: fact.revision,
                        observed: fact.observed,
                    },
                    other_hand,
                }
            }
            infra::SystemRequest::Environment { environment } => {
                let name = environment_name(&environment);
                let fact = self.production.read_environment(&name);
                let other_hand = fact.is_none() && self.production.deleted_by(&name).is_none();
                let fact = fact.map(|row| infra::EnvironmentFact {
                    created_by: infra_key(row.key),
                    ready_at: row.ready_at,
                    until: row.until,
                    price: row.price,
                });
                infra::SystemEvent::Environment { environment, fact, other_hand }
            }
            infra::SystemRequest::Pool { pool } => {
                let name = std::str::from_utf8(&pool.0).expect("world pool is UTF-8");
                let (quota, used) = self.production.read_pool(name).expect("world pool exists");
                infra::SystemEvent::Pool { pool, quota, used }
            }
            infra::SystemRequest::Apply { key, attempt, effect } => {
                let result = self.apply(key, effect);
                let result = match result {
                    production::ResultValue::Made => infra::ApplyResult::Made,
                    production::ResultValue::Conflict => infra::ApplyResult::Conflict,
                    production::ResultValue::Missing => infra::ApplyResult::Missing,
                    production::ResultValue::Full => infra::ApplyResult::Full,
                    production::ResultValue::Error => infra::ApplyResult::Error,
                    production::ResultValue::Uncertain => infra::ApplyResult::Uncertain,
                };
                infra::SystemEvent::Applied { key, attempt, result }
            }
            infra::SystemRequest::Look { key, effect } => {
                infra::SystemEvent::Looked { key, result: self.look(key, effect) }
            }
        }
    }

    fn apply(&mut self, key: infra::Key, effect: infra::Effect) -> production::ResultValue {
        let owner = production_key(key);
        match effect {
            infra::Effect::Restart { service, operation } => {
                self.production.restart_with_key(&service_name(&service), operation, owner)
            }
            infra::Effect::Scale { service, from, to } => {
                self.production.scale_with_key(&service_name(&service), from, to, owner)
            }
            infra::Effect::Rollback { service, from, to } => self.production.rollback_with_key(
                &service_name(&service),
                std::str::from_utf8(&from).expect("world version is UTF-8"),
                std::str::from_utf8(&to).expect("world version is UTF-8"),
                owner,
            ),
            infra::Effect::CreateEnvironment { environment, until, price } => {
                self.production.create_environment(environment_name(&environment), owner, until, price)
            }
            infra::Effect::TearDown { environment, created_by } => {
                self.production.tear_down(&environment_name(&environment), production_key(created_by))
            }
        }
    }

    fn look(&mut self, key: infra::Key, effect: infra::Effect) -> infra::Looked {
        match effect {
            infra::Effect::Restart { service, operation } => match self.production.find_restart(operation) {
                Some(found) if found == service_name(&service) => infra::Looked::Made,
                Some(_) => infra::Looked::Ambiguous,
                None => infra::Looked::CanRetry,
            },
            infra::Effect::Scale { service, from, to } => {
                let fact = self.production.read_service(&service_name(&service));
                match fact {
                    Some(row) if row.replicas == to && row.last_change == Some(production_key(key)) => {
                        infra::Looked::Made
                    }
                    Some(row) if row.replicas == from => infra::Looked::CanRetry,
                    Some(_) | None => infra::Looked::Ambiguous,
                }
            }
            infra::Effect::Rollback { service, from, to } => {
                let fact = self.production.read_service(&service_name(&service));
                match fact {
                    Some(row) if row.version.as_bytes() == &*to && row.last_change == Some(production_key(key)) => {
                        infra::Looked::Made
                    }
                    Some(row) if row.version.as_bytes() == &*from => infra::Looked::CanRetry,
                    Some(_) | None => infra::Looked::Ambiguous,
                }
            }
            infra::Effect::CreateEnvironment { environment, .. } => {
                match self.production.read_environment(&environment_name(&environment)) {
                    Some(row) if row.key == production_key(key) => infra::Looked::Made,
                    None => infra::Looked::CanRetry,
                    Some(_) => infra::Looked::Ambiguous,
                }
            }
            infra::Effect::TearDown { environment, created_by } => {
                let name = environment_name(&environment);
                match self.production.read_environment(&name) {
                    Some(row) if row.key == production_key(created_by) => infra::Looked::CanRetry,
                    Some(_) => infra::Looked::Ambiguous,
                    None => {
                        if self.production.deleted_by(&name) == Some(production_key(created_by)) {
                            infra::Looked::Made
                        } else {
                            infra::Looked::Ambiguous
                        }
                    }
                }
            }
        }
    }

    /// Restore records and refresh named resources before settling the outbox.
    pub fn restart(&mut self) -> Vec<infra::Request> {
        self.domain = infra::Domain::new(self.backend, &infrastructure_limits());
        let records: Vec<_> = self.records.values().cloned().collect();
        for record in &records {
            let restored = self.event(infra::Event::Restore { record: record.clone() });
            assert!(restored.is_empty(), "restoration makes no call");
        }
        let mut live = BTreeSet::new();
        for record in records {
            match record {
                infra::Record::Rely { resources, .. } => {
                    live.extend(resources.iter().cloned());
                }
                infra::Record::Procedure(state) => match state.procedure {
                    infra::Procedure::Remediate { service, .. } => {
                        live.insert(infra::Resource::Service(service));
                    }
                    infra::Procedure::Provision { environment, .. }
                    | infra::Procedure::TearDown { environment, .. } => {
                        live.insert(infra::Resource::Environment(environment));
                    }
                },
                infra::Record::Outbox(entry) => match entry.effect {
                    infra::Effect::Restart { service, .. }
                    | infra::Effect::Scale { service, .. }
                    | infra::Effect::Rollback { service, .. } => {
                        live.insert(infra::Resource::Service(service));
                    }
                    infra::Effect::CreateEnvironment { environment, .. }
                    | infra::Effect::TearDown { environment, .. } => {
                        live.insert(infra::Resource::Environment(environment));
                    }
                },
                infra::Record::Made { environment, .. } => {
                    live.insert(infra::Resource::Environment(environment));
                }
            }
        }
        for resource in live {
            let call = match resource {
                infra::Resource::Service(service) => infra::SystemRequest::Service { service },
                infra::Resource::Environment(environment) => infra::SystemRequest::Environment { environment },
                infra::Resource::Pool(pool) | infra::Resource::Quota(pool) => infra::SystemRequest::Pool { pool },
            };
            let answer = self.system(call);
            self.event(infra::Event::System(answer));
        }
        let requests = self.event(infra::Event::Restart);
        self.release(&requests)
    }
}
