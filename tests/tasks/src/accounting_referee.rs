//! Independent conservation from durable task allocations, ended transitions
//! and admitted priced inputs (domain/tasks.md, 2 and 5).
use jig_core_tasks::{Funder, Key, Stored, TaskRecord};
use std::collections::BTreeMap;

#[derive(Default, Debug)]
pub struct Accounting {
    before: BTreeMap<Key, Stored>,
    charged: Option<(u64, u64)>,
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

impl Accounting {
    fn newly_retired_pools(&self, rows: &BTreeMap<Key, Stored>, period: Funder) -> u64 {
        self.before
            .values()
            .filter_map(|row| {
                let Stored::Ledger(old) = row else { return None };
                if old.parent != Some(period) || old.closed {
                    return None;
                }
                let Some(Stored::Ledger(new)) = rows.get(&Key::Ledger(old.funder)) else { return None };
                if !new.closed {
                    return None;
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

    fn delta(&self, task: u64) -> u64 {
        self.charged.filter(|(number, _)| *number == task).map_or(0, |(_, delta)| delta)
    }

    /// # Errors
    /// Rejects changed identities, missing postings, lost reservations or
    /// invented expense at the durable boundary (domain/tasks.md, 2).
    #[expect(clippy::too_many_lines, reason = "accounting referee checks all durable row forms")]
    pub fn committed(&mut self, rows: &BTreeMap<Key, Stored>) -> Result<(), &'static str> {
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
            if (old.funder != task.funder && !recurring_reset) || old.numbers.budget != task.numbers.budget {
                return Err("allocation identity or promise changed");
            }
            let posted = self.newly_settled(rows, Funder::Task(old.number));
            if task.numbers.spent != old.numbers.spent + self.delta(old.number)
                || task.numbers.spent_below != old.numbers.spent_below + posted
            {
                return Err("task expense or actual descendant posting differs");
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
                            | Stored::Ended(_)
                            | Stored::Stub(_)
                            | Stored::Ledger(_)
                            | Stored::History(_)
                            | Stored::PersonProposal(_) => None,
                        })
                        .sum();
                    if reserved.checked_add(task.run_reserved) != Some(task.numbers.reserved) {
                        return Err("task reservation differs from actual funding links");
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
                            | Stored::Ended(_)
                            | Stored::Stub(_)
                            | Stored::Ledger(_)
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
                    if ledger.numbers.reserved != reserved
                        || ledger.numbers.spent != 0
                        || ledger.numbers.spent_below != before_posted + posted + retired
                    {
                        return Err("external reservation or actual settlement posting differs");
                    }
                    if let Some(Stored::Ledger(old)) = self.before.get(key)
                        && (ledger.funder != old.funder
                            || ledger.parent != old.parent
                            || ledger.numbers.budget != old.numbers.budget
                            || (old.closed && !ledger.closed)
                            || (ledger.closed && ledger.numbers.reserved != 0))
                    {
                        return Err("original source identity changed");
                    }
                }
                Stored::Ended(_)
                | Stored::Writer(_)
                | Stored::Stub(_)
                | Stored::History(_)
                | Stored::PersonProposal(_) => {}
            }
        }
        self.before = rows.clone();
        self.charged = None;
        Ok(())
    }

    pub fn reset(&mut self, rows: &BTreeMap<Key, Stored>) {
        self.before = rows.clone();
        self.charged = None;
    }
}
