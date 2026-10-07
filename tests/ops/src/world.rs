use std::collections::{BTreeMap, BTreeSet, VecDeque};

use jig_ops_domain_observability as obs;
use jig_ops_fake_production as production;
use skein_lib::{Env, Queue, Time, Token, Wall};

/// Small limits for the connector stories.
#[must_use]
pub fn limits() -> obs::Limits {
    obs::Limits {
        services: 4,
        subscriptions: 8,
        watches: 2,
        staged: 2,
        effects: 2,
        judges: 4,
        reads: 4,
        subscribers_per_alert: 8,
        samples_per_service: 16,
        services_per_watch: 3,
        alerts_per_batch: 8,
        name_bytes: 64,
        answer_bytes: 128,
        max_attempts: 3,
        retry_after_seconds: 30,
    }
}

/// The service shared by the two connectors' stories.
#[must_use]
pub fn service() -> obs::Service {
    obs::Service::new(Box::from(*b"production"), Box::from(*b"checkout"))
}

fn production_name(name: &obs::Service) -> production::ServiceName {
    production::ServiceName::new(
        std::str::from_utf8(&name.environment).expect("world names are UTF-8"),
        std::str::from_utf8(&name.name).expect("world names are UTF-8"),
    )
}

fn production_key(key: obs::Key) -> production::Key {
    production::Key { deployment: key.deployment, task: key.task, purpose: key.purpose }
}

/// One connector under a scripted root and above the shared production.
#[derive(Debug)]
pub struct World {
    /// The production the connector reads and drives.
    pub production: production::Production,
    /// Seconds on the world's clock.
    pub now: u64,
    /// All outputs seen by the scripted root.
    pub seen: Vec<obs::Request>,
    domain: obs::Domain,
    records: BTreeMap<obs::RecordKey, obs::Record>,
    /// Number of root commits containing a record mutation.
    pub commits: u64,
}

impl World {
    /// Builds the system, connector and scripted root.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        let mut production = production::Production::new(seed, production::Backend::Fake);
        production.add_service(production::ServiceName::new("production", "checkout"), "v1", 3);
        production.add_alert_rule("checkout-errors", production::ServiceName::new("production", "checkout"));
        Self {
            production,
            now: 0,
            seen: Vec::new(),
            domain: obs::Domain::new(&limits()),
            records: BTreeMap::new(),
            commits: 0,
        }
    }

    /// One connector step; the root commits its writes before returning requests.
    pub fn event(&mut self, input: obs::Event) -> Vec<obs::Request> {
        let wall = self.now.checked_mul(1_000_000_000).expect("world time fits");
        let env = Env { now: Time::from_nanos(self.now), wall: Wall::from_nanos(wall), limits: limits() };
        let mut out = Queue::with_capacity(obs::MAX_OUT);
        obs::step(&mut self.domain, &env, input, &mut out);
        let mut requests = Vec::new();
        while let Some(request) = out.pop() {
            requests.push(request);
        }
        let mut written = false;
        for request in &requests {
            match request {
                obs::Request::Save { record } => {
                    self.records.insert(record.key(), record.clone());
                    written = true;
                }
                obs::Request::Erase { key } => {
                    self.records.remove(key);
                    written = true;
                }
                obs::Request::Verdict { .. }
                | obs::Request::Answer { .. }
                | obs::Request::News { .. }
                | obs::Request::Health { .. }
                | obs::Request::Triage { .. }
                | obs::Request::Described { .. }
                | obs::Request::Refused { .. }
                | obs::Request::Make { .. }
                | obs::Request::Outcome { .. }
                | obs::Request::System(..)
                | obs::Request::Changed { .. } => {}
            }
        }
        if written {
            self.commits = self.commits.checked_add(1).expect("commit number fits");
        }
        self.seen.extend(requests.iter().cloned());
        requests
    }

    /// Release requests only after the root committed their records.
    pub fn release(&mut self, requests: &[obs::Request]) -> Vec<obs::Request> {
        let mut pending: VecDeque<obs::Request> = requests.iter().cloned().collect();
        let mut all = requests.to_vec();
        let mut turns = 0_u32;
        while let Some(request) = pending.pop_front() {
            turns = turns.checked_add(1).expect("turn count fits");
            assert!(turns < 100, "connector cascade is finite");
            let generated = match request {
                obs::Request::Make { key } => self.event(obs::Event::Make { key }),
                obs::Request::System(call) => {
                    let result = self.system(call);
                    self.event(obs::Event::System(result))
                }
                obs::Request::Save { .. }
                | obs::Request::Erase { .. }
                | obs::Request::Verdict { .. }
                | obs::Request::Answer { .. }
                | obs::Request::News { .. }
                | obs::Request::Health { .. }
                | obs::Request::Triage { .. }
                | obs::Request::Described { .. }
                | obs::Request::Refused { .. }
                | obs::Request::Outcome { .. }
                | obs::Request::Changed { .. } => Vec::new(),
            };
            pending.extend(generated.iter().cloned());
            all.extend(generated);
        }
        all
    }

    /// Apply one system request and return what the fake observed.
    fn system(&mut self, call: obs::SystemRequest) -> obs::SystemEvent {
        match call {
            obs::SystemRequest::Facts { service } => {
                let name = production_name(&service);
                let current = self.production.read_service(&name).expect("world service exists");
                let from = self.now.saturating_sub(60);
                let samples = self.production.read_series(&name, from, self.now, 16 * 16);
                let load = samples
                    .iter()
                    .map(|sample| obs::LoadPoint { at: sample.at, percent: sample.load_percent })
                    .collect();
                obs::SystemEvent::Fact {
                    service,
                    fact: obs::Fact {
                        observed: current.observed,
                        healthy_replicas: current.healthy_replicas,
                        errors: current.errors,
                        error_rate_percent: current.error_rate_percent,
                        load_percent: current.load_percent,
                        load,
                    },
                }
            }
            obs::SystemRequest::Read { token, read } => {
                let bytes = match read {
                    obs::Read::Logs { service, window, filter, max_bytes } => {
                        let name = production_name(&service);
                        let filter = std::str::from_utf8(&filter).expect("world filter is UTF-8");
                        let rows =
                            self.production.read_logs(&name, window.from, window.through, filter, max_bytes as usize);
                        rows.into_iter().flat_map(|line| line.text.into_bytes()).take(max_bytes as usize).collect()
                    }
                    obs::Read::Series { service, window, max_bytes } => {
                        let name = production_name(&service);
                        let rows = self.production.read_series(&name, window.from, window.through, max_bytes as usize);
                        format!("{rows:?}").into_bytes().into_iter().take(max_bytes as usize).collect()
                    }
                    obs::Read::Fired { service, window, max_bytes } => {
                        let name = production_name(&service);
                        let rows = self.production.read_alerts(&name, window.from, window.through, max_bytes as usize);
                        format!("{rows:?}").into_bytes().into_iter().take(max_bytes as usize).collect()
                    }
                };
                obs::SystemEvent::ReadDone { token, bytes }
            }
            obs::SystemRequest::Silence { key, attempt, effect } => {
                let rule = std::str::from_utf8(&effect.rule).expect("world rule is UTF-8");
                let result = self.production.silence(rule, production_key(key), effect.until);
                let outcome = match result {
                    production::ResultValue::Made => obs::Outcome::Made,
                    production::ResultValue::Uncertain => obs::Outcome::Uncertain,
                    production::ResultValue::Conflict
                    | production::ResultValue::Missing
                    | production::ResultValue::Full
                    | production::ResultValue::Error => obs::Outcome::Failed,
                };
                obs::SystemEvent::Applied { key, attempt, outcome }
            }
            obs::SystemRequest::FindSilence { key, rule: _ } => {
                obs::SystemEvent::Found { key, found: self.production.find_silence(production_key(key)) }
            }
        }
    }

    /// Rebuild the connector from committed records, then let it settle outbox entries.
    pub fn restart(&mut self) -> Vec<obs::Request> {
        self.domain = obs::Domain::new(&limits());
        let records: Vec<_> = self.records.values().cloned().collect();
        let mut live = BTreeSet::new();
        for record in &records {
            match record {
                obs::Record::Subscription { topic, .. } => match topic {
                    obs::Topic::Alerts(service) | obs::Topic::Health(service) => {
                        live.insert(service.clone());
                    }
                },
                obs::Record::Watch(watch) => {
                    live.extend(watch.services.iter().cloned());
                }
                obs::Record::Outbox(..) => {}
            }
        }
        for record in records {
            let restored = self.event(obs::Event::Restore { record });
            assert!(restored.is_empty(), "restore makes no external request");
        }
        for service in live {
            self.release(&[obs::Request::System(obs::SystemRequest::Facts { service })]);
        }
        let requests = self.event(obs::Event::Restart);
        self.release(&requests)
    }

    /// Recheck uncertain effects when their saved absolute deadlines pass.
    pub fn fire(&mut self) -> Vec<obs::Request> {
        let wall = self.now.checked_mul(1_000_000_000).expect("world time fits");
        let env = Env { now: Time::from_nanos(self.now), wall: Wall::from_nanos(wall), limits: limits() };
        let mut out = Queue::with_capacity(obs::MAX_OUT);
        obs::fire(&mut self.domain, &env, &mut out);
        let mut requests = Vec::new();
        while let Some(request) = out.pop() {
            requests.push(request);
        }
        self.release(&requests)
    }

    /// A stable token for one scripted root request.
    #[must_use]
    pub fn token(number: u64) -> Token {
        Token::new(number)
    }
}
