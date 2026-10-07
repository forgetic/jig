//! Durable, level-triggered connector procedures (domain/connectors.md, section 6).
use alloc::boxed::Box;
use skein_lib::{Env, Queue, Wall};

use crate::domain::Domain;
use crate::{
    Effect, Limits, ProcedureAction, ProcedureResult, ProcedureSignal, ProcedureSpec, ProcedureState, Record, Request,
    StepDecision,
};

fn program(domain: &Domain, number: u16) -> Option<ProcedureSpec> {
    for spec in &domain.config.procedures {
        if spec.number == number {
            return Some(spec.clone());
        }
    }
    None
}

/// One incoming event merely gives a durable state machine another chance to
/// decide from its state and current facts. The event is never its input data.
pub(crate) fn step(
    domain: &mut Domain,
    env: &Env<Limits>,
    task: u64,
    number: u16,
    resource: crate::Path,
    signal: ProcedureSignal,
    out: &mut Queue<Request>,
) {
    let Some(spec) = program(domain, number) else {
        out.push(Request::Step { task, decision: StepDecision::Hold { reason: 0 } });
        return;
    };
    let new_state = !domain.procedures.contains_key(&task);
    let mut state = if let Some(existing) = domain.procedures.get(&task) {
        if existing.number != number || existing.resource != resource {
            out.push(Request::Step { task, decision: StepDecision::Hold { reason: 0 } });
            return;
        }
        existing.clone()
    } else {
        if signal != ProcedureSignal::Activate || domain.procedures.len() >= env.limits.procedures {
            return;
        }
        ProcedureState { task, number, resource, index: 0, steps: 0, awaiting: false, deadline: None }
    };
    let original = state.clone();
    if usize::from(state.index) >= spec.actions.len() {
        return;
    }
    if signal == ProcedureSignal::Cancel {
        state.index = u16::try_from(spec.actions.len()).expect("program length admitted");
        state.awaiting = false;
        save(domain, state, out);
        out.push(Request::Step { task, decision: StepDecision::Finish { result: ProcedureResult::Local { code: 0 } } });
        return;
    }
    if state.awaiting {
        let expected = match spec.actions.get(usize::from(state.index)) {
            Some(ProcedureAction::Effect { .. }) => ProcedureSignal::Settled,
            Some(ProcedureAction::Delegate { .. }) => ProcedureSignal::DelegateDone,
            Some(ProcedureAction::Propose { .. }) => ProcedureSignal::ProposalDone,
            Some(_) | None => return,
        };
        if signal == expected {
            state.awaiting = false;
            state.index = state.index.saturating_add(1);
        } else {
            return;
        }
    }
    if usize::from(state.index) >= spec.actions.len() {
        save_change(domain, state, &original, new_state, out);
        return;
    }
    if state.steps >= spec.max_steps {
        save_change(domain, state, &original, new_state, out);
        out.push(Request::Step { task, decision: StepDecision::Hold { reason: u16::MAX } });
        return;
    }
    if advance_met_waits(domain, &spec, &mut state) {
        save_change(domain, state, &original, new_state, out);
        return;
    }
    let decision = decide(domain, env, task, &spec, &mut state, out);
    if new_state || state != original {
        state.steps = state.steps.saturating_add(1);
        save(domain, state, out);
    }
    out.push(Request::Step { task, decision });
}

fn decide(
    domain: &mut Domain,
    env: &Env<Limits>,
    task: u64,
    spec: &ProcedureSpec,
    state: &mut ProcedureState,
    out: &mut Queue<Request>,
) -> StepDecision {
    match spec.actions.get(usize::from(state.index)).expect("unfinished program has an action") {
        ProcedureAction::Effect { kind, purpose, target } => {
            let condition = match crate::requirements::current(domain, &state.resource) {
                Some(fact) => fact.state,
                None => None,
            };
            state.awaiting = true;
            StepDecision::Effect(Effect {
                kind: *kind,
                resources: Box::from([state.resource.clone()]),
                purpose: *purpose,
                condition,
                target: *target,
                state: *target,
            })
        }
        ProcedureAction::Delegate { kinds } => {
            state.awaiting = true;
            StepDecision::Delegate { kinds: kinds.clone() }
        }
        ProcedureAction::Propose { number } => {
            state.awaiting = true;
            StepDecision::Propose { number: *number }
        }
        ProcedureAction::Wait { .. } => {
            let deadline = match state.deadline {
                Some(deadline) => deadline,
                None => Wall::from_nanos(env.wall.as_nanos().saturating_add(spec.stall.as_nanos())),
            };
            state.deadline = Some(deadline);
            if env.wall >= deadline {
                StepDecision::Hold { reason: u16::MAX - 1 }
            } else {
                StepDecision::Wait { deadline }
            }
        }
        ProcedureAction::Stall => StepDecision::Stall,
        ProcedureAction::Hold { reason } => StepDecision::Hold { reason: *reason },
        ProcedureAction::Finish { result } => {
            state.index = state.index.saturating_add(1);
            if let ProcedureResult::Local { code } = result {
                domain.results.insert(task, *code).expect("one result per live procedure");
                out.push(Request::Save { record: Record::Result { task, code: *code } });
            }
            StepDecision::Finish { result: *result }
        }
    }
}

fn advance_met_waits(domain: &Domain, spec: &ProcedureSpec, state: &mut ProcedureState) -> bool {
    // Passing an already-met wait is a state transition, not a new decision.
    while let Some(ProcedureAction::Wait { state: wanted }) = spec.actions.get(usize::from(state.index)) {
        let observed = crate::requirements::current(domain, &state.resource);
        let met = match observed {
            Some(fact) => fact.state == Some(*wanted) && !fact.pending,
            None => false,
        };
        if !met {
            break;
        }
        state.index = state.index.saturating_add(1);
        state.deadline = None;
    }
    usize::from(state.index) >= spec.actions.len()
}

fn save(domain: &mut Domain, state: ProcedureState, out: &mut Queue<Request>) {
    domain.procedures.insert(state.task, state.clone()).expect("procedure capacity checked");
    out.push(Request::Save { record: Record::Procedure(state) });
}

fn save_change(
    domain: &mut Domain,
    state: ProcedureState,
    original: &ProcedureState,
    new_state: bool,
    out: &mut Queue<Request>,
) {
    if new_state || state != *original {
        save(domain, state, out);
    }
}

pub(crate) fn next_deadline(domain: &Domain) -> Option<Wall> {
    let mut first = None;
    for (_, state) in &domain.procedures {
        if let Some(deadline) = state.deadline
            && match first {
                Some(current) => deadline < current,
                None => true,
            }
        {
            first = Some(deadline);
        }
    }
    first
}
