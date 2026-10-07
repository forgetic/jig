use jig_core_tasks::{Contract, Ending, Last, Limits, Parameter, Party, Phase, Status, TaskRecord, TaskResult, Was};
use skein_lib::Duration;
use skein_world::domain::{Expectations, Judge};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Seen {
    Batch { members: Vec<u64>, accepted: bool, made: Vec<u64> },
    Made { task: u64, parent: Party, dependencies: Vec<u64>, depth: u32 },
    Durable { commit: u64 },
    Replied { call: u64, after: u64 },
    Assigned { task: u64, attempt: u64, after: u64, adopted: bool },
    Terminal { task: u64, attempt: u64 },
    Closing { task: u64 },
    Settled { task: u64 },
    Ended { task: u64, status: Status, after: u64 },
    Cancelled { task: u64 },
    Limit { live: usize, cap: u32 },
    Stored { live: Vec<TaskRecord>, limits: Box<Limits> },
    Finished,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Name {
    End(u64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stimulus {
    Restart,
}

#[derive(Default, Debug)]
pub struct Tasks {
    durable: u64,
    replies: BTreeSet<u64>,
    parents: BTreeMap<u64, Party>,
    dependencies: BTreeMap<u64, Vec<u64>>,
    runs: BTreeMap<u64, u64>,
    attempts: BTreeMap<u64, u64>,
    closing: BTreeSet<u64>,
    settled: BTreeSet<u64>,
    ended: BTreeMap<u64, Status>,
    cancelled: BTreeSet<u64>,
}

impl Tasks {
    #[must_use]
    pub fn has_task(&self, task: u64) -> bool {
        self.parents.contains_key(&task)
    }

    #[must_use]
    pub fn descendants(&self, ancestor: u64) -> Vec<u64> {
        self.parents
            .keys()
            .filter(|task| !self.ended.contains_key(task))
            .filter(|task| {
                let mut at = Some(**task);
                for _ in 0..self.parents.len() {
                    let Some(number) = at else {
                        return false;
                    };
                    if number == ancestor {
                        return true;
                    }
                    at = match self.parents.get(&number) {
                        Some(Party::Task(parent)) => Some(*parent),
                        Some(Party::Person(_) | Party::Deployment { .. }) | None => None,
                    };
                }
                false
            })
            .copied()
            .collect()
    }

    fn end(&mut self, task: u64, status: Status, after: u64, judge: &mut Judge<Name, Stimulus>) {
        if after > self.durable {
            judge.fail("result before durability");
        }
        if !self.settled.contains(&task) {
            judge.fail("result before settlement");
        }
        if self.runs.contains_key(&task)
            || self
                .parents
                .iter()
                .any(|(child, parent)| *parent == Party::Task(task) && !self.ended.contains_key(child))
        {
            judge.fail("requester ended before descendant");
        }
        if self.ended.insert(task, status).is_some() {
            judge.fail("result delivered twice");
        }
        if self.cancelled.contains(&task) && status != Status::Cancelled {
            judge.fail("cancelled descendant returned different ending");
        }
        judge.meet(&Name::End(task));
    }

    fn cycle(&self) -> bool {
        for start in self.parents.keys() {
            let mut pending = self.dependencies.get(start).cloned().unwrap_or_default();
            for (child, parent) in &self.parents {
                if *parent == Party::Task(*start) && !self.ended.contains_key(child) {
                    pending.push(*child);
                }
            }
            let mut visited = BTreeSet::new();
            while let Some(task) = pending.pop() {
                if task == *start {
                    return true;
                }
                if !visited.insert(task) {
                    continue;
                }
                if let Some(dependencies) = self.dependencies.get(&task) {
                    pending.extend(dependencies);
                }
                for (child, parent) in &self.parents {
                    if *parent == Party::Task(task) && !self.ended.contains_key(child) {
                        pending.push(*child);
                    }
                }
            }
        }
        false
    }
}

impl Expectations for Tasks {
    type Seen = Seen;

    type Name = Name;

    type Stimulus = Stimulus;

    fn observe(&mut self, seen: Seen, judge: &mut Judge<Name, Stimulus>) {
        match seen {
            Seen::Batch { mut members, accepted, mut made } => {
                members.sort_unstable();
                made.sort_unstable();
                if (accepted && made != members) || (!accepted && !made.is_empty()) {
                    judge.fail("batch partially admitted");
                }
            }
            Seen::Made { task, parent, dependencies, depth } => {
                if self.parents.insert(task, parent).is_some() {
                    judge.fail("task number reused");
                }
                if dependencies.contains(&task) {
                    judge.fail("task waits on itself");
                }
                if let Party::Task(parent) = parent
                    && (!self.parents.contains_key(&parent) || self.ended.contains_key(&parent) || depth == 0)
                {
                    judge.fail("delegate has no live parent");
                }
                self.dependencies.insert(task, dependencies);
                if self.cycle() {
                    judge.fail("cycle through dependencies or delegates");
                }
            }
            Seen::Durable { commit } => self.durable = commit,
            Seen::Replied { call, after } => {
                if after > self.durable {
                    judge.fail("reply before durability");
                }
                if !self.replies.insert(call) {
                    judge.fail("call replied twice");
                }
            }
            Seen::Assigned { task, attempt, after, adopted } => {
                if after > self.durable {
                    judge.fail("run before claim durable");
                }
                if self.runs.get(&task).is_some_and(|old| *old != attempt || !adopted) {
                    judge.fail("two runs for task");
                }
                if !adopted && self.attempts.get(&task).is_some_and(|old| *old >= attempt) {
                    judge.fail("attempt did not grow");
                }
                if self.dependencies.get(&task).is_some_and(|dependencies| {
                    dependencies.iter().any(|number| self.ended.get(number) != Some(&Status::Done))
                }) {
                    judge.fail("run before dependencies done");
                }
                self.runs.insert(task, attempt);
                self.attempts.insert(task, attempt);
            }
            Seen::Terminal { task, attempt } => {
                if self.runs.get(&task) != Some(&attempt) {
                    judge.fail("terminal is not current run");
                }
                self.runs.remove(&task);
            }
            Seen::Closing { task } => {
                if self.runs.contains_key(&task) {
                    judge.fail("effects closed before own run ended");
                }
                if self
                    .parents
                    .iter()
                    .any(|(child, parent)| *parent == Party::Task(task) && !self.ended.contains_key(child))
                {
                    judge.fail("effects closed before delegates ended");
                }
                self.closing.insert(task);
            }
            Seen::Settled { task } => {
                if !self.closing.contains(&task) {
                    judge.fail("settled before closing");
                }
                self.settled.insert(task);
            }
            Seen::Ended { task, status, after } => self.end(task, status, after, judge),
            Seen::Cancelled { task } => {
                for task in self.descendants(task) {
                    if self.cancelled.insert(task) {
                        judge.expect(Name::End(task), Duration::from_secs(10));
                    }
                }
            }
            Seen::Limit { live, cap } => {
                if live > usize::try_from(cap).expect("u32 fits usize") {
                    judge.fail("live task limit exceeded");
                }
            }
            Seen::Stored { live, limits } => stored(&live, &limits, judge),
            Seen::Finished => {
                if self.cancelled.iter().any(|task| !self.ended.contains_key(task)) {
                    judge.fail("cancelled task did not end");
                }
            }
        }
    }
}

fn bytes(result: &TaskResult) -> usize {
    match result {
        TaskResult::Report { words } | TaskResult::Verdict { words, .. } | TaskResult::Change { words, .. } => {
            words.len()
        }
        TaskResult::Failure { reason } => reason.len(),
    }
}

fn end_within(ending: &Ending, cap: usize) -> bool {
    match ending {
        Ending::Done(result) => bytes(result) <= cap,
        Ending::Failed { reason } => reason.len() <= cap,
        Ending::Cancelled { reason, result } => {
            reason.len() <= cap && result.as_ref().is_none_or(|result| bytes(result) <= cap)
        }
    }
}

fn within(task: &TaskRecord, limits: &Limits) -> bool {
    let spec_bytes = task.spec.words.len()
        + task
            .spec
            .parameters
            .iter()
            .map(|parameter| match parameter {
                Parameter::Bytes { value, .. } => value.len(),
                Parameter::Number { .. } | Parameter::Resource { .. } => 0,
            })
            .sum::<usize>();
    let grant_bytes = task
        .authority
        .grants
        .iter()
        .map(|grant| {
            grant.pattern.segments.iter().map(|segment| segment.len()).sum::<usize>()
                + match &grant.pattern.last {
                    Last::Exact(bytes) | Last::Open(bytes) => bytes.len(),
                }
        })
        .sum::<usize>();
    let counts = [
        (task.delegates.len(), limits.delegates),
        (task.dependencies.len(), limits.dependencies),
        (task.spec.inputs.len(), limits.inputs),
        (task.spec.parameters.len(), limits.parameters),
        (spec_bytes, limits.spec_bytes),
        (task.authority.grants.len(), limits.authority_grants),
        (task.authority.delegation.kinds.len(), limits.executor_kinds),
        (grant_bytes, limits.authority_bytes),
    ];
    if task.depth > limits.depth
        || task.made > limits.tree_tasks
        || counts.into_iter().any(|(count, limit)| count > usize::try_from(limit).expect("u32 fits usize"))
        || task.authority.grants.iter().any(|grant| {
            grant.pattern.segments.len() > usize::try_from(limits.authority_segments).expect("u32 fits usize")
        })
    {
        return false;
    }
    let result_cap = usize::try_from(limits.result_bytes).expect("u32 fits usize");
    let contract = match &task.contract {
        Contract::Report { words } | Contract::Change { words, .. } => *words <= limits.result_bytes,
        Contract::Verdict { choices } => {
            !choices.is_empty()
                && choices.len() <= usize::try_from(limits.contract_choices).expect("u32 fits usize")
                && choices.iter().all(|choice| choice.words <= limits.result_bytes)
        }
    };
    contract
        && match &task.phase {
            Phase::Closing(closing) | Phase::Held { was: Was::Closing(closing), .. } => {
                end_within(&closing.ending, result_cap)
            }
            Phase::Ended(ending) => end_within(ending, result_cap),
            Phase::Waiting | Phase::Active(_) | Phase::Held { was: Was::Waiting | Was::Active(_), .. } => true,
        }
}

fn stored(live: &[TaskRecord], limits: &Limits, judge: &mut Judge<Name, Stimulus>) {
    if !live.iter().all(|task| within(task, limits))
        || live.iter().any(|task| {
            live.iter().filter(|other| other.project == task.project).count()
                > usize::try_from(limits.project_tasks).expect("u32 fits usize")
        })
    {
        judge.fail("stored shape exceeds configured limit");
    }
}
