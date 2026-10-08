//! A volatile party watch crosses the people and views children without a store decision.

use alloc::boxed::Box;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_core_views as views;
use skein_lib::{Env, List, Queue, ReplyTo, Token};

use crate::{Core, Limits, Now, Request, Requests};

/// Resolve the project of a concrete view subject from the task hub.
#[must_use]
pub fn watch_project(core: &Core, subject: views::Subject) -> u32 {
    match subject {
        views::Subject::Run { task, .. } | views::Subject::Tree { task } => match core.tasks.delegation(task.raw()) {
            Some(context) => context.project,
            None => 0,
        },
        views::Subject::Goals { project } => project,
        views::Subject::Inbox { .. } => 0,
    }
}

fn watch_views(subject: people::WatchSubject, project: u32) -> views::Subject {
    match subject {
        people::WatchSubject::Run { task, attempt } => {
            views::Subject::Run { task: Token::new(task), attempt: Token::new(attempt) }
        }
        people::WatchSubject::Tree { task } => views::Subject::Tree { task: Token::new(task) },
        people::WatchSubject::Goals => views::Subject::Goals { project },
        people::WatchSubject::Inbox { party } => views::Subject::Inbox { party },
    }
}

fn view_byte(bytes: &mut List<u8>, value: &[u8]) -> Option<()> {
    for byte in value {
        bytes.push(*byte).ok()?;
    }
    Some(())
}

fn in_tree(rows: &[tasks::ViewTask], number: u64, ancestor: u64, depth: u32) -> bool {
    let mut current = number;
    for _ in 0..=depth {
        if current == ancestor {
            return true;
        }
        let mut next = None;
        for row in rows {
            if row.number == current {
                next = match row.requester {
                    tasks::Party::Task(parent) => Some(parent),
                    tasks::Party::Person(_) | tasks::Party::Deployment { .. } => None,
                };
                break;
            }
        }
        let Some(parent) = next else { return false };
        current = parent;
    }
    false
}

fn view_snapshot(core: &Core, limits: &Limits, subject: views::Subject) -> Option<Box<[u8]>> {
    let mut bytes = List::with_capacity(limits.views.snapshot_bytes);
    match subject {
        views::Subject::Run { task, attempt } => {
            let proof = core.proofs.get(&task.raw())?;
            if proof.attempt != attempt.raw() {
                return None;
            }
            view_byte(&mut bytes, &proof.attempt.to_be_bytes())?;
            let turn = match proof.turn {
                Some(turn) => turn.turn,
                None => 0,
            };
            view_byte(&mut bytes, &turn.to_be_bytes())?;
        }
        views::Subject::Tree { task: ancestor } => {
            let rows = core.tasks.view_tasks();
            for row in &rows {
                if in_tree(&rows, row.number, ancestor.raw(), limits.tasks.depth) {
                    view_byte(&mut bytes, &row.number.to_be_bytes())?;
                    view_byte(&mut bytes, &row.phase.to_be_bytes())?;
                    view_byte(&mut bytes, &row.tracked.unwrap_or(0).to_be_bytes())?;
                }
            }
        }
        views::Subject::Goals { project } => {
            let rows = core.tasks.view_tasks();
            for row in &rows {
                if row.project == project && row.tracked.is_some() {
                    view_byte(&mut bytes, &row.number.to_be_bytes())?;
                    view_byte(&mut bytes, &row.phase.to_be_bytes())?;
                    view_byte(&mut bytes, &row.tracked.unwrap_or(0).to_be_bytes())?;
                }
            }
        }
        views::Subject::Inbox { .. } => {}
    }
    Some(bytes.into_boxed())
}

fn watch_refusal(out: &mut Queue<Request>, watcher: Token, refusal: people::Refusal) {
    out.push(Request::Now(Box::new(Now::WatchRefused { watcher, refusal })));
}

fn people_reply(out: &mut Queue<Request>, to: ReplyTo, reply: people::Reply) {
    match reply {
        people::Reply::Outcome(people::Outcome::Watching { watcher }) => {
            out.push(Request::Now(Box::new(Now::View(views::Request::Watching { watcher }))));
        }
        people::Reply::Outcome(people::Outcome::Refused(refusal)) | people::Reply::Refused(refusal) => {
            watch_refusal(out, to.into_token(), refusal);
        }
        people::Reply::Outcome(_) | people::Reply::SignedIn { .. } | people::Reply::SignedOut => {
            unreachable!("watch replies only with open or refusal")
        }
    }
}

#[expect(clippy::too_many_arguments, reason = "the keyed watch carries its caller, project and subject")]
#[expect(clippy::too_many_lines, reason = "one volatile route admits the party then opens the view")]
pub(super) fn open(
    core: &mut Core,
    env: &Env<Limits>,
    watcher: Token,
    sign_in: u64,
    key: [u8; 16],
    project: u32,
    subject: people::WatchSubject,
    ready: bool,
) -> Requests {
    let room = people::max_out(&env.limits.people).checked_mul(2).expect("bounded people outputs");
    let room = room.checked_add(views::max_out(&env.limits.views)).expect("bounded view outputs");
    let room = room.checked_add(1).expect("bounded watch outputs");
    let mut out = Queue::with_capacity(room);
    if !ready {
        watch_refusal(&mut out, watcher, people::Refusal::Busy);
        out.push(Request::Decided);
        return Requests::Out(out);
    }
    let ask = people::Ask::Watch { project, subject };
    let mut admitted = Queue::with_capacity(people::max_out(&env.limits.people));
    people::step(
        &mut core.people,
        &Env { now: env.now, wall: env.wall, limits: env.limits.people },
        people::Event::Ask { reply_to: ReplyTo::new(watcher), sign_in, key, ask },
        &mut admitted,
    );
    for _ in 0..admitted.len() {
        match admitted.pop().expect("watch admission output count") {
            people::Request::Route { request, person, project, role, ask } => {
                let people::Ask::Watch { subject, .. } = *ask else {
                    unreachable!("watch admission routes only its watch ask")
                };
                let subject = watch_views(subject, project);
                let actual = watch_project(core, subject);
                let inbox = match subject {
                    views::Subject::Inbox { .. } => true,
                    views::Subject::Run { .. } | views::Subject::Tree { .. } | views::Subject::Goals { .. } => false,
                };
                let mut opened = false;
                let outcome = if actual != project || (project == 0 && !inbox) {
                    people::Outcome::Refused(people::Refusal::Unknown)
                } else if project != 0 && !core.watch_authorized(project, role) {
                    people::Outcome::Refused(people::Refusal::Authority)
                } else if core.watching.contains_key(&watcher) || core.watching.len() >= env.limits.views.watchers {
                    people::Outcome::Refused(people::Refusal::Busy)
                } else {
                    match view_snapshot(core, &env.limits, subject) {
                        Some(snapshot) => {
                            let mut views_out = Queue::with_capacity(views::max_out(&env.limits.views));
                            views::step(
                                &mut core.views,
                                &Env { now: env.now, wall: env.wall, limits: env.limits.views },
                                views::Event::Watch { watcher, subject, snapshot },
                                &mut views_out,
                            );
                            let mut result = people::Outcome::Refused(people::Refusal::Unknown);
                            for _ in 0..views_out.len() {
                                match views_out.pop().expect("watch output count") {
                                    views::Request::Watching { watcher: opened_watcher } => {
                                        assert!(opened_watcher == watcher, "watch name is echoed");
                                        out.push(Request::Now(Box::new(Now::View(views::Request::Watching {
                                            watcher,
                                        }))));
                                        result = people::Outcome::Watching { watcher };
                                        opened = true;
                                    }
                                    request @ views::Request::Deliver { .. } => {
                                        out.push(Request::Now(Box::new(Now::View(request))));
                                    }
                                    views::Request::Refused { refusal, .. } => {
                                        result = people::Outcome::Refused(match refusal {
                                            views::Refusal::Busy => people::Refusal::Busy,
                                            views::Refusal::Oversized => people::Refusal::Limit,
                                            views::Refusal::Unknown | views::Refusal::Unfollowed => {
                                                people::Refusal::Unknown
                                            }
                                        });
                                    }
                                    views::Request::Ended { .. } => unreachable!("new watch cannot end before opening"),
                                }
                            }
                            result
                        }
                        None => people::Outcome::Refused(people::Refusal::Limit),
                    }
                };
                if opened {
                    assert!(core.watching.insert(watcher, person) == Ok(None), "watch slot checked before opening");
                }
                let mut decided = Queue::with_capacity(people::max_out(&env.limits.people));
                people::step(
                    &mut core.people,
                    &Env { now: env.now, wall: env.wall, limits: env.limits.people },
                    people::Event::Decided { request, outcome },
                    &mut decided,
                );
                while let Some(reply) = decided.pop() {
                    match reply {
                        people::Request::Reply { to, reply } => {
                            if opened {
                                match reply {
                                    people::Reply::Outcome(people::Outcome::Watching { .. }) => {}
                                    other @ (people::Reply::SignedIn { .. }
                                    | people::Reply::SignedOut
                                    | people::Reply::Outcome(_)
                                    | people::Reply::Refused(_)) => people_reply(&mut out, to, other),
                                }
                            } else {
                                people_reply(&mut out, to, reply);
                            }
                        }
                        people::Request::Save { .. }
                        | people::Request::Erase { .. }
                        | people::Request::Route { .. }
                        | people::Request::RolesApplied { .. }
                        | people::Request::RolesRefused { .. }
                        | people::Request::ServiceMade { .. }
                        | people::Request::RestoreRefused { .. } => unreachable!("watch writes nothing"),
                    }
                }
            }
            people::Request::Reply { to, reply } => people_reply(&mut out, to, reply),
            people::Request::Save { .. }
            | people::Request::Erase { .. }
            | people::Request::RolesApplied { .. }
            | people::Request::RolesRefused { .. }
            | people::Request::ServiceMade { .. }
            | people::Request::RestoreRefused { .. } => unreachable!("watch admission writes nothing"),
        }
    }
    out.push(Request::Decided);
    Requests::Out(out)
}
