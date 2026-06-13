//! Offline structural conformance: derive masked templates from real recordings
//! and assert jig reproduces them exactly at the *format* level.
//!
//! This module is the dialect-agnostic conformance layer P2 (#14) adds on top of
//! the parsers and renderers. It is **async-free** and reads no clock or network,
//! so it runs under the default offline `cargo test` — the conformance half of
//! the record/conform split (see `docs/explanation/record-and-conform.md`).
//!
//! The pieces:
//!
//! - [`mask`] — the committed, reviewable **volatile-masking policy**: which body
//!   keys and which headers are volatile, and how each is rewritten.
//! - [`template`] — **template derivation**: turn a real recording into the
//!   masked [`ResponseTemplate`] / [`RequestTemplate`] skeletons and the
//!   [`DriveShape`] that drives jig, plus the strip functions the T1/T2 checks
//!   compare against.
//! - [`diff`] — a readable structural diff so a T1/T2 failure shows *where* the
//!   shapes diverged, not just that they did.
//! - [`grammar`] — **request-grammar reduction** for T3-style cross-client
//!   checks: reduce a request body to its wire-grammar skeleton and assert a
//!   subject SDK's grammar is conformant with the authoritative client's
//!   template. Consumed by subject SDKs from their *own* repositories (proving
//!   an SDK against jig is that SDK's job) — tongs is the worked example.
//!
//! The conformance properties asserted over the committed `fixtures/`:
//!
//! - **T1**: drive jig with `drive-shape.json` → [`render_openai`] → strip →
//!   must equal `response.template.json`.
//! - **T2**: the authoritative `request.json` → strip → must equal
//!   `request.template.json`.
//! - **T3** (in the subject SDK's repo): the subject's recorded `request.json`
//!   → [`request_grammar`] → must be conformant with the authoritative
//!   `request.template.json` body's grammar ([`grammar_findings`] empty).
//!
//! [`render_openai`]: crate::render::render_openai

pub mod diff;
pub mod grammar;
pub mod mask;
pub mod template;

pub use diff::structural_diff;
pub use grammar::{GrammarFinding, grammar_findings, request_grammar};
pub use mask::{HeaderClass, MASK, classify_header, mask_body_value, mask_request_body};
pub use template::{
    ConformParseError, DriveShape, RequestTemplate, ResponseTemplate, TemplateHeader,
    derive_drive_shape, derive_request_template, derive_response_template, mask_reply,
    strip_rendered_response, strip_request, template_headers, terminator_for,
};
