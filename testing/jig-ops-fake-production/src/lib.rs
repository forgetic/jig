//! One deterministic production seen by both ops connectors (examples.md, section 9).
//!
//! The fake reports what its own API received. An incident changes both its
//! logs and metrics; a remediation changes both again. Calls, unlike stored
//! history, are counted so connector worlds can check idle load.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};

/// A service's name in a production environment.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceName {
    /// Environment containing the service.
    pub environment: String,
    /// Service within that environment.
    pub service: String,
}

impl ServiceName {
    /// Names one service.
    #[must_use]
    pub fn new(environment: &str, service: &str) -> Self {
        Self { environment: environment.into(), service: service.into() }
    }
}

/// An environment's name within a pool.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EnvironmentName {
    /// Pool that owns the slot.
    pub pool: String,
    /// Name within that pool.
    pub name: String,
}

impl EnvironmentName {
    /// Names one environment.
    #[must_use]
    pub fn new(pool: &str, name: &str) -> Self {
        Self { pool: pool.into(), name: name.into() }
    }
}

/// An operation's deployment-scoped identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    /// Deployment making the effect.
    pub deployment: u64,
    /// Task owning the effect.
    pub task: u64,
    /// Purpose within the task.
    pub purpose: u64,
}

/// The recovery behavior of the infrastructure backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Restarts accept an operation ID.
    Fake,
    /// Restarts have no operation ID and cannot be recovered after uncertainty.
    NoOperationIds,
}

/// A failure or outside hand drawn by a seeded world or explicitly scheduled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The API rejects a call before doing work.
    ApiError,
    /// The API performs an effect, then loses its answer.
    LostAnswer,
    /// A pool has no usable slots for this request.
    QuotaExhausted,
    /// Provisioning takes additional seconds.
    SlowProvisioning { seconds: u64 },
    /// Another hand restarts the named service.
    HandRestart { service: ServiceName },
    /// Another hand changes the replica count.
    HandScale { service: ServiceName, replicas: u32 },
    /// Another hand deletes the named environment.
    HandDelete { environment: EnvironmentName },
}

/// A log line emitted by a service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Log {
    /// Seconds on the world's clock.
    pub at: u64,
    /// The line's text.
    pub text: String,
}

/// A metric sample emitted by a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Seconds on the world's clock.
    pub at: u64,
    /// Load as a percent.
    pub load_percent: u8,
    /// Errors in the sample interval.
    pub errors: u32,
}

/// An alert emitted when an incident starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    /// Number unique within this production.
    pub number: u64,
    /// The affected service.
    pub service: ServiceName,
    /// Time of the alert.
    pub at: u64,
}

/// Facts returned by a service read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceFacts {
    /// Version currently serving.
    pub version: String,
    /// Configured replicas.
    pub replicas: u32,
    /// Replicas currently healthy.
    pub healthy_replicas: u32,
    /// Current load.
    pub load_percent: u8,
    /// Current error count.
    pub errors: u32,
    /// Time of this read.
    pub observed: u64,
    /// Revision changed by either hand.
    pub revision: u64,
}

/// Facts returned by an environment read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentFacts {
    /// Identity of its creator.
    pub key: Key,
    /// Time when provisioning finishes.
    pub ready_at: u64,
    /// Time when its lease expires.
    pub until: u64,
    /// Maximum charged price.
    pub price: u64,
}

/// What an API call did or why it could not do it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResultValue {
    /// An effect was applied or its key was already present.
    Made,
    /// A conditional effect saw a different starting state.
    Conflict,
    /// No target was found.
    Missing,
    /// A pool has no available capacity.
    Full,
    /// The API failed before applying the effect.
    Error,
    /// The effect was made, but its answer was lost.
    Uncertain,
}

/// An effect the production API received, including refused copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservedEffect {
    /// Restarting a service by operation ID where supported.
    Restart { service: ServiceName, operation: u64, applied: bool },
    /// Changing a service's replica count.
    Scale { service: ServiceName, from: u32, to: u32, applied: bool },
    /// Replacing a service's version.
    Rollback { service: ServiceName, from: String, to: String, applied: bool },
    /// Creating an environment with a durable key.
    Create { environment: EnvironmentName, key: Key, price: u64, applied: bool },
    /// Deleting only the environment created under the key.
    TearDown { environment: EnvironmentName, key: Key, applied: bool },
}

#[derive(Clone, Debug)]
struct Service {
    facts: ServiceFacts,
    incident: bool,
    recovery_at: Option<u64>,
    logs: Vec<Log>,
    samples: Vec<Sample>,
}

#[derive(Clone, Copy, Debug)]
struct Pool {
    quota: u32,
}

/// A seeded model of services, streams, pools and environments.
#[derive(Debug)]
pub struct Production {
    seed: u64,
    now: u64,
    backend: Backend,
    services: BTreeMap<ServiceName, Service>,
    pools: BTreeMap<String, Pool>,
    environments: BTreeMap<EnvironmentName, EnvironmentFacts>,
    created: BTreeMap<Key, EnvironmentName>,
    operations: BTreeMap<u64, ServiceName>,
    alerts: Vec<Alert>,
    observed: Vec<ObservedEffect>,
    faults: VecDeque<Fault>,
    calls: u64,
    next_alert: u64,
    history: Vec<u64>,
}

impl Production {
    /// Makes an empty production. Seed zero schedules no automatic fault.
    #[must_use]
    pub fn new(seed: u64, backend: Backend) -> Self {
        Self {
            seed,
            now: 0,
            backend,
            services: BTreeMap::new(),
            pools: BTreeMap::new(),
            environments: BTreeMap::new(),
            created: BTreeMap::new(),
            operations: BTreeMap::new(),
            alerts: Vec::new(),
            observed: Vec::new(),
            faults: VecDeque::new(),
            calls: 0,
            next_alert: 1,
            history: Vec::new(),
        }
    }

    /// Current wall time in seconds.
    #[must_use]
    pub const fn now(&self) -> u64 {
        self.now
    }

    /// Calls made through the production API, excluding setup and history.
    #[must_use]
    pub const fn calls(&self) -> u64 {
        self.calls
    }

    /// Effects received by the production API, in order.
    #[must_use]
    pub fn observed(&self) -> &[ObservedEffect] {
        &self.observed
    }

    /// Alerts emitted so far.
    #[must_use]
    pub fn alerts(&self) -> &[Alert] {
        &self.alerts
    }

    /// Schedules one fault for the next eligible call.
    pub fn queue_fault(&mut self, fault: Fault) {
        self.faults.push_back(fault);
    }

    /// Adds unrelated past data without adding a live resource or API call.
    pub fn preload_history(&mut self, count: u32) {
        for n in 0..count {
            self.history.push(u64::from(n));
        }
    }

    /// Seeds a service with baseline noise and one initial sample.
    pub fn add_service(&mut self, name: ServiceName, version: &str, replicas: u32) {
        let noise = noise(self.seed, &name, 0);
        let facts = ServiceFacts {
            version: version.into(),
            replicas,
            healthy_replicas: replicas,
            load_percent: noise,
            errors: 0,
            observed: self.now,
            revision: 1,
        };
        let service = Service {
            facts,
            incident: false,
            recovery_at: None,
            logs: vec![Log { at: self.now, text: "baseline".into() }],
            samples: vec![Sample { at: self.now, load_percent: noise, errors: 0 }],
        };
        self.services.insert(name, service);
    }

    /// Adds or changes a pool's capacity without evicting existing environments.
    pub fn set_pool_quota(&mut self, name: &str, quota: u32) {
        self.pools.insert(name.into(), Pool { quota });
    }

    /// Number of slots already held in a pool.
    #[must_use]
    pub fn used_slots(&self, pool: &str) -> u32 {
        u32::try_from(self.environments.keys().filter(|name| name.pool == pool).count())
            .expect("world's environment count fits u32")
    }

    /// Starts an incident affecting the service's logs, metrics and health.
    pub fn inject_incident(&mut self, name: &ServiceName) -> bool {
        let Some(service) = self.services.get_mut(name) else {
            return false;
        };
        service.incident = true;
        service.recovery_at = None;
        service.facts.errors = 20;
        service.facts.healthy_replicas = service.facts.replicas.saturating_sub(1);
        service.facts.load_percent = 95;
        service.facts.revision = service.facts.revision.checked_add(1).expect("revision fits");
        service.logs.push(Log { at: self.now, text: "incident: elevated errors".into() });
        service.samples.push(Sample { at: self.now, load_percent: 95, errors: 20 });
        self.alerts.push(Alert { number: self.next_alert, service: name.clone(), at: self.now });
        self.next_alert = self.next_alert.checked_add(1).expect("alert number fits");
        true
    }

    /// Advances the world clock, emitting samples and finishing recovery and provisioning.
    pub fn advance(&mut self, seconds: u64) {
        self.now = self.now.checked_add(seconds).expect("world time fits");
        for (name, service) in &mut self.services {
            if service.recovery_at.is_some_and(|at| at <= self.now) {
                service.incident = false;
                service.recovery_at = None;
                service.facts.healthy_replicas = service.facts.replicas;
                service.facts.errors = 0;
                service.logs.push(Log { at: self.now, text: "recovered".into() });
            }
            service.facts.observed = self.now;
            service.facts.load_percent = if service.incident { 95 } else { noise(self.seed, name, self.now) };
            service.samples.push(Sample {
                at: self.now,
                load_percent: service.facts.load_percent,
                errors: service.facts.errors,
            });
            if !service.incident {
                service.logs.push(Log { at: self.now, text: "baseline".into() });
            }
        }
        if self.seed != 0
            && self.now % 53 == self.seed % 53
            && let Some(name) = self.services.keys().next().cloned()
        {
            self.other_hand(Fault::HandRestart { service: name });
        }
        if self.seed != 0
            && self.now % 71 == self.seed % 71
            && let Some(name) = self.environments.keys().next().cloned()
        {
            self.other_hand(Fault::HandDelete { environment: name });
        }
    }

    /// Reads current service facts through the API.
    pub fn read_service(&mut self, name: &ServiceName) -> Option<ServiceFacts> {
        self.count_call();
        self.services.get(name).map(|service| service.facts.clone())
    }

    /// Reads matching log lines within a window and a byte limit.
    pub fn read_logs(
        &mut self,
        name: &ServiceName,
        from: u64,
        through: u64,
        filter: &str,
        max_bytes: usize,
    ) -> Vec<Log> {
        self.count_call();
        let mut result = Vec::new();
        let mut remaining = max_bytes;
        if let Some(service) = self.services.get(name) {
            for line in &service.logs {
                if line.at >= from && line.at <= through && line.text.contains(filter) && line.text.len() <= remaining {
                    remaining -= line.text.len();
                    result.push(line.clone());
                }
            }
        }
        result
    }

    /// Reads samples within a window and a maximum number of bytes.
    pub fn read_series(&mut self, name: &ServiceName, from: u64, through: u64, max_bytes: usize) -> Vec<Sample> {
        self.count_call();
        let mut result = Vec::new();
        if let Some(service) = self.services.get(name) {
            for sample in &service.samples {
                if sample.at >= from
                    && sample.at <= through
                    && result.len().saturating_add(1).saturating_mul(16) <= max_bytes
                {
                    result.push(*sample);
                }
            }
        }
        result
    }

    /// Reads fired alerts in a window.
    pub fn read_alerts(&mut self, name: &ServiceName, from: u64, through: u64, max_bytes: usize) -> Vec<Alert> {
        self.count_call();
        let mut result = Vec::new();
        for alert in &self.alerts {
            if &alert.service == name
                && alert.at >= from
                && alert.at <= through
                && result.len().saturating_add(1).saturating_mul(24) <= max_bytes
            {
                result.push(alert.clone());
            }
        }
        result
    }

    /// Reads one environment, including its durable creation key.
    pub fn read_environment(&mut self, name: &EnvironmentName) -> Option<EnvironmentFacts> {
        self.count_call();
        self.environments.get(name).cloned()
    }

    /// Reads a pool's capacity and occupied slots.
    pub fn read_pool(&mut self, name: &str) -> Option<(u32, u32)> {
        self.count_call();
        self.pools.get(name).map(|pool| (pool.quota, self.used_slots(name)))
    }

    /// Finds a restart by operation ID, where the backend supports IDs.
    pub fn find_restart(&mut self, operation: u64) -> Option<ServiceName> {
        self.count_call();
        match self.backend {
            Backend::Fake => self.operations.get(&operation).cloned(),
            Backend::NoOperationIds => None,
        }
    }

    /// Restarts a service, once by operation ID on the fake backend.
    pub fn restart(&mut self, name: &ServiceName, operation: u64) -> ResultValue {
        self.count_call();
        let fault = self.take_fault();
        if fault == Some(Fault::ApiError) {
            return ResultValue::Error;
        }
        if self.backend == Backend::Fake
            && let Some(prior) = self.operations.get(&operation)
        {
            self.observed.push(ObservedEffect::Restart { service: name.clone(), operation, applied: false });
            return if prior == name { ResultValue::Made } else { ResultValue::Conflict };
        }
        let Some(service) = self.services.get_mut(name) else {
            return ResultValue::Missing;
        };
        service.facts.revision = service.facts.revision.checked_add(1).expect("revision fits");
        service.recovery_at = Some(self.now.saturating_add(5));
        service.logs.push(Log { at: self.now, text: "restart requested".into() });
        if self.backend == Backend::Fake {
            self.operations.insert(operation, name.clone());
        }
        self.observed.push(ObservedEffect::Restart { service: name.clone(), operation, applied: true });
        Self::after_effect(fault.as_ref())
    }

    /// Sets replicas only from the count the caller decided from.
    pub fn scale(&mut self, name: &ServiceName, from: u32, to: u32) -> ResultValue {
        self.count_call();
        let fault = self.take_fault();
        if fault == Some(Fault::ApiError) {
            return ResultValue::Error;
        }
        let Some(service) = self.services.get_mut(name) else {
            return ResultValue::Missing;
        };
        let applied = service.facts.replicas == from;
        if applied {
            service.facts.replicas = to;
            service.facts.healthy_replicas = if service.incident { to.saturating_sub(1) } else { to };
            service.facts.revision = service.facts.revision.checked_add(1).expect("revision fits");
        }
        self.observed.push(ObservedEffect::Scale { service: name.clone(), from, to, applied });
        if applied { Self::after_effect(fault.as_ref()) } else { ResultValue::Conflict }
    }

    /// Sets a version only from the version the caller decided from.
    pub fn rollback(&mut self, name: &ServiceName, from: &str, to: &str) -> ResultValue {
        self.count_call();
        let fault = self.take_fault();
        if fault == Some(Fault::ApiError) {
            return ResultValue::Error;
        }
        let Some(service) = self.services.get_mut(name) else {
            return ResultValue::Missing;
        };
        let applied = service.facts.version == from;
        if applied {
            service.facts.version = to.into();
            service.facts.revision = service.facts.revision.checked_add(1).expect("revision fits");
        }
        self.observed.push(ObservedEffect::Rollback {
            service: name.clone(),
            from: from.into(),
            to: to.into(),
            applied,
        });
        if applied { Self::after_effect(fault.as_ref()) } else { ResultValue::Conflict }
    }

    /// Creates an environment once by its key, if the pool has room.
    pub fn create_environment(&mut self, name: EnvironmentName, key: Key, until: u64, price: u64) -> ResultValue {
        self.count_call();
        let fault = self.take_fault();
        if fault == Some(Fault::ApiError) {
            return ResultValue::Error;
        }
        if let Some(existing) = self.environments.get(&name) {
            let same = existing.key == key;
            self.observed.push(ObservedEffect::Create { environment: name, key, price, applied: false });
            return if same { ResultValue::Made } else { ResultValue::Conflict };
        }
        if self.created.contains_key(&key) {
            self.observed.push(ObservedEffect::Create { environment: name, key, price, applied: false });
            return ResultValue::Conflict;
        }
        let Some(pool) = self.pools.get(&name.pool).copied() else {
            return ResultValue::Missing;
        };
        if fault == Some(Fault::QuotaExhausted) || self.used_slots(&name.pool) >= pool.quota {
            self.observed.push(ObservedEffect::Create { environment: name, key, price, applied: false });
            return ResultValue::Full;
        }
        let extra = match fault {
            Some(Fault::SlowProvisioning { seconds }) => seconds,
            _ => 0,
        };
        self.environments.insert(
            name.clone(),
            EnvironmentFacts { key, ready_at: self.now.saturating_add(5).saturating_add(extra), until, price },
        );
        self.created.insert(key, name.clone());
        self.observed.push(ObservedEffect::Create { environment: name, key, price, applied: true });
        if fault == Some(Fault::LostAnswer) { ResultValue::Uncertain } else { ResultValue::Made }
    }

    /// Deletes an environment only when it has the expected creation key.
    pub fn tear_down(&mut self, name: &EnvironmentName, key: Key) -> ResultValue {
        self.count_call();
        let fault = self.take_fault();
        if fault == Some(Fault::ApiError) {
            return ResultValue::Error;
        }
        let Some(existing) = self.environments.get(name) else {
            return ResultValue::Missing;
        };
        let applied = existing.key == key;
        if applied {
            self.environments.remove(name);
        }
        self.observed.push(ObservedEffect::TearDown { environment: name.clone(), key, applied });
        if applied { Self::after_effect(fault.as_ref()) } else { ResultValue::Conflict }
    }

    /// Makes a hand restart, change or deletion without an application's key.
    pub fn other_hand(&mut self, fault: Fault) {
        match fault {
            Fault::HandRestart { service: name } => {
                if let Some(service) = self.services.get_mut(&name) {
                    service.facts.revision = service.facts.revision.checked_add(1).expect("revision fits");
                    service.recovery_at = Some(self.now.saturating_add(5));
                    service.logs.push(Log { at: self.now, text: "hand restart".into() });
                }
            }
            Fault::HandScale { service: name, replicas } => {
                if let Some(service) = self.services.get_mut(&name) {
                    service.facts.replicas = replicas;
                    service.facts.healthy_replicas = replicas;
                    service.facts.revision = service.facts.revision.checked_add(1).expect("revision fits");
                }
            }
            Fault::HandDelete { environment } => {
                self.environments.remove(&environment);
            }
            Fault::ApiError | Fault::LostAnswer | Fault::QuotaExhausted | Fault::SlowProvisioning { .. } => {}
        }
    }

    fn count_call(&mut self) {
        self.calls = self.calls.checked_add(1).expect("call count fits");
    }

    fn take_fault(&mut self) -> Option<Fault> {
        self.faults.pop_front().or_else(|| {
            if self.seed == 0 || self.calls % 97 != self.seed % 97 {
                None
            } else {
                match self.seed % 4 {
                    0 => Some(Fault::ApiError),
                    1 => Some(Fault::LostAnswer),
                    2 => Some(Fault::QuotaExhausted),
                    3 => Some(Fault::SlowProvisioning { seconds: 30 }),
                    _ => unreachable!("modulo four has four residues"),
                }
            }
        })
    }

    fn after_effect(fault: Option<&Fault>) -> ResultValue {
        if fault == Some(&Fault::LostAnswer) { ResultValue::Uncertain } else { ResultValue::Made }
    }
}

fn noise(seed: u64, name: &ServiceName, at: u64) -> u8 {
    let mut n = seed ^ at;
    for byte in name.environment.bytes().chain(name.service.bytes()) {
        n = n.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(u64::from(byte));
    }
    u8::try_from(n % 20).expect("noise is below 20")
}

#[cfg(test)]
mod tests;
