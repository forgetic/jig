# Script file format

Reference for the JSON script format the `jig` binary loads, and for the
Rust-only rule closures.

A script file is JSON describing what `jig` replies. The top level is exactly one
of:

- `fixed`: serve the same reply every time
- `sequence`: serve replies in order, then repeat the last once exhausted
- `phases`: inspect the incoming request, pick the first matching named phase,
  and advance that phase's own sequence cursor
- `reference_delivery`: Temper's built-in operator-demo rule, deriving target
  repositories and checkout paths from each request

```json
{ "fixed": { "text": "hello" } }
```

```json
{ "sequence": [ { "text": "first" }, { "text": "second" } ] }
```

```json
{
  "phases": [
    {
      "name": "architect-triage",
      "when": { "messages_contain": ["ROLE: architect"] },
      "sequence": [
        { "text": "architect first" },
        { "text": "architect final" }
      ]
    },
    {
      "name": "engineer-implementation",
      "when": { "messages_contain": ["ROLE: engineer"] },
      "sequence": [
        { "text": "engineer first" },
        { "text": "engineer final" }
      ]
    }
  ]
}
```

Phase matching is first-match-wins. Omit `when` on a final phase to make it a
catch-all. Each phase sequence repeats its own last reply once exhausted, so an
extra tool loop in the architect phase does not consume the engineer phase's
first reply. Matchers can check message substrings (`messages_contain`,
`any_message_contains`, `last_message_contains`), `prior_tool_results`, `model`,
`dialect`, and `ignore_case`.

```json
{ "reference_delivery": {} }
```

The `reference_delivery` built-in keeps the richer Temper demo configurable: it
parses the request transcript to find `target_repo` entries, repository checkout
lines, and architect/engineer/reviewer roles, then emits deterministic role
replies.

A **reply** is either the `{ "text": "…" }` shorthand — one normal-stop text turn
— or the full form with explicit turns and optional `usage` / `stop`:

```json
{
  "fixed": {
    "turns": [
      { "thinking": "let me think" },
      { "text": "here is the answer" },
      { "tool_call": { "id": "call_1", "name": "write",
                       "args": { "path": "out.txt", "contents": "hi" } } }
    ],
    "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
    "stop": "tool_calls"
  }
}
```

- A **turn** is exactly one of `{ "text": "…" }`, `{ "thinking": "…" }`, or
  `{ "tool_call": { "id": "…", "name": "…", "args": <json> } }`.
- `stop` is one of `"stop"` (default), `"tool_calls"`, or `"error"`.
- `usage` defaults to `{ "prompt_tokens": 1, "completion_tokens": 1 }`.

A **script action** is either a successful reply (the existing format) or an
HTTP error action. Use `http_error` when a test needs the client to observe a
provider/API failure rather than a normal terminal model stop:

```json
{
  "sequence": [
    {
      "http_error": {
        "status": 500,
        "code": "server_error",
        "message": "temporary upstream failure"
      }
    },
    { "text": "eventual success" }
  ]
}
```

`http_error.dialect` is optional; when omitted, the body shape follows the route
being called. OpenAI chat-completions and Codex responses render
`{"error":{"code":"…","message":"…"}}`; Anthropic messages render
`{"type":"error","error":{"type":"…","message":"…"}}`. Any `extra` object is
merged into the provider `error` object, which can express fixtures such as
Codex usage limits (`plan_type`, `resets_at`). For exact parser-regression
fixtures, use a raw body:

```json
{
  "fixed": {
    "http_error": {
      "status": 502,
      "raw": { "content_type": "text/plain", "body": "bad gateway" }
    }
  }
}
```

For fully custom Rust logic, `Script::rule` remains available through the
in-process API, and `Script::action_rule` can return HTTP errors or future action
kinds. Their closures are `FnMut + Send`: `FakeLlm` calls them on its loop
thread, and they may keep state. The `phases` file format is a data-driven
subset of that power for multi-phase workflow fixtures. See
[`crates/jig-core/src/script_file.rs`](../../crates/jig-core/src/script_file.rs) for the
authoritative schema and `jig_core::ScriptFile` to load it programmatically.

