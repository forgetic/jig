//! The inline agent's two routes: the host-facing Smith vocabulary and
//! completion requests to its root (domain/hosts.md, section 5.2).

use alloc::boxed::Box;
use skein_lib::Token;
use smith_domain as smith;
use smith_host_domain as host;

/// A provider completion or cancellation returned below Smith's domain.
#[derive(PartialEq, Eq, Debug)]
pub enum Completion {
    /// The LLM supplied a completion.
    Completed { owner: Token, completion: smith::llm::Completion },
    /// The LLM failed the request with transport evidence.
    Failed { owner: Token, failure: smith::llm::Failure, evidence: smith::llm::Evidence, detail: Box<[u8]> },
    /// The lower layer confirmed cancellation.
    Cancelled { owner: Token },
}

/// Upward requests, in the order Smith's domain emitted them.
#[derive(PartialEq, Eq, Debug)]
#[expect(clippy::large_enum_variant, reason = "the host boundary's sealed terminal is carried directly to its root")]
pub enum Request {
    /// What Smith's process host would tell its parent.
    Host(host::Request),
    /// LLM work routed directly through the composing root.
    Lower { client: Token, request: smith::Request },
}
