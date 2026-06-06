//! The on-disk script file format.
//!
//! The standalone binary loads a [`Script`] from a file; this module defines the
//! file's schema and its conversion into the in-memory [`Script`]. The format is
//! a **public contract** that humans hand-write, so it is intentionally kept
//! separate from the internal [`crate::Reply`] / [`crate::Turn`] type
//! representations (whose default `serde` encodings are tag-heavy and awkward to
//! author by hand). Changing an internal type's derive must not silently reshape
//! the file format.
//!
//! Only the data-driven [`Script`] variants are expressible: [`Script::Fixed`]
//! and [`Script::Sequence`]. [`Script::Rule`] is a `Fn` closure — code-only by
//! nature — and has no file representation (see bootstrap.md: "the `Rule` variant
//! is code-only").
//!
//! # Schema
//!
//! The top level is exactly one of `fixed` or `sequence`:
//!
//! ```json
//! { "fixed": <reply> }
//! ```
//! ```json
//! { "sequence": [ <reply>, <reply>, ... ] }
//! ```
//!
//! A `<reply>` is either the **text shorthand**
//!
//! ```json
//! { "text": "hello" }
//! ```
//!
//! which expands to a single normal-stop text turn, or the **full form**
//!
//! ```json
//! {
//!   "turns": [ { "text": "thinking out loud" }, { "thinking": "hmm" },
//!              { "tool_call": { "id": "call_1", "name": "write",
//!                               "args": { "path": "out.txt" } } } ],
//!   "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
//!   "stop": "stop"
//! }
//! ```
//!
//! In the full form `usage` defaults to [`Usage::default`] and `stop` defaults to
//! `"stop"`, so the smallest full reply is `{ "turns": [ { "text": "hi" } ] }`.
//!
//! A `<turn>` is exactly one of:
//! - `{ "text": "…" }`
//! - `{ "thinking": "…" }`
//! - `{ "tool_call": { "id": "…", "name": "…", "args": <json> } }`
//!
//! A `<stop>` is one of `"stop"`, `"tool_calls"`, or `"error"`.

use serde::{Deserialize, Serialize};

use crate::{Reply, Script, StopReason, Turn, Usage};

/// A parsed script file: the data-driven half of [`Script`].
///
/// `serde`'s default externally-tagged enum encoding is exactly the
/// `{ "fixed": … }` / `{ "sequence": [ … ] }` schema documented on this module,
/// with the variant names lowercased.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptFile {
    /// Serve the same reply for every request — becomes [`Script::Fixed`].
    Fixed(ReplySpec),
    /// Serve replies in order, repeating the last once exhausted — becomes
    /// [`Script::Sequence`].
    Sequence(Vec<ReplySpec>),
}

/// A reply in the file format: either the `{ "text": … }` shorthand or the full
/// `{ "turns": …, "usage": …, "stop": … }` form.
///
/// `#[serde(untagged)]` lets a single string-text reply be written as
/// `{ "text": "…" }` while the full form carries explicit turns. The two arms are
/// unambiguous because the shorthand has a `text` key and the full form has a
/// `turns` key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReplySpec {
    /// `{ "text": "hello" }` — one normal-stop text turn.
    Text { text: String },
    /// The full form with explicit turns and optional usage / stop.
    Full {
        turns: Vec<TurnSpec>,
        #[serde(default)]
        usage: Usage,
        #[serde(default)]
        stop: StopSpec,
    },
}

/// One turn in the file format. Exactly one of the three keys is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnSpec {
    /// Plain assistant text.
    Text(String),
    /// Reasoning / "thinking" content.
    Thinking(String),
    /// A tool call the caller should execute.
    ToolCall(ToolCallSpec),
}

/// The fields of a [`TurnSpec::ToolCall`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallSpec {
    pub id: String,
    pub name: String,
    /// Arbitrary JSON arguments, passed through verbatim.
    #[serde(default)]
    pub args: serde_json::Value,
}

/// The stop reason in the file format. Lowercase, dialect-agnostic names that map
/// onto [`StopReason`]. Defaults to [`StopSpec::Stop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopSpec {
    /// Normal completion.
    #[default]
    Stop,
    /// The reply ends with tool calls the caller must execute.
    ToolCalls,
    /// The model signalled an error.
    Error,
}

/// Error from loading a script file: either the bytes were not valid JSON for the
/// schema, or (when reading from disk) the file could not be read.
#[derive(Debug)]
pub enum ScriptFileError {
    /// The file could not be read from disk.
    Io(std::io::Error),
    /// The bytes did not parse into the [`ScriptFile`] schema.
    Parse(serde_json::Error),
}

impl std::fmt::Display for ScriptFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScriptFileError::Io(err) => write!(f, "reading script file: {err}"),
            ScriptFileError::Parse(err) => write!(f, "parsing script file: {err}"),
        }
    }
}

impl std::error::Error for ScriptFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ScriptFileError::Io(err) => Some(err),
            ScriptFileError::Parse(err) => Some(err),
        }
    }
}

impl From<ScriptFileError> for std::io::Error {
    fn from(err: ScriptFileError) -> Self {
        match err {
            ScriptFileError::Io(err) => err,
            ScriptFileError::Parse(err) => {
                std::io::Error::new(std::io::ErrorKind::InvalidData, err)
            }
        }
    }
}

impl ScriptFile {
    /// Parse a script file from JSON bytes.
    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, ScriptFileError> {
        serde_json::from_slice(bytes).map_err(ScriptFileError::Parse)
    }

    /// Parse a script file from a JSON string.
    pub fn from_json_str(s: &str) -> Result<Self, ScriptFileError> {
        serde_json::from_str(s).map_err(ScriptFileError::Parse)
    }

    /// Read and parse a script file from a path.
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, ScriptFileError> {
        let bytes = std::fs::read(path).map_err(ScriptFileError::Io)?;
        Self::from_json_slice(&bytes)
    }

    /// Convert the parsed file into an in-memory [`Script`].
    pub fn into_script(self) -> Script {
        match self {
            ScriptFile::Fixed(reply) => Script::Fixed(reply.into_reply()),
            ScriptFile::Sequence(replies) => {
                Script::sequence(replies.into_iter().map(ReplySpec::into_reply).collect())
            }
        }
    }
}

impl ReplySpec {
    /// Lower a file-format reply into the canonical [`Reply`].
    pub fn into_reply(self) -> Reply {
        match self {
            ReplySpec::Text { text } => Reply::text(text),
            ReplySpec::Full { turns, usage, stop } => Reply {
                turns: turns.into_iter().map(TurnSpec::into_turn).collect(),
                usage,
                stop: stop.into_stop_reason(),
            },
        }
    }
}

impl TurnSpec {
    /// Lower a file-format turn into the canonical [`Turn`].
    pub fn into_turn(self) -> Turn {
        match self {
            TurnSpec::Text(text) => Turn::Text(text),
            TurnSpec::Thinking(text) => Turn::Thinking(text),
            TurnSpec::ToolCall(ToolCallSpec { id, name, args }) => {
                Turn::ToolCall { id, name, args }
            }
        }
    }
}

impl StopSpec {
    /// Map the file-format stop reason onto the canonical [`StopReason`].
    pub fn into_stop_reason(self) -> StopReason {
        match self {
            StopSpec::Stop => StopReason::Stop,
            StopSpec::ToolCalls => StopReason::ToolCalls,
            StopSpec::Error => StopReason::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dialect, RequestView};

    #[test]
    fn fixed_text_shorthand_round_trips_into_a_text_reply() {
        let file = ScriptFile::from_json_str(r#"{ "fixed": { "text": "hello" } }"#).unwrap();
        assert_eq!(
            file,
            ScriptFile::Fixed(ReplySpec::Text {
                text: "hello".into()
            })
        );

        let script = file.into_script();
        match script {
            Script::Fixed(reply) => assert_eq!(reply, Reply::text("hello")),
            _ => panic!("expected Script::Fixed"),
        }
    }

    #[test]
    fn sequence_of_text_shorthands_loads_in_order() {
        let json = r#"{ "sequence": [ { "text": "first" }, { "text": "second" } ] }"#;
        let script = ScriptFile::from_json_str(json).unwrap().into_script();

        // Drive the sequence through a throwaway view to confirm order + the
        // "last repeats once exhausted" behaviour from M2.
        let view = RequestView::new(Dialect::OpenAi, None, vec![], 0);
        assert_eq!(script.next_reply(&view), Reply::text("first"));
        assert_eq!(script.next_reply(&view), Reply::text("second"));
        assert_eq!(script.next_reply(&view), Reply::text("second"));
    }

    #[test]
    fn full_form_carries_turns_usage_and_stop() {
        let json = r#"
            {
              "fixed": {
                "turns": [
                  { "thinking": "let me think" },
                  { "tool_call": { "id": "call_1", "name": "write",
                                   "args": { "path": "out.txt" } } }
                ],
                "usage": { "prompt_tokens": 7, "completion_tokens": 9 },
                "stop": "tool_calls"
              }
            }
        "#;
        let file = ScriptFile::from_json_str(json).unwrap();
        let reply = match file {
            ScriptFile::Fixed(spec) => spec.into_reply(),
            _ => panic!("expected fixed"),
        };
        assert_eq!(
            reply,
            Reply {
                turns: vec![
                    Turn::Thinking("let me think".into()),
                    Turn::ToolCall {
                        id: "call_1".into(),
                        name: "write".into(),
                        args: serde_json::json!({ "path": "out.txt" }),
                    },
                ],
                usage: Usage {
                    prompt_tokens: 7,
                    completion_tokens: 9
                },
                stop: StopReason::ToolCalls,
            }
        );
    }

    #[test]
    fn full_form_usage_and_stop_default() {
        // The smallest full form: just turns. usage → default, stop → Stop.
        let json = r#"{ "fixed": { "turns": [ { "text": "hi" } ] } }"#;
        let reply = match ScriptFile::from_json_str(json).unwrap() {
            ScriptFile::Fixed(spec) => spec.into_reply(),
            _ => panic!("expected fixed"),
        };
        assert_eq!(reply.usage, Usage::default());
        assert_eq!(reply.stop, StopReason::Stop);
        assert_eq!(reply.turns, vec![Turn::Text("hi".into())]);
    }

    #[test]
    fn invalid_json_is_a_parse_error_not_a_panic() {
        let err = ScriptFile::from_json_str("not json at all").unwrap_err();
        assert!(matches!(err, ScriptFileError::Parse(_)));
    }

    #[test]
    fn unknown_top_level_variant_is_rejected() {
        // Neither `fixed` nor `sequence`: must not silently succeed.
        let err = ScriptFile::from_json_str(r#"{ "rule": {} }"#).unwrap_err();
        assert!(matches!(err, ScriptFileError::Parse(_)));
    }

    #[test]
    fn script_file_serializes_back_to_the_documented_schema() {
        // A round-trip through serialize → parse must be stable, which is what
        // makes the schema a dependable public contract.
        let file = ScriptFile::Sequence(vec![
            ReplySpec::Text { text: "a".into() },
            ReplySpec::Full {
                turns: vec![TurnSpec::Text("b".into())],
                usage: Usage::default(),
                stop: StopSpec::Stop,
            },
        ]);
        let json = serde_json::to_string(&file).unwrap();
        let reparsed = ScriptFile::from_json_str(&json).unwrap();
        assert_eq!(file, reparsed);
        // And the top-level tag is the documented lowercase `sequence`.
        assert!(json.contains("\"sequence\""));
    }
}
