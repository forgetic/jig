//! Application conformance from durable records and independent neighbours
//! (`domain/testing.md`, 5–7).
#![forbid(unsafe_code)]
pub mod harness;
pub mod referee;
pub use harness::{Application, Clock, Cut, Harness, Input, Outcome, Output};
