//! Write the Smith charter codec from the shared charter
//! (domain/hosts.md, section 8; smith's `protocol/charter.md`, section 5).
//! The configured endpoint table supplies names; the charter keeps numbers.

use alloc::boxed::Box;
use skein_lib::{List, Writer};
use smith_charter::v1 as wire;
use smith_domain_run as run;

/// One configured endpoint name and its domain-visible identities.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EndpointName {
    pub number: u32,
    pub dialect: u32,
    pub account: u32,
    pub name: Box<[u8]>,
}

fn count(length: usize) -> Option<u32> {
    u32::try_from(length).ok()
}

fn endpoint(value: &run::charter::Llm, names: &[EndpointName]) -> Option<Box<[u8]>> {
    for name in names {
        if name.number == value.endpoint.0 && name.dialect == value.dialect && name.account == value.account {
            return Some(name.name.clone());
        }
    }
    None
}

fn llm(value: run::charter::Llm, names: &[EndpointName], limits: &wire::Limits) -> Option<wire::Llm> {
    let name = endpoint(&value, names)?;
    let prices = wire::Prices::new(
        limits,
        wire::PricesParts {
            input: value.prices.input,
            cached: value.prices.cached,
            output: value.prices.output,
            unit: value.prices.unit,
        },
    )
    .ok()?;
    wire::Llm::new(limits, wire::LlmParts { endpoint: name, model: value.model, max_tokens: value.max_tokens, prices })
        .ok()
}

fn fields(source: Box<[run::outcome::FieldRule]>, limits: &wire::Limits) -> Option<List<wire::FieldRule>> {
    let mut out = List::with_capacity(count(source.len())?);
    for field in source {
        out.push(wire::FieldRule::new(limits, wire::FieldRuleParts { name: field.name, max: field.max }).ok()?).ok()?;
    }
    Some(out)
}

fn text(source: run::outcome::TextSpec, limits: &wire::Limits) -> Option<wire::TextRule> {
    wire::TextRule::new(limits, wire::TextRuleParts { max: source.max, fields: fields(source.fields, limits)? }).ok()
}

fn change(source: run::outcome::ChangeSpec, limits: &wire::Limits) -> Option<wire::ChangeRule> {
    wire::ChangeRule::new(
        limits,
        wire::ChangeRuleParts { checks_must_pass: source.checks_must_pass, fields: fields(source.fields, limits)? },
    )
    .ok()
}

fn verdict(source: run::outcome::VerdictRule, limits: &wire::Limits) -> Option<wire::VerdictRule> {
    let mut kinds = List::with_capacity(count(source.items.kinds.len())?);
    for kind in source.items.kinds {
        kinds
            .push(
                wire::ItemKind::new(
                    limits,
                    wire::ItemKindParts { kind: kind.kind, fields: fields(kind.fields, limits)? },
                )
                .ok()?,
            )
            .ok()?;
    }
    wire::VerdictRule::new(
        limits,
        wire::VerdictRuleParts {
            label: source.name,
            text_max: source.text_max,
            fields: fields(source.fields, limits)?,
            least: source.items.min,
            most: source.items.max,
            kinds,
        },
    )
    .ok()
}

fn contract(source: run::outcome::OutcomeSpec, limits: &wire::Limits) -> Option<wire::Contract> {
    let mut verdicts = List::with_capacity(count(source.verdicts.len())?);
    for item in source.verdicts {
        verdicts.push(verdict(item, limits)?).ok()?;
    }
    wire::Contract::new(
        limits,
        wire::ContractParts {
            report: match source.report {
                Some(rule) => Some(text(rule, limits)?),
                None => None,
            },
            verdicts,
            change: match source.change {
                Some(rule) => Some(change(rule, limits)?),
                None => None,
            },
            failure: match source.failure {
                Some(rule) => Some(text(rule, limits)?),
                None => None,
            },
        },
    )
    .ok()
}

fn tools(source: run::charter::Grants, limits: &wire::Limits) -> Option<wire::Tools> {
    let families = wire::Families::new(
        limits,
        wire::FamiliesParts {
            skein_bools: [source.tools.inspect, source.tools.modify, source.tools.shell, source.agents],
        },
    )
    .ok()?;
    let mut host = List::with_capacity(count(source.host_tools.len())?);
    for tool in source.host_tools {
        let effect = match tool.effect {
            run::HostEffect::Read => wire::Effect::Read,
            run::HostEffect::Write => wire::Effect::Write,
        };
        host.push(
            wire::HostTool::new(
                limits,
                wire::HostToolParts {
                    name: tool.name,
                    description: tool.description,
                    input: tool.schema,
                    effect,
                    deadline: tool.timeout,
                },
            )
            .ok()?,
        )
        .ok()?;
    }
    wire::Tools::new(
        limits,
        wire::ToolsParts {
            families,
            wait: source.wait,
            deliver: match source.deliver {
                Some(rule) => Some(change(rule, limits)?),
                None => None,
            },
            host,
        },
    )
    .ok()
}

/// Encode the complete charter for worker transport or an inline start.
/// The endpoint identities must match their configured Smith wire names.
#[must_use]
pub fn encode(source: run::Charter, names: &[EndpointName], limits: &wire::Limits) -> Option<Box<[u8]>> {
    let mut brief = List::with_capacity(count(source.brief.sections.len())?);
    for section in source.brief.sections {
        brief
            .push(wire::Section::new(limits, wire::SectionParts { title: section.title, body: section.text }).ok()?)
            .ok()?;
    }
    let mut models = List::with_capacity(count(source.models.len())?);
    for candidate in source.models {
        models.push(llm(candidate, names, limits)?).ok()?;
    }
    let conventions = match source.conventions {
        Some(paths) => Some(
            wire::Conventions::new(limits, wire::ConventionsParts { guide: paths.guide, checks: paths.checks }).ok()?,
        ),
        None => None,
    };
    let budget = wire::Budget::new(
        limits,
        wire::BudgetParts { turns: source.budget.turns, spend: source.budget.spend, time: source.budget.time },
    )
    .ok()?;
    let charter = wire::Charter::new(
        limits,
        wire::CharterParts {
            instructions: source.instructions,
            brief,
            tools: tools(source.grants, limits)?,
            contract: contract(source.outcome, limits)?,
            conventions,
            budget,
            main: llm(source.llm, names, limits)?,
            models,
            waiting: source.waiting,
            resume: source.resume,
        },
    )
    .ok()?;
    let size = usize::try_from(charter.measure()).ok()?;
    let mut writer = Writer::new(size);
    charter.encode(&mut writer).ok()?;
    Some(writer.finish())
}
