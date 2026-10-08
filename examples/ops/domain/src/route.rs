//! Each variant fixes its destination. Replace connector arms when copying;
//! retain the journal marks and engine's permanent host link (domain/root.md, 5).
use crate::boundary::Payload;
use crate::domain::{Work, child, held, now, write};
use crate::translate;
use crate::{Domain, Event, Key, Limits, Output, Record, Released, Write};
use alloc::boxed::Box;
use jig_core as core;
use jig_core_authority as authority;
use jig_core_fleet as fleet;
use jig_core_tasks as tasks;
use jig_host as host;
use jig_inline_agent as agent;
use jig_ops_domain_infrastructure as infrastructure;
use jig_ops_domain_observability as observability;
use skein_lib::{Decision, Env, List, Queue, ReplyTo, Token};

pub(crate) fn work(
    domain: &mut Domain,
    env: &Env<Limits>,
    decision: &mut Decision<Write, Output>,
    item: Work,
    work: &mut Queue<Work>,
) {
    match item {
        Work::Event(event) => event_in(domain, env, decision, event, work),
        Work::Core(event) => {
            let requests =
                core::step(&mut domain.core, &Env { now: env.now, wall: env.wall, limits: env.limits.core }, event);
            core_out(domain, env, decision, requests, work);
        }
        Work::ResumeFleet => {
            let requests =
                core::resume_fleet(&mut domain.core, &Env { now: env.now, wall: env.wall, limits: env.limits.core });
            core_out(domain, env, decision, requests, work);
            let mut hub_out = Queue::with_capacity(host::max_out(&env.limits.host));
            host::resume(
                &mut domain.host,
                &Env { now: env.now, wall: env.wall, limits: env.limits.host },
                &mut hub_out,
            );
            host_out(domain, env, decision, &mut hub_out, work);
            // Smith exposes bounded ready work just like the fleet. A copied
            // root resumes both children in its ordinary domain iteration.
            let mut out = Queue::with_capacity(agent::max_out(&env.limits.agent));
            agent::resume(
                &mut domain.agents,
                &Env { now: env.now, wall: env.wall, limits: env.limits.agent },
                &mut out,
            );
            agent_out(domain, env, decision, &mut out, work);
        }
        Work::Infrastructure(event) => {
            let mut out = Queue::with_capacity(infrastructure::MAX_OUT);
            infrastructure::step(
                &mut domain.infrastructure,
                &Env { now: env.now, wall: env.wall, limits: env.limits.infrastructure },
                event,
                &mut out,
            );
            infrastructure_out(domain, env, decision, &mut out, work);
        }
        Work::Observability(event) => {
            let mut out = Queue::with_capacity(observability::MAX_OUT);
            observability::step(
                &mut domain.observability,
                &Env { now: env.now, wall: env.wall, limits: env.limits.observability },
                event,
                &mut out,
            );
            observability_out(domain, env, decision, &mut out, work);
        }
        Work::Host(event) => {
            let mut out = Queue::with_capacity(host::max_out(&env.limits.host));
            host::step(
                &mut domain.host,
                &Env { now: env.now, wall: env.wall, limits: env.limits.host },
                event,
                &mut out,
            );
            host_out(domain, env, decision, &mut out, work);
        }
        Work::Agent(event) => {
            let mut out = Queue::with_capacity(agent::max_out(&env.limits.agent));
            agent::step(
                &mut domain.agents,
                &Env { now: env.now, wall: env.wall, limits: env.limits.agent },
                event,
                &mut out,
            );
            agent_out(domain, env, decision, &mut out, work);
        }
        Work::Restart(request) => crate::restart::route(domain, env, decision, request, work),
        Work::AdoptDone => {
            let request = domain.core.restart_done(core::RestartStep::AdoptRuns);
            work.push(Work::Restart(request));
        }
    }
}

fn event_in(
    domain: &mut Domain,
    env: &Env<Limits>,
    decision: &mut Decision<Write, Output>,
    event: Event,
    work: &mut Queue<Work>,
) {
    match event {
        Event::Core(event) => work.push(Work::Core(event)),
        Event::Infrastructure(event) => work.push(Work::Infrastructure(event)),
        Event::Observability(event) => work.push(Work::Observability(event)),
        Event::Host(event) => work.push(Work::Host(event)),
        Event::Restart => {
            domain.restarting = true;
            let request = domain.core.restart_begin();
            work.push(Work::Restart(request));
        }
        Event::Store(store) => crate::restart::store(domain, env, decision, store, work),
        Event::Released(released) => match released {
            Released::Core(event) => work.push(Work::Core(event)),
            Released::Fleet(event) => work.push(Work::Core(core::Event::Fleet(event))),
            Released::Host(event) => work.push(Work::Host(event)),
            Released::Agent(event) => work.push(Work::Agent(event)),
            Released::Infrastructure(event) => work.push(Work::Infrastructure(event)),
            Released::Observability(event) => work.push(Work::Observability(event)),
            Released::Restart(step) => work.push(Work::Restart(core::RestartRequest::Step(step))),
            Released::Procedure { task, step: _, connector, code: _ } => {
                if connector == domain.numbers.infrastructure {
                    work.push(Work::Infrastructure(infrastructure::Event::Procedure {
                        task,
                        signal: infrastructure::ProcedureSignal::Step,
                    }));
                }
            }
        },
        Event::Effect { to, key, effect, deadline, proposal } => {
            let owner = domain.token();
            domain.effects.insert(owner, effect).expect("admitted decoded effect");
            let origin = match proposal {
                Some(reason) => core::EffectOrigin::Propose { to, key, reason, as_holder: false },
                None => core::EffectOrigin::Call { to, key, deadline },
            };
            work.push(Work::Core(core::Event::EffectStart { owner, connector: domain.numbers.infrastructure, origin }));
        }
        Event::Timer(timer) => {
            let requests =
                core::fire(&mut domain.core, &Env { now: env.now, wall: env.wall, limits: env.limits.core }, timer);
            core_out(domain, env, decision, requests, work);
        }
        Event::InfrastructureTimer => {
            let mut out = Queue::with_capacity(infrastructure::MAX_OUT);
            infrastructure::fire(
                &mut domain.infrastructure,
                &Env { now: env.now, wall: env.wall, limits: env.limits.infrastructure },
                &mut out,
            );
            infrastructure_out(domain, env, decision, &mut out, work);
        }
        Event::ObservabilityTimer => {
            let mut out = Queue::with_capacity(observability::MAX_OUT);
            observability::fire(
                &mut domain.observability,
                &Env { now: env.now, wall: env.wall, limits: env.limits.observability },
                &mut out,
            );
            observability_out(domain, env, decision, &mut out, work);
        }
        Event::HostTimer => {
            let mut out = Queue::with_capacity(agent::max_out(&env.limits.agent));
            agent::fire(&mut domain.agents, &Env { now: env.now, wall: env.wall, limits: env.limits.agent }, &mut out);
            agent_out(domain, env, decision, &mut out, work);
        }
        Event::Llm { client, completion } => {
            let mut out = Queue::with_capacity(agent::max_out(&env.limits.agent));
            agent::terminal(
                &mut domain.agents,
                &Env { now: env.now, wall: env.wall, limits: env.limits.agent },
                client,
                completion,
                &mut out,
            );
            agent_out(domain, env, decision, &mut out, work);
        }
    }
}

pub(crate) fn core_out(
    domain: &mut Domain,
    env: &Env<Limits>,
    decision: &mut Decision<Write, Output>,
    requests: core::Requests,
    work: &mut Queue<Work>,
) {
    let core::Requests::Out(mut out) = requests;
    for _ in 0..out.len() {
        match out.pop().expect("core request count") {
            core::Request::Write(core::Write::Save(record)) => {
                write(domain, decision, Write::Save(Record::Core(record)));
            }
            core::Request::Write(core::Write::Erase(key)) => write(domain, decision, Write::Erase(Key::Core(key))),
            core::Request::Held(value) => core_held(domain, decision, *value, work),
            core::Request::Now(value) => core_now(domain, env, *value, work),
            core::Request::Ask { connector, ask } => core_ask(domain, env, decision, connector, ask, work),
            core::Request::Decided => {}
        }
    }
}
fn core_held(domain: &mut Domain, decision: &mut Decision<Write, Output>, value: core::Held, work: &mut Queue<Work>) {
    match value {
        core::Held::MakeEffect { connector, entry } => {
            if connector == domain.numbers.infrastructure {
                child(decision, Released::Infrastructure(infrastructure::Event::MakeEntry { entry }));
            } else if connector == domain.numbers.observability {
                child(decision, Released::Observability(observability::Event::MakeEntry { entry }));
            }
        }
        core::Held::Assign { channel, run, attempt, .. } => {
            assert!(channel == Token::new(0), "ops engine's permanent link");
            let assignment = domain.assignments.remove(&run.raw()).expect("core asked to retain its assignment");
            assert!(assignment.attempt == attempt.raw(), "assignment fence");
            child(
                decision,
                Released::Host(host::Event::Assign { reply_to: ReplyTo::new(run), assignment: assignment.value }),
            );
        }
        core::Held::SettledCall { to, key, call } => {
            domain.calls.remove(&key);
            let answer = domain.payload(Payload::Settled(call));
            work.push(Work::Core(core::Event::Fleet(fleet::Event::Relayed { to, answer })));
        }
        core::Held::Relayed { run, attempt, call, answer, .. } => {
            if let Some(released) = crate::assemble::relayed(domain, run, attempt, call, answer) {
                child(decision, released);
            }
        }
        core::Held::Inbound { run, attempt, message, .. } => {
            let value = domain.payloads.remove(&message.words).expect("message token");
            let message_word = match value {
                Payload::Word(message_word) => message_word,
                Payload::Turn { .. } | Payload::Answer { .. } | Payload::Settled(_) => {
                    unreachable!("inbox word family")
                }
            };
            child(
                decision,
                Released::Host(host::Event::Inbound {
                    run,
                    attempt,
                    name: message.name,
                    sender: crate::assemble::sender(message_word.from),
                    words: message_word.words,
                }),
            );
        }
        core::Held::Relay { task, attempt, word, .. } => {
            let name = Token::new(word.number);
            let words = domain.payload(Payload::Word(word));
            child(
                decision,
                Released::Fleet(fleet::Event::Inbound {
                    run: Token::new(task),
                    attempt: Token::new(attempt),
                    message: fleet::Message { name, sender: Token::new(0), words },
                }),
            );
        }
        core::Held::TaskTurnKept { task: run, attempt, turn } => {
            child(
                decision,
                Released::Fleet(fleet::Event::TurnKept { run: Token::new(run), attempt: Token::new(attempt), turn }),
            );
        }
        core::Held::AcknowledgeTurn { run, attempt, turn, .. } => {
            child(decision, Released::Host(host::Event::AcknowledgeTurn { run, attempt, turn }));
        }
        core::Held::Cancel { run, attempt, .. } => {
            child(decision, Released::Host(host::Event::Cancel { run, attempt }));
        }
        core::Held::StopRun { task, attempt } => child(
            decision,
            Released::Fleet(fleet::Event::Cancel { run: Token::new(task), attempt: Token::new(attempt) }),
        ),
        core::Held::Acknowledge { .. } => child(decision, Released::Host(host::Event::Unacknowledged { answers: 0 })),
        core::Held::TaskTerminalAcknowledged { task, attempt } => child(
            decision,
            Released::Fleet(fleet::Event::Acknowledge { run: Token::new(task), attempt: Token::new(attempt) }),
        ),
        core::Held::CallAnswer { to, key, part } => work.push(Work::Core(core::Event::SettledCall {
            to,
            key,
            call: crate::assemble::answer(domain, key, part),
        })),
        other @ (core::Held::PeopleReply { .. }
        | core::Held::ViewStart { .. }
        | core::Held::ViewFinished { .. }
        | core::Held::ViewTaskPhase { .. }
        | core::Held::Result { .. }
        | core::Held::ViewTurn { .. }
        | core::Held::Refuse { .. }
        | core::Held::TurnBusy { .. }
        | core::Held::NotesLoad { .. }
        | core::Held::NotesWritten { .. }
        | core::Held::NotesDeleted { .. }) => held(decision, Output::Core(other)),
    }
}

#[expect(clippy::too_many_lines, reason = "one exhaustive dispatcher translates this child boundary")]
fn core_ask(
    domain: &mut Domain,
    env: &Env<Limits>,
    decision: &mut Decision<Write, Output>,
    number: u16,
    ask: core::Ask,
    work: &mut Queue<Work>,
) {
    match ask {
        core::Ask::Effect(ask) => effect_ask(domain, env, number, ask, work),
        core::Ask::TaskHoldings { request, spec, .. } => work.push(Work::Core(core::Event::Holdings {
            request,
            connector: number,
            holdings: translate::holdings(number, domain.numbers.infrastructure, &spec),
        })),
        core::Ask::DelegateHoldings { request, members, .. } => {
            let mut holdings = List::with_capacity(u32::try_from(members.len()).expect("bounded batch"));
            let mut valid = true;
            for member in members {
                match translate::holdings(number, domain.numbers.infrastructure, &member.spec) {
                    Some(value) => holdings.push(value).expect("one holdings row"),
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
            let mut holdings = List::with_capacity(u32::try_from(members.len()).expect("bounded batch"));
            let mut valid = true;
            for member in members {
                match translate::holdings(number, domain.numbers.infrastructure, &member.spec) {
                    Some(value) => holdings.push(value).expect("one holdings row"),
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
            let task = context.task;
            let code = match context.executor {
                tasks::Executor::Procedure { code, .. } => code,
                tasks::Executor::Agent { .. } | tasks::Executor::Person(_) => {
                    unreachable!("core-selected procedure executor")
                }
            };
            domain.procedures.insert(task, (step, number, code)).expect("core-selected procedure continuation");
            if number == domain.numbers.infrastructure {
                match translate::procedure(code, &context.spec) {
                    Some(procedure) => {
                        work.push(Work::Infrastructure(infrastructure::Event::StartProcedure { task, procedure }));
                    }
                    None => work.push(Work::Core(core::Event::ProcedureStep {
                        task,
                        step,
                        connector: number,
                        code,
                        action: core::ProcedureAction::Hold(tasks::Hold::Procedure),
                    })),
                }
                child(decision, Released::Procedure { task, step, connector: number, code });
            } else if number == domain.numbers.observability {
                match translate::spec_service(&context.spec) {
                    Some(service) => {
                        work.push(Work::Observability(observability::Event::StartWatch {
                            task,
                            services: Box::new([observability::Service::new(service.environment, service.name)]),
                            template: u16::try_from(translate::spec_number(&context.spec, 3).unwrap_or(1))
                                .expect("watch template"),
                        }));
                        if let Some(row) = domain.core.tasks.task(task) {
                            for subscription in &row.subscriptions {
                                match subscription.kind {
                                    tasks::SubscriptionKind::Topic { connector, topic } => {
                                        if connector == number && topic == task {
                                            work.push(Work::Observability(observability::Event::LinkWatch {
                                                task,
                                                subscription: subscription.number,
                                            }));
                                        }
                                    }
                                    tasks::SubscriptionKind::Task { .. } | tasks::SubscriptionKind::Timer { .. } => {}
                                }
                            }
                        }
                        let mut news = false;
                        for word in &context.inbox {
                            match word.kind {
                                tasks::MessageKind::News { .. } => news = true,
                                tasks::MessageKind::Escalation { .. }
                                | tasks::MessageKind::Proposal { .. }
                                | tasks::MessageKind::ProposalDecision { .. }
                                | tasks::MessageKind::Words
                                | tasks::MessageKind::Amendment { .. }
                                | tasks::MessageKind::Question
                                | tasks::MessageKind::Answer { .. }
                                | tasks::MessageKind::Notice { .. }
                                | tasks::MessageKind::Timer { .. }
                                | tasks::MessageKind::Result(_) => {}
                            }
                        }
                        if news {
                            work.push(Work::Observability(observability::Event::WakeWatch {
                                task,
                                batch: context.last_message,
                                alerts: Box::new([]),
                            }));
                        } else {
                            domain.procedures.remove(&task);
                            work.push(Work::Core(core::Event::ProcedureStep {
                                task,
                                step,
                                connector: number,
                                code,
                                action: core::ProcedureAction::Wait,
                            }));
                        }
                    }
                    None => work.push(Work::Core(core::Event::ProcedureStep {
                        task,
                        step,
                        connector: number,
                        code,
                        action: core::ProcedureAction::Hold(tasks::Hold::Procedure),
                    })),
                }
            }
        }
        core::Ask::ClaimDone { task, attempt } => {
            if number == domain.numbers.infrastructure {
                child(
                    decision,
                    Released::Fleet(fleet::Event::Start {
                        reply_to: ReplyTo::new(Token::new(task)),
                        run: Token::new(task),
                        attempt: Token::new(attempt),
                        workstream: task,
                        assignment: fleet::Assignment { turns: Token::new(task), answered: Token::new(task) },
                        kinds: fleet::Kinds::Engine,
                    }),
                );
            }
        }
        core::Ask::Close { task, .. } => {
            if number == domain.numbers.observability {
                work.push(Work::Observability(observability::Event::Close { task }));
            } else if number == domain.numbers.infrastructure {
                work.push(Work::Infrastructure(infrastructure::Event::Close { task }));
            }
        }
        core::Ask::Release { task, .. } => {
            if number == domain.numbers.observability {
                work.push(Work::Observability(observability::Event::Release { task }));
            } else if number == domain.numbers.infrastructure {
                work.push(Work::Infrastructure(infrastructure::Event::Release { task }));
            }
        }
        core::Ask::Gather { section, .. } | core::Ask::CutTo { section, .. } => {
            work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::SectionReady {
                section,
                size: None,
            })));
        }
        core::Ask::ProjectGoal { feed } => {
            // These two connectors have no goal projection. Their closing
            // hand-off has no pending writes to settle.
            if feed.closing {
                work.push(Work::Core(core::Event::ProjectionSettled { goal: feed.goal.number, connector: number }));
            }
        }
        core::Ask::SubscriptionDone { request, key, subscription } => {
            if number == domain.numbers.observability {
                work.push(Work::Observability(observability::Event::LinkWatch { task: key.task, subscription }));
            }
            // The core broadcasts this completed task operation to every
            // connector. The permanent hub receives one assembled answer.
            if number == domain.numbers.infrastructure {
                work.push(Work::Core(core::Event::NamedAnswer {
                    to: ReplyTo::new(request),
                    key,
                    part: core::CallPart::Subscribed { subscription },
                }));
            }
        }
        core::Ask::UnsubscriptionDone { request, key } => {
            if number == domain.numbers.infrastructure {
                work.push(Work::Core(core::Event::NamedAnswer {
                    to: ReplyTo::new(request),
                    key,
                    part: core::CallPart::Unsubscribed,
                }));
            }
        }
        // These proof-of-concept connectors provide no projection, brief
        // section, adoption, workspace or writer-handoff state. A copying
        // application fills these arms when its connectors expose it.
        core::Ask::ForgetProjection { .. }
        | core::Ask::Adopt { .. }
        | core::Ask::Hold { .. }
        | core::Ask::EndTopic { .. }
        | core::Ask::Drop { .. }
        | core::Ask::Lost { .. }
        | core::Ask::DropSubscription { .. }
        | core::Ask::RepairRefused { .. }
        | core::Ask::DelegateRefused { .. } => {}
    }
}
#[expect(clippy::too_many_lines, reason = "one exhaustive dispatcher translates this child boundary")]
fn effect_ask(domain: &mut Domain, env: &Env<Limits>, number: u16, ask: core::connector::Ask, work: &mut Queue<Work>) {
    match ask {
        core::connector::Ask::Describe { owner } => match domain.effects.remove(&owner) {
            Some(effect) => work.push(Work::Infrastructure(infrastructure::Event::Describe { token: owner, effect })),
            None => {
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::DescribeRefused { owner })));
            }
        },
        core::connector::Ask::DescribeProposal { owner, proposal } => {
            work.push(Work::Infrastructure(infrastructure::Event::DescribeProposal { token: owner, number: proposal }));
        }
        core::connector::Ask::KeepProposal { owner, proposal, task } => work
            .push(Work::Infrastructure(infrastructure::Event::KeepProposal { token: owner, number: proposal, task })),
        core::connector::Ask::DropProposal { proposal } => {
            work.push(Work::Infrastructure(infrastructure::Event::DropProposal { number: proposal }));
        }
        core::connector::Ask::Keep { owner, entry, key, .. } => {
            domain.effect_procedures.remove(&owner);
            if number == domain.numbers.infrastructure {
                work.push(Work::Infrastructure(infrastructure::Event::Keep {
                    token: owner,
                    entry,
                    key: translate::infrastructure_key(key),
                }));
            } else if number == domain.numbers.observability {
                work.push(Work::Observability(observability::Event::Keep {
                    token: owner,
                    entry,
                    key: translate::observability_key(key),
                }));
            }
        }
        core::connector::Ask::Drop { owner, answer } => {
            if let Some(task) = domain.effect_procedures.remove(&owner) {
                let signal = match answer {
                    authority::Answer::Allow => infrastructure::ProcedureSignal::EffectMade,
                    authority::Answer::Wait => infrastructure::ProcedureSignal::EffectWaiting,
                    authority::Answer::Propose | authority::Answer::Refuse => {
                        infrastructure::ProcedureSignal::EffectFailed
                    }
                };
                work.push(Work::Infrastructure(infrastructure::Event::Procedure { task, signal }));
            }
            domain.effects.remove(&owner);
            if number == domain.numbers.infrastructure {
                work.push(Work::Infrastructure(infrastructure::Event::Drop { token: owner }));
            } else if number == domain.numbers.observability {
                work.push(Work::Observability(observability::Event::Drop { token: owner }));
            }
        }
        core::connector::Ask::Judge { owner, judge, state, resources } => {
            let token = domain.token();
            match translate::service(&resources) {
                Some(service) => match translate::requirement(judge.requirement) {
                    Some(requirement) => {
                        domain.judges.insert(token, (owner, judge, state)).expect("core-selected judge continuation");
                        work.push(Work::Observability(observability::Event::Judge {
                            token,
                            requirement,
                            service,
                            freshness: 30,
                        }));
                    }
                    None => work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Verdict {
                        owner,
                        judge,
                        verdict: authority::Verdict::Refuse,
                        at: env.wall,
                        guarded: false,
                        state,
                    }))),
                },
                None => work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Verdict {
                    owner,
                    judge,
                    verdict: authority::Verdict::Refuse,
                    at: env.wall,
                    guarded: false,
                    state,
                }))),
            }
        }
        core::connector::Ask::Make { entry } => {
            if number == domain.numbers.infrastructure {
                work.push(Work::Infrastructure(infrastructure::Event::MakeEntry { entry }));
            } else if number == domain.numbers.observability {
                work.push(Work::Observability(observability::Event::MakeEntry { entry }));
            }
        }
        core::connector::Ask::ReadAfresh => {
            if number == domain.numbers.infrastructure {
                work.push(Work::Infrastructure(infrastructure::Event::ReadAfresh));
            } else if number == domain.numbers.observability {
                work.push(Work::Observability(observability::Event::ReadAfresh));
            }
        }
        core::connector::Ask::SettleOutbox => {
            if number == domain.numbers.infrastructure {
                work.push(Work::Infrastructure(infrastructure::Event::Restart));
            } else if number == domain.numbers.observability {
                work.push(Work::Observability(observability::Event::Restart));
            }
        }
        core::connector::Ask::Names { .. }
        | core::connector::Ask::Unname { .. }
        | core::connector::Ask::Hold { .. }
        | core::connector::Ask::ReleaseHold { .. }
        | core::connector::Ask::Writer { .. }
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
        | core::connector::Ask::Restore { .. } => {}
    }
}

#[expect(clippy::too_many_lines, reason = "one exhaustive dispatcher translates this child boundary")]
fn infrastructure_out(
    domain: &mut Domain,
    _env: &Env<Limits>,
    decision: &mut Decision<Write, Output>,
    out: &mut Queue<infrastructure::Request>,
    work: &mut Queue<Work>,
) {
    for _ in 0..out.len() {
        match out.pop().expect("infrastructure requests") {
            infrastructure::Request::Closed { task } => {
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Closed {
                    task,
                    connector: domain.numbers.infrastructure,
                })));
            }
            infrastructure::Request::Released { task } => {
                work.push(Work::Core(core::Event::Tasks(tasks::Event::Settled { task })));
            }
            infrastructure::Request::Save { record } => {
                write(domain, decision, Write::Save(Record::Infrastructure(record)));
            }
            infrastructure::Request::Erase { key } => write(domain, decision, Write::Erase(Key::Infrastructure(key))),
            infrastructure::Request::System(request) => held(decision, Output::Infrastructure(request)),
            infrastructure::Request::Described { token, description } => {
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Described {
                    owner: token,
                    description: Box::new(translate::infrastructure_description(
                        domain.numbers.infrastructure,
                        description,
                    )),
                })));
            }
            infrastructure::Request::Refused { token } => {
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::DescribeRefused {
                    owner: token,
                })));
            }
            infrastructure::Request::Make { key } => {
                child(decision, Released::Infrastructure(infrastructure::Event::Make { key }));
            }
            infrastructure::Request::Outcome { entry, key, outcome } => {
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Outbox {
                    entry,
                    task: key.task,
                    outcome: translate::infrastructure_outcome(outcome),
                })));
                match key.origin {
                    infrastructure::Purpose::Procedure { .. } => {
                        work.push(Work::Infrastructure(infrastructure::Event::Procedure {
                            task: key.task,
                            signal: match outcome {
                                infrastructure::Outcome::Made => infrastructure::ProcedureSignal::EffectMade,
                                infrastructure::Outcome::Failed => infrastructure::ProcedureSignal::EffectFailed,
                                infrastructure::Outcome::Uncertain => infrastructure::ProcedureSignal::Step,
                            },
                        }));
                    }
                    infrastructure::Purpose::Call { .. } | infrastructure::Purpose::Projection { .. } => {}
                }
            }
            infrastructure::Request::Step { task, decision: action } => {
                let Some((step, connector, code)) = domain.procedures.remove(&task) else { continue };
                match action {
                    infrastructure::StepDecision::Effect(effect) => {
                        let owner = domain.token();
                        domain.effects.insert(owner, effect).expect("admitted procedure effect");
                        domain.effect_procedures.insert(owner, task).expect("procedure effect continuation");
                        work.push(Work::Core(core::Event::EffectStart {
                            owner,
                            connector,
                            origin: core::EffectOrigin::Procedure { task, step, entry: None },
                        }));
                    }
                    infrastructure::StepDecision::Wait { .. } => work.push(Work::Core(core::Event::ProcedureStep {
                        task,
                        step,
                        connector,
                        code,
                        action: core::ProcedureAction::Wait,
                    })),
                    infrastructure::StepDecision::Finish => work.push(Work::Core(core::Event::ProcedureStep {
                        task,
                        step,
                        connector,
                        code,
                        action: core::ProcedureAction::Result(tasks::TaskResult::Report { words: Box::new([]) }),
                    })),
                    infrastructure::StepDecision::Hold { .. } => work.push(Work::Core(core::Event::ProcedureStep {
                        task,
                        step,
                        connector,
                        code,
                        action: core::ProcedureAction::Hold(tasks::Hold::Effects),
                    })),
                }
            }
            infrastructure::Request::Drift { task, resource } => {
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Drift {
                    task,
                    resource: tasks::Name { connector: domain.numbers.infrastructure, path: resource.segments() },
                })));
            }
            infrastructure::Request::Slots { pool, quota, used: _ } => {
                work.push(Work::Core(core::Event::Tasks(tasks::Event::Slots {
                    pool: tasks::Name {
                        connector: domain.numbers.infrastructure,
                        path: infrastructure::Resource::Pool(pool).segments(),
                    },
                    slots: quota,
                })));
            }
            infrastructure::Request::Changed { resource } => {
                for task in domain.infrastructure.readers(&resource) {
                    work.push(Work::Core(core::Event::Tasks(tasks::Event::WakeProcedure { task })));
                }
            }
            infrastructure::Request::Named { .. } => {}
            infrastructure::Request::Restarted { stage } => {
                let step = match stage {
                    infrastructure::RestartStage::ReadAfresh => {
                        core::RestartStep::ReadAfresh { connector: domain.numbers.infrastructure }
                    }
                    infrastructure::RestartStage::Outbox => {
                        core::RestartStep::SettleOutbox { connector: domain.numbers.infrastructure }
                    }
                };
                let request = domain.core.restart_done(step);
                work.push(Work::Restart(request));
            }
        }
    }
}
#[expect(clippy::too_many_lines, reason = "one exhaustive dispatcher translates this child boundary")]
fn observability_out(
    domain: &mut Domain,
    env: &Env<Limits>,
    decision: &mut Decision<Write, Output>,
    out: &mut Queue<observability::Request>,
    work: &mut Queue<Work>,
) {
    for _ in 0..out.len() {
        match out.pop().expect("observability requests") {
            observability::Request::Closed { task } => {
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Closed {
                    task,
                    connector: domain.numbers.observability,
                })));
            }
            observability::Request::Released { task } => {
                work.push(Work::Core(core::Event::Tasks(tasks::Event::Settled { task })));
            }
            observability::Request::Save { record } => {
                write(domain, decision, Write::Save(Record::Observability(record)));
            }
            observability::Request::Erase { key } => write(domain, decision, Write::Erase(Key::Observability(key))),
            observability::Request::System(request @ observability::SystemRequest::Read { .. }) => {
                now(domain, Output::Observability(request));
            }
            observability::Request::System(
                request @ (observability::SystemRequest::Facts { .. }
                | observability::SystemRequest::Silence { .. }
                | observability::SystemRequest::FindSilence { .. }),
            ) => held(decision, Output::Observability(request)),
            observability::Request::Answer { token, bytes } => {
                if let Some(call) = domain.read_calls.remove(&token) {
                    crate::assemble::read_answer(domain, call, false, bytes, work);
                } else {
                    now(domain, Output::Read { owner: token, bytes });
                }
            }
            observability::Request::Verdict { token, verdict } => {
                let Some((owner, judge, state)) = domain.judges.remove(&token) else { continue };
                let (verdict, at) = translate::verdict(verdict, env.wall);
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Verdict {
                    owner,
                    judge,
                    verdict,
                    at,
                    guarded: false,
                    state,
                })));
            }
            observability::Request::Described { token, effect } => {
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Described {
                    owner: token,
                    description: Box::new(translate::observability_description(domain.numbers.observability, effect)),
                })));
            }
            observability::Request::Refused { token } => {
                if let Some(call) = domain.read_calls.remove(&token) {
                    crate::assemble::read_answer(domain, call, true, Box::from(&b"unavailable"[..]), work);
                } else {
                    work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::DescribeRefused {
                        owner: token,
                    })));
                }
            }
            observability::Request::Make { key } => {
                child(decision, Released::Observability(observability::Event::Make { key }));
            }
            observability::Request::Outcome { entry, key, outcome } => {
                work.push(Work::Core(core::Event::EffectConnector(core::connector::Event::Outbox {
                    entry,
                    task: key.task,
                    outcome: translate::observability_outcome(outcome),
                })));
            }
            observability::Request::News { alert, subscribers } => {
                for subscriber in subscribers {
                    let class = translate::news_class(subscriber.class);
                    if let Some(event) = domain.core.connector_news(
                        subscriber.task,
                        subscriber.subscription,
                        class,
                        translate::alert_words(&alert),
                        env.wall,
                    ) {
                        work.push(Work::Core(core::Event::Tasks(event)));
                    }
                }
            }
            observability::Request::Health { service, healthy, subscribers } => {
                for subscriber in subscribers {
                    let class = translate::news_class(subscriber.class);
                    if let Some(event) = domain.core.connector_news(
                        subscriber.task,
                        subscriber.subscription,
                        class,
                        translate::health_words(&service, healthy),
                        env.wall,
                    ) {
                        work.push(Work::Core(core::Event::Tasks(event)));
                    }
                }
            }
            observability::Request::Triage { watch, delegate, alerts, .. } => {
                let Some((step, connector, code)) = domain.procedures.remove(&watch) else { continue };
                let action = match translate::triage(&delegate, &alerts) {
                    Some(batch) => core::ProcedureAction::Delegate(batch),
                    None => core::ProcedureAction::Hold(tasks::Hold::Procedure),
                };
                work.push(Work::Core(core::Event::ProcedureStep { task: watch, step, connector, code, action }));
            }
            observability::Request::Changed { service } => {
                let resource =
                    infrastructure::Resource::Service(infrastructure::Service::new(service.environment, service.name));
                for task in domain.infrastructure.readers(&resource) {
                    work.push(Work::Core(core::Event::Tasks(tasks::Event::WakeProcedure { task })));
                }
            }
            observability::Request::Restarted { stage } => {
                let step = match stage {
                    observability::RestartStage::ReadAfresh => {
                        core::RestartStep::ReadAfresh { connector: domain.numbers.observability }
                    }
                    observability::RestartStage::Outbox => {
                        core::RestartStep::SettleOutbox { connector: domain.numbers.observability }
                    }
                };
                let request = domain.core.restart_done(step);
                work.push(Work::Restart(request));
            }
        }
    }
}

#[expect(clippy::too_many_lines, reason = "one exhaustive dispatcher translates this child boundary")]
fn core_now(domain: &mut Domain, env: &Env<Limits>, value: core::Now, work: &mut Queue<Work>) {
    match value {
        core::Now::Relayed { run, attempt, call, answer, .. } => {
            if let Some(released) = crate::assemble::relayed(domain, run, attempt, call, answer) {
                now(domain, Output::ToChild(released));
            }
        }
        core::Now::EffectAnswer { to, key, part } => {
            let call = crate::assemble::answer(domain, key, part);
            domain.calls.remove(&key);
            let answer = domain.payload(Payload::Settled(call));
            work.push(Work::Core(core::Event::Fleet(fleet::Event::Relayed { to, answer })));
        }
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
            now(domain, Output::CoreLoad(value));
        }
        core::Now::BriefCorePlanned { task, sections, .. } => {
            work.push(Work::Core(core::Event::Brief(jig_core_brief::GatherEvent::Plan {
                brief: Token::new(task),
                budget: env.limits.core.brief.brief_bytes,
                deadline: env.now.saturating_add(skein_lib::Duration::from_secs(30)),
                sections: sections.into_boxed(),
            })));
        }
        core::Now::CompleteBrief { brief, order } => {
            let sections = crate::assemble::brief(order);
            domain.briefs.insert(brief.raw(), sections).expect("core-selected brief continuation");
            work.push(Work::Core(core::Event::BriefAssembled { task: brief.raw(), ready: true }));
        }
        core::Now::WorkspaceRequest { task, attempt, .. } => {
            work.push(Work::Core(core::Event::WorkspacePrepared { task, attempt, writes: Some(Box::new([])) }));
        }
        core::Now::RunPrepared { task, attempt, run, transcript, grant, .. } => {
            let budget = run.budget;
            let sections = domain.briefs.remove(&task).expect("completed brief parts");
            match crate::assemble::assignment(domain, env, task, attempt, *run, sections, transcript, grant) {
                Some(value) => {
                    domain
                        .assignments
                        .insert(task, crate::domain::Assignment { attempt, value })
                        .expect("core-selected assignment");
                    work.push(Work::Core(core::Event::ClaimPrepared { task, attempt, budget, writes: Box::new([]) }));
                }
                None => work.push(Work::Core(core::Event::PreparationFailed { task })),
            }
        }
        core::Now::RunPreparationFailed { task } | core::Now::DropAssignment { task } => {
            drop(domain.assignments.remove(&task));
            drop(domain.briefs.remove(&task));
        }
        core::Now::DropPayload { payload } => {
            drop(domain.payloads.remove(&payload));
        }
        core::Now::TurnPayload { run, attempt, turn, body } => {
            let payload = domain.payloads.get(&body).expect("fleet retained the turn body");
            let (spent, read) = match payload {
                Payload::Turn { spent, read, .. } => (*spent, *read),
                Payload::Word(_) | Payload::Settled(_) | Payload::Answer { .. } => {
                    unreachable!("turn body family")
                }
            };
            work.push(Work::Core(core::Event::TurnPayload { run, attempt, turn, body, cumulative: spent, read }));
        }
        core::Now::AnswerPayload { run, attempt, payload } => {
            let body = domain.payloads.get(&payload).expect("fleet retained the answer body");
            let (spent, end) = match body {
                Payload::Answer { spent, end, .. } => (*spent, end.clone()),
                Payload::Word(_) | Payload::Settled(_) | Payload::Turn { .. } => {
                    unreachable!("answer body family")
                }
            };
            work.push(Work::Core(core::Event::AnswerPayload {
                run,
                attempt,
                payload,
                cumulative: spent,
                end,
                saved: None,
                invalid_saved: false,
            }));
        }
        core::Now::AcceptedTurn { payload, task, attempt, turn, accepted } => {
            let body = domain.payloads.remove(&payload).expect("accepted turn body");
            let (spent, read, transcript) = match body {
                Payload::Turn { spent, read, body: transcript, .. } => (spent, read, transcript),
                Payload::Word(_) | Payload::Settled(_) | Payload::Answer { .. } => {
                    unreachable!("accepted turn body family")
                }
            };
            work.push(Work::Core(core::Event::AcceptedTurn {
                task,
                attempt,
                turn,
                accepted,
                cumulative: spent,
                read,
                transcript,
            }));
        }
        core::Now::RefusedPayload { request, problem } => {
            let payload = match domain.payloads.remove(&request) {
                Some(Payload::Turn { task, attempt, turn, .. }) => {
                    Some(core::PayloadRefusal::Turn { task, attempt, turn })
                }
                Some(Payload::Answer { task, attempt, .. }) => Some(core::PayloadRefusal::Answer { task, attempt }),
                Some(Payload::Word(_) | Payload::Settled(_)) => {
                    unreachable!("answer is not a turn or terminal")
                }
                None => None,
            };
            work.push(Work::Core(core::Event::RefusedPayload { request, problem, payload }));
        }
        load @ (core::Now::HistoricalProposal { .. } | core::Now::HistoricalEscalation { .. }) => {
            now(domain, Output::CoreLoad(load));
        }
        core::Now::ProcedureDelegateOutcome { .. } => {}
        core::Now::RestoreRefused => {
            let _request = domain.core.restart_refuse();
        }
        core::Now::EscalationInspection { waiter, context } => {
            now(domain, Output::Now(core::Now::EscalationInspection { waiter, context }));
        }
        core::Now::Call { to, run, attempt, call } => {
            crate::assemble::call(domain, env, to, run, attempt, call, work);
        }
        core::Now::SettledCallRefused { to, key, name, tool } => {
            domain.calls.remove(&key);
            crate::assemble::read_answer(
                domain,
                crate::domain::ReadCall { to: to.into_token(), name, tool },
                true,
                Box::from(&b"unavailable"[..]),
                work,
            );
        }
        core::Now::DropCall { call } => {
            if let Some(binding) = domain.host_calls.remove(&call.name) {
                now(
                    domain,
                    Output::ToChild(Released::Host(host::Event::Relayed {
                        run: binding.run,
                        attempt: binding.attempt,
                        call: binding.delivery,
                        answer: crate::assemble::host_answer(core::SettledAnswer::Host {
                            error: true,
                            body: Box::from(&b"unavailable"[..]),
                        }),
                    })),
                );
            }
        }
        core::Now::NoteBusy { to, key } => {
            let (name, tool) = domain.calls.remove(&key).expect("named note call envelope");
            crate::assemble::read_answer(
                domain,
                crate::domain::ReadCall { to: to.into_token(), name, tool },
                true,
                Box::from(&b"busy"[..]),
                work,
            );
        }
        other @ (core::Now::SignInRefused { .. }
        | core::Now::WatchRefused { .. }
        | core::Now::Account(_)
        | core::Now::View(_)
        | core::Now::NotesIndexed { .. }
        | core::Now::NotesRecalled { .. }
        | core::Now::NotesRefused { .. }
        | core::Now::EscalationReply { .. }
        | core::Now::EscalationRefused { .. }) => now(domain, Output::Now(other)),
    }
}

#[expect(clippy::manual_map, reason = "the strict subset uses a match instead of closure-taking methods")]
#[expect(clippy::too_many_lines, reason = "one exhaustive dispatcher translates the complete hub boundary")]
fn host_out(
    domain: &mut Domain,
    _env: &Env<Limits>,
    _decision: &mut Decision<Write, Output>,
    out: &mut Queue<host::Request>,
    work: &mut Queue<Work>,
) {
    for _ in 0..out.len() {
        let request = out.pop().expect("host requests");
        match request.to_agent() {
            Ok(request) => to_agent(domain, request, work),
            Err(request) => match request {
                host::Request::Relay { run, attempt, call, delivery, tool, writes, input, deadline } => {
                    domain
                        .host_calls
                        .insert(call.clone(), crate::domain::HostCall { run, attempt, delivery })
                        .expect("host bounded its relays");
                    work.push(Work::Core(core::Event::Fleet(fleet::Event::Relay {
                        channel: Token::new(0),
                        run,
                        attempt,
                        call: fleet::Call { name: call, tool, writes, input, deadline },
                    })));
                }
                host::Request::Turn { run, attempt, turn, .. } => {
                    let body = domain.payload(Payload::Turn {
                        task: run.raw(),
                        attempt: attempt.raw(),
                        turn: turn.turn,
                        spent: turn.spent,
                        read: match turn.read {
                            Some(read) => Some(read.raw()),
                            None => None,
                        },
                        body: turn.body,
                    });
                    work.push(Work::Core(core::Event::Fleet(fleet::Event::Turn {
                        channel: Token::new(0),
                        run,
                        attempt,
                        turn: match domain.payloads.get(&body).expect("turn token") {
                            Payload::Turn { turn, .. } => *turn,
                            Payload::Answer { .. } | Payload::Word(_) | Payload::Settled(_) => {
                                unreachable!("turn token family");
                            }
                        },
                        body,
                    })));
                }
                host::Request::Answer { run, attempt, answer, .. } => {
                    let end = crate::assemble::ending(answer.ending);
                    let payload = domain.payload(Payload::Answer {
                        task: run.raw(),
                        attempt: attempt.raw(),
                        spent: answer.spent,
                        end,
                    });
                    work.push(Work::Core(core::Event::Fleet(fleet::Event::Answer {
                        channel: Token::new(0),
                        run,
                        attempt,
                        answer: fleet::Answer::Ended,
                        payload,
                    })));
                }
                host::Request::CancelRelay { call } => {
                    let mut name = None;
                    for (candidate, binding) in &domain.host_calls {
                        if binding.delivery == call {
                            name = Some(candidate.clone());
                            break;
                        }
                    }
                    if let Some(name) = name {
                        domain.host_calls.remove(&name);
                    }
                    work.push(Work::Host(host::Event::RelayCancelled { call }));
                }
                host::Request::Bounced { run, attempt, name, bounce } => {
                    work.push(Work::Core(core::Event::Fleet(fleet::Event::Bounced {
                        channel: Token::new(0),
                        run,
                        attempt,
                        name,
                        bounce: match bounce {
                            host::Bounce::TooLarge => fleet::Bounce::TooLarge,
                            host::Bounce::Full => fleet::Bounce::Full,
                            host::Bounce::Ending => fleet::Bounce::Ending,
                        },
                    })));
                }
                host::Request::Hosting { .. } => {}
                host::Request::Prepare { owner, .. } => work.push(Work::Host(host::Event::Unprepared {
                    owner,
                    failure: host::Preparation::Permanent { resource: None },
                    detail: Box::new([]),
                })),
                host::Request::Deliver { .. }
                | host::Request::Abort { .. }
                | host::Request::Save { .. }
                | host::Request::Release { .. } => unreachable!("engine hub has no workspace capability"),
                host::Request::Message { .. }
                | host::Request::Reply { .. }
                | host::Request::Start { .. }
                | host::Request::AcknowledgeAgentTurn { .. }
                | host::Request::Stop { .. }
                | host::Request::Grant { .. } => unreachable!("to_agent classified the agent capability"),
            },
        }
    }
}
fn to_agent(domain: &mut Domain, request: host::ToAgent, work: &mut Queue<Work>) {
    use smith_host_domain as smith;
    let event = match request {
        host::ToAgent::Start { owner, workspace, charter, activation, turns, answered, grants } => {
            assert!(workspace.is_none(), "engine hub has no workspace");
            let logical_run = domain.host.hosting(owner).expect("hosted agent start").run;
            let mut calls = List::with_capacity(u32::try_from(answered.len()).expect("settled calls"));
            for call in answered {
                let reply = match call.answer {
                    host::SettledAnswer::Host { error, body } => smith::SavedReply::Host { error, body },
                    host::SettledAnswer::Delivery { .. } => unreachable!("engine's charter has no workspace delivery"),
                };
                calls
                    .push(smith::AnsweredCall {
                        name: translate::named(&call.name).expect("activation-qualified name"),
                        tool: call.tool,
                        reply,
                    })
                    .expect("settled call room");
            }
            let mut translated = List::with_capacity(u32::try_from(grants.len()).expect("account grants"));
            for grant in grants {
                translated.push(translate::grant(grant)).expect("one grant per account");
            }
            smith::Event::Spawn {
                client: owner,
                start: smith::Start {
                    logical_run,
                    activation,
                    workspace: None,
                    charter,
                    transcript: if turns.is_empty() { None } else { Some(turns) },
                    answered: calls.into_boxed(),
                    directories: Box::new([]),
                    grants: translated.into_boxed(),
                },
            }
        }
        host::ToAgent::Message { agent, name, sender, words } => {
            smith::Event::Message { agent, name, label: sender, text: words }
        }
        host::ToAgent::Answer { agent, call, reply } => {
            let mut found = None;
            for (callback, name) in &domain.agent_calls {
                if callback.0 == agent && name.as_slice() == call.as_ref() {
                    found = Some(*callback);
                    break;
                }
            }
            let callback = found.expect("retained Smith callback");
            domain.agent_calls.remove(&callback);
            let reply = match reply {
                host::Reply::Relayed { answer } => crate::assemble::agent_reply(&answer),
                host::Reply::Busy => smith::Reply::Busy,
                host::Reply::Unavailable => smith::Reply::Unavailable,
                host::Reply::Withdrawn => smith::Reply::Withdrawn,
                host::Reply::Delivered(_) => unreachable!("engine's charter has no workspace delivery"),
            };
            smith::Event::Answer { agent, call: callback.1, reply }
        }
        host::ToAgent::Grant { agent, grant } => smith::Event::Grant { agent, grant: translate::grant(grant) },
        host::ToAgent::Cancel { agent } => smith::Event::Stop { agent },
        host::ToAgent::Acknowledge { agent, turn } => smith::Event::Acknowledge { agent, turn },
    };
    work.push(Work::Agent(event));
}
#[expect(clippy::too_many_lines, reason = "one exhaustive dispatcher translates this child boundary")]
fn agent_out(
    domain: &mut Domain,
    env: &Env<Limits>,
    decision: &mut Decision<Write, Output>,
    out: &mut Queue<agent::Request>,
    work: &mut Queue<Work>,
) {
    use smith_host_domain as smith;
    for _ in 0..out.len() {
        let event = match out.pop().expect("inline agent requests") {
            agent::Request::Lower { client, request } => {
                held(decision, Output::Llm { client, request });
                continue;
            }
            agent::Request::Host(request) => match request {
                smith::Request::Started { client, agent } => {
                    domain.agent_owners.insert(client, agent).expect("one agent per hub start");
                    host::FromAgent::Started { owner: client, agent }
                }
                smith::Request::Admitted { .. } => continue,
                smith::Request::Called { client, call, name, deadline, ask, .. } => {
                    let agent = *domain.agent_owners.get(&client).expect("a started agent calls");
                    let name = translate::name(name);
                    domain.agent_calls.insert((agent, call), name).expect("Smith bounded its callbacks");
                    let ask = match ask {
                        smith::Ask::Host { tool, effect, body } => host::Ask::Relay {
                            tool,
                            writes: match effect {
                                smith::Effect::Read => false,
                                smith::Effect::Write => true,
                            },
                            input: body,
                            deadline: deadline.saturating_since(env.now),
                        },
                        smith::Ask::Deliver { .. } => unreachable!("engine's charter has no workspace delivery"),
                    };
                    host::FromAgent::Called { owner: client, call: Box::from(name), ask }
                }
                smith::Request::Withdrawn { client, call } => {
                    let agent = *domain.agent_owners.get(&client).expect("a started agent withdraws");
                    let name = *domain.agent_calls.get(&(agent, call)).expect("outstanding callback");
                    host::FromAgent::Withdrawn { owner: client, call: Box::from(name) }
                }
                smith::Request::Turn { client, turn } => host::FromAgent::Turn {
                    owner: client,
                    turn: host::Turn { turn: turn.number, spent: turn.spent, read: turn.read, body: turn.body },
                },
                smith::Request::Waiting { client, .. } => host::FromAgent::Yielded { owner: client },
                smith::Request::Told { client, body } => host::FromAgent::Facts { owner: client, fact: body },
                smith::Request::Answered { client, answer } => host::FromAgent::Finished {
                    owner: client,
                    turns: answer.turns,
                    spent: answer.spent,
                    finish: crate::assemble::finish(answer.result),
                },
                smith::Request::Faulted { client, fault } => host::FromAgent::Faulted {
                    owner: client,
                    fault: match fault {
                        smith::Fault::Exited => host::AgentFailure::Exited,
                        smith::Fault::Rules | smith::Fault::TooLarge => host::AgentFailure::Rules,
                        smith::Fault::NoProgress => host::AgentFailure::NoProgress,
                        smith::Fault::WallTime => host::AgentFailure::WallTime,
                    },
                },
                smith::Request::Bounced { client, name, bounce } => host::FromAgent::Bounced {
                    owner: client,
                    name,
                    bounce: match bounce {
                        smith::Bounce::TooLarge => host::Bounce::TooLarge,
                        smith::Bounce::Full => host::Bounce::Full,
                        smith::Bounce::ReusedName | smith::Bounce::Ending => host::Bounce::Ending,
                    },
                },
                smith::Request::Gone { client, detail, .. } => {
                    domain.agent_owners.remove(&client);
                    host::FromAgent::Gone { owner: client, detail }
                }
                smith::Request::Rejected { client, account, generation } => {
                    if let Some(hosting) = domain.host.hosting(client) {
                        work.push(Work::Core(core::Event::Fleet(fleet::Event::Rejected {
                            channel: Token::new(0),
                            run: hosting.run,
                            attempt: hosting.attempt,
                            account,
                            generation,
                        })));
                    }
                    continue;
                }
                smith::Request::Exhausted { client, account, retry_after } => {
                    if let Some(hosting) = domain.host.hosting(client) {
                        work.push(Work::Core(core::Event::Fleet(fleet::Event::Exhausted {
                            channel: Token::new(0),
                            run: hosting.run,
                            attempt: hosting.attempt,
                            account,
                            retry_after,
                        })));
                    }
                    continue;
                }
                smith::Request::Spawn { .. }
                | smith::Request::Send { .. }
                | smith::Request::Read { .. }
                | smith::Request::Signal { .. }
                | smith::Request::Wait { .. }
                | smith::Request::Reap { .. } => unreachable!("inline agent has no process boundary"),
            },
        };
        work.push(Work::Host(host::Event::from_agent(event)));
    }
}
