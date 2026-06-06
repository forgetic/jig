# Bootstrapping `jig` — a fake LLM provider for e2e testing

`jig` is a small Rust process that impersonates an LLM provider so that
downstream code (initially [`smith`](../smith)) can be exercised end-to-end
without real credentials, network access, or token spend. It speaks the wire
dialects smith's SDK (`pi_agent_rust`) actually uses: **OpenAI/DeepSeek
chat-completions**, **Anthropic messages**, and **OpenAI Codex responses**.

It does not need to be a complete or correct model — only faithful enough at the
*transport and framing* level that the client SDK parses its replies and drives
the agent loop. Behaviour is **scripted**, not generated.

## Why this shape

These findings (from reading `pi_agent_rust 0.1.13` and smith's provider wiring)
drive every decision below:

1. **All three dialects stream Server-Sent Events.** Every provider sets
   `stream: true` and *requires* `Content-Type: text/event-stream` — a non-SSE
   content type is rejected outright. The fake cannot return a plain JSON body;
   it must emit SSE frames.

2. **The only integration seam is `base_url`.** Each dialect normalizes a custom
   base URL deterministically, so pointing a client at `http://127.0.0.1:PORT`
   routes cleanly:

   | API string (smith auth mode) | Route `jig` must serve |
   | --- | --- |
   | `openai-completions` (DeepSeek / OpenAI-compatible) | `POST {base}/chat/completions` |
   | `anthropic-messages` | `POST {base}/v1/messages` |
   | `openai-codex-responses` | `POST {base}/backend-api/codex/responses` |

   (See `normalize_openai_base`, `normalize_anthropic_base`,
   `normalize_openai_codex_responses_base` in `pi_agent_rust`'s
   `providers/mod.rs`.)

3. **Auth is irrelevant.** `jig` ignores the bearer / `x-api-key` entirely. This
   matters for client plumbing: smith's `ProviderConfig::new()` already accepts
   an arbitrary OpenAI-compatible `base_url` + dummy key, so the **DeepSeek/OpenAI
   dialect needs zero smith changes**. The Anthropic and Codex auth modes
   hardcode their base URL, so reaching `jig` through them needs a small
   test-gated `base_url` override on the smith side (out of scope for `jig`
   itself, tracked as a follow-up).

4. **The runtime is single-threaded and lives on its own thread, isolated from
   callers.** `jig` runs on a **single-threaded tokio runtime**
   (`tokio::runtime::Builder::new_current_thread`) hosted on one dedicated OS
   thread that the public API spawns. We use tokio from the start so the
   async HTTP/SSE plumbing is idiomatic, and the *same* code path serves both
   the standalone process and the embedded library. Two consequences matter:

   - Clients reach the server via `reqwest` over HTTP, so the runtime is
     invisible to them.
   - Because the runtime lives on its own thread, **in-process tests never share
     the server's executor.** A *synchronous* test can `start` a `FakeLlm`, make
     ordinary blocking `reqwest` calls against `base_url()`, assert on captured
     requests, and `drop` it — with no `#[tokio::main]` or async runtime of its
     own. Starting/stopping the `jig` thread is the entire lifecycle.

## Code organization

`jig` is a Cargo **workspace**. The binary at the repository root is *only* glue;
all behaviour lives in sub-crates under `crates/` so it is unit-testable and
embeddable.

- **`src/main.rs`** (root package `jig`, the binary) — thin glue: load a script,
  call the service API, print the bound `base_url`, then block. No logic beyond
  argument/script wiring.
- **`crates/jig-core`** — the dialect-agnostic logic, no async: the canonical
  `Reply`/`Turn` model, the three SSE renderers, dialect body parsing into
  `RequestView`, the `Script` types, and `RecordedRequest`. Pure and synchronous;
  fast to unit-test.
- **`crates/jig-server`** — the embeddable **service API**. Owns `FakeLlm`, which
  spawns the dedicated thread + single-threaded tokio runtime, runs the HTTP
  server, routes per dialect, and renders `jig-core` replies as SSE. This crate
  is what `src/main.rs` *and* in-process tests use.

## Design

### Core abstraction: dialect-agnostic scripts

Define one canonical reply shape and render it into each wire dialect. This keeps
the interesting logic (what the model "says") in one place and isolates the three
SSE encoders. These types live in `jig-core`.

```rust
/// One thing the fake model emits within a single assistant turn.
enum Turn {
    Text(String),
    Thinking(String),
    ToolCall { id: String, name: String, args: serde_json::Value },
}

/// A single assistant response (one HTTP request → one streamed reply).
struct Reply {
    turns: Vec<Turn>,
    usage: Usage,        // input/output token counts (can be canned)
    stop: StopReason,    // stop | tool_calls | error
}
```

Three renderers consume a `Reply`:

- `render_openai(&Reply) -> Vec<SseFrame>`
- `render_anthropic(&Reply) -> Vec<SseFrame>`
- `render_codex(&Reply) -> Vec<SseFrame>`

### Minimal SSE sequences each SDK parser accepts

**OpenAI / DeepSeek** (`data:`-only frames, terminated by `[DONE]`):

```
data: {"choices":[{"delta":{"role":"assistant"},"finish_reason":null}]}

data: {"choices":[{"delta":{"content":"{\"action\""},"finish_reason":null}]}

data: {"choices":[{"delta":{"content":":\"do_thing\"}"},"finish_reason":null}]}

data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}

data: [DONE]
```

Tool calls ride in `choices[].delta.tool_calls[]` (`index`, `id`,
`function.name`, `function.arguments` — arguments may be chunked across frames).

**Anthropic** (requires `event:` lines):

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"fake","content":[],"stop_reason":null,"usage":{"input_tokens":1,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"..."}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}

event: message_stop
data: {"type":"message_stop"}
```

Tool calls = a `content_block_start` with `content_block.type":"tool_use"` (id,
name), `input_json_delta` deltas, then `content_block_stop`. `ping` events are
ignored by the client and may be omitted.

**OpenAI Codex responses** (requires `event:` lines):

```
event: response.output_text.delta
data: {"type":"response.output_text.delta","item_id":"msg_1","content_index":0,"delta":"..."}

event: response.completed
data: {"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1}}}
```

Tool calls = `response.output_item.added` (item `type":"function_call"` with id,
call_id, name) + `response.function_call_arguments.delta` +
`response.output_item.done`.

### Scripting / driving model

smith has two caller patterns with different needs:

- **`run_decision`** — one tool-less turn returning a single JSON object.
  Trivial: a `Reply` with one `Turn::Text(json)`.
- **`run_coding_agent`** — a real tool-use loop (read/edit/bash). The fake must
  serve a *sequence* of replies across successive HTTP requests, e.g. turn 1 =
  a `write` `ToolCall`, turn 2 = final JSON text.

Support both with a **`Script`** (in `jig-core`) that yields the next `Reply` per
request:

```rust
enum Script {
    /// Same reply every request.
    Fixed(Reply),
    /// Replies in order; last one repeats once exhausted.
    Sequence(Vec<Reply>),
    /// Decide based on the parsed request (turn count + last message).
    Rule(Box<dyn Fn(&RequestView) -> Reply + Send + Sync>),
}
```

`RequestView` exposes a normalized read-only view of the incoming request (dialect,
model id, messages/instructions, the count of prior tool results) so a rule can
branch without each rule re-parsing three different body schemas. The server
parses the dialect-specific body once and projects it into `RequestView`.

### Runtime & HTTP layer

A **single-threaded tokio runtime**
(`tokio::runtime::Builder::new_current_thread().enable_all()`) built on a
dedicated OS thread that `FakeLlm::start` spawns. The server uses
`tokio::net::TcpListener` bound to `127.0.0.1:0` (OS-assigned port). For HTTP/1.1
+ chunked SSE, use a minimal async HTTP layer over tokio — `hyper`
(`http-body-util` for the streaming body) is the natural fit since we already
depend on tokio; hand-rolling the request reader + chunked SSE writer directly on
the tokio socket is also acceptable if it stays smaller. Pick whichever is
leanest; reach for `hyper` if hand-rolling proves fiddly.

Rationale:

- Single-threaded keeps ordering deterministic and the footprint tiny — this is a
  low-traffic test double, not a server under load.
- Hosting the runtime on its own thread keeps it off the caller's executor, so
  embedded tests stay synchronous and need no async runtime of their own.
- One async path serves both the standalone binary and the embedded library — no
  behavioural divergence.

Responsibilities:

- Route on path → dialect (table above). Unknown path → `404`.
- Read the request body; project into `RequestView`.
- Ask the `Script` for a `Reply`; render to frames for the route's dialect.
- Respond `200` + `Content-Type: text/event-stream` + chunked frames.
- Be permissive about auth headers (accept anything, including none).
- Record each request for later assertion (shared state behind `Arc<Mutex<…>>`,
  reachable from both the runtime thread and the caller's thread).

### Public API (library-first)

The `jig-server` crate exposes the embeddable service API. This handle *is* the
in-process test API.

```rust
pub struct FakeLlm { /* runtime-thread JoinHandle + addr + shutdown signal */ }

impl FakeLlm {
    /// Spawn a dedicated OS thread hosting a single-threaded tokio runtime that
    /// serves `script` until the handle is dropped.
    pub fn start(script: Script) -> std::io::Result<FakeLlm>;
    pub fn base_url(&self) -> String;   // "http://127.0.0.1:PORT"
    pub fn requests(&self) -> Vec<RecordedRequest>; // for assertions
}
impl Drop for FakeLlm { /* signal shutdown, join the thread */ }
```

A synchronous test calls `FakeLlm::start(script)`, points a client at
`base_url()`, asserts on `requests()`, and lets `Drop` tear the thread down — no
async machinery in the test itself. The standalone binary (`src/main.rs`) is the
same `start` call followed by a block; there is no second implementation.

## Repository layout

```
jig/
├── Cargo.toml              # [workspace] + thin [package] jig (the binary)
├── README.md               # what it is, how to run, the three routes
├── bootstrap.md            # this file
├── src/
│   └── main.rs             # thin glue: load script, FakeLlm::start, print base_url, block
└── crates/
    ├── jig-core/           # dialect-agnostic logic, no async
    │   ├── Cargo.toml      # deps: serde, serde_json
    │   └── src/
    │       ├── lib.rs      # Turn, Reply, Usage, StopReason, Script, RequestView, RecordedRequest
    │       ├── request.rs  # parse each dialect's body → RequestView
    │       └── render/
    │           ├── mod.rs
    │           ├── openai.rs    # chat-completions SSE
    │           ├── anthropic.rs # messages SSE
    │           └── codex.rs     # responses SSE
    └── jig-server/         # the embeddable service API
        ├── Cargo.toml      # deps: jig-core, tokio (current-thread rt + net + io), serde_json, hyper?
        ├── src/
        │   ├── lib.rs      # FakeLlm::start/base_url/requests + Drop (spawns the runtime thread)
        │   └── server.rs   # tokio current-thread loop, routing, SSE writing
        └── tests/
            ├── openai_dialect.rs    # start FakeLlm, raw HTTP → assert SSE frames parse
            ├── anthropic_dialect.rs
            └── codex_dialect.rs
```

Keep `serde_json` for bodies/frames and `tokio` on `new_current_thread`. Keep
`jig-core` async-free so renderers and parsers unit-test without a runtime; all
async lives in `jig-server`.

## Milestones

1. **Workspace skeleton + OpenAI dialect.** Stand up the workspace (root binary
   package + `crates/jig-core` + `crates/jig-server`), the core `Reply`/`Turn`
   types and `Script::Fixed` in `jig-core`, `render_openai`, and the
   `jig-server` service API: `FakeLlm::start` spawning a single-threaded tokio
   runtime on its own thread, `tokio::net::TcpListener`, route
   `/chat/completions`, `Drop` shutdown. Test: a synchronous test starts
   `FakeLlm`, hits `base_url()` with `reqwest`, and asserts a parseable streamed
   reply ending in `[DONE]`. This is the smallest thing that proves the
   transport *and* the in-process API.
2. **Request capture + `Sequence`/`Rule` scripts + `RequestView`.** Enables
   multi-turn tool-use scripting and request assertions.
3. **Anthropic dialect.** `render_anthropic` + `/v1/messages` route + dialect
   test.
4. **Codex dialect.** `render_codex` + `/backend-api/codex/responses` route +
   tool-call output items + dialect test.
5. **Tool-call rendering across all three dialects** (text-only first, then tool
   calls once the happy path is solid).
6. **Standalone binary + script file format** (`src/main.rs` glue, README usage).

Each milestone should land with its own test and a green default `cargo test`
(no `#[ignore]`, no network egress, no credentials).

## Integrating with smith (consumer side, tracked separately)

- **OpenAI/DeepSeek path:** no smith change. A test builds
  `ProviderConfig::new("deepseek", model, fake.base_url(), "test-key")` and calls
  `run_decision` / `run_coding_agent` directly (in-process), or passes
  `fake.base_url()` to a spawned smith binary via env (subprocess).
- **Anthropic / Codex paths:** need a small test-gated `base_url` override on
  smith's `ProviderConfig::{anthropic_oauth, chatgpt_oauth}` (they currently
  hardcode the base URL). File this against smith; not part of `jig`.

The same `jig` server serves both the in-process live-style tests and the
subprocess e2e tests unchanged — both reach it over HTTP at `base_url`.

## Non-goals (for now)

- Real token counting, real model behaviour, or prompt-faithful generation.
- Non-streaming responses (the SDK always streams).
- Auth/credential validation.
- Provider features smith does not exercise (image inputs, citations, batching).
- Multi-threaded throughput: the runtime is intentionally single-threaded; this
  is a low-traffic test double.

## References

- smith provider wiring: `smith/crates/smith-temper-agent/src/provider.rs`
- smith decision loop: `smith/crates/smith-temper-agent/src/decision.rs`
- smith coding agent (tool loop): `smith/crates/smith-temper-agent/src/coding_agent.rs`
- SDK base-URL normalization + SSE parsing: `pi_agent_rust 0.1.13`
  `src/providers/mod.rs`, `src/providers/{openai,anthropic,openai_responses}.rs`,
  `src/sse.rs`
- Existing (ignored, live) e2e for the shapes `jig` replaces:
  `smith/crates/smith-temper-agent-cli/tests/coding_agent_e2e.rs`,
  `smith/crates/smith-temper-agent/tests/{chatgpt,anthropic}_oauth_live.rs`
