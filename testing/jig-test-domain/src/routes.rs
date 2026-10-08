//! Total boundary translations for the testing application's current walking
//! routes (`domain/root.md`, sections 3 and 11). The later effect and restart
//! scripts extend the same root in `domain/engine.md`, sections 6–7.

use alloc::boxed::Box;
use jig_core as core;
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
            core::Request::Write(core::Write::Save(record)) => write(decision, Write::Save(Record::Core(record))),
            core::Request::Write(core::Write::Erase(key)) => write(decision, Write::Erase(Key::Core(key))),
            core::Request::Held(held) => match *held {
                core::Held::Assign { channel, run, attempt } => {
                    let assignment = domain.assignments.remove(&run.raw()).expect("prepared claim has assignment");
                    assert!(assignment.attempt == attempt.raw(), "current assignment fence");
                    hold(decision, Delivery::Assigned { channel, assignment });
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
                | core::Held::Relayed { .. }
                | core::Held::Inbound { .. }
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

fn route_ask(
    domain: &mut Domain,
    _env: &Env<Limits>,
    decision: &mut Decision<Write, Delivery>,
    number: u16,
    ask: core::Ask,
    work: &mut Queue<Work>,
) {
    match ask {
        core::Ask::TaskHoldings { request, .. } => {
            work.push(Work::Core(core::Event::Holdings { request, connector: number, holdings: Some(Box::new([])) }));
        }
        core::Ask::DelegateHoldings { request, members, .. } => {
            let mut holdings: List<Box<[tasks::Holding]>> =
                List::with_capacity(u32::try_from(members.len()).expect("bounded batch"));
            for _member in members {
                holdings.push(Box::new([])).expect("batch room");
            }
            work.push(Work::Core(core::Event::DelegateHoldings {
                request,
                connector: number,
                holdings: Some(holdings.into_boxed()),
            }));
        }
        core::Ask::ProcedureHoldings { task, step, members } => {
            let mut holdings: List<Box<[tasks::Holding]>> =
                List::with_capacity(u32::try_from(members.len()).expect("bounded batch"));
            for _member in members {
                holdings.push(Box::new([])).expect("batch room");
            }
            work.push(Work::Core(core::Event::ProcedureHoldings {
                task,
                step,
                connector: number,
                holdings: Some(holdings.into_boxed()),
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
                        kinds: fleet::Kinds::Workers,
                    }),
                );
            }
        }
        core::Ask::Close { task, .. } => {
            if number == 1 {
                work.push(Work::Core(core::Event::Tasks(tasks::Event::EffectsSettled { task })));
            }
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
        core::Ask::Hold { .. }
        | core::Ask::EndTopic { .. }
        | core::Ask::Lost { .. }
        | core::Ask::ProjectGoal { .. }
        | core::Ask::SubscriptionDone { .. }
        | core::Ask::UnsubscriptionDone { .. }
        | core::Ask::DropSubscription { .. }
        | core::Ask::RepairRefused { .. }
        | core::Ask::DelegateRefused { .. } => {}
    }
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
    _env: &Env<Limits>,
    _decision: &mut Decision<Write, Delivery>,
    now: core::Now,
    work: &mut Queue<Work>,
) {
    match now {
        core::Now::Activate { context } => work.push(Work::Core(core::Event::Activate { context, ready: true })),
        core::Now::PrepareAgent { context } => {
            work.push(Work::Core(core::Event::PreparedAgent { context, transcript_waiter: None, busy: false }));
        }
        core::Now::StartPreparation { task, transcript_waiter: None } => {
            work.push(Work::Core(core::Event::Tasks(tasks::Event::Prepare {
                reply_to: ReplyTo::new(Token::new(task)),
                task,
            })));
            work.push(Work::Core(core::Event::StartBrief { task }));
        }
        core::Now::BriefCorePlanned { task, .. } => {
            work.push(Work::Core(core::Event::BriefAssembled { task, ready: true }));
        }
        core::Now::WorkspaceRequest { task, attempt, .. } => {
            work.push(Work::Core(core::Event::WorkspacePrepared { task, attempt, writes: Some(Box::new([])) }));
        }
        core::Now::RunPrepared { task, attempt, charter, run, inbox, transcript, grant, .. } => {
            let budget = run.budget;
            assert!(
                domain
                    .assignments
                    .insert(task, crate::Assignment { task, attempt, charter, run, inbox, transcript, grant })
                    .is_ok(),
                "one prepared assignment per task"
            );
            work.push(Work::Core(core::Event::ClaimPrepared { task, attempt, budget, writes: Box::new([]) }));
        }
        core::Now::RunPreparationFailed { task } | core::Now::DropAssignment { task } => {
            drop(domain.assignments.remove(&task));
        }
        core::Now::DropPayload { payload } => {
            drop(domain.payloads.remove(&payload));
        }
        core::Now::TurnPayload { run, attempt, turn, body } => {
            let payload = domain.payloads.get(&body).expect("fleet retained the turn body");
            let cumulative = match payload {
                crate::Payload::Turn { cumulative, .. } => *cumulative,
                crate::Payload::Answer { .. } => unreachable!("turn body family"),
            };
            work.push(Work::Core(core::Event::TurnPayload { run, attempt, turn, body, cumulative, read: None }));
        }
        core::Now::AnswerPayload { run, attempt, payload } => {
            let body = domain.payloads.get(&payload).expect("fleet retained the answer body");
            let (cumulative, end) = match body {
                crate::Payload::Answer { cumulative, end, .. } => (*cumulative, end.clone()),
                crate::Payload::Turn { .. } => unreachable!("answer body family"),
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
            let (cumulative, transcript) = match body {
                crate::Payload::Turn { cumulative, transcript, .. } => (cumulative, transcript),
                crate::Payload::Answer { .. } => unreachable!("accepted turn body family"),
            };
            work.push(Work::Core(core::Event::AcceptedTurn {
                task,
                attempt,
                turn,
                accepted,
                cumulative,
                read: None,
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
                None => None,
            };
            work.push(Work::Core(core::Event::RefusedPayload { request, problem, payload }));
        }
        core::Now::StartPreparation { transcript_waiter: Some(_), .. }
        | core::Now::HistoricalProposal { .. }
        | core::Now::HistoricalEscalation { .. }
        | core::Now::EscalationInspection { .. }
        | core::Now::CallPayload { .. }
        | core::Now::CompleteBrief { .. }
        | core::Now::ProcedureDelegateOutcome { .. }
        | core::Now::RestoreRefused => {
            unreachable!("this route awaits the testing application's scripted peer");
        }
        other @ (core::Now::SignInRefused { .. }
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

pub(super) fn route_connector(
    domain: &mut Domain,
    _env: &Env<Limits>,
    decision: &mut Decision<Write, Delivery>,
    number: u16,
    requests: &mut Queue<connector::Request>,
    work: &mut Queue<Work>,
) {
    for _ in 0..requests.len() {
        match requests.pop().expect("connector request count") {
            connector::Request::Save { record } => {
                write(decision, Write::Save(Record::Connector { number, record }));
            }
            connector::Request::Erase { key } => {
                write(decision, Write::Erase(Key::Connector { number, key }));
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
                    connector::StepDecision::Effect(_)
                    | connector::StepDecision::Delegate { .. }
                    | connector::StepDecision::Propose { .. } => {
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
            connector::Request::Adopted { project: _, resource: _, result: _ }
            | connector::Request::Named { task: _, resources: _ }
            | connector::Request::Unknown { task: _, resource: _ }
            | connector::Request::Refused { task: _ }
            | connector::Request::Slots { pool: _, slots: _ }
            | connector::Request::Drift { pool: _, tasks: _ }
            | connector::Request::News { topic: _, subscribers: _ }
            | connector::Request::DriftResource { tasks: _, resource: _ }
            | connector::Request::Changed { resource: _ }
            | connector::Request::RestartDone
            | connector::Request::Verdict { token: _, verdict: _ }
            | connector::Request::Answer { token: _, bytes: _ }
            | connector::Request::Ready { token: _, size: _ }
            | connector::Request::Section { token: _, bytes: _ }
            | connector::Request::Workspace { token: _, items: _ }
            | connector::Request::Described { token: _, description: _ }
            | connector::Request::EffectRefused { token: _ }
            | connector::Request::Make { entry: _ }
            | connector::Request::Outcome { entry: _, outcome: _ } => {}
        }
    }
}
