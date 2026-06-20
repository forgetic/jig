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
//! The file schema supports two simple data-driven scripts — [`Script::Fixed`]
//! and [`Script::Sequence`] — plus a phase-aware script that lowers to a
//! data-driven subset of [`Script::Rule`]. A phase script inspects each
//! [`RequestView`](crate::RequestView), picks the first matching phase, and
//! advances that phase's own sequence cursor. This lets one file model a
//! multi-step workflow (for example architect triage and engineer implementation)
//! without one phase's extra tool calls shifting the replies for another phase.
//!
//! # Schema
//!
//! The top level is exactly one of `fixed`, `sequence`, or `phases`:
//!
//! ```json
//! { "fixed": <reply> }
//! ```
//! ```json
//! { "sequence": [ <reply>, <reply>, ... ] }
//! ```
//! ```json
//! {
//!   "phases": [
//!     {
//!       "name": "architect-triage",
//!       "when": { "messages_contain": ["ROLE: architect"] },
//!       "sequence": [ <reply>, <reply>, ... ]
//!     },
//!     {
//!       "name": "engineer-implementation",
//!       "when": { "messages_contain": ["ROLE: engineer"] },
//!       "sequence": [ <reply>, <reply>, ... ]
//!     }
//!   ]
//! }
//! ```
//!
//! Phase matching is first-match-wins. A phase whose `when` is omitted (or whose
//! matcher has no fields) matches every request, so it can be used as a catch-all
//! by placing it last. Each phase's sequence repeats its last reply once
//! exhausted, exactly like top-level `sequence`. If no phase matches, or the
//! matching phase has an empty sequence, the script returns an empty text reply.
//!
//! A phase `when` matcher may use:
//! - `messages_contain`: all listed substrings must appear somewhere in the
//!   normalized transcript (`"role: content"` lines across all messages).
//!   `all_messages_contain` is accepted as an alias.
//! - `any_message_contains`: at least one listed substring must appear in the
//!   normalized transcript.
//! - `last_message_contains`: all listed substrings must appear in the last
//!   message's content.
//! - `prior_tool_results`: either an exact number (`1`) or a range object
//!   (`{ "min": 1 }`, `{ "max": 0 }`, or `{ "min": 1, "max": 3 }`).
//! - `model`: an exact model id.
//! - `dialect`: one of `"open_ai"`, `"anthropic"`, or `"codex"`.
//! - `ignore_case`: when `true`, string comparisons are case-insensitive.
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

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::{Dialect, Reply, RequestView, Script, StopReason, Turn, Usage};

/// A parsed script file.
///
/// `serde`'s default externally-tagged enum encoding is exactly the documented
/// `{ "fixed": … }` / `{ "sequence": [ … ] }` / `{ "phases": [ … ] }` schema,
/// with the variant names lowercased.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptFile {
    /// Serve the same reply for every request — becomes [`Script::Fixed`].
    Fixed(ReplySpec),
    /// Serve replies in order, repeating the last once exhausted — becomes
    /// [`Script::Sequence`].
    Sequence(Vec<ReplySpec>),
    /// Select a named phase from the request and advance that phase's own
    /// sequence cursor — becomes a data-driven [`Script::Rule`].
    Phases(Vec<PhaseSpec>),
}

/// One named phase in a [`ScriptFile::Phases`] script.
///
/// Phases are checked in file order. The first phase whose [`PhaseMatcher`]
/// matches the incoming [`RequestView`] serves the next reply from `sequence`.
/// The `name` is for humans and diagnostics; selection is entirely by `when`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseSpec {
    pub name: String,
    #[serde(default)]
    pub when: PhaseMatcher,
    pub sequence: Vec<ReplySpec>,
}

/// Request predicates for selecting a phase.
///
/// All populated fields must match. Empty lists are ignored, and an entirely
/// empty matcher matches every request (useful as a final catch-all phase).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PhaseMatcher {
    /// All listed substrings must appear somewhere in the normalized transcript.
    #[serde(alias = "all_messages_contain")]
    pub messages_contain: Vec<String>,
    /// At least one listed substring must appear somewhere in the normalized
    /// transcript. An empty list imposes no condition.
    pub any_message_contains: Vec<String>,
    /// All listed substrings must appear in the last message's content. An empty
    /// list imposes no condition.
    pub last_message_contains: Vec<String>,
    /// Match by prior tool-result count.
    pub prior_tool_results: Option<CountSpec>,
    /// Match by exact model id.
    pub model: Option<String>,
    /// Match by dialect route.
    pub dialect: Option<DialectSpec>,
    /// Make string predicates case-insensitive.
    pub ignore_case: bool,
}

/// A numeric guard used by [`PhaseMatcher::prior_tool_results`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CountSpec {
    /// Match exactly this count.
    Exact(usize),
    /// Match an inclusive range. Missing bounds are open-ended.
    Range {
        min: Option<usize>,
        max: Option<usize>,
    },
}

/// Dialect names accepted in a phase matcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialectSpec {
    OpenAi,
    Anthropic,
    Codex,
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
            ScriptFile::Phases(phases) => phases_into_script(phases),
        }
    }
}

/// Lower a phase file script into a rule closure with one sequence cursor per
/// phase. Keeping the cursors independent is what prevents an extra tool turn in
/// one phase from consuming another phase's reply.
fn phases_into_script(phases: Vec<PhaseSpec>) -> Script {
    struct CompiledPhase {
        when: PhaseMatcher,
        replies: Vec<Reply>,
    }

    let phases: Vec<CompiledPhase> = phases
        .into_iter()
        .map(|phase| CompiledPhase {
            when: phase.when,
            replies: phase
                .sequence
                .into_iter()
                .map(ReplySpec::into_reply)
                .collect(),
        })
        .collect();
    let cursors = Mutex::new(vec![0usize; phases.len()]);

    Script::rule(move |view| {
        let Some(phase_index) = phases.iter().position(|phase| phase.when.matches(view)) else {
            return Reply::text("");
        };
        let phase = &phases[phase_index];
        if phase.replies.is_empty() {
            return Reply::text("");
        }

        let mut cursors = cursors.lock().unwrap_or_else(|p| p.into_inner());
        let cursor = &mut cursors[phase_index];
        let chosen = phase.replies[*cursor].clone();
        if *cursor + 1 < phase.replies.len() {
            *cursor += 1;
        }
        chosen
    })
}

impl PhaseMatcher {
    /// Return true when every populated predicate in this matcher matches the
    /// request view.
    pub fn matches(&self, view: &RequestView) -> bool {
        let transcript = normalized_transcript(view);
        if !contains_all(&transcript, &self.messages_contain, self.ignore_case) {
            return false;
        }
        if !contains_any(&transcript, &self.any_message_contains, self.ignore_case) {
            return false;
        }

        let last = view
            .last_message()
            .map(|message| message.content.as_str())
            .unwrap_or("");
        if !contains_all(last, &self.last_message_contains, self.ignore_case) {
            return false;
        }

        if let Some(count) = &self.prior_tool_results
            && !count.matches(view.prior_tool_results)
        {
            return false;
        }
        if let Some(expected) = &self.model {
            let Some(actual) = view.model.as_deref() else {
                return false;
            };
            if !string_eq(actual, expected, self.ignore_case) {
                return false;
            }
        }
        if let Some(expected) = self.dialect
            && !expected.matches(view.dialect)
        {
            return false;
        }

        true
    }
}

impl CountSpec {
    /// Return true when `count` satisfies this exact/range matcher.
    pub fn matches(&self, count: usize) -> bool {
        match self {
            CountSpec::Exact(expected) => count == *expected,
            CountSpec::Range { min, max } => {
                min.as_ref().is_none_or(|min| count >= *min)
                    && max.as_ref().is_none_or(|max| count <= *max)
            }
        }
    }
}

impl DialectSpec {
    /// Return true when this file-format dialect matches the request dialect.
    pub fn matches(self, dialect: Dialect) -> bool {
        matches!(
            (self, dialect),
            (DialectSpec::OpenAi, Dialect::OpenAi)
                | (DialectSpec::Anthropic, Dialect::Anthropic)
                | (DialectSpec::Codex, Dialect::Codex)
        )
    }
}

/// Render the request's message list into the text surface matchers inspect.
fn normalized_transcript(view: &RequestView) -> String {
    let mut transcript = String::new();
    for message in &view.messages {
        transcript.push_str(&message.role);
        transcript.push_str(": ");
        transcript.push_str(&message.content);
        transcript.push('\n');
    }
    transcript
}

fn contains_all(haystack: &str, needles: &[String], ignore_case: bool) -> bool {
    if needles.is_empty() {
        return true;
    }
    if ignore_case {
        let haystack = haystack.to_lowercase();
        needles
            .iter()
            .all(|needle| haystack.contains(&needle.to_lowercase()))
    } else {
        needles.iter().all(|needle| haystack.contains(needle))
    }
}

fn contains_any(haystack: &str, needles: &[String], ignore_case: bool) -> bool {
    if needles.is_empty() {
        return true;
    }
    if ignore_case {
        let haystack = haystack.to_lowercase();
        needles
            .iter()
            .any(|needle| haystack.contains(&needle.to_lowercase()))
    } else {
        needles.iter().any(|needle| haystack.contains(needle))
    }
}

fn string_eq(left: &str, right: &str, ignore_case: bool) -> bool {
    if ignore_case {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
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
    use crate::{Dialect, RequestView, ViewMessage};

    fn view_with_message(content: &str) -> RequestView {
        RequestView::new(
            Dialect::OpenAi,
            Some("fake".to_string()),
            vec![ViewMessage {
                role: "system".to_string(),
                content: content.to_string(),
            }],
            0,
        )
    }

    fn view_with_tool_results(count: usize) -> RequestView {
        RequestView::new(Dialect::OpenAi, None, vec![], count)
    }

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
    fn phases_branch_by_message_content_and_keep_independent_sequences() {
        let json = r#"
            {
              "phases": [
                {
                  "name": "architect-triage",
                  "when": { "messages_contain": ["ROLE: architect"] },
                  "sequence": [ { "text": "architect first" }, { "text": "architect final" } ]
                },
                {
                  "name": "engineer-implementation",
                  "when": { "messages_contain": ["ROLE: engineer"] },
                  "sequence": [ { "text": "engineer first" }, { "text": "engineer final" } ]
                }
              ]
            }
        "#;
        let script = ScriptFile::from_json_str(json).unwrap().into_script();
        let architect = view_with_message("ROLE: architect (triage_workspace capability)");
        let engineer = view_with_message("ROLE: engineer (coding_workspace capability)");

        assert_eq!(
            script.next_reply(&architect),
            Reply::text("architect first")
        );
        assert_eq!(script.next_reply(&engineer), Reply::text("engineer first"));
        // Returning to the architect phase uses the architect cursor, not the
        // global position that the engineer request advanced.
        assert_eq!(
            script.next_reply(&architect),
            Reply::text("architect final")
        );
        assert_eq!(script.next_reply(&engineer), Reply::text("engineer final"));
        assert_eq!(script.next_reply(&engineer), Reply::text("engineer final"));
    }

    #[test]
    fn phase_matchers_can_use_tool_result_counts_and_catch_all_phases() {
        let json = r#"
            {
              "phases": [
                {
                  "name": "after-tool",
                  "when": { "prior_tool_results": { "min": 1 } },
                  "sequence": [ { "text": "after tool" } ]
                },
                {
                  "name": "catch-all",
                  "sequence": [ { "text": "before tool" } ]
                }
              ]
            }
        "#;
        let script = ScriptFile::from_json_str(json).unwrap().into_script();

        assert_eq!(
            script.next_reply(&view_with_tool_results(0)),
            Reply::text("before tool")
        );
        assert_eq!(
            script.next_reply(&view_with_tool_results(1)),
            Reply::text("after tool")
        );
        assert_eq!(
            script.next_reply(&view_with_tool_results(3)),
            Reply::text("after tool")
        );
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
        // Neither `fixed`, `sequence`, nor `phases`: must not silently succeed.
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
