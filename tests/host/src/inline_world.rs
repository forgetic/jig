//! A direct engine link, Jig's host, an inline Smith agent, and a fake LLM.
//! The world supplies the same parent translation on both agent boundaries.

use std::collections::{BTreeMap, VecDeque};

use jig_charter as charter;
use jig_host as hub;
use jig_inline_agent as inline;
use skein_fake_llm_domain::{self as provider, api};
use skein_lib::{Duration, Env, List, Queue, ReplyTo, Time, Token, Wall};
use smith_domain::{self as smith, llm, tools};
use smith_host_domain as agent_host;
use smith_protocol_channel as protocol;

const RUN: Token = Token::new(31);
const ATTEMPT: Token = Token::new(1);

/// The provider story used by a composed inline run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Script {
    Answer,
    Wait,
}

/// Boundary observations, independent of the hub's retained state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Seen {
    Admitted,
    Completion,
    CompletionCancelled,
    Turn(u32),
    TurnAck(u32),
    Waiting,
    Answer,
    Gone,
}

/// One hub and one composed agent; the parent commits through a direct link.
pub struct World {
    hub: hub::Domain,
    hub_env: Env<hub::Limits>,
    hub_out: Queue<hub::Request>,
    hub_events: VecDeque<hub::Event>,
    inline: inline::Agent,
    inline_env: Env<inline::Limits>,
    inline_out: Queue<inline::Request>,
    inline_events: VecDeque<agent_host::Event>,
    provider: provider::Domain,
    provider_env: Env<provider::Config>,
    provider_out: Queue<provider::Request>,
    provider_events: VecDeque<provider::Event>,
    flights: BTreeMap<Token, (tools::Grants, Box<[llm::Served]>)>,
    calls: BTreeMap<Vec<u8>, Token>,
    agent_client: Option<Token>,
    seen: Vec<Seen>,
    turns: BTreeMap<u32, hub::Turn>,
    answer: Option<hub::AnswerV2>,
    auto_ack: bool,
}

fn script(kind: Script) -> Box<[api::Script]> {
    let turns = match kind {
        Script::Answer => Box::new([api::Turn {
            lines: Box::new([api::Line::Call {
                name: Box::from(&b"finish"[..]),
                arguments: Box::from(&br#"{"report":"Done."}"#[..]),
            }]),
            finish: api::Finish::ToolCalls,
            tokens: 10,
        }]) as Box<[api::Turn]>,
        Script::Wait => Box::new([
            api::Turn {
                lines: Box::new([api::Line::Call { name: Box::from(&b"wait"[..]), arguments: Box::from(&b"{}"[..]) }]),
                finish: api::Finish::ToolCalls,
                tokens: 10,
            },
            api::Turn {
                lines: Box::new([api::Line::Text { text: Box::from(&b"Ready for a message."[..]) }]),
                finish: api::Finish::Stop,
                tokens: 10,
            },
            api::Turn {
                lines: Box::new([api::Line::Call {
                    name: Box::from(&b"finish"[..]),
                    arguments: Box::from(&br#"{"report":"Welcome back."}"#[..]),
                }]),
                finish: api::Finish::ToolCalls,
                tokens: 10,
            },
        ]),
    };
    Box::new([api::Script { cue: Box::from(&b"@local"[..]), turns }])
}

fn name(name: agent_host::CallName) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16);
    bytes.extend_from_slice(&name.activation.to_le_bytes());
    bytes.extend_from_slice(&name.completion.to_le_bytes());
    bytes.extend_from_slice(&name.position.to_le_bytes());
    bytes
}

fn failure(source: agent_host::RunFailure) -> hub::RunFailure {
    match source {
        agent_host::RunFailure::Transcript(_) | agent_host::RunFailure::Policy(_) => hub::RunFailure::Policy,
        agent_host::RunFailure::Model(agent_host::ModelFault::Exhausted) => hub::RunFailure::Exhausted,
        agent_host::RunFailure::Model(_) => hub::RunFailure::Model,
        agent_host::RunFailure::Budget(_) => hub::RunFailure::Budget,
        agent_host::RunFailure::Cancelled => hub::RunFailure::Cancelled,
        agent_host::RunFailure::Stale => hub::RunFailure::Stale,
    }
}

fn charter_bytes(budget: charter::Budget, prices: charter::Prices, resumed: bool) -> Box<[u8]> {
    let typed = charter::charter(
        charter::Charter {
            instructions: Box::from(&b"@local Answer this task."[..]),
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
            budget,
            model: charter::Model {
                prices,
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
        Box::new([charter::Section { title: Box::from(&b"Task"[..]), text: Box::from(&b"Answer."[..]) }]),
        resumed,
    )
    .expect("bounded charter");
    charter::encode(
        typed,
        &[charter::EndpointName { number: 0, dialect: 0, account: 0, name: Box::from(&b"fake"[..]) }],
        &smith_charter::v1::CEILINGS,
    )
    .expect("Smith charter codec")
}

impl World {
    /// One engine slot with a direct, never-lost link and no workspace.
    #[must_use]
    pub fn new(script_kind: Script, auto_ack: bool) -> Self {
        let smith = smith_agent_world::Settings::calm(19);
        let max_turn = smith::max_turn_bytes(&smith.limits).expect("bounded Smith turn");
        let inline_limits = inline::Limits {
            slots: 1,
            smith: smith.limits,
            window: smith::Window { turns: 2, bytes: max_turn.checked_mul(2).expect("two bounded turns") },
            charter: smith_charter::v1::CEILINGS,
            transcript: smith_transcript::v2::CEILINGS,
            cancel_grace: Duration::from_secs(2),
        };
        let mut endpoints = List::with_capacity(1);
        endpoints
            .push(protocol::Endpoint { name: Box::from(&b"fake"[..]), number: 0, dialect: 0, account: 0 })
            .expect("one endpoint");
        let inline = inline::Agent::new(
            &inline_limits,
            Box::new([smith::run::charter::Endpoint(0)]),
            protocol::Endpoints::new(endpoints),
            19,
        );
        let mut host_limits = crate::Settings::calm(19).host;
        host_limits.slots = 1;
        host_limits.charter_bytes = 16_384;
        host_limits.transcript_bytes = 2_097_152;
        host_limits.turns = 2;
        host_limits.turn_bytes = 1_048_576;
        host_limits.turn_queue_bytes = 2_097_152;
        host_limits.run_calls = 2;
        assert!(hub::worst_case(&host_limits).is_some(), "hub limits have a bound");
        let provider_config = provider::Config {
            latency_min: Duration::from_millis(1),
            latency_max: Duration::from_millis(1),
            ..smith.provider
        };
        Self {
            hub: hub::Domain::new(&host_limits),
            hub_env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits: host_limits },
            hub_out: Queue::with_capacity(hub::max_out(&host_limits)),
            hub_events: VecDeque::new(),
            inline,
            inline_env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits: inline_limits },
            inline_out: Queue::with_capacity(inline::max_out(&inline_limits)),
            inline_events: VecDeque::new(),
            provider: provider::Domain::scripted(&provider_config, 0x13 ^ 0x25, script(script_kind)),
            provider_env: Env { now: Time::ZERO, wall: Wall::EPOCH, limits: provider_config },
            provider_out: Queue::with_capacity(provider::MAX_OUT),
            provider_events: VecDeque::new(),
            flights: BTreeMap::new(),
            calls: BTreeMap::new(),
            agent_client: None,
            seen: Vec::new(),
            turns: BTreeMap::new(),
            answer: None,
            auto_ack,
        }
    }

    /// Commit and assign one run with a budget and optional saved turn bodies.
    pub fn assign(&mut self, budget: charter::Budget, prices: charter::Prices, transcript: Box<[Box<[u8]>]>) {
        let assignment = hub::Assignment {
            run: RUN,
            attempt: ATTEMPT,
            workspace: None,
            save: false,
            charter: charter_bytes(budget, prices, !transcript.is_empty()),
            grants: Box::new([hub::Grant { account: 0, generation: 1, valid: Duration::from_secs(3600) }]),
        };
        self.hub_events.push_back(hub::Event::AssignTyped {
            reply_to: ReplyTo::new(RUN),
            assignment: hub::AssignmentTyped { assignment, turns: transcript, answered: Box::new([]) },
        });
    }

    /// Return what the root observed at its boundaries.
    #[must_use]
    pub fn seen(&self) -> &[Seen] {
        &self.seen
    }

    /// The hub's answer, once the root has committed it.
    #[must_use]
    pub fn answer(&self) -> Option<&hub::AnswerV2> {
        self.answer.as_ref()
    }

    /// Combined declared child bounds plus the world's bounded routing ledger.
    #[must_use]
    pub fn memory_bound(&self) -> u64 {
        hub::worst_case(&self.hub_env.limits)
            .expect("hub bound")
            .checked_add(inline::worst_case(&self.inline_env.limits).expect("inline agent bound"))
            .and_then(|sum| {
                sum.checked_add(provider::worst_case(&self.provider_env.limits).expect("fake provider bound"))
            })
            .and_then(|sum| sum.checked_add(65_536))
            .expect("composed memory bound")
    }

    /// The exact turn still kept by the hub, pending core acknowledgement.
    #[must_use]
    pub fn retained(&self, number: u32) -> bool {
        self.hub.turn(RUN, ATTEMPT, number).is_some()
    }

    /// Saved committed bytes for a later activation.
    #[must_use]
    pub fn transcript(&self) -> Box<[Box<[u8]>]> {
        self.turns.values().map(|turn| turn.body.clone()).collect()
    }

    /// Send one named person message through the hub.
    pub fn message(&mut self, name: Token, words: &[u8]) {
        self.hub_events.push_back(hub::Event::InboundTyped {
            run: RUN,
            attempt: ATTEMPT,
            name,
            sender: Box::from(&b"person"[..]),
            words: Box::from(words),
        });
    }

    /// Request cancellation at the core boundary.
    pub fn cancel(&mut self) {
        self.hub_events.push_back(hub::Event::Cancel { run: RUN, attempt: ATTEMPT });
    }

    /// Commit one retained turn, then return its exact credit to both children.
    pub fn acknowledge(&mut self, number: u32) {
        assert!(self.turns.contains_key(&number), "the core saw the turn");
        self.seen.push(Seen::TurnAck(number));
        self.hub_events.push_back(hub::Event::AcknowledgeTurn { run: RUN, attempt: ATTEMPT, turn: number });
    }

    /// Move until one observation appears, advancing only injected clocks.
    pub fn until(&mut self, wanted: Seen) {
        for _ in 0..512 {
            if self.seen.contains(&wanted) {
                self.cycle();
                return;
            }
            if !self.cycle() {
                let next = match (inline::next_deadline(&self.inline), self.provider.next_deadline()) {
                    (Some(agent), Some(provider)) => Some(agent.min(provider)),
                    (Some(agent), None) => Some(agent),
                    (None, Some(provider)) => Some(provider),
                    (None, None) => None,
                };
                if let Some(next) = next
                    && next > self.hub_env.now
                {
                    self.hub_env.now = next;
                    self.inline_env.now = next;
                    self.provider_env.now = next;
                }
            }
        }
        panic!("the composed world did not observe {wanted:?}: {:?}", self.seen);
    }

    fn cycle(&mut self) -> bool {
        let mut moved = false;
        for _ in 0..512 {
            let mut this = false;
            if let Some(event) = self.hub_events.pop_front() {
                hub::step(&mut self.hub, &self.hub_env, event, &mut self.hub_out);
                this = true;
            }
            hub::resume(&mut self.hub, &self.hub_env, &mut self.hub_out);
            while let Some(request) = self.hub_out.pop() {
                self.route_hub(request);
                this = true;
            }
            if let Some(event) = self.inline_events.pop_front() {
                inline::step(&mut self.inline, &self.inline_env, event, &mut self.inline_out);
                this = true;
            }
            inline::resume(&mut self.inline, &self.inline_env, &mut self.inline_out);
            while let Some(request) = self.inline_out.pop() {
                self.route_inline(request);
                this = true;
            }
            if let Some(event) = self.provider_events.pop_front() {
                provider::step(&mut self.provider, &self.provider_env, event, &mut self.provider_out);
                this = true;
            }
            if self.provider.is_due(self.hub_env.now) {
                provider::fire(&mut self.provider, &self.provider_env, &mut self.provider_out);
                this = true;
            }
            while let Some(request) = self.provider_out.pop() {
                self.route_provider(request);
                this = true;
            }
            if inline::next_deadline(&self.inline).is_some_and(|time| time <= self.hub_env.now) {
                inline::fire(&mut self.inline, &self.inline_env, &mut self.inline_out);
                this = true;
            }
            if !this {
                break;
            }
            moved = true;
        }
        hub::Domain::reclaim(&mut self.hub);
        inline::reclaim(&mut self.inline);
        self.provider.reclaim();
        moved
    }

    fn route_hub(&mut self, request: hub::Request) {
        match request {
            hub::Request::StartTyped { owner, workspace, charter, activation, turns, answered, grants } => {
                assert_eq!(workspace, None, "engine slots have no workspace");
                assert!(answered.is_empty(), "this world has no recovered host calls");
                self.agent_client = Some(owner);
                let grants = grants
                    .into_vec()
                    .into_iter()
                    .map(|grant| agent_host::Grant {
                        account: grant.account,
                        generation: grant.generation,
                        valid: grant.valid,
                    })
                    .collect();
                self.inline_events.push_back(agent_host::Event::Spawn {
                    client: owner,
                    start: agent_host::Start {
                        logical_run: RUN,
                        activation,
                        workspace: None,
                        charter,
                        transcript: if turns.is_empty() { None } else { Some(turns) },
                        answered: Box::new([]),
                        directories: Box::new([]),
                        grants,
                    },
                });
            }
            hub::Request::Turn { turn, .. } => {
                let number = turn.turn;
                assert!(self.turns.insert(number, turn).is_none(), "each turn crosses the direct link once");
                self.seen.push(Seen::Turn(number));
                if self.auto_ack {
                    self.acknowledge(number);
                }
            }
            hub::Request::AcknowledgeAgentTurn { agent, turn } => {
                self.inline_events.push_back(agent_host::Event::Acknowledge { agent, turn });
            }
            hub::Request::Hosting { .. } => {}
            hub::Request::DeliverTyped { agent, name, sender, words } => {
                self.inline_events.push_back(agent_host::Event::Message { agent, name, label: sender, text: words });
            }
            hub::Request::ReplyTyped { agent, call, reply } => {
                let owner = *self.calls.get(call.as_ref()).expect("host returned a known call");
                let reply = match reply {
                    hub::Reply::Relayed { answer } => agent_host::Reply::Host { error: false, body: answer },
                    hub::Reply::Busy => agent_host::Reply::Busy,
                    hub::Reply::Unavailable => agent_host::Reply::Unavailable,
                    hub::Reply::Withdrawn => agent_host::Reply::Withdrawn,
                    hub::Reply::Delivered(_) => panic!("no inline workspace delivery"),
                };
                self.inline_events.push_back(agent_host::Event::Answer { agent, call: owner, reply });
            }
            hub::Request::RelayTyped { run, attempt, delivery, .. } => {
                self.hub_events.push_back(hub::Event::Relayed {
                    run,
                    attempt,
                    call: delivery,
                    answer: Box::from(&b"42"[..]),
                });
            }
            hub::Request::Stop { agent } => self.inline_events.push_back(agent_host::Event::Stop { agent }),
            hub::Request::Grant { agent, grant } => self.inline_events.push_back(agent_host::Event::Grant {
                agent,
                grant: agent_host::Grant { account: grant.account, generation: grant.generation, valid: grant.valid },
            }),
            hub::Request::AnswerV2 { answer, .. } => {
                assert!(self.answer.replace(answer).is_none(), "one core answer");
                self.seen.push(Seen::Answer);
                self.hub_events.push_back(hub::Event::Unacknowledged { answers: 0 });
            }
            hub::Request::Bounced { .. } => panic!("the scripted message fits"),
            hub::Request::Prepare { .. }
            | hub::Request::Abort { .. }
            | hub::Request::DeliverV2 { .. }
            | hub::Request::Save { .. }
            | hub::Request::Release { .. }
            | hub::Request::CancelRelay { .. } => panic!("unscripted hub request: {request:?}"),
        }
    }

    fn route_inline(&mut self, request: inline::Request) {
        match request {
            inline::Request::Lower { client, request } => self.route_lower(client, request),
            inline::Request::Host(request) => match request {
                agent_host::Request::Started { client, agent } => {
                    self.hub_events.push_back(hub::Event::Started { owner: client, agent });
                }
                agent_host::Request::Admitted { .. } => self.seen.push(Seen::Admitted),
                agent_host::Request::Turn { client, turn } => {
                    self.hub_events.push_back(hub::Event::Turn {
                        owner: client,
                        turn: hub::Turn { turn: turn.number, spent: turn.spent, read: turn.read, body: turn.body },
                    });
                }
                agent_host::Request::Called { client, call, name: operation, deadline, ask, .. } => {
                    let name = name(operation);
                    assert!(self.calls.insert(name.clone(), call).is_none(), "durable operation name is unique");
                    let ask = match ask {
                        agent_host::Ask::Host { tool, effect, body } => hub::Ask::RelayTyped {
                            tool,
                            writes: effect == agent_host::Effect::Write,
                            input: body,
                            deadline: deadline.saturating_since(self.hub_env.now),
                        },
                        agent_host::Ask::Deliver { .. } => panic!("no inline workspace delivery"),
                    };
                    self.hub_events.push_back(hub::Event::CalledTyped {
                        owner: client,
                        call: name.into_boxed_slice(),
                        ask,
                    });
                }
                agent_host::Request::Withdrawn { client, call } => {
                    let name = self
                        .calls
                        .iter()
                        .find(|(_, owner)| **owner == call)
                        .map(|(name, _)| name.clone())
                        .expect("known call");
                    self.hub_events
                        .push_back(hub::Event::WithdrawnTyped { owner: client, call: name.into_boxed_slice() });
                }
                agent_host::Request::Waiting { client, .. } => {
                    self.seen.push(Seen::Waiting);
                    self.hub_events.push_back(hub::Event::Yielded { owner: client });
                }
                agent_host::Request::Answered { client, answer } => {
                    let finish = match answer.result {
                        agent_host::RunResult::Accepted { outcome } => hub::FinishV2::Ended { outcome },
                        agent_host::RunResult::Parked => hub::FinishV2::Parked,
                        agent_host::RunResult::Failed { failure: why } => {
                            hub::FinishV2::Failed { failure: failure(why) }
                        }
                        agent_host::RunResult::Refused { refusal } => panic!("valid start was refused: {refusal:?}"),
                    };
                    self.hub_events.push_back(hub::Event::FinishedV2 {
                        owner: client,
                        turns: answer.turns,
                        spent: answer.spent,
                        finish,
                    });
                }
                agent_host::Request::Faulted { client, fault } => {
                    let fault = match fault {
                        agent_host::Fault::Exited => hub::AgentFailure::Exited,
                        agent_host::Fault::Rules | agent_host::Fault::TooLarge => hub::AgentFailure::Rules,
                        agent_host::Fault::NoProgress => hub::AgentFailure::NoProgress,
                        agent_host::Fault::WallTime => hub::AgentFailure::WallTime,
                    };
                    self.hub_events.push_back(hub::Event::Faulted { owner: client, fault });
                }
                agent_host::Request::Gone { client, .. } => {
                    self.seen.push(Seen::Gone);
                    self.hub_events.push_back(hub::Event::Gone { owner: client, detail: Box::new([]) });
                }
                agent_host::Request::Bounced { client, name, bounce } => {
                    let bounce = match bounce {
                        agent_host::Bounce::TooLarge => hub::Bounce::TooLarge,
                        agent_host::Bounce::Full => hub::Bounce::Full,
                        agent_host::Bounce::Ending | agent_host::Bounce::ReusedName => hub::Bounce::Ending,
                    };
                    self.hub_events.push_back(hub::Event::Bounced { owner: client, name, bounce });
                }
                agent_host::Request::Told { client, body } => {
                    self.hub_events.push_back(hub::Event::Facts { owner: client, fact: body });
                }
                agent_host::Request::Rejected { .. } | agent_host::Request::Exhausted { .. } => {}
                agent_host::Request::Spawn { .. }
                | agent_host::Request::Send { .. }
                | agent_host::Request::Read { .. }
                | agent_host::Request::Signal { .. }
                | agent_host::Request::Wait { .. }
                | agent_host::Request::Reap { .. } => panic!("inline agent has no process request"),
            },
        }
    }

    fn route_lower(&mut self, client: Token, request: smith::Request) {
        match request {
            smith::Request::Complete { owner, prompt, .. } => {
                self.seen.push(Seen::Completion);
                let grants = prompt.tools;
                let served = prompt.served.clone();
                assert!(self.flights.insert(owner, (grants, served)).is_none(), "one completion owner");
                self.provider_events.push_back(provider::Event::Call {
                    reply_to: ReplyTo::new(owner),
                    query: smith_agent_world::translate::query(prompt),
                });
            }
            smith::Request::Cancel { owner } => {
                self.seen.push(Seen::CompletionCancelled);
                self.flights.remove(&owner);
                inline::terminal(
                    &mut self.inline,
                    &self.inline_env,
                    client,
                    inline::Completion::Cancelled { owner },
                    &mut self.inline_out,
                );
            }
            smith::Request::Rejected { .. } | smith::Request::Exhausted { .. } => {}
            other @ (smith::Request::Waiting { .. }
            | smith::Request::Turn { .. }
            | smith::Request::HostCall { .. }
            | smith::Request::WithdrawHost { .. }
            | smith::Request::Admitted { .. }
            | smith::Request::Answer { .. }
            | smith::Request::Checking { .. }
            | smith::Request::ChecksEnded { .. }
            | smith::Request::Deliver { .. }
            | smith::Request::Io { .. }
            | smith::Request::CancelIo { .. }
            | smith::Request::Read { .. }
            | smith::Request::Probe { .. }
            | smith::Request::Check { .. }
            | smith::Request::Abort { .. }) => panic!("unexpected lower inline request: {other:?}"),
        }
    }

    fn route_provider(&mut self, request: provider::Request) {
        let provider::Request::Reply { to, result } = request;
        let owner = to.into_token();
        let Some((grants, served)) = self.flights.remove(&owner) else { return };
        let terminal = match result {
            Ok(answer) => inline::Completion::Completed {
                owner,
                completion: smith_agent_world::translate::completion(answer, grants, &served),
            },
            Err(error) => inline::Completion::Failed {
                owner,
                failure: smith_agent_world::translate::failure(error),
                evidence: llm::Evidence::Unknown,
                detail: Box::new([]),
            },
        };
        inline::terminal(
            &mut self.inline,
            &self.inline_env,
            self.agent_client.expect("live inline agent"),
            terminal,
            &mut self.inline_out,
        );
    }
}
