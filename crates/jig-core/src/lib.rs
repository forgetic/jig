//! The engine core composes the task hub, policy, parties, fleet, accounts,
//! briefs, notes and views. It keeps work in jig's vocabulary and asks its
//! application's root to route the store, hosts, parties and connectors.
//! See `domain/engine.md`, sections 3 to 5.
//!
//! The core never holds a connector's value or a store encoding.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod amendments;
mod brief_text;
pub use brief_text::{BriefPart, BriefTextLimits, note_index_text, read_brief_part};
pub mod connector;
mod delegation;
pub use amendments::TaskAmendDenied;
pub use delegation::{Delegate, Dependency, ProcedureAction, resolved_delegate_authority, symbolic_grants};
mod effects;
mod escalation;
pub use effects::{EffectKey, EffectOrigin, EffectPurpose, effect_worst_case};
mod goals;
mod historical;
mod inbox;
mod memory;
pub use goals::GoalStart;
pub use memory::worst_case;
mod numbers;
mod person_task;
mod policy;
mod projections;
pub use projections::{
    MilestoneId, Projection, ProjectionFeed, ProjectionGoal, ProjectionKey, ProjectionMilestone, ProjectionRecord,
    ProjectionTask, finish_decision, projection_bytes, projection_record_bytes,
};
mod proposals;
mod reading;
mod restart;
pub use restart::{RestartRequest, RestartStep};
mod roles;
mod routing;
mod run;
mod sibling_routes;
pub use numbers::{Counters, Deployment, Family, fresh};
pub use routing::{
    Ask, CoreBriefBudgets, EscalationChoice, Event, Held, Limits, NamedAction, Now, PayloadRefusal, ProposalChoice,
    Request, Requests, Timer, ToolKind, Write, fire, resume_fleet, room, room_max, step,
};
pub use sibling_routes::{MadeRoute, PersonMessage, SentRoute};
mod conversation;
pub use conversation::{DeliveryOutcome, SettledAnswer, SettledCall, conversation_worst_case};
mod stored;
mod translate;
mod watch;
pub use run::{
    GoalRoute, HistoricalResult, Model, PendingRelay, PersonProposalRoute, PersonTaskRoute, RestoringProof, RoutedCall,
    RunAdmission, RunCharter, RunPolicy, Transcript,
};
pub use stored::{
    CallKey, CallPart, CallRecord, CallReplay, CoreKey, CoreRecord, EscalationDecisionRecord, Key,
    ProposalDecisionRecord, Record, Restored, RunProof, TerminalRecord, TurnProof, TurnRecord,
};
pub use translate::task_authority;
pub use watch::watch_project;

use alloc::boxed::Box;
use jig_core_accounts as accounts;
use jig_core_authority as authority;
use jig_core_brief as brief;
use jig_core_fleet as fleet;
use jig_core_notes as notes;
use jig_core_people as people;
use jig_core_tasks as tasks;
use jig_core_views as views;
use skein_lib::{List, Map, Queue, Token};

/// Application-supplied initial core state, in jig's vocabulary.
#[derive(Debug)]
pub struct Config {
    /// Identity committed with the first decision.
    pub deployment: [u8; 16],
    /// Deterministic task scheduling seed.
    pub seed: u64,
    /// Initial authenticated owners.
    pub owners: Box<[people::InitialOwner]>,
    /// Configured authority and policy.
    pub authority: authority::Domain,
    /// Policy and run settings retained by the core.
    pub settings: Settings,
    /// Configured project identifiers for bootstrap.
    pub projects: List<u32>,
    /// Policy-controlled connector permissions per project.
    pub permission_roles: Map<u32, Box<[people::PermissionRole]>>,
    /// Numbered connectors the application routes for task lifecycle handoffs.
    pub connectors: Box<[u16]>,
}

/// Immutable policy and run settings used by core decisions.
#[derive(Debug)]
pub struct Settings {
    /// Provider number for the application's services.
    pub deployment_provider: u16,
    /// Agent charter selected for chats.
    pub charter: u32,
    /// Bounded run policy for the charter.
    pub run: RunPolicy,
    /// Maximum committed conversation bytes resumed whole.
    pub resume_bytes: u32,
    /// Funding period identifier.
    pub period: u64,
    /// Initial project period budget.
    pub period_budget: u64,
    /// Initial person pool budget.
    pub person_budget: u64,
    /// Authority of a directly requested chat.
    pub chat_authority: authority::Authority,
    /// Account selected for the charter.
    pub account: u32,
    /// Initial account generation.
    pub account_generation: u64,
    /// Initial credential lifetime.
    pub account_valid: Option<skein_lib::Duration>,
    /// Connector number for recurring procedures.
    pub recurring_connector: u16,
    /// Configured one-bit authority family for each engine tool.
    pub tools: ToolFamilies,
}

/// Authority bits assigned to the engine's tool families by the charter.
#[derive(Clone, Copy, Debug)]
pub struct ToolFamilies {
    pub delegate: authority::Tools,
    pub message: authority::Tools,
    pub control: authority::Tools,
    pub decide: authority::Tools,
    pub propose: authority::Tools,
    pub subscribe: authority::Tools,
    pub effect: authority::Tools,
    pub note: authority::Tools,
    pub recall: authority::Tools,
    pub read: authority::Tools,
}

impl ToolFamilies {
    /// Every offered family has one distinct authority bit.
    #[must_use]
    pub fn valid(&self) -> bool {
        let mut seen = 0_u64;
        for family in [
            self.delegate,
            self.message,
            self.control,
            self.decide,
            self.propose,
            self.subscribe,
            self.effect,
            self.note,
            self.recall,
            self.read,
        ] {
            if family.0.count_ones() != 1 || seen & family.0 != 0 {
                return false;
            }
            seen |= family.0;
        }
        true
    }

    /// A ten-family assignment available to a charter that offers every engine tool.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            delegate: authority::Tools(1),
            message: authority::Tools(2),
            control: authority::Tools(4),
            decide: authority::Tools(8),
            propose: authority::Tools(16),
            subscribe: authority::Tools(32),
            effect: authority::Tools(64),
            note: authority::Tools(128),
            recall: authority::Tools(256),
            read: authority::Tools(512),
        }
    }
}

/// The eight child domains of the core. Routing and durable live state are
/// added here as the root's boundary is drawn.
#[derive(Debug)]
#[expect(clippy::partial_pub_fields, reason = "the keyed escalation route remains private to the core")]
pub struct Core {
    pub(crate) restart: restart::Restart,
    pub(crate) restart_attempts: u32,
    /// Application connectors addressed only by number.
    pub connectors: Box<[u16]>,
    /// Live view watcher correlation.
    pub watching: Map<Token, u64>,
    /// Last projected task phases.
    pub view_phases: Map<u64, (u32, Option<u32>)>,
    /// Historical result reads in flight.
    pub reading_results: Map<u64, Token>,
    /// Historical dependency results held for preparation.
    pub dependency_results: Map<u64, Box<[HistoricalResult]>>,
    /// Numbered task creation flights.
    pub made: Map<Token, (u64, bool)>,
    /// Goal proposal decisions in flight.
    pub goal_routes: Map<Token, GoalRoute>,
    /// Delegations awaiting input checks.
    pub delegating: Map<Token, (CallKey, Box<[tasks::Stub]>)>,
    /// Person proposal decisions in flight.
    pub routing_people_proposals: Map<Token, PersonProposalRoute>,
    /// Ended result positions waiting to be delivered.
    pub ending_positions: Map<u64, u64>,
    /// Person messages awaiting task admission.
    pub saying: Map<Token, (u64, Option<u64>)>,
    /// Person task moves awaiting their answer.
    pub moving: Map<Token, u64>,
    /// Person task request routes in flight.
    pub person_tasks: Map<Token, PersonTaskRoute>,
    /// Authenticated person escalation choices awaiting task semantics.
    pub(crate) person_escalations: Map<Token, routing::PersonEscalation>,
    /// Person task creations waiting for connector-owned resource holdings.
    pub(crate) creating: Map<Token, routing::Creation>,
    /// Named task delegation batches awaiting connector-owned holdings.
    pub(crate) creating_delegates: Map<Token, routing::DelegateCreation>,
    /// Procedure batches awaiting connector-owned holdings.
    pub(crate) creating_procedures: Map<(u64, u64), routing::ProcedureCreation>,
    /// One committed word awaiting host delivery.
    pub relaying: Option<PendingRelay>,
    /// Candidate sign-in in flight.
    pub signing_in: Option<u64>,
    /// Configured project identifiers for bootstrap.
    pub projects: List<u32>,
    /// Mutable connector permission mappings approved by policy edits.
    pub permission_roles: Map<u32, Box<[people::PermissionRole]>>,
    /// Immutable settings for core decisions.
    pub settings: Settings,
    /// Named calls awaiting their routed decision.
    pub pending_calls: Map<CallKey, bool>,
    /// Durable live named-call decisions, with connector payloads retained by
    /// their owner and represented here only by connector number.
    pub call_parts: Map<CallKey, CallPart>,
    /// Settled host answers and opaque delivery evidence under the same retained names.
    pub call_settled: Map<CallKey, SettledCall>,
    /// Calls routed among the core children.
    pub routing_calls: Map<Token, RoutedCall>,
    /// Synchronous connector descriptions and verdicts in this decision.
    pub(crate) effect_flights: Map<Token, Box<effects::Flight>>,
    /// Live reply rights waiting for a committed effect or its answer deadline.
    pub(crate) effect_replies: Map<u64, effects::Waiting>,
    /// Connectors whose outboxes must finish before a task closes.
    pub(crate) closing_connectors: Map<(u64, u16), ()>,
    pub(crate) projections: Map<u64, Box<Projection>>,
    /// Connector payloads awaiting the task hub's proposal admission.
    pub(crate) proposing_effects: Map<Token, (u16, Token)>,
    /// Correlations while the notes child loads a page for a caller.
    pub(crate) note_routes: Map<Token, routing::NoteRoute>,
    /// Preparations waiting for the notes child's current load or write.
    pub(crate) pending_note_briefs: Map<u64, bool>,
    /// Transient owner names for notes routes; pending calls are restored from their records.
    pub(crate) next_note_owner: u64,
    /// Durable deployment numbers.
    pub counters: Counters,
    /// Live task preparation contexts.
    pub contexts: Map<u64, Box<tasks::RunContext>>,
    /// Briefs waiting for their application connector's workspace translation.
    pub(crate) workspace_pending: Map<u64, u64>,
    /// Recent transcript windows for runs being prepared.
    pub transcripts: Map<u64, Transcript>,
    /// Current durable claim evidence.
    pub proofs: Map<u64, RunProof>,
    /// Correlation while current proofs are restored.
    pub restoring_proofs: Map<u64, RestoringProof>,
    /// Restored claims not yet reported by a host.
    pub unreported_restored: Map<u64, u64>,
    /// Claims awaiting completion of their route.
    pub claiming: Map<u64, u64>,
    /// Child work due to activation.
    pub due: Queue<Box<tasks::RunContext>>,
    /// Host adoption events held through startup.
    pub adopted: Queue<fleet::Event>,
    /// The task hub.
    pub tasks: tasks::Domain,
    /// Policy and authority.
    pub authority: authority::Domain,
    /// Parties and authenticated requests.
    pub people: people::Domain,
    /// Hosts and attempts.
    pub fleet: fleet::Domain,
    /// Secret-free account lifetimes.
    pub accounts: accounts::Domain,
    /// Run brief gathering.
    pub brief: brief::GatherDomain,
    /// Scoped notes.
    pub notes: notes::Domain,
    /// Live views.
    pub views: views::Domain,
}

fn assert_connector_registry(connectors: &[u16]) {
    for connector in connectors {
        let mut count = 0_u32;
        for other in connectors {
            if connector == other {
                count = count.checked_add(1).expect("bounded connector registry");
            }
        }
        assert_eq!(count, 1, "connector numbers are unique");
    }
}

impl Core {
    /// Whether a live task still holds this exact fenced attempt.
    #[must_use]
    pub fn current_proof(&self, task: u64, attempt: u64) -> bool {
        match self.proofs.get(&task) {
            Some(proof) => proof.attempt == attempt,
            None => false,
        }
    }

    /// Retain the canonical terminal of an unpriced attempt before the task
    /// child makes its next lifecycle decision.
    pub fn remember_unpriced_terminal(&mut self, run: Token, attempt: Token, end: tasks::End) {
        if self.committed_terminal(run.raw(), attempt.raw()).is_some() {
            return;
        }
        let proof = self.proofs.get_mut(&run.raw()).expect("current fleet terminal has pre-reserved proof");
        assert!(proof.attempt == attempt.raw(), "fleet terminal belongs to current proof");
        let cumulative = match proof.turn {
            Some(turn) => turn.cumulative,
            None => 0,
        };
        proof.terminal = Some(TerminalRecord { task: run.raw(), attempt: attempt.raw(), cumulative, end });
    }

    pub(crate) fn committed_terminal(&self, task: u64, attempt: u64) -> Option<TerminalRecord> {
        if self.tasks.task(task)?.last_answer != Some(attempt) {
            return None;
        }
        let proof = self.proofs.get(&task)?;
        if proof.attempt != attempt {
            return None;
        }
        proof.terminal.clone()
    }

    /// Retain one newly accepted named call decision. The application saves
    /// its store row and holds the outward answer behind the same commit.
    pub fn record_call(&mut self, key: CallKey, part: CallPart) {
        assert!(self.call_parts.insert(key, part) == Ok(None), "named call room reserved");
    }

    /// Restore one live named call only after its current task proof was
    /// restored. The connector's separately owned answer is validated by the
    /// application before it asks the core to retain this part.
    pub fn restore_call(&mut self, key: CallKey, part: CallPart) -> bool {
        let deployment = self.counters.deployment();
        let valid = key.task != 0
            && key.attempt != 0
            && key.completion != 0
            && key.task <= deployment.tasks
            && key.attempt <= deployment.runs
            && match self.proofs.get(&key.task) {
                Some(proof) => proof.attempt >= key.attempt,
                None => false,
            };
        valid && self.call_parts.insert(key, part) == Ok(None)
    }

    /// Forget named calls whose answer entered a committed later turn. The
    /// application erases these keys in the same decision and drops any
    /// connector-owned answer for them.
    pub fn retire_calls(&mut self, task: u64, attempt: u64, turn: u32, room: u32) -> Box<[CallKey]> {
        let mut retired = List::with_capacity(room);
        for (&key, _) in &self.call_parts {
            if key.task == task && (key.attempt < attempt || (key.attempt == attempt && key.completion < turn)) {
                retired.push(key).expect("all retained call names fit their configured bound");
            }
        }
        for &key in &retired {
            self.call_settled.remove(&key);
            match self.call_parts.remove(&key) {
                Some(CallPart::Effect { entry, .. }) => {
                    self.effect_replies.remove(&entry);
                }
                Some(
                    CallPart::Connector { .. }
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
                    | CallPart::Unavailable,
                )
                | None => {}
            }
        }
        retired.into_boxed()
    }

    /// Allocate the eight children and the bounded live core tables.
    #[must_use]
    pub fn new(config: Config, limits: &Limits) -> Core {
        assert!(config.settings.tools.valid(), "each engine tool has one configured authority bit");
        assert!(*config.authority.limits() == limits.authority, "core prices its authority's configured bounds");
        assert!(
            config.connectors.len() <= usize::try_from(limits.connectors).expect("connector count fits usize"),
            "core prices every configured connector number"
        );
        assert_connector_registry(&config.connectors);
        assert!(
            config.settings.resume_bytes > 0 && config.settings.resume_bytes <= limits.resume_bytes,
            "core prices its transcript retention"
        );
        assert!(
            match memory::policy_owned_bytes(&config.settings.run) {
                Some(bytes) => bytes <= u64::from(limits.run_bytes),
                None => false,
            },
            "core prices its configured run policy"
        );
        assert!(config.projects.capacity() <= limits.authority.projects, "core prices configured projects");
        assert!(config.permission_roles.len() <= limits.authority.projects, "core prices project permissions");
        for (_, roles) in &config.permission_roles {
            let bytes = u64::try_from(roles.len())
                .expect("permission count fits u64")
                .checked_mul(u64::try_from(size_of::<people::PermissionRole>()).expect("role size fits u64"));
            assert!(
                match bytes {
                    Some(bytes) => bytes <= limits.policy_bytes,
                    None => false,
                },
                "core prices configured permission roles"
            );
        }
        Core {
            restart: restart::Restart::Cold,
            restart_attempts: limits.fleet.attempts,
            counters: Counters::bootstrap(config.deployment),
            watching: Map::with_capacity(limits.views.watchers),
            view_phases: Map::with_capacity(limits.tasks.tasks),
            reading_results: Map::with_capacity(limits.load_slots),
            dependency_results: Map::with_capacity(limits.tasks.tasks),
            made: Map::with_capacity(limits.people.pending),
            goal_routes: Map::with_capacity(limits.people.pending),
            delegating: Map::with_capacity(limits.fleet.calls),
            routing_people_proposals: Map::with_capacity(limits.people.pending),
            ending_positions: Map::with_capacity(limits.tasks.tasks),
            saying: Map::with_capacity(limits.people.pending),
            moving: Map::with_capacity(limits.people.pending),
            person_tasks: Map::with_capacity(limits.people.pending),
            person_escalations: Map::with_capacity(limits.people.pending),
            creating: Map::with_capacity(limits.people.pending),
            creating_delegates: Map::with_capacity(limits.call_records),
            creating_procedures: Map::with_capacity(limits.tasks.tasks),
            relaying: None,
            signing_in: None,
            projects: config.projects,
            permission_roles: config.permission_roles,
            connectors: config.connectors,
            pending_calls: Map::with_capacity(limits.call_records),
            call_parts: Map::with_capacity(limits.call_records),
            call_settled: Map::with_capacity(limits.call_records),
            routing_calls: Map::with_capacity(limits.fleet.calls),
            effect_flights: Map::with_capacity(
                limits
                    .call_records
                    .checked_add(limits.tasks.tasks)
                    .expect("call and procedure flights")
                    .checked_add(limits.people.pending)
                    .expect("effect flights"),
            ),
            effect_replies: Map::with_capacity(limits.call_records),
            closing_connectors: Map::with_capacity(
                limits.tasks.tasks.checked_mul(limits.connectors).expect("bounded closing connectors"),
            ),
            projections: Map::with_capacity(limits.tasks.tasks),
            proposing_effects: Map::with_capacity(limits.call_records),
            note_routes: Map::with_capacity(2),
            pending_note_briefs: Map::with_capacity(limits.brief.briefs),
            next_note_owner: 1,
            contexts: Map::with_capacity(limits.tasks.tasks),
            workspace_pending: Map::with_capacity(limits.tasks.tasks),
            transcripts: Map::with_capacity(limits.tasks.tasks),
            proofs: Map::with_capacity(limits.tasks.tasks),
            restoring_proofs: Map::with_capacity(limits.tasks.tasks),
            unreported_restored: Map::with_capacity(limits.tasks.tasks),
            claiming: Map::with_capacity(limits.tasks.tasks),
            due: Queue::with_capacity(limits.tasks.tasks),
            adopted: Queue::with_capacity(limits.tasks.tasks.checked_mul(2).expect("adoption room representable")),
            tasks: tasks::Domain::new(&limits.tasks, config.seed, Box::new([config.settings.charter])),
            authority: config.authority,
            people: people::Domain::new(&limits.people, config.owners, config.settings.deployment_provider),
            fleet: fleet::Domain::new(&limits.fleet),
            accounts: accounts::Domain::new(&limits.accounts),
            brief: brief::GatherDomain::new(&limits.brief),
            notes: notes::Domain::new(&limits.notes),
            views: views::Domain::new(&limits.views),
            settings: config.settings,
        }
    }

    /// Reclaim retired child slots at the application's iteration boundary.
    pub fn reclaim(&mut self) {
        self.tasks.reclaim();
        self.people.reclaim();
        self.fleet.reclaim();
        self.views.reclaim();
    }

    /// Discard expendable child observations without changing a decision.
    pub fn drain_facts(&mut self, limits: &Limits) {
        for _ in 0..limits.tasks.facts {
            let _fact = self.tasks.pop_fact();
        }
        for _ in 0..limits.people.facts {
            let _fact = self.people.pop_fact();
        }
        for _ in 0..limits.fleet.facts {
            let _fact = self.fleet.pop_fact();
        }
        for _ in 0..limits.accounts.facts {
            let _fact = self.accounts.pop_fact();
        }
        for _ in 0..limits.views.facts {
            let _fact = self.views.pop_fact();
        }
    }

    /// Pure idle fence for core work; a host can still hold a live attempt.
    #[must_use]
    pub fn quiescent(&self, journal_idle: bool) -> bool {
        self.counters.quiescent(journal_idle)
            && self.reading_results.is_empty()
            && self.dependency_results.is_empty()
            && self.pending_calls.is_empty()
            && self.effect_flights.is_empty()
            && self.proposing_effects.is_empty()
            && self.routing_calls.is_empty()
            && self.note_routes.is_empty()
            && self.pending_note_briefs.is_empty()
            && self.routing_people_proposals.is_empty()
            && self.made.is_empty()
            && self.goal_routes.is_empty()
            && self.delegating.is_empty()
            && self.ending_positions.is_empty()
            && self.saying.is_empty()
            && self.moving.is_empty()
            && self.person_tasks.is_empty()
            && self.relaying.is_none()
            && self.signing_in.is_none()
            && self.adopted.is_empty()
            && self.due.is_empty()
            && self.claiming.is_empty()
            && self.contexts.is_empty()
            && self.transcripts.is_empty()
            && self.restoring_proofs.is_empty()
            && self.unreported_restored.is_empty()
            && self.brief.is_idle()
            && !self.fleet.is_ready()
            && self.fleet.turns() == 0
            && self.fleet.calls() == 0
            && self.fleet.next_deadline().is_none()
            && !self.accounts.waiting()
    }

    /// Restore one current claim only when it matches the live task's
    /// transient correlation. An archive row can never enter this live map.
    #[expect(clippy::manual_let_else, reason = "the step-code subset uses exhaustive matches")]
    pub fn restore_proof(&mut self, proof: RunProof, result_bytes: u32) -> bool {
        let expected = match self.restoring_proofs.remove(&proof.task) {
            Some(expected) => expected,
            None => return false,
        };
        let turn = match proof.turn {
            Some(turn) => turn.turn,
            None => 0,
        };
        if proof.attempt != expected.attempt || turn != expected.turn || !valid_proof(&proof, &expected, result_bytes) {
            return false;
        }
        self.proofs.insert(proof.task, proof).is_ok()
    }
}

fn valid_proof(proof: &RunProof, expected: &RestoringProof, result_bytes: u32) -> bool {
    if proof.task == 0 || proof.attempt == 0 || proof.transcript_from > proof.attempt {
        return false;
    }
    let spent = match proof.turn {
        Some(turn) => {
            let read_valid = match turn.read {
                Some(number) => match proof.offered {
                    Some(high) => number <= high,
                    None => false,
                },
                None => true,
            };
            if turn.turn == 0 || !read_valid || turn.cumulative > expected.run_spent {
                return false;
            }
            turn.cumulative
        }
        None => 0,
    };
    match &proof.terminal {
        Some(terminal) => {
            terminal.task == proof.task
                && terminal.attempt == proof.attempt
                && terminal.cumulative == expected.run_spent
                && expected.last_answer == Some(proof.attempt)
                && match tasks::terminal_bytes(&terminal.end) {
                    Some(bytes) => bytes <= u64::from(result_bytes).checked_mul(2).expect("result bound representable"),
                    None => false,
                }
        }
        None => spent == expected.run_spent && expected.last_answer != Some(proof.attempt),
    }
}
