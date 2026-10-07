//! Connector-owned facts and exact verdicts (domain/connectors.md, section 7).
use skein_lib::{Env, List, Queue, Token};

use crate::domain::{Domain, resource_spec};
use crate::{Fact, Limits, Origin, Path, Record, Request, ResourceRole, SystemRequest, Verdict};

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Question {
    pub(crate) requirement: u16,
    pub(crate) resource: Path,
    pub(crate) state: u64,
}

pub(crate) fn requirement(domain: &Domain, number: u16) -> Option<crate::RequirementSpec> {
    for spec in &domain.config.requirements {
        if spec.number == number {
            return Some(*spec);
        }
    }
    None
}

pub(crate) fn current(domain: &Domain, path: &Path) -> Option<Fact> {
    for (resource, fact) in &domain.facts {
        if resource == path {
            return Some(*fact);
        }
    }
    None
}

pub(crate) fn verdict(domain: &Domain, env: &Env<Limits>, question: &Question) -> Verdict {
    let Some(spec) = requirement(domain, question.requirement) else {
        return Verdict::Refuse { actual: None };
    };
    let Some(fact) = current(domain, &question.resource) else {
        return Verdict::Wait;
    };
    let age = env.wall.as_nanos().saturating_sub(fact.observed.as_nanos());
    if fact.pending || fact.observed > env.wall || age > spec.freshness.as_nanos() {
        return Verdict::Wait;
    }
    if fact.state == Some(question.state) {
        Verdict::Met { state: question.state, observed: fact.observed, guarded: spec.guarded }
    } else {
        Verdict::Refuse { actual: fact.state }
    }
}

pub(crate) fn judge(
    domain: &mut Domain,
    env: &Env<Limits>,
    token: Token,
    number: u16,
    resources: &[Path],
    state: u64,
    out: &mut Queue<Request>,
) {
    if resources.is_empty() || requirement(domain, number).is_none() {
        out.push(Request::Verdict { token, verdict: Verdict::Refuse { actual: None } });
        return;
    }
    let path = resources.first().expect("nonempty question checked");
    if resource_spec(&domain.config, path).is_none() {
        out.push(Request::Verdict { token, verdict: Verdict::Refuse { actual: None } });
        return;
    }
    let question = Question { requirement: number, resource: path.clone(), state };
    let answer = verdict(domain, env, &question);
    out.push(Request::Verdict { token, verdict: answer });
    if answer == Verdict::Wait {
        if domain.judges.contains_key(&token) || domain.judges.len() < env.limits.judges {
            domain.judges.insert(token, question).expect("judge capacity checked");
        }
        out.push(Request::System(SystemRequest::ReadFact { resource: path.clone(), observed: env.wall }));
    } else {
        domain.judges.remove(&token);
    }
}

pub(crate) fn fact(
    domain: &mut Domain,
    env: &Env<Limits>,
    resource: Path,
    fact: Fact,
    origin: Origin,
    out: &mut Queue<Request>,
) {
    if resource_spec(&domain.config, &resource).is_none()
        || domain.facts.len() >= env.limits.facts && !domain.facts.contains_key(&resource)
    {
        return;
    }
    let old = domain.facts.insert(resource.clone(), fact).expect("fact capacity checked");
    if old == Some(fact) {
        return;
    }
    out.push(Request::Changed { resource: resource.clone() });
    let changed = match old {
        Some(previous) => previous.state != fact.state,
        None => false,
    };
    if origin == Origin::Other && changed {
        let mut tasks: List<u64> = List::with_capacity(env.limits.tasks);
        for (task, record) in &domain.tasks {
            if let Record::Task { project, resources, .. } = record
                && resources.contains(&resource)
                && domain.adoptions.get(&(*project, resource.clone())) == Some(&ResourceRole::Owned)
            {
                tasks.push(*task).expect("one task per live row");
            }
        }
        if !tasks.is_empty() {
            out.push(Request::DriftResource { tasks: tasks.into_boxed(), resource: resource.clone() });
        }
    }
    let mut answered: List<Token> = List::with_capacity(env.limits.judges);
    for (token, question) in &domain.judges {
        if question.resource == resource {
            let answer = verdict(domain, env, question);
            if answer != Verdict::Wait {
                out.push(Request::Verdict { token: *token, verdict: answer });
                answered.push(*token).expect("judge count within limit");
            }
        }
    }
    for token in &answered {
        domain.judges.remove(token);
    }
}
