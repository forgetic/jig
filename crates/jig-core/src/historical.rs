//! Validation of retained task results used by core dependency and delegation routes.

use alloc::boxed::Box;
use jig_core_tasks as tasks;
use skein_lib::{List, Token};

use crate::{Core, HistoricalResult};

fn result_notice(ending: tasks::Ending) -> (tasks::ResultKind, Box<[u8]>) {
    match ending {
        tasks::Ending::Done(result) => match result {
            tasks::TaskResult::Report { words } => (tasks::ResultKind::Report, words),
            tasks::TaskResult::Verdict { code, words } => (tasks::ResultKind::Verdict { code }, words),
            tasks::TaskResult::Change { connector, kind, resource, words } => {
                (tasks::ResultKind::Change { connector, kind, resource }, words)
            }
            tasks::TaskResult::Failure { reason } => (tasks::ResultKind::Failed, reason),
        },
        tasks::Ending::Failed { reason } => (tasks::ResultKind::Failed, reason),
        tasks::Ending::Cancelled { reason, .. } => (tasks::ResultKind::Cancelled, reason),
    }
}

impl Core {
    /// Select the historical results required by one preparing run, in their task order.
    #[must_use]
    pub fn preparation_ids(&self, task: u64) -> Option<Box<[u64]>> {
        let context = self.contexts.get(&task)?;
        let count = context.dependencies.len().checked_add(context.spec.inputs.len())?;
        let mut ids = List::with_capacity(u32::try_from(count).ok()?);
        for &dependency in &context.dependencies {
            ids.push(dependency).ok()?;
        }
        for &input in &context.spec.inputs {
            ids.push(input).ok()?;
        }
        Some(ids.into_boxed())
    }

    /// Admit one archived dependency result into the preparing task's context.
    #[must_use]
    pub fn dependency_result(&self, task: u64, wanted: u64, row: Box<tasks::TaskRecord>) -> Option<HistoricalResult> {
        let context = self.contexts.get(&task)?;
        if row.number != wanted || row.project != context.project {
            return None;
        }
        let ending = match row.phase {
            tasks::Phase::Ended(ending) => ending,
            tasks::Phase::Waiting | tasks::Phase::Active(_) | tasks::Phase::Closing(_) | tasks::Phase::Held { .. } => {
                return None;
            }
        };
        let (kind, words) = result_notice(ending);
        Some(HistoricalResult { task: wanted, kind, words })
    }

    /// Admit one archived input as a completed child of the named delegator.
    #[must_use]
    pub fn input_stub(
        &self,
        creator: u64,
        project: u32,
        wanted: u64,
        row: Box<tasks::TaskRecord>,
    ) -> Option<tasks::Stub> {
        if row.number != wanted || row.project != project || row.requester != tasks::Party::Task(creator) {
            return None;
        }
        let phase = match row.phase {
            tasks::Phase::Ended(tasks::Ending::Done(_)) => tasks::Status::Done,
            tasks::Phase::Ended(tasks::Ending::Failed { .. }) => tasks::Status::Failed,
            tasks::Phase::Ended(tasks::Ending::Cancelled { .. }) => tasks::Status::Cancelled,
            tasks::Phase::Waiting | tasks::Phase::Active(_) | tasks::Phase::Closing(_) | tasks::Phase::Held { .. } => {
                return None;
            }
        };
        Some(tasks::Stub { task: wanted, phase, result: Token::new(wanted) })
    }
}
