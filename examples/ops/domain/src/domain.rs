//! Copy this entry-point shape. Every child hand-off belongs to the admitted
//! decision; no child receives an event before its whole route fits (domain/root.md, 3–4).
use crate::boundary::Payload;
use crate::{Event, Limits, Output, Record, Released, Store, Write};
use alloc::boxed::Box;
use jig_core as core;
use jig_core_authority as authority;
use jig_core_fleet as fleet;
use jig_host as host;
use jig_inline_agent as agent;
use jig_ops_domain_infrastructure as infrastructure;
use jig_ops_domain_observability as observability;
use skein_lib::{Decision, Env, Journal, Map, Queue, Token};

/// Prepared output parts retained under the core's assignment continuation.
#[derive(Debug)]
pub(crate) struct Assignment {
    pub(crate) attempt: u64,
    pub(crate) value: host::Assignment,
}

/// A connector read keeps only its caller continuation and opaque envelope.
#[derive(Debug)]
pub(crate) struct ReadCall {
    pub(crate) to: Token,
    pub(crate) name: Box<[u8]>,
    pub(crate) tool: Box<[u8]>,
}

/// The hub owns this relay's fenced destination and delivery continuation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HostCall {
    pub(crate) run: Token,
    pub(crate) attempt: Token,
    pub(crate) delivery: Token,
}

/// Child labels are routing configuration, never policy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Numbers {
    /// Number assigned to observability in the core's configuration.
    pub observability: u16,
    /// Number assigned to infrastructure in the core's configuration.
    pub infrastructure: u16,
}

/// Configuration is handed to the child that owns each value.
#[derive(Debug)]
pub struct Config {
    /// Core rules and its children.
    pub core: core::Config,
    /// Connector labels used by the same core configuration.
    pub numbers: Numbers,
    /// Observability owns the standing watches' delegate templates.
    pub triage_templates: Box<[observability::TriageTemplate]>,
    /// The infrastructure backend's available recovery promise.
    pub backend: infrastructure::Backend,
    /// Endpoint identities for the inline agent.
    pub endpoints: Box<[smith_domain_run::charter::Endpoint]>,
    /// The protocol layer's names for the same endpoint identities.
    pub endpoint_names: smith_protocol_channel::Endpoints,
    /// Charter codec endpoint names for the same identities.
    pub charter_endpoints: Box<[jig_charter::EndpointName]>,
    /// Injected agent seed.
    pub seed: u64,
}

pub(crate) type CallEnvelope = (Box<[u8]>, Box<[u8]>);

/// Children, one journal, and opaque child-requested continuation bindings.
/// No root-owned record or policy is stored (domain/root.md, 2).
#[derive(Debug)]
pub struct Domain {
    pub(crate) core: core::Core,
    pub(crate) observability: observability::Domain,
    pub(crate) infrastructure: infrastructure::Domain,
    pub(crate) host: host::Domain,
    pub(crate) agents: agent::Agent,
    pub(crate) journal: Journal<Write, Output>,
    pub(crate) numbers: Numbers,
    pub(crate) charter_endpoints: Box<[jig_charter::EndpointName]>,
    pub(crate) assignments: Map<u64, Assignment>,
    pub(crate) briefs: Map<u64, Box<[jig_charter::Section]>>,
    pub(crate) calls: Map<core::CallKey, CallEnvelope>,
    pub(crate) read_calls: Map<Token, ReadCall>,
    pub(crate) payloads: Map<Token, Payload>,
    pub(crate) effects: Map<Token, infrastructure::Effect>,
    pub(crate) judges: Map<Token, (Token, authority::Judge, [u8; 32])>,
    pub(crate) procedures: Map<u64, (u64, u16, u32)>,
    pub(crate) effect_procedures: Map<Token, u64>,
    pub(crate) agent_owners: Map<Token, Token>,
    pub(crate) agent_calls: Map<(Token, Token), [u8; 16]>,
    pub(crate) host_calls: Map<Box<[u8]>, HostCall>,
    pub(crate) next_token: u64,
    pub(crate) wrote: bool,
    pub(crate) stopped_reported: bool,
    pub(crate) restarting: bool,
    /// Header supplied by the core in this step; applied after its empty
    /// restore decision is accepted, so journal admission remains paired.
    pub(crate) restored_commits: Option<u64>,
}

impl Domain {
    /// Construct without issuing a request; the caller supplies the first input.
    #[must_use]
    pub fn new(config: Config, limits: &Limits) -> Self {
        let room = crate::limits::room(limits).expect("finite root route");
        assert!(room.writes <= limits.journal.writes && room.held <= limits.journal.held, "admit the complete route");
        assert!(config.numbers.observability != config.numbers.infrastructure, "distinct child labels");
        assert!(limits.routes > 0 && limits.journal.now > 0, "nonempty route and door");
        assert!(
            limits.host.slots == limits.agent.slots && limits.host.slots == limits.core.fleet.engine_slots,
            "engine slot agreement"
        );
        assert!(
            config.charter_endpoints.len() <= usize::try_from(limits.agent.smith.endpoints).expect("endpoint count"),
            "bounded codec endpoints"
        );
        for endpoint in &config.charter_endpoints {
            assert!(
                u64::try_from(endpoint.name.len()).expect("endpoint bytes") <= limits.host.charter_bytes,
                "endpoint name bound"
            );
        }
        let agent_calls = limits.host.slots.checked_mul(limits.agent.smith.run.calls).expect("agent callbacks");
        let host_calls = limits.host.slots.checked_mul(limits.host.run_calls).expect("hub relays");
        Self {
            core: core::Core::new(config.core, &limits.core),
            observability: observability::Domain::with_templates(&limits.observability, config.triage_templates),
            infrastructure: infrastructure::Domain::new(config.backend, &limits.infrastructure),
            host: host::Domain::new(&limits.host),
            agents: agent::Agent::new(&limits.agent, config.endpoints, config.endpoint_names, config.seed),
            journal: Journal::new(&limits.journal),
            numbers: config.numbers,
            charter_endpoints: config.charter_endpoints,
            assignments: Map::with_capacity(limits.core.tasks.tasks),
            briefs: Map::with_capacity(limits.core.tasks.tasks),
            calls: Map::with_capacity(limits.core.call_records),
            read_calls: Map::with_capacity(limits.observability.reads),
            payloads: Map::with_capacity(
                limits.core.fleet.turns.checked_add(limits.core.fleet.attempts).expect("payloads"),
            ),
            effects: Map::with_capacity(
                limits.core.call_records.checked_add(limits.core.tasks.tasks).expect("effect flights"),
            ),
            judges: Map::with_capacity(limits.core.authority.facts),
            procedures: Map::with_capacity(limits.core.tasks.tasks),
            effect_procedures: Map::with_capacity(limits.core.tasks.tasks),
            agent_owners: Map::with_capacity(limits.host.slots),
            agent_calls: Map::with_capacity(agent_calls),
            host_calls: Map::with_capacity(host_calls),
            next_token: 1,
            wrote: false,
            stopped_reported: false,
            restarting: false,
            restored_commits: None,
        }
    }

    /// Decisions open only when the core's cold-start script says so.
    #[must_use]
    pub fn ready(&self) -> bool {
        !self.restarting || self.core.restart_ready()
    }

    /// The iterate adapter may park once the journal and immediate work settle.
    #[must_use]
    pub fn quiescent(&self) -> bool {
        self.journal.idle()
            && (!self.ready()
                || (!self.host.is_ready()
                    && !self.agents.is_ready()
                    && (self.core.due.is_empty() || !self.core.accounts.usable(self.core.settings.account))))
    }

    /// Reclaim child-retired slots after an iteration.
    pub fn reclaim(&mut self) {
        self.core.reclaim();
        self.host.reclaim();
        agent::reclaim(&mut self.agents);
    }

    pub(crate) fn token(&mut self) -> Token {
        let token = Token::new(self.next_token);
        self.next_token = self.next_token.checked_add(1).expect("continuation numbering");
        token
    }
    pub(crate) fn payload(&mut self, payload: Payload) -> Token {
        let token = self.token();
        self.payloads.insert(token, payload).expect("admitted child payload room");
        token
    }
}

/// Synchronous hand-offs, bounded by one application's configured route room.
#[derive(Debug)]
pub(crate) enum Work {
    Event(Event),
    Core(core::Event),
    ResumeFleet,
    Infrastructure(infrastructure::Event),
    Observability(observability::Event),
    Host(host::Event),
    Agent(smith_host_domain::Event),
    Restart(core::RestartRequest),
    AdoptDone,
}

/// Store terminals update the journal; every other input admits one decision.
pub fn step(domain: &mut Domain, env: &Env<Limits>, event: Event) {
    match event {
        Event::Store(Store::Committed { number }) => domain.journal.committed(number),
        Event::Store(Store::Failed { number }) => domain.journal.failed(number),
        other @ (Event::Core(_)
        | Event::Infrastructure(_)
        | Event::Observability(_)
        | Event::Host(_)
        | Event::Llm { .. }
        | Event::Store(Store::Restore(_) | Store::Restored(_) | Store::Transcript { .. } | Store::Loaded(_))
        | Event::Released(_)
        | Event::Restart
        | Event::Timer(_)
        | Event::ObservabilityTimer
        | Event::InfrastructureTimer
        | Event::HostTimer
        | Event::Effect { .. }) => decide(domain, env, Work::Event(other)),
    }
}

fn decide(domain: &mut Domain, env: &Env<Limits>, first: Work) {
    let room = match &first {
        // A read only keeps or consumes a volatile callback. It needs no
        // durable slot, including while a commit is in flight.
        Work::Event(
            Event::Core(core::Event::Fleet(fleet::Event::Relay { call: fleet::Call { writes: false, .. }, .. }))
            | Event::Host(host::Event::Called { ask: host::Ask::Relay { writes: false, .. }, .. })
            | Event::Observability(
                observability::Event::Read { .. }
                | observability::Event::System(observability::SystemEvent::ReadDone { .. }),
            ),
        ) => skein_lib::JournalRoom { writes: 0, held: 0 },
        Work::Event(_)
        | Work::Core(_)
        | Work::ResumeFleet
        | Work::Infrastructure(_)
        | Work::Observability(_)
        | Work::Host(_)
        | Work::Agent(_)
        | Work::Restart(_)
        | Work::AdoptDone => crate::limits::room(&env.limits).expect("validated route room"),
    };
    let Some(mut decision) = domain.journal.decision(&room) else {
        let _accepted = domain.journal.now(Output::Busy);
        return;
    };
    domain.wrote = false;
    let mut work = Queue::with_capacity(env.limits.routes);
    work.push(first);
    for _ in 0..env.limits.routes {
        let next = match work.pop() {
            Some(next) => next,
            None => {
                // Coalesce projections only after ordinary hand-offs quiesce.
                let requests = core::finish_decision(
                    &mut domain.core,
                    &Env { now: env.now, wall: env.wall, limits: env.limits.core },
                );
                crate::route::core_out(domain, env, &mut decision, requests, &mut work);
                let Some(next) = work.pop() else { break };
                next
            }
        };
        crate::route::work(domain, env, &mut decision, next, &mut work);
    }
    assert!(work.is_empty(), "complete synchronous route fits its admission");
    if domain.wrote || domain.core.counters.dirty() {
        let header = domain.core.counters.next_commit();
        write(
            domain,
            &mut decision,
            Write::Save(Record::Core(core::Record::Core(core::CoreRecord::Deployment(header)))),
        );
    }
    domain.journal.accept(decision);
    if let Some(commits) = domain.restored_commits.take() {
        assert!(domain.journal.idle(), "the deployment header is the first cold-start row");
        domain.journal = Journal::from_durable(&env.limits.journal, commits);
    }
}

/// The adapter supplies the due owner's timer; admission precedes firing it.
/// Copy this dispatch and replace only the application's connector timers.
pub fn fire(domain: &mut Domain, env: &Env<Limits>, timer: crate::Timer) {
    let event = match timer {
        crate::Timer::Core(timer) => Event::Timer(timer),
        crate::Timer::Observability => Event::ObservabilityTimer,
        crate::Timer::Infrastructure => Event::InfrastructureTimer,
        crate::Timer::Agent => Event::HostTimer,
    };
    step(domain, env, event);
}

/// Iterate drains only the journal: one commit and a bounded ordered release.
pub fn release(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Output>) {
    if domain.journal.stopped() || domain.core.restart_failure().is_some() {
        if !domain.stopped_reported {
            domain.stopped_reported = true;
            out.push(Output::Stop);
        }
        return;
    }
    if let Some(commit) = domain.journal.commit() {
        out.push(Output::Commit { number: commit.number, writes: commit.writes });
    }
    let mut ready = Queue::with_capacity(env.limits.journal.release);
    let _released = domain.journal.release(&mut ready);
    for _ in 0..ready.len() {
        out.push(ready.pop().expect("journal release count"));
    }
    if domain.journal.idle() && domain.ready() {
        if domain.core.accounts.usable(domain.core.settings.account)
            && let Some(context) = domain.core.due.pop()
        {
            decide(domain, env, Work::Core(core::Event::Activate { context, ready: true }));
        } else {
            decide(domain, env, Work::ResumeFleet);
        }
    }
}

pub(crate) fn write(domain: &mut Domain, decision: &mut Decision<Write, Output>, write: Write) {
    domain.wrote = true;
    decision.write(write).expect("reserved whole-route writes");
}
pub(crate) fn held(decision: &mut Decision<Write, Output>, value: Output) {
    decision.hold(value).expect("reserved whole-route outputs");
}
pub(crate) fn child(decision: &mut Decision<Write, Output>, value: Released) {
    held(decision, Output::ToChild(value));
}
pub(crate) fn now(domain: &mut Domain, value: Output) {
    domain.journal.now(value).expect("bounded door drained by each domain iteration");
}
