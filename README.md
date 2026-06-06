# jig

`jig` is a small Rust process that impersonates an LLM provider so downstream
code can be exercised end-to-end without real credentials, network access, or
token spend. Behaviour is **scripted**, not generated — it only needs to be
faithful at the *transport and framing* level so a client SDK parses its replies
and drives its agent loop.

See [`bootstrap.md`](./bootstrap.md) for the full design.

## Routes

`jig` serves one route per wire dialect; the only integration seam is the
`base_url` a client is pointed at:

| Dialect | Route |
| --- | --- |
| OpenAI / DeepSeek chat-completions | `POST {base}/chat/completions` |
| Anthropic messages | `POST {base}/v1/messages` |
| OpenAI Codex responses | `POST {base}/backend-api/codex/responses` |

Every route streams Server-Sent Events with `Content-Type: text/event-stream`.
Auth headers are accepted but ignored. Unknown paths return `404`.

## Using it in-process (the test API)

`jig` runs a single-threaded tokio runtime on its own OS thread, so a
*synchronous* test can drive it with blocking HTTP and no async runtime of its
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
```

It prints the bound `base_url` on stdout and blocks until stdin closes (Ctrl-D)
or the process is signalled. The standalone binary is the same `FakeLlm::start`
call the tests use, preceded only by loading the script file and followed by a
block — there is no second implementation.

## Script file format

A script file is JSON describing what `jig` replies. The top level is exactly one
of `fixed` (serve the same reply every time) or `sequence` (serve replies in
order, then repeat the last once exhausted):

```json
{ "fixed": { "text": "hello" } }
```

```json
{ "sequence": [ { "text": "first" }, { "text": "second" } ] }
```

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

The `Script::Rule` variant — which decides the reply from the request — is
code-only and has no file representation; use the in-process API for that. See
[`crates/jig-core/src/script_file.rs`](crates/jig-core/src/script_file.rs) for the
authoritative schema and `jig_core::ScriptFile` to load it programmatically.

## Workspace layout

- `crates/jig-core` — dialect-agnostic, async-free core: `Reply`/`Turn`/`Usage`/
  `StopReason`, `Script`, the SSE renderers, and the `ScriptFile` file format.
- `crates/jig-server` — the embeddable service API (`FakeLlm`): spawns the
  runtime thread, runs the HTTP server, routes per dialect, renders replies.
- `src/main.rs` — thin glue binary: load a script file, `FakeLlm::start`, print
  `base_url`, block.
