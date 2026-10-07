//! Tasks with scripted executors and an atomic durable parent at the retained
//! production boundary (domain/tasks.md, 11; domain/engine.md, 7).
pub mod accounting_referee;
pub mod referee;
mod world;
pub use world::{Frozen, LIMITS, RETRY, Reply, World, authority, run_story, run_story_facts, task};
