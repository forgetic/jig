//! Assemble outputs in the decision that owns them. A copying application
//! replaces its tools and connector results; charter conversion stays shared
//! (domain/root.md, 7; domain/hosts.md, 8).
use crate::domain::Work;
use crate::{Domain, Limits};
use alloc::boxed::Box;
use jig_charter as charter;
use jig_core as core;
use jig_core_accounts as accounts;
use jig_core_brief as brief;
use jig_core_tasks as tasks;
use jig_host as host;
use skein_lib::{Decimal, Env, List, Queue, Reader, ReplyTo, Token, Writer};

pub(crate) fn brief(order: Box<[brief::GatherPlaced]>) -> Box<[charter::Section]> {
    let mut sections = List::with_capacity(u32::try_from(order.len()).expect("bounded sections"));
    for placed in order {
        let section = match placed {
            brief::GatherPlaced::Core { kind, text } => charter::Section { title: Box::from(title(kind)), text },
            brief::GatherPlaced::CoreMissing { kind, .. } => {
                charter::Section { title: Box::from(title(kind)), text: Box::from(&b"[missing]"[..]) }
            }
            brief::GatherPlaced::Missing { .. } => {
                charter::Section { title: Box::from(&b"Context"[..]), text: Box::from(&b"[missing]"[..]) }
            }
            brief::GatherPlaced::Connector { .. } => unreachable!("ops offers no connector brief sections"),
        };
        sections.push(section).expect("one section per placement");
    }
    sections.into_boxed()
}
const fn title(kind: brief::Core) -> &'static [u8] {
    match kind {
        brief::Core::Task => b"Task",
        brief::Core::Lineage => b"Requesters",
        brief::Core::Inbox => b"Inbox",
        brief::Core::Results => b"Results",
        brief::Core::Plan => b"Plan",
        brief::Core::Attempts => b"Attempts",
        brief::Core::Calls => b"Calls",
        brief::Core::Waiting => b"Waiting",
        brief::Core::NotesIndex => b"Notes",
        brief::Core::TranscriptTail => b"Conversation",
    }
}
#[expect(clippy::too_many_arguments, reason = "assemble the owners parts without a shared sibling type")]
pub(crate) fn assignment(
    domain: &Domain,
    env: &Env<Limits>,
    task: u64,
    attempt: u64,
    run: core::RunCharter,
    sections: Box<[charter::Section]>,
    transcript: Box<[Box<[u8]>]>,
    grant: accounts::Grant,
) -> Option<host::Assignment> {
    let policy = run.policy;
    let mut models = List::with_capacity(u32::try_from(policy.alternatives.len()).ok()?);
    for value in policy.alternatives {
        models.push(model(value)).ok()?;
    }
    let value = charter::charter(
        charter::Charter {
            instructions: policy.instructions,
            tools: crate::translate::tools(policy.call_timeout),
            wait: true,
            agents: policy.agents,
            workspace: charter::WorkspaceTools { inspect: false, modify: false, shell: false },
            conventions: None,
            contract: contract(run.contract),
            budget: charter::Budget { turns: policy.turns, spend: run.budget, time: policy.time },
            model: model(policy.model),
            models: models.into_boxed(),
            waiting: policy.waiting,
            resumes: policy.resume,
        },
        sections,
        !transcript.is_empty(),
    )?;
    let encoded = charter::encode(value, &domain.charter_endpoints, &env.limits.agent.charter)?;
    let mut answered = List::with_capacity(env.limits.core.call_records);
    for value in domain.core.settled_calls(task, attempt) {
        let answer = match value.answer {
            core::SettledAnswer::Host { error, body } => host::SettledAnswer::Host { error, body },
            core::SettledAnswer::Delivery { outcome, evidence } => {
                host::SettledAnswer::Delivery { outcome: delivery(outcome), evidence }
            }
        };
        answered.push(host::AnsweredCall { name: value.name, tool: value.tool, answer }).ok()?;
    }
    Some(host::Assignment {
        assignment: host::RunAssignment {
            run: Token::new(task),
            attempt: Token::new(attempt),
            workspace: None,
            save: false,
            charter: encoded,
            grants: Box::new([host::Grant {
                account: grant.account,
                generation: grant.generation,
                valid: grant.valid,
            }]),
        },
        turns: transcript,
        answered: answered.into_boxed(),
    })
}
fn model(value: core::Model) -> charter::Model {
    charter::Model {
        prices: charter::Prices {
            input: value.input_price,
            cached: value.cached_price,
            output: value.output_price,
            unit: value.price_unit,
        },
        dialect: value.dialect,
        account: value.account,
        endpoint: value.endpoint,
        name: value.name,
        max_tokens: value.max_tokens,
    }
}
fn contract(value: tasks::Contract) -> charter::Contract {
    match value {
        tasks::Contract::Report { words } => charter::Contract {
            report: Some(charter::TextRule { max: words, fields: Box::new([]) }),
            failure: Some(charter::TextRule { max: words, fields: Box::new([]) }),
            verdicts: Box::new([]),
            change: None,
        },
        tasks::Contract::Verdict { choices } => {
            let mut verdicts = List::with_capacity(u32::try_from(choices.len()).expect("verdict choices"));
            for choice in choices {
                verdicts
                    .push(charter::VerdictRule {
                        name: Box::from(Decimal::of(u64::from(choice.code)).as_bytes()),
                        text_max: choice.words,
                        fields: Box::new([]),
                        items: charter::Items { min: 0, max: 0, kinds: Box::new([]) },
                    })
                    .expect("one verdict rule");
            }
            charter::Contract {
                report: None,
                failure: Some(charter::TextRule { max: 1024, fields: Box::new([]) }),
                verdicts: verdicts.into_boxed(),
                change: None,
            }
        }
        tasks::Contract::Change { words, .. } => charter::Contract {
            report: None,
            failure: Some(charter::TextRule { max: words, fields: Box::new([]) }),
            verdicts: Box::new([]),
            change: Some(charter::ChangeRule {
                checks_must_pass: true,
                fields: Box::new([
                    charter::FieldRule { name: Box::from(&b"title"[..]), max: words },
                    charter::FieldRule { name: Box::from(&b"body"[..]), max: words },
                ]),
            }),
        },
    }
}
const fn delivery(value: core::DeliveryOutcome) -> host::DeliveryOutcome {
    match value {
        core::DeliveryOutcome::Delivered => host::DeliveryOutcome::Delivered,
        core::DeliveryOutcome::Nothing => host::DeliveryOutcome::Nothing,
        core::DeliveryOutcome::Stale => host::DeliveryOutcome::Stale,
        core::DeliveryOutcome::Refused | core::DeliveryOutcome::Invalid => host::DeliveryOutcome::Refused,
        core::DeliveryOutcome::Failed => host::DeliveryOutcome::Failed,
    }
}
/// Preserve the task-owned message kind when translating its opaque words.
pub(crate) fn inbox_words(word: &tasks::Word) -> Box<[u8]> {
    let (label, number) = match word.kind {
        tasks::MessageKind::Result(_) => (
            b"Result from task ".as_slice(),
            match word.from {
                tasks::Party::Task(task) => task,
                tasks::Party::Person(person) => person,
                tasks::Party::Deployment { .. } => 0,
            },
        ),
        tasks::MessageKind::ProposalDecision { proposal, accepted } => {
            (if accepted { b"Proposal accepted ".as_slice() } else { b"Proposal rejected ".as_slice() }, proposal)
        }
        tasks::MessageKind::Words
        | tasks::MessageKind::Question
        | tasks::MessageKind::Answer { .. }
        | tasks::MessageKind::Notice { .. }
        | tasks::MessageKind::News { .. }
        | tasks::MessageKind::Timer { .. }
        | tasks::MessageKind::Amendment { .. }
        | tasks::MessageKind::Proposal { .. }
        | tasks::MessageKind::Escalation { .. } => return word.words.clone(),
    };
    let number = Decimal::of(number);
    let bytes = label.len().checked_add(number.as_bytes().len()).expect("bounded message label");
    let bytes = bytes.checked_add(2).expect("bounded message separator");
    let bytes = bytes.checked_add(word.words.len()).expect("bounded message envelope");
    let mut writer = Writer::new(bytes);
    writer.put(label).expect("measured label");
    writer.put(number.as_bytes()).expect("measured source");
    writer.put(b":\n").expect("measured separator");
    writer.put(&word.words).expect("measured words");
    writer.finish()
}

pub(crate) fn sender(value: tasks::Party) -> Box<[u8]> {
    match value {
        tasks::Party::Task(number) | tasks::Party::Person(number) => Box::from(Decimal::of(number).as_bytes()),
        tasks::Party::Deployment { .. } => Box::from(&b"deployment"[..]),
    }
}
pub(crate) fn host_answer(value: core::SettledAnswer) -> Box<[u8]> {
    match value {
        core::SettledAnswer::Host { error, body } => {
            // The hub carries opaque bytes; keep the result mark inside that
            // envelope so the live answer agrees with its restored call row.
            let mut writer = Writer::new(body.len().checked_add(1).expect("reply envelope"));
            writer.put(&[u8::from(error)]).expect("reply mark");
            writer.put(&body).expect("bounded reply");
            writer.finish()
        }
        core::SettledAnswer::Delivery { .. } => unreachable!("engine charter has no delivery tool"),
    }
}

pub(crate) fn agent_reply(bytes: &[u8]) -> smith_host_domain::Reply {
    let error = match bytes.first() {
        Some(0) => false,
        Some(1) => true,
        Some(_) | None => return smith_host_domain::Reply::Unavailable,
    };
    smith_host_domain::Reply::Host { error, body: Box::from(bytes.get(1..).expect("reply envelope")) }
}
pub(crate) fn ending(value: host::Ending) -> tasks::End {
    match value {
        host::Ending::Refused(_) => tasks::End::Refused,
        host::Ending::Parked { .. } => tasks::End::Parked,
        host::Ending::Failed { failure, .. } => tasks::End::Failed(match failure {
            host::Failure::Unprepared(host::Preparation::Transient) => tasks::Class::Transient,
            host::Failure::Unprepared(host::Preparation::Permanent { .. }) => tasks::Class::Permanent,
            host::Failure::Run(_) => tasks::Class::Run,
            host::Failure::Agent(_) | host::Failure::Cancelled(_) => tasks::Class::Agent,
        }),
        host::Ending::Ended { outcome, .. } => match decode_result(&outcome) {
            Some(result) => tasks::End::Finished { result, cancel_delegates: false },
            None => tasks::End::Failed(tasks::Class::Invalid),
        },
    }
}
fn decode_result(bytes: &[u8]) -> Option<tasks::TaskResult> {
    let mut reader = Reader::new(bytes);
    let value = smith_charter::RunResult::decode(&smith_charter::v1::CEILINGS, &mut reader).ok()?;
    match value.form() {
        smith_charter::Form::Report => Some(tasks::TaskResult::Report { words: Box::from(value.text()) }),
        smith_charter::Form::Failure => Some(tasks::TaskResult::Failure { reason: Box::from(value.text()) }),
        smith_charter::Form::Verdict => {
            if !value.items().is_empty() {
                return None;
            }
            let code = crate::translate::decimal(value.label().as_deref()?)?;
            Some(tasks::TaskResult::Verdict { code: u32::try_from(code).ok()?, words: Box::from(value.text()) })
        }
        smith_charter::Form::Change => None,
    }
}
pub(crate) fn finish(value: smith_host_domain::RunResult) -> host::Finish {
    use smith_host_domain as smith;
    match value {
        smith::RunResult::Accepted { outcome } => host::Finish::Ended { outcome },
        smith::RunResult::Parked => host::Finish::Parked,
        smith::RunResult::Failed { failure } => host::Finish::Failed {
            failure: match failure {
                smith::RunFailure::Transcript(_)
                | smith::RunFailure::Model(
                    smith::ModelFault::Completion { .. }
                    | smith::ModelFault::Provider
                    | smith::ModelFault::ContextFull
                    | smith::ModelFault::Refused
                    | smith::ModelFault::Truncated
                    | smith::ModelFault::Malformed,
                ) => host::RunFailure::Model,
                smith::RunFailure::Model(smith::ModelFault::Exhausted) => host::RunFailure::Exhausted,
                smith::RunFailure::Budget(_) => host::RunFailure::Budget,
                smith::RunFailure::Policy(_) => host::RunFailure::Policy,
                smith::RunFailure::Cancelled => host::RunFailure::Cancelled,
                smith::RunFailure::Stale => host::RunFailure::Stale,
            },
        },
        smith::RunResult::Refused { refusal } => host::Finish::Failed {
            failure: match refusal {
                smith::Refusal::Busy => host::RunFailure::Model,
                smith::Refusal::Invalid(_) => host::RunFailure::Policy,
            },
        },
    }
}
pub(crate) fn answer(domain: &Domain, key: core::CallKey, part: core::CallPart) -> core::SettledCall {
    let (name, tool) = domain.calls.get(&key).expect("attested call envelope");
    core::SettledCall {
        serial: 0,
        name: name.clone(),
        tool: tool.clone(),
        answer: core::SettledAnswer::Host { error: false, body: render(part) },
    }
}
fn render(part: core::CallPart) -> Box<[u8]> {
    let (status, number) = match part {
        core::CallPart::Effect { entry, .. } => (b"effect".as_slice(), Some(entry)),
        core::CallPart::Connector { .. } => (b"connector".as_slice(), None),
        core::CallPart::Proposed { proposal } => (b"proposed".as_slice(), Some(proposal)),
        core::CallPart::ProposalDecided { proposal, .. } => (b"decided".as_slice(), Some(proposal)),
        core::CallPart::Sent { message } => (b"sent".as_slice(), Some(message)),
        core::CallPart::Subscribed { subscription } => (b"subscribed".as_slice(), Some(subscription)),
        core::CallPart::Delegated(numbers) => return delegated(&numbers),
        core::CallPart::NoteWritten { name, .. } => (b"noted".as_slice(), Some(name)),
        core::CallPart::Controlled
        | core::CallPart::Introduced
        | core::CallPart::Unsubscribed
        | core::CallPart::EscalationDecided { .. } => (b"done".as_slice(), None),
        core::CallPart::EffectDenied { .. }
        | core::CallPart::ToolDenied { .. }
        | core::CallPart::EscalationRefused(_)
        | core::CallPart::ProposalRefused(_)
        | core::CallPart::ControlRefused(_)
        | core::CallPart::ControlDenied { .. }
        | core::CallPart::MessageRefused(_)
        | core::CallPart::SubscriptionRefused(_)
        | core::CallPart::DelegationDenied { .. }
        | core::CallPart::DelegationRefused(_)
        | core::CallPart::NoteRefused(_) => (b"refused".as_slice(), None),
        core::CallPart::NoteRecalled { entries, more } => return notes(&entries, more),
        core::CallPart::Unavailable => (b"unavailable".as_slice(), None),
    };
    let suffix = match number {
        Some(number) => b",\"number\":".len().checked_add(Decimal::of(number).as_bytes().len()).expect("number field"),
        None => 0,
    };
    let bytes = b"{\"status\":\""
        .len()
        .checked_add(status.len())
        .expect("status bytes")
        .checked_add(2)
        .expect("closing quote and brace")
        .checked_add(suffix)
        .expect("answer bytes");
    let mut writer = Writer::new(bytes);
    writer.put(b"{\"status\":\"").expect("status prefix");
    writer.put(status).expect("bounded status");
    writer.put(b"\"").expect("quote");
    if let Some(number) = number {
        writer.put(b",\"number\":").expect("number field");
        writer.put(Decimal::of(number).as_bytes()).expect("decimal");
    }
    writer.put(b"}").expect("object end");
    writer.finish()
}

fn delegated(numbers: &[u64]) -> Box<[u8]> {
    let prefix = b"{\"status\":\"delegated\",\"numbers\":[";
    let mut bytes = prefix.len().checked_add(2).expect("array and object closing");
    for (index, number) in numbers.iter().enumerate() {
        bytes = bytes.checked_add(Decimal::of(*number).as_bytes().len()).expect("delegate identity");
        if index > 0 {
            bytes = bytes.checked_add(1).expect("comma");
        }
    }
    let mut writer = Writer::new(bytes);
    writer.put(prefix).expect("delegated prefix");
    for (index, number) in numbers.iter().enumerate() {
        if index > 0 {
            writer.put(b",").expect("comma");
        }
        writer.put(Decimal::of(*number).as_bytes()).expect("delegate identity");
    }
    writer.put(b"]}").expect("array and object closing");
    writer.finish()
}

fn notes(entries: &[jig_core_notes::Entry], more: bool) -> Box<[u8]> {
    let tail = if more { b"\nMore notes remain.".as_slice() } else { b"\nEnd of notes.".as_slice() };
    let mut bytes = b"Notes".len().checked_add(tail.len()).expect("page framing");
    for entry in entries {
        bytes = bytes
            .checked_add(b"\nNote ".len())
            .expect("note heading")
            .checked_add(Decimal::of(entry.name).as_bytes().len())
            .expect("note identity")
            .checked_add(b" revision ".len())
            .expect("revision heading")
            .checked_add(Decimal::of(u64::from(entry.revision)).as_bytes().len())
            .expect("revision")
            .checked_add(2)
            .expect("line breaks")
            .checked_add(entry.description.len())
            .expect("description")
            .checked_add(entry.body.len())
            .expect("body")
            .checked_add(b"\nReferences:".len())
            .expect("references heading");
        for reference in &entry.references {
            bytes = bytes
                .checked_add(1)
                .expect("reference separator")
                .checked_add(Decimal::of(*reference).as_bytes().len())
                .expect("reference");
        }
    }
    let mut writer = Writer::new(bytes);
    writer.put(b"Notes").expect("page heading");
    for entry in entries {
        writer.put(b"\nNote ").expect("note heading");
        writer.put(Decimal::of(entry.name).as_bytes()).expect("note identity");
        writer.put(b" revision ").expect("revision heading");
        writer.put(Decimal::of(u64::from(entry.revision)).as_bytes()).expect("revision");
        writer.put(b"\n").expect("line break");
        writer.put(&entry.description).expect("description");
        writer.put(b"\n").expect("line break");
        writer.put(&entry.body).expect("body");
        writer.put(b"\nReferences:").expect("references heading");
        for reference in &entry.references {
            writer.put(b" ").expect("reference separator");
            writer.put(Decimal::of(*reference).as_bytes()).expect("reference");
        }
    }
    writer.put(tail).expect("page tail");
    writer.finish()
}
pub(crate) fn call(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    run: Token,
    attempt: Token,
    call: jig_core_fleet::Call,
    work: &mut Queue<Work>,
) {
    let Some(name) = crate::translate::named(&call.name) else {
        read_answer(
            domain,
            crate::domain::ReadCall { to: to.into_token(), name: call.name, tool: call.tool },
            true,
            Box::from(&b"unavailable"[..]),
            work,
        );
        return;
    };
    let key =
        core::CallKey { task: run.raw(), attempt: attempt.raw(), completion: name.completion, position: name.position };
    if !call.writes {
        let binding = crate::domain::ReadCall { to: to.into_token(), name: call.name, tool: call.tool.clone() };
        match crate::translate::decode(&call.tool, &call.input) {
            Ok(crate::translate::Decoded::Read(read)) => {
                let description = crate::translate::read_description(domain.numbers.observability, &read);
                let mut findings = Queue::with_capacity(
                    jig_core_authority::max_out(&env.limits.core.authority).expect("authority findings"),
                );
                if domain.core.connector_read_admit(run.raw(), description, &mut findings)
                    != jig_core_authority::Answer::Allow
                    || domain.read_calls.len() == domain.read_calls.capacity()
                {
                    read_answer(domain, binding, true, Box::from(&b"refused"[..]), work);
                } else {
                    let token = domain.token();
                    domain.read_calls.insert(token, binding).expect("read continuation admitted");
                    work.push(Work::Observability(jig_ops_domain_observability::Event::Read { token, read }));
                }
            }
            Ok(
                crate::translate::Decoded::Action(_)
                | crate::translate::Decoded::Delegate(_)
                | crate::translate::Decoded::Effect { .. },
            )
            | Err(_) => read_answer(domain, binding, true, Box::from(&b"unavailable"[..]), work),
        }
        return;
    }
    domain.calls.insert(key, (call.name, call.tool.clone())).expect("core admitted this named call");
    match crate::translate::decode(&call.tool, &call.input) {
        Ok(crate::translate::Decoded::Read(_)) => {
            work.push(Work::Core(core::Event::NamedAnswer { to, key, part: core::CallPart::Unavailable }));
        }
        Ok(crate::translate::Decoded::Action(action)) => {
            work.push(Work::Core(core::Event::NamedAction { to, key, action }));
        }
        Ok(crate::translate::Decoded::Delegate(batch)) => {
            work.push(Work::Core(core::Event::DelegateValidated { to, key, batch, stubs: Box::new([]) }));
        }
        Ok(crate::translate::Decoded::Effect { effect, reason }) => work.push(Work::Event(crate::Event::Effect {
            to,
            key,
            effect,
            deadline: skein_lib::Wall::from_nanos(env.wall.as_nanos().saturating_add(call.deadline.as_nanos())),
            proposal: reason,
        })),
        Err(_) => work.push(Work::Core(core::Event::NamedAnswer { to, key, part: core::CallPart::Unavailable })),
    }
}

/// Take the owner's answer once, preserving the core's held or now mark.
pub(crate) fn relayed(
    domain: &mut Domain,
    run: Token,
    attempt: Token,
    call: Box<[u8]>,
    answer: Token,
) -> Option<crate::Released> {
    let value = match domain.payloads.remove(&answer).expect("settled call token") {
        crate::boundary::Payload::Settled(value) => value,
        crate::boundary::Payload::Turn { .. }
        | crate::boundary::Payload::Answer { .. }
        | crate::boundary::Payload::Word(_) => unreachable!("settled call family"),
    };
    let binding = domain.host_calls.remove(&call)?;
    assert!(binding.run == run && binding.attempt == attempt, "hub relay fence");
    Some(crate::Released::Host(host::Event::Relayed {
        run,
        attempt,
        call: binding.delivery,
        answer: host_answer(value.answer),
    }))
}

pub(crate) fn read_answer(
    domain: &mut Domain,
    call: crate::domain::ReadCall,
    error: bool,
    body: Box<[u8]>,
    work: &mut Queue<Work>,
) {
    let answer = domain.payload(crate::boundary::Payload::Settled(core::SettledCall {
        serial: 0,
        name: call.name,
        tool: call.tool,
        answer: core::SettledAnswer::Host { error, body },
    }));
    work.push(Work::Core(core::Event::Fleet(jig_core_fleet::Event::Relayed { to: ReplyTo::new(call.to), answer })));
}
