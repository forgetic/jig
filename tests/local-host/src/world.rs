//! A small deterministic core and provider composition for local runs.

use std::collections::BTreeMap;

use jig_local_host as host;
use skein_fake_llm_domain::{self as provider, api};
use skein_lib::{Duration, ReplyTo, Time, Token};
use skein_world::domain::Stage;
use smith_domain::{self as smith, llm, run, tools};

/// The fake provider's fixed conversation for one story.
#[derive(Clone, Copy, Debug)]
pub enum Script {
    Answer,
    Wait,
    Call,
}

/// What the scripted core or provider observed at their boundary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Observation {
    Admitted,
    Completion,
    ProviderFailed,
    CompletionCancelled,
    Turn(u32),
    TurnAcknowledged(u32),
    Call,
    CallAnswered,
    Waiting,
    Answer,
    Stopped,
}

/// One engine slot, a scripted committing core, and one fake LLM domain.
pub struct World {
    host: host::Host,
    host_stage: Stage<host::Limits, host::Event, host::Request>,
    provider: provider::Domain,
    provider_stage: Stage<provider::Config, provider::Event, provider::Request>,
    in_flight: BTreeMap<Token, (tools::Grants, Box<[llm::Served]>)>,
    seen: Vec<Observation>,
    turns: Vec<smith::Turn>,
    answer: Option<run::Answer>,
    auto_ack: bool,
}

fn call(name: &[u8], arguments: &[u8]) -> api::Line {
    api::Line::Call { name: name.into(), arguments: arguments.into() }
}

fn scripts(script: Script) -> Box<[api::Script]> {
    let turns: Box<[api::Turn]> = match script {
        Script::Answer => Box::new([api::Turn {
            lines: Box::new([call(b"finish", br#"{"report":"Done."}"#)]),
            finish: api::Finish::ToolCalls,
            tokens: 10,
        }]),
        Script::Wait => Box::new([
            api::Turn { lines: Box::new([call(b"wait", b"{}")]), finish: api::Finish::ToolCalls, tokens: 10 },
            api::Turn {
                lines: Box::new([api::Line::Text { text: b"Ready for a message.".as_slice().into() }]),
                finish: api::Finish::Stop,
                tokens: 10,
            },
            api::Turn {
                lines: Box::new([call(b"finish", br#"{"report":"Welcome back."}"#)]),
                finish: api::Finish::ToolCalls,
                tokens: 10,
            },
        ]),
        Script::Call => Box::new([
            api::Turn {
                lines: Box::new([call(b"lookup", br#"{"key":"answer"}"#)]),
                finish: api::Finish::ToolCalls,
                tokens: 10,
            },
            api::Turn {
                lines: Box::new([call(b"finish", br#"{"report":"Found it."}"#)]),
                finish: api::Finish::ToolCalls,
                tokens: 10,
            },
        ]),
    };
    Box::new([api::Script { cue: b"@local".as_slice().into(), turns }])
}

impl World {
    /// Set up a single local slot with a deterministic fake provider.
    #[must_use]
    pub fn new(script: Script, auto_ack: bool) -> Self {
        Self::seeded(script, auto_ack, 19, 0)
    }

    /// Set the provider seed and chance, per mille, of a failed completion.
    #[must_use]
    pub fn seeded(script: Script, auto_ack: bool, seed: u64, unavailable: u32) -> Self {
        let settings = smith_agent_world::Settings::calm(seed);
        let smith = settings.limits;
        let largest = smith::max_turn_bytes(&smith).expect("bounded Smith turn");
        let limits = host::Limits {
            slots: 1,
            smith,
            window: smith::Window { turns: 2, bytes: largest.checked_mul(2).expect("two bounded turns") },
            cancel_grace: Duration::from_secs(2),
        };
        let provider_config = provider::Config {
            latency_min: Duration::from_millis(1),
            latency_max: Duration::from_millis(1),
            unavailable,
            ..settings.provider
        };
        Self {
            host: host::Host::new(&limits, Box::new([run::charter::Endpoint(0)]), seed),
            host_stage: Stage::new(limits, host::max_out(&limits), host::max_out(&limits) + 2),
            provider: provider::Domain::scripted(&provider_config, seed ^ 0x25, scripts(script)),
            provider_stage: Stage::new(provider_config, provider::MAX_OUT, provider::MAX_OUT + 2),
            in_flight: BTreeMap::new(),
            seen: Vec::new(),
            turns: Vec::new(),
            answer: None,
            auto_ack,
        }
    }

    /// Commit one assignment on the core side and send it to the host.
    pub fn assign(&mut self, budget: host::Budget, with_tool: bool) {
        self.assign_history(budget, with_tool, host::Prices { input: 0, cached: 0, output: 0, unit: 1 }, None);
    }

    /// Assign a charter with explicit model prices for budget stories.
    pub fn assign_priced(&mut self, budget: host::Budget, with_tool: bool, prices: host::Prices) {
        self.assign_history(budget, with_tool, prices, None);
    }

    /// Commit an assignment with concrete retained history from a parked run.
    pub fn assign_history(
        &mut self,
        budget: host::Budget,
        with_tool: bool,
        prices: host::Prices,
        transcript: Option<smith::Transcript>,
    ) {
        let tools: Box<[host::Tool]> = if with_tool {
            Box::new([host::Tool {
                name: b"lookup".as_slice().into(),
                description: b"Look up a value".as_slice().into(),
                schema: br#"{"type":"object"}"#.as_slice().into(),
                effect: host::ToolEffect::Read,
                timeout: Duration::from_secs(5),
            }])
        } else {
            Box::new([])
        };
        let assignment = host::Assignment {
            task: 7,
            attempt: 1,
            charter: host::Charter {
                instructions: b"@local Answer this task.".as_slice().into(),
                tools,
                wait: true,
                agents: false,
                contract: host::Contract {
                    report: Some(host::TextRule { max: 128, fields: Box::new([]) }),
                    failure: None,
                    verdicts: Box::new([]),
                },
                budget,
                model: host::Model {
                    prices,
                    dialect: 0,
                    account: 0,
                    endpoint: 0,
                    name: b"fake-1".as_slice().into(),
                    max_tokens: 100,
                },
                models: Box::new([]),
                waiting: Duration::from_secs(1),
                resumes: true,
            },
            brief: Box::new([
                host::Section { title: b"Task".as_slice().into(), text: b"Answer.".as_slice().into() },
                host::Section { title: b"Context".as_slice().into(), text: b"Use the evidence.".as_slice().into() },
            ]),
            transcript,
            calls: Box::new([]),
            grants: Box::new([host::Grant { account: 0, generation: 1, valid: Duration::from_secs(3_600) }]),
        };
        self.host_stage.push(host::Event::Assign { slot: 0, assignment: Box::new(assignment) });
    }

    /// Send a message under the active task and attempt.
    pub fn message(&mut self, name: u64, words: &[u8]) {
        self.host_stage.push(host::Event::Message {
            task: 7,
            attempt: 1,
            name,
            label: b"person".as_slice().into(),
            words: words.into(),
        });
    }

    /// Ask Smith to cancel this run.
    pub fn cancel(&mut self) {
        self.host_stage.push(host::Event::Cancel { task: 7, attempt: 1 });
    }

    /// Acknowledge the core's committed turns through `number`.
    pub fn acknowledge(&mut self, number: u32) {
        self.seen.push(Observation::TurnAcknowledged(number));
        self.host_stage.push(host::Event::AcknowledgeTurn { task: 7, attempt: 1, turn: number });
    }

    /// The public retained turn, while the core still owes its acknowledgement.
    #[must_use]
    pub fn retained(&self, number: u32) -> bool {
        self.host.retained_turn(7, 1, number).is_some()
    }

    /// Observations made only from requests and the scripted core's commits.
    #[must_use]
    pub fn observations(&self) -> &[Observation] {
        &self.seen
    }

    /// The terminal answer emitted to the core, if one arrived.
    #[must_use]
    pub fn answer(&self) -> Option<&run::Answer> {
        self.answer.as_ref()
    }

    /// Concrete committed turns for the next activation's transcript.
    #[must_use]
    pub fn transcript(&self) -> smith::Transcript {
        let first = self.turns.first().expect("a parked run has a concrete turn");
        smith::Transcript {
            version: first.version,
            endpoint: first.endpoint,
            dialect: first.dialect,
            turns: self.turns.clone().into(),
        }
    }

    /// The world time, advanced only by the deterministic driver.
    #[must_use]
    pub fn now(&self) -> Time {
        self.host_stage.env.now
    }

    /// Advance to a named injected instant without reading a live clock.
    pub fn tick(&mut self, now: Time) {
        self.host_stage.tick(now);
        self.provider_stage.tick(now);
    }

    /// Run one finite shell iteration, returning whether anything moved.
    pub fn cycle(&mut self) -> bool {
        let mut moved = false;
        if let Some(event) = self.host_stage.next_event() {
            host::step(&mut self.host, &self.host_stage.env, event, &mut self.host_stage.out);
            moved = true;
        }
        host::resume(&mut self.host, &self.host_stage.env, &mut self.host_stage.out);
        moved |= self.drain_host();
        if let Some(event) = self.provider_stage.next_event() {
            provider::step(&mut self.provider, &self.provider_stage.env, event, &mut self.provider_stage.out);
            moved = true;
        }
        moved |= self.drain_provider();
        if self.provider.is_due(self.now()) {
            provider::fire(&mut self.provider, &self.provider_stage.env, &mut self.provider_stage.out);
            moved = true;
            moved |= self.drain_provider();
        }
        if host::next_deadline(&self.host).is_some_and(|due| due <= self.now()) {
            host::fire(&mut self.host, &self.host_stage.env, &mut self.host_stage.out);
            moved = true;
            moved |= self.drain_host();
        }
        host::reclaim(&mut self.host);
        self.provider.reclaim();
        moved
    }

    /// Drive until `wanted` appears or the world has no immediate work.
    pub fn until(&mut self, wanted: Observation) {
        let mut quiet = 0;
        for _ in 0..256 {
            if self.seen.contains(&wanted) {
                return;
            }
            let moved = self.cycle();
            if moved {
                quiet = 0;
            } else {
                quiet += 1;
            }
            if !self.host_stage.has_events() && !self.provider_stage.has_events() {
                let next = match (host::next_deadline(&self.host), self.provider.next_deadline()) {
                    (Some(host), Some(provider)) => Some(host.min(provider)),
                    (Some(host), None) => Some(host),
                    (None, Some(provider)) => Some(provider),
                    (None, None) => None,
                };
                match next {
                    Some(next) if next > self.now() && quiet >= 4 => {
                        self.tick(next);
                        quiet = 0;
                    }
                    Some(_) | None => {}
                }
            }
        }
        assert!(self.seen.contains(&wanted), "world never observed {wanted:?}: {:?}", self.seen);
    }

    fn drain_host(&mut self) -> bool {
        let mut moved = false;
        while let Some(request) = self.host_stage.out.pop() {
            moved = true;
            match request {
                host::Request::Admitted { .. } => self.seen.push(Observation::Admitted),
                host::Request::Protocol { request, .. } => self.protocol(request),
                host::Request::Turn { number, turn, .. } => {
                    self.seen.push(Observation::Turn(number));
                    self.turns.push(turn);
                    if self.auto_ack {
                        self.acknowledge(number);
                    }
                }
                host::Request::Call { relay, .. } => {
                    self.seen.push(Observation::Call);
                    self.seen.push(Observation::CallAnswered);
                    self.host_stage.push(host::Event::Answered {
                        task: 7,
                        attempt: 1,
                        relay,
                        reply: run::HostReply::Answered(
                            run::HostAnswer::new(b"42".as_slice().into(), false).expect("bounded host answer"),
                        ),
                    });
                }
                host::Request::Waiting { .. } => self.seen.push(Observation::Waiting),
                host::Request::Answer { answer, .. } => {
                    self.seen.push(Observation::Answer);
                    assert!(self.answer.replace(answer).is_none(), "one terminal answer");
                }
                host::Request::Stopped { .. } => self.seen.push(Observation::Stopped),
                host::Request::Refused { why, .. } => panic!("assignment refused: {why:?}"),
                host::Request::Bounced { why, .. } => panic!("message bounced: {why:?}"),
                host::Request::WithdrawCall { .. } => {}
            }
        }
        moved
    }

    fn protocol(&mut self, request: smith::Request) {
        if let smith::Request::Complete { owner, prompt, .. } = request {
            self.seen.push(Observation::Completion);
            let grants = prompt.tools;
            let served = prompt.served.clone();
            self.in_flight.insert(owner, (grants, served));
            self.provider_stage.push(provider::Event::Call {
                reply_to: ReplyTo::new(owner),
                query: smith_agent_world::translate::query(prompt),
            });
        } else if let smith::Request::Cancel { owner } = request {
            self.seen.push(Observation::CompletionCancelled);
            self.in_flight.remove(&owner);
            self.host_stage.push(host::Event::Completion {
                task: 7,
                attempt: 1,
                terminal: host::Completion::Cancelled { owner },
            });
        }
    }

    fn drain_provider(&mut self) -> bool {
        let mut moved = false;
        while let Some(request) = self.provider_stage.out.pop() {
            moved = true;
            let provider::Request::Reply { to, result } = request;
            let owner = to.into_token();
            let Some((grants, served)) = self.in_flight.remove(&owner) else { continue };
            let terminal = match result {
                Ok(answer) => host::Completion::Completed {
                    owner,
                    completion: smith_agent_world::translate::completion(answer, grants, &served),
                },
                Err(error) => host::Completion::Failed {
                    owner,
                    failure: smith_agent_world::translate::failure(error),
                    evidence: llm::Evidence::Unknown,
                    detail: Box::new([]),
                },
            };
            if matches!(terminal, host::Completion::Failed { .. }) {
                self.seen.push(Observation::ProviderFailed);
            }
            self.host_stage.push(host::Event::Completion { task: 7, attempt: 1, terminal });
        }
        moved
    }
}
