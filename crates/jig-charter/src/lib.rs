//! Translation of a core-owned charter into Smith's run vocabulary
//! (domain/hosts.md, section 8). The application's root supplies rendered
//! sections and connector tools; this crate owns no application policy.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
extern crate alloc;

mod boundary;
mod charter;
mod encode;
#[cfg(test)]
mod tests;

pub use boundary::{
    Budget, ChangeRule, Charter, Contract, Conventions, FieldRule, ItemRule, Items, Model, Prices, Section, TextRule,
    Tool, ToolEffect, VerdictRule, WorkspaceTools,
};
pub use charter::charter;
pub use encode::{EndpointName, encode};
