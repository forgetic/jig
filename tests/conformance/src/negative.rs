//! Deliberate boundary corruptions for the outside referee (`domain/testing.md`,
//! 5–6; `domain/root.md`, 11). They change inputs and released outputs; the
//! referee still receives only independent peer observations and durable rows.
use crate::{Config, Delivery, Testing, env};
use jig_conformance::{Application, Clock, Input, Output};
use jig_core as core;
use jig_fake_store::Write;
use jig_test_connector as connector;
use jig_test_domain as root;
use skein_lib::{Token, Wall};

/// One deliberately broken root or connector, selected before construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The faithful testing adapter.
    None,
    /// A journal-held answer reaches its peer before the store applies its commit.
    EarlyOutput,
    /// An effect's outbox and its charge are submitted separately.
    SplitDecision,
    /// Journal admission rejects task writes after the task child changed.
    AdmissionAfterMutation,
    /// The root requests a later cold load before the first one.
    WrongRestart,
    /// The root tells a connector to keep an effect without asking the core.
    UncheckedEffect,
    /// The root changes an effect kind on its way to the system.
    WrongKind,
    /// A keyed connector sends copies under an idempotent system class.
    RepeatedEffect,
    /// A connector gives old facts a fresh timestamp before judging them.
    StaleVerdict,
    /// Cold reconstruction extends an existing attempt's absolute deadline.
    ForgottenDeadline,
    /// The same restored decision changes its target on a second step.
    DifferentDecision,
}

type EngineOutput = Output<root::Key, root::Record, Delivery>;

pub(crate) fn input(
    domain: &mut root::Domain,
    config: &Config,
    clock: Clock,
    mut input: Input<root::Event, root::Record>,
) -> Input<root::Event, root::Record> {
    match (&mut input, config.fault) {
        (Input::Restore(root::Record::Connector { record: connector::Record::Outbox(entry), .. }), fault) => {
            match fault {
                Fault::ForgottenDeadline => {
                    if let Some(attempt) = &mut entry.attempt {
                        attempt.deadline = Wall::from_nanos(attempt.deadline.as_nanos() + 1_000_000_000);
                    }
                }
                Fault::DifferentDecision => entry.effect.target += 1,
                Fault::None
                | Fault::EarlyOutput
                | Fault::SplitDecision
                | Fault::AdmissionAfterMutation
                | Fault::WrongRestart
                | Fault::UncheckedEffect
                | Fault::WrongKind
                | Fault::RepeatedEffect
                | Fault::StaleVerdict => {}
            }
        }
        (Input::Event(root::Event::EffectCall { effect, number, key, .. }), Fault::UncheckedEffect) => {
            let token = Token::new(99);
            root::step(
                domain,
                &env(config, clock),
                root::Event::Connector {
                    number: *number,
                    event: connector::Event::Describe { token, effect: effect.clone() },
                },
            );
            root::step(
                domain,
                &env(config, clock),
                root::Event::Connector {
                    number: *number,
                    event: connector::Event::Keep {
                        token,
                        entry: 1,
                        task: key.task,
                        key: connector::Key {
                            deployment: [u8::try_from(config.seed).expect("tiny negative seed"); 16],
                            task: key.task,
                            attempt: key.attempt,
                            completion: key.completion,
                            position: key.position,
                            purpose: effect.purpose,
                        },
                    },
                },
            );
            return Input::Event(root::Event::Connector {
                number: *number,
                event: connector::Event::Make { entry: 1 },
            });
        }
        (Input::Event(root::Event::EffectCall { .. }), Fault::StaleVerdict) => {
            root::step(
                domain,
                &env(config, clock),
                root::Event::Connector {
                    number: 2,
                    event: connector::Event::System(connector::SystemEvent::Fact {
                        resource: jig_test_connector_world::path(1, 1),
                        fact: connector::Fact {
                            state: Some(13),
                            observed: Wall::from_nanos(clock.now),
                            pending: false,
                        },
                        origin: connector::Origin::Other,
                    }),
                },
            );
        }
        _ => {}
    }
    input
}

pub(crate) fn released(
    domain: &mut root::Domain,
    config: &Config,
    clock: Clock,
    outputs: Vec<EngineOutput>,
) -> Vec<EngineOutput> {
    let mut corrupt = Vec::new();
    for output in outputs {
        let Output::Commit { number, writes } = output else {
            corrupt.push(output);
            continue;
        };
        let effect = writes.iter().any(|write| {
            matches!(
                write,
                Write::Save { row: root::Record::Connector { record: connector::Record::Outbox(_), .. }, .. }
            )
        });
        let signin = writes.iter().any(|write| {
            matches!(
                write,
                Write::Save {
                    row: root::Record::Core(core::Record::People(jig_core_people::Stored::SignIn { .. })),
                    ..
                }
            )
        });
        match config.fault {
            Fault::EarlyOutput if signin => {
                // Advance only the root's journal; its store remains unchanged.
                root::step(domain, &env(config, clock), root::Event::Committed { number });
                corrupt.extend(Testing::release(domain, &Config { fault: Fault::None, ..*config }, clock));
                corrupt.push(Output::Commit { number, writes });
            }
            Fault::SplitDecision if effect => {
                let (outbox, rest): (Vec<_>, Vec<_>) = writes.into_vec().into_iter().partition(|write| {
                    matches!(
                        write,
                        Write::Save { row: root::Record::Connector { record: connector::Record::Outbox(_), .. }, .. }
                    )
                });
                corrupt.push(Output::Commit { number, writes: outbox.into_boxed_slice() });
                corrupt.push(Output::Commit { number: number + 1, writes: rest.into_boxed_slice() });
            }
            Fault::AdmissionAfterMutation if effect => {
                let writes = writes
                    .into_vec()
                    .into_iter()
                    .filter(|write| {
                        !matches!(write, Write::Save { row: root::Record::Core(core::Record::Tasks(_)), .. })
                    })
                    .collect();
                corrupt.push(Output::Commit { number, writes });
            }
            Fault::None
            | Fault::EarlyOutput
            | Fault::SplitDecision
            | Fault::AdmissionAfterMutation
            | Fault::WrongRestart
            | Fault::UncheckedEffect
            | Fault::WrongKind
            | Fault::RepeatedEffect
            | Fault::StaleVerdict
            | Fault::ForgottenDeadline
            | Fault::DifferentDecision => {
                corrupt.push(Output::Commit { number, writes });
            }
        }
    }
    corrupt
}

pub(crate) fn delivery(fault: Fault, mut delivery: Delivery) -> Delivery {
    match (&mut delivery, fault) {
        (Delivery::Held(root::Delivery::Restart(step)), Fault::WrongRestart) => {
            *step = core::RestartStep::ReadAfresh { connector: 2 };
        }
        (
            Delivery::Held(root::Delivery::System { call: connector::SystemRequest::Apply { effect, .. }, .. }),
            Fault::WrongKind,
        ) => {
            effect.kind += 1;
        }
        _ => {}
    }
    delivery
}

pub(crate) fn repeat(system: &mut jig_test_system::System, call: &connector::SystemRequest) {
    if let connector::SystemRequest::Apply { entry, attempt, key, effect, .. } = call {
        for _ in 0..2 {
            let _ = system.answer(
                connector::SystemRequest::Apply {
                    entry: *entry,
                    attempt: *attempt,
                    key: *key,
                    effect: effect.clone(),
                    form: connector::Form::Set,
                    recovery: connector::Recovery::Idempotent,
                },
                jig_test_system::Fault::None,
            );
        }
    }
}
