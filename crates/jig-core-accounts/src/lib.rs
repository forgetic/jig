//! Account refresh and grant availability (domain/engine.md, section 12).
//!
//! The child keeps account numbers, token generations, expiry and retry times,
//! and bounded, best-effort facts. It never sees token values or secret records.
//! The protocol layer saves a rotation before reporting `Refreshed`.
//!
//! `Domain::new` establishes the account table; `step` takes one completion or
//! control event, and `fire` takes one due timer. The parent asks `usable` and
//! `grant` before starting a run. A step has room for [`MAX_OUT`] requests.
//!
//! | State | Event | Next state | Requests |
//! | --- | --- | --- | --- |
//! | absent | add | fresh or refreshing | grant or refresh |
//! | fresh | due or rejected | refreshing | refresh |
//! | refreshing | refreshed | fresh | grant, availability |
//! | refreshing | failed | retrying or revoked | availability |
//! | retrying | due | refreshing | keep or refresh |
//! | any live state | close | closing or absent | cancel or closed |
//! | closing | terminal completion | absent | closed |
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

mod boundary;
mod domain;
mod limits;

pub use boundary::{Event, Fact, Failure, Grant, Request, State};
pub use domain::{Domain, MAX_OUT, fire, step};
pub use limits::{Limits, worst_case};
