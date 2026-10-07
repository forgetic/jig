//! Typed person amendment payloads (domain/people.md, 5.1; domain/tasks.md, 6).
//! People retains these only with keyed requests. The root translates them to
//! tasks and checks current policy; this child never sees task records.
use alloc::boxed::Box;
use core::mem::size_of;
use skein_lib::{Duration, Wall};

/// A person's whole optional task edit and bounded explanation.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Amendment {
    pub spec: Option<Spec>,
    pub wake: Option<WakePolicy>,
    pub dependencies: Option<Box<[u64]>>,
    pub authority: Option<Authority>,
    pub reason: Box<[u8]>,
}

/// Human supplied words, typed parameters and historical inputs.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Spec {
    pub words: Box<[u8]>,
    pub parameters: Box<[Parameter]>,
    pub inputs: Box<[u64]>,
}

/// One typed human supplied specification parameter.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Parameter {
    Number { name: u32, value: u64 },
    Bytes { name: u32, value: Box<[u8]> },
    Resource { name: u32, connector: u16, resource: u64 },
}

/// Wake rules supplied by a person for the task's later messages.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WakePolicy {
    pub words: WakeRule,
    pub notices: WakeRule,
    pub news: WakeRule,
    pub results: ResultsWake,
    pub questions: bool,
    pub answers: bool,
    pub timers: bool,
}

/// One way a class of messages wakes a task.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WakeRule {
    Never,
    Immediate,
    Batch { count: u32, age: Duration },
}

/// How delegate results wake their requester.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ResultsWake {
    Never,
    Each,
    LastOrFailure,
}

/// Human requested authority value checked against role and deployment ceilings by root.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Authority {
    pub tools: u64,
    pub grants: Box<[Grant]>,
    pub delegation: Delegation,
    pub spend: u64,
    pub deadline: Option<Wall>,
    pub notes: u8,
    pub note_resources: Box<[ResourceScope]>,
}

/// One connector resource pattern in a person's requested note authority.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ResourceScope {
    pub connector: u16,
    pub pattern: Pattern,
}

/// One connector resource grant in a person's requested authority.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Grant {
    pub connector: u16,
    pub kind: u16,
    pub pattern: Pattern,
}

/// Literal segments and terminal coverage of one resource grant.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Pattern {
    pub segments: Box<[Box<[u8]>]>,
    pub last: Last,
}

/// Terminal resource coverage requested by a person.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Last {
    Exact(Box<[u8]>),
    Open(Box<[u8]>),
}

/// Allowed child executor identities and structural limits.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Delegation {
    pub kinds: Box<[Executor]>,
    pub tasks: u32,
    pub depth: u32,
}

/// One requested child executor kind.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Executor {
    Charter(u32),
    Procedure(u32),
    Role(u32),
}

/// Checked owned payload size of one amendment, excluding fixed inline fields.
#[must_use]
pub fn amendment_bytes(amendment: &Amendment) -> Option<u64> {
    let mut total = u64::try_from(amendment.reason.len()).ok()?;
    if let Some(spec) = &amendment.spec {
        total = total.checked_add(u64::try_from(spec.words.len()).ok()?)?;
        total = total.checked_add(
            u64::try_from(spec.parameters.len()).ok()?.checked_mul(u64::try_from(size_of::<Parameter>()).ok()?)?,
        )?;
        total = total.checked_add(u64::try_from(spec.inputs.len()).ok()?.checked_mul(8)?)?;
        for parameter in &spec.parameters {
            match parameter {
                Parameter::Bytes { value, .. } => total = total.checked_add(u64::try_from(value.len()).ok()?)?,
                Parameter::Number { .. } | Parameter::Resource { .. } => {}
            }
        }
    }
    if let Some(dependencies) = &amendment.dependencies {
        total = total.checked_add(u64::try_from(dependencies.len()).ok()?.checked_mul(8)?)?;
    }
    if let Some(authority) = &amendment.authority {
        total = total.checked_add(authority_bytes(authority)?)?;
    }
    Some(total)
}

/// Checked deep-byte size of a person's authority value.
#[must_use]
pub fn authority_bytes(authority: &Authority) -> Option<u64> {
    let mut total = u64::try_from(authority.grants.len()).ok()?.checked_mul(u64::try_from(size_of::<Grant>()).ok()?)?;
    total = total.checked_add(
        u64::try_from(authority.note_resources.len())
            .ok()?
            .checked_mul(u64::try_from(size_of::<ResourceScope>()).ok()?)?,
    )?;
    total = total.checked_add(
        u64::try_from(authority.delegation.kinds.len())
            .ok()?
            .checked_mul(u64::try_from(size_of::<Executor>()).ok()?)?,
    )?;
    for grant in &authority.grants {
        total = total.checked_add(
            u64::try_from(grant.pattern.segments.len())
                .ok()?
                .checked_mul(u64::try_from(size_of::<Box<[u8]>>()).ok()?)?,
        )?;
        for segment in &grant.pattern.segments {
            total = total.checked_add(u64::try_from(segment.len()).ok()?)?;
        }
        total = total.checked_add(match &grant.pattern.last {
            Last::Exact(word) | Last::Open(word) => u64::try_from(word.len()).ok()?,
        })?;
    }
    for scope in &authority.note_resources {
        total = total.checked_add(
            u64::try_from(scope.pattern.segments.len())
                .ok()?
                .checked_mul(u64::try_from(size_of::<Box<[u8]>>()).ok()?)?,
        )?;
        for segment in &scope.pattern.segments {
            total = total.checked_add(u64::try_from(segment.len()).ok()?)?;
        }
        total = total.checked_add(match &scope.pattern.last {
            Last::Exact(word) | Last::Open(word) => u64::try_from(word.len()).ok()?,
        })?;
    }
    Some(total)
}
