//! Independent conservation from durable task allocations, ended transitions
//! and admitted priced inputs (domain/tasks.md, 2 and 5).
use jig_core_tasks::{
    Accepted, Escalation, EscalationHolder, Executor, Funder, Key, MessageKind, ProposalHolder, ProposalState, Request,
    Stored, TaskRecord,
};
use std::collections::BTreeMap;

#[derive(Default, Debug)]
pub struct Accounting {
    before: BTreeMap<Key, Stored>,
    charged: Option<(u64, u64)>,
    procedure: Option<(u64, u64, Option<u64>)>,
}

fn live(rows: &BTreeMap<Key, Stored>, task: u64) -> Option<&TaskRecord> {
    match rows.get(&Key::Live(task)) {
        Some(Stored::Live(row)) => Some(row),
        _ => None,
    }
}

fn current(rows: &BTreeMap<Key, Stored>, task: u64) -> Option<&TaskRecord> {
    live(rows, task).or_else(|| match rows.get(&Key::Ended(task)) {
        Some(Stored::Ended(row)) => Some(row),
        _ => None,
    })
}

fn standing_reset(before: &TaskRecord, after: &TaskRecord, rows: &BTreeMap<Key, Stored>) -> Option<Funder> {
    let old_period = match before.funder {
        Funder::Period { project, period } | Funder::Pool { project, period, .. } => Funder::Period { project, period },
        Funder::Task(_) | Funder::Recurring { .. } => return None,
    };
    let Funder::Period { project, period } = old_period else { unreachable!() };
    let Funder::Period { project: fresh_project, period: fresh_period } = after.funder else { return None };
    if project != fresh_project
        || fresh_period <= period
        || before.project != project
        || before.number != before.root
        || before.recurring.is_some()
        || before.subscriptions.is_empty()
        || !matches!(before.executor, Executor::Procedure { .. })
        || before.run_reserved != 0
        || before.numbers.budget != after.numbers.budget
        || after.numbers.spent != 0
        || after.numbers.spent_below != 0
        || after.numbers.reserved != 0
        || after.made != 1
    {
        return None;
    }
    let archived = Funder::Recurring { project, task: before.number, period };
    let Some(Stored::Ledger(ledger)) = rows.get(&Key::Ledger(archived)) else { return None };
    if ledger.parent != Some(old_period)
        || ledger.numbers.budget != before.numbers.budget
        || ledger.numbers.spent_below != before.numbers.spent + before.numbers.spent_below
        || ledger.numbers.reserved != before.numbers.reserved
        || ledger.made != before.made
    {
        return None;
    }
    Some(archived)
}

fn merged_hint(before: MessageKind, after: MessageKind) -> bool {
    match (before, after) {
        (MessageKind::Notice { subscription: old, .. }, MessageKind::Notice { subscription: new, .. })
        | (MessageKind::Timer { subscription: old }, MessageKind::Timer { subscription: new })
        | (MessageKind::News { subscription: old, .. }, MessageKind::News { subscription: new, .. }) => old == new,
        (MessageKind::Amendment { .. }, MessageKind::Amendment { .. }) => true,
        _ => false,
    }
}

impl Accounting {
    fn messages_and_holders(&self, rows: &BTreeMap<Key, Stored>, requests: &[Request]) -> Result<(), &'static str> {
        for old in self.before.values() {
            let Stored::Live(old) = old else { continue };
            let Some(after) = live(rows, old.number) else { continue };
            if old.recurring.is_some() && old.funder != after.funder {
                continue;
            }
            let taken = requests.iter().any(|request| {
                matches!(request, Request::TurnAcknowledged { task, accepted: Accepted::New, .. } if *task == old.number)
            });
            if taken {
                continue;
            }
            let read = match self.procedure {
                Some((task, step, read)) if task == old.number && after.attempt == step && old.attempt < step => read,
                Some(_) | None => None,
            };
            for word in &old.inbox {
                if read.is_some_and(|fence| word.number <= fence) {
                    if after.inbox.iter().any(|new| new.number == word.number) {
                        return Err("procedure step kept its consumed inbox prefix");
                    }
                    continue;
                }
                let kept = after.inbox.iter().any(|new| {
                    new.number == word.number || (new.number > word.number && merged_hint(word.kind, new.kind))
                });
                if !kept {
                    return Err("committed message disappeared without a turn, procedure step or merge");
                }
            }
        }
        for request in requests {
            if let Request::Sent { task, word, .. } = request {
                let Some(row) = live(rows, *task) else { return Err("committed message lost its task") };
                let kept = row.inbox.iter().any(|after| {
                    (after.number == word.number
                        && after.from == word.from
                        && after.kind == word.kind
                        && after.words == word.words)
                        || (after.number > word.number && merged_hint(word.kind, after.kind))
                });
                if !kept || row.last_message < word.number {
                    return Err("accepted message missing from durable inbox");
                }
            }
        }
        for row in rows.values() {
            let Stored::Live(task) = row else { continue };
            if let Some(proposal) = &task.proposal
                && let ProposalState::Pending { holder, .. } = proposal.state
            {
                let reachable = match holder {
                    ProposalHolder::Task(number) => live(rows, number).is_some_and(|holder| {
                        holder.project == task.project && !matches!(holder.executor, Executor::Procedure { .. })
                    }),
                    ProposalHolder::Person(number) => number != 0,
                    ProposalHolder::Policy { project, .. } => project == task.project,
                };
                if !reachable {
                    return Err("proposal has no deciding holder");
                }
            }
            if let Escalation::Waiting { holder, .. } = task.escalation {
                let reachable = match holder {
                    EscalationHolder::Task(number) => live(rows, number).is_some_and(|holder| {
                        holder.project == task.project && !matches!(holder.executor, Executor::Procedure { .. })
                    }),
                    EscalationHolder::Person(number) => number != 0,
                    EscalationHolder::Role { project, role } => project == task.project && role != 0,
                };
                if !reachable {
                    return Err("escalation has no deciding holder");
                }
            }
        }
        Ok(())
    }

    fn newly_retired_pools(&self, rows: &BTreeMap<Key, Stored>, period: Funder) -> u64 {
        rows.values()
            .filter_map(|row| {
                let Stored::Ledger(new) = row else { return None };
                if new.parent != Some(period) || !new.closed {
                    return None;
                }
                match self.before.get(&Key::Ledger(new.funder)) {
                    Some(Stored::Ledger(old)) if old.closed => return None,
                    Some(Stored::Ledger(_)) | None => {}
                    Some(_) => return None,
                }
                new.numbers.spent.checked_add(new.numbers.spent_below)
            })
            .sum()
    }

    fn newly_settled(&self, rows: &BTreeMap<Key, Stored>, funder: Funder) -> u64 {
        self.before
            .values()
            .filter_map(|row| {
                let Stored::Live(old) = row else { return None };
                if old.funder != funder {
                    return None;
                }
                let Some(Stored::Ended(ended)) = rows.get(&Key::Ended(old.number)) else { return None };
                ended.numbers.spent.checked_add(ended.numbers.spent_below)
            })
            .sum()
    }

    /// Accepted cumulative input supplied by the scripted parent, independently
    /// of production counters (domain/tasks.md, 5).
    pub fn charged(&mut self, task: u64, cumulative: u64) {
        let old = live(&self.before, task).expect("priced input names a live allocation");
        self.charged = Some((task, cumulative.checked_sub(old.run_spent).expect("admitted cumulative grows")));
    }

    /// Procedure input observed before its committed attempt and inbox changes.
    pub fn procedure(&mut self, task: u64, step: u64, read: Option<u64>) {
        self.procedure = Some((task, step, read));
    }

    fn delta(&self, task: u64) -> u64 {
        self.charged.filter(|(number, _)| *number == task).map_or(0, |(_, delta)| delta)
    }

    /// # Errors
    /// Rejects changed identities, missing postings, lost reservations or
    /// invented expense at the durable boundary (domain/tasks.md, 2).
    #[expect(clippy::too_many_lines, reason = "accounting referee checks all durable row forms")]
    pub fn committed(&mut self, rows: &BTreeMap<Key, Stored>, requests: &[Request]) -> Result<(), &'static str> {
        self.messages_and_holders(rows, requests)?;
        for (key, old) in &self.before {
            if matches!(old, Stored::Ended(_)) && rows.get(key) != Some(old) {
                return Err("immutable accounting row changed");
            }
            if matches!(old, Stored::Ledger(_)) && !rows.contains_key(key) {
                return Err("funding source vanished");
            }
        }
        for old in self.before.values() {
            let Stored::Live(old) = old else {
                continue;
            };
            let task = current(rows, old.number).ok_or("live allocation vanished")?;
            let recurring_reset = match old.funder {
                Funder::Period { project: before, period: old_period } => match task.funder {
                    Funder::Period { project: after, period: new_period } => {
                        old.recurring.is_some() && before == after && new_period > old_period && old.numbers.budget == 0
                    }
                    Funder::Task(_) | Funder::Pool { .. } | Funder::Recurring { .. } => false,
                },
                Funder::Task(_) | Funder::Pool { .. } | Funder::Recurring { .. } => false,
            };
            let renewed = standing_reset(old, task, rows).is_some();
            let standing_delegate = match (old.funder, task.funder) {
                (Funder::Task(parent), Funder::Recurring { task: renewed, .. }) if parent == renewed => {
                    match (live(&self.before, parent), live(rows, parent)) {
                        (Some(before), Some(after)) => standing_reset(before, after, rows) == Some(task.funder),
                        _ => false,
                    }
                }
                _ => false,
            };
            if (old.funder != task.funder && !recurring_reset && !renewed && !standing_delegate)
                || old.numbers.budget != task.numbers.budget
            {
                return Err("allocation identity or promise changed");
            }
            let posted = self.newly_settled(rows, Funder::Task(old.number));
            if !renewed
                && (task.numbers.spent != old.numbers.spent + self.delta(old.number)
                    || task.numbers.spent_below != old.numbers.spent_below + posted)
            {
                return Err("task expense or descendant posting differs");
            }
            if task.attempt == old.attempt && task.run_spent != old.run_spent + self.delta(old.number) {
                return Err("cumulative attempt expense differs");
            }
        }
        for (key, row) in rows {
            match row {
                Stored::Live(task) => {
                    let reserved: u64 = rows
                        .values()
                        .filter_map(|row| match row {
                            Stored::Live(child) if child.funder == Funder::Task(task.number) => {
                                Some(child.numbers.budget)
                            }
                            Stored::Live(_)
                            | Stored::Writer(_)
                            | Stored::Pool(_)
                            | Stored::Ended(_)
                            | Stored::Stub(_)
                            | Stored::Ledger(_)
                            | Stored::Milestone(_)
                            | Stored::History(_)
                            | Stored::PersonProposal(_) => None,
                        })
                        .sum();
                    if reserved.checked_add(task.run_reserved) != Some(task.numbers.reserved) {
                        return Err("task reservation differs from funding links");
                    }
                    if task
                        .numbers
                        .spent
                        .checked_add(task.numbers.spent_below)
                        .and_then(|spent| spent.checked_add(task.numbers.reserved))
                        .is_none_or(|used| used > task.numbers.budget)
                    {
                        return Err("recorded expense exceeds allotment");
                    }
                    if !self.before.contains_key(key)
                        && (task.numbers.spent != 0 || task.numbers.spent_below != 0 || task.run_spent != 0)
                    {
                        return Err("new allocation carries invented expense");
                    }
                }
                Stored::Ledger(ledger) => {
                    let reserved: u64 = rows
                        .values()
                        .filter_map(|row| match row {
                            Stored::Live(task) if task.funder == ledger.funder => Some(task.numbers.budget),
                            Stored::Ledger(pool) if pool.parent == Some(ledger.funder) && !pool.closed => {
                                Some(pool.numbers.budget)
                            }
                            Stored::Live(_)
                            | Stored::Writer(_)
                            | Stored::Pool(_)
                            | Stored::Ended(_)
                            | Stored::Stub(_)
                            | Stored::Ledger(_)
                            | Stored::Milestone(_)
                            | Stored::History(_)
                            | Stored::PersonProposal(_) => None,
                        })
                        .sum();
                    let posted = self.newly_settled(rows, ledger.funder);
                    let before_posted = match self.before.get(key) {
                        Some(Stored::Ledger(old)) => old.numbers.spent_below,
                        _ => 0,
                    };
                    let retired = self.newly_retired_pools(rows, ledger.funder);
                    let transferred = match ledger.funder {
                        Funder::Recurring { task, .. } if !self.before.contains_key(key) => {
                            match (live(&self.before, task), live(rows, task)) {
                                (Some(before), Some(after))
                                    if standing_reset(before, after, rows) == Some(ledger.funder) =>
                                {
                                    before.numbers.spent + before.numbers.spent_below
                                }
                                _ => 0,
                            }
                        }
                        Funder::Task(_) | Funder::Pool { .. } | Funder::Period { .. } | Funder::Recurring { .. } => 0,
                    };
                    if ledger.numbers.reserved != reserved
                        || ledger.numbers.spent != 0
                        || ledger.numbers.spent_below != before_posted + posted + retired + transferred
                    {
                        return Err("external reservation or settlement posting differs");
                    }
                    if let Some(Stored::Ledger(old)) = self.before.get(key)
                        && (ledger.funder != old.funder
                            || ledger.parent != old.parent
                            || {
                                let carved: u64 = match ledger.funder {
                                    Funder::Pool { .. } => self
                                        .before
                                        .values()
                                        .filter_map(|row| {
                                            let Stored::Live(task) = row else { return None };
                                            if task.funder != ledger.funder {
                                                return None;
                                            }
                                            let after = live(rows, task.number)?;
                                            standing_reset(task, after, rows).map(|_| task.numbers.budget)
                                        })
                                        .sum(),
                                    Funder::Task(_) | Funder::Period { .. } | Funder::Recurring { .. } => 0,
                                };
                                old.numbers.budget.checked_sub(carved) != Some(ledger.numbers.budget)
                            }
                            || (old.closed && !ledger.closed)
                            || (ledger.closed && ledger.numbers.reserved != 0))
                    {
                        return Err("original source identity changed");
                    }
                }
                Stored::Ended(_)
                | Stored::Writer(_)
                | Stored::Pool(_)
                | Stored::Stub(_)
                | Stored::Milestone(_)
                | Stored::History(_)
                | Stored::PersonProposal(_) => {}
            }
        }
        self.before = rows.clone();
        self.charged = None;
        self.procedure = None;
        Ok(())
    }

    pub fn reset(&mut self, rows: &BTreeMap<Key, Stored>) {
        self.before = rows.clone();
        self.charged = None;
        self.procedure = None;
    }
}
