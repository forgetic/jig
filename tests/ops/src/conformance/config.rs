//! Finite story configuration, reconstructed unchanged on every cold start.
use jig_host as host;
use jig_ops_domain::{Config, Limits, Numbers};
use jig_ops_domain_infrastructure as infrastructure;
use skein_lib::{Duration, JournalLimits, List};

#[must_use]
#[expect(clippy::too_many_lines, reason = "the fixture lists every child bound")]
pub fn root(seed: u64) -> (Config, Limits) {
    let mut smith = smith_agent_world::Settings::calm(seed).limits;
    smith.run.budget.spend = 100;
    smith.run.budget.turns = 32;
    smith.run.budget.time = Duration::from_secs(7200);
    smith.session.spend = 100;
    smith.session.budget.turns = 32;
    smith.session.budget.time = Duration::from_secs(7200);
    smith.run.host_tools = 32;
    smith.run.brief_sections = 32;
    let mut core_limits = jig_core_world::world::limits().core;
    core_limits.fleet.engine_slots = 1;
    core_limits.tasks.tasks = 8;
    core_limits.tasks.project_tasks = 8;
    core_limits.tasks.tree_tasks = 8;
    core_limits.tasks.depth = 4;
    core_limits.tasks.delegates = 3;
    core_limits.tasks.batch = 2;
    core_limits.tasks.parameters = 4;
    core_limits.tasks.spec_bytes = 512;
    core_limits.tasks.authority_grants = 8;
    core_limits.tasks.authority_segments = 3;
    core_limits.tasks.authority_bytes = 64;
    core_limits.tasks.executor_kinds = 3;
    core_limits.tasks.funders = 16;
    core_limits.tasks.inbox_messages = 8;
    core_limits.tasks.inbox_bytes = 2048;
    core_limits.tasks.message_bytes = 512;
    core_limits.tasks.proposal_stall = Duration::from_secs(60);
    core_limits.people.words = 512;
    core_limits.people.requests = 16;
    core_limits.people.inbox_entries = 16;
    core_limits.people.sign_in_lifetime = Duration::from_secs(7200);
    core_limits.authority.grants = 8;
    core_limits.notes.scopes = 11;
    core_limits.authority.segments = 4;
    core_limits.authority.executors = 3;
    core_limits.authority.batch = 2;
    core_limits.authority.requirements = 2;
    core_limits.fleet.attempts = 8;
    core_limits.fleet.turns = 32;
    core_limits.fleet.calls = 8;
    core_limits.call_records = 16;
    core_limits.resume_bytes = 32768;
    core_limits.brief.sections = 11;
    core_limits.brief.brief_bytes = 8192;
    core_limits.brief.read_bytes = 8192;
    core_limits.call_answer_bytes = 1024;
    let mut limits = Limits {
        core: core_limits,
        infrastructure: crate::infrastructure_limits(),
        observability: crate::limits(),
        host: host::Limits {
            slots: 1,
            accounts: 1,
            charter_bytes: 65536,
            transcript_bytes: 32768,
            delivery_evidence_bytes: 0,
            turn_bytes: 8192,
            outcome_bytes: 512,
            detail_bytes: 64,
            held: 2,
            event_bytes: 8192,
            run_calls: 4,
            facts: 2,
            told: 2,
            fact_bytes: 128,
            turns: 32,
            turn_queue_bytes: 32768,
        },
        agent: jig_inline_agent::Limits {
            slots: 1,
            smith,
            window: smith_domain::Window {
                turns: 32,
                bytes: smith_domain::max_turn_bytes(&smith)
                    .expect("bounded Smith turn")
                    .checked_mul(32)
                    .expect("bounded transcript window"),
            },
            charter: smith_charter::v1::CEILINGS,
            transcript: smith_transcript::v2::CEILINGS,
            cancel_grace: Duration::from_secs(2),
        },
        journal: JournalLimits { commits: 4, writes: 20000, held: 20000, now: 128, release: 128 },
        routes: 256,
    };
    let mut endpoints = List::with_capacity(1);
    endpoints
        .push(smith_protocol_channel::Endpoint { name: Box::from(&b"fake"[..]), number: 1, dialect: 1, account: 1 })
        .expect("one endpoint fits");
    let mut config = Config {
        core: jig_core_world::world::config(seed).core,
        numbers: Numbers { observability: 1, infrastructure: 2 },
        triage_templates: Box::new([jig_ops_domain_observability::TriageTemplate {
            number: 1,
            delegate: super::scripts::triage(),
        }]),
        backend: infrastructure::Backend::OperationIds,
        endpoints: Box::new([smith_domain_run::charter::Endpoint(1)]),
        endpoint_names: smith_protocol_channel::Endpoints::new(endpoints),
        charter_endpoints: Box::new([jig_charter::EndpointName {
            number: 1,
            dialect: 1,
            account: 1,
            name: Box::from(&b"fake"[..]),
        }]),
        seed,
    };
    limits.observability.answer_bytes = 2048;
    limits.infrastructure.tasks = 8;
    limits.infrastructure.procedures = 8;
    config.core.authority = policy_domain(&limits.core.authority);
    config.core.settings.chat_authority = authority(100, false);
    config.core.settings.period = 1;
    config.core.settings.run.resume = true;
    config.core.settings.resume_bytes = 32768;
    config.core.settings.run.waiting = Duration::ZERO;
    config.core.settings.run.turns = 32;
    config.core.settings.run.time = Duration::from_secs(7200);
    (config, limits)
}

use jig_core_authority as authority;
use jig_core_tasks as tasks;
use skein_lib::Queue;

fn pattern(environment: Option<&[u8]>) -> authority::Pattern {
    let mut segments = vec![b"env".as_slice().into()];
    if let Some(environment) = environment {
        segments.push(environment.into());
        segments.push(b"service".as_slice().into());
    }
    authority::Pattern { segments: segments.into_boxed_slice(), last: authority::Last::Open(Box::new([])) }
}

/// Staging grants are the agent policy; the person additionally covers production.
#[must_use]
pub fn authority(spend: u64, person: bool) -> authority::Authority {
    let mut grants = Vec::new();
    for kind in [1, 2] {
        grants.push(authority::Grant {
            connector: 2,
            kind,
            pattern: pattern(if person { None } else { Some(b"staging") }),
        });
    }
    for kind in [2, 3, 4] {
        grants.push(authority::Grant {
            connector: 1,
            kind,
            pattern: authority::Pattern { segments: Box::new([]), last: authority::Last::Open(Box::new([])) },
        });
    }
    authority::Authority {
        tools: authority::Tools(1023),
        grants: grants.into_boxed_slice(),
        delegation: authority::Delegation {
            kinds: Box::new([
                authority::Executor::Charter(1),
                authority::Executor::Procedure(1),
                authority::Executor::Procedure(2),
            ]),
            tasks: if person { 8 } else { 6 },
            depth: if person { 5 } else { 4 },
        },
        budget: authority::Budget { spend, deadline: None },
        notes: authority::Scopes(0),
        note_resources: Box::new([]),
    }
}

fn policy_domain(limits: &authority::Limits) -> authority::Domain {
    let requirements: Box<[_]> = [
        authority::Requirement {
            connector: 2,
            kind: 1,
            pattern: pattern(None),
            judge: authority::Judge { connector: 1, requirement: 1, parameters: 0 },
            guard: authority::Guard::Observed { freshness: Duration::from_secs(30) },
            must_be_guarded: false,
        },
        authority::Requirement {
            connector: 2,
            kind: 2,
            pattern: pattern(None),
            judge: authority::Judge { connector: 1, requirement: 2, parameters: 0 },
            guard: authority::Guard::Observed { freshness: Duration::from_secs(30) },
            must_be_guarded: false,
        },
    ]
    .into();
    let ceiling = authority(1000, true);
    let mut domain = authority::Domain::new(
        authority::Rules {
            ceiling: ceiling.clone(),
            period_spend: 1000,
            minimum_run_spend: 1,
            maximum_run_spend: 19,
            implies: authority::Implies::new(Box::new([]), 0).expect("empty implications"),
            requirements,
        },
        *limits,
    )
    .expect("bounded scenario policy");
    let mut out = Queue::with_capacity(authority::POLICY_MAX_OUT);
    authority::step(
        &mut domain,
        authority::Event::Policy {
            project: 1,
            policy: authority::Policy {
                projections: Box::new([]),
                escalation_role: Some(0),
                ceiling,
                period_spend: 1000,
                roles: Box::new([authority::Role {
                    number: 0,
                    authority: authority(500, true),
                    period_spend: 500,
                    requests: authority::Requests::ALL,
                    decides: authority::Proposals::ALL,
                }]),
                requirements: Box::new([]),
            },
        },
        &mut out,
    );
    assert_eq!(out.pop(), Some(authority::PolicyFact::Added { project: 1 }));
    domain
}

#[must_use]
pub fn task_authority(spend: u64) -> tasks::Authority {
    let value = authority(spend, false);
    tasks::Authority {
        tools: tasks::Tools(value.tools.0),
        grants: value
            .grants
            .into_iter()
            .map(|grant| tasks::Grant {
                connector: grant.connector,
                kind: grant.kind,
                pattern: tasks::Pattern {
                    segments: grant.pattern.segments,
                    last: match grant.pattern.last {
                        authority::Last::Open(x) => tasks::Last::Open(x),
                        authority::Last::Exact(x) => tasks::Last::Exact(x),
                    },
                },
            })
            .collect(),
        delegation: tasks::Delegation {
            kinds: Box::new([
                tasks::AuthorityExecutor::Charter(1),
                tasks::AuthorityExecutor::Procedure(1),
                tasks::AuthorityExecutor::Procedure(2),
            ]),
            tasks: 6,
            depth: 4,
        },
        budget: tasks::Budget { spend, deadline: None },
        notes: tasks::Scopes(0),
        note_resources: Box::new([]),
    }
}
