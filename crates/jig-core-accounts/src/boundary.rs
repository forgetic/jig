use skein_lib::{Duration, Time};

/// A name and the time its token has left; never the token itself.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Grant {
    pub account: u32,
    pub generation: u64,
    pub valid: Duration,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Event {
    /// Configured at startup. None means only a refresh token was kept.
    Add {
        account: u32,
        generation: u64,
        valid: Option<Duration>,
    },
    Refreshed {
        account: u32,
        generation: u64,
        valid: Duration,
    },
    Failed {
        account: u32,
        generation: u64,
        failure: Failure,
    },
    Rejected {
        account: u32,
        generation: u64,
    },
    Exhausted {
        account: u32,
        retry_after: Duration,
    },
    Close {
        account: u32,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Failure {
    Unavailable,
    TimedOut,
    RateLimited {
        retry_after: Duration,
    },
    Refused,
    /// The newly bought token is held by the protocol, but saving failed.
    /// Retry its write, never spend the rotating refresh token again.
    Unsaved {
        valid: Duration,
    },
    Cancelled,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Request {
    Refresh { account: u32, generation: u64 },
    Keep { account: u32, generation: u64 },
    Cancel { account: u32, generation: u64 },
    Granted { grant: Grant },
    Availability { account: u32, usable: bool },
    Refused { account: u32 },
    Closed { account: u32 },
}

/// Account state for views. A spent overlay is carried separately by facts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum State {
    Fresh,
    Refreshing,
    Retrying,
    Revoked,
    Closing,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Fact {
    pub account: u32,
    pub state: State,
    pub generation: u64,
    pub usable: bool,
    pub unsaved: bool,
    pub spent_until: Option<Time>,
    /// Revocation, an unsaved rotation, or a long account cooldown.
    pub attention: bool,
}
