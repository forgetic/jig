//! Copy the core-selected script. A connector reports when it has completed
//! reads or settlement; the root never advances it by guessing (domain/root.md, 8).
use crate::domain::{Work, held};
use crate::{Domain, Limits, Output, Range, Record, Store, Write};
use jig_core as core;
use jig_core_notes as notes;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_ops_domain_infrastructure as infrastructure;
use jig_ops_domain_observability as observability;
use skein_lib::{Decision, Env, Queue};

pub(crate) fn route(
    domain: &mut Domain,
    _env: &Env<Limits>,
    decision: &mut Decision<Write, Output>,
    request: core::RestartRequest,
    work: &mut Queue<Work>,
) {
    match request {
        core::RestartRequest::Idle | core::RestartRequest::Refused { .. } => {}
        core::RestartRequest::Step(step) => match step {
            core::RestartStep::LoadCore => held(decision, Output::Load { step, range: Range::Core }),
            core::RestartStep::RestoreConnector { connector } => {
                let range = if connector == domain.numbers.infrastructure {
                    Range::Infrastructure
                } else if connector == domain.numbers.observability {
                    Range::Observability
                } else {
                    let _request = domain.core.restart_refuse();
                    return;
                };
                held(decision, Output::Load { step, range });
            }
            core::RestartStep::AdoptRuns => {
                work.push(Work::Core(core::Event::Tasks(tasks::Event::Restored)));
                work.push(Work::AdoptClaims);
            }
            core::RestartStep::ReadAfresh { connector } => {
                if connector == domain.numbers.infrastructure {
                    work.push(Work::Infrastructure(infrastructure::Event::ReadAfresh));
                } else if connector == domain.numbers.observability {
                    work.push(Work::Observability(observability::Event::ReadAfresh));
                } else {
                    let _request = domain.core.restart_refuse();
                }
            }
            core::RestartStep::SettleOutbox { connector } => {
                if connector == domain.numbers.infrastructure {
                    work.push(Work::Infrastructure(infrastructure::Event::Restart));
                } else if connector == domain.numbers.observability {
                    work.push(Work::Observability(observability::Event::Restart));
                } else {
                    let _request = domain.core.restart_refuse();
                }
            }
            core::RestartStep::Open => {
                let _request = domain.core.restart_done(step);
            }
        },
    }
}

pub(crate) fn store(
    domain: &mut Domain,
    env: &Env<Limits>,
    _decision: &mut Decision<Write, Output>,
    store: Store,
    work: &mut Queue<Work>,
) {
    match store {
        Store::Committed { .. } | Store::Failed { .. } => unreachable!("store terminals belong to the journal"),
        Store::Loaded(event) => work.push(Work::Core(event)),
        Store::Restored(step) => {
            if step == core::RestartStep::LoadCore {
                // The completed core range is also the people's restoration
                // terminal. Retain it when copying this cold-load adapter.
                work.push(Work::Core(core::Event::People(people::Event::Restored)));
            }
            let request = domain.core.restart_done(step);
            work.push(Work::Restart(request));
        }
        Store::Transcript { task, rows, done } => {
            // A message can invalidate preparation while this store read is
            // in flight. Its old answer belongs to no current preparation.
            if !domain.core.transcripts.contains_key(&task) {
                return;
            }
            for row in rows {
                if !domain.core.append_transcript(task, row) {
                    work.push(Work::Core(core::Event::PreparationFailed { task }));
                    return;
                }
            }
            if done {
                work.push(Work::Core(core::Event::StartBrief { task }));
            }
        }
        Store::Restore(record) => {
            let valid = match record {
                Record::Core(core::Record::Core(row)) => match domain.core.restore_core(row, &env.limits.core) {
                    core::Restored::Deployment { commits } => {
                        domain.restored_commits = Some(commits);
                        true
                    }
                    core::Restored::Live | core::Restored::Archive => true,
                    core::Restored::Rejected => false,
                },
                Record::Core(core::Record::Tasks(tasks::Stored::Milestone(_))) => true,
                Record::Core(core::Record::Tasks(row)) => {
                    if domain.core.restore_task_row(&row) {
                        let mut out = Queue::with_capacity(tasks::max_out(&env.limits.core.tasks));
                        tasks::step(
                            &mut domain.core.tasks,
                            &Env { now: env.now, wall: env.wall, limits: env.limits.core.tasks },
                            tasks::Event::Restore { record: row },
                            &mut out,
                        );
                        out.is_empty()
                    } else {
                        false
                    }
                }
                Record::Core(core::Record::People(people::Stored::Policy { project, value })) => {
                    domain.core.restore_policy(&env.limits.core, project, value)
                }
                Record::Core(core::Record::People(row)) => {
                    let mut out = Queue::with_capacity(people::max_out(&env.limits.core.people));
                    people::step(
                        &mut domain.core.people,
                        &Env { now: env.now, wall: env.wall, limits: env.limits.core.people },
                        people::Event::Restore { record: row },
                        &mut out,
                    );
                    out.is_empty()
                }
                Record::Core(core::Record::Notes(row)) => {
                    work.push(Work::Core(core::Event::Notes(notes::Event::Restore { record: row })));
                    true
                }
                Record::Infrastructure(record) => {
                    let valid = restore_roles(domain, env, &record);
                    if valid {
                        work.push(Work::Infrastructure(infrastructure::Event::Restore { record }));
                    }
                    valid
                }
                Record::Observability(record) => {
                    work.push(Work::Observability(observability::Event::Restore { record }));
                    true
                }
            };
            if !valid {
                let _request = domain.core.restart_refuse();
            }
        }
    }
}

/// Preserve connector-owned project adoption before the core admits live claims.
fn restore_roles(domain: &mut Domain, env: &Env<Limits>, record: &infrastructure::Record) -> bool {
    let mut valid = true;
    match record {
        infrastructure::Record::Rely { project, resources, .. } => {
            for named in resources {
                valid &= domain.core.restore_resource_role(
                    &Env { now: env.now, wall: env.wall, limits: env.limits.core },
                    *project,
                    tasks::Name { connector: domain.numbers.infrastructure, path: named.resource.segments() },
                    crate::translate::resource_role(named.role),
                );
            }
        }
        infrastructure::Record::Procedure(_)
        | infrastructure::Record::Proposal { .. }
        | infrastructure::Record::Outbox(_)
        | infrastructure::Record::Made { .. } => {}
    }
    valid
}
