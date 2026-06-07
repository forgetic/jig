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

pub mod anthropic_oauth;
pub mod auth;
pub mod subject;
