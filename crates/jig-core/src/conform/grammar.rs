//! Request **grammar** reduction for cross-driver request validation (T3, issue
//! #17).
//!
//! # Why a grammar, not equality
//!
//! T1/T2 compare jig (or an authoritative recording) against its *own* derived
//! template, so byte-after-masking equality works. T3 is different: it compares a
//! **subject SDK's** request against the **official client's** `authoritative`
//! request template. Those two requests are *not* the same request — the official
//! client (e.g. Claude Code) sends its full system prompt, its whole tool
//! catalogue, and client-specific fields, while the subject SDK sends one tiny
//! tool and a one-line prompt. Their *content and size* differ by design; what
//! must agree is the **wire grammar**: which JSON keys exist, their value
//! **types**, how messages/roles/tool-calls/tool-results are spelled, and how
//! arrays of those are framed.
//!
//! So T3 reduces both requests to a **grammar skeleton** — every leaf value
//! replaced by a type sentinel, every array collapsed to the set of distinct
//! element grammars — and asserts the subject's grammar is **conformant** with
//! the authoritative grammar: every key the subject sends appears in the
//! authoritative grammar with a compatible shape. A key or shape the subject uses
//! that the official client never does is a **finding** (a candidate SDK bug or a
//! drift from the official contract), surfaced by [`grammar_findings`].
//!
//! Conformance is one-directional on purpose: the authoritative request is richer
//! (more tools, more fields), so the subject being a *subset* of it is the
//! contract. The subject is not required to exercise every authoritative field —
//! only to not invent wire structure the official client does not use.
//!
//! Everything here is a pure transform over `serde_json::Value`, so it runs in the
//! offline `cargo test` alongside the other conformance checks.

use std::collections::BTreeSet;

use serde_json::Value;

/// The type sentinels a leaf value reduces to in a grammar skeleton. Stable
/// strings so a grammar diffs as plain JSON and reads clearly in a finding.
const STRING: &str = "<string>";
const NUMBER: &str = "<number>";
const BOOL: &str = "<bool>";
const NULL: &str = "<null>";
/// The wildcard key a **content-map** collapses its arbitrary keys to.
const ANY_KEY: &str = "<*>";

/// Object keys whose **value is a content-map**: an object whose own keys are
/// caller/domain data, not wire grammar. The canonical case is JSON-Schema
/// `properties` — under it the keys are a tool's *argument names* (`city`,
/// `cmd`, …), which differ entirely between two clients' tools and are content,
/// not structure. The value of such a key is reduced as a map (every child key →
/// the wildcard [`ANY_KEY`], child grammars unioned) so two tool schemas with
/// different argument names but the same shape reduce identically.
const CONTENT_MAP_KEYS: &[&str] = &["properties"];

/// Reduce a request body [`Value`] to its **grammar skeleton**: a canonical shape
/// where content is abstracted to types but structure (keys, nesting, array
/// element grammars) is preserved.
///
/// - An object keeps its keys; each value is reduced recursively. The exception
///   is a [`CONTENT_MAP_KEYS`] value (e.g. JSON-Schema `properties`), whose
///   arbitrary keys are content and collapse to the [`ANY_KEY`] wildcard.
/// - An array is reduced to the **sorted set of distinct element grammars** it
///   contains — so `[{role:user},{role:assistant}]` reduces to the two distinct
///   message grammars regardless of how many of each appear or their order
///   (order and arity are content, not grammar). An empty array stays `[]`.
/// - A leaf becomes its type sentinel.
///
/// Reducing arrays to a *set* is what makes two differently-sized requests
/// comparable: a 1-tool subject request and a 40-tool authoritative request both
/// reduce to "an array of <the tool grammar>", so the tool *encoding* is what gets
/// compared, not the tool *count*.
pub fn request_grammar(value: &Value) -> Value {
    request_grammar_inner(value, None)
}

/// `parent_key` is the object key whose value `value` is, so a content-map key
/// can switch its child object into map-collapse mode.
fn request_grammar_inner(value: &Value, parent_key: Option<&str>) -> Value {
    match value {
        // A content-map (`properties`): collapse arbitrary keys to one wildcard,
        // unioning the distinct child-value grammars so {city:string} and
        // {cmd:string, login:bool} reduce to the same map grammar.
        Value::Object(map) if parent_key.is_some_and(|k| CONTENT_MAP_KEYS.contains(&k)) => {
            let mut seen = BTreeSet::new();
            let mut distinct = Vec::new();
            for v in map.values() {
                let g = request_grammar_inner(v, None);
                let key = serde_json::to_string(&g).unwrap_or_default();
                if seen.insert(key) {
                    distinct.push(g);
                }
            }
            distinct.sort_by_key(|g| serde_json::to_string(g).unwrap_or_default());
            // One wildcard key mapping to the (single, when homogeneous) child
            // grammar — or an array of distinct child grammars when heterogeneous.
            let collapsed = match distinct.len() {
                0 => Value::Object(serde_json::Map::new()),
                1 => distinct.pop().unwrap(),
                _ => Value::Array(distinct),
            };
            let mut obj = serde_json::Map::new();
            obj.insert(ANY_KEY.to_string(), collapsed);
            Value::Object(obj)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), request_grammar_inner(v, Some(k))))
                .collect(),
        ),
        Value::Array(items) => {
            // Distinct element grammars, sorted for determinism. Serializing each
            // to a canonical string keys the set (serde_json::Value is not Ord).
            let mut seen = BTreeSet::new();
            let mut distinct = Vec::new();
            for item in items {
                let g = request_grammar_inner(item, None);
                let key = serde_json::to_string(&g).unwrap_or_default();
                if seen.insert(key) {
                    distinct.push(g);
                }
            }
            // Sort the distinct grammars by their serialized form so the array is
            // order-independent (two requests with the same element grammars in a
            // different order reduce identically).
            distinct.sort_by_key(|g| serde_json::to_string(g).unwrap_or_default());
            Value::Array(distinct)
        }
        Value::String(_) => Value::String(STRING.to_string()),
        Value::Number(_) => Value::String(NUMBER.to_string()),
        Value::Bool(_) => Value::String(BOOL.to_string()),
        Value::Null => Value::String(NULL.to_string()),
    }
}

/// One divergence found when checking a subject grammar against an authoritative
/// one, addressed by a JSON-path-ish locator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarFinding {
    /// The path into the request where the subject diverges.
    pub path: String,
    /// What diverged, in human terms.
    pub detail: String,
}

impl std::fmt::Display for GrammarFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.detail)
    }
}

/// Check that `subject` request grammar is **conformant** with `authoritative`:
/// every key/shape the subject sends must appear in the authoritative grammar.
/// Returns the list of divergences (empty = conformant).
///
/// Both arguments are raw request bodies; they are reduced with [`request_grammar`]
/// internally so callers pass the unreduced `request.json` bodies.
pub fn grammar_findings(subject: &Value, authoritative: &Value) -> Vec<GrammarFinding> {
    let subj = request_grammar(subject);
    let auth = request_grammar(authoritative);
    let mut out = Vec::new();
    conform("", &subj, &auth, &mut out);
    out
}

/// Walk the subject grammar against the authoritative grammar. A subject node
/// must be *representable* in the authoritative grammar:
///
/// - object: every subject key must exist in the authoritative object, and its
///   value grammar must conform recursively;
/// - array: every distinct subject element grammar must conform to *some*
///   authoritative element grammar (the official client's array carries at least
///   that element shape);
/// - leaf (type sentinel): must equal the authoritative leaf type at that path.
fn conform(path: &str, subject: &Value, authoritative: &Value, out: &mut Vec<GrammarFinding>) {
    match (subject, authoritative) {
        (Value::Object(subj), Value::Object(auth)) => {
            for (key, subj_val) in subj {
                match auth.get(key) {
                    Some(auth_val) => conform(&child(path, key), subj_val, auth_val, out),
                    None => out.push(GrammarFinding {
                        path: child(path, key),
                        detail: "subject sends a key the authoritative client does not".to_string(),
                    }),
                }
            }
        }
        (Value::Array(subj), Value::Array(auth)) => {
            // Every distinct subject element grammar must match some authoritative
            // element grammar. An empty authoritative array means the official
            // client never populates it, so any subject element is a divergence.
            for (i, subj_el) in subj.iter().enumerate() {
                let element_path = index(path, i);
                // Probe each authoritative element; an exact match clears this
                // subject element. Otherwise report the **closest** element's
                // internal diff (the fewest nested findings) so the path points at
                // the precise divergence (e.g. `messages[1].content`) rather than
                // the whole element.
                let mut best: Option<Vec<GrammarFinding>> = None;
                let mut matched = false;
                for auth_el in auth {
                    let mut probe = Vec::new();
                    conform(&element_path, subj_el, auth_el, &mut probe);
                    if probe.is_empty() {
                        matched = true;
                        break;
                    }
                    if best.as_ref().is_none_or(|b| probe.len() < b.len()) {
                        best = Some(probe);
                    }
                }
                if matched {
                    continue;
                }
                match best {
                    // The closest authoritative element diverges in a specific,
                    // addressable way — surface that nested diff.
                    Some(nested) if !nested.is_empty() => out.extend(nested),
                    // No authoritative element at all (empty array): the subject
                    // populates an array the official client never does.
                    _ => out.push(GrammarFinding {
                        path: element_path,
                        detail: "subject array element grammar not present in the authoritative client's array".to_string(),
                    }),
                }
            }
        }
        (subj_leaf, auth_leaf) if subj_leaf != auth_leaf => {
            out.push(GrammarFinding {
                path: path_or_root(path),
                detail: format!(
                    "type/shape differs (subject {}, authoritative {})",
                    render(subj_leaf),
                    render(auth_leaf)
                ),
            });
        }
        _ => {}
    }
}

fn child(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn index(path: &str, i: usize) -> String {
    format!("{}[{i}]", path_or_root(path))
}

fn path_or_root(path: &str) -> String {
    if path.is_empty() {
        "(root)".to_string()
    } else {
        path.to_string()
    }
}

/// Compact rendering of a grammar node for a finding message.
fn render(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(_) => "<object>".to_string(),
        Value::Array(_) => "<array>".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn leaves_reduce_to_type_sentinels() {
        assert_eq!(request_grammar(&json!("hi")), json!("<string>"));
        assert_eq!(request_grammar(&json!(42)), json!("<number>"));
        assert_eq!(request_grammar(&json!(true)), json!("<bool>"));
        assert_eq!(request_grammar(&json!(null)), json!("<null>"));
    }

    #[test]
    fn object_keeps_keys_and_reduces_values() {
        let g = request_grammar(&json!({ "model": "deepseek-chat", "stream": true }));
        assert_eq!(g, json!({ "model": "<string>", "stream": "<bool>" }));
    }

    #[test]
    fn array_reduces_to_distinct_element_grammars_order_independent() {
        // Two messages with the same grammar collapse to one; order does not matter.
        let a = request_grammar(&json!([
            { "role": "user", "content": "a" },
            { "role": "assistant", "content": "b" },
        ]));
        let b = request_grammar(&json!([
            { "role": "x", "content": "y" },
            { "role": "z", "content": "w" },
        ]));
        // Both reduce to a single distinct element grammar {role,content}.
        assert_eq!(a, b);
        assert_eq!(a, json!([{ "role": "<string>", "content": "<string>" }]));
    }

    #[test]
    fn count_independent_tools_reduce_identically() {
        // A 1-tool subject and a 3-tool authoritative reduce to the same grammar:
        // "an array of <the tool grammar>". Tool *count* is not grammar.
        let subject = json!({ "tools": [{ "name": "get_weather", "type": "function" }] });
        let authoritative = json!({ "tools": [
            { "name": "Read", "type": "function" },
            { "name": "Write", "type": "function" },
            { "name": "Bash", "type": "function" },
        ]});
        assert!(grammar_findings(&subject, &authoritative).is_empty());
    }

    #[test]
    fn subject_using_only_a_subset_of_keys_conforms() {
        // The authoritative request is richer; the subject is a subset → conformant.
        let subject = json!({ "model": "m", "messages": [{ "role": "user", "content": "x" }] });
        let authoritative = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 32000,
            "stream": true,
            "system": "big prompt",
            "messages": [{ "role": "user", "content": "the whole claude code prompt" }],
        });
        assert!(grammar_findings(&subject, &authoritative).is_empty());
    }

    #[test]
    fn subject_inventing_a_key_is_a_finding() {
        let subject = json!({ "model": "m", "weird_sdk_only_field": 1 });
        let authoritative = json!({ "model": "claude-sonnet-4-5" });
        let findings = grammar_findings(&subject, &authoritative);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].path.contains("weird_sdk_only_field"));
    }

    #[test]
    fn subject_wrong_type_is_a_finding() {
        // The official client sends `stream` as a bool; a subject sending it as a
        // string is a wire divergence.
        let subject = json!({ "stream": "true" });
        let authoritative = json!({ "stream": true });
        let findings = grammar_findings(&subject, &authoritative);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].detail.contains("type/shape differs"));
        assert!(findings[0].path.contains("stream"));
    }

    #[test]
    fn json_schema_properties_keys_are_content_not_grammar() {
        // Two tool schemas with *different argument names* but the same shape
        // (an object of typed properties) must reduce to the same grammar — the
        // property names are content. This is the false positive the content-map
        // collapse fixes (codex `get_weather` vs `exec_command`).
        let subject = json!({ "tools": [{
            "name": "get_weather", "type": "function",
            "parameters": { "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"] }
        }]});
        let authoritative = json!({ "tools": [{
            "name": "exec_command", "type": "function", "strict": false,
            "parameters": { "type": "object", "additionalProperties": false,
                "properties": {
                    "cmd": { "description": "shell command", "type": "string" },
                    "login": { "description": "login shell", "type": "boolean" },
                },
                "required": ["cmd"] }
        }]});
        // The subject's `properties` (one string prop) must conform to the
        // authoritative's (string + boolean props) — both collapse to a map whose
        // values are property-schema grammars; string is among them.
        let findings = grammar_findings(&subject, &authoritative);
        assert!(
            findings.is_empty(),
            "schema property names should be content, not grammar: {findings:?}"
        );
    }

    #[test]
    fn content_map_collapses_arbitrary_keys_to_a_wildcard() {
        let g = request_grammar(&json!({
            "properties": { "a": { "type": "string" }, "b": { "type": "string" } }
        }));
        // Both child schemas have the same grammar, so the map collapses to a
        // single wildcard key → that grammar.
        assert_eq!(
            g,
            json!({ "properties": { "<*>": { "type": "<string>" } } })
        );
    }

    #[test]
    fn subject_tool_call_encoding_must_match_authoritative() {
        // Both encode an assistant tool call the same way → conformant; a subject
        // that spelled it differently (e.g. `tool_calls` vs the dialect's shape)
        // would surface as an array-element divergence.
        let subject = json!({ "messages": [
            { "role": "assistant", "tool_calls": [
                { "id": "call_1", "type": "function",
                  "function": { "name": "get_weather", "arguments": "{}" } }
            ]}
        ]});
        let authoritative = json!({ "messages": [
            { "role": "user", "content": "hi" },
            { "role": "assistant", "tool_calls": [
                { "id": "call_x", "type": "function",
                  "function": { "name": "Read", "arguments": "{\"path\":\"a\"}" } }
            ]}
        ]});
        assert!(grammar_findings(&subject, &authoritative).is_empty());
    }
}
