//! Child entry points and their requests in the core's vocabulary
//! (`domain/engine.md`, sections 3 and 4). A root receives a whole child
//! request batch and routes it within the same decision.

use alloc::boxed::Box;
use jig_core_accounts as accounts;
use jig_core_brief as brief;
use jig_core_fleet as fleet;
use jig_core_notes as notes;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_core_views as views;
use skein_lib::{Env, List, Queue, ReplyTo, Token};

use crate::{Core, Key, Record};

/// A request from the core to its application root. Its variant fixes the
/// commit relationship before the root translates or wraps it.
#[derive(Debug)]
pub enum Request {
    /// Store mutation in the current decision.
    Write(Write),
    /// Synchronous connector handoff within this decision.
    Ask { connector: u16, ask: Ask },
    /// Output held until this decision is durable.
    Held(Box<Held>),
    /// Read answer or refusal that decided nothing.
    Now(Box<Now>),
    /// Synchronous route whose result joins this decision.
    Route(Box<Route>),
    /// All synchronous handoffs for this decision have completed.
    Decided,
}

/// A question or retained-section command for a numbered connector.
#[derive(Debug)]
pub enum Ask {
    /// Gather a section within the brief's budget.
    Gather { section: Token, budget: u32 },
    /// Cut a retained section to a smaller bound.
    CutTo { section: Token, size: u32 },
    /// Release a retained section.
    Drop { section: Token },
}

/// A child terminal that needs another core route before a decision ends.
#[derive(Debug)]
pub enum Route {
    /// A task hub terminal or handoff to route inside the same decision.
    Tasks(Box<tasks::Request>),
    /// A party request or keyed reply for the core to route onward.
    People(people::Request),
    /// A brief completion or refusal routed to the run being prepared.
    Brief(Box<brief::GatherRequest>),
    /// A host and attempt terminal routed within this decision.
    Fleet(Box<fleet::Request>),
}

/// Store mutation issued by a child of the core.
#[derive(Debug)]
pub enum Write {
    /// Save one authentic core or child record.
    Save(Record),
    /// Erase one authentic core or child key.
    Erase(Key),
}

/// Core output that must follow the decision's commit.
#[derive(Debug)]
pub enum Held {
    /// A saved word relayed to its fenced live attempt after commit.
    Relay { task: u64, attempt: u64, previous: Option<u64>, word: tasks::Word },
    /// A party's keyed terminal, released after its decision is durable.
    PeopleReply { to: ReplyTo, sign_in: Option<u64>, reply: people::Reply },
    /// Store page requested by the notes child after earlier commits.
    NotesLoad { owner: Token, range: notes::Range },
    /// A note write's accepted revision.
    NotesWritten { owner: Token, name: u64, revision: u32 },
    /// A note deletion's accepted name.
    NotesDeleted { owner: Token, name: u64 },
}

/// Core output that follows no mutation.
#[derive(Debug)]
pub enum Now {
    /// Secret-free account protocol output with no store decision.
    Account(accounts::Request),
    /// A live view observation or terminal with no durable mutation.
    View(views::Request),
    /// One page of scope index lines.
    NotesIndexed { owner: Token, lines: List<notes::Line>, more: u32 },
    /// One page of recalled entries.
    NotesRecalled { owner: Token, entries: List<notes::Entry>, more: bool },
    /// A note request refused before changing state.
    NotesRefused { owner: Token, why: notes::Refusal },
}

/// Limits of the core's child routes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    /// Historical result load slots.
    pub load_slots: u32,
    /// Named call replay and in-flight route slots.
    pub call_records: u32,
    /// The task hub's finite work.
    pub tasks: tasks::Limits,
    /// Parties and their requests.
    pub people: people::Limits,
    /// Host slots and attempts.
    pub fleet: fleet::Limits,
    /// Brief gathering.
    pub brief: brief::Limits,
    /// Secret-free account lifetimes.
    pub accounts: accounts::Limits,
    /// Scoped notes and their pages.
    pub notes: notes::Limits,
    /// Live view streams.
    pub views: views::Limits,
}

/// One event routed to a child of the core.
#[derive(Debug)]
pub enum Event {
    /// Task lifecycle or hub work.
    Tasks(tasks::Event),
    /// Party identity or request.
    People(people::Event),
    /// Host, run or attempt.
    Fleet(fleet::Event),
    /// A gathered section.
    Brief(brief::GatherEvent),
    /// An account refresh or grant.
    Account(accounts::Event),
    /// A live view.
    View(views::Event),
    /// A scoped note.
    Notes(notes::Event),
}

/// One child's complete bounded request batch, for the root to route before
/// accepting its journal decision.
#[derive(Debug)]
pub enum Requests {
    /// Fully routed core requests, each bearing its commit mark.
    Out(Queue<Request>),
}

/// One due child timer. The caller reserves journal room before a timer whose
/// route may change durable state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Timer {
    /// Task deadlines and standing work.
    Tasks,
    /// Host, call and attempt deadlines.
    Fleet,
    /// Brief gathering deadline.
    Brief,
    /// Account refresh or retry.
    Account,
    /// Live view deadline.
    View,
}

fn tag_tasks(core: &Core, mut child: Queue<tasks::Request>, room: u32) -> Requests {
    let mut out = Queue::with_capacity(room.checked_add(1).expect("task output room"));
    for _ in 0..child.len() {
        let request = match child.pop().expect("task output count") {
            tasks::Request::Erase { key } => Request::Write(Write::Erase(Key::Tasks(key))),
            tasks::Request::Relay { task, attempt, previous, word } => {
                let previous = core.relay_previous(task, previous, &word.kind);
                Request::Held(Box::new(Held::Relay { task, attempt, previous, word }))
            }
            request @ (tasks::Request::Taken { .. }
            | tasks::Request::Waiting { .. }
            | tasks::Request::WriterWaiting { .. }
            | tasks::Request::PersonProposed { .. }
            | tasks::Request::PersonProposalDecided { .. }
            | tasks::Request::RecurringDue { .. }
            | tasks::Request::ProposalRerouteNeeded { .. }
            | tasks::Request::EscalationStalled { .. }
            | tasks::Request::ProposalStalled { .. }
            | tasks::Request::ProposalDecided { .. }
            | tasks::Request::Notify { .. }
            | tasks::Request::Timer { .. }
            | tasks::Request::EndTopic { .. }
            | tasks::Request::Sent { .. }
            | tasks::Request::EscalationsInspected { .. }
            | tasks::Request::EscalationsRechecked { .. }
            | tasks::Request::EscalationNeeded { .. }
            | tasks::Request::EscalationInspected { .. }
            | tasks::Request::EscalationDecided { .. }
            | tasks::Request::Made { .. }
            | tasks::Request::Refused { .. }
            | tasks::Request::Done { .. }
            | tasks::Request::Acknowledged { .. }
            | tasks::Request::TurnAcknowledged { .. }
            | tasks::Request::Activate { .. }
            | tasks::Request::Stop { .. }
            | tasks::Request::Adopt { .. }
            | tasks::Request::Close { .. }
            | tasks::Request::Release { .. }
            | tasks::Request::Ended { .. }
            | tasks::Request::Save { .. }
            | tasks::Request::RestoreRefused { .. }) => Request::Route(Box::new(Route::Tasks(Box::new(request)))),
        };
        out.push(request);
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn tag_brief(mut child: Queue<brief::GatherRequest>, room: u32) -> Requests {
    let mut out = Queue::with_capacity(room.checked_add(1).expect("brief output room"));
    for _ in 0..child.len() {
        let request = match child.pop().expect("brief output count") {
            brief::GatherRequest::Gather { connector, section, budget } => {
                Request::Ask { connector, ask: Ask::Gather { section, budget } }
            }
            brief::GatherRequest::CutTo { connector, section, size } => {
                Request::Ask { connector, ask: Ask::CutTo { section, size } }
            }
            brief::GatherRequest::Drop { connector, section } => Request::Ask { connector, ask: Ask::Drop { section } },
            request @ (brief::GatherRequest::Complete { .. }
            | brief::GatherRequest::Failed { .. }
            | brief::GatherRequest::Refused { .. }) => Request::Route(Box::new(Route::Brief(Box::new(request)))),
        };
        out.push(request);
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn tag_fleet(mut child: Queue<fleet::Request>, room: u32) -> Requests {
    let mut out = Queue::with_capacity(room.checked_add(1).expect("fleet output room"));
    for _ in 0..child.len() {
        let request = match child.pop().expect("fleet output count") {
            request @ (fleet::Request::AssignTyped { .. }
            | fleet::Request::InboundTyped { .. }
            | fleet::Request::RelayTyped { .. }
            | fleet::Request::RelayedTyped { .. }
            | fleet::Request::DropTyped { .. }
            | fleet::Request::Assign { .. }
            | fleet::Request::Grant { .. }
            | fleet::Request::Rejected { .. }
            | fleet::Request::Exhausted { .. }
            | fleet::Request::Inbound { .. }
            | fleet::Request::Cancel { .. }
            | fleet::Request::Relayed { .. }
            | fleet::Request::Acknowledge { .. }
            | fleet::Request::Turned { .. }
            | fleet::Request::AcknowledgeTurn { .. }
            | fleet::Request::TurnBusy { .. }
            | fleet::Request::Refuse { .. }
            | fleet::Request::Placed { .. }
            | fleet::Request::Listed { .. }
            | fleet::Request::Answered { .. }
            | fleet::Request::NotStarted { .. }
            | fleet::Request::Lost { .. }
            | fleet::Request::Withdrawn { .. }
            | fleet::Request::Refused { .. }
            | fleet::Request::Relay { .. }
            | fleet::Request::Bounced { .. }
            | fleet::Request::Undelivered { .. }
            | fleet::Request::UndeliveredTyped { .. }
            | fleet::Request::Told { .. }
            | fleet::Request::Drop { .. }) => Request::Route(Box::new(Route::Fleet(Box::new(request)))),
        };
        out.push(request);
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn tag_view(mut child: Queue<views::Request>, room: u32) -> Requests {
    let mut out = Queue::with_capacity(room.checked_add(1).expect("view output room"));
    for _ in 0..child.len() {
        let request = match child.pop().expect("view output count") {
            request @ (views::Request::Watching { .. }
            | views::Request::Refused { .. }
            | views::Request::Deliver { .. }
            | views::Request::Ended { .. }) => Request::Now(Box::new(Now::View(request))),
        };
        out.push(request);
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

fn tag_account(mut child: Queue<accounts::Request>) -> Requests {
    let mut out = Queue::with_capacity(accounts::MAX_OUT.checked_add(1).expect("account output room"));
    for _ in 0..child.len() {
        let request = match child.pop().expect("account output count") {
            request @ (accounts::Request::Refresh { .. }
            | accounts::Request::Keep { .. }
            | accounts::Request::Cancel { .. }
            | accounts::Request::Granted { .. }
            | accounts::Request::Availability { .. }
            | accounts::Request::Refused { .. }
            | accounts::Request::Closed { .. }) => Request::Now(Box::new(Now::Account(request))),
        };
        out.push(request);
    }
    out.push(Request::Decided);
    Requests::Out(out)
}

/// Route one child event; the root retains the returned request batch's mark
/// when it gathers the decision.
#[expect(clippy::too_many_lines, reason = "one exhaustive dispatcher covers the core's child entry points")]
pub fn step(core: &mut Core, env: &Env<Limits>, event: Event) -> Requests {
    match event {
        Event::Tasks(event) => {
            let mut out = Queue::with_capacity(tasks::max_out(&env.limits.tasks));
            tasks::step(
                &mut core.tasks,
                &Env { now: env.now, wall: env.wall, limits: env.limits.tasks },
                event,
                &mut out,
            );
            tag_tasks(core, out, tasks::max_out(&env.limits.tasks))
        }
        Event::People(event) => {
            let mut child = Queue::with_capacity(people::max_out(&env.limits.people));
            people::step(
                &mut core.people,
                &Env { now: env.now, wall: env.wall, limits: env.limits.people },
                event,
                &mut child,
            );
            let mut out =
                Queue::with_capacity(people::max_out(&env.limits.people).checked_add(1).expect("people output room"));
            for _ in 0..child.len() {
                let request = match child.pop().expect("people output count") {
                    people::Request::Save { record } => Request::Write(Write::Save(Record::People(record))),
                    people::Request::Erase { key } => Request::Write(Write::Erase(Key::People(key))),
                    people::Request::Reply { to, reply } => {
                        Request::Held(Box::new(Held::PeopleReply { to, sign_in: core.signing_in, reply }))
                    }
                    request @ (people::Request::ServiceMade { .. }
                    | people::Request::RolesApplied { .. }
                    | people::Request::Route { .. }
                    | people::Request::RolesRefused { .. }
                    | people::Request::RestoreRefused { .. }) => Request::Route(Box::new(Route::People(request))),
                };
                out.push(request);
            }
            out.push(Request::Decided);
            Requests::Out(out)
        }
        Event::Fleet(event) => {
            let mut out = Queue::with_capacity(fleet::max_out(&env.limits.fleet));
            fleet::step(
                &mut core.fleet,
                &Env { now: env.now, wall: env.wall, limits: env.limits.fleet },
                event,
                &mut out,
            );
            tag_fleet(out, fleet::max_out(&env.limits.fleet))
        }
        Event::Brief(event) => {
            let mut out = Queue::with_capacity(brief::gather_max_out(&env.limits.brief));
            brief::gather_step(
                &mut core.brief,
                &Env { now: env.now, wall: env.wall, limits: env.limits.brief },
                event,
                &mut out,
            );
            tag_brief(out, brief::gather_max_out(&env.limits.brief))
        }
        Event::Account(event) => {
            let mut out = Queue::with_capacity(accounts::MAX_OUT);
            accounts::step(
                &mut core.accounts,
                &Env { now: env.now, wall: env.wall, limits: env.limits.accounts },
                event,
                &mut out,
            );
            tag_account(out)
        }
        Event::View(event) => {
            let mut out = Queue::with_capacity(views::max_out(&env.limits.views));
            views::step(
                &mut core.views,
                &Env { now: env.now, wall: env.wall, limits: env.limits.views },
                event,
                &mut out,
            );
            tag_view(out, views::max_out(&env.limits.views))
        }
        Event::Notes(event) => {
            let mut child = Queue::with_capacity(notes::max_out(&env.limits.notes));
            notes::step(
                &mut core.notes,
                &Env { now: env.now, wall: env.wall, limits: env.limits.notes },
                event,
                &mut child,
            );
            let mut out =
                Queue::with_capacity(notes::max_out(&env.limits.notes).checked_add(1).expect("notes output room"));
            for _ in 0..child.len() {
                let request = match child.pop().expect("notes output count") {
                    notes::Request::Save { record } => Request::Write(Write::Save(Record::Notes(record))),
                    notes::Request::Erase { key } => Request::Write(Write::Erase(Key::Notes(key))),
                    notes::Request::Load { owner, range } => Request::Held(Box::new(Held::NotesLoad { owner, range })),
                    notes::Request::Written { owner, name, revision } => {
                        Request::Held(Box::new(Held::NotesWritten { owner, name, revision }))
                    }
                    notes::Request::Deleted { owner, name } => {
                        Request::Held(Box::new(Held::NotesDeleted { owner, name }))
                    }
                    notes::Request::Indexed { owner, lines, more } => {
                        Request::Now(Box::new(Now::NotesIndexed { owner, lines, more }))
                    }
                    notes::Request::Recalled { owner, entries, more } => {
                        Request::Now(Box::new(Now::NotesRecalled { owner, entries, more }))
                    }
                    notes::Request::Refused { owner, why } => Request::Now(Box::new(Now::NotesRefused { owner, why })),
                };
                out.push(request);
            }
            out.push(Request::Decided);
            Requests::Out(out)
        }
    }
}

/// Fire one due child timer, returning its whole bounded request batch.
pub fn fire(core: &mut Core, env: &Env<Limits>, timer: Timer) -> Requests {
    match timer {
        Timer::Tasks => {
            let mut out = Queue::with_capacity(tasks::max_out(&env.limits.tasks));
            tasks::fire(&mut core.tasks, &Env { now: env.now, wall: env.wall, limits: env.limits.tasks }, &mut out);
            tag_tasks(core, out, tasks::max_out(&env.limits.tasks))
        }
        Timer::Fleet => {
            let mut out = Queue::with_capacity(fleet::max_out(&env.limits.fleet));
            fleet::fire(&mut core.fleet, &Env { now: env.now, wall: env.wall, limits: env.limits.fleet }, &mut out);
            tag_fleet(out, fleet::max_out(&env.limits.fleet))
        }
        Timer::Brief => {
            let mut out = Queue::with_capacity(brief::gather_max_out(&env.limits.brief));
            brief::gather_fire(
                &mut core.brief,
                &Env { now: env.now, wall: env.wall, limits: env.limits.brief },
                &mut out,
            );
            tag_brief(out, brief::gather_max_out(&env.limits.brief))
        }
        Timer::Account => {
            let mut out = Queue::with_capacity(accounts::MAX_OUT);
            accounts::fire(
                &mut core.accounts,
                &Env { now: env.now, wall: env.wall, limits: env.limits.accounts },
                &mut out,
            );
            tag_account(out)
        }
        Timer::View => {
            let mut out = Queue::with_capacity(views::max_out(&env.limits.views));
            views::fire(&mut core.views, &Env { now: env.now, wall: env.wall, limits: env.limits.views }, &mut out);
            tag_view(out, views::max_out(&env.limits.views))
        }
    }
}
