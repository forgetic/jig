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
//! run against a jig proven faithful by the dialect work in P3/P4. The pi-SDK
//! *recording* harness (T3 request validation, T4 cross-driver) is **not** here:
//! recording needs live provider credentials + the structural templates from P2
//! (#14), neither of which is available offline in `cargo test`. See the crate's
//! test module and the PR description for the boundary.
