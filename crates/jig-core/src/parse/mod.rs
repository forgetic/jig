//! SSE stream parsers: wire `text/event-stream` bytes → canonical [`crate::Reply`].
//!
//! These parsers are the **inverse** of the [`crate::render`] family: a renderer
//! turns a canonical [`Reply`] into ordered [`crate::render::SseFrame`]s, and a
//! parser here recovers a [`Reply`] from the bytes of such a stream — whether
//! those bytes came from `jig`'s own renderer or from a real provider captured
//! by the recorder (issue #18). Recovering the canonical model from an
//! *authoritative* capture is the keystone the structural-template machinery
//! (P2, #14) and the T1/T2 conformance checks (P3, #15) build on: T1 is
//! "render → strip → == template", and a parser is what lets a capture be
//! reduced to the same canonical shape a render produces.
//!
//! Everything here is pure and synchronous — it consumes a `&[u8]` buffer and
//! returns data — so it unit-tests without a runtime, a network leg, or a live
//! credential, and therefore runs under `cargo test` in CI (unlike the recorder,
//! which is driven manually against a real backend).
//!
//! P3 (#15) ships the [`anthropic`] messages parser. The OpenAI and Codex
//! parsers are follow-ups in their own phases.

mod anthropic;
mod sse;

pub use anthropic::{ParseError, parse_anthropic_sse};
pub use sse::{SseEvent, parse_sse};
