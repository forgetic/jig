//! Effect decisions and answer lifetimes belong to the core. Descriptions,
//! facts and outbox payloads belong to their numbered connectors
//! (`domain/engine.md`, 4.3 and 7.3; `domain/connectors.md`, 4.4–4.5).

use crate::{
    Ask, CallKey, CallPart, CallRecord, Core, CoreRecord, Event, Family, Held, Limits, Now, Record, Request, Requests,
    ToolKind, Write, connector, fresh,
};
use alloc::boxed::Box;
use jig_core_authority as authority;
use jig_core_tasks as tasks;
use skein_lib::{Env, List, Queue, ReplyTo, Token, Wall};

/// The purpose fixes the key independently of retry or outbox numbering.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EffectPurpose {
    Call { attempt: u64, completion: u32, position: u32 },
    Procedure { purpose: u64 },
}

/// Full deployment identity and the effect's stable purpose.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EffectKey {
    pub deployment: [u8; 16],
    pub task: u64,
    pub origin: EffectPurpose,
    pub purpose: u64,
}

/// Who requested the connector-owned effect and when a call must answer.
#[derive(Debug)]
pub enum EffectOrigin {
    Call {
        to: ReplyTo,
        key: CallKey,
        deadline: Wall,
    },
    /// An explicit proposal keeps its connector payload without making it.
    Propose {
        to: ReplyTo,
        key: CallKey,
        reason: Box<[u8]>,
        as_holder: bool,
    },
    /// A holder decision already authenticated by the core's proposal route.
    Accept {
        to: ReplyTo,
        key: Option<CallKey>,
        proposal: Box<tasks::Proposal>,
        by: tasks::Party,
    },
    Procedure {
        task: u64,
        step: u64,
        entry: Option<u64>,
    },
}

impl EffectOrigin {
    fn task(&self) -> u64 {
        match self {
            Self::Call { key, .. } | Self::Propose { key, .. } => key.task,
            Self::Accept { proposal, .. } => proposal.proposer,
            Self::Procedure { task, .. } => *task,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Flight {
    connector: u16,
    origin: EffectOrigin,
    description: Option<Box<connector::EffectDescription>>,
    judges: List<authority::Judge>,
    given: List<authority::Given>,
}

#[derive(Debug)]
pub(crate) struct Waiting {
    to: ReplyTo,
    key: CallKey,
    deadline: Wall,
}

fn end(out: &mut Queue<Request>) {
    out.push(Request::Decided);
}

fn save(out: &mut Queue<Request>, key: CallKey, part: CallPart) {
    out.push(Request::Write(Write::Save(Record::Core(CoreRecord::Call(CallRecord { key, part })))));
}

fn answer(core: &mut Core, out: &mut Queue<Request>, to: ReplyTo, key: CallKey, part: CallPart) {
    if core.decide_named_call(key, part.clone()) {
        save(out, key, part.clone());
        out.push(Request::Held(Box::new(Held::CallAnswer { to, key, part })));
    }
}

fn drop_effect(out: &mut Queue<Request>, number: u16, owner: Token, answer: authority::Answer) {
    out.push(Request::Ask { connector: number, ask: Ask::Effect(connector::Ask::Drop { owner, answer }) });
}

pub(crate) fn start(core: &mut Core, env: &Env<Limits>, owner: Token, number: u16, origin: EffectOrigin) -> Requests {
    let mut out = Queue::with_capacity(5);
    match origin {
        EffectOrigin::Propose { to, key, reason, as_holder } => {
            return propose_start(core, env, owner, number, to, key, reason, as_holder);
        }
        EffectOrigin::Accept { to, key, proposal, by } => {
            let pending = core.tasks.proposal(proposal.proposer, proposal.number);
            if pending.as_ref() != Some(&proposal) {
                end(&mut out);
                return Requests::Out(out);
            }
            let proposal_number = proposal.number;
            start_flight(core, env, owner, number, EffectOrigin::Accept { to, key, proposal, by });
            out.push(Request::Ask {
                connector: number,
                ask: Ask::Effect(connector::Ask::DescribeProposal { owner, proposal: proposal_number }),
            });
            end(&mut out);
            return Requests::Out(out);
        }
        EffectOrigin::Call { to, key, deadline } => {
            match core.call_parts.get(&key) {
                Some(CallPart::Effect {
                    entry,
                    deadline: due,
                    outcome: None | Some(connector::OutboxOutcome::Uncertain | connector::OutboxOutcome::Held { .. }),
                    ..
                }) if env.wall < *due => {
                    let entry = *entry;
                    let due = *due;
                    assert!(
                        core.effect_replies.insert(entry, Waiting { to, key, deadline: due }).is_ok(),
                        "effect reply room retained"
                    );
                    drop_effect(&mut out, number, owner, authority::Answer::Refuse);
                    end(&mut out);
                    return Requests::Out(out);
                }
                Some(part) => {
                    out.push(Request::Held(Box::new(Held::CallAnswer { to, key, part: part.clone() })));
                    drop_effect(&mut out, number, owner, authority::Answer::Refuse);
                    end(&mut out);
                    return Requests::Out(out);
                }
                None => {}
            }
            if core.pending_calls.contains_key(&key)
                || core.counters.deployment().calls == u64::MAX
                || core.call_parts.len().saturating_add(core.pending_calls.len()) >= env.limits.call_records
            {
                out.push(Request::Now(Box::new(Now::EffectAnswer { to, key, part: CallPart::Unavailable })));
                drop_effect(&mut out, number, owner, authority::Answer::Refuse);
                end(&mut out);
                return Requests::Out(out);
            }
            if core.current_proof(key.task, key.attempt) {
                if let Some(part) = core.authorize_tool(key, ToolKind::Effect) {
                    answer(core, &mut out, to, key, part);
                    drop_effect(&mut out, number, owner, authority::Answer::Refuse);
                    end(&mut out);
                    return Requests::Out(out);
                }
            } else {
                drop_effect(&mut out, number, owner, authority::Answer::Refuse);
                end(&mut out);
                return Requests::Out(out);
            }
            assert!(core.reserve_connector_call(key), "named effect room admitted");
            start_flight(core, env, owner, number, EffectOrigin::Call { to, key, deadline });
        }
        EffectOrigin::Procedure { task, step, entry } => {
            let valid = match core.tasks.task(task) {
                Some(row) => {
                    row.phase == tasks::Phase::Active(tasks::Active::Due)
                        && row.attempt.checked_add(1) == Some(step)
                        && match row.executor {
                            tasks::Executor::Procedure { connector, .. } => connector == number,
                            tasks::Executor::Agent { .. } | tasks::Executor::Person(_) => false,
                        }
                }
                None => false,
            };
            let numbered = match entry {
                Some(number) => number != 0 && number <= core.counters.deployment().connector_rows,
                None => true,
            };
            if !valid || !numbered {
                drop_effect(&mut out, number, owner, authority::Answer::Refuse);
                end(&mut out);
                return Requests::Out(out);
            }
            start_flight(core, env, owner, number, EffectOrigin::Procedure { task, step, entry });
        }
    }
    out.push(Request::Ask { connector: number, ask: Ask::Effect(connector::Ask::Describe { owner }) });
    end(&mut out);
    Requests::Out(out)
}

#[expect(clippy::too_many_arguments, reason = "one named proposal carries its connector payload and reason")]
fn propose_start(
    core: &mut Core,
    env: &Env<Limits>,
    owner: Token,
    number: u16,
    to: ReplyTo,
    key: CallKey,
    reason: Box<[u8]>,
    as_holder: bool,
) -> Requests {
    let mut out = Queue::with_capacity(5);
    if let Some(part) = core.call_parts.get(&key) {
        out.push(Request::Held(Box::new(Held::CallAnswer { to, key, part: part.clone() })));
        drop_effect(&mut out, number, owner, authority::Answer::Refuse);
        end(&mut out);
        return Requests::Out(out);
    }
    if !core.current_proof(key.task, key.attempt) {
        drop_effect(&mut out, number, owner, authority::Answer::Refuse);
        end(&mut out);
        return Requests::Out(out);
    }
    if core.call_parts.len().saturating_add(core.pending_calls.len()) >= env.limits.call_records
        || core.pending_calls.contains_key(&key)
        || core.counters.deployment().calls == u64::MAX
    {
        out.push(Request::Now(Box::new(Now::EffectAnswer { to, key, part: CallPart::Unavailable })));
        drop_effect(&mut out, number, owner, authority::Answer::Refuse);
        end(&mut out);
        return Requests::Out(out);
    }
    if let Some(part) = core.authorize_tool(key, ToolKind::Propose) {
        answer(core, &mut out, to, key, part);
        drop_effect(&mut out, number, owner, authority::Answer::Refuse);
        end(&mut out);
        return Requests::Out(out);
    }
    assert!(core.reserve_connector_call(key), "proposal call room");
    start_flight(core, env, owner, number, EffectOrigin::Propose { to, key, reason, as_holder });
    out.push(Request::Ask { connector: number, ask: Ask::Effect(connector::Ask::Describe { owner }) });
    end(&mut out);
    Requests::Out(out)
}

fn start_flight(core: &mut Core, env: &Env<Limits>, owner: Token, number: u16, origin: EffectOrigin) {
    assert!(
        core.effect_flights
            .insert(
                owner,
                Box::new(Flight {
                    connector: number,
                    origin,
                    description: None,
                    judges: List::with_capacity(env.limits.authority.facts),
                    given: List::with_capacity(env.limits.authority.facts),
                })
            )
            .is_ok(),
        "synchronous effect flight room"
    );
}

pub(crate) fn connector(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    event: connector::Event,
) -> Requests {
    let mut out = Queue::with_capacity(crate::room_max(&env.limits).expect("validated core room").held);
    match event {
        connector::Event::DescribeBusy { owner } => {
            if let Some(flight) = core.effect_flights.remove(&owner) {
                drop_effect(&mut out, flight.connector, owner, authority::Answer::Refuse);
                match flight.origin {
                    EffectOrigin::Call { to, key, .. } | EffectOrigin::Propose { to, key, .. } => {
                        core.pending_calls.remove(&key);
                        out.push(Request::Now(Box::new(Now::EffectAnswer { to, key, part: CallPart::Unavailable })));
                    }
                    EffectOrigin::Accept { to, key, .. } => {
                        accept_refused(core, work, &mut out, to, key, authority::Answer::Wait);
                    }
                    EffectOrigin::Procedure { task, step, .. } => procedure_wait(work, task, step),
                }
            }
        }
        connector::Event::DescribeRefused { owner } => {
            if let Some(flight) = core.effect_flights.remove(&owner) {
                drop_effect(&mut out, flight.connector, owner, authority::Answer::Refuse);
                match flight.origin {
                    EffectOrigin::Call { to, key, .. } | EffectOrigin::Propose { to, key, .. } => answer(
                        core,
                        &mut out,
                        to,
                        key,
                        CallPart::EffectDenied { answer: authority::Answer::Refuse, findings: Box::new([]) },
                    ),
                    EffectOrigin::Accept { to, key, .. } => {
                        accept_refused(core, work, &mut out, to, key, authority::Answer::Refuse);
                    }
                    EffectOrigin::Procedure { task, step, entry: _ } => procedure_wait(work, task, step),
                }
            }
        }
        connector::Event::Described { owner, description } => described(core, env, work, &mut out, owner, description),
        connector::Event::Verdict { owner, judge, verdict, at, guarded, state } => {
            match core.effect_flights.get_mut(&owner) {
                Some(flight) => {
                    let mut duplicate = false;
                    for given in &flight.given {
                        if given.judge == judge {
                            duplicate = true;
                        }
                    }
                    if flight.judges.as_slice().contains(&judge) && !duplicate {
                        let description = flight.description.as_mut().expect("judge follows description");
                        if guarded
                            && state == description.effect.state
                            && judge.connector == description.connector
                            && !description.effect.guards.contains(&judge)
                        {
                            let mut guards = List::with_capacity(env.limits.authority.facts);
                            for guard in &description.effect.guards {
                                guards.push(*guard).expect("admitted guards");
                            }
                            if guards.len() < env.limits.authority.facts {
                                guards.push(judge).expect("one guard per asked judge");
                            }
                            description.effect.guards = guards.into_boxed();
                        }
                        flight
                            .given
                            .push(authority::Given { judge, verdict, at, state })
                            .expect("one verdict per judge");
                    }
                    if flight.given.len() != flight.judges.len() {
                        end(&mut out);
                        return Requests::Out(out);
                    }
                }
                None => {
                    end(&mut out);
                    return Requests::Out(out);
                }
            }
            complete(core, env, work, &mut out, owner);
        }
        connector::Event::Outbox { entry, task, outcome } => settled(core, work, &mut out, entry, task, outcome),
        connector::Event::Resource { .. }
        | connector::Event::PoolSlots { .. }
        | connector::Event::Procedure { .. }
        | connector::Event::News { .. }
        | connector::Event::SectionReady { .. }
        | connector::Event::WorkspaceReady { .. }
        | connector::Event::Adopted { .. }
        | connector::Event::Drift { .. }
        | connector::Event::RestartDone { .. } => {}
    }
    end(&mut out);
    Requests::Out(out)
}

fn described(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    out: &mut Queue<Request>,
    owner: Token,
    description: Box<connector::EffectDescription>,
) {
    let proposed = match core.effect_flights.get(&owner) {
        Some(flight) => match flight.origin {
            EffectOrigin::Propose { .. } => true,
            EffectOrigin::Call { .. } | EffectOrigin::Procedure { .. } | EffectOrigin::Accept { .. } => false,
        },
        None => return,
    };
    if proposed {
        core.effect_flights.get_mut(&owner).expect("proposal owner").description = Some(description);
        complete(core, env, work, out, owner);
        return;
    }
    let project = match core.effect_flights.get(&owner) {
        Some(flight) => match core.tasks.task(flight.origin.task()) {
            Some(row) => row.project,
            None => 0,
        },
        None => return,
    };
    let judges = core.connector_needed_judges(project, &description.effect);
    let flight = core.effect_flights.get_mut(&owner).expect("description owner retained");
    flight.description = Some(description);
    match judges {
        Some(judges) if judges.len() <= env.limits.authority.facts => {
            for judge in &judges {
                flight.judges.push(*judge).expect("bounded judges");
            }
        }
        Some(_) | None => {
            complete(core, env, work, out, owner);
            return;
        }
    }
    if flight.judges.is_empty() {
        complete(core, env, work, out, owner);
        return;
    }
    let effect = &flight.description.as_ref().expect("described").effect;
    let mut resources = List::with_capacity(
        u32::try_from(effect.additional.len()).expect("bounded resources").checked_add(1).expect("effect resources"),
    );
    resources.push(effect.name.clone()).expect("first resource");
    for resource in &effect.additional {
        resources.push(resource.name.clone()).expect("additional resource");
    }
    for judge in &flight.judges {
        out.push(Request::Ask {
            connector: judge.connector,
            ask: Ask::Effect(connector::Ask::Judge {
                owner,
                judge: *judge,
                state: effect.state,
                resources: resources.as_slice().into(),
            }),
        });
    }
}

fn complete(core: &mut Core, env: &Env<Limits>, work: &mut Queue<Event>, out: &mut Queue<Request>, owner: Token) {
    let flight = core.effect_flights.remove(&owner).expect("completed effect flight");
    let description = flight.description.expect("effect described before checking");
    let origin = match flight.origin {
        EffectOrigin::Propose { to, key, reason, as_holder } => {
            complete_proposal(core, env, work, out, owner, flight.connector, to, key, *description, reason, as_holder);
            return;
        }
        origin @ (EffectOrigin::Call { .. } | EffectOrigin::Procedure { .. } | EffectOrigin::Accept { .. }) => origin,
    };
    let mut findings = Queue::with_capacity(authority::max_out(&env.limits.authority).expect("authority output bound"));
    let mut checked = check(core, env, &origin, &description, flight.given.as_slice(), &mut findings);
    if flight.connector != description.connector {
        checked = authority::Answer::Refuse;
    }
    if checked != authority::Answer::Allow {
        drop_effect(out, flight.connector, owner, checked);
        match origin {
            EffectOrigin::Call { to, key, .. } => {
                let mut reasons = List::with_capacity(findings.len());
                for _ in 0..findings.len() {
                    reasons.push(findings.pop().expect("finding count")).expect("finding room");
                }
                let part = CallPart::EffectDenied { answer: checked, findings: reasons.into_boxed() };
                if checked == authority::Answer::Wait {
                    let _pending = core.pending_calls.remove(&key);
                    out.push(Request::Now(Box::new(Now::EffectAnswer { to, key, part })));
                } else {
                    answer(core, out, to, key, part);
                }
            }
            EffectOrigin::Accept { to, key, .. } => accept_refused(core, work, out, to, key, checked),
            EffectOrigin::Propose { .. } => unreachable!("proposals completed separately"),
            EffectOrigin::Procedure { task, step, entry: _ } => procedure_wait(work, task, step),
        }
        return;
    }
    let acceptance_room = match &origin {
        EffectOrigin::Accept { proposal, .. } => {
            core.counters.deployment().messages != u64::MAX
                && match core.tasks.task(proposal.proposer) {
                    Some(row) => row.revision != u64::MAX,
                    None => false,
                }
        }
        EffectOrigin::Call { .. } | EffectOrigin::Propose { .. } | EffectOrigin::Procedure { .. } => true,
    };
    let needs_entry = match origin {
        EffectOrigin::Call { .. } | EffectOrigin::Accept { .. } | EffectOrigin::Procedure { entry: None, .. } => true,
        EffectOrigin::Propose { .. } => unreachable!("proposal has no entry"),
        EffectOrigin::Procedure { entry: Some(_), .. } => false,
    };
    if !acceptance_room || (needs_entry && core.counters.deployment().connector_rows == u64::MAX) {
        drop_effect(out, flight.connector, owner, authority::Answer::Refuse);
        match origin {
            EffectOrigin::Call { to, key, .. } => {
                core.pending_calls.remove(&key);
                out.push(Request::Now(Box::new(Now::EffectAnswer { to, key, part: CallPart::Unavailable })));
            }
            EffectOrigin::Accept { to, key, .. } => accept_refused(core, work, out, to, key, authority::Answer::Refuse),
            EffectOrigin::Propose { .. } => unreachable!("proposal has no entry"),
            EffectOrigin::Procedure { task, step, .. } => procedure_wait(work, task, step),
        }
        return;
    }
    charge(core, work, &origin, description.effect.price);
    let entry = match &origin {
        EffectOrigin::Procedure { entry: Some(entry), .. } => *entry,
        EffectOrigin::Propose { .. } => unreachable!("proposal has no entry"),
        EffectOrigin::Call { .. } | EffectOrigin::Accept { .. } | EffectOrigin::Procedure { entry: None, .. } => {
            fresh(&mut core.counters, Family::ConnectorRow).expect("effect entry counter admitted")
        }
    };
    let task = origin.task();
    let purpose = match origin {
        EffectOrigin::Call { to, key, deadline } => {
            let part = CallPart::Effect { connector: flight.connector, entry, deadline, outcome: None };
            assert!(core.decide_named_call(key, part.clone()), "effect fenced by current claim");
            save(out, key, part);
            assert!(
                core.effect_replies.insert(entry, Waiting { to, key, deadline }).is_ok(),
                "one effect reply per admitted call"
            );
            EffectPurpose::Call { attempt: key.attempt, completion: key.completion, position: key.position }
        }
        EffectOrigin::Propose { .. } => unreachable!("proposal has no entry"),
        origin @ EffectOrigin::Accept { .. } => complete_accept(core, work, out, origin, flight.connector),
        EffectOrigin::Procedure { task, step, entry: _ } => {
            procedure_wait(work, task, step);
            EffectPurpose::Procedure { purpose: description.purpose }
        }
    };
    let key =
        EffectKey { deployment: core.counters.deployment().id, task, origin: purpose, purpose: description.purpose };
    out.push(Request::Ask {
        connector: flight.connector,
        ask: Ask::Effect(connector::Ask::Keep { owner, entry, task, key }),
    });
    out.push(Request::Held(Box::new(Held::MakeEffect { connector: flight.connector, entry })));
}

fn charge(core: &mut Core, work: &mut Queue<Event>, origin: &EffectOrigin, price: Option<u64>) {
    if let Some(maximum) = price {
        let funder = match origin {
            EffectOrigin::Call { key, .. } => tasks::Funder::Task(key.task),
            EffectOrigin::Procedure { task, .. } => tasks::Funder::Task(*task),
            EffectOrigin::Accept { proposal, by, .. } => match *by {
                tasks::Party::Task(task) => tasks::Funder::Task(task),
                tasks::Party::Person(person) => {
                    let (period, pool) = core.goal_open_pool(proposal.project, person);
                    if let Some(event) = period {
                        work.push(Event::Tasks(event));
                    }
                    if let Some(event) = pool {
                        work.push(Event::Tasks(event));
                    }
                    tasks::Funder::Pool { project: proposal.project, person, period: core.settings.period }
                }
                tasks::Party::Deployment { .. } => unreachable!("authenticated effect accepter"),
            },
            EffectOrigin::Propose { .. } => unreachable!("proposal has no charge"),
        };
        work.push(Event::Tasks(tasks::Event::ChargeEffect { funder, maximum }));
    }
}

fn complete_accept(
    core: &mut Core,
    work: &mut Queue<Event>,
    out: &mut Queue<Request>,
    origin: EffectOrigin,
    number: u16,
) -> EffectPurpose {
    let (to, key, proposal, by) = match origin {
        EffectOrigin::Accept { to, key, proposal, by } => (to, key, proposal, by),
        EffectOrigin::Call { .. } | EffectOrigin::Propose { .. } | EffectOrigin::Procedure { .. } => {
            unreachable!("accepted effect")
        }
    };
    let purpose = match proposal.action {
        tasks::ProposalAction::Effect { attempt, completion, position, .. } => {
            EffectPurpose::Call { attempt, completion, position }
        }
        tasks::ProposalAction::Batch(_)
        | tasks::ProposalAction::Amend { .. }
        | tasks::ProposalAction::Widen { .. }
        | tasks::ProposalAction::Release { .. } => unreachable!("accepted effect"),
    };
    let message = fresh(&mut core.counters, Family::Message).expect("acceptance message admitted");
    let token = to.into_token();
    match key {
        Some(key) => {
            assert!(
                core.routing_calls.insert(token, crate::RoutedCall::Decide { key, proposal: proposal.number }).is_ok(),
                "effect acceptance route"
            );
        }
        None => {
            let person = match by {
                tasks::Party::Person(person) => person,
                tasks::Party::Task(_) | tasks::Party::Deployment { .. } => unreachable!("person accepter"),
            };
            assert!(
                core.routing_people_proposals
                    .insert(
                        token,
                        crate::PersonProposalRoute::Deciding {
                            request: token,
                            proposer: proposal.proposer,
                            proposal: proposal.number,
                            by: person
                        }
                    )
                    .is_ok(),
                "person effect acceptance"
            );
        }
    }
    let decided = tasks::Event::DecideProposal {
        reply_to: ReplyTo::new(token),
        proposer: proposal.proposer,
        proposal: proposal.number,
        message: Some(message),
        by,
        decision: tasks::ProposalDecision::Accept,
    };
    work.push(match key {
        Some(_) => Event::Tasks(decided),
        None => Event::PersonProposal(decided),
    });
    out.push(Request::Ask {
        connector: number,
        ask: Ask::Effect(connector::Ask::DropProposal { proposal: proposal.number }),
    });
    purpose
}

#[expect(clippy::too_many_arguments, reason = "one explicit proposal retains its checked carrier and connector owner")]
fn complete_proposal(
    core: &mut Core,
    env: &Env<Limits>,
    work: &mut Queue<Event>,
    out: &mut Queue<Request>,
    owner: Token,
    number: u16,
    to: ReplyTo,
    key: CallKey,
    description: connector::EffectDescription,
    reason: Box<[u8]>,
    as_holder: bool,
) {
    if number != description.connector || number != description.effect.connector {
        drop_effect(out, number, owner, authority::Answer::Refuse);
        answer(
            core,
            out,
            to,
            key,
            CallPart::EffectDenied { answer: authority::Answer::Refuse, findings: Box::new([]) },
        );
        return;
    }
    let Some(needed) = authority::needs(&authority::Action::Effect(description.effect.clone())) else {
        drop_effect(out, number, owner, authority::Answer::Refuse);
        answer(core, out, to, key, CallPart::Unavailable);
        return;
    };
    let token = to.into_token();
    let requests = crate::routing::named_propose(
        core,
        env,
        work,
        ReplyTo::new(token),
        key,
        tasks::ProposalAction::Effect {
            connector: number,
            authority: crate::translate::task_authority(&needed),
            attempt: key.attempt,
            completion: key.completion,
            position: key.position,
            purpose: description.purpose,
        },
        reason,
        as_holder,
    );
    match requests {
        Requests::Out(mut rows) => {
            for _ in 0..rows.len() {
                out.push(rows.pop().expect("proposal output count"));
            }
        }
    }
    if core.routing_calls.contains_key(&token) {
        assert!(
            core.proposing_effects.insert(token, (number, owner)).is_ok(),
            "one effect proposal admission per call"
        );
    } else {
        drop_effect(out, number, owner, authority::Answer::Refuse);
    }
}

fn check(
    core: &Core,
    env: &Env<Limits>,
    origin: &EffectOrigin,
    description: &connector::EffectDescription,
    given: &[authority::Given],
    findings: &mut Queue<authority::Finding>,
) -> authority::Answer {
    match origin {
        EffectOrigin::Call { key, .. } => core.connector_effect_admit(key.task, description, env.wall, given, findings),
        EffectOrigin::Procedure { task, .. } => {
            core.connector_effect_admit(*task, description, env.wall, given, findings)
        }
        EffectOrigin::Propose { .. } => unreachable!("proposal checks its ceiling without making"),
        EffectOrigin::Accept { proposal, by, .. } => {
            if core.tasks.proposal(proposal.proposer, proposal.number).as_ref() != Some(proposal) {
                return authority::Answer::Refuse;
            }

            let (number, needed, purpose) = match &proposal.action {
                tasks::ProposalAction::Effect { connector, authority, purpose, .. } => {
                    (*connector, authority, *purpose)
                }
                tasks::ProposalAction::Batch(_)
                | tasks::ProposalAction::Amend { .. }
                | tasks::ProposalAction::Widen { .. }
                | tasks::ProposalAction::Release { .. } => return authority::Answer::Refuse,
            };
            let actual = authority::needs(&authority::Action::Effect(description.effect.clone()));
            if description.connector != number
                || description.purpose != purpose
                || actual != Some(crate::translate::authority_value(needed))
            {
                return authority::Answer::Refuse;
            }
            let current = match proposal.state {
                tasks::ProposalState::Pending { holder, .. } => holder,
                tasks::ProposalState::Accepted { .. }
                | tasks::ProposalState::Rejected { .. }
                | tasks::ProposalState::Withdrawn => return authority::Answer::Refuse,
            };
            match *by {
                tasks::Party::Task(task) => {
                    if current != tasks::ProposalHolder::Task(task) {
                        return authority::Answer::Refuse;
                    }
                    core.connector_effect_admit(task, description, env.wall, given, findings)
                }
                tasks::Party::Person(person) => {
                    let standing = match current {
                        tasks::ProposalHolder::Person(holder) => holder == person,
                        tasks::ProposalHolder::Policy { project, kind } => {
                            project == proposal.project
                                && kind == tasks::ProposalKind::Effect
                                && core.proposal_policy_standing(person, project, authority::ProposalKind::Effect)
                        }
                        tasks::ProposalHolder::Task(_) => false,
                    };
                    if !standing {
                        return authority::Answer::Refuse;
                    }
                    let Some(role) = core.people.role(person, proposal.project) else {
                        return authority::Answer::Refuse;
                    };
                    let pool = match core.tasks.funding(tasks::Funder::Pool {
                        project: proposal.project,
                        person,
                        period: core.settings.period,
                    }) {
                        Some(pool) => pool.numbers,
                        None => tasks::Numbers {
                            budget: core.settings.person_budget,
                            spent: 0,
                            spent_below: 0,
                            reserved: 0,
                        },
                    };
                    let Some(policy) = core.authority.policy(proposal.project) else {
                        return authority::Answer::Refuse;
                    };
                    let mut acting = None;
                    for entry in &policy.roles {
                        if entry.number == role.number() {
                            acting = Some(entry.authority.clone());
                        }
                    }
                    let Some(authority) = acting else {
                        return authority::Answer::Refuse;
                    };
                    authority::check_effect(
                        &core.authority,
                        &authority::EffectAsk {
                            project: proposal.project,
                            authority,
                            numbers: crate::translate::authority_numbers(pool),
                            effect: description.effect.clone(),
                            now: env.wall,
                        },
                        given,
                        findings,
                    )
                }
                tasks::Party::Deployment { .. } => authority::Answer::Refuse,
            }
        }
    }
}

fn accept_refused(
    core: &mut Core,
    work: &mut Queue<Event>,
    out: &mut Queue<Request>,
    to: ReplyTo,
    key: Option<CallKey>,
    why: authority::Answer,
) {
    match key {
        Some(key) => {
            if why == authority::Answer::Wait {
                core.pending_calls.remove(&key);
                out.push(Request::Now(Box::new(Now::EffectAnswer {
                    to,
                    key,
                    part: CallPart::EffectDenied { answer: why, findings: Box::new([]) },
                })));
            } else {
                answer(
                    core,
                    out,
                    to,
                    key,
                    CallPart::ProposalRefused(tasks::Problem {
                        task: Some(key.task),
                        why: tasks::Refusal::AuthorityShape,
                        blocked_by: None,
                    }),
                );
            }
        }
        None => work.push(Event::People(jig_core_people::Event::Decided {
            request: to.into_token(),
            outcome: jig_core_people::Outcome::Refused(if why == authority::Answer::Wait {
                jig_core_people::Refusal::Busy
            } else {
                jig_core_people::Refusal::Authority
            }),
        })),
    }
}

fn procedure_wait(work: &mut Queue<Event>, task: u64, step: u64) {
    work.push(Event::Tasks(tasks::Event::Procedure {
        reply_to: ReplyTo::new(Token::new(u64::MAX)),
        task,
        step,
        decision: tasks::ProcedureDecision::Wait,
    }));
}

fn settled(
    core: &mut Core,
    work: &mut Queue<Event>,
    out: &mut Queue<Request>,
    entry: u64,
    task: u64,
    outcome: connector::OutboxOutcome,
) {
    let mut found = None;
    for (&key, part) in &core.call_parts {
        match part {
            CallPart::Effect { connector, entry: number, deadline, .. } if *number == entry && key.task == task => {
                found = Some((key, *connector, *deadline));
                break;
            }
            CallPart::Effect { .. }
            | CallPart::Connector { .. }
            | CallPart::EffectDenied { .. }
            | CallPart::ToolDenied { .. }
            | CallPart::EscalationDecided { .. }
            | CallPart::EscalationRefused(_)
            | CallPart::Proposed { .. }
            | CallPart::ProposalDecided { .. }
            | CallPart::ProposalRefused(_)
            | CallPart::Controlled
            | CallPart::ControlRefused(_)
            | CallPart::ControlDenied { .. }
            | CallPart::Sent { .. }
            | CallPart::Introduced
            | CallPart::MessageRefused(_)
            | CallPart::Subscribed { .. }
            | CallPart::Unsubscribed
            | CallPart::SubscriptionRefused(_)
            | CallPart::Delegated(_)
            | CallPart::DelegationDenied { .. }
            | CallPart::DelegationRefused(_)
            | CallPart::NoteWritten { .. }
            | CallPart::NoteRecalled { .. }
            | CallPart::NoteRefused(_)
            | CallPart::Unavailable => {}
        }
    }
    if let Some((key, connector, deadline)) = found {
        let part = CallPart::Effect { connector, entry, deadline, outcome: Some(outcome) };
        assert!(core.call_parts.insert(key, part.clone()).is_ok(), "retained call row");
        save(out, key, part.clone());
        match outcome {
            connector::OutboxOutcome::Made | connector::OutboxOutcome::Failed | connector::OutboxOutcome::Withdrawn => {
                if let Some(reply) = core.effect_replies.remove(&entry) {
                    out.push(Request::Held(Box::new(Held::CallAnswer { to: reply.to, key, part })));
                }
            }
            connector::OutboxOutcome::Uncertain | connector::OutboxOutcome::Held { .. } => {}
        }
    }
    let procedure = match core.tasks.task(task) {
        Some(row) => match row.executor {
            tasks::Executor::Procedure { .. } => true,
            tasks::Executor::Agent { .. } | tasks::Executor::Person(_) => false,
        },
        None => false,
    };
    if let Some(event) = core.connector_outbox(task, outcome, procedure) {
        work.push(Event::Tasks(event));
    }
}

pub(crate) fn deadline(core: &mut Core, env: &Env<Limits>) -> Requests {
    let mut out = Queue::with_capacity(env.limits.call_records.checked_add(1).expect("deadline replies"));
    let mut due = List::with_capacity(env.limits.call_records);
    for (&entry, waiting) in &core.effect_replies {
        if waiting.deadline <= env.wall {
            due.push(entry).expect("bounded replies");
        }
    }
    for &entry in &due {
        let waiting = core.effect_replies.remove(&entry).expect("due effect reply");
        if let Some(part) = core.call_parts.get(&waiting.key) {
            out.push(Request::Held(Box::new(Held::CallAnswer {
                to: waiting.to,
                key: waiting.key,
                part: part.clone(),
            })));
        }
    }
    end(&mut out);
    Requests::Out(out)
}

/// Heap owned by effect handoffs and live reply rights. Roots that price the
/// core's children separately add this bound for the core's effect state.
#[must_use]
pub fn effect_worst_case(limits: &Limits) -> Option<u64> {
    use core::mem::size_of;
    use skein_lib::Map;
    let flights = limits.call_records.checked_add(limits.tasks.tasks)?.checked_add(limits.people.pending)?;
    let name = u64::from(limits.authority.segments).checked_mul(
        u64::try_from(size_of::<Box<[u8]>>()).ok()?.checked_add(u64::from(limits.authority.segment_bytes))?,
    )?;
    let resources = u64::from(limits.authority.writes.checked_add(1)?)
        .checked_mul(name.checked_add(u64::try_from(size_of::<authority::EffectResource>()).ok()?)?)?;
    let description = u64::try_from(size_of::<connector::EffectDescription>())
        .ok()?
        .checked_add(resources)?
        .checked_add(List::<authority::Judge>::worst_case(limits.authority.facts)?)?;
    let flight = u64::try_from(size_of::<Flight>())
        .ok()?
        .checked_add(description)?
        .checked_add(u64::try_from(size_of::<tasks::Proposal>()).ok()?)?
        .checked_add(u64::from(limits.tasks.message_bytes))?
        .checked_add(u64::from(limits.tasks.authority_bytes))?
        .checked_add(u64::from(limits.tasks.authority_grants).checked_mul(
            u64::try_from(size_of::<tasks::Grant>()).ok()?.checked_add(
                u64::from(limits.tasks.authority_segments).checked_mul(u64::try_from(size_of::<Box<[u8]>>()).ok()?)?,
            )?,
        )?)?
        .checked_add(List::<authority::Judge>::worst_case(limits.authority.facts)?)?
        .checked_add(List::<authority::Given>::worst_case(limits.authority.facts)?)?;
    Map::<Token, Box<Flight>>::worst_case(flights)?
        .checked_add(u64::from(flights).checked_mul(flight)?)?
        .checked_add(Map::<u64, Waiting>::worst_case(limits.call_records)?)?
        .checked_add(Map::<Token, (u16, Token)>::worst_case(limits.call_records)?)?
        // One completed handoff owns a cloned description and all judge asks.
        .checked_add(description.checked_mul(u64::from(limits.authority.facts.checked_add(1)?))?)
}
