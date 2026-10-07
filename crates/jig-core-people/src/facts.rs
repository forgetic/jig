//! Optional content-free child observations.
//! People emits these through its bounded diagnostic queue; dropping them
//! changes no decision. They neither persist state nor acknowledge root commits.

/// Content-free observations; keeping or dropping them changes no decision.
/// Optional child-to-parent content-free observation in a bounded queue; it is not a durable-write
/// or request-terminal acknowledgement.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Fact {
    /// A new sign-in was admitted, with persistence requested from the parent.
    SignedIn {
        person: u64,
        /** Newly admitted root-issued sign-in number. */
        sign_in: u64,
    },
    /// An existing sign-in was removed and its erase requested.
    SignedOut {
        /** Root-issued sign-in number that was removed or expired. */
        sign_in: u64,
    },
    /// A new eligible keyed request routed once to the root.
    Routed {
        /** Authenticated requester whose new flight routed. */
        person: u64,
    },
    /// A permanent keyed outcome was retained and its save requested; parent durability still
    /// governs replies.
    Answered {
        /** Person whose permanent keyed answer was saved in the child. */
        person: u64,
    },
}
