//! Translate a local assignment into Smith's charter without adding policy
//! (domain/hosts.md, section 8; smith's `run.md`, section 3).

use alloc::boxed::Box;
use skein_lib::List;
use smith::run;
use smith_domain as smith;

use crate::boundary::{self, Assignment};

fn count(len: usize) -> Option<u32> {
    u32::try_from(len).ok()
}

fn model(model: boundary::Model) -> run::charter::Llm {
    run::charter::Llm {
        prices: run::Prices {
            input: model.prices.input,
            cached: model.prices.cached,
            output: model.prices.output,
            unit: model.prices.unit,
        },
        dialect: model.dialect,
        account: model.account,
        endpoint: run::charter::Endpoint(model.endpoint),
        model: model.name,
        max_tokens: model.max_tokens,
    }
}

fn fields(fields: Box<[boundary::FieldRule]>) -> Option<Box<[run::outcome::FieldRule]>> {
    let mut mapped = List::with_capacity(count(fields.len())?);
    for field in fields {
        mapped.push(run::outcome::FieldRule { name: field.name, max: field.max }).ok()?;
    }
    Some(mapped.into_boxed())
}

fn text(rule: boundary::TextRule) -> Option<run::outcome::TextSpec> {
    Some(run::outcome::TextSpec { max: rule.max, fields: fields(rule.fields)? })
}

fn items(items: boundary::Items) -> Option<run::outcome::ItemSpec> {
    let mut kinds = List::with_capacity(count(items.kinds.len())?);
    for kind in items.kinds {
        kinds.push(run::outcome::ItemRule { kind: kind.kind, fields: fields(kind.fields)? }).ok()?;
    }
    Some(run::outcome::ItemSpec { min: items.min, max: items.max, kinds: kinds.into_boxed() })
}

fn outcome(contract: boundary::Contract) -> Option<run::outcome::OutcomeSpec> {
    let mut verdicts = List::with_capacity(count(contract.verdicts.len())?);
    for verdict in contract.verdicts {
        verdicts
            .push(run::outcome::VerdictRule {
                name: verdict.name,
                text_max: verdict.text_max,
                fields: fields(verdict.fields)?,
                items: items(verdict.items)?,
            })
            .ok()?;
    }
    Some(run::outcome::OutcomeSpec {
        change: None,
        verdicts: verdicts.into_boxed(),
        report: match contract.report {
            Some(rule) => Some(text(rule)?),
            None => None,
        },
        failure: match contract.failure {
            Some(rule) => Some(text(rule)?),
            None => None,
        },
    })
}

/// The Smith-owned values prepared for one local start.
pub(crate) struct Start {
    pub(crate) charter: run::Charter,
    pub(crate) transcript: Option<smith::Transcript>,
    pub(crate) answered: Box<[smith::AnsweredCall]>,
    pub(crate) grants: Box<[smith::Grant]>,
}

/// Move a core-shaped local assignment into Smith's typed start values.
pub(crate) fn start(assignment: Assignment) -> Option<Start> {
    let mut sections = List::with_capacity(count(assignment.brief.len())?);
    for section in assignment.brief {
        sections.push(run::Section { title: section.title, text: section.text }).ok()?;
    }
    let mut tools = List::with_capacity(count(assignment.charter.tools.len())?);
    for tool in assignment.charter.tools {
        tools
            .push(run::charter::HostTool {
                name: tool.name,
                description: tool.description,
                schema: tool.schema,
                effect: match tool.effect {
                    boundary::ToolEffect::Read => run::HostEffect::Read,
                    boundary::ToolEffect::Write => run::HostEffect::Write,
                },
                timeout: tool.timeout,
            })
            .ok()?;
    }
    let mut models = List::with_capacity(count(assignment.charter.models.len())?);
    for candidate in assignment.charter.models {
        models.push(model(candidate)).ok()?;
    }
    let mut grants = List::with_capacity(count(assignment.grants.len())?);
    for grant in assignment.grants {
        grants
            .push(smith::Grant {
                name: smith::GrantName { account: grant.account, generation: grant.generation },
                valid: grant.valid,
            })
            .ok()?;
    }
    let charter = run::Charter {
        resume: assignment.charter.resumes && assignment.transcript.is_some(),
        waiting: assignment.charter.waiting,
        instructions: assignment.charter.instructions,
        brief: run::Brief { sections: sections.into_boxed() },
        conventions: None,
        grants: run::charter::Grants {
            wait: assignment.charter.wait,
            deliver: None,
            tools: run::charter::Tools { inspect: false, modify: false, shell: false },
            agents: assignment.charter.agents,
            host_tools: tools.into_boxed(),
        },
        outcome: outcome(assignment.charter.contract)?,
        budget: run::Budget {
            turns: assignment.charter.budget.turns,
            spend: assignment.charter.budget.spend,
            time: assignment.charter.budget.time,
        },
        llm: model(assignment.charter.model),
        models: models.into_boxed(),
    };
    Some(Start { charter, transcript: assignment.transcript, answered: assignment.calls, grants: grants.into_boxed() })
}
