use alloc::boxed::Box;
use jig_charter as charter;
use skein_lib::{Duration, Env, List, Queue, Time, Token, Wall};
use smith_domain as smith;
use smith_host_domain as host;
use smith_protocol_channel as protocol;

use crate::{Agent, Limits, Request, fire, max_out, step};

fn fixture() -> (Agent, Limits, host::Start) {
    let smith = smith_agent_world::Settings::calm(13).limits;
    let limits = Limits {
        slots: 1,
        smith,
        window: smith::Window {
            turns: 2,
            bytes: smith::max_turn_bytes(&smith).expect("bounded turn").checked_mul(2).expect("two turns fit"),
        },
        charter: smith_charter::v1::CEILINGS,
        transcript: smith_transcript::v2::CEILINGS,
        cancel_grace: Duration::from_secs(2),
    };
    let names = [charter::EndpointName { number: 0, dialect: 0, account: 0, name: Box::from(&b"fake"[..]) }];
    let charter = charter::charter(
        charter::Charter {
            instructions: Box::from(&b"Answer this task"[..]),
            tools: Box::new([]),
            wait: true,
            agents: false,
            workspace: charter::WorkspaceTools { inspect: false, modify: false, shell: false },
            conventions: None,
            contract: charter::Contract {
                report: Some(charter::TextRule { max: 128, fields: Box::new([]) }),
                failure: None,
                verdicts: Box::new([]),
                change: None,
            },
            budget: charter::Budget { turns: 2, spend: 1, time: Duration::from_secs(60) },
            model: charter::Model {
                prices: charter::Prices { input: 0, cached: 0, output: 0, unit: 1 },
                dialect: 0,
                account: 0,
                endpoint: 0,
                name: Box::from(&b"fake-1"[..]),
                max_tokens: 100,
            },
            models: Box::new([]),
            waiting: Duration::from_secs(1),
            resumes: true,
        },
        Box::new([charter::Section { title: Box::from(&b"Task"[..]), text: Box::from(&b"Do it"[..]) }]),
        false,
    )
    .expect("valid charter");
    let encoded = charter::encode(charter, &names, &limits.charter).expect("encodable charter");
    let mut entries = List::with_capacity(1);
    entries
        .push(protocol::Endpoint { name: Box::from(&b"fake"[..]), number: 0, dialect: 0, account: 0 })
        .expect("one endpoint");
    let agent =
        Agent::new(&limits, Box::new([smith::run::charter::Endpoint(0)]), protocol::Endpoints::new(entries), 13);
    let start = host::Start {
        logical_run: Token::new(9),
        activation: 1,
        workspace: None,
        charter: encoded,
        transcript: None,
        answered: Box::new([]),
        directories: Box::new([]),
        grants: Box::new([host::Grant { account: 0, generation: 1, valid: Duration::from_secs(60) }]),
    };
    (agent, limits, start)
}

#[test]
fn inline_spawn_routes_admission_and_provider_request_then_grace_settles() {
    let (mut agent, limits, start) = fixture();
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits };
    let mut out = Queue::with_capacity(max_out(&limits));
    let client = Token::new(7);
    step(&mut agent, &env, host::Event::Spawn { client, start }, &mut out);
    assert_eq!(out.pop(), Some(Request::Host(host::Request::Started { client, agent: client })));
    assert_eq!(out.pop(), Some(Request::Host(host::Request::Admitted { client })));
    match out.pop() {
        Some(Request::Lower { client: seen, request: smith::Request::Complete { .. } }) => assert_eq!(seen, client),
        other => panic!("expected a completion request, got {other:?}"),
    }
    assert!(out.is_empty());
    assert_eq!(agent.hosted(), 1);

    step(&mut agent, &env, host::Event::Stop { agent: client }, &mut out);
    let due = Env { now: Time::from_nanos(Duration::from_secs(3).as_nanos()), wall: Wall::EPOCH, limits };
    fire(&mut agent, &due, &mut out);
    let mut faulted = false;
    for item in &out {
        if let Request::Host(host::Request::Faulted { client: seen, .. }) = item {
            assert_eq!(*seen, client);
            faulted = true;
        }
    }
    assert!(faulted, "grace expiration reports a charter fault");
}

#[test]
fn deferred_smith_work_is_ready_after_reclaim_and_resumed_before_parking() {
    let (mut agent, limits, start) = fixture();
    let env = Env { now: Time::ZERO, wall: Wall::EPOCH, limits };
    let mut out = Queue::with_capacity(max_out(&limits));
    let client = Token::new(7);
    assert!(!agent.is_ready());
    step(&mut agent, &env, host::Event::Spawn { client, start }, &mut out);
    let mut owner = None;
    while let Some(item) = out.pop() {
        if let Request::Lower { request: smith::Request::Complete { owner: name, .. }, .. } = item {
            owner = Some(name);
        }
    }
    assert!(!agent.is_ready(), "a provider completion is external work");
    crate::terminal(
        &mut agent,
        &env,
        client,
        crate::Completion::Completed {
            owner: owner.expect("spawn requested a completion"),
            completion: smith::llm::Completion {
                content: Box::new([smith::llm::Said::ToolCall {
                    id: Box::from(&b"wait-1"[..]),
                    name: Box::from(&b"wait"[..]),
                    input: Box::from(&b"{}"[..]),
                    call: smith::llm::Decoded::Served { ask: smith::run::Ask::Wait },
                    replay: None,
                }]),
                stop: smith::llm::Stop::ToolUse,
                usage: smith::llm::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                },
            },
        },
        &mut out,
    );
    assert!(!agent.is_ready(), "handoffs wait for the iteration boundary");
    crate::reclaim(&mut agent);
    assert!(agent.is_ready(), "deferred Smith work keeps the adapter running");
    crate::resume(&mut agent, &env, &mut out);
    let mut completed = false;
    for item in &out {
        if let Request::Lower { request: smith::Request::Complete { .. }, .. } = item {
            completed = true;
        }
    }
    assert!(completed, "resuming reaches the provider boundary");
    assert!(!agent.is_ready(), "waiting for a provider completion permits parking");
}
