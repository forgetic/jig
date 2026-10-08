//! In-process Smith composition behind the process host's parent vocabulary.

use alloc::boxed::Box;
use skein_lib::{Env, List, Map, Queue, Time, Token};
use smith_domain as smith;
use smith_host_domain as host;
use smith_protocol_channel as protocol;

use crate::boundary::{Completion, Request};
use crate::limits::{self, Limits};
use crate::translation;

#[derive(Debug)]
pub(crate) struct Relay {
    name: smith::run::RelayName,
    withdrawn: bool,
}

#[derive(Debug)]
#[expect(
    clippy::struct_field_names,
    reason = "Smith's admitted run and the parent's logical run are different identifiers"
)]
pub(crate) struct Run {
    smith: Option<smith::Domain>,
    smith_run: Option<Token>,
    logical_run: Token,
    relays: Map<Token, Relay>,
    completions: Map<Token, bool>,
    messages: Queue<Token>,
    read: Option<Token>,
    cancel_deadline: Option<Time>,
    answered: bool,
}

/// Bounded collection of independent composed Smith runs.
#[derive(Debug)]
pub struct Agent {
    runs: Map<Token, Run>,
    endpoints: Box<[smith::run::charter::Endpoint]>,
    names: protocol::Endpoints,
    lower: Queue<smith::Request>,
    seed: u64,
}

impl Agent {
    /// Construct the agent with the same endpoint identity table used by its codecs.
    #[must_use]
    pub fn new(
        limits: &Limits,
        endpoints: Box<[smith::run::charter::Endpoint]>,
        names: protocol::Endpoints,
        seed: u64,
    ) -> Self {
        assert!(limits::worst_case(limits).is_some(), "inline agent limits have a worst case");
        assert!(
            endpoints.len() <= usize::try_from(limits.smith.endpoints).expect("u32 cap"),
            "configured endpoints fit Smith's limit"
        );
        for (index, endpoint) in endpoints.iter().enumerate() {
            for other in endpoints.get(index.saturating_add(1)..).unwrap_or_default() {
                assert!(endpoint != other, "endpoint identities are unique");
            }
        }
        Self {
            runs: Map::with_capacity(limits.slots),
            endpoints,
            names,
            lower: Queue::with_capacity(smith::max_out(&limits.smith)),
            seed,
        }
    }

    /// Number of occupied run slots.
    #[must_use]
    pub fn hosted(&self) -> u32 {
        self.runs.len()
    }

    /// Whether a hosted Smith domain has deferred work for [`resume`].
    #[must_use]
    pub fn is_ready(&self) -> bool {
        for (_, run) in &self.runs {
            if let Some(smith) = &run.smith
                && smith.is_ready()
            {
                return true;
            }
        }
        false
    }

    /// Drain one content-free observation from a live Smith domain.
    pub fn pop_fact(&mut self, client: Token) -> Option<smith::Fact> {
        self.runs.get_mut(&client)?.smith.as_mut()?.pop_fact()
    }

    /// Drain one bounded content observation from a live Smith domain.
    pub fn pop_content(&mut self, client: Token) -> Option<smith::Content> {
        self.runs.get_mut(&client)?.smith.as_mut()?.pop_content()
    }
}

/// Maximum requests produced by one call into this child.
#[must_use]
pub const fn max_out(limits: &Limits) -> u32 {
    limits::max_out(limits)
}

fn child_env(env: &Env<Limits>) -> Env<smith::Limits> {
    Env { now: env.now, wall: env.wall, limits: env.limits.smith }
}

fn smith_step(agent: &mut Agent, env: &Env<Limits>, client: Token, event: smith::Event, out: &mut Queue<Request>) {
    let run = agent.runs.get_mut(&client).expect("Smith event has a live run");
    smith::step(run.smith.as_mut().expect("active Smith domain"), &child_env(env), event, &mut agent.lower);
    route(agent, env, client, out);
}

fn spawn(agent: &mut Agent, env: &Env<Limits>, client: Token, start: host::Start, out: &mut Queue<Request>) {
    if agent.runs.contains_key(&client) || agent.runs.len() >= env.limits.slots {
        out.push(Request::Host(host::Request::Gone { client, end: host::End::Busy, detail: Box::new([]) }));
        return;
    }
    let logical_run = start.logical_run;
    let Some(event) = translation::start(client, start, &env.limits, &agent.names) else {
        out.push(Request::Host(host::Request::Gone {
            client,
            end: host::End::Invalid(host::Invalid::Charter),
            detail: Box::new([]),
        }));
        return;
    };
    let run = Run {
        smith: Some(smith::Domain::new(
            &env.limits.smith,
            smith::Config { endpoints: agent.endpoints.clone() },
            agent.seed ^ client.raw(),
        )),
        smith_run: None,
        logical_run,
        relays: Map::with_capacity(env.limits.smith.run.calls),
        completions: Map::with_capacity(env.limits.smith.run.conversations),
        messages: Queue::with_capacity(env.limits.smith.run.messages),
        read: None,
        cancel_deadline: None,
        answered: false,
    };
    assert!(agent.runs.insert(client, run).expect("a free slot fits").is_none(), "client has no prior run");
    out.push(Request::Host(host::Request::Started { client, agent: client }));
    smith_step(agent, env, client, event, out);
}

fn message(
    agent: &mut Agent,
    env: &Env<Limits>,
    client: Token,
    name: Token,
    label: Box<[u8]>,
    text: Box<[u8]>,
    out: &mut Queue<Request>,
) {
    let Some(run) = agent.runs.get(&client) else {
        out.push(Request::Host(host::Request::Bounced { client, name, bounce: host::Bounce::Ending }));
        return;
    };
    let Some(smith_run) = run.smith_run else {
        out.push(Request::Host(host::Request::Bounced { client, name, bounce: host::Bounce::Ending }));
        return;
    };
    if run.cancel_deadline.is_some() || run.answered {
        out.push(Request::Host(host::Request::Bounced { client, name, bounce: host::Bounce::Ending }));
        return;
    }
    let mut reused = run.read == Some(name);
    for old in &run.messages {
        if *old == name {
            reused = true;
        }
    }
    if reused {
        out.push(Request::Host(host::Request::Bounced { client, name, bounce: host::Bounce::ReusedName }));
        return;
    }
    if run.messages.len() >= env.limits.smith.run.messages {
        out.push(Request::Host(host::Request::Bounced { client, name, bounce: host::Bounce::Full }));
        return;
    }
    let prefix = label.len().checked_add(2);
    let length = match prefix {
        Some(prefix) => prefix.checked_add(text.len()),
        None => None,
    };
    let Some(length) = length else {
        out.push(Request::Host(host::Request::Bounced { client, name, bounce: host::Bounce::TooLarge }));
        return;
    };
    if label.is_empty() || length > usize::try_from(env.limits.smith.run.message_bytes).expect("u32 cap") {
        out.push(Request::Host(host::Request::Bounced { client, name, bounce: host::Bounce::TooLarge }));
        return;
    }
    let mut joined = List::with_capacity(u32::try_from(length).expect("bounded length"));
    for byte in label {
        joined.push(byte).expect("precounted label");
    }
    joined.push(b':').expect("precounted separator");
    joined.push(b' ').expect("precounted separator");
    for byte in text {
        joined.push(byte).expect("precounted text");
    }
    agent.runs.get_mut(&client).expect("live run").messages.push(name);
    smith_step(agent, env, client, smith::Event::Message { run: smith_run, name, text: joined.into_boxed() }, out);
}

fn read(agent: &mut Agent, client: Token, seen: Option<Token>) {
    let Some(seen) = seen else { return };
    let run = agent.runs.get_mut(&client).expect("read belongs to live run");
    if run.read == Some(seen) {
        return;
    }
    while let Some(name) = run.messages.pop() {
        if name == seen {
            run.read = Some(name);
            break;
        }
    }
}

/// Consume one parent event; process-specific lower terminals have no inline owner.
pub fn step(agent: &mut Agent, env: &Env<Limits>, event: host::Event, out: &mut Queue<Request>) {
    match event {
        host::Event::Spawn { client, start } => spawn(agent, env, client, start, out),
        host::Event::Message { agent: client, name, label, text } => {
            message(agent, env, client, name, label, text, out);
        }
        host::Event::Answer { agent: client, call, reply } => {
            if let Some(run) = agent.runs.get_mut(&client)
                && let Some(relay) = run.relays.remove(&call)
            {
                if !run.answered
                    && let Some(reply) = translation::host_reply(reply)
                {
                    smith_step(agent, env, client, smith::Event::HostReturned { relay: relay.name, reply }, out);
                }
                settle(agent, client, out);
            }
        }
        host::Event::Acknowledge { agent: client, turn } => {
            if let Some(run) = agent.runs.get(&client)
                && !run.answered
                && let Some(smith_run) = run.smith_run
            {
                smith_step(agent, env, client, smith::Event::Acknowledge { run: smith_run, turn }, out);
            }
        }
        host::Event::Grant { agent: client, grant } => {
            if let Some(run) = agent.runs.get(&client)
                && !run.answered
            {
                smith_step(
                    agent,
                    env,
                    client,
                    smith::Event::Grant {
                        grant: smith::Grant {
                            name: smith::GrantName { account: grant.account, generation: grant.generation },
                            valid: grant.valid,
                        },
                    },
                    out,
                );
            }
        }
        host::Event::Stop { agent: client } => {
            if let Some(run) = agent.runs.get_mut(&client)
                && run.cancel_deadline.is_none()
                && !run.answered
            {
                run.cancel_deadline = Some(env.now.saturating_add(env.limits.cancel_grace));
                if let Some(smith_run) = run.smith_run {
                    smith_step(agent, env, client, smith::Event::Cancel { run: smith_run }, out);
                }
            }
        }
        host::Event::Spawned { .. }
        | host::Event::Unspawned { .. }
        | host::Event::Sent { .. }
        | host::Event::Unsent { .. }
        | host::Event::Received { .. }
        | host::Event::Malformed { .. }
        | host::Event::Hangup { .. }
        | host::Event::Signalled { .. }
        | host::Event::Exited { .. }
        | host::Event::Reaped { .. } => {}
    }
}

/// Return one provider terminal directly from the root's protocol layer.
pub fn terminal(agent: &mut Agent, env: &Env<Limits>, client: Token, completion: Completion, out: &mut Queue<Request>) {
    let owner = match &completion {
        Completion::Completed { owner, .. } | Completion::Failed { owner, .. } | Completion::Cancelled { owner } => {
            *owner
        }
    };
    let Some(run) = agent.runs.get_mut(&client) else { return };
    if run.completions.remove(&owner).is_none() {
        return;
    }
    if run.answered {
        settle(agent, client, out);
        return;
    }
    let event = match completion {
        Completion::Completed { owner, completion } => smith::Event::Completed { owner, completion },
        Completion::Failed { owner, failure, evidence, detail } => {
            smith::Event::Failed { owner, failure, evidence, detail }
        }
        Completion::Cancelled { owner } => smith::Event::Cancelled { owner },
    };
    smith_step(agent, env, client, event, out);
}

#[expect(clippy::too_many_lines, reason = "every Smith request has one explicit route")]
fn route(agent: &mut Agent, env: &Env<Limits>, client: Token, out: &mut Queue<Request>) {
    for _ in 0..smith::max_out(&env.limits.smith) {
        let Some(request) = agent.lower.pop() else { break };
        let run = agent.runs.get(&client).expect("output belongs to live run");
        match request {
            smith::Request::Admitted { host_run, run: smith_run } => {
                assert_eq!(run.logical_run, host_run, "admission belongs to the hosted scope");
                agent.runs.get_mut(&client).expect("live run").smith_run = Some(smith_run);
                out.push(Request::Host(host::Request::Admitted { client }));
            }
            smith::Request::Turn { host_run, number, read, spent, turn, .. } => {
                assert_eq!(run.logical_run, host_run, "turn belongs to the hosted scope");
                self::read(agent, client, read);
                let body = protocol::encode_turn(&turn, &env.limits.transcript, &agent.names)
                    .expect("Smith turn fits configured transcript codec");
                out.push(Request::Host(host::Request::Turn {
                    client,
                    turn: host::Turn { number, spent: spent.units, read, body },
                }));
            }
            smith::Request::HostCall { host_run, relay, name, tool, effect, input, deadline } => {
                assert_eq!(run.logical_run, host_run, "call belongs to the hosted scope");
                assert!(
                    agent
                        .runs
                        .get_mut(&client)
                        .expect("live run")
                        .relays
                        .insert(relay.owner, Relay { name: relay, withdrawn: false })
                        .expect("bounded calls")
                        .is_none(),
                    "live relay owners are unique"
                );
                out.push(Request::Host(host::Request::Called {
                    client,
                    logical_run: host_run,
                    call: relay.owner,
                    name: host::CallName {
                        activation: name.activation,
                        completion: name.completion,
                        position: name.position,
                    },
                    deadline,
                    ask: host::Ask::Host {
                        tool,
                        effect: match effect {
                            smith::run::HostEffect::Read => host::Effect::Read,
                            smith::run::HostEffect::Write => host::Effect::Write,
                        },
                        body: Box::from(input.bytes()),
                    },
                }));
            }
            smith::Request::WithdrawHost { relay } => {
                if let Some(pending) = agent.runs.get_mut(&client).expect("live run").relays.get_mut(&relay.owner)
                    && !pending.withdrawn
                {
                    pending.withdrawn = true;
                    out.push(Request::Host(host::Request::Withdrawn { client, call: relay.owner }));
                }
            }
            smith::Request::Waiting { host_run, read } => {
                assert_eq!(run.logical_run, host_run, "wait belongs to the hosted scope");
                self::read(agent, client, read);
                out.push(Request::Host(host::Request::Waiting { client, read }));
            }
            smith::Request::Answer { to, answer } => {
                assert_eq!(to.into_token(), client, "answer belongs to the hosted client");
                let answer =
                    translation::answer(answer, &env.limits.charter).expect("Smith answer fits configured codec");
                out.push(Request::Host(host::Request::Answered { client, answer }));
                finish(agent, client, out);
            }
            request @ smith::Request::Complete { owner, .. } => {
                assert!(
                    agent
                        .runs
                        .get_mut(&client)
                        .expect("live run")
                        .completions
                        .insert(owner, false)
                        .expect("bounded completions")
                        .is_none(),
                    "live completion owners are unique"
                );
                out.push(Request::Lower { client, request });
            }
            request @ smith::Request::Cancel { owner } => {
                if let Some(cancelled) = agent.runs.get_mut(&client).expect("live run").completions.get_mut(&owner) {
                    *cancelled = true;
                }
                out.push(Request::Lower { client, request });
            }
            smith::Request::Rejected { grant } => {
                out.push(Request::Host(host::Request::Rejected {
                    client,
                    account: grant.account,
                    generation: grant.generation,
                }));
            }
            smith::Request::Exhausted { account, retry_after } => {
                out.push(Request::Host(host::Request::Exhausted { client, account, retry_after }));
            }
            smith::Request::Checking { .. }
            | smith::Request::ChecksEnded { .. }
            | smith::Request::Deliver { .. }
            | smith::Request::Io { .. }
            | smith::Request::CancelIo { .. }
            | smith::Request::Read { .. }
            | smith::Request::Probe { .. }
            | smith::Request::Check { .. }
            | smith::Request::Abort { .. } => unreachable!("inline charter has no workspace"),
        }
    }
}

fn finish(agent: &mut Agent, client: Token, out: &mut Queue<Request>) {
    let run = agent.runs.get_mut(&client).expect("answered run was live");
    run.answered = true;
    run.smith = None;
    let mut owners = List::with_capacity(run.completions.capacity());
    for (owner, cancelled) in &run.completions {
        if !cancelled {
            owners.push(*owner).expect("bounded owners");
        }
    }
    for owner in &owners {
        *run.completions.get_mut(owner).expect("collected owner") = true;
        out.push(Request::Lower { client, request: smith::Request::Cancel { owner: *owner } });
    }
    let mut withdraw = List::with_capacity(run.relays.capacity());
    for (owner, relay) in &run.relays {
        if !relay.withdrawn {
            withdraw.push(*owner).expect("bounded relays");
        }
    }
    for owner in &withdraw {
        run.relays.get_mut(owner).expect("collected relay").withdrawn = true;
        out.push(Request::Host(host::Request::Withdrawn { client, call: *owner }));
    }
    settle(agent, client, out);
}

fn settle(agent: &mut Agent, client: Token, out: &mut Queue<Request>) {
    let Some(run) = agent.runs.get(&client) else { return };
    if run.answered && run.completions.is_empty() && run.relays.is_empty() {
        agent.runs.remove(&client);
        out.push(Request::Host(host::Request::Gone { client, end: host::End::Stopped, detail: Box::new([]) }));
    }
}

/// Earliest child or cancellation-grace alarm.
#[must_use]
pub fn next_deadline(agent: &Agent) -> Option<Time> {
    let mut earliest: Option<Time> = None;
    for (_, run) in &agent.runs {
        if let Some(smith) = &run.smith
            && let Some(candidate) = smith.next_deadline()
        {
            earliest = Some(earlier(earliest, candidate));
        }
        if let Some(candidate) = run.cancel_deadline {
            earliest = Some(earlier(earliest, candidate));
        }
    }
    earliest
}

fn earlier(current: Option<Time>, candidate: Time) -> Time {
    match current {
        Some(current) => current.min(candidate),
        None => candidate,
    }
}

/// Fire one due run, or drop a run that exceeded its cancellation grace.
pub fn fire(agent: &mut Agent, env: &Env<Limits>, out: &mut Queue<Request>) {
    for slot in 0..env.limits.slots {
        let Some((client, _)) = agent.runs.iter().nth(usize::try_from(slot).expect("u32 index")) else { break };
        let client = *client;
        let run = agent.runs.get(&client).expect("iterated run");
        if let Some(smith) = &run.smith
            && smith.is_due(env.now)
        {
            smith::fire(
                agent.runs.get_mut(&client).expect("live run").smith.as_mut().expect("active domain"),
                &child_env(env),
                &mut agent.lower,
            );
            route(agent, env, client, out);
            return;
        }
        if let Some(deadline) = run.cancel_deadline
            && deadline <= env.now
        {
            out.push(Request::Host(host::Request::Faulted { client, fault: host::Fault::Exited }));
            finish(agent, client, out);
            return;
        }
    }
}

/// Advance one deferred Smith handoff.
pub fn resume(agent: &mut Agent, env: &Env<Limits>, out: &mut Queue<Request>) {
    for slot in 0..env.limits.slots {
        let Some((client, _)) = agent.runs.iter().nth(usize::try_from(slot).expect("u32 index")) else { break };
        let client = *client;
        if let Some(smith) = &agent.runs.get(&client).expect("iterated run").smith
            && smith.is_ready()
        {
            smith::resume(
                agent.runs.get_mut(&client).expect("live run").smith.as_mut().expect("active domain"),
                &child_env(env),
                &mut agent.lower,
            );
            route(agent, env, client, out);
            return;
        }
    }
}

/// Reclaim every child domain at the iteration boundary.
pub fn reclaim(agent: &mut Agent) {
    for slot in 0..agent.runs.len() {
        let client = *agent.runs.iter().nth(usize::try_from(slot).expect("u32 index")).expect("bounded slot").0;
        if let Some(smith) = &mut agent.runs.get_mut(&client).expect("iterated run").smith {
            smith.reclaim();
        }
    }
}
