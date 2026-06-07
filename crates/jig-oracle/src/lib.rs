//! Offline pi-SDK ↔ jig oracle.
//!
//! This crate has no runtime API. It exists to host the offline oracle test
//! (`tests/oracle.rs`), which drives the **pi SDK** (`pi_agent_rust`, used
//! **directly** — no smith) against an in-process [`jig_server::FakeLlm`] with
//! **no network and no real credentials**, and asserts the SDK parses jig's
//! wire output into its canonical event model and completes the agent loop
//! (including a tool-call → tool-result → final turn).
//!
//! It is the "offline oracle" deliverable of issue #17 (P6): a pi-SDK oracle
//! run against a jig proven faithful by the dialect work in P3/P4.
//!
//! The pi-SDK **recording** track (capturing real `subject` fixtures, T3/T4) is
//! additive on top: it needs live credentials and the structural templates from
//! P2 (#14), so it cannot run in the offline `cargo test`. To keep the heavy SDK
//! dependency out of the normal build graph, that track's reusable, unit-tested
//! pieces — the Anthropic subscription workaround, the credential resolver, and
//! the dialect/scenario driving core — live under `tests/support/` as a shared
//! module compiled only into this crate's `pi`-pulling **test/example** targets,
//! never the library. The online capture itself is the `#[ignore]`d
//! `tests/pi_subject_record.rs`; the offline T3/T4 conformance over the committed
//! recordings is `crates/jig-core/tests/pi_sdk_conformance.rs`.
