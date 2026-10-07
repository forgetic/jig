//! A scripted parent and connector for the brief child (domain/testing.md,
//! section 3). The fake connector owns bytes until the parent takes or drops
//! its token; the brief sees only its reported size.

pub mod referee;
mod world;

pub use world::{LIMITS, World};
