//! The core-owned charter vocabulary toward Smith (domain/hosts.md, section 8).
//! Its sections, tools and contracts name no application system.

use alloc::boxed::Box;
use skein_lib::Duration;

/// One model the charter may use, with prices in the deployment's unit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Model {
    pub prices: Prices,
    pub dialect: u32,
    pub account: u32,
    pub endpoint: u32,
    pub name: Box<[u8]>,
    pub max_tokens: u32,
}

/// Prices per `unit` tokens, rounded once for each completion by Smith.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Prices {
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub unit: u32,
}

/// One ordered section of the run's brief.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Section {
    pub title: Box<[u8]>,
    pub text: Box<[u8]>,
}

/// A host tool declared by the core or an application connector.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Tool {
    pub name: Box<[u8]>,
    pub description: Box<[u8]>,
    pub schema: Box<[u8]>,
    pub effect: ToolEffect,
    pub timeout: Duration,
}

/// Whether a host tool may run beside another read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolEffect {
    /// The call reads without changing the host's state.
    Read,
    /// The call can change the host's state.
    Write,
}

/// Maximum use of the activation's turns, spend and time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Budget {
    pub turns: u32,
    pub spend: u64,
    pub time: Duration,
}

/// One required named value in a result.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FieldRule {
    pub name: Box<[u8]>,
    pub max: u32,
}

/// The text and fields of a report or declared failure.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TextRule {
    pub max: u32,
    pub fields: Box<[FieldRule]>,
}

/// One kind of item a verdict may include.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ItemRule {
    pub kind: Box<[u8]>,
    pub fields: Box<[FieldRule]>,
}

/// Counts and kinds of a verdict's follow-up items.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Items {
    pub min: u32,
    pub max: u32,
    pub kinds: Box<[ItemRule]>,
}

/// One permitted verdict label and its required shape.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VerdictRule {
    pub name: Box<[u8]>,
    pub text_max: u32,
    pub fields: Box<[FieldRule]>,
    pub items: Items,
}

/// The permitted result forms for a run with no workspace.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Contract {
    pub report: Option<TextRule>,
    pub failure: Option<TextRule>,
    pub verdicts: Box<[VerdictRule]>,
    pub change: Option<ChangeRule>,
}

/// Workspace delivery fields required by a declared change.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChangeRule {
    pub checks_must_pass: bool,
    pub fields: Box<[FieldRule]>,
}

/// Guide and check paths interpreted only by Smith's workspace tools.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Conventions {
    pub guide: Box<[u8]>,
    pub checks: Box<[u8]>,
}

/// Explicit workspace tool rights, all absent for an engine-local run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WorkspaceTools {
    pub inspect: bool,
    pub modify: bool,
    pub shell: bool,
}

/// One core charter with its explicit tool rights.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Charter {
    pub instructions: Box<[u8]>,
    pub tools: Box<[Tool]>,
    pub wait: bool,
    pub agents: bool,
    pub workspace: WorkspaceTools,
    pub conventions: Option<Conventions>,
    pub contract: Contract,
    pub budget: Budget,
    pub model: Model,
    pub models: Box<[Model]>,
    pub waiting: Duration,
    pub resumes: bool,
}
