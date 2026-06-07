//! Structural-template derivation: turn a real recording into the masked
//! skeleton the offline conformance tests assert against.
//!
//! Three artifacts per scenario, mirroring the taxonomy in
//! `docs/explanation/record-and-conform.md`:
//!
//! - [`ResponseTemplate`] (`response.template.json`) — the masked structural
//!   skeleton of the response: the framing invariants (`content-type`, the
//!   `[DONE]` terminator), the canonical [`Reply`] shape with volatile values
//!   ([`Turn::Text`] content, token counts, tool-call ids) masked, and the
//!   response-header policy result. Derived from an authoritative recording's
//!   `response.sse` + `response.headers`.
//! - [`RequestTemplate`] (`request.template.json`) — the masked request: method,
//!   path, the request-header policy result, and the masked request body
//!   (requested `model` kept, tool-call correlation ids masked). Derived from a
//!   recording's `request.json`.
//! - [`DriveShape`] (`drive-shape.json`) — the canonical [`Reply`] (with its real
//!   content intact) that drives jig in the T1 test. It is the *input* jig
//!   renders; masking happens to the rendered output, not to the drive shape.
//!
//! # Why a masked canonical [`Reply`] is the response skeleton
//!
//! The provider streams text in arbitrary chunk fragments and tags every frame
//! with volatile ids and a drifting served-`model`. None of that is part of the
//! contract. [`parse_openai_sse`] already folds the fragmented, id-tagged frames
//! back into the canonical [`Reply`] — coalescing chunk boundaries and dropping
//! per-frame volatility — so the [`Reply`] *is* the chunk-boundary-independent
//! structural skeleton. Masking the [`Reply`]'s remaining volatile values (text
//! content, token counts, tool ids) yields a template that **both** the real
//! capture and jig's own [`render_openai`] reduce to identically. That identity
//! is exactly the T1 property: `render(drive-shape) → parse → mask == template`.
//!
//! Everything here is pure and synchronous, so derivation and the conformance
//! checks run under the default offline `cargo test`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::parse::{parse_anthropic_sse, parse_codex_sse, parse_openai_sse};
use crate::render::{frames_to_body, render_anthropic, render_codex, render_openai};
use crate::request::Dialect;
use crate::{Reply, Turn};

use super::mask::{HeaderClass, MASK, classify_header, mask_body_value, mask_request_body};

/// Why deriving or stripping an SSE response template failed: the captured (or
/// rendered) stream did not parse under its dialect. Dialect-agnostic so the
/// conformance harness and `xtask derive` handle every dialect uniformly; the
/// inner message is the per-dialect parser's own error rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformParseError(pub String);

impl std::fmt::Display for ConformParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConformParseError {}

/// Parse an SSE byte stream into the canonical [`Reply`] using the parser for
/// `dialect`. The single place dialect → parser is chosen for conformance.
fn parse_for(dialect: Dialect, bytes: &[u8]) -> Result<Reply, ConformParseError> {
    match dialect {
        Dialect::OpenAi => parse_openai_sse(bytes).map_err(|e| ConformParseError(e.to_string())),
        Dialect::Anthropic => {
            parse_anthropic_sse(bytes).map_err(|e| ConformParseError(e.to_string()))
        }
        Dialect::Codex => parse_codex_sse(bytes).map_err(|e| ConformParseError(e.to_string())),
    }
}

/// Render a canonical [`Reply`] to its dialect SSE body — the inverse of
/// [`parse_for`], used by T1 to reduce jig's own output the same way the template
/// was derived.
fn render_for(dialect: Dialect, reply: &Reply) -> String {
    match dialect {
        Dialect::OpenAi => frames_to_body(&render_openai(reply)),
        Dialect::Anthropic => frames_to_body(&render_anthropic(reply)),
        Dialect::Codex => frames_to_body(&render_codex(reply)),
    }
}

/// The SSE stream-terminator sentinel each dialect ends on, recorded in the
/// template so it asserts the framing contract, not just the body shape.
///
/// OpenAI/DeepSeek chat-completions ends on the `data: [DONE]` sentinel; the
/// Anthropic messages stream ends on the `message_stop` event; the Codex
/// responses stream has no distinct terminator sentinel (it ends on
/// `response.completed`), so its terminator is that event name.
pub fn terminator_for(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::OpenAi => "[DONE]",
        Dialect::Anthropic => "message_stop",
        Dialect::Codex => "response.completed",
    }
}

/// A `{ name, value }` header in a template, after the masking policy is applied.
/// Only invariant and masked headers reach a template (ignored ones are dropped);
/// a masked header's `value` is the [`MASK`] sentinel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateHeader {
    pub name: String,
    pub value: String,
}

/// Apply the header policy to a captured header list, producing the template's
/// header view: invariant headers keep their value, masked headers keep their
/// name with a [`MASK`]ed value, ignored headers are dropped. Order is preserved.
///
/// Header *names* are lowercased so the template is stable against a backend that
/// changes header casing between captures (`Content-Type` vs `content-type`).
pub fn template_headers(headers: &[(String, String)]) -> Vec<TemplateHeader> {
    headers
        .iter()
        .filter_map(|(name, value)| match classify_header(name) {
            HeaderClass::Invariant => Some(TemplateHeader {
                name: name.to_ascii_lowercase(),
                value: value.clone(),
            }),
            HeaderClass::Masked => Some(TemplateHeader {
                name: name.to_ascii_lowercase(),
                value: MASK.to_string(),
            }),
            HeaderClass::Ignored => None,
        })
        .collect()
}

/// The masked structural skeleton of a response — the `response.template.json`
/// artifact the T1 test compares jig's rendered+stripped output against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseTemplate {
    /// The response-header policy result (invariant + masked headers).
    pub headers: Vec<TemplateHeader>,
    /// The SSE stream-terminator sentinel that must end the stream. Always
    /// `[DONE]` for the chat-completions dialect; recorded explicitly so the
    /// template asserts the framing contract, not just the body shape.
    pub terminator: String,
    /// The canonical [`Reply`] with volatile values masked, serialized as JSON.
    /// This is the chunk-boundary-independent body skeleton (see module docs).
    pub reply: Value,
}

/// The masked request skeleton — the `request.template.json` artifact the T2
/// test compares a recording's `request.json` against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestTemplate {
    pub method: String,
    pub path: String,
    /// The request-header policy result (invariant + masked headers).
    pub headers: Vec<TemplateHeader>,
    /// The masked request body (requested `model` kept; correlation ids masked).
    pub body: Value,
}

/// The turn shape that drives jig in the T1 test — the canonical [`Reply`]
/// recovered from the authoritative capture, content intact.
///
/// It is a thin newtype over [`Reply`] so the on-disk `drive-shape.json` is
/// exactly the serialized canonical reply (the same shape `jig` scripts use),
/// keeping the artifact self-describing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveShape {
    pub reply: Reply,
}

/// Mask a canonical [`Reply`] into its structural skeleton: every [`Turn::Text`]
/// content string and the token counts are volatile (content-irrelevant) and
/// become [`MASK`]; every tool-call `id` is volatile and becomes [`MASK`]; tool
/// names, argument *structure*, turn ordering, and the stop reason are invariant
/// and survive.
///
/// Returned as a `serde_json::Value` so it embeds directly in a
/// [`ResponseTemplate`] and diffs as plain JSON. Tool-call arguments are masked
/// with [`mask_body_value`] so a volatile id *inside* the arguments (rare, but
/// possible) is caught while the argument keys/shape remain asserted.
pub fn mask_reply(reply: &Reply) -> Value {
    let turns: Vec<Value> = reply
        .turns
        .iter()
        .map(|turn| match turn {
            Turn::Text(_) => serde_json::json!({ "Text": MASK }),
            Turn::Thinking(_) => serde_json::json!({ "Thinking": MASK }),
            Turn::ToolCall { name, args, .. } => serde_json::json!({
                "ToolCall": {
                    "id": MASK,
                    "name": name,
                    "args": mask_body_value(args),
                }
            }),
        })
        .collect();

    serde_json::json!({
        "turns": turns,
        "usage": { "prompt_tokens": MASK, "completion_tokens": MASK },
        "stop": stop_slug(reply),
    })
}

/// The stop reason as a stable slug for the template (mirrors the serde form of
/// [`crate::StopReason`]).
fn stop_slug(reply: &Reply) -> &'static str {
    match reply.stop {
        crate::StopReason::Stop => "Stop",
        crate::StopReason::ToolCalls => "ToolCalls",
        crate::StopReason::Error => "Error",
    }
}

/// Derive the [`DriveShape`] from an authoritative `response.sse` capture: parse
/// the real stream into the canonical [`Reply`] under `dialect`. This is what jig
/// is driven with in T1; masking is applied to jig's *output*, never to the drive
/// shape.
pub fn derive_drive_shape(
    dialect: Dialect,
    response_sse: &[u8],
) -> Result<DriveShape, ConformParseError> {
    Ok(DriveShape {
        reply: parse_for(dialect, response_sse)?,
    })
}

/// Derive the [`ResponseTemplate`] from an authoritative capture: parse the real
/// `response.sse` into the canonical [`Reply`] under `dialect`, mask it, record
/// the dialect's terminator, and run the header policy over `response.headers`.
pub fn derive_response_template(
    dialect: Dialect,
    response_sse: &[u8],
    response_headers: &[(String, String)],
) -> Result<ResponseTemplate, ConformParseError> {
    let reply = parse_for(dialect, response_sse)?;
    Ok(ResponseTemplate {
        headers: template_headers(response_headers),
        terminator: terminator_for(dialect).to_string(),
        reply: mask_reply(&reply),
    })
}

/// Strip jig's *own* rendered response down to the same skeleton the template is
/// in: render the [`DriveShape`]'s [`Reply`] with [`render_openai`], parse it
/// back, and mask. The framing invariants jig guarantees by construction
/// (`content-type: text/event-stream`, the `[DONE]` terminator) are filled in to
/// match the template's shape, so a [`ResponseTemplate`] built from this can be
/// compared field-for-field against the derived one — that comparison is T1.
///
/// `expected_headers` is the template's header view, reused verbatim: jig's
/// in-process server is not what produced the recorded headers, so T1 asserts
/// the body/framing skeleton, and the header contract is asserted separately by
/// derivation being deterministic (re-derivation equals the committed template).
pub fn strip_rendered_response(
    dialect: Dialect,
    drive: &DriveShape,
    expected_headers: &[TemplateHeader],
) -> Result<ResponseTemplate, ConformParseError> {
    let body = render_for(dialect, &drive.reply);
    let reply = parse_for(dialect, body.as_bytes())?;
    Ok(ResponseTemplate {
        headers: expected_headers.to_vec(),
        terminator: terminator_for(dialect).to_string(),
        reply: mask_reply(&reply),
    })
}

/// Derive the [`RequestTemplate`] from a recording's `request.json` pieces: keep
/// method and path, run the header policy, and mask the JSON body.
pub fn derive_request_template(
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &Value,
) -> RequestTemplate {
    RequestTemplate {
        method: method.to_string(),
        path: path.to_string(),
        headers: template_headers(headers),
        body: mask_request_body(body),
    }
}

/// Strip a captured `request.json` body to the request skeleton, for the T2
/// comparison: the same masking the template used, so an authoritative request
/// reduces to its committed [`RequestTemplate`] exactly.
pub fn strip_request(
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &Value,
) -> RequestTemplate {
    derive_request_template(method, path, headers, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StopReason, Usage};

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn template_headers_keep_invariant_mask_volatile_drop_rest() {
        let captured = headers(&[
            ("Content-Type", "text/event-stream; charset=utf-8"),
            ("Date", "Sat, 06 Jun 2026 22:00:18 GMT"),
            ("Transfer-Encoding", "chunked"),
            ("x-ds-trace-id", "deadbeef"),
        ]);
        let tmpl = template_headers(&captured);
        assert_eq!(
            tmpl,
            vec![
                TemplateHeader {
                    name: "content-type".to_string(),
                    value: "text/event-stream; charset=utf-8".to_string(),
                },
                TemplateHeader {
                    name: "date".to_string(),
                    value: MASK.to_string(),
                },
                TemplateHeader {
                    name: "x-ds-trace-id".to_string(),
                    value: MASK.to_string(),
                },
            ]
        );
    }

    #[test]
    fn mask_reply_masks_text_and_usage_keeps_tool_name_and_stop() {
        let reply = Reply {
            turns: vec![
                Turn::Text("some real content".to_string()),
                Turn::ToolCall {
                    id: "call_volatile".to_string(),
                    name: "get_weather".to_string(),
                    args: serde_json::json!({ "city": "Paris" }),
                },
            ],
            usage: Usage {
                prompt_tokens: 291,
                completion_tokens: 37,
            },
            stop: StopReason::ToolCalls,
        };
        let masked = mask_reply(&reply);
        assert_eq!(masked["turns"][0]["Text"], MASK);
        assert_eq!(masked["turns"][1]["ToolCall"]["id"], MASK);
        // Tool name + argument structure are invariant.
        assert_eq!(masked["turns"][1]["ToolCall"]["name"], "get_weather");
        assert_eq!(masked["turns"][1]["ToolCall"]["args"]["city"], "Paris");
        // Token counts masked; stop reason kept.
        assert_eq!(masked["usage"]["prompt_tokens"], MASK);
        assert_eq!(masked["stop"], "ToolCalls");
    }

    #[test]
    fn t1_property_render_strip_equals_derived_template() {
        // Build a reply, render it the way jig would, derive the template from
        // that rendered stream, then strip a fresh render of the same drive
        // shape: the two must be identical (the T1 invariant, in miniature).
        let reply = Reply {
            turns: vec![Turn::Text("hello".to_string())],
            usage: Usage {
                prompt_tokens: 9,
                completion_tokens: 1,
            },
            stop: StopReason::Stop,
        };
        let rendered = frames_to_body(&render_openai(&reply));
        let resp_headers = headers(&[("Content-Type", "text/event-stream")]);

        let template =
            derive_response_template(Dialect::OpenAi, rendered.as_bytes(), &resp_headers).unwrap();
        let drive = derive_drive_shape(Dialect::OpenAi, rendered.as_bytes()).unwrap();
        let stripped = strip_rendered_response(Dialect::OpenAi, &drive, &template.headers).unwrap();

        assert_eq!(stripped, template);
    }

    #[test]
    fn t1_property_holds_for_the_anthropic_dialect() {
        // The same T1 invariant, driven through the Anthropic parser/renderer and
        // its `message_stop` terminator: a thinking turn carries no canonical
        // surface, so it round-trips to the same masked skeleton.
        let reply = Reply {
            turns: vec![
                Turn::Thinking("scratch".to_string()),
                Turn::Text("the answer".to_string()),
                Turn::ToolCall {
                    id: "toolu_1".to_string(),
                    name: "write".to_string(),
                    args: serde_json::json!({ "path": "out.txt" }),
                },
            ],
            usage: Usage {
                prompt_tokens: 11,
                completion_tokens: 3,
            },
            stop: StopReason::ToolCalls,
        };
        let rendered = render_for(Dialect::Anthropic, &reply);
        let resp_headers = headers(&[("Content-Type", "text/event-stream")]);

        let template =
            derive_response_template(Dialect::Anthropic, rendered.as_bytes(), &resp_headers)
                .unwrap();
        assert_eq!(template.terminator, "message_stop");
        let drive = derive_drive_shape(Dialect::Anthropic, rendered.as_bytes()).unwrap();
        let stripped =
            strip_rendered_response(Dialect::Anthropic, &drive, &template.headers).unwrap();

        assert_eq!(stripped, template);
    }

    #[test]
    fn t2_property_strip_request_equals_derived_template() {
        let body = serde_json::json!({
            "model": "deepseek-chat",
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let req_headers = headers(&[
            ("Content-Type", "application/json"),
            ("Authorization", "REDACTED"),
        ]);
        let template = derive_request_template("POST", "/chat/completions", &req_headers, &body);
        let stripped = strip_request("POST", "/chat/completions", &req_headers, &body);
        assert_eq!(stripped, template);
        // Requested model survives; Authorization is ignored (not in the policy).
        assert_eq!(template.body["model"], "deepseek-chat");
        assert!(template.headers.iter().all(|h| h.name != "authorization"));
    }

    #[test]
    fn derivation_is_deterministic() {
        let reply = Reply {
            turns: vec![Turn::Text("x".to_string())],
            usage: Usage::default(),
            stop: StopReason::Stop,
        };
        let rendered = frames_to_body(&render_openai(&reply));
        let h = headers(&[("Content-Type", "text/event-stream")]);
        let a = derive_response_template(Dialect::OpenAi, rendered.as_bytes(), &h).unwrap();
        let b = derive_response_template(Dialect::OpenAi, rendered.as_bytes(), &h).unwrap();
        assert_eq!(a, b);
    }
}
