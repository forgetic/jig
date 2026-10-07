//! The notes child's domain world. A scripted parent commits every decision's
//! writes together, serves bounded store pages, and can rebuild the child from
//! those records. The referee observes calls and durable records.

mod referee;
mod world;

pub use referee::{Intent, Referee, Write, saved_entry};
pub use world::{Answer, LIMITS, World, correction, key_of, one_scope, search, task_entry};
