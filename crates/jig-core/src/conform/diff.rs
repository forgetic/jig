//! A readable structural diff between two JSON values, for conformance failures.
//!
//! When a T1/T2 assertion fails, "templates differ" is useless — the operator
//! needs to see *which* path diverged and how. [`structural_diff`] walks two
//! [`Value`]s in lockstep and returns one human line per divergence, addressed
//! by a JSON-path-ish locator (`reply.turns[1].ToolCall.name`), so a failure
//! diff points straight at the structural delta (the issue #14 requirement
//! "failure diffs must be readable").
//!
//! It reports three kinds of divergence: a key present on only one side, an array
//! length mismatch, and a leaf value mismatch (including a type change). Object
//! keys are compared as a set so a reordering is not a false diff (JSON object
//! key order is not significant), while array order *is* significant (frame and
//! turn order are part of the contract).

use std::fmt::Write as _;

use serde_json::Value;

/// Compare two JSON values structurally and return one message per divergence,
/// each prefixed with the path to the differing node. An empty vec means the two
/// values are structurally equal. The `expected`/`actual` framing matches the
/// conformance tests: `expected` is the committed template, `actual` is what jig
/// produced (or the stripped recording).
pub fn structural_diff(expected: &Value, actual: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk("", expected, actual, &mut out);
    out
}

fn walk(path: &str, expected: &Value, actual: &Value, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(exp), Value::Object(act)) => {
            // Keys missing on the actual side.
            for key in exp.keys() {
                if !act.contains_key(key) {
                    out.push(format!(
                        "{}: missing key (expected present)",
                        child(path, key)
                    ));
                }
            }
            // Keys only on the actual side.
            for key in act.keys() {
                if !exp.contains_key(key) {
                    out.push(format!(
                        "{}: unexpected key (not in template)",
                        child(path, key)
                    ));
                }
            }
            // Recurse into shared keys.
            for (key, exp_val) in exp {
                if let Some(act_val) = act.get(key) {
                    walk(&child(path, key), exp_val, act_val, out);
                }
            }
        }
        (Value::Array(exp), Value::Array(act)) => {
            if exp.len() != act.len() {
                out.push(format!(
                    "{}: array length differs (expected {}, actual {})",
                    path_or_root(path),
                    exp.len(),
                    act.len()
                ));
            }
            for (i, (exp_val, act_val)) in exp.iter().zip(act.iter()).enumerate() {
                walk(&index(path, i), exp_val, act_val, out);
            }
        }
        (exp, act) if exp != act => {
            out.push(format!(
                "{}: value differs (expected {}, actual {})",
                path_or_root(path),
                render_leaf(exp),
                render_leaf(act)
            ));
        }
        _ => {}
    }
}

/// `path.key`, or just `key` at the root.
fn child(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        let mut s = String::with_capacity(path.len() + key.len() + 1);
        let _ = write!(s, "{path}.{key}");
        s
    }
}

/// `path[i]`.
fn index(path: &str, i: usize) -> String {
    let mut s = String::with_capacity(path.len() + 4);
    let _ = write!(s, "{path}[{i}]");
    s
}

/// `(root)` when the path is empty, else the path itself.
fn path_or_root(path: &str) -> &str {
    if path.is_empty() { "(root)" } else { path }
}

/// Compact one-line rendering of a leaf value for a diff message (a long string
/// is truncated so the message stays readable).
fn render_leaf(value: &Value) -> String {
    match value {
        Value::String(s) if s.len() > 40 => format!("{:?}…", &s[..40]),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn equal_values_have_no_diff() {
        let a = json!({ "a": 1, "b": [1, 2, { "c": "x" }] });
        let b = json!({ "b": [1, 2, { "c": "x" }], "a": 1 });
        // Key order does not matter.
        assert!(structural_diff(&a, &b).is_empty());
    }

    #[test]
    fn reports_a_leaf_value_mismatch_with_path() {
        let exp = json!({ "turns": [{ "ToolCall": { "name": "read" } }] });
        let act = json!({ "turns": [{ "ToolCall": { "name": "write" } }] });
        let diff = structural_diff(&exp, &act);
        assert_eq!(diff.len(), 1);
        assert!(
            diff[0].contains("turns[0].ToolCall.name"),
            "path missing in: {}",
            diff[0]
        );
        assert!(diff[0].contains("read") && diff[0].contains("write"));
    }

    #[test]
    fn reports_missing_and_unexpected_keys() {
        let exp = json!({ "stop": "Stop", "usage": {} });
        let act = json!({ "stop": "Stop", "extra": 1 });
        let diff = structural_diff(&exp, &act);
        assert!(
            diff.iter()
                .any(|d| d.contains("usage") && d.contains("missing"))
        );
        assert!(
            diff.iter()
                .any(|d| d.contains("extra") && d.contains("unexpected"))
        );
    }

    #[test]
    fn reports_array_length_mismatch() {
        let exp = json!({ "turns": [1, 2] });
        let act = json!({ "turns": [1] });
        let diff = structural_diff(&exp, &act);
        assert!(
            diff.iter()
                .any(|d| d.contains("turns") && d.contains("length"))
        );
    }
}
