//! Independent production answers; no engine state is available here.
use jig_ops_domain_infrastructure as infra;
use jig_ops_domain_observability as obs;
use jig_ops_fake_production as production;

pub struct Systems {
    pub production: production::Production,
    pub now: u64,
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
    production::Key {
        deployment: key.deployment,
        task: key.task,
        purpose: key.purpose,
        origin: match key.origin {
            infra::Purpose::Call { attempt, completion, position } => {
                production::Purpose::Call { attempt, completion, position }
            }
            infra::Purpose::Procedure { purpose } => production::Purpose::Procedure { purpose },
            infra::Purpose::Projection { purpose } => production::Purpose::Projection { purpose },
        },
    }
}

fn infra_key(key: production::Key) -> infra::Key {
    infra::Key {
        deployment: key.deployment,
        task: key.task,
        purpose: key.purpose,
        origin: match key.origin {
            production::Purpose::Call { attempt, completion, position } => {
                infra::Purpose::Call { attempt, completion, position }
            }
            production::Purpose::Procedure { purpose } => infra::Purpose::Procedure { purpose },
            production::Purpose::Projection { purpose } => infra::Purpose::Projection { purpose },
        },
    }
}

fn production_name(name: &obs::Service) -> production::ServiceName {
    production::ServiceName::new(
        std::str::from_utf8(&name.environment).expect("world names are UTF-8"),
        std::str::from_utf8(&name.name).expect("world names are UTF-8"),
    )
}

fn obs_production_key(key: obs::Key) -> production::Key {
    production::Key {
        deployment: key.deployment,
        task: key.task,
        purpose: key.purpose,
        origin: match key.origin {
            obs::Purpose::Call { attempt, completion, position } => {
                production::Purpose::Call { attempt, completion, position }
            }
            obs::Purpose::Procedure { purpose } => production::Purpose::Procedure { purpose },
            obs::Purpose::Projection { purpose } => production::Purpose::Projection { purpose },
        },
    }
}

impl Systems {
    pub fn infrastructure(&mut self, call: infra::SystemRequest) -> infra::SystemEvent {
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

    pub fn observability(&mut self, call: obs::SystemRequest) -> obs::SystemEvent {
        match call {
            obs::SystemRequest::Facts { service } => {
                let name = production_name(&service);
                let current = self.production.read_service(&name).expect("world service exists");
                let from = 0;
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
                let result = self.production.silence(rule, obs_production_key(key), effect.until);
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
                obs::SystemEvent::Found { key, found: self.production.find_silence(obs_production_key(key)) }
            }
        }
    }
}
