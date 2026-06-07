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
//! - [`grammar`] — request **grammar** reduction + cross-driver conformance, the
//!   basis of the pi-SDK request-validation check (T3, P6 #17). Unlike T2 (a
//!   recording against its *own* template), T3 compares a pi-SDK `subject` request
//!   against the official client's `authoritative` template — two *different*
//!   requests whose **wire grammar** (not content/size) must agree.
//!
//! The conformance properties asserted over the committed `fixtures/`:
//!
//! - **T1**: drive jig with `drive-shape.json` → [`render_openai`] → strip →
//!   must equal `response.template.json`.
//! - **T2**: the authoritative `request.json` → strip → must equal
//!   `request.template.json`.
//! - **T3**: the pi-SDK `subject` `request.json`, reduced to its request grammar,
//!   must be **conformant** with the authoritative request grammar (every key /
//!   shape the subject sends appears in the official client's request); a
//!   divergence is a reviewed SDK finding.
//! - **T4**: the pi-SDK `subject` response, reduced like a template, must carry
//!   the same canonical reply/framing grammar as the authoritative response —
//!   cross-driver response consistency.
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
