# jig

`jig` is a small Rust process that impersonates an LLM provider so downstream
code can be exercised end-to-end without real credentials, network access, or
token spend. Behaviour is **scripted**, not generated — it only needs to be
faithful at the *transport and framing* level so a client SDK parses its replies
and drives its agent loop.

## Routes

`jig` serves one route per wire dialect; the only integration seam is the
`base_url` a client is pointed at:

| Dialect | Route |
| --- | --- |
| OpenAI / DeepSeek chat-completions | `POST {base}/chat/completions` |
| Anthropic messages | `POST {base}/v1/messages` |
| OpenAI Codex responses | `POST {base}/backend-api/codex/responses` |

Successful replies stream Server-Sent Events with `Content-Type: text/event-stream`.
Scripted HTTP errors return a normal non-2xx response instead (no SSE or chunked
framing). Auth headers are accepted but ignored. Unknown paths return `404`.

## Using it in-process (the test API)

`jig` runs a single-threaded [skein](../skein) runtime on its own OS thread, so
a *synchronous* test can drive it with blocking HTTP and no async runtime of its
own:

```rust
use jig_core::{Reply, Script};
use jig_server::FakeLlm;

let fake = FakeLlm::start(Script::Fixed(Reply::text("hello"))).unwrap();
let url = fake.base_url(); // "http://127.0.0.1:PORT"
// ... point a blocking client at `url`, assert on the stream ...
// dropping `fake` signals shutdown and joins the runtime thread.
```

## Running the binary

```sh
# Serve a built-in default script (one fixed text reply for every request):
cargo run

# Or load a script file:
cargo run -- script.json

# Or drive the Temper basic-delivery phase fixture:
cargo run -- fixtures/basic-delivery.json

# Or drive the richer Temper reference-delivery fixture:
cargo run -- fixtures/reference-delivery.json
```

It prints the bound `base_url` on stdout and blocks until stdin closes (Ctrl-D)
or the process is signalled. The standalone binary is the same `FakeLlm::start`
call the tests use, preceded only by loading the script file and followed by a
block — there is no second implementation.

## Script file format

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

For fully custom Rust logic, `Script::Rule` remains available through the
in-process API, and `Script::action_rule` can return HTTP errors or future action
kinds. The `phases` file format is a data-driven subset of that power for
multi-phase workflow fixtures. See
[`crates/jig-core/src/script_file.rs`](crates/jig-core/src/script_file.rs) for the
authoritative schema and `jig_core::ScriptFile` to load it programmatically.

## Recording real interactions

`jig record` is a passthrough recorder: it proxies a real client ↔ real backend
exchange to redacted, client/role-tagged fixtures so the scripted replies above
can be derived from ground truth. Recording is manual (it needs a live API key
and network); the captured taxonomy and workflow live in
[`crates/jig-record/README.md`](crates/jig-record/README.md).

To refresh fixtures in one command, `xtask record` orchestrates the recorder
across the scenario matrix, and `xtask staleness` reports how old the committed
captures are:

```sh
cargo run -p xtask -- record --dialect openai   # one dialect (--all for everything)
cargo run -p xtask -- staleness                 # offline: flag captures past N days
```

See [the refresh how-to](docs/how-to/refresh-fixtures.md) for the full procedure
and [the record-and-conform explanation](docs/explanation/record-and-conform.md)
for the design.

## Workspace layout

- `crates/jig-core` — dialect-agnostic, async-free core: `Reply`/`Turn`/`Usage`/
  `StopReason`, `Script`, the SSE renderers, and the `ScriptFile` file format.
- `crates/jig-runtime` — the shared skein runtime bootstrap: builds the
  single-threaded reactor runtime and runs a future as a task with its `Cx`.
- `crates/jig-server` — the embeddable service API (`FakeLlm`): spawns the
  runtime thread, runs the HTTP server, routes per dialect, renders replies.
- `crates/jig-record` — the passthrough recorder: forwards a client request to
  the real upstream over HTTPS, streams the response back unbuffered, and writes
  a redacted fixture (redactor + fixture writer are unit-tested, network-free).
- `crates/xtask` — the developer task runner: the `record` orchestrator (expands
  the scenario matrix → drives `jig record`) and the offline `staleness` check.
- `src/main.rs` — thin glue binary: serve a script (`FakeLlm::start`) or run one
  passthrough capture (`record`).
