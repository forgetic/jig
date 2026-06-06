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
| Anthropic messages *(M3)* | `POST {base}/v1/messages` |
| OpenAI Codex responses *(M4)* | `POST {base}/backend-api/codex/responses` |

Every route streams Server-Sent Events with `Content-Type: text/event-stream`.
Auth headers are accepted but ignored. Unknown paths return `404`.

**M1 implements the OpenAI / DeepSeek route only.**

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
cargo run
```

It prints the bound `base_url` and blocks. The standalone binary is the same
`FakeLlm::start` call the tests use, followed by a block — there is no second
implementation. (Script-file loading lands in M6.)

## Workspace layout

- `crates/jig-core` — dialect-agnostic, async-free core: `Reply`/`Turn`/`Usage`/
  `StopReason`, `Script`, and the SSE renderers.
- `crates/jig-server` — the embeddable service API (`FakeLlm`): spawns the
  runtime thread, runs the HTTP server, routes per dialect, renders replies.
- `src/main.rs` — thin glue binary.
