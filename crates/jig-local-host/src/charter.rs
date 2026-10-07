//! Adapt one local assignment to the shared charter translation
//! (domain/hosts.md, section 8).

use alloc::boxed::Box;
use skein_lib::List;
use smith_domain as smith;

use crate::boundary::Assignment;

/// The Smith-owned values prepared for one local start.
pub(crate) struct Start {
    pub(crate) charter: smith::run::Charter,
    pub(crate) transcript: Option<smith::Transcript>,
    pub(crate) answered: Box<[smith::AnsweredCall]>,
    pub(crate) grants: Box<[smith::Grant]>,
}

/// Move a core-shaped local assignment into Smith's typed start values.
pub(crate) fn start(assignment: Assignment) -> Option<Start> {
    let resumed = assignment.transcript.is_some();
    let charter = jig_charter::charter(assignment.charter, assignment.brief, resumed)?;
    let mut grants = List::with_capacity(u32::try_from(assignment.grants.len()).ok()?);
    for grant in assignment.grants {
        grants
            .push(smith::Grant {
                name: smith::GrantName { account: grant.account, generation: grant.generation },
                valid: grant.valid,
            })
            .ok()?;
    }
    Some(Start { charter, transcript: assignment.transcript, answered: assignment.calls, grants: grants.into_boxed() })
}
