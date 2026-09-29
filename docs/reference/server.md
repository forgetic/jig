# The fake LLM server: routes, API and binary

Reference for `jig-server`: the routes it serves, how to run it in-process or
with no sockets at all, and the `jig` binary. What it replies is set by a
script; see [the script file format](script-file.md).

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

