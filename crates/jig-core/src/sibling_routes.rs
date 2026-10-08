//! Routes between the core's children (domain/engine.md, 4.4). These
//! translations use only the children's vocabularies; the application root
//! carries their events and handles the resulting store and host requests.

use alloc::boxed::Box;
use jig_core_authority as authority;
use jig_core_fleet as fleet;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_core_views as views;
use skein_lib::{Env, List, Queue, Token, Wall};

use crate::{CallKey, Core, Family, GoalRoute, Limits, PersonProposalRoute, RoutedCall, fresh};

/// Destination of an accepted message after the task child saved it.
#[derive(Debug)]
pub enum SentRoute {
    /// A named host call receives its durable answer.
    Call { key: CallKey, message: u64 },
    /// A party's keyed request receives its answer from the people child.
    Person(Box<people::Event>),
}

/// The next core route after the task hub admitted a numbered batch.
#[derive(Debug)]
pub enum MadeRoute {
    /// Internal connector or deployment admission needs no keyed reply.
    Internal,
    /// The accepted goal needs its semantic proposal decision.
    Tasks(tasks::Event),
    /// A person proposal needs its semantic task decision.
    PersonProposal(tasks::Event),
    /// A person request receives its keyed result.
    Person(Box<people::Event>),
    /// A named run call receives its batch, after remembering historical stubs.
    Delegated { key: CallKey, tasks: Box<[u64]>, stubs: Box<[tasks::Stub]> },
}

impl Core {
    /// Project a person's durable goal proposal into the party inbox.
    #[must_use]
    pub fn person_proposal_entries(&self, row: &tasks::PersonProposal) -> Box<[people::Entry]> {
        let mut entries = List::with_capacity(self.authority.limits().roles.max(1));
        match &row.state {
            tasks::PersonProposalState::Pending { since } => {
                if let Some(project) = self.authority.policy(row.project) {
                    for policy in &project.roles {
                        if !policy.decides.allows(authority::ProposalKind::Batch) {
                            continue;
                        }
                        entries
                            .push(people::Entry {
                                task: row.goal.number,
                                project: row.project,
                                whom: people::Whom::Role { project: row.project, role: policy.number },
                                kind: people::EntryKind::Proposal { number: row.number },
                                at: *since,
                            })
                            .expect("one entry per policy role");
                    }
                }
            }
            tasks::PersonProposalState::Rejected { message, at, .. } => {
                entries
                    .push(people::Entry {
                        task: row.goal.number,
                        project: row.project,
                        whom: people::Whom::Person(row.proposer),
                        kind: people::EntryKind::Reply { message: *message },
                        at: *at,
                    })
                    .expect("one rejection reply");
            }
            tasks::PersonProposalState::Accepted { .. } => {}
        }
        entries.into_boxed()
    }

    /// Close the party's volatile watch when the view child ends it.
    pub fn watch_closed(&mut self, env: &Env<Limits>, watcher: Token) {
        self.watching.remove(&watcher);
        let mut out = Queue::with_capacity(people::max_out(&env.limits.people));
        people::step(
            &mut self.people,
            &Env { now: env.now, wall: env.wall, limits: env.limits.people },
            people::Event::WatchClosed { watcher },
            &mut out,
        );
        assert!(out.is_empty(), "closing a live watch writes nothing");
    }

    /// A lost channel changes fleet topology without an external output.
    pub fn lost_channel(&mut self, env: &Env<Limits>, channel: Token) {
        let mut out = Queue::with_capacity(fleet::max_out(&env.limits.fleet));
        fleet::step(
            &mut self.fleet,
            &Env { now: env.now, wall: env.wall, limits: env.limits.fleet },
            fleet::Event::Lost { channel },
            &mut out,
        );
        assert!(out.is_empty(), "loss changes topology and deadlines without child effects");
    }

    /// Route a restored task's current claim to views and the fleet, holding
    /// host adoption until the application's live-range load finishes.
    pub fn adopt(&mut self, env: &Env<Limits>, task: u64, attempt: u64, kept: u32) {
        assert!(self.unreported_restored.insert(task, attempt) == Ok(None), "one restored claim per task");
        let mut view_out = Queue::with_capacity(views::max_out(&env.limits.views));
        views::step(
            &mut self.views,
            &Env { now: env.now, wall: env.wall, limits: env.limits.views },
            views::Event::Started { task: Token::new(task), attempt: Token::new(attempt) },
            &mut view_out,
        );
        assert!(view_out.is_empty(), "restored run following has no external effect");
        self.adopted.push(fleet::Event::Adopt {
            kind: fleet::HostKind::Worker,
            worked: kept > 0,
            reply_to: skein_lib::ReplyTo::new(Token::new(task)),
            run: Token::new(task),
            attempt: Token::new(attempt),
            kept,
        });
    }

    /// Translate the task hub's stop into the fleet's fenced cancellation.
    #[must_use]
    pub fn stop_run(task: u64, attempt: u64) -> fleet::Event {
        fleet::Event::Cancel { run: Token::new(task), attempt: Token::new(attempt) }
    }

    /// Route a refused goal flight back to the requesting party. The hub's
    /// structural reasons are translated at the core's sibling boundary.
    pub fn goal_refused(&mut self, request: Token, why: tasks::Refusal) -> Option<people::Event> {
        self.goal_routes.remove(&request)?;
        let refusal = match why {
            tasks::Refusal::Busy | tasks::Refusal::NotReady => people::Refusal::Busy,
            tasks::Refusal::Funding | tasks::Refusal::AuthorityShape => people::Refusal::Authority,
            tasks::Refusal::Unknown | tasks::Refusal::State => people::Refusal::Ended,
            tasks::Refusal::Duplicate
            | tasks::Refusal::Empty
            | tasks::Refusal::Batch
            | tasks::Refusal::Live
            | tasks::Refusal::Project
            | tasks::Refusal::Tree
            | tasks::Refusal::Depth
            | tasks::Refusal::Delegates
            | tasks::Refusal::Subscription
            | tasks::Refusal::Dependencies
            | tasks::Refusal::Cycle
            | tasks::Refusal::Executor
            | tasks::Refusal::Spec
            | tasks::Refusal::Contract
            | tasks::Refusal::Inputs
            | tasks::Refusal::Attempt
            | tasks::Refusal::LiveDelegates
            | tasks::Refusal::Restore
            | tasks::Refusal::Read
            | tasks::Refusal::Turn
            | tasks::Refusal::Reference
            | tasks::Refusal::HoldKind
            | tasks::Refusal::HoldTaken
            | tasks::Refusal::Holds => people::Refusal::Limit,
        };
        Some(people::Event::Decided { request, outcome: people::Outcome::Refused(refusal) })
    }

    /// Advance a successful batch to the child or caller that owns its
    /// continuation. The root only carries the returned event or answer.
    pub fn made(&mut self, request: Token, tasks: Box<[u64]>, person_proposal: bool) -> MadeRoute {
        if request.raw() >= u64::MAX - 3 {
            return MadeRoute::Internal;
        }
        if let Some(GoalRoute::Accepting { proposer, proposal, by, task }) = self.goal_routes.get(&request).copied() {
            assert!(tasks.as_ref() == [task], "accepted goal task made");
            let inserted = self.goal_routes.insert(request, GoalRoute::Deciding { proposer, proposal, by });
            assert!(inserted.is_ok(), "goal route replaced");
            return MadeRoute::Tasks(tasks::Event::DecidePersonProposal {
                reply_to: skein_lib::ReplyTo::new(request),
                proposer,
                proposal,
                by: tasks::Party::Person(by),
                message: None,
                decision: tasks::ProposalDecision::Accept,
            });
        }
        let person_route = if person_proposal { self.routing_people_proposals.remove(&request) } else { None };
        if let Some(PersonProposalRoute::Accepting { request: named, person, proposer, proposal, message }) =
            person_route
        {
            assert!(request == named, "person acceptance correlation");
            self.routing_people_proposals
                .insert(request, PersonProposalRoute::Deciding { request, proposer, proposal, by: person })
                .expect("person route room");
            return MadeRoute::PersonProposal(tasks::Event::DecideProposal {
                reply_to: skein_lib::ReplyTo::new(request),
                proposer,
                proposal,
                message: Some(message),
                by: tasks::Party::Person(person),
                decision: tasks::ProposalDecision::Accept,
            });
        }
        if let Some(RoutedCall::Accepting { key, proposer, proposal, message }) = self.routing_calls.remove(&request) {
            self.routing_calls.insert(request, RoutedCall::Decide { key, proposal }).expect("acceptance route room");
            return MadeRoute::Tasks(tasks::Event::DecideProposal {
                reply_to: skein_lib::ReplyTo::new(request),
                proposer,
                proposal,
                message: Some(message),
                by: tasks::Party::Task(key.task),
                decision: tasks::ProposalDecision::Accept,
            });
        }
        match self.delegating.remove(&request) {
            Some((key, stubs)) => MadeRoute::Delegated { key, tasks, stubs },
            None => {
                let (expected, goal) = self.made.remove(&request).expect("pending make route");
                assert!(tasks.as_ref() == [expected], "one person task created");
                MadeRoute::Person(Box::new(people::Event::Decided {
                    request,
                    outcome: if goal {
                        people::Outcome::GoalStarted { task: expected }
                    } else {
                        people::Outcome::Started { task: expected }
                    },
                }))
            }
        }
    }

    /// Translate the task child's successful person-origin proposal into the
    /// people child's keyed reply, consuming the in-flight correlation.
    pub fn person_proposed(&mut self, request: Token, proposal: u64) -> people::Event {
        let route = self.goal_routes.remove(&request).expect("person proposal route");
        assert!(route == GoalRoute::Proposing { proposal }, "proposal identity");
        people::Event::Decided { request, outcome: people::Outcome::GoalProposed { proposal } }
    }

    /// Translate the task child's person-origin proposal decision into the
    /// people child's keyed reply.
    pub fn person_proposal_decided(
        &mut self,
        request: Token,
        proposer: u64,
        number: u64,
        outcome: tasks::ProposalOutcome,
    ) -> people::Event {
        let route = self.goal_routes.remove(&request).expect("goal decision route");
        let (named, proposal, by) = match route {
            GoalRoute::Deciding { proposer, proposal, by } => (proposer, proposal, by),
            GoalRoute::Proposing { .. } | GoalRoute::Accepting { .. } => unreachable!("goal decision stage"),
        };
        assert!(named == proposer && proposal == number, "goal decision identity");
        let choice = match outcome {
            tasks::ProposalOutcome::Accepted => people::ProposalChoice::Accepted,
            tasks::ProposalOutcome::Rejected => people::ProposalChoice::Rejected,
            tasks::ProposalOutcome::Passed => people::ProposalChoice::Passed,
            tasks::ProposalOutcome::Withdrawn => people::ProposalChoice::Withdrawn,
            tasks::ProposalOutcome::Stale => people::ProposalChoice::Stale,
        };
        people::Event::Decided {
            request,
            outcome: people::Outcome::ProposalDecided { proposer, proposal: number, by, choice },
        }
    }

    /// Complete a task proposal decided on a person's keyed request. Other
    /// task proposal decisions continue to the named host-call route.
    pub fn task_proposal_decided_for_person(
        &mut self,
        request: Token,
        proposer: u64,
        number: u64,
        outcome: tasks::ProposalOutcome,
        person_proposal: bool,
    ) -> Option<people::Event> {
        let route = if person_proposal { self.routing_people_proposals.remove(&request) } else { None };
        match route {
            Some(PersonProposalRoute::Deciding { request: named, proposer: expected, proposal, by })
                if expected == proposer && proposal == number =>
            {
                let choice = match outcome {
                    tasks::ProposalOutcome::Accepted => people::ProposalChoice::Accepted,
                    tasks::ProposalOutcome::Rejected => people::ProposalChoice::Rejected,
                    tasks::ProposalOutcome::Passed => people::ProposalChoice::Passed,
                    tasks::ProposalOutcome::Withdrawn => people::ProposalChoice::Withdrawn,
                    tasks::ProposalOutcome::Stale => people::ProposalChoice::Stale,
                };
                Some(people::Event::Decided {
                    request: named,
                    outcome: people::Outcome::ProposalDecided { proposer, proposal: number, by, choice },
                })
            }
            Some(PersonProposalRoute::Deciding { .. } | PersonProposalRoute::Accepting { .. }) => {
                unreachable!("matching person proposal decision")
            }
            None => None,
        }
    }

    /// Number one subscribed state notice and route it back to the task hub.
    pub fn notice(
        &mut self,
        task: u64,
        subscription: u64,
        target: u64,
        state: tasks::NoticeState,
        words: Box<[u8]>,
        at: Wall,
    ) -> tasks::Event {
        let number = fresh(&mut self.counters, Family::Message).expect("notification number admitted");
        tasks::Event::Notice {
            task,
            word: tasks::Word {
                number,
                from: tasks::Party::Task(target),
                kind: tasks::MessageKind::Notice { subscription, target, state },
                words,
                at,
                hits: 1,
                eligible: false,
            },
        }
    }

    /// Number one due subscription timer and route it back to the task hub.
    pub fn notice_timer(&mut self, task: u64, subscription: u64, at: Wall) -> tasks::Event {
        let number = fresh(&mut self.counters, Family::Message).expect("timer number admitted");
        tasks::Event::Notice {
            task,
            word: tasks::Word {
                number,
                from: tasks::Party::Task(task),
                kind: tasks::MessageKind::Timer { subscription },
                words: Box::new([]),
                at,
                hits: 1,
                eligible: false,
            },
        }
    }

    /// Keep the newly committed word in a preparing run's brief and route
    /// the terminal to the caller who asked the task child to send it.
    pub fn sent(
        &mut self,
        request: Token,
        task: u64,
        word: &tasks::Word,
        inbox_messages: u32,
        tasks: u32,
    ) -> SentRoute {
        if let Some(context) = self.contexts.get_mut(&task) {
            let capacity = inbox_messages
                .checked_add(tasks.checked_mul(2).expect("two decision kinds per task"))
                .expect("validated virtual proposal room");
            let mut inbox = List::with_capacity(capacity);
            for old in &context.inbox {
                inbox.push(old.clone()).expect("accepted inbox count");
            }
            inbox.push(word.clone()).expect("accepted inbox count");
            context.inbox = inbox.into_boxed();
            context.last_message = word.number;
        }
        match self.routing_calls.remove(&request) {
            Some(RoutedCall::Message(key)) => SentRoute::Call { key, message: word.number },
            Some(
                RoutedCall::Introduce(_)
                | RoutedCall::Propose { .. }
                | RoutedCall::Decide { .. }
                | RoutedCall::Escalation { .. }
                | RoutedCall::Withdraw { .. }
                | RoutedCall::Accepting { .. }
                | RoutedCall::Subscribe { .. }
                | RoutedCall::Unsubscribe(_)
                | RoutedCall::Control(_),
            ) => unreachable!("only message calls produce Sent"),
            None => {
                let route = self.saying.remove(&request);
                let outcome = match route {
                    Some((named, None)) if named == task => people::Outcome::Said { task, message: word.number },
                    Some((named, Some(question))) if named == task => {
                        people::Outcome::QuestionAnswered { task, question, message: word.number }
                    }
                    Some(_) | None => unreachable!("matching pending person message flight"),
                };
                SentRoute::Person(Box::new(people::Event::Decided { request, outcome }))
            }
        }
    }

    /// The first offer of a proposal or escalation is fenced by the last
    /// committed offer in the current proof. Other messages retain the task
    /// child's own predecessor.
    #[must_use]
    pub fn relay_previous(&self, task: u64, previous: Option<u64>, kind: &tasks::MessageKind) -> Option<u64> {
        match kind {
            tasks::MessageKind::Proposal { .. } | tasks::MessageKind::Escalation { .. } => {
                match self.proofs.get(&task) {
                    Some(proof) => proof.offered,
                    None => None,
                }
            }
            tasks::MessageKind::ProposalDecision { .. }
            | tasks::MessageKind::Words
            | tasks::MessageKind::Amendment { .. }
            | tasks::MessageKind::Question
            | tasks::MessageKind::Answer { .. }
            | tasks::MessageKind::Notice { .. }
            | tasks::MessageKind::Timer { .. }
            | tasks::MessageKind::News { .. }
            | tasks::MessageKind::Result(_) => previous,
        }
    }
}
