//! Scripted hosts on `domain/hosts.md`, 2, 6. They know their own assignments,
//! scripts and retained turns and answers; they know no core or task types.
//! Every connection begins with hello, followed by retained traffic. A dropped
//! connection preserves flights; past the stop bound it parks the runs. A
//! vanished worker loses its flights. Slots remain occupied until acknowledgments.
//! Engine hosts use the same scripts over a permanent direct link.
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use skein_lib::{Duration, Time};
use std::collections::{BTreeMap, VecDeque};

/// Identity and independently supplied allowance of an assigned attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    pub task: u64,
    pub attempt: u64,
    pub budget: u64,
    pub transcript: Box<[Box<[u8]>]>,
}

/// A classified run failure, in the host fake's own terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Temporary failure.
    Transient,
    /// External correction required.
    Permanent,
    /// The run failed.
    Run,
    /// Its agent failed.
    Agent,
    /// The worker was lost.
    Lost,
    /// Its result was invalid.
    Invalid,
}

/// An opaque host call or one semantic engine tool in a script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    /// Relay the declared host tool and input without interpreting either.
    Opaque { name: Box<[u8]>, tool: Box<[u8]>, input: Box<[u8]>, writes: bool },
    /// Propose a tracked goal as the accepting holder's own work.
    ProposeGoal { words: Box<[u8]>, budget: u64, priority: u32 },
    /// Delegate one person task to choose between two reports.
    Choose { role: u32, first: Box<[u8]>, second: Box<[u8]> },
}

/// One action of an assigned run's script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Script {
    /// Take a whole turn, within the independently supplied allowance.
    Turn { body: Box<[u8]>, cost: u64, read: Option<u64> },
    /// Ask one call and wait for its answer before advancing.
    Call(Call),
    /// Hold the slot until a new message arrives.
    Wait,
    /// End this activation while preserving the task.
    Park,
    /// Finish with a report.
    Finish { report: Box<[u8]> },
    /// End with a typed failure.
    Fail(Failure),
    /// Lose the agent, reported as an agent failure.
    Crash,
    /// Produce no traffic for this bounded interval.
    Silence { for_: Duration },
}

/// Faults of a worker's independently scripted link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Disconnect and return after this interval.
    DropChannel { for_: Duration },
    /// Lose the worker and all its unacknowledged traffic.
    Vanish,
    /// Delay script progress while keeping the connection open.
    Slow { by: Duration },
}

/// A retained activation outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// Successful report.
    Finished(Box<[u8]>),
    /// Successful park.
    Parked,
    /// Classified failure.
    Failed(Failure),
}

/// Independent reports at the worker face; the world translates them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Up {
    /// Begin a connection and list every retained slot.
    Hello { channel: u64, slots: u32, stop_bound: Duration, hosting: Box<[(u64, u64, bool)]> },
    /// The channel disappeared.
    Lost { channel: u64 },
    /// One retained whole turn.
    Turn { channel: u64, task: u64, attempt: u64, turn: u32, spent: u64, read: Option<u64>, body: Box<[u8]> },
    /// A call waiting for one answer.
    Call { channel: u64, task: u64, attempt: u64, number: u32, call: Call },
    /// A retained activation answer.
    Answer { channel: u64, task: u64, attempt: u64, spent: u64, end: End },
}

#[derive(Debug)]
struct Run {
    assignment: Assignment,
    script: VecDeque<Script>,
    spent: u64,
    kept_spent: u64,
    next_turn: u32,
    next_call: u32,
    turns: Vec<Up>,
    answer: Option<Up>,
    call: Option<Up>,
    until: Option<Time>,
    waiting: bool,
    answer_acked: bool,
}

/// Independent cumulative expense for one assignment, retained after its slot ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Expense {
    pub spent: u64,
    pub kept: u64,
    pub lost: u64,
    pub lost_turns: u32,
}

/// A bounded host, with a script per new assignment and observations of its own.
#[derive(Debug)]
pub struct Worker {
    pub channel: u64,
    pub slots: u32,
    pub stop_bound: Duration,
    pub seen: Vec<Assignment>,
    pub messages: Vec<(u64, u64, u64, Box<[u8]>)>,
    pub call_answers: Vec<(u64, u64, u32, Box<[u8]>)>,
    pub turn_acks: u32,
    pub answer_acks: u32,
    pub actual_spent: u64,
    pub lost_spent: u64,
    /// Per-attempt expenses, including turns lost before acknowledgement.
    pub expenses: BTreeMap<(u64, u64), Expense>,
    scripts: VecDeque<Box<[Script]>>,
    runs: BTreeMap<(u64, u64), Run>,
    connected: bool,
    hello_due: bool,
    disconnected_at: Option<Time>,
    reconnect: Option<Time>,
    slow_until: Option<Time>,
    vanished: bool,
    notices: Vec<Up>,
}

impl Worker {
    /// Construct a worker, or an engine host at channel zero.
    #[must_use]
    pub fn new(channel: u64, slots: u32, scripts: Vec<Box<[Script]>>) -> Self {
        assert!(slots > 0);
        Self {
            channel,
            slots,
            stop_bound: Duration::from_millis(100),
            seen: Vec::new(),
            messages: Vec::new(),
            call_answers: Vec::new(),
            turn_acks: 0,
            answer_acks: 0,
            actual_spent: 0,
            lost_spent: 0,
            expenses: BTreeMap::new(),
            scripts: scripts.into(),
            runs: BTreeMap::new(),
            connected: true,
            hello_due: channel != 0,
            disconnected_at: None,
            reconnect: None,
            slow_until: None,
            vanished: false,
            notices: Vec::new(),
        }
    }

    /// Admit an assignment only onto a free slot, retaining its independent view.
    pub fn assign(&mut self, assignment: Assignment) {
        assert!(self.runs.len() < usize::try_from(self.slots).expect("slot count fits"), "host slot overflow");
        let script = self.scripts.pop_front().unwrap_or_default();
        self.seen.push(assignment.clone());
        let key = (assignment.task, assignment.attempt);
        self.expenses.insert(key, Expense::default());
        assert!(
            self.runs
                .insert(
                    key,
                    Run {
                        assignment,
                        script: script.into_vec().into(),
                        spent: 0,
                        kept_spent: 0,
                        next_turn: 1,
                        next_call: 1,
                        turns: Vec::new(),
                        answer: None,
                        call: None,
                        until: None,
                        waiting: false,
                        answer_acked: false,
                    }
                )
                .is_none(),
            "attempt assigned once"
        );
    }

    /// A delivered message releases one scripted wait.
    pub fn message(&mut self, task: u64, attempt: u64, name: u64, words: Box<[u8]>) {
        self.messages.push((task, attempt, name, words));
        if let Some(run) = self.runs.get_mut(&(task, attempt)) {
            run.waiting = false;
        }
    }

    /// One call answer ends its flight; duplicate answers are contract failures.
    pub fn call_answer(&mut self, task: u64, attempt: u64, number: u32, bytes: Box<[u8]>) {
        let run = self.runs.get_mut(&(task, attempt)).expect("answer for hosted attempt");
        assert!(
            matches!(run.call.take(), Some(Up::Call { number: named, .. }) if named == number),
            "one answer per call"
        );
        self.call_answers.push((task, attempt, number, bytes));
    }

    /// A committed prefix may be forgotten; each new acknowledgement is counted.
    pub fn acknowledge_turn(&mut self, task: u64, attempt: u64, turn: u32) {
        if let Some(run) = self.runs.get_mut(&(task, attempt)) {
            let before = run.turns.len();
            for row in &run.turns {
                if let Up::Turn { turn: number, spent, .. } = row
                    && *number <= turn
                {
                    run.kept_spent = run.kept_spent.max(*spent);
                }
            }
            run.turns.retain(|row| !matches!(row, Up::Turn { turn: number, .. } if *number <= turn));
            self.expenses.get_mut(&(task, attempt)).expect("assigned expense").kept = run.kept_spent;
            self.turn_acks += u32::try_from(before - run.turns.len()).expect("bounded retained turns");
            self.retire(task, attempt);
        }
    }

    /// A committed activation answer frees its slot after all turns are kept.
    pub fn acknowledge_answer(&mut self, task: u64, attempt: u64) {
        if let Some(run) = self.runs.get_mut(&(task, attempt)) {
            assert!(run.answer.is_some(), "answer kept before acknowledgement");
            if !run.answer_acked {
                self.answer_acks += 1;
            }
            run.kept_spent = run.spent;
            self.expenses.get_mut(&(task, attempt)).expect("assigned expense").kept = run.spent;
            run.answer_acked = true;
            self.retire(task, attempt);
        }
    }

    fn retire(&mut self, task: u64, attempt: u64) {
        if self.runs.get(&(task, attempt)).is_some_and(|run| run.answer_acked && run.turns.is_empty()) {
            self.runs.remove(&(task, attempt));
        }
    }

    /// Cancel at most once; an already answered activation keeps its answer.
    pub fn cancel(&mut self, task: u64, attempt: u64) {
        if let Some(run) = self.runs.get_mut(&(task, attempt))
            && run.answer.is_none()
        {
            run.script.clear();
            run.call = None;
            let up = answer(self.channel, run, End::Parked);
            run.answer = Some(up.clone());
            self.notices.push(up);
        }
    }

    /// Apply a worker fault, reporting link loss independently of the core.
    #[must_use]
    pub fn fault(&mut self, fault: Fault, now: Time) -> Vec<Up> {
        assert!(self.channel != 0 || matches!(fault, Fault::Slow { .. }), "the engine link is permanent");
        match fault {
            Fault::DropChannel { for_ } => {
                self.connected = false;
                self.disconnected_at = Some(now);
                self.reconnect = Some(now.saturating_add(for_));
                vec![Up::Lost { channel: self.channel }]
            }
            Fault::Vanish => {
                self.connected = false;
                self.vanished = true;
                self.lost_spent += self.runs.values().map(|run| run.spent - run.kept_spent).sum::<u64>();
                for (key, run) in &self.runs {
                    let expense = self.expenses.get_mut(key).expect("assigned expense");
                    expense.lost = run.spent - run.kept_spent;
                    expense.lost_turns = u32::try_from(run.turns.len()).expect("bounded retained turns");
                }
                self.runs.clear();
                vec![Up::Lost { channel: self.channel }]
            }
            Fault::Slow { by } => {
                self.slow_until = Some(now.saturating_add(by));
                Vec::new()
            }
        }
    }

    /// A new engine process loses its own activations; remote hosts send hello
    /// and all retained traffic again. A vanished host stays absent.
    pub fn restart_engine(&mut self) {
        if self.channel == 0 {
            for (key, run) in &self.runs {
                let expense = self.expenses.get_mut(key).expect("assigned expense");
                expense.lost = run.spent - run.kept_spent;
                expense.lost_turns = u32::try_from(run.turns.len()).expect("bounded retained turns");
                self.lost_spent += expense.lost;
            }
            self.runs.clear();
            self.notices.clear();
        } else if !self.vanished {
            self.hello_due = true;
        }
    }

    /// Advance each run by at most one action, with at most two retained turns.
    #[must_use]
    pub fn tick(&mut self, now: Time) -> Vec<Up> {
        if self.vanished {
            return Vec::new();
        }
        if !self.connected && self.disconnected_at.is_some_and(|at| now >= at.saturating_add(self.stop_bound)) {
            for run in self.runs.values_mut() {
                if run.answer.is_none() {
                    run.script.clear();
                    run.call = None;
                    run.answer = Some(answer(self.channel, run, End::Parked));
                }
            }
        }
        if self.reconnect.is_some_and(|until| now >= until) {
            self.connected = true;
            self.reconnect = None;
            self.hello_due = true;
        }
        let mut out = Vec::new();
        if self.connected && self.hello_due {
            self.hello_due = false;
            self.disconnected_at = None;
            self.notices.clear();
            return self.replay();
        }
        if self.connected {
            out.append(&mut self.notices);
        }
        if self.slow_until.is_some_and(|until| now < until) {
            return out;
        }
        for run in self.runs.values_mut() {
            if run.answer.is_some()
                || run.call.is_some()
                || run.waiting
                || run.turns.len() >= 2
                || run.until.is_some_and(|until| now < until)
            {
                continue;
            }
            run.until = None;
            let Some(action) = run.script.pop_front() else { continue };
            let emitted = match action {
                Script::Turn { body, cost, read } => {
                    run.spent = run.spent.checked_add(cost).expect("spend fits");
                    assert!(run.spent <= run.assignment.budget, "scripted completion stays in its allowance");
                    self.actual_spent += cost;
                    self.expenses
                        .get_mut(&(run.assignment.task, run.assignment.attempt))
                        .expect("assigned expense")
                        .spent = run.spent;
                    let turn = Up::Turn {
                        channel: self.channel,
                        task: run.assignment.task,
                        attempt: run.assignment.attempt,
                        turn: run.next_turn,
                        spent: run.spent,
                        read,
                        body,
                    };
                    run.next_turn += 1;
                    run.next_call = run.next_call.max(run.next_turn);
                    run.turns.push(turn.clone());
                    Some(turn)
                }
                Script::Call(call) => {
                    let up = Up::Call {
                        channel: self.channel,
                        task: run.assignment.task,
                        attempt: run.assignment.attempt,
                        number: run.next_call,
                        call,
                    };
                    run.next_call += 1;
                    run.call = Some(up.clone());
                    Some(up)
                }
                Script::Wait => {
                    run.waiting = true;
                    None
                }
                Script::Silence { for_ } => {
                    run.until = Some(now.saturating_add(for_));
                    None
                }
                Script::Park => Some(answer(self.channel, run, End::Parked)),
                Script::Finish { report } => Some(answer(self.channel, run, End::Finished(report))),
                Script::Fail(why) => Some(answer(self.channel, run, End::Failed(why))),
                Script::Crash => Some(answer(self.channel, run, End::Failed(Failure::Agent))),
            };
            if let Some(up) = emitted {
                if matches!(up, Up::Answer { .. }) {
                    run.answer = Some(up.clone());
                }
                if self.connected {
                    out.push(up);
                }
            }
        }
        out
    }

    fn replay(&self) -> Vec<Up> {
        let mut out = Vec::new();
        out.push(Up::Hello {
            channel: self.channel,
            slots: self.slots,
            stop_bound: self.stop_bound,
            hosting: self
                .runs
                .values()
                .map(|run| (run.assignment.task, run.assignment.attempt, run.answer.is_some()))
                .collect(),
        });
        for run in self.runs.values() {
            out.extend(run.turns.iter().cloned());
            out.extend(run.call.iter().cloned());
            out.extend(run.answer.iter().cloned());
        }
        out
    }

    /// Whether every slot and retained flight was released.
    #[must_use]
    pub fn quiescent(&self) -> bool {
        self.runs.is_empty()
    }
}

fn answer(channel: u64, run: &Run, end: End) -> Up {
    Up::Answer { channel, task: run.assignment.task, attempt: run.assignment.attempt, spent: run.spent, end }
}
