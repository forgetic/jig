//! Optional bounded content-free observations (domain/tasks.md, section 11). Facts are diagnostic; retaining or dropping them changes no
//! lifecycle, reply, financial posting or durability decision.
use crate::{Class, Hold, Party, Status};

/// Optional content-free task observation in a bounded queue; it is neither a durable
/// acknowledgement nor a requester delivery credit, and dropping it changes no decision.
/// (domain/tasks.md, section 5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Fact {
    /// New task admitted and its save requested; root durability still follows.
    Made { task: u64, requester: Party },
    /// New fenced activation claimed and its save requested.
    Claimed {
        task: u64,
        /** `Exact` root-issued `new` claim identity. */
        attempt: u64,
    },
    /// One activation failure classified and counted.
    Failed {
        /** Task whose classified activation failure was counted. */
        task: u64,
        /** Failure category counted in its retry history. */
        class: Class,
    },
    /// Task entered a hold; does not acknowledge any `Stop` or root decision.
    Held {
        /** Task newly held for a decision. */
        task: u64,
        /** Reason for the `new` hold. */
        why: Hold,
    },
    /// Task settled and emitted its final financial/persistence/requester outputs; not a delivery
    /// or durability acknowledgement.
    Ended {
        /** Task removed from the live arena after settlement. */
        task: u64,
        /** Content-free final classification, separate from the bounded result bytes. */
        status: Status,
    },
}
