//! Translate the core charter into Smith's without adding policy
//! (domain/hosts.md, section 8; smith's `run.md`, section 3).

use alloc::boxed::Box;
use skein_lib::List;
use smith_domain_run as run;

use crate::boundary::{self, Charter, Section};

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
        change: match contract.change {
            Some(rule) => {
                Some(run::outcome::ChangeSpec { checks_must_pass: rule.checks_must_pass, fields: fields(rule.fields)? })
            }
            None => None,
        },
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

/// Move the core's bounded charter and ordered brief into Smith's start.
/// `resumed` is true only when a validated transcript accompanies the start.
#[must_use]
pub fn charter(charter: Charter, brief: Box<[Section]>, resumed: bool) -> Option<run::Charter> {
    let mut sections = List::with_capacity(count(brief.len())?);
    for section in brief {
        sections.push(run::Section { title: section.title, text: section.text }).ok()?;
    }
    let mut tools = List::with_capacity(count(charter.tools.len())?);
    for tool in charter.tools {
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
    let mut models = List::with_capacity(count(charter.models.len())?);
    for candidate in charter.models {
        models.push(model(candidate)).ok()?;
    }
    let outcome = outcome(charter.contract)?;
    Some(run::Charter {
        resume: charter.resumes && resumed,
        waiting: charter.waiting,
        instructions: charter.instructions,
        brief: run::Brief { sections: sections.into_boxed() },
        conventions: match charter.conventions {
            Some(paths) => Some(run::Conventions { guide: paths.guide, checks: paths.checks }),
            None => None,
        },
        grants: run::charter::Grants {
            wait: charter.wait,
            deliver: outcome.change.clone(),
            tools: run::charter::Tools {
                inspect: charter.workspace.inspect,
                modify: charter.workspace.modify,
                shell: charter.workspace.shell,
            },
            agents: charter.agents,
            host_tools: tools.into_boxed(),
        },
        outcome,
        budget: run::Budget { turns: charter.budget.turns, spend: charter.budget.spend, time: charter.budget.time },
        llm: model(charter.model),
        models: models.into_boxed(),
    })
}
