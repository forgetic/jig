//! Shared support modules for the pi-SDK driver tests (issue #17, P6).
//!
//! Compiled into this crate's `pi`-pulling test/example targets only — never the
//! `jig-oracle` library — so the heavy SDK dependency stays out of the normal
//! build graph (the library remains runtime-free, as PR #25 left it). Both the
//! offline oracle (`oracle.rs`) and the online recording harness
//! (`pi_subject_record.rs`) include this via `mod support;`.
//!
//! - [`anthropic_oauth`] — the duplicated Anthropic subscription workaround
//!   (identity headers, dual-schema bearer resolve/refresh, the mandatory system
//!   identity), with offline unit tests.
//! - [`auth`] — real-credential resolution per dialect from `~/.pi/agent/auth.json`.
//! - [`subject`] — the dialect/scenario driving core: build a pi-SDK `ModelEntry`
//!   pointed at a base URL and produce the per-scenario `Context`/`StreamOptions`.

// These support modules are shared verbatim across multiple test/example targets
// (the `#[ignore]`d recording test and the xtask-callable `pi_subject_record`
// example, issue #19), and each target exercises a *different* subset of the
// helpers — e.g. the example records one cell and never calls `Dialect::all`,
// which the whole-matrix test does. A helper unused in one target is not dead
// code, so silence the per-target `dead_code` lint here rather than scatter
// `#[allow]`s across the shared API.
#![allow(dead_code)]

pub mod anthropic_oauth;
pub mod auth;
pub mod subject;
