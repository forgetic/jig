//! The people child domain, with a scripted root, authority and tasks.
//! The parent closes each decision, commits its records and task together,
//! withholds replies until durability, and cold-restores from durable rows.
//! Referee observations come from client replies and durable task creations.
pub mod referee;
mod world;
pub use world::{ENDINGS, LIMITS, Settings, Stats, World, identity};
