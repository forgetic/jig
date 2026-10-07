//! Ops's connector worlds: the shared production below and a scripted root above.
#![forbid(unsafe_code)]

mod infrastructure_world;
mod world;

pub use infrastructure_world::{InfrastructureWorld, infra_environment, infra_service, infrastructure_limits};
pub use world::{World, limits, service};
