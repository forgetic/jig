//! The test connector's domain world (domain/connectors.md, section 15).
//!
//! A scripted root commits connector records at the end of each step and only
//! then releases outbox entries or system calls. Its fake system is below the
//! connector. Both the root and the system survive a cold connector restart.
//! A seed changes names, kinds' use and the moments at which faults and
//! restarts are injected; replaying the same seed produces the same trace.
#![forbid(unsafe_code)]

mod world;

pub use world::{LIMITS, World, config, kind, path};
