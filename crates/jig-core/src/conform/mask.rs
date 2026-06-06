//! The volatile-masking policy: what conformance treats as *invariant* (must
//! match) versus *volatile* (masked before comparing).
//!
//! This is the committed, reviewable heart of the "format-faithful,
//! content-irrelevant" principle (see `docs/explanation/record-and-conform.md`).
//! The *content* of a recorded conversation is non-deterministic and must not be
//! asserted on; what must match exactly is the conversation **format**. So
//! before a recording is compared against jig's output, every volatile value is
//! rewritten to the stable [`MASK`] sentinel, leaving only the structural
//! skeleton — JSON keys, types, nesting, and the non-volatile values (roles,
//! tool names, stop-reason mappings).
//!
//! The policy is expressed as **data**: [`VOLATILE_BODY_KEYS`] lists the JSON
//! object keys whose value is volatile anywhere they appear, and the header
//! policy ([`HeaderClass`]) splits headers into invariant-asserted, masked, and
//! ignored. Keeping it as named lists (rather than ad-hoc masking scattered
//! through the template code) is what makes the policy auditable and what lets
//! P3/P6 extend it for the version-volatile client-identity values the Anthropic
//! and Codex dialects carry (see issue #13).
//!
//! Everything here is pure and synchronous — transforms over `serde_json::Value`
//! and `(name, value)` header pairs — so it unit-tests offline with no runtime.

use serde_json::Value;

/// The stable placeholder a masked (volatile) value is rewritten to. Stable
/// across runs, dialects, and value types so templates diff cleanly and a masked
/// field is unmistakable in a failure diff.
pub const MASK: &str = "<MASKED>";

/// JSON object keys whose **value** is volatile wherever the key appears in a
/// response/request body, and so is rewritten to [`MASK`] by
/// [`mask_body_value`].
///
/// These are the ids, timestamps, token counts, fingerprints, and nonces called
/// out in the design (issue #13 "Volatile (masked before comparing)"):
///
/// - `id`, `chatcmpl-…`/`call_…` ids, request/response correlation ids.
/// - `created` (a unix timestamp) and any `*_at` timestamp.
/// - `model` — the *served* model id drifts (`deepseek-chat` →
///   `deepseek-v4-flash`), so it is volatile even though the *requested* model
///   in a request body is asserted (it is not in this list; request masking
///   uses [`mask_request_body`], which keeps `model`).
/// - `system_fingerprint`, `fingerprint`.
/// - every token-count field (`*_tokens`) and the nested usage detail objects.
///
/// Matched exactly (case-sensitive) — wire JSON keys are stable lowercase.
pub const VOLATILE_BODY_KEYS: &[&str] = &[
    "id",
    "created",
    "system_fingerprint",
    "fingerprint",
    "prompt_tokens",
    "completion_tokens",
    "total_tokens",
    "cached_tokens",
    "prompt_cache_hit_tokens",
    "prompt_cache_miss_tokens",
    "prompt_tokens_details",
    "completion_tokens_details",
];

/// Recursively rewrite every [`VOLATILE_BODY_KEYS`] value in `value` to [`MASK`],
/// preserving structure (keys, nesting, array order) everywhere else.
///
/// A masked key's value becomes the [`MASK`] string regardless of its original
/// type, so `created: 1780783218` and `id: "chatcmpl-…"` both collapse to the
/// same sentinel — the template asserts the key is *present and structural*, not
/// its volatile value. Nested objects under a volatile key (e.g.
/// `prompt_tokens_details`) are masked wholesale rather than recursed into.
pub fn mask_body_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    if VOLATILE_BODY_KEYS.contains(&k.as_str()) {
                        (k.clone(), Value::String(MASK.to_string()))
                    } else {
                        (k.clone(), mask_body_value(v))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(mask_body_value).collect()),
        other => other.clone(),
    }
}

/// Mask a **request** body. Same volatile-key policy as [`mask_body_value`], but
/// the requested `model` is an *invariant* the SDK must send correctly, so it is
/// preserved (it is not in [`VOLATILE_BODY_KEYS`]). Tool-call ids the client
/// echoes back in a follow-up request (`tool_call_id`, and the `id` on prior
/// `assistant.tool_calls`) are volatile and masked.
pub fn mask_request_body(value: &Value) -> Value {
    mask_request_inner(value)
}

/// Request-side keys that are volatile in addition to [`VOLATILE_BODY_KEYS`]:
/// the tool-call correlation ids a multi-turn request carries.
const VOLATILE_REQUEST_KEYS: &[&str] = &["tool_call_id"];

fn mask_request_inner(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    // `id` is volatile in a request only inside a tool_calls entry
                    // (the model-assigned call id); the top-level request has no
                    // bare `id`. Treat both `id` and `tool_call_id` as volatile.
                    if VOLATILE_BODY_KEYS.contains(&k.as_str())
                        || VOLATILE_REQUEST_KEYS.contains(&k.as_str())
                    {
                        (k.clone(), Value::String(MASK.to_string()))
                    } else {
                        (k.clone(), mask_request_inner(v))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(mask_request_inner).collect()),
        other => other.clone(),
    }
}

/// How the conformance policy treats a response/request header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderClass {
    /// An invariant header: its presence (and, for `content-type`, its value
    /// format) is part of the contract and is asserted. The value is kept.
    Invariant,
    /// A volatile header that is present but whose value drifts every request
    /// (`date`, request ids, CDN/edge headers, cookies). Kept in the template
    /// with its value [`MASK`]ed so the *shape* (which headers appear) is still
    /// asserted without pinning the volatile value.
    Masked,
    /// A header that is neither part of the contract nor worth tracking — it is
    /// dropped from the template entirely so an incidental header a backend adds
    /// or removes does not churn the template.
    Ignored,
}

/// Header names (lowercased) whose presence and value are part of the wire
/// contract. `content-type` carries the `text/event-stream` framing signal that
/// is the whole point of the response, so it is the canonical invariant header.
const INVARIANT_HEADERS: &[&str] = &["content-type"];

/// Header names (lowercased) that are always present but always volatile, so the
/// template keeps the key with a masked value. This is the allowlist from the
/// design: `date`, request-ids, the `cf-*` edge family, `set-cookie`, `server`,
/// and the equivalent CloudFront/DeepSeek edge headers seen in real captures.
const MASKED_HEADERS: &[&str] = &[
    "date",
    "server",
    "set-cookie",
    "x-request-id",
    "x-amzn-requestid",
    "x-ds-trace-id",
    "x-amz-cf-id",
    "x-amz-cf-pop",
    "x-cache",
    "via",
];

/// Header-name prefixes (lowercased) marking a family of volatile edge/request
/// headers, any value of which is masked. Covers Cloudflare's `cf-*` and the
/// `x-amz-cf-*` CloudFront family without listing each member.
const MASKED_HEADER_PREFIXES: &[&str] = &["cf-", "x-amz-cf-"];

/// Classify a header by name (case-insensitive) under the policy.
///
/// Order: invariant first, then the masked allowlist (exact then prefix);
/// anything else is ignored. This keeps the template stable against incidental
/// headers (`transfer-encoding`, `connection`, `vary`, security headers) that
/// are neither a contract signal nor a volatile-but-tracked value.
pub fn classify_header(name: &str) -> HeaderClass {
    let lower = name.to_ascii_lowercase();
    if INVARIANT_HEADERS.contains(&lower.as_str()) {
        return HeaderClass::Invariant;
    }
    if MASKED_HEADERS.contains(&lower.as_str())
        || MASKED_HEADER_PREFIXES.iter().any(|p| lower.starts_with(p))
    {
        return HeaderClass::Masked;
    }
    HeaderClass::Ignored
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn masks_ids_timestamps_and_token_counts_anywhere() {
        let chunk = json!({
            "id": "chatcmpl-abc",
            "created": 1780783218u64,
            "object": "chat.completion.chunk",
            "choices": [{ "index": 0, "delta": { "content": "hi" } }],
            "usage": {
                "prompt_tokens": 9,
                "completion_tokens": 1,
                "total_tokens": 10,
                "prompt_tokens_details": { "cached_tokens": 0 }
            }
        });
        let masked = mask_body_value(&chunk);
        assert_eq!(masked["id"], MASK);
        assert_eq!(masked["created"], MASK);
        // Structural, non-volatile values survive.
        assert_eq!(masked["object"], "chat.completion.chunk");
        assert_eq!(masked["choices"][0]["delta"]["content"], "hi");
        // Token counts and the nested detail object are all masked.
        assert_eq!(masked["usage"]["prompt_tokens"], MASK);
        assert_eq!(masked["usage"]["completion_tokens"], MASK);
        assert_eq!(masked["usage"]["total_tokens"], MASK);
        assert_eq!(masked["usage"]["prompt_tokens_details"], MASK);
    }

    #[test]
    fn masking_preserves_structure_and_is_idempotent() {
        let chunk = json!({ "id": "x", "usage": { "prompt_tokens": 1 } });
        let once = mask_body_value(&chunk);
        let twice = mask_body_value(&once);
        assert_eq!(once, twice, "masking is idempotent");
    }

    #[test]
    fn request_masking_keeps_model_but_masks_tool_call_ids() {
        let req = json!({
            "model": "deepseek-chat",
            "messages": [
                { "role": "assistant", "tool_calls": [
                    { "id": "call_abc", "type": "function",
                      "function": { "name": "get_weather", "arguments": "{}" } }
                ]},
                { "role": "tool", "tool_call_id": "call_abc", "content": "..." }
            ]
        });
        let masked = mask_request_body(&req);
        // The requested model is an invariant — kept.
        assert_eq!(masked["model"], "deepseek-chat");
        // Tool-call correlation ids are volatile — masked.
        assert_eq!(masked["messages"][0]["tool_calls"][0]["id"], MASK);
        assert_eq!(masked["messages"][1]["tool_call_id"], MASK);
        // Tool name and role are structural — kept.
        assert_eq!(
            masked["messages"][0]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(masked["messages"][1]["role"], "tool");
    }

    #[test]
    fn header_classification_follows_the_allowlist() {
        assert_eq!(classify_header("Content-Type"), HeaderClass::Invariant);
        assert_eq!(classify_header("content-type"), HeaderClass::Invariant);

        for masked in [
            "Date",
            "Server",
            "Set-Cookie",
            "X-Request-Id",
            "x-ds-trace-id",
            "X-Amz-Cf-Id",
            "CF-Ray",
            "cf-cache-status",
        ] {
            assert_eq!(
                classify_header(masked),
                HeaderClass::Masked,
                "{masked} should be masked"
            );
        }

        for ignored in [
            "Transfer-Encoding",
            "Connection",
            "Vary",
            "X-Content-Type-Options",
        ] {
            assert_eq!(
                classify_header(ignored),
                HeaderClass::Ignored,
                "{ignored} should be ignored"
            );
        }
    }
}
