//! Hosted run lifecycle (hosts.md, section 6). An accepted run prepares a
//! workspace, starts its agent, then serves calls and turns until it stops.
//! The host waits for agent exit and each in-flight delivery before saving,
//! releasing, and answering. The same tail follows cancellation or a fault.

use alloc::boxed::Box;
use core::mem;

use skein_lib::bytes::copy_of;
use skein_lib::{Env, Id, List, Map, Queue, ReplyTo, Set, Slab, Token};

use crate::assignment::{self, len};
use crate::boundary::{
    AgentFailure, Answer, AnsweredCall, Ask, Assignment, AssignmentTyped, Bounce, Delivery, Failure, Finish, Grant,
    Hosting, Phase, Preparation, Reason, Refusal, Reply, Request, RunFailure, SettledAnswer, Work,
};
use crate::call::{self, Call};
use crate::domain::Domain;
use crate::facts::{Fact, Facts, Told};
use crate::limits::Limits;

#[derive(Debug)]
pub(crate) struct Hosted {
    /// The engine's names for the run and for the attempt hosted.
    run: Token,
    attempt: Token,
    grants: Box<[Grant]>,
    refreshed: bool,
    /// Whether the workspace saves unfinished work before release.
    save: bool,
    /// The application's token for the last delivery that changed the workspace.
    left: Option<Token>,
    /// Its relayed calls in flight, while it is live.
    relays: Set<Id<Call>>,
    /// Its delivery in flight, if it has one: a write, so one at a time, and
    /// waited for through the stop.
    delivery: Option<Id<Call>>,
    runtime: Runtime,
    state: State,
}

#[derive(Debug)]
struct Runtime {
    start: Option<TypedStart>,
    turns: u32,
    spent: u64,
}

type CallName = Box<[u8]>;

fn reply(agent: Token, call: CallName, reply: Reply, out: &mut Queue<Request>) {
    out.push(Request::ReplyTyped { agent, call, reply });
}

fn take_name(call: &mut Call) -> CallName {
    call.typed.take().expect("one reply for each typed call")
}

#[derive(Debug)]
struct TypedStart {
    turns: Box<[Box<[u8]>]>,
    answered: Box<[AnsweredCall]>,
}

#[derive(Debug)]
pub(crate) struct NamedEvent {
    name: Token,
    event: Box<[u8]>,
    sender: Option<Box<[u8]>>,
}

#[derive(Debug)]
enum State {
    /// Its workspace is being prepared. The charter and the snapshot wait for
    /// the agent, and inbound events in `held`.
    Preparing { reply_to: ReplyTo, charter: Box<[u8]>, snapshot: Option<Box<[u8]>>, held: Queue<NamedEvent> },
    /// Cancelled for `reason` as it was prepared: it answers once the prepare
    /// has settled.
    Cancelling { reply_to: ReplyTo, reason: Reason },
    /// Its agent is starting, with a workspace when it has items.
    Starting { reply_to: ReplyTo, workspace: Option<Token>, held: Queue<NamedEvent> },
    /// Cancelled for `reason` as its agent started: the agent is stopped once
    /// it has started.
    Unwanted { reply_to: ReplyTo, workspace: Option<Token>, reason: Reason },
    /// Its agent `agent` is at work, with a workspace when it has items.
    Active { reply_to: ReplyTo, workspace: Option<Token>, agent: Token },
    /// It yielded, and waits for its next inbound event.
    Waiting { reply_to: ReplyTo, workspace: Option<Token>, agent: Token },
    /// It ends with `ending` once its agent has gone (`gone`) and its delivery in
    /// flight has settled.
    Stopping { reply_to: ReplyTo, workspace: Option<Token>, agent: Token, ending: Ending, gone: bool },
    /// Its unfinished work is being saved; it ends with `ending` once it is.
    Saving { reply_to: ReplyTo, workspace: Token, ending: Ending },
    /// Terminal: holds nothing.
    Closed,
}

/// How a run ends.
#[derive(Debug)]
enum Ending {
    /// It ended with `outcome`, as it said.
    Ended { outcome: Box<[u8]> },
    /// It parked, as it said.
    Parked { snapshot: Option<Box<[u8]>> },
    /// It failed: as it said, or its agent broke the rules saying it, or
    /// exited without a word.
    Failed { failure: Failure, detail: Box<[u8]> },
    /// The host stopped it, for `failure`: a cancel, or a fault of its
    /// agent. What the run says before its agent has gone still wins.
    Stopped { failure: Failure, detail: Box<[u8]> },
}

// Entry points, one per event: look the run up, take its state out, run the
// cell's handler, conclude from the run's new state.

pub(crate) fn assign_typed(
    domain: &mut Domain,
    env: &Env<Limits>,
    reply_to: ReplyTo,
    next: AssignmentTyped,
    out: &mut Queue<Request>,
) {
    let AssignmentTyped { assignment, turns, answered } = next;
    if assignment.snapshot.is_some() || !typed_fits(&turns, &answered, &env.limits) {
        let invalid = if assignment.snapshot.is_some() { crate::Invalid::Version } else { crate::Invalid::Transcript };
        refuse_v2(reply_to, &assignment, Refusal::Invalid(invalid), out);
        return;
    }
    let typed = TypedStart { turns, answered };
    assign_runtime(domain, env, reply_to, assignment, Runtime { start: Some(typed), turns: 0, spent: 0 }, out);
}

fn typed_fits(turns: &[Box<[u8]>], answered: &[AnsweredCall], limits: &Limits) -> bool {
    let Ok(turn_count) = u64::try_from(turns.len()) else { return false };
    let Ok(turn_size) = u64::try_from(size_of::<Box<[u8]>>()) else { return false };
    let Some(mut bytes) = turn_count.checked_mul(turn_size) else {
        return false;
    };
    let Ok(answer_count) = u64::try_from(answered.len()) else { return false };
    let Ok(answer_size) = u64::try_from(size_of::<AnsweredCall>()) else { return false };
    let Some(rows) = answer_count.checked_mul(answer_size) else {
        return false;
    };
    let Some(sum) = bytes.checked_add(rows) else { return false };
    bytes = sum;
    for body in turns {
        if len(body) > limits.turn_bytes {
            return false;
        }
        let Some(sum) = bytes.checked_add(len(body)) else { return false };
        bytes = sum;
    }
    for call in answered {
        if len(&call.name) > limits.event_bytes || len(&call.tool) > limits.event_bytes {
            return false;
        }
        let body = match &call.answer {
            SettledAnswer::Host { error: _, body } => len(body),
            SettledAnswer::Delivery { outcome: _, evidence } => {
                if len(evidence) > limits.delivery_evidence_bytes {
                    return false;
                }
                len(evidence)
            }
        };
        let Some(sum) = bytes.checked_add(len(&call.name)) else { return false };
        let Some(sum) = sum.checked_add(len(&call.tool)) else { return false };
        let Some(sum) = sum.checked_add(body) else { return false };
        bytes = sum;
    }
    bytes <= limits.transcript_bytes
}

fn assign_runtime(
    domain: &mut Domain,
    env: &Env<Limits>,
    reply_to: ReplyTo,
    assignment: Assignment,
    runtime: Runtime,
    out: &mut Queue<Request>,
) {
    // A fresh child call always consumes its own ReplyTo. Wire retries are
    // deduplicated by the parent before it creates such a call.
    if fenced(&domain.names, &domain.hosted, assignment.run, assignment.attempt).is_some() {
        refused(reply_to, &assignment, Refusal::Busy, &runtime, out);
        return;
    }
    let Domain { hosted, names, facts, shut, unacknowledged, .. } = domain;
    // An assignment that can never fit is invalid, room or not: busy invites a
    // retry.
    if let Err(invalid) = assignment::check(&assignment, &env.limits) {
        refused(reply_to, &assignment, Refusal::Invalid(invalid), &runtime, out);
        return;
    }
    // A slot whose run has answered is not free until the engine has the
    // answer.
    let taken = hosted.len().saturating_add(*unacknowledged);
    if *shut || taken >= env.limits.slots || names.contains_key(&assignment.run) {
        refused(reply_to, &assignment, Refusal::Busy, &runtime, out);
        return;
    }
    let Assignment { run, attempt, workspace, save, charter, snapshot, grants } = assignment;
    let held = Queue::with_capacity(env.limits.held);
    let entry = Hosted {
        run,
        attempt,
        grants,
        refreshed: false,
        save: save && workspace.is_some(),
        left: None,
        relays: Set::with_capacity(env.limits.run_calls),
        delivery: None,
        runtime,
        state: State::Preparing { reply_to, charter, snapshot, held },
    };
    let id = hosted.insert(entry).expect("checked for room above");
    let named = names.insert(run, id).expect("a name for every slot");
    assert!(named.is_none(), "checked the run is not hosted above");
    facts.push(Fact::Admitted { run, attempt });
    match workspace {
        Some(workspace) => out.push(Request::Prepare { owner: id.token(), workspace }),
        None => start_prepared(domain, id.token(), None, out),
    }
}

#[expect(clippy::too_many_arguments, reason = "one typed message's fields enter through the host boundary")]
pub(crate) fn inbound_typed(
    domain: &mut Domain,
    env: &Env<Limits>,
    run: Token,
    attempt: Token,
    name: Token,
    sender: Box<[u8]>,
    words: Box<[u8]>,
    out: &mut Queue<Request>,
) {
    inbound_named(domain, env, run, attempt, NamedEvent { name, event: words, sender: Some(sender) }, out);
}

fn inbound_named(
    domain: &mut Domain,
    env: &Env<Limits>,
    run: Token,
    attempt: Token,
    message: NamedEvent,
    out: &mut Queue<Request>,
) {
    let Some(id) = fenced(&domain.names, &domain.hosted, run, attempt) else {
        return;
    };
    let sender_len = match &message.sender {
        Some(sender) => len(sender),
        None => 0,
    };
    let fits = match sender_len.checked_add(len(&message.event)) {
        Some(bytes) => bytes <= env.limits.event_bytes,
        None => false,
    };
    if !fits {
        out.push(Request::Bounced { run, attempt, name: message.name, bounce: Bounce::TooLarge });
        return;
    }
    let entry = domain.hosted.get_mut(id).expect("a named run is hosted");
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Preparing { reply_to, charter, snapshot, mut held } => {
            hold(&mut held, message, run, attempt, out);
            State::Preparing { reply_to, charter, snapshot, held }
        }
        State::Starting { reply_to, workspace, mut held } => {
            hold(&mut held, message, run, attempt, out);
            State::Starting { reply_to, workspace, held }
        }
        State::Active { reply_to, workspace, agent } | State::Waiting { reply_to, workspace, agent } => {
            deliver_message(agent, message, out);
            State::Active { reply_to, workspace, agent }
        }
        state @ (State::Cancelling { .. } | State::Unwanted { .. } | State::Stopping { .. } | State::Saving { .. }) => {
            out.push(Request::Bounced { run, attempt, name: message.name, bounce: Bounce::Ending });
            state
        }
        State::Closed => unreachable!("a closed run has left the names"),
    };
    drain_stopping(domain, env, id, out);
    conclude(domain, id, out);
}

pub(crate) fn cancel(domain: &mut Domain, env: &Env<Limits>, run: Token, attempt: Token, out: &mut Queue<Request>) {
    let Some(id) = fenced(&domain.names, &domain.hosted, run, attempt) else {
        return;
    };
    stop(domain, env, id, Reason::Engine, out);
}

pub(crate) fn is_hosting(domain: &Domain, run: Token, attempt: Token) -> bool {
    fenced(&domain.names, &domain.hosted, run, attempt).is_some()
}

pub(crate) fn is_relayed_for(domain: &Domain, run: Token, attempt: Token, call: Token) -> bool {
    let Some(id) = fenced(&domain.names, &domain.hosted, run, attempt) else {
        return false;
    };
    let Some(entry) = domain.calls.get(Id::<Call>::from_token(call)) else {
        return false;
    };
    if entry.hosted != id {
        return false;
    }
    match entry.state {
        call::State::Relayed { .. } | call::State::Settling => true,
        call::State::Delivering { .. } | call::State::Closed => false,
    }
}

pub(crate) fn relayed(
    domain: &mut Domain,
    run: Token,
    attempt: Token,
    call: Token,
    answer: Box<[u8]>,
    out: &mut Queue<Request>,
) {
    let Some(id) = fenced(&domain.names, &domain.hosted, run, attempt) else {
        return;
    };
    let call_id = Id::<Call>::from_token(call);
    let entry = domain.calls.get_mut(call_id).expect("a relay lives until its terminal");
    if entry.hosted != id {
        return;
    }
    match entry.state {
        call::State::Relayed { agent } => {
            let name = take_name(entry);
            reply(agent, name, Reply::Relayed { answer }, out);
        }
        call::State::Settling => {}
        call::State::Delivering { .. } | call::State::Closed => unreachable!("only a live relay receives an answer"),
    }
    relay_ended(domain, call_id, out);
}

pub(crate) fn relay_cancelled(domain: &mut Domain, call: Token, out: &mut Queue<Request>) {
    let call_id = Id::<Call>::from_token(call);
    let entry = domain.calls.get(call_id).expect("a cancelled relay lives until its terminal");
    match entry.state {
        call::State::Settling => {}
        call::State::Relayed { .. } | call::State::Delivering { .. } | call::State::Closed => {
            unreachable!("a relay is cancelled only after its cancel request")
        }
    }
    relay_ended(domain, call_id, out);
}

fn relay_ended(domain: &mut Domain, call_id: Id<Call>, out: &mut Queue<Request>) {
    let entry = domain.calls.get_mut(call_id).expect("a relay lives until its terminal");
    let id = entry.hosted;
    entry.state = call::State::Closed;
    domain.calls.retire(call_id);
    let entry = domain.hosted.get_mut(id).expect("a run lives until every relay settles");
    let removed = entry.relays.remove(&call_id);
    assert!(removed, "a relay belongs to its run until it settles");
    conclude(domain, id, out);
}

pub(crate) fn cancel_all(domain: &mut Domain, reason: Reason) {
    let Domain { names, ready, shut, .. } = domain;
    match reason {
        // A host shutting down takes no more work.
        Reason::Shutdown => *shut = true,
        Reason::Engine | Reason::Contact => {}
    }
    for (_, id) in names.iter() {
        if !ready.contains_key(id) {
            let earlier = ready.insert(*id, reason).expect("room on the ready list for every run");
            assert!(earlier.is_none(), "checked the run is not ready above");
        }
    }
}

/// Cancels the first run on the ready list, if there is one.
pub(crate) fn resume(domain: &mut Domain, env: &Env<Limits>, out: &mut Queue<Request>) {
    let Some((id, reason)) = domain.ready.first() else {
        return;
    };
    let (id, reason) = (*id, *reason);
    domain.ready.remove(&id);
    stop(domain, env, id, reason, out);
}

pub(crate) fn report(domain: &Domain, out: &mut Queue<Request>) {
    let mut runs = List::with_capacity(domain.names.len());
    for (_, id) in &domain.names {
        let entry = domain.hosted.get(*id).expect("a named run is hosted");
        let hosting = Hosting { run: entry.run, attempt: entry.attempt, phase: phase(&entry.state) };
        runs.push(hosting).expect("room for every named run");
    }
    out.push(Request::Hosting { runs: runs.into_boxed() });
}

pub(crate) fn prepared(domain: &mut Domain, owner: Token, workspace: Token, out: &mut Queue<Request>) {
    start_prepared(domain, owner, Some(workspace), out);
}

fn start_prepared(domain: &mut Domain, owner: Token, workspace: Option<Token>, out: &mut Queue<Request>) {
    let Domain { hosted, facts, .. } = domain;
    let id = Id::<Hosted>::from_token(owner);
    let entry = hosted.get_mut(id).expect("a run lives until its prepare has settled");
    if workspace.is_some() {
        facts.push(Fact::Prepared { run: entry.run, attempt: entry.attempt });
    }
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Preparing { reply_to, charter, snapshot, held } => {
            let mut grants = List::with_capacity(u32::try_from(entry.grants.len()).expect("validated grants"));
            for grant in &entry.grants {
                grants.push(*grant).expect("room for every grant");
            }
            let typed = entry.runtime.start.take().expect("an agent is started once");
            assert!(snapshot.is_none(), "typed assignments contain conversation turns");
            out.push(Request::StartTyped {
                owner,
                workspace,
                charter,
                activation: entry.attempt.raw(),
                turns: typed.turns,
                answered: typed.answered,
                grants: grants.into_boxed(),
            });
            entry.refreshed = false;
            State::Starting { reply_to, workspace, held }
        }
        State::Cancelling { reply_to, reason } => {
            let ending = Ending::Stopped { failure: Failure::Cancelled(reason), detail: Box::new([]) };
            release(entry, facts, reply_to, workspace, ending, None, out)
        }
        State::Starting { .. }
        | State::Unwanted { .. }
        | State::Active { .. }
        | State::Waiting { .. }
        | State::Stopping { .. }
        | State::Saving { .. }
        | State::Closed => unreachable!("a workspace is prepared once, while its run prepares"),
    };
    conclude(domain, id, out);
}

pub(crate) fn unprepared(
    domain: &mut Domain,
    env: &Env<Limits>,
    owner: Token,
    failure: Preparation,
    detail: Box<[u8]>,
    out: &mut Queue<Request>,
) {
    let Domain { hosted, facts, .. } = domain;
    let id = Id::<Hosted>::from_token(owner);
    let entry = hosted.get_mut(id).expect("a run lives until its prepare has settled");
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        // Nothing of the workspace is held: nothing to release.
        State::Preparing { reply_to, held, .. } => {
            bounce_held(held, entry.run, entry.attempt, out);
            let detail = tail(detail, &env.limits);
            let ending = Ending::Failed { failure: Failure::Unprepared(failure), detail };
            answer(entry, facts, reply_to, ending, None, out)
        }
        // Cancelled first: the prepare's failure is not the answer's.
        State::Cancelling { reply_to, reason } => {
            let ending = Ending::Stopped { failure: Failure::Cancelled(reason), detail: Box::new([]) };
            answer(entry, facts, reply_to, ending, None, out)
        }
        State::Starting { .. }
        | State::Unwanted { .. }
        | State::Active { .. }
        | State::Waiting { .. }
        | State::Stopping { .. }
        | State::Saving { .. }
        | State::Closed => unreachable!("a workspace is prepared once, while its run prepares"),
    };
    drain_stopping(domain, env, id, out);
    conclude(domain, id, out);
}

pub(crate) fn started(domain: &mut Domain, env: &Env<Limits>, owner: Token, agent: Token, out: &mut Queue<Request>) {
    let Domain { hosted, facts, .. } = domain;
    let id = Id::<Hosted>::from_token(owner);
    let entry = hosted.get_mut(id).expect("a run lives until its agent has gone");
    facts.push(Fact::Started { run: entry.run, attempt: entry.attempt });
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Starting { reply_to, workspace, mut held } => {
            if entry.refreshed {
                for grant in &entry.grants {
                    out.push(Request::Grant { agent, grant: *grant });
                }
                entry.refreshed = false;
            }
            // Bounded: the hold has room for the limit, and no more.
            for _ in 0..env.limits.held {
                let Some(event) = held.pop() else {
                    break;
                };
                deliver_message(agent, event, out);
            }
            State::Active { reply_to, workspace, agent }
        }
        State::Unwanted { reply_to, workspace, reason } => {
            out.push(Request::Stop { agent });
            let ending = Ending::Stopped { failure: Failure::Cancelled(reason), detail: Box::new([]) };
            State::Stopping { reply_to, workspace, agent, ending, gone: false }
        }
        State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Active { .. }
        | State::Waiting { .. }
        | State::Stopping { .. }
        | State::Saving { .. }
        | State::Closed => unreachable!("an agent starts once, after its start"),
    };
    drain_stopping(domain, env, id, out);
    conclude(domain, id, out);
}

pub(crate) fn called_typed(
    domain: &mut Domain,
    env: &Env<Limits>,
    owner: Token,
    call: Box<[u8]>,
    ask: Ask,
    out: &mut Queue<Request>,
) {
    called_named(domain, env, owner, call, ask, out);
}

fn called_named(
    domain: &mut Domain,
    env: &Env<Limits>,
    owner: Token,
    call: CallName,
    ask: Ask,
    out: &mut Queue<Request>,
) {
    let Domain { hosted, calls, .. } = domain;
    let id = Id::<Hosted>::from_token(owner);
    let entry = hosted.get_mut(id).expect("a run lives until its agent has gone");
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        // A run that calls is at work, whatever it said before.
        State::Active { reply_to, workspace, agent } | State::Waiting { reply_to, workspace, agent } => {
            serve(entry, calls, id, workspace, agent, call, ask, &env.limits, out);
            State::Active { reply_to, workspace, agent }
        }
        // Once a run is cancelled or ending, its calls are not served.
        State::Stopping { reply_to, workspace, agent, ending, gone } => {
            reply(agent, call, Reply::Unavailable, out);
            State::Stopping { reply_to, workspace, agent, ending, gone }
        }
        State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Starting { .. }
        | State::Unwanted { .. }
        | State::Saving { .. }
        | State::Closed => unreachable!("only an agent that has started and not gone calls"),
    };
    drain_stopping(domain, env, id, out);
    conclude(domain, id, out);
}

pub(crate) fn withdrawn_typed(domain: &mut Domain, owner: Token, call: Box<[u8]>, out: &mut Queue<Request>) {
    withdrawn_named(domain, owner, call, out);
}

fn withdrawn_named(domain: &mut Domain, owner: Token, call: CallName, out: &mut Queue<Request>) {
    let Domain { hosted, calls, .. } = domain;
    let id = Id::<Hosted>::from_token(owner);
    let entry = hosted.get_mut(id).expect("a run lives until its agent has gone");
    match &entry.state {
        State::Active { .. } | State::Waiting { .. } => withdraw(entry, calls, call, out),
        // Its relays were answered as it left live, and its delivery goes on.
        State::Stopping { .. } => {}
        State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Starting { .. }
        | State::Unwanted { .. }
        | State::Saving { .. }
        | State::Closed => unreachable!("only an agent that has started and not gone withdraws a call"),
    }
    conclude(domain, id, out);
}

pub(crate) fn bounced(domain: &mut Domain, owner: Token, name: Token, bounce: Bounce, out: &mut Queue<Request>) {
    let id = Id::<Hosted>::from_token(owner);
    let entry = domain.hosted.get(id).expect("a run lives until its agent has gone");
    match &entry.state {
        State::Active { .. } | State::Waiting { .. } | State::Stopping { .. } => {
            out.push(Request::Bounced { run: entry.run, attempt: entry.attempt, name, bounce });
        }
        State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Starting { .. }
        | State::Unwanted { .. }
        | State::Saving { .. }
        | State::Closed => unreachable!("only an agent that has started and not gone bounces an event"),
    }
    conclude(domain, id, out);
}

pub(crate) fn yielded(domain: &mut Domain, owner: Token, out: &mut Queue<Request>) {
    let id = Id::<Hosted>::from_token(owner);
    let entry = domain.hosted.get_mut(id).expect("a run lives until its agent has gone");
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Active { reply_to, workspace, agent } | State::Waiting { reply_to, workspace, agent } => {
            State::Waiting { reply_to, workspace, agent }
        }
        state @ State::Stopping { .. } => state,
        State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Starting { .. }
        | State::Unwanted { .. }
        | State::Saving { .. }
        | State::Closed => unreachable!("only an agent that has started and not gone yields"),
    };
    conclude(domain, id, out);
}

pub(crate) fn finished(domain: &mut Domain, env: &Env<Limits>, owner: Token, finish: Finish, out: &mut Queue<Request>) {
    let Domain { hosted, calls, .. } = domain;
    let id = Id::<Hosted>::from_token(owner);
    let entry = hosted.get_mut(id).expect("a run lives until its agent has gone");
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Active { reply_to, workspace, agent } | State::Waiting { reply_to, workspace, agent } => {
            let ending = said(finish, Failure::Run(RunFailure::Cancelled), &env.limits);
            leave(&entry.relays, calls, out);
            out.push(Request::Stop { agent });
            State::Stopping { reply_to, workspace, agent, ending, gone: false }
        }
        State::Stopping { reply_to, workspace, agent, ending, gone } => {
            let ending = match ending {
                // Stopped by the host, the run says how it finishes as it
                // winds down: its own ending wins.
                Ending::Stopped { failure, detail: _ } => said(finish, failure, &env.limits),
                // It said how it finishes already, or its agent exited.
                ending @ (Ending::Ended { .. } | Ending::Parked { .. } | Ending::Failed { .. }) => ending,
            };
            State::Stopping { reply_to, workspace, agent, ending, gone }
        }
        State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Starting { .. }
        | State::Unwanted { .. }
        | State::Saving { .. }
        | State::Closed => unreachable!("only an agent that has started and not gone finishes"),
    };
    drain_stopping(domain, env, id, out);
    conclude(domain, id, out);
}

pub(crate) fn faulted(
    domain: &mut Domain,
    env: &Env<Limits>,
    owner: Token,
    fault: AgentFailure,
    out: &mut Queue<Request>,
) {
    let Domain { hosted, calls, .. } = domain;
    let id = Id::<Hosted>::from_token(owner);
    let entry = hosted.get_mut(id).expect("a run lives until its agent has gone");
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Active { reply_to, workspace, agent } | State::Waiting { reply_to, workspace, agent } => {
            let ending = Ending::Stopped { failure: Failure::Agent(fault), detail: Box::new([]) };
            leave(&entry.relays, calls, out);
            out.push(Request::Stop { agent });
            State::Stopping { reply_to, workspace, agent, ending, gone: false }
        }
        // How the run ends is decided already, or the host stopped it first.
        state @ State::Stopping { .. } => state,
        State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Starting { .. }
        | State::Unwanted { .. }
        | State::Saving { .. }
        | State::Closed => unreachable!("only an agent that has started and not gone is faulted"),
    };
    drain_stopping(domain, env, id, out);
    conclude(domain, id, out);
}

pub(crate) fn gone(domain: &mut Domain, env: &Env<Limits>, owner: Token, detail: Box<[u8]>, out: &mut Queue<Request>) {
    let Domain { hosted, calls, facts, .. } = domain;
    let id = Id::<Hosted>::from_token(owner);
    let entry = hosted.get_mut(id).expect("a run lives until its agent has gone");
    let detail = tail(detail, &env.limits);
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        // Nothing ran: nothing to save.
        State::Starting { reply_to, workspace, held } => {
            bounce_held(held, entry.run, entry.attempt, out);
            let ending = Ending::Failed { failure: Failure::Agent(AgentFailure::Unstarted), detail };
            release(entry, facts, reply_to, workspace, ending, None, out)
        }
        State::Unwanted { reply_to, workspace, reason } => {
            let ending = Ending::Stopped { failure: Failure::Cancelled(reason), detail };
            release(entry, facts, reply_to, workspace, ending, None, out)
        }
        // It exited without saying how its run finishes.
        State::Active { reply_to, workspace, agent } | State::Waiting { reply_to, workspace, agent } => {
            let ending = Ending::Failed { failure: Failure::Agent(AgentFailure::Exited), detail };
            leave(&entry.relays, calls, out);
            State::Stopping { reply_to, workspace, agent, ending, gone: true }
        }
        State::Stopping { reply_to, workspace, agent, ending, gone } => {
            assert!(!gone, "an agent goes once");
            let ending = explained(ending, detail);
            State::Stopping { reply_to, workspace, agent, ending, gone: true }
        }
        State::Preparing { .. } | State::Cancelling { .. } | State::Saving { .. } | State::Closed => {
            unreachable!("only an agent that was started goes")
        }
    };
    drain_stopping(domain, env, id, out);
    conclude(domain, id, out);
}

pub(crate) fn delivered(domain: &mut Domain, owner: Token, delivery: Delivery, out: &mut Queue<Request>) {
    let Domain { hosted, calls, .. } = domain;
    let call_id = Id::<Call>::from_token(owner);
    let call = calls.get_mut(call_id).expect("a delivery call lives until it settles");
    let id = call.hosted;
    let state = mem::replace(&mut call.state, call::State::Closed);
    let typed = call.typed.take();
    calls.retire(call_id);
    let entry = hosted.get_mut(id).expect("a run lives until its delivery settles");
    assert!(entry.delivery == Some(call_id), "one delivery is in flight");
    entry.delivery = None;
    if delivery.changed {
        entry.left = Some(delivery.left);
    }
    match state {
        call::State::Delivering { agent } => {
            let name = typed.expect("a delivery retains its typed name");
            reply(agent, name, Reply::Delivered(delivery), out);
        }
        call::State::Relayed { .. } | call::State::Settling | call::State::Closed => {
            unreachable!("only a delivery ends a delivery call")
        }
    }
    conclude(domain, id, out);
}

pub(crate) fn saved(domain: &mut Domain, owner: Token, at: Option<Token>, out: &mut Queue<Request>) {
    let Domain { hosted, facts, .. } = domain;
    let id = Id::<Hosted>::from_token(owner);
    let entry = hosted.get_mut(id).expect("a run lives until its save settles");
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        State::Saving { reply_to, workspace, ending } => {
            release(entry, facts, reply_to, Some(workspace), ending, at, out)
        }
        State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Starting { .. }
        | State::Unwanted { .. }
        | State::Active { .. }
        | State::Waiting { .. }
        | State::Stopping { .. }
        | State::Closed => unreachable!("a save ends once, while its run saves"),
    };
    conclude(domain, id, out);
}

/// The engine's names for the hosted run `owner`, and where it is, unless it
/// has closed.
pub(crate) fn hosting(domain: &Domain, owner: Token) -> Option<Hosting> {
    let entry = domain.hosted.get(Id::<Hosted>::from_token(owner))?;
    match &entry.state {
        State::Closed => None,
        state @ (State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Starting { .. }
        | State::Unwanted { .. }
        | State::Active { .. }
        | State::Waiting { .. }
        | State::Stopping { .. }
        | State::Saving { .. }) => Some(Hosting { run: entry.run, attempt: entry.attempt, phase: phase(state) }),
    }
}

/// The hosted run the engine names `run`, if it hosts that attempt at it.
fn fenced(names: &Map<Token, Id<Hosted>>, hosted: &Slab<Hosted>, run: Token, attempt: Token) -> Option<Id<Hosted>> {
    let id = *names.get(&run)?;
    let entry = hosted.get(id).expect("a named run is hosted");
    (entry.attempt == attempt).then_some(id)
}

/// Cancels the run `id` for `reason`: the cancel cells.
fn stop(domain: &mut Domain, env: &Env<Limits>, id: Id<Hosted>, reason: Reason, out: &mut Queue<Request>) {
    let Domain { hosted, calls, .. } = domain;
    let entry = hosted.get_mut(id).expect("a run on the names or the ready list is hosted");
    let state = mem::replace(&mut entry.state, State::Closed);
    entry.state = match state {
        // A prepare in flight is abandoned, and its end waited for.
        State::Preparing { reply_to, held, .. } => {
            bounce_held(held, entry.run, entry.attempt, out);
            out.push(Request::Abort { owner: id.token() });
            State::Cancelling { reply_to, reason }
        }
        State::Starting { reply_to, workspace, held } => {
            bounce_held(held, entry.run, entry.attempt, out);
            State::Unwanted { reply_to, workspace, reason }
        }
        State::Active { reply_to, workspace, agent } | State::Waiting { reply_to, workspace, agent } => {
            let ending = Ending::Stopped { failure: Failure::Cancelled(reason), detail: Box::new([]) };
            leave(&entry.relays, calls, out);
            out.push(Request::Stop { agent });
            State::Stopping { reply_to, workspace, agent, ending, gone: false }
        }
        // How the run ends is decided already, or it is stopping.
        state @ (State::Cancelling { .. } | State::Unwanted { .. } | State::Stopping { .. } | State::Saving { .. }) => {
            state
        }
        State::Closed => unreachable!("a closed run has left the names and the ready list"),
    };
    drain_stopping(domain, env, id, out);
    conclude(domain, id, out);
}

fn drain_stopping(domain: &mut Domain, env: &Env<Limits>, id: Id<Hosted>, out: &mut Queue<Request>) {
    let entry = domain.hosted.get(id).expect("hosted run lives through its transition");
    let stopping = match entry.state {
        State::Stopping { .. } => true,
        State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Starting { .. }
        | State::Unwanted { .. }
        | State::Active { .. }
        | State::Waiting { .. }
        | State::Saving { .. }
        | State::Closed => false,
    };
    if stopping {
        domain.turns.admit(entry.run, entry.attempt, true, &env.limits, out);
    }
}

/// What a run's state implies, applied after every transition: a stopping run
/// that has settled saves, or releases its workspace and answers; and a
/// Closed run is retired, leaving the names and the ready list.
fn conclude(domain: &mut Domain, id: Id<Hosted>, out: &mut Queue<Request>) {
    let Domain { hosted, names, ready, facts, .. } = domain;
    let entry = hosted.get_mut(id).expect("a run lives until it is retired");
    let settled = match &entry.state {
        State::Stopping { gone, .. } => *gone && entry.delivery.is_none() && entry.relays.is_empty(),
        State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Starting { .. }
        | State::Unwanted { .. }
        | State::Active { .. }
        | State::Waiting { .. }
        | State::Saving { .. }
        | State::Closed => false,
    };
    if settled {
        let state = mem::replace(&mut entry.state, State::Closed);
        entry.state = match state {
            State::Stopping { reply_to, workspace, agent: _, ending, gone: _ } => {
                settle(entry, facts, id, reply_to, workspace, ending, out)
            }
            State::Preparing { .. }
            | State::Cancelling { .. }
            | State::Starting { .. }
            | State::Unwanted { .. }
            | State::Active { .. }
            | State::Waiting { .. }
            | State::Saving { .. }
            | State::Closed => unreachable!("only a stopping run settles"),
        };
    }
    let closed = match &entry.state {
        State::Closed => true,
        State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Starting { .. }
        | State::Unwanted { .. }
        | State::Active { .. }
        | State::Waiting { .. }
        | State::Stopping { .. }
        | State::Saving { .. } => false,
    };
    if closed {
        let run = entry.run;
        names.remove(&run);
        ready.remove(&id);
        hosted.retire(id);
    }
}

// Cell handlers and what they share.

/// Stopping, settled: nothing of the run is running and nothing it asked of
/// the workspace is in flight. Its work is saved if the assignment asks,
/// unless it ended with a landed change, which leaves nothing to save; it
/// releases its workspace and answers otherwise. A save of an unchanged tree
/// pushes nothing.
fn settle(
    entry: &mut Hosted,
    facts: &mut Facts,
    id: Id<Hosted>,
    reply_to: ReplyTo,
    workspace: Option<Token>,
    ending: Ending,
    out: &mut Queue<Request>,
) -> State {
    let delivered_end = match &ending {
        Ending::Ended { .. } => entry.left.is_some(),
        Ending::Parked { .. } | Ending::Failed { .. } | Ending::Stopped { .. } => false,
    };
    match workspace {
        Some(workspace) if entry.save && !delivered_end => {
            out.push(Request::Save { owner: id.token(), workspace });
            State::Saving { reply_to, workspace, ending }
        }
        Some(_) | None => release(entry, facts, reply_to, workspace, ending, None, out),
    }
}

/// Releases the run's workspace and answers.
fn release(
    entry: &Hosted,
    facts: &mut Facts,
    reply_to: ReplyTo,
    workspace: Option<Token>,
    ending: Ending,
    saved: Option<Token>,
    out: &mut Queue<Request>,
) -> State {
    if let Some(workspace) = workspace {
        out.push(Request::Release { workspace });
    }
    answer(entry, facts, reply_to, ending, saved, out)
}

/// Answers the engine: the run's one answer.
fn answer(
    entry: &Hosted,
    facts: &mut Facts,
    reply_to: ReplyTo,
    ending: Ending,
    saved: Option<Token>,
    out: &mut Queue<Request>,
) -> State {
    let (run, attempt) = (entry.run, entry.attempt);
    let work = Work { left: entry.left, saved };
    let answer = match ending {
        Ending::Ended { outcome } => {
            facts.push(Fact::Ended { run, attempt });
            Answer::Ended { outcome, work }
        }
        Ending::Parked { snapshot } => {
            facts.push(Fact::Parked { run, attempt });
            Answer::Parked { snapshot, work }
        }
        Ending::Failed { failure, detail } | Ending::Stopped { failure, detail } => {
            facts.push(Fact::Failed { run, attempt, failure });
            Answer::Failed { failure, detail, work }
        }
    };
    out.push(Request::AnswerV2 {
        to: reply_to,
        run,
        attempt,
        answer: next_answer(entry.runtime.turns, entry.runtime.spent, answer),
    });
    State::Closed
}

/// Holds `event` until the run is live, or bounces it if the hold is full.
fn hold(held: &mut Queue<NamedEvent>, event: NamedEvent, run: Token, attempt: Token, out: &mut Queue<Request>) {
    let name = event.name;
    if held.try_push(event).is_err() {
        out.push(Request::Bounced { run, attempt, name, bounce: Bounce::Full });
    }
}

fn deliver_message(agent: Token, message: NamedEvent, out: &mut Queue<Request>) {
    match message.sender {
        Some(sender) => out.push(Request::DeliverTyped { agent, name: message.name, sender, words: message.event }),
        None => unreachable!("every held message retains its sender label"),
    }
}

/// Return messages accepted during preparation when the run cannot start.
fn bounce_held(mut held: Queue<NamedEvent>, run: Token, attempt: Token, out: &mut Queue<Request>) {
    for _ in 0..held.len() {
        let event = held.pop().expect("the queue held this many messages");
        out.push(Request::Bounced { run, attempt, name: event.name, bounce: Bounce::Ending });
    }
}

/// Serves the host call `call` of the live run `id`, through its workspace
/// or the engine, or answers it as busy.
#[expect(clippy::too_many_arguments, reason = "a cell handler takes the fields it touches")]
fn serve(
    entry: &mut Hosted,
    calls: &mut Slab<Call>,
    id: Id<Hosted>,
    workspace: Option<Token>,
    agent: Token,
    call: CallName,
    ask: Ask,
    limits: &Limits,
    out: &mut Queue<Request>,
) {
    let name_fits = len(&call) <= limits.event_bytes;
    let input_fits = match &ask {
        Ask::RelayTyped { tool, input, .. } => match len(tool).checked_add(len(input)) {
            Some(bytes) => bytes <= limits.event_bytes,
            None => false,
        },
        Ask::DeliverV2 { .. } => true,
    };
    if !name_fits || !input_fits {
        reply(agent, call, Reply::Unavailable, out);
        return;
    }
    let delivery = match &ask {
        Ask::DeliverV2 { .. } => true,
        Ask::RelayTyped { .. } => false,
    };
    if delivery && workspace.is_none() {
        reply(agent, call, Reply::Unavailable, out);
        return;
    }
    for pending in &entry.relays {
        let pending = calls.get(*pending).expect("the run owns every relay");
        if pending.typed.as_ref() == Some(&call) {
            reply(agent, call, Reply::Busy, out);
            return;
        }
    }
    let in_flight = entry.relays.len().saturating_add(u32::from(entry.delivery.is_some()));
    // A delivery changes a workspace: one at a time.
    if in_flight >= limits.run_calls || (delivery && entry.delivery.is_some()) {
        reply(agent, call, Reply::Busy, out);
        return;
    }
    let typed = Some(call.clone());
    let state = if delivery { call::State::Delivering { agent } } else { call::State::Relayed { agent } };
    // Calls answered in this iteration keep their slots until the reclaim
    // point: a call may find none free even within its run's limit.
    let Ok(call_id) = calls.insert(Call { hosted: id, state, typed }) else {
        reply(agent, call, Reply::Busy, out);
        return;
    };
    match ask {
        Ask::DeliverV2 { title, body } => {
            entry.delivery = Some(call_id);
            out.push(Request::DeliverV2 {
                owner: call_id.token(),
                workspace: workspace.expect("a delivery has a workspace"),
                title,
                body,
            });
        }
        Ask::RelayTyped { tool, writes, input, deadline } => {
            let added = entry.relays.insert(call_id).expect("checked the run's calls for room above");
            assert!(added, "a call is new to its run");

            out.push(Request::RelayTyped {
                run: entry.run,
                attempt: entry.attempt,
                call,
                delivery: call_id.token(),
                tool,
                writes,
                input,
                deadline,
            });
        }
    }
}

/// The live run withdrew the call its agent names `call`: a relay in flight is
/// answered as withdrawn, and closes. A delivery goes on, and is answered once it
/// settles; a call answered already is not found, and nothing happens.
fn withdraw(entry: &mut Hosted, calls: &mut Slab<Call>, call: CallName, out: &mut Queue<Request>) {
    let mut found = None;
    for call_id in &entry.relays {
        let relayed = calls.get(*call_id).expect("a run's calls live until they close");
        let same = relayed.typed.as_ref() == Some(&call);
        if same {
            found = Some(*call_id);
        }
    }
    let Some(call_id) = found else {
        return;
    };
    let relayed = calls.get_mut(call_id).expect("found above");
    match relayed.state {
        call::State::Relayed { agent } => {
            let name = take_name(relayed);
            reply(agent, name, Reply::Withdrawn, out);
            relayed.state = call::State::Settling;
            out.push(Request::CancelRelay { call: call_id.token() });
        }
        call::State::Settling | call::State::Delivering { .. } | call::State::Closed => {
            unreachable!("found among the active relays")
        }
    }
}

/// Leaving live answers each relay's agent at once, then waits for the
/// cancellation of local delivery. Each binding ends with a terminal event.
fn leave(relays: &Set<Id<Call>>, calls: &mut Slab<Call>, out: &mut Queue<Request>) {
    for call_id in relays {
        let entry = calls.get_mut(*call_id).expect("a run's relays live until their terminals");
        match entry.state {
            call::State::Relayed { agent } => {
                let name = take_name(entry);
                reply(agent, name, Reply::Unavailable, out);
                entry.state = call::State::Settling;
                out.push(Request::CancelRelay { call: call_id.token() });
            }
            call::State::Settling => {}
            call::State::Delivering { .. } | call::State::Closed => {
                unreachable!("the run holds only its pending relays")
            }
        }
    }
}

/// How the run ends, as it says it finishes; a cancel it reports is
/// `cancelled`. Saying more than the limits allow breaks the rules.
fn said(finish: Finish, cancelled: Failure, limits: &Limits) -> Ending {
    let rules = Ending::Failed { failure: Failure::Agent(AgentFailure::Rules), detail: Box::new([]) };
    match finish {
        Finish::Ended { outcome } if len(&outcome) > limits.outcome_bytes => rules,
        Finish::Ended { outcome } => Ending::Ended { outcome },
        Finish::Parked { snapshot: Some(snapshot) } if len(&snapshot) > limits.snapshot_bytes => rules,
        Finish::Parked { snapshot } => Ending::Parked { snapshot },
        Finish::Failed { failure: RunFailure::Cancelled } => {
            Ending::Failed { failure: cancelled, detail: Box::new([]) }
        }
        Finish::Failed { failure } => Ending::Failed { failure: Failure::Run(failure), detail: Box::new([]) },
    }
}

/// A failure's ending gains the detail its agent left as it went.
fn explained(ending: Ending, detail: Box<[u8]>) -> Ending {
    match ending {
        Ending::Failed { failure, detail: _ } => Ending::Failed { failure, detail },
        Ending::Stopped { failure, detail: _ } => Ending::Stopped { failure, detail },
        ending @ (Ending::Ended { .. } | Ending::Parked { .. }) => ending,
    }
}

/// The last bytes of `detail`, as many as the limits keep.
fn tail(detail: Box<[u8]>, limits: &Limits) -> Box<[u8]> {
    let keep = usize::try_from(limits.detail_bytes).expect("a u32 fits in a usize");
    match detail.len().checked_sub(keep) {
        Some(cut) if cut > 0 => copy_of(detail.get(cut..).expect("cut within the detail")),
        Some(_) | None => detail,
    }
}

fn phase(state: &State) -> Phase {
    match state {
        State::Preparing { .. } => Phase::Preparing,
        State::Starting { .. } => Phase::Starting,
        State::Active { .. } => Phase::Active,
        State::Waiting { .. } => Phase::Waiting,
        State::Cancelling { .. } | State::Unwanted { .. } | State::Stopping { .. } | State::Saving { .. } => {
            Phase::Ending
        }
        State::Closed => unreachable!("a closed run has left the names"),
    }
}

/// A refresh is fenced to the attempt and to one of its admitted accounts.
pub(crate) fn grant(domain: &mut Domain, run: Token, attempt: Token, grant: Grant, out: &mut Queue<Request>) {
    let Some(id) = fenced(&domain.names, &domain.hosted, run, attempt) else {
        return;
    };
    let entry = domain.hosted.get_mut(id).expect("a named run is hosted");
    let mut accepted = false;
    for current in &mut entry.grants {
        if current.account == grant.account && current.generation <= grant.generation {
            *current = grant;
            accepted = true;
        }
    }
    if !accepted {
        return;
    }
    entry.refreshed = true;
    match entry.state {
        State::Active { agent, .. } | State::Waiting { agent, .. } => out.push(Request::Grant { agent, grant }),
        State::Preparing { .. }
        | State::Starting { .. }
        | State::Cancelling { .. }
        | State::Unwanted { .. }
        | State::Stopping { .. }
        | State::Saving { .. }
        | State::Closed => {}
    }
}

fn next_answer(turns: u32, spent: u64, answer: Answer) -> crate::AnswerV2 {
    let ending = match answer {
        Answer::Refused(refusal) => crate::EndingV2::Refused(refusal),
        Answer::Ended { outcome, work } => crate::EndingV2::Ended { outcome, work },
        Answer::Parked { snapshot: _, work } => crate::EndingV2::Parked { work },
        Answer::Failed { failure, detail, work } => crate::EndingV2::Failed { failure, detail, work },
    };
    crate::AnswerV2 { turns, spent, ending }
}

fn refuse_v2(reply_to: ReplyTo, assignment: &Assignment, refusal: Refusal, out: &mut Queue<Request>) {
    out.push(Request::AnswerV2 {
        to: reply_to,
        run: assignment.run,
        attempt: assignment.attempt,
        answer: next_answer(0, 0, Answer::Refused(refusal)),
    });
}

fn refused(reply_to: ReplyTo, assignment: &Assignment, refusal: Refusal, _runtime: &Runtime, out: &mut Queue<Request>) {
    refuse_v2(reply_to, assignment, refusal, out);
}

pub(crate) fn turned(
    domain: &mut Domain,
    env: &Env<Limits>,
    owner: Token,
    turn: crate::Turn,
    out: &mut Queue<Request>,
) {
    let id = Id::<Hosted>::from_token(owner);
    let Some(entry) = domain.hosted.get_mut(id) else {
        return;
    };
    let agent = match entry.state {
        State::Active { agent, .. } | State::Waiting { agent, .. } | State::Stopping { agent, gone: false, .. } => {
            agent
        }
        State::Preparing { .. }
        | State::Cancelling { .. }
        | State::Starting { .. }
        | State::Unwanted { .. }
        | State::Stopping { gone: true, .. }
        | State::Saving { .. }
        | State::Closed => return,
    };
    let valid = if entry.runtime.turns.checked_add(1) != Some(turn.turn) || len(&turn.body) > env.limits.turn_bytes {
        false
    } else {
        entry.runtime.turns = turn.turn;
        true
    };
    if valid {
        let (run, attempt) = (entry.run, entry.attempt);
        domain.turns.retain(agent, run, attempt, turn);
        let stopping = match entry.state {
            State::Stopping { .. } => true,
            State::Preparing { .. }
            | State::Cancelling { .. }
            | State::Starting { .. }
            | State::Unwanted { .. }
            | State::Active { .. }
            | State::Waiting { .. }
            | State::Saving { .. }
            | State::Closed => false,
        };
        domain.turns.admit(run, attempt, stopping, &env.limits, out);
    } else {
        faulted(domain, env, owner, AgentFailure::Rules, out);
    }
}

/// Release a committed turn and resume the agent reader when room returns.
pub(crate) fn acknowledge_turn(
    domain: &mut Domain,
    env: &Env<Limits>,
    run: Token,
    attempt: Token,
    turn: u32,
    out: &mut Queue<Request>,
) {
    domain.turns.acknowledge(run, attempt, turn);
    domain.turns.admit(run, attempt, false, &env.limits, out);
}

/// Keep a live agent fact for the engine, dropping it when bounded room is full.
pub(crate) fn told(domain: &mut Domain, env: &Env<Limits>, owner: Token, fact: Box<[u8]>) {
    let Some(hosting) = domain.hosting(owner) else {
        return;
    };
    domain.told.push(Told { run: hosting.run, attempt: hosting.attempt, fact }, env.limits.fact_bytes);
}

pub(crate) fn finished_v2(
    domain: &mut Domain,
    env: &Env<Limits>,
    owner: Token,
    turns: u32,
    spent: u64,
    finish: crate::FinishV2,
    out: &mut Queue<Request>,
) {
    let Some(entry) = domain.hosted.get_mut(Id::<Hosted>::from_token(owner)) else {
        return;
    };
    let valid = turns == entry.runtime.turns;
    if valid {
        entry.runtime.spent = spent;
    }
    if !valid {
        faulted(domain, env, owner, AgentFailure::Rules, out);
        return;
    }
    let finish = match finish {
        crate::FinishV2::Ended { outcome } => Finish::Ended { outcome },
        crate::FinishV2::Parked => Finish::Parked { snapshot: None },
        crate::FinishV2::Failed { failure } => Finish::Failed { failure },
    };
    finished(domain, env, owner, finish, out);
}
