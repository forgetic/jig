//! Entry-point checks for the reference journal and routes.
use crate::{Config, Domain, Event, Limits, Numbers, Output, Released, Store, release, step};
use alloc::boxed::Box;
use jig_core as core;
use jig_host as host;
use jig_ops_domain_infrastructure as infrastructure;
use jig_ops_domain_observability as observability;
use skein_lib::{Duration, Env, JournalLimits, List, Queue, Time, Token, Wall};

fn fixture() -> (Domain, Env<Limits>) {
    let mut smith = smith_agent_world::Settings::calm(17).limits;
    smith.run.budget.spend = 100;
    smith.session.spend = 100;
    smith.run.host_tools = 32;
    smith.run.brief_sections = 32;
    let mut core_limits = jig_core_world::world::limits().core;
    core_limits.fleet.engine_slots = 1;
    let limits = Limits {
        core: core_limits,
        infrastructure: jig_ops_world::infrastructure_limits(),
        observability: jig_ops_world::limits(),
        host: host::Limits {
            slots: 1,
            accounts: 1,
            charter_bytes: 65536,
            transcript_bytes: 1024,
            delivery_evidence_bytes: 0,
            turn_bytes: 1024,
            outcome_bytes: 512,
            detail_bytes: 64,
            held: 2,
            event_bytes: 512,
            run_calls: 2,
            facts: 2,
            told: 2,
            fact_bytes: 128,
            turns: 2,
            turn_queue_bytes: 2048,
        },
        agent: jig_inline_agent::Limits {
            slots: 1,
            smith,
            window: smith_domain::Window {
                turns: 2,
                bytes: smith_domain::max_turn_bytes(&smith).unwrap().checked_mul(2).unwrap(),
            },
            charter: smith_charter::v1::CEILINGS,
            transcript: smith_transcript::v2::CEILINGS,
            cancel_grace: Duration::from_secs(2),
        },
        journal: JournalLimits { commits: 1, writes: 20000, held: 20000, now: 128, release: 128 },
        routes: 256,
    };
    let mut endpoints = List::with_capacity(1);
    endpoints
        .push(smith_protocol_channel::Endpoint { name: Box::from(&b"fake"[..]), number: 1, dialect: 1, account: 1 })
        .unwrap();
    let config = Config {
        core: jig_core_world::world::config(17).core,
        numbers: Numbers { observability: 1, infrastructure: 2 },
        triage_templates: Box::new([]),
        backend: infrastructure::Backend::OperationIds,
        endpoints: Box::new([smith_domain_run::charter::Endpoint(1)]),
        endpoint_names: smith_protocol_channel::Endpoints::new(endpoints),
        charter_endpoints: Box::new([jig_charter::EndpointName {
            number: 1,
            dialect: 1,
            account: 1,
            name: Box::from(&b"fake"[..]),
        }]),
        seed: 17,
    };
    (Domain::new(config, &limits), Env { now: Time::ZERO, wall: Wall::EPOCH, limits })
}

fn take(domain: &mut Domain, env: &Env<Limits>) -> Box<[Output]> {
    let mut out = Queue::with_capacity(256);
    release(domain, env, &mut out);
    let mut values = List::with_capacity(256);
    while let Some(value) = out.pop() {
        values.push(value).unwrap();
    }
    domain.reclaim();
    values.into_boxed()
}

fn name(task: u64) -> Event {
    Event::Infrastructure(infrastructure::Event::Names {
        project: 1,
        task,
        resources: Box::new([infrastructure::Resource::Service(jig_ops_world::infra_service())]),
    })
}

#[test]
fn admission_precedes_a_connectors_change_and_its_outputs_wait_for_the_commit() {
    let (mut domain, env) = fixture();
    step(&mut domain, &env, name(1));
    let first = take(&mut domain, &env);
    match first.as_ref() {
        [Output::Commit { number: 1, .. }] => {}
        other => panic!("one commit: {other:?}"),
    }
    step(&mut domain, &env, name(2));
    let denied = take(&mut domain, &env);
    match denied.as_ref() {
        [Output::Busy] => {}
        other => panic!("busy: {other:?}"),
    }
    step(&mut domain, &env, Event::Store(Store::Committed { number: 1 }));
    take(&mut domain, &env);
    step(&mut domain, &env, name(2));
    let retry = take(&mut domain, &env);
    let Output::Commit { number, mut writes } = retry.into_iter().next().unwrap() else { panic!("retry commits") };
    assert_eq!(number, 2);
    assert!(writes.pop().is_some(), "the refused event is admitted once retried");
}

#[test]
fn a_failed_commit_stops_the_reference_root_before_held_system_requests_leave() {
    let (mut domain, env) = fixture();
    step(
        &mut domain,
        &env,
        Event::Observability(observability::Event::Subscribe {
            task: 1,
            topic: observability::Topic::Alerts(jig_ops_world::service()),
            wake_at: 5,
            keep_at: 1,
        }),
    );
    let values = take(&mut domain, &env);
    let Output::Commit { number, .. } = &values[0] else { panic!("subscription committed") };
    let number = *number;
    assert_eq!(values.len(), 1, "the fact read follows the subscription commit");
    step(&mut domain, &env, Event::Store(Store::Failed { number }));
    match take(&mut domain, &env).as_ref() {
        [Output::Stop] => {}
        other => panic!("stop: {other:?}"),
    }
    assert!(take(&mut domain, &env).is_empty());
}

#[test]
fn a_connector_read_answer_uses_the_door_and_makes_no_commit() {
    let (mut domain, env) = fixture();
    step(&mut domain, &env, name(1));
    take(&mut domain, &env);
    step(&mut domain, &env, Event::Store(Store::Committed { number: 1 }));
    take(&mut domain, &env);
    step(&mut domain, &env, name(2));
    match take(&mut domain, &env).as_ref() {
        [Output::Commit { number: 2, .. }] => {}
        other => panic!("unrelated pending commit: {other:?}"),
    }
    step(
        &mut domain,
        &env,
        Event::Observability(observability::Event::Read {
            token: Token::new(91),
            read: observability::Read::Logs {
                service: jig_ops_world::service(),
                window: observability::Window { from: 0, through: 1 },
                filter: Box::new([]),
                max_bytes: 128,
            },
        }),
    );
    // Both the request and its returned bytes use the door.
    match take(&mut domain, &env).as_ref() {
        [Output::Observability(observability::SystemRequest::Read { .. })] => {}
        other => panic!("read leaves while the unrelated commit is in flight: {other:?}"),
    }
    step(
        &mut domain,
        &env,
        Event::Observability(observability::Event::System(observability::SystemEvent::ReadDone {
            token: Token::new(91),
            bytes: Box::from(&b"log bytes"[..]),
        })),
    );
    match take(&mut domain, &env).as_ref() {
        [Output::Read { owner, bytes }] => {
            assert_eq!(*owner, Token::new(91));
            assert_eq!(bytes.as_ref(), b"log bytes");
        }
        other => panic!("read answer: {other:?}"),
    }
}

#[test]
fn cold_restart_loads_every_child_before_opening_decisions() {
    let (mut original, env) = fixture();
    step(&mut original, &env, name(1));
    let mut header = None;
    for output in take(&mut original, &env) {
        match output {
            Output::Commit { mut writes, .. } => {
                for _ in 0..writes.len() {
                    match writes.pop().unwrap() {
                        crate::Write::Save(
                            record @ crate::Record::Core(core::Record::Core(core::CoreRecord::Deployment(_))),
                        ) => header = Some(record),
                        crate::Write::Save(_) | crate::Write::Erase(_) => {}
                    }
                }
            }
            Output::Load { .. }
            | Output::CoreLoad(_)
            | Output::Core(_)
            | Output::Now(_)
            | Output::Observability(_)
            | Output::Infrastructure(_)
            | Output::Read { .. }
            | Output::Llm { .. }
            | Output::ToChild(_)
            | Output::Busy
            | Output::Stop => panic!("one initial commit"),
        }
    }
    assert!(header.is_some());
    let (mut domain, _) = fixture();
    step(&mut domain, &env, Event::Restart);
    let mut loads = List::with_capacity(16);
    for _ in 0..16_u32 {
        for output in take(&mut domain, &env) {
            match output {
                Output::Load { step: stage, .. } => {
                    loads.push(stage).unwrap();
                    if let Some(record) = header.take() {
                        step(&mut domain, &env, Event::Store(Store::Restore(record)));
                        assert!(take(&mut domain, &env).is_empty(), "header restore is an empty admitted decision");
                    }
                    step(&mut domain, &env, Event::Store(Store::Restored(stage)));
                }
                Output::ToChild(Released::Restart(stage)) => {
                    step(&mut domain, &env, Event::Store(Store::Restored(stage)));
                }
                Output::ToChild(value) => step(&mut domain, &env, Event::Released(value)),
                Output::Commit { number, .. } => step(&mut domain, &env, Event::Store(Store::Committed { number })),
                other @ (Output::CoreLoad(_)
                | Output::Core(_)
                | Output::Now(_)
                | Output::Observability(_)
                | Output::Infrastructure(_)
                | Output::Read { .. }
                | Output::Llm { .. }
                | Output::Busy
                | Output::Stop) => panic!("empty restart has no other work: {other:?}"),
            }
        }
        if domain.ready() {
            break;
        }
    }
    assert_eq!(
        loads.as_slice(),
        [
            core::RestartStep::LoadCore,
            core::RestartStep::RestoreConnector { connector: 1 },
            core::RestartStep::RestoreConnector { connector: 2 }
        ]
    );
    assert!(domain.ready());
}

#[test]
fn the_reference_root_reports_a_finite_sum_and_refuses_overflow() {
    let (_, env) = fixture();
    assert!(crate::worst_case(&env.limits).is_some());
    let mut limits = env.limits;
    limits.core.fleet.attempts = u32::MAX;
    assert!(crate::worst_case(&limits).is_none());
}

fn settle(domain: &mut Domain, env: &Env<Limits>) -> Box<[Output]> {
    let mut seen = List::with_capacity(4096);
    for _ in 0..64_u32 {
        let values = take(domain, env);
        for value in values {
            match value {
                Output::Commit { number, .. } => step(domain, env, Event::Store(Store::Committed { number })),
                Output::ToChild(value) => step(domain, env, Event::Released(value)),
                Output::Core(core::Held::NotesLoad { owner, .. }) => step(
                    domain,
                    env,
                    Event::Core(core::Event::Notes(jig_core_notes::Event::Loaded {
                        owner,
                        rows: jig_core_notes::Rows { records: List::with_capacity(env.limits.core.notes.load_rows) },
                        more: false,
                    })),
                ),
                other @ (Output::Load { .. }
                | Output::CoreLoad(_)
                | Output::Core(_)
                | Output::Now(_)
                | Output::Observability(_)
                | Output::Infrastructure(_)
                | Output::Read { .. }
                | Output::Llm { .. }
                | Output::Busy
                | Output::Stop) => seen.push(other).unwrap(),
            }
        }
    }
    seen.into_boxed()
}

fn started_chat() -> (Domain, Env<Limits>, Token) {
    use jig_core_accounts as accounts;
    use jig_core_fleet as fleet;
    use jig_core_people as people;
    use jig_core_tasks as tasks;
    let (mut domain, env) = fixture();
    for event in [
        core::Event::People(people::Event::Roles { project: 1, holdings: Box::new([]) }),
        core::Event::People(people::Event::Restored),
        core::Event::Tasks(tasks::Event::Restored),
        core::Event::Fleet(fleet::Event::Loaded),
        core::Event::Account(accounts::Event::Add { account: 1, generation: 1, valid: Some(Duration::from_secs(60)) }),
    ] {
        step(&mut domain, &env, Event::Core(event));
        settle(&mut domain, &env);
    }
    step(
        &mut domain,
        &env,
        Event::Core(core::Event::SignIn {
            reply_to: skein_lib::ReplyTo::new(Token::new(101)),
            identity: people::Identity {
                key: people::IdentityKey { provider: 0, subject: 7_u64.to_be_bytes().into() },
                login: b"person".as_ref().into(),
                name: b"Person".as_ref().into(),
            },
        }),
    );
    let signed = settle(&mut domain, &env);
    let mut sign_in = None;
    for output in signed {
        match output {
            Output::Core(core::Held::PeopleReply { sign_in: session, .. }) => sign_in = session,
            Output::Commit { .. }
            | Output::Load { .. }
            | Output::CoreLoad(_)
            | Output::Core(_)
            | Output::Now(_)
            | Output::Observability(_)
            | Output::Infrastructure(_)
            | Output::Read { .. }
            | Output::Llm { .. }
            | Output::ToChild(_)
            | Output::Busy
            | Output::Stop => {}
        }
    }
    let sign_in = sign_in.expect("signed in");
    step(
        &mut domain,
        &env,
        Event::Core(core::Event::People(people::Event::Ask {
            reply_to: skein_lib::ReplyTo::new(Token::new(102)),
            sign_in,
            key: [2; 16],
            ask: people::Ask::StartChat { project: 1, words: b"find the incident".as_ref().into() },
        })),
    );
    let outputs = settle(&mut domain, &env);
    let mut completed = None;
    for output in &outputs {
        match output {
            Output::Llm { client, .. } => completed = Some(*client),
            Output::Commit { .. }
            | Output::Load { .. }
            | Output::CoreLoad(_)
            | Output::Core(_)
            | Output::Now(_)
            | Output::Observability(_)
            | Output::Infrastructure(_)
            | Output::Read { .. }
            | Output::ToChild(_)
            | Output::Busy
            | Output::Stop => {}
        }
    }
    let client = completed.expect("the assigned hub run reached its LLM");
    (domain, env, client)
}

#[test]
fn a_committed_chat_assignment_starts_smith_through_the_engine_hub() {
    started_chat();
}

#[test]
fn a_tool_mutation_and_its_rendered_reply_share_one_commit() {
    let (mut domain, env, client) = started_chat();
    step(
        &mut domain,
        &env,
        Event::Host(host::Event::Called {
            owner: client,
            call: Box::from(crate::translate::name(smith_host_domain::CallName {
                activation: 1,
                completion: 1,
                position: 0,
            })),
            ask: host::Ask::Relay {
                tool: Box::from(&b"subscribe"[..]),
                writes: true,
                input: Box::from(&br#"{"kind":"timer","at":10000000000}"#[..]),
                deadline: Duration::from_secs(10),
            },
        }),
    );
    let outputs = take(&mut domain, &env);
    let mut commit = None;
    let mut call = false;
    let mut task = false;
    for output in outputs {
        match output {
            Output::Commit { number, mut writes } => {
                assert!(commit.replace(number).is_none(), "one decision makes one commit");
                for _ in 0..writes.len() {
                    match writes.pop().unwrap() {
                        crate::Write::Save(crate::Record::Core(core::Record::Core(core::CoreRecord::Call(row)))) => {
                            let Some(settled) = row.settled else { continue };
                            match settled.answer {
                                core::SettledAnswer::Host { error, body } => {
                                    assert!(!error);
                                    assert_eq!(body.as_ref(), br#"{"status":"subscribed","number":1}"#);
                                }
                                core::SettledAnswer::Delivery { .. } => panic!("host reply"),
                            }
                            call = true;
                        }
                        crate::Write::Save(crate::Record::Core(core::Record::Tasks(_))) => task = true,
                        crate::Write::Save(_) | crate::Write::Erase(_) => {}
                    }
                }
            }
            Output::Load { .. }
            | Output::CoreLoad(_)
            | Output::Core(_)
            | Output::Now(_)
            | Output::Observability(_)
            | Output::Infrastructure(_)
            | Output::Read { .. }
            | Output::Llm { .. }
            | Output::ToChild(_)
            | Output::Busy
            | Output::Stop => panic!("the reply waits for its commit"),
        }
    }
    assert!(call && task, "the same transaction owns the mutation and settled reply");
    step(&mut domain, &env, Event::Store(Store::Committed { number: commit.unwrap() }));
    let outputs = take(&mut domain, &env);
    let mut answered = false;
    for output in outputs {
        match output {
            Output::ToChild(Released::Host(host::Event::Relayed { answer, .. })) => {
                assert_eq!(answer.first(), Some(&0));
                assert_eq!(answer.get(1..).unwrap(), br#"{"status":"subscribed","number":1}"#);
                answered = true;
            }
            Output::Commit { .. }
            | Output::Load { .. }
            | Output::CoreLoad(_)
            | Output::Core(_)
            | Output::Now(_)
            | Output::Observability(_)
            | Output::Infrastructure(_)
            | Output::Read { .. }
            | Output::Llm { .. }
            | Output::ToChild(_)
            | Output::Busy
            | Output::Stop => {}
        }
    }
    assert!(answered, "the hub receives the reply after this transaction commits");
}
