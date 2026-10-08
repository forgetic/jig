//! Total boundary translations for the testing application's tools, effects
//! and restart script (`domain/root.md`, sections 3 and 11;
//! `domain/engine.md`, sections 6–7).

use alloc::boxed::Box;
use jig_core as core;
use jig_core_authority as authority;
use jig_core_fleet as fleet;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_test_connector as connector;
use skein_lib::{Decision, Env, List, Queue, ReplyTo, Token};

use crate::{Delivery, Domain, Key, Limits, Record, Work, Write, hold, write};

pub(super) fn route_core(
    domain: &mut Domain,
    env: &Env<Limits>,
    decision: &mut Decision<Write, Delivery>,
    requests: core::Requests,
    work: &mut Queue<Work>,
) {
    let core::Requests::Out(mut requests) = requests;
    for _ in 0..requests.len() {
        match requests.pop().expect("core request count") {
            core::Request::Write(core::Write::Save(record)) => {
                write(domain, decision, Write::Save(Record::Core(record)));
            }
            core::Request::Write(core::Write::Erase(key)) => write(domain, decision, Write::Erase(Key::Core(key))),
            core::Request::Held(held) => match *held {
                core::Held::Assign { channel, run, attempt, .. } => {
                    let assignment = domain.assignments.remove(&run.raw()).expect("prepared claim has assignment");
                    assert!(assignment.attempt == attempt.raw(), "current assignment fence");
                    hold(decision, Delivery::Assigned { channel, assignment });
                }
                core::Held::SettledCall { to, call, .. } => {
                    let answer = crate::payload(domain, crate::Payload::SettledCall(call));
                    hold(decision, Delivery::Fleet(fleet::Event::Relayed { to, answer }));
                }
                core::Held::Relayed { channel, run, attempt, call: name, answer } => {
                    let Some(crate::Payload::SettledCall(call)) = domain.payloads.remove(&answer) else {
                        unreachable!("typed answer payload")
                    };
                    hold(
                        decision,
                        Delivery::CallAnswer { channel, task: run.raw(), attempt: attempt.raw(), name, call },
                    );
                }
                core::Held::MakeEffect { connector, entry } => {
                    hold(decision, Delivery::Core(core::Held::MakeEffect { connector, entry }));
                }
                core::Held::Inbound { channel, run, attempt, message } => {
                    let Some(crate::Payload::InboxWord(line)) = domain.payloads.remove(&message.words) else {
                        unreachable!("inbox word payload")
                    };
                    hold(decision, Delivery::Message { channel, task: run.raw(), attempt: attempt.raw(), word: line });
                }
                other @ (core::Held::Relay { .. }
                | core::Held::PeopleReply { .. }
                | core::Held::CallAnswer { .. }
                | core::Held::ViewStart { .. }
                | core::Held::ViewFinished { .. }
                | core::Held::ViewTaskPhase { .. }
                | core::Held::Result { .. }
                | core::Held::Acknowledge { .. }
                | core::Held::TaskTerminalAcknowledged { .. }
                | core::Held::TaskTurnKept { .. }
                | core::Held::ViewTurn { .. }
                | core::Held::AcknowledgeTurn { .. }
                | core::Held::Cancel { .. }
                | core::Held::Refuse { .. }
                | core::Held::TurnBusy { .. }
                | core::Held::StopRun { .. }
                | core::Held::NotesLoad { .. }
                | core::Held::NotesWritten { .. }
                | core::Held::NotesDeleted { .. }) => hold(decision, Delivery::Core(other)),
            },
            core::Request::Now(now) => route_now(domain, env, decision, *now, work),
            core::Request::Ask { connector, ask } => route_ask(domain, env, decision, connector, ask, work),
            core::Request::Decided => {}
        }
    }
}

#[expect(clippy::too_many_lines, reason = "one dispatcher translates the complete connector ask vocabulary")]
fn route_ask(
    domain: &mut Domain,
    env: &Env<Limits>,
    decision: &mut Decision<Write, Delivery>,
    number: u16,
    ask: core::Ask,
    work: &mut Queue<Work>,
) {
    match ask {
        core::Ask::Effect(ask) => route_effect_ask(domain, number, ask, work),
        core::Ask::TaskHoldings { request, spec, .. } => {
            let holdings = specification_holdings(domain, env, number, &spec);
            work.push(Work::Core(core::Event::Holdings { request, connector: number, holdings }));
        }
        core::Ask::DelegateHoldings { request, members, .. } => {
            let mut holdings: List<Box<[tasks::Holding]>> =
                List::with_capacity(u32::try_from(members.len()).expect("bounded batch"));
            let mut valid = true;
            for member in members {
                match specification_holdings(domain, env, number, &member.spec) {
                    Some(named) => holdings.push(named).expect("batch room"),
                    None => valid = false,
                }
            }
            work.push(Work::Core(core::Event::DelegateHoldings {
                request,
                connector: number,
                holdings: if valid { Some(holdings.into_boxed()) } else { None },
            }));
        }
        core::Ask::ProcedureHoldings { task, step, members } => {
            let mut holdings: List<Box<[tasks::Holding]>> =
                List::with_capacity(u32::try_from(members.len()).expect("bounded batch"));
            let mut valid = true;
            for member in members {
                match specification_holdings(domain, env, number, &member.spec) {
                    Some(named) => holdings.push(named).expect("batch room"),
                    None => valid = false,
                }
            }
            work.push(Work::Core(core::Event::ProcedureHoldings {
                task,
                step,
                connector: number,
                holdings: if valid { Some(holdings.into_boxed()) } else { None },
            }));
        }
        core::Ask::StartProcedure { context, step } => {
            let code = match context.executor {
                tasks::Executor::Procedure { code, .. } => u16::try_from(code).expect("test procedure code"),
                tasks::Executor::Agent { .. } | tasks::Executor::Person(_) => {
                    unreachable!("procedure context has a procedure executor")
                }
            };
            assert!(domain.procedure_steps.insert(context.task, (step, code)).is_ok(), "one procedure step flight");
            hold(decision, Delivery::Procedure { task: context.task, step, connector: number, code });
        }
        core::Ask::ClaimDone { task, attempt } => {
            if number == 1 {
                hold(
                    decision,
                    Delivery::Fleet(fleet::Event::Start {
                        reply_to: ReplyTo::new(Token::new(task)),
                        run: Token::new(task),
                        attempt: Token::new(attempt),
                        workstream: task,
                        assignment: fleet::Assignment { turns: Token::new(task), answered: Token::new(task) },
                        kinds: domain.hosting,
                    }),
                );
            }
        }
        core::Ask::Close { task, .. } => {
            work.push(Work::Connector { number, event: connector::Event::Close { task } });
        }
        core::Ask::Release { task, .. } => {
            work.push(Work::Connector { number, event: connector::Event::Unnamed { task } });
            if number == 1 {
                work.push(Work::Core(core::Event::Tasks(tasks::Event::Settled { task })));
            }
        }
        core::Ask::Adopt { request: _, project, adoption } => {
            let segments = adoption.resource.path;
            let path = path(segments);
            let role = match adoption.role {
                people::ResourceRole::Owned => connector::ResourceRole::Owned,
                people::ResourceRole::Fork => connector::ResourceRole::Participating,
                people::ResourceRole::Context => connector::ResourceRole::Context,
            };
            work.push(Work::Connector { number, event: connector::Event::Adopt { project, resource: path, role } });
        }
        core::Ask::Gather { section, budget } => {
            work.push(Work::Connector {
                number,
                event: connector::Event::Gather { token: section, task: section.raw(), budget },
            });
        }
        core::Ask::CutTo { section, size } => {
            work.push(Work::Connector { number, event: connector::Event::Cut { token: section, size } });
        }
        core::Ask::Drop { section } => {
            work.push(Work::Connector { number, event: connector::Event::HandOver { token: section } });
        }
        core::Ask::Lost { task, attempt } => {
            for resource in domain.core.tasks.lost_writes(task, attempt) {
                if resource.connector == number {
                    let call = connector::SystemRequest::ReadFact {
                        resource: path(resource.path.clone()),
                        observed: env.wall,
                    };
                    hold(decision, Delivery::LostRead { connector: number, task, attempt, resource, call });
                }
            }
        }
        core::Ask::Hold { .. }
        | core::Ask::EndTopic { .. }
        | core::Ask::ProjectGoal { .. }
        | core::Ask::SubscriptionDone { .. }
        | core::Ask::UnsubscriptionDone { .. }
        | core::Ask::DropSubscription { .. }
        | core::Ask::RepairRefused { .. }
        | core::Ask::DelegateRefused { .. } => {}
    }
}

/// Translate the connector's one-based resource IDs. Kinds one and two are
/// respectively exclusive and pooled in this testing application's charter.
fn specification_holdings(
    domain: &mut Domain,
    env: &Env<Limits>,
    number: u16,
    spec: &tasks::Spec,
) -> Option<Box<[tasks::Holding]>> {
    let mut holdings = List::with_capacity(env.limits.core.tasks.holdings);
    for parameter in &spec.parameters {
        match parameter {
            tasks::Parameter::Resource { connector: owner, resource, .. } => {
                if *owner == number {
                    let described = crate::numbered(domain, number).specification_resource(*resource)?;
                    let name = tasks::Name { connector: number, path: name(described.path.clone()).segments };
                    let holding = match described.hold {
                        connector::Hold::Exclusive { .. } => Some(tasks::Holding::Write { resource: name, kind: 1 }),
                        connector::Hold::Pooled { .. } => Some(tasks::Holding::Slot { pool: name, kind: 2 }),
                        connector::Hold::None => None,
                    };
                    if let Some(holding) = holding {
                        holdings.push(holding).ok()?;
                    }
                }
            }
            tasks::Parameter::Number { .. } | tasks::Parameter::Bytes { .. } => {}
        }
    }
    Some(holdings.into_boxed())
}

fn path(segments: Box<[Box<[u8]>]>) -> connector::Path {
    let capacity = u32::try_from(segments.len()).expect("bounded path segments");
    let mut path = List::with_capacity(capacity);
    for segment in segments {
        path.push(segment).expect("path room");
    }
    connector::Path::new(path, u32::MAX).expect("admitted nonempty path")
}

#[expect(clippy::too_many_lines, reason = "one exhaustive root continuation boundary")]
fn route_now(
    domain: &mut Domain,
    env: &Env<Limits>,
    _decision: &mut Decision<Write, Delivery>,
    now: core::Now,
    work: &mut Queue<Work>,
) {
    match now {
        core::Now::Undelivered { message, .. } => {
            drop(domain.payloads.remove(&message.words));
        }

        core::Now::Activate { context } => {
            work.push(Work::Core(core::Event::Activate { context, ready: domain.ready() }));
        }
        core::Now::PrepareAgent { context } => {
            let transcript_waiter = if context.ever_turned { Some(Token::new(context.task)) } else { None };
            work.push(Work::Core(core::Event::PreparedAgent { context, transcript_waiter, busy: false }));
        }
        core::Now::StartPreparation { task, transcript_waiter: None } => {
            work.push(Work::Core(core::Event::Tasks(tasks::Event::Prepare {
                reply_to: ReplyTo::new(Token::new(task)),
                task,
            })));
            work.push(Work::Core(core::Event::StartBrief { task }));
        }
        value @ core::Now::StartPreparation { task, transcript_waiter: Some(_) } => {
            work.push(Work::Core(core::Event::Tasks(tasks::Event::Prepare {
                reply_to: ReplyTo::new(Token::new(task)),
                task,
            })));
            domain.now.push(value);
        }
        core::Now::BriefCorePlanned { task, .. } => {
            work.push(Work::Core(core::Event::BriefAssembled { task, ready: true }));
        }
        core::Now::WorkspaceRequest { task, attempt, context } => {
            let mut names = List::with_capacity(env.limits.core.tasks.holdings);
            let mut checks = List::with_capacity(env.limits.core.authority.writes);
            for number in [1, 2] {
                let Some(holdings) = specification_holdings(domain, env, number, &context.spec) else {
                    work.push(Work::Core(core::Event::WorkspacePrepared { task, attempt, writes: None }));
                    return;
                };
                for holding in holdings {
                    match holding {
                        tasks::Holding::Write { resource, .. } => {
                            checks
                                .push(authority::Write {
                                    effect: authority::Effect {
                                        connector: resource.connector,
                                        kind: 5,
                                        name: authority::Name { segments: resource.path.clone() },
                                        state: [0; 32],
                                        price: None,
                                        access: authority::EffectAccess::Owned,
                                        additional: Box::new([]),
                                        guards: Box::new([]),
                                    },
                                    held: authority::Writer::Task,
                                })
                                .expect("workspace write check room");
                            names.push(resource).expect("admitted holding count");
                        }
                        tasks::Holding::Slot { .. } => {}
                    }
                }
            }
            assert!(domain.workspace_writes.insert(task, names.into_boxed()).is_ok(), "workspace per task");
            work.push(Work::Core(core::Event::WorkspacePrepared { task, attempt, writes: Some(checks.into_boxed()) }));
        }
        core::Now::RunPrepared { task, attempt, charter, run, inbox, transcript, grant, .. } => {
            let budget = run.budget;
            assert!(
                domain
                    .assignments
                    .insert(
                        task,
                        crate::Assignment {
                            task,
                            attempt,
                            charter,
                            run,
                            inbox,
                            transcript,
                            answered: domain.core.settled_calls(task, attempt),
                            grant
                        }
                    )
                    .is_ok(),
                "one prepared assignment per task"
            );
            let writes = domain.workspace_writes.remove(&task).expect("assembled workspace writes");
            work.push(Work::Core(core::Event::ClaimPrepared { task, attempt, budget, writes }));
        }
        core::Now::RunPreparationFailed { task } | core::Now::DropAssignment { task } => {
            drop(domain.assignments.remove(&task));
            drop(domain.workspace_writes.remove(&task));
        }
        core::Now::DropPayload { payload } => {
            drop(domain.payloads.remove(&payload));
        }
        core::Now::TurnPayload { run, attempt, turn, body } => {
            let payload = domain.payloads.get(&body).expect("fleet retained the turn body");
            let (cumulative, read) = match payload {
                crate::Payload::Turn { cumulative, read, .. } => (*cumulative, *read),
                crate::Payload::InboxWord(_) | crate::Payload::SettledCall(_) | crate::Payload::Answer { .. } => {
                    unreachable!("turn body family")
                }
            };
            work.push(Work::Core(core::Event::TurnPayload { run, attempt, turn, body, cumulative, read }));
        }
        core::Now::AnswerPayload { run, attempt, payload } => {
            let body = domain.payloads.get(&payload).expect("fleet retained the answer body");
            let (cumulative, end) = match body {
                crate::Payload::Answer { cumulative, end, .. } => (*cumulative, end.clone()),
                crate::Payload::InboxWord(_) | crate::Payload::SettledCall(_) | crate::Payload::Turn { .. } => {
                    unreachable!("answer body family")
                }
            };
            work.push(Work::Core(core::Event::AnswerPayload {
                run,
                attempt,
                payload,
                cumulative,
                end,
                saved: None,
                invalid_saved: false,
            }));
        }
        core::Now::AcceptedTurn { payload, task, attempt, turn, accepted } => {
            let body = domain.payloads.remove(&payload).expect("accepted turn body");
            let (cumulative, read, transcript) = match body {
                crate::Payload::Turn { cumulative, read, transcript, .. } => (cumulative, read, transcript),
                crate::Payload::InboxWord(_) | crate::Payload::SettledCall(_) | crate::Payload::Answer { .. } => {
                    unreachable!("accepted turn body family")
                }
            };
            work.push(Work::Core(core::Event::AcceptedTurn {
                task,
                attempt,
                turn,
                accepted,
                cumulative,
                read,
                transcript,
            }));
        }
        core::Now::RefusedPayload { request, problem } => {
            let payload = match domain.payloads.remove(&request) {
                Some(crate::Payload::Turn { task, attempt, turn, .. }) => {
                    Some(core::PayloadRefusal::Turn { task, attempt, turn })
                }
                Some(crate::Payload::Answer { task, attempt, .. }) => {
                    Some(core::PayloadRefusal::Answer { task, attempt })
                }
                Some(crate::Payload::InboxWord(_) | crate::Payload::SettledCall(_)) => {
                    unreachable!("typed answer is not a turn or terminal")
                }
                None => None,
            };
            work.push(Work::Core(core::Event::RefusedPayload { request, problem, payload }));
        }
        core::Now::HistoricalProposal { .. }
        | core::Now::HistoricalEscalation { .. }
        | core::Now::EscalationInspection { .. }
        | core::Now::CompleteBrief { .. }
        | core::Now::ProcedureDelegateOutcome { .. }
        | core::Now::RestoreRefused => {
            unreachable!("this route awaits the testing application's scripted peer");
        }
        other @ (core::Now::SettledCallRefused { .. }
        | core::Now::Call { .. }
        | core::Now::DropCall { .. }
        | core::Now::EffectAnswer { .. }
        | core::Now::SignInRefused { .. }
        | core::Now::WatchRefused { .. }
        | core::Now::Account(_)
        | core::Now::View(_)
        | core::Now::NotesIndexed { .. }
        | core::Now::NotesRecalled { .. }
        | core::Now::NotesRefused { .. }
        | core::Now::NoteBusy { .. }
        | core::Now::EscalationReply { .. }
        | core::Now::EscalationRefused { .. }) => domain.now.push(other),
    }
}

#[expect(clippy::too_many_lines, reason = "one exhaustive connector boundary dispatcher")]
pub(super) fn route_connector(
    domain: &mut Domain,
    env: &Env<Limits>,
    decision: &mut Decision<Write, Delivery>,
    number: u16,
    requests: &mut Queue<connector::Request>,
    work: &mut Queue<Work>,
) {
    for _ in 0..requests.len() {
        match requests.pop().expect("connector request count") {
            connector::Request::Closed { task } => {
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Closed {
                    task,
                    connector: number,
                })));
            }
            connector::Request::Drift { pool: resource, tasks }
            | connector::Request::DriftResource { tasks, resource } => {
                let resource = name(resource);
                for task in tasks {
                    work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Drift {
                        task,
                        resource: tasks::Name { connector: number, path: resource.segments.clone() },
                    })));
                }
            }
            connector::Request::RestartDone => {
                let step = core::RestartStep::SettleOutbox { connector: number };
                let request = domain.core.restart_done(step);
                work.push(Work::Restart(request));
            }
            connector::Request::Save { record } => {
                match &record {
                    connector::Record::Outbox(row) => {
                        assert!(domain.outbox_tasks.insert(row.number, row.task).is_ok(), "bounded outbox task routes");
                    }
                    connector::Record::Proposal { .. }
                    | connector::Record::Task { .. }
                    | connector::Record::Adoption { .. }
                    | connector::Record::Subscription { .. }
                    | connector::Record::Pool { .. }
                    | connector::Record::Procedure(_)
                    | connector::Record::Result { .. }
                    | connector::Record::Made { .. } => {}
                }
                write(domain, decision, Write::Save(Record::Connector { number, record }));
            }
            connector::Request::Erase { key } => {
                write(domain, decision, Write::Erase(Key::Connector { number, key }));
            }
            connector::Request::Step { task, decision: step } => {
                let (numbered_step, code) = domain.procedure_steps.remove(&task).expect("procedure chose a step");
                let action = match step {
                    connector::StepDecision::Finish { result: connector::ProcedureResult::Succeeded } => {
                        core::ProcedureAction::Result(tasks::TaskResult::Report { words: Box::new([]) })
                    }
                    connector::StepDecision::Finish { result: connector::ProcedureResult::Local { code } } => {
                        core::ProcedureAction::Result(tasks::TaskResult::Change {
                            connector: number,
                            kind: code,
                            resource: 0,
                            words: Box::new([]),
                        })
                    }
                    connector::StepDecision::Wait { .. } | connector::StepDecision::Stall => {
                        core::ProcedureAction::Wait
                    }
                    connector::StepDecision::Hold { .. } => core::ProcedureAction::Hold(tasks::Hold::Effects),
                    connector::StepDecision::Effect(effect) => {
                        let owner = crate::effect_payload(domain, effect);
                        assert!(domain.effect_procedures.insert(owner, task).is_ok(), "one procedure effect flight");
                        work.push(Work::Core(core::Event::EffectStart {
                            owner,
                            connector: number,
                            origin: core::EffectOrigin::Procedure { task, step: numbered_step, entry: None },
                        }));
                        continue;
                    }
                    connector::StepDecision::Delegate { .. } | connector::StepDecision::Propose { .. } => {
                        unreachable!("effect and delegation scripts enter in the next session")
                    }
                };
                work.push(Work::Core(core::Event::ProcedureStep {
                    task,
                    step: numbered_step,
                    connector: number,
                    code: u32::from(code),
                    action,
                }));
            }
            connector::Request::System(call) => hold(decision, Delivery::System { connector: number, call }),
            connector::Request::Described { token, description } => {
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Described {
                    owner: token,
                    description: Box::new(describe(number, description)),
                })));
            }
            connector::Request::EffectBusy { token } => work
                .push(Work::Core(core::Event::EffectConnector(core::connector::Event::DescribeBusy { owner: token }))),
            connector::Request::EffectRefused { token } => {
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::DescribeRefused {
                    owner: token,
                })));
            }
            connector::Request::Verdict { token, verdict } => {
                let Some((owner, judge, asked_state)) = domain.judges.remove(&token) else { continue };
                let (verdict, at, guarded, state) = match verdict {
                    connector::Verdict::Met { state, observed, guarded } => {
                        let pin = state_pin(state);
                        (authority::Verdict::Met, observed, guarded, pin)
                    }
                    connector::Verdict::Wait => (authority::Verdict::Wait, env.wall, false, asked_state),
                    connector::Verdict::Refuse { .. } => (authority::Verdict::Refuse, env.wall, false, asked_state),
                };
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Verdict {
                    owner,
                    judge,
                    verdict,
                    at,
                    guarded,
                    state,
                })));
            }
            connector::Request::Outcome { entry, outcome } => {
                let task = match domain.outbox_tasks.get(&entry) {
                    Some(task) => *task,
                    None => continue,
                };
                let made = match outcome {
                    connector::Outcome::Made { .. } => true,
                    connector::Outcome::Failed
                    | connector::Outcome::Uncertain
                    | connector::Outcome::Withdrawn
                    | connector::Outcome::Held => false,
                };
                let settled = match outcome {
                    connector::Outcome::Made { .. } | connector::Outcome::Failed | connector::Outcome::Withdrawn => {
                        true
                    }
                    connector::Outcome::Uncertain | connector::Outcome::Held => false,
                };
                match domain.outbox_procedures.get(&entry) {
                    Some(number) if settled => work.push(Work::Connector {
                        number: *number,
                        event: connector::Event::EffectDecision { task, made },
                    }),
                    Some(_) | None => {}
                }
                if settled {
                    domain.outbox_tasks.remove(&entry);
                    domain.outbox_procedures.remove(&entry);
                }
                let outcome = match outcome {
                    connector::Outcome::Made { .. } => core::connector::OutboxOutcome::Made,
                    connector::Outcome::Failed => core::connector::OutboxOutcome::Failed,
                    connector::Outcome::Uncertain => core::connector::OutboxOutcome::Uncertain,
                    connector::Outcome::Held => core::connector::OutboxOutcome::Held { entry },
                    connector::Outcome::Withdrawn => core::connector::OutboxOutcome::Withdrawn,
                };
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Outbox {
                    entry,
                    task,
                    outcome,
                })));
            }
            connector::Request::Slots { pool, slots } => {
                let pool = tasks::Name { connector: number, path: name(pool).segments };
                work.push(Work::Core(core::Event::Tasks(tasks::Event::Slots { pool, slots })));
            }
            connector::Request::Make { entry: _ }
            | connector::Request::Adopted { project: _, resource: _, result: _ }
            | connector::Request::Named { task: _, resources: _ }
            | connector::Request::Unknown { task: _, resource: _ }
            | connector::Request::Refused { task: _ }
            | connector::Request::News { topic: _, subscribers: _ }
            | connector::Request::Changed { resource: _ }
            | connector::Request::Answer { token: _, bytes: _ }
            | connector::Request::Ready { token: _, size: _ }
            | connector::Request::Section { token: _, bytes: _ }
            | connector::Request::Workspace { token: _, items: _ } => {}
        }
    }
}

/// Translate the core's choice without choosing a judge or admitting a write.
fn route_effect_ask(domain: &mut Domain, number: u16, ask: core::connector::Ask, work: &mut Queue<Work>) {
    let event = match ask {
        core::connector::Ask::Describe { owner } => connector::Event::Describe {
            token: owner,
            effect: domain.effects.remove(&owner).expect("connector-owned decoded effect"),
        },
        core::connector::Ask::DescribeProposal { owner, proposal } => {
            connector::Event::DescribeProposal { token: owner, number: proposal }
        }
        core::connector::Ask::KeepProposal { owner, proposal, task } => {
            connector::Event::KeepProposal { token: owner, number: proposal, task }
        }
        core::connector::Ask::DropProposal { proposal } => connector::Event::DropProposal { number: proposal },
        core::connector::Ask::Keep { owner, entry, task, key } => {
            if domain.effect_procedures.remove(&owner).is_some() {
                assert!(domain.outbox_procedures.insert(entry, number).is_ok(), "procedure effect route");
            }
            let (attempt, completion, position) = match key.origin {
                core::EffectPurpose::Call { attempt, completion, position } => (attempt, completion, position),
                core::EffectPurpose::Procedure { .. } => (0, 0, 0),
            };
            connector::Event::Keep {
                token: owner,
                entry,
                task,
                key: connector::Key {
                    deployment: key.deployment,
                    task: key.task,
                    purpose: key.purpose,
                    attempt,
                    completion,
                    position,
                },
            }
        }
        core::connector::Ask::Drop { owner, .. } => {
            if let Some(task) = domain.effect_procedures.remove(&owner) {
                work.push(Work::Connector { number, event: connector::Event::EffectDecision { task, made: false } });
            }
            drop(domain.effects.remove(&owner));
            connector::Event::Drop { token: owner }
        }
        core::connector::Ask::Judge { owner, judge, state, resources } => {
            let token = Token::new(domain.next_payload);
            domain.next_payload = domain.next_payload.checked_add(1).expect("bounded judge owner numbers");
            assert!(domain.judges.insert(token, (owner, judge, state)).is_ok(), "one route per core-selected judge");
            let mut names = List::with_capacity(u32::try_from(resources.len()).expect("bounded resource names"));
            for name in resources {
                names.push(path(name.segments)).expect("one path per name");
            }
            let bytes = [state[24], state[25], state[26], state[27], state[28], state[29], state[30], state[31]];
            connector::Event::Judge {
                token,
                requirement: judge.requirement,
                resources: names.into_boxed(),
                state: u64::from_be_bytes(bytes),
            }
        }
        core::connector::Ask::Names { .. }
        | core::connector::Ask::Unname { .. }
        | core::connector::Ask::Hold { .. }
        | core::connector::Ask::ReleaseHold { .. }
        | core::connector::Ask::Writer { .. }
        | core::connector::Ask::Make { .. }
        | core::connector::Ask::ProcedureMade { .. }
        | core::connector::Ask::ProcedureActivate { .. }
        | core::connector::Ask::ProcedureMessage { .. }
        | core::connector::Ask::ProcedureClose { .. }
        | core::connector::Ask::Project { .. }
        | core::connector::Ask::Release { .. }
        | core::connector::Ask::Subscribe { .. }
        | core::connector::Ask::Unsubscribe { .. }
        | core::connector::Ask::Read { .. }
        | core::connector::Ask::Gather { .. }
        | core::connector::Ask::CutTo { .. }
        | core::connector::Ask::Take { .. }
        | core::connector::Ask::Prepare { .. }
        | core::connector::Ask::Left { .. }
        | core::connector::Ask::Adopt { .. }
        | core::connector::Ask::Restore { .. }
        | core::connector::Ask::ReadAfresh
        | core::connector::Ask::SettleOutbox => unreachable!("effect handoff vocabulary"),
    };
    work.push(Work::Connector { number, event });
}

fn name(path: connector::Path) -> authority::Name {
    let mut segments = List::with_capacity(u32::try_from(path.segments().len()).expect("bounded segments"));
    for segment in path.segments() {
        segments.push(segment.clone()).expect("one segment per path");
    }
    authority::Name { segments: segments.into_boxed() }
}

fn describe(number: u16, description: connector::Description) -> core::connector::EffectDescription {
    let form = match description.form {
        connector::Form::Creation => core::connector::EffectForm::Creation,
        connector::Form::Transition => core::connector::EffectForm::Transition,
        connector::Form::Set => core::connector::EffectForm::Set,
    };
    let recovery = match description.recovery {
        connector::Recovery::Keyed => core::connector::Recovery::Keyed,
        connector::Recovery::Conditional => core::connector::Recovery::Conditional,
        connector::Recovery::Idempotent => core::connector::Recovery::Idempotent,
        connector::Recovery::Unrecoverable => core::connector::Recovery::Unrecoverable,
    };
    let mut resources = List::with_capacity(u32::try_from(description.resources.len()).expect("bounded resources"));
    for resource in description.resources {
        resources.push(name(resource)).expect("one name per resource");
    }
    let mut additional = List::with_capacity(resources.len().saturating_sub(1));
    let first = resources.as_slice().first().expect("connector described at least one resource").clone();
    for resource in resources.as_slice().get(1..).expect("at least one resource") {
        additional
            .push(authority::EffectResource { name: resource.clone(), access: authority::EffectAccess::Owned })
            .expect("additional resources");
    }
    let state = state_pin(description.state);
    core::connector::EffectDescription {
        connector: number,
        purpose: description.purpose,
        form,
        recovery,
        effect: authority::Effect {
            connector: number,
            kind: description.kind,
            name: first,
            state,
            price: description.price,
            access: authority::EffectAccess::Owned,
            additional: additional.into_boxed(),
            guards: Box::new([]),
        },
    }
}

fn state_pin(state: u64) -> [u8; 32] {
    let bytes = state.to_be_bytes();
    let mut pin = [0; 32];
    for offset in 0..8 {
        *pin.get_mut(24usize.checked_add(offset).expect("pin offset")).expect("eight bytes fit the pin") =
            *bytes.get(offset).expect("eight-byte integer");
    }
    pin
}
