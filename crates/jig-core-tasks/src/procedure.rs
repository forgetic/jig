//! Fenced procedure decisions at the tasks hub (domain/tasks.md, sections 5.3 and 8.2).
//! The owner keeps procedure state and chooses a step; tasks keeps only the
//! executor identity, live topology, result contract and the step's offered inbox fence. It never
//! knows connector facts or effects. A due step may make one whole batch,
//! finish after its delegates, hold, or wait for changed facts.
use crate::domain::{Domain, make, publish, record, refused, task_mut};
use crate::{
    Active, Closing, Ending, Executor, Limits, Party, Phase, ProcedureDecision, Refusal, Request, Stage, TaskResult,
};
use skein_lib::{Env, Queue, ReplyTo};

#[expect(clippy::too_many_arguments, reason = "a procedure decision carries its offered inbox fence")]
pub(crate) fn stepped(
    domain: &mut Domain,
    env: &Env<Limits>,
    to: ReplyTo,
    number: u64,
    step: u64,
    read: Option<u64>,
    decision: ProcedureDecision,
    out: &mut Queue<Request>,
) {
    if !domain.ready() {
        return refused(to, Some(number), Refusal::NotReady, out);
    }
    let Some(task) = record(domain, number) else {
        return refused(to, Some(number), Refusal::Unknown, out);
    };
    match task.executor {
        Executor::Procedure { .. } => {}
        Executor::Agent { .. } | Executor::Person(_) => return refused(to, Some(number), Refusal::Executor, out),
    }
    if task.phase != Phase::Active(Active::Due) || task.attempt.checked_add(1) != Some(step) {
        return refused(to, Some(number), Refusal::Attempt, out);
    }
    if read != domain.procedure_offered(number) {
        return refused(to, Some(number), Refusal::Read, out);
    }
    match &decision {
        ProcedureDecision::Delegate(batch) => {
            if let Err(problem) = crate::batch::check(domain, &env.limits, Party::Task(number), batch) {
                return out.push(Request::Refused { reply_to: to, problem });
            }
        }
        ProcedureDecision::Result(result) => {
            if !task.delegates.is_empty() {
                return out.push(Request::Refused {
                    reply_to: to,
                    problem: crate::Problem::live_delegates(number, &task.delegates),
                });
            }
            if !crate::run::valid_result(&task.contract, result, &env.limits) {
                return refused(to, Some(number), Refusal::Contract, out);
            }
        }
        ProcedureDecision::Hold(_) | ProcedureDecision::Wait => {}
    }
    let live = task_mut(domain, number).expect("procedure step preflighted");
    live.record.attempt = step;
    crate::inbox::take(&mut live.record, read);
    live.procedure_offered = None;
    match decision {
        ProcedureDecision::Delegate(batch) => {
            task_mut(domain, number).expect("procedure task live").record.phase = Phase::Active(Active::Idle);
            publish(domain, env, number, out);
            make(domain, env, to, Party::Task(number), batch, out);
        }
        ProcedureDecision::Result(result) => {
            let ending = match result {
                TaskResult::Failure { reason } => Ending::Failed { reason },
                TaskResult::Report { .. } | TaskResult::Verdict { .. } | TaskResult::Change { .. } => {
                    Ending::Done(result)
                }
            };
            task_mut(domain, number).expect("procedure task live").record.phase =
                Phase::Closing(Closing { stage: Stage::Delegates, ending });
            publish(domain, env, number, out);
            out.push(Request::Done { reply_to: to });
        }
        ProcedureDecision::Hold(why) => {
            crate::run::hold(domain, env, number, why, out);
            out.push(Request::Done { reply_to: to });
        }
        ProcedureDecision::Wait => {
            task_mut(domain, number).expect("procedure task live").record.phase = Phase::Active(Active::Idle);
            publish(domain, env, number, out);
            out.push(Request::Done { reply_to: to });
        }
    }
    crate::wake::after_step(domain, env, number, out);
}
