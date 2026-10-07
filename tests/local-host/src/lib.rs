//! A scripted core and a typed fake provider around the local host
//! (domain/hosts.md, sections 5 and 11). The core commits each emitted turn
//! before acknowledging it. The fake provider owns every pending completion.
//! Boundary observations drive the referee; host internals are not inspected.

pub mod referee;
mod world;

pub use world::{Observation, Script, World};
