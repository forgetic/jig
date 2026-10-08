//! Live run and restart correlation held by the core (domain/engine.md, 3 and 7).

use crate::{CallKey, Core, TurnRecord};
use alloc::boxed::Box;
use jig_core_authority as authority;
use jig_core_tasks as tasks;
use skein_lib::{List, Queue, Token, Wall};

use crate::translate;

/// The core's decision before a due agent run is prepared or claimed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RunAdmission {
    /// Current policy, funds, account and writes allow preparation.
    Allow,
    /// A transient account refresh is still needed.
    Account,
    /// The task child must hold the run for the named reason.
    Hold(tasks::Hold),
}

/// Bounded recent opaque conversation while a due task's store pages are read.
/// Empty turn bodies need no slot: they add nothing to a resumed conversation.
#[derive(Debug)]
pub struct Transcript {
    pub previous_attempt: u64,
    pub last: Option<(u64, u32)>,
    pub from: u64,
    pub bytes: u64,
    pub kept: u64,
    pub turns: Queue<Box<[u8]>>,
}

impl Core {
    /// Decide a run against current policy, funds, account usability and
    /// connector-translated workspace writes.
    #[must_use]
    pub fn run_admission(&self, task: &tasks::RunContext, wall: Wall, writes: Box<[authority::Write]>) -> RunAdmission {
        let mut findings =
            Queue::with_capacity(authority::max_out(self.authority.limits()).expect("authority check bound"));
        let numbers = translate::authority_numbers(task.numbers);
        let checked = authority::check_run(
            &self.authority,
            &authority::RunAsk {
                project: task.project,
                authority: translate::authority_value(&task.authority),
                numbers,
                budget: authority::left(numbers).min(self.authority.rules().maximum_run_spend),
                wall,
                accounts: Box::new([self.accounts.usable(self.settings.account)]),
                writes,
            },
            &mut findings,
        );
        match checked {
            authority::Answer::Allow => RunAdmission::Allow,
            authority::Answer::Wait | authority::Answer::Propose | authority::Answer::Refuse => {
                let mut account = false;
                let mut hold = None;
                for _ in 0..findings.len() {
                    match findings.pop().expect("authority finding count") {
                        authority::Finding::Account => account = true,
                        authority::Finding::Deadline => hold = Some(tasks::Hold::Deadline),
                        authority::Finding::RunBudget
                        | authority::Finding::RunCap
                        | authority::Finding::Arithmetic
                        | authority::Finding::Spend { .. }
                        | authority::Finding::Price { .. } => {
                            if hold != Some(tasks::Hold::Deadline) {
                                hold = Some(tasks::Hold::Budget);
                            }
                        }
                        authority::Finding::Oversized
                        | authority::Finding::UnknownProject
                        | authority::Finding::UnknownRole
                        | authority::Finding::Authority { .. }
                        | authority::Finding::Executor { .. }
                        | authority::Finding::Tasks { .. }
                        | authority::Finding::Writer
                        | authority::Finding::Tool
                        | authority::Finding::Grant { .. }
                        | authority::Finding::ResourceAccess
                        | authority::Finding::Reference
                        | authority::Finding::Scope { .. }
                        | authority::Finding::Required { .. }
                        | authority::Finding::Failed { .. }
                        | authority::Finding::Unguarded { .. }
                        | authority::Finding::Unpermitted
                        | authority::Finding::Undecidable
                        | authority::Finding::PeriodSpend => {
                            if hold.is_none() {
                                hold = Some(tasks::Hold::Effects);
                            }
                        }
                    }
                }
                match hold {
                    Some(why) => RunAdmission::Hold(why),
                    None if account => RunAdmission::Account,
                    None => RunAdmission::Hold(tasks::Hold::Effects),
                }
            }
        }
    }

    /// Begin a bounded transcript load for a due task's previous attempt.
    pub fn begin_transcript(&mut self, task: u64, previous_attempt: u64) {
        let from = match self.proofs.get(&task) {
            Some(proof) => proof.transcript_from,
            None => 0,
        };
        assert!(
            self.transcripts
                .insert(
                    task,
                    Transcript {
                        previous_attempt,
                        last: None,
                        from,
                        bytes: 0,
                        kept: 0,
                        turns: Queue::with_capacity(self.settings.resume_bytes),
                    },
                )
                .is_ok(),
            "one transcript preparation per live task"
        );
    }

    /// Admit one restored turn in store order, retaining only the bounded
    /// tail while still counting the full conversation for resume policy.
    #[must_use]
    pub fn append_transcript(&mut self, task: u64, turn: TurnRecord) -> bool {
        let transcript = self.transcripts.get_mut(&task).expect("load belongs to prepared task");
        if turn.task != task || turn.attempt > transcript.previous_attempt || turn.attempt == 0 || turn.turn == 0 {
            return false;
        }
        if let Some(previous) = transcript.last
            && (turn.attempt, turn.turn) <= previous
        {
            return false;
        }
        if turn.attempt < transcript.from {
            transcript.last = Some((turn.attempt, turn.turn));
            return true;
        }
        let length = u64::try_from(turn.transcript.len()).expect("stored turn size fits u64");
        let Some(total) = transcript.bytes.checked_add(length) else { return false };
        transcript.bytes = total;
        transcript.last = Some((turn.attempt, turn.turn));
        if length == 0 {
            return true;
        }
        let bound = u64::from(self.settings.resume_bytes);
        let body = if length > bound {
            let start = turn
                .transcript
                .len()
                .checked_sub(usize::try_from(bound).expect("u32 bound fits usize"))
                .expect("oversize turn has tail");
            Box::<[u8]>::from(turn.transcript.get(start..).expect("tail starts inside stored turn"))
        } else {
            turn.transcript
        };
        let body_len = u64::try_from(body.len()).expect("bounded turn fits u64");
        for _ in 0..transcript.turns.len() {
            if transcript.kept.checked_add(body_len).expect("two bounded tails") <= bound {
                break;
            }
            let old = transcript.turns.pop().expect("overfull transcript has an older turn");
            transcript.kept = transcript
                .kept
                .checked_sub(u64::try_from(old.len()).expect("bounded old turn"))
                .expect("old turn was counted");
        }
        transcript.turns.push(body);
        transcript.kept = transcript.kept.checked_add(body_len).expect("bounded transcript tail");
        true
    }

    /// Whether the full prior conversation exceeded this charter's resume bound.
    #[must_use]
    pub fn transcript_oversized(&self, task: u64) -> bool {
        self.transcripts.get(&task).expect("prepared transcript state").bytes > u64::from(self.settings.resume_bytes)
    }

    /// Select whole prior turns only while their cumulative bytes fit the
    /// charter's resume bound. A longer conversation starts fresh.
    #[must_use]
    pub fn resumed_turns(&self, transcript: Transcript) -> Box<[Box<[u8]>]> {
        let mut turns = List::with_capacity(transcript.turns.len());
        if transcript.bytes <= u64::from(self.settings.resume_bytes) {
            let mut kept = transcript.turns;
            for _ in 0..kept.len() {
                turns.push(kept.pop().expect("counted transcript turn")).expect("bounded transcript turns");
            }
        }
        turns.into_boxed()
    }
}

#[derive(Debug)]
pub struct RestoringProof {
    pub attempt: u64,
    pub turn: u32,
    pub run_spent: u64,
    pub last_answer: Option<u64>,
}

/// One configured model and its deployment-unit prices (domain/agent.md, 4.6).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Model {
    pub dialect: u32,
    pub account: u32,
    pub endpoint: u32,
    pub name: Box<[u8]>,
    pub max_tokens: u32,
    pub input_price: u64,
    pub cached_price: u64,
    pub output_price: u64,
    pub price_unit: u32,
}

/// Root-owned charter policy, independent of Smith's run vocabulary.
#[derive(Clone, PartialEq, Eq, Debug)]
#[expect(clippy::struct_excessive_bools, reason = "independent charter grants and run behavior")]
pub struct RunPolicy {
    pub instructions: Box<[u8]>,
    pub waiting: skein_lib::Duration,
    pub resume: bool,
    pub turns: u32,
    pub time: skein_lib::Duration,
    pub model: Model,
    pub alternatives: Box<[Model]>,
    pub inspect: bool,
    pub modify: bool,
    pub shell: bool,
    pub agents: bool,
    pub call_timeout: skein_lib::Duration,
}

/// The root's complete task-specific charter, kept behind one bounded
/// assignment cell so delivery queues carry a small fixed-size value.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RunCharter {
    pub policy: RunPolicy,
    pub contract: tasks::Contract,
    pub authority: tasks::Authority,
    pub budget: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoutedCall {
    Escalation { key: CallKey, task: u64, revision: u64 },
    Propose { key: CallKey, proposal: u64 },
    Decide { key: CallKey, proposal: u64 },
    Withdraw { key: CallKey, proposal: u64 },
    Accepting { key: CallKey, proposer: u64, proposal: u64, message: u64 },
    Message(CallKey),
    Introduce(CallKey),
    Subscribe { key: CallKey, subscription: u64 },
    Unsubscribe(CallKey),
    Control(CallKey),
}

#[derive(Debug)]
pub struct HistoricalResult {
    pub task: u64,
    pub kind: tasks::ResultKind,
    pub words: Box<[u8]>,
}

#[derive(Debug)]
pub struct PendingRelay {
    pub previous: Option<u64>,
    pub word: tasks::Word,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PersonProposalRoute {
    Deciding { request: Token, proposer: u64, proposal: u64, by: u64 },
    Accepting { request: Token, person: u64, proposer: u64, proposal: u64, message: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PersonTaskRoute {
    PoolSet { project: u32, person: u64 },
    Take(u64),
    HandBack(u64),
    Answer(u64),
    Cancel(u64),
    Release(u64),
    Prioritised(u32),
    Amended(u64),
    AmendProposed { task: u64, proposal: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GoalRoute {
    Proposing { proposal: u64 },
    Accepting { proposer: u64, proposal: u64, by: u64, task: u64 },
    Deciding { proposer: u64, proposal: u64, by: u64 },
}
