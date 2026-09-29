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

`FakeLlm::start` binds a loopback port on the caller's thread, then serves it
from one OS thread of its own. That thread runs a single-threaded loop that
never awaits, so a *synchronous* test can drive it with blocking HTTP and no
async runtime of its own:

```rust
use jig_core::{Reply, Script};
use jig_server::FakeLlm;

let fake = FakeLlm::start(Script::Fixed(Reply::text("hello"))).unwrap();
let url = fake.base_url(); // "http://127.0.0.1:PORT"
// ... point a blocking client at `url`, assert on the stream ...
let seen = fake.requests(); // every request so far, as a Vec<RecordedRequest>
// dropping `fake` stops the loop and joins its thread.
```

How the server behaves:

- **Concurrent connections.** An idle or slow client does not hold up the
  others. Script cursors advance in the order requests complete, and
  `requests()` lists them in that order. A client making sequential requests
  sees no difference.
- **One request per connection.** Every response carries `Connection: close`.
- **Strict request heads.** The HTTP layer answers bad requests itself, so they
  never reach the script or `requests()`. A malformed head or `Content-Length`
  gets `400`. A head over 64 KiB, more than 100 header fields, or a declared
  body over 64 MiB gets `413`. A request with `Transfer-Encoding` gets `501`.
- **Bounded shutdown.** Dropping the handle closes the listener and every
  connection that has not sent a request. Responses still being written get a
  1 s grace to finish and are then cut. `drop` then joins the thread, so a
  client that stops reading cannot hang it.

The layers under `FakeLlm` are public. `jig_server::serve_request` drives the
pure provider core with one request and no sockets at all:

```rust
use jig_core::{Reply, Script};
use jig_server::provider::Request;
use jig_server::{Provider, serve_request};

let (plan, mut rule) = Script::Fixed(Reply::text("hello")).split();
let mut provider = Provider::new(plan);
let request = Request {
    method: "POST".to_string(),
    target: "/chat/completions".to_string(),
    headers: Vec::new(),
    body: br#"{"model":"m","stream":true,"messages":[]}"#.to_vec(),
};
let (response, recorded) = serve_request(&mut provider, rule.as_mut(), request);
assert_eq!(response.status, 200);
assert_eq!(recorded.path, "/chat/completions");
```

`Provider`, `ServerIo` and `FakeLlmHost` are the core, I/O step and host that
`FakeLlm` runs, for an embedder that wants its own loop.

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

For fully custom Rust logic, `Script::rule` remains available through the
in-process API, and `Script::action_rule` can return HTTP errors or future action
kinds. Their closures are `FnMut + Send`: `FakeLlm` calls them on its loop
thread, and they may keep state. The `phases` file format is a data-driven
subset of that power for multi-phase workflow fixtures. See
[`crates/jig-core/src/script_file.rs`](crates/jig-core/src/script_file.rs) for the
authoritative schema and `jig_core::ScriptFile` to load it programmatically.

## Recording real interactions

`jig record` is a passthrough recorder: it proxies a real client ↔ real backend
exchange to redacted, client/role-tagged fixtures so the scripted replies above
can be derived from ground truth. Recording is manual (it needs a live API key
and network); the captured taxonomy and workflow live in
[`crates/jig-record/README.md`](crates/jig-record/README.md).

From Rust, the recorder is `jig_record::Recorder`, built like `FakeLlm`: one
loop thread over a loopback listener, serving client connections concurrently.
Paths that are not a dialect route (connectivity preflights such as `HEAD /`)
get `204`. Dialect routes are forwarded over HTTPS, and the response is relayed
back unbuffered while it is captured. `RecorderConfig::mode` sets how many
exchanges it captures:

- `Mode::Once` (the default) stops after the first routable exchange ends, and
  captures it if it completed. `next_capture(timeout)` returns that capture. If
  the exchange failed, `next_capture` fails as soon as the recorder stops, and
  stderr says why. `jig_record::record_once` is this mode plus writing the
  fixture; `jig record` calls it.
- `Mode::Pump` captures every routable exchange until `stop()`, which returns
  the captures not yet taken. Relays still in flight get a 1 s grace and are
  captured if they finish. The capture examples use this mode for clients that
  make several requests.

```rust
use jig_record::{Mode, Recorder, RecorderConfig};

let recorder = Recorder::start(RecorderConfig { mode: Mode::Pump, ..RecorderConfig::default() })?;
let url = recorder.base_url(); // point the client here
// ... drive the client ...
let exchanges = recorder.stop(); // Vec<(ClientRequest, UpstreamResponse, Route)>
```

`RecorderConfig::upstream_host` forwards to another host than the dialect's
own (DeepSeek or a gateway for the OpenAI dialect). `upstream` (connect to this
address, with or without TLS) and `roots` (trust these CA roots instead of
webpki's) are test hooks.

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

## Architecture

The server and the recorder have the same shape. Business logic and I/O
planning are pure step functions. One loop per OS thread waits on a reactor,
feeds what happened to the pure steps, and performs the syscalls they plan.
Nothing awaits, and there is no async runtime.
[The sans-IO shell](docs/explanation/sans-io-shell.md) explains the design and
the reasoning behind it.

## Workspace layout

- `crates/steploop` — the generic loop, with no jig types: the reactor (an
  owned wrapper over `polling`), the `run` driver, tap and replay, and pure
  TCP, TLS client (feature `tls`) and HTTP/1 server and client planners.
- `crates/jig-core` — dialect-agnostic, async-free core: `Reply`/`Turn`/`Usage`/
  `StopReason`, `Script` (split into a data `Plan` and an optional `Rule`
  closure), request views, the SSE renderers and parsers, the conformance
  masking and templates, and the `ScriptFile` file format.
- `crates/jig-server` — the embeddable service API: the pure provider core
  (`Provider`, `serve_request`), its I/O step (`ServerIo`), its host
  (`FakeLlmHost`), and `FakeLlm`, which runs the three on one loop thread.
- `crates/jig-record` — the passthrough recorder, layered the same way: pure
  routing, redaction and fixture writer, the relay core (`RecorderCore`), its
  I/O step (`RecorderIo`), its host (`RecorderHost`), and the `Recorder`
  handle. The capture harnesses are its examples.
- `crates/xtask` — the developer task runner: the `record` orchestrator (expands
  the scenario matrix and runs each cell's capture harness), `derive` (templates
  from recordings) and the offline `staleness` check.
- `src/main.rs` — thin glue binary: serve a script (`FakeLlm::start`) or run one
  passthrough capture (`record`, via `jig_record::record_once`).
