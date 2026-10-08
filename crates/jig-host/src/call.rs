//! Host calls in flight (hosts.md, section 6): a delivery served by the
//! workspace, or a call relayed to the engine. A delivery stays in flight
//! after withdrawal or stop until the workspace reports its terminal. A
//! relay settles after its local cancellation terminal arrives.

use alloc::boxed::Box;

use skein_lib::{Id, Token};

use crate::hosted::Hosted;

#[derive(Debug)]
pub(crate) struct Call {
    /// The run that made it.
    pub(crate) hosted: Id<Hosted>,
    pub(crate) state: State,
    /// The agent's opaque name, moved out when its one reply is sent.
    pub(crate) typed: Option<Box<[u8]>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum State {
    /// A delivery in flight, for the call `call` of the agent `agent`.
    Delivering { agent: Token },
    /// Relayed to the engine, for the call `call` of the agent `agent`.
    Relayed { agent: Token },
    /// The agent has its answer; local delivery is being cancelled.
    Settling,
    /// Terminal: holds nothing.
    Closed,
}
