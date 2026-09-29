# jig

## What jig is

jig is a fake LLM provider for end-to-end testing. It pretends to be OpenAI/DeepSeek, Anthropic, or Codex, so client code can run its full agent loop with no API keys, no network access and no token spend. Replies are scripted, not generated. What jig does guarantee is that the wire format is exact: SSE framing, event names and order, and the JSON shape. A real SDK parses its replies without noticing any difference.

## The pieces

**1. Serving scripted replies** (`jig-server`, `jig-core`)
- It has one route per provider format: `/chat/completions` (OpenAI and DeepSeek), `/v1/messages` (Anthropic) and `/backend-api/codex/responses` (Codex). You point a client's `base_url` at jig, and that is the only change needed.
- **Scripts** decide what jig replies:
  - `fixed`: the same reply every time.
  - `sequence`: replies in order.
  - `phases`: picks a reply by matching the request, for example on message text, model or format.
  - `reference_delivery`: a built-in rule for Temper's demo.
  - In Rust you can also write a custom `rule` closure.
- A **reply** is made of text, thinking and tool-call turns, plus optional usage figures and a stop reason. A script can also return **HTTP errors** shaped the way each provider formats them.
- You can use it three ways:
  - **In-process:** `FakeLlm::start(script)` gives you a URL and the list of requests it received.
  - **As a binary:** `jig script.json`.
  - **With no sockets at all:** `serve_request(&mut Provider, …)` for tests that don't need I/O.

**2. Recording real traffic** (`jig-record`, `jig record`)
- A passthrough proxy sits between an official client (Claude Code, Codex, the OpenAI/DeepSeek SDK) and the real backend.
- It captures each exchange as a fixture, removes secrets at capture time, and tags it with the client and a role. `authoritative` means an official client; `subject` means an SDK being tested.

**3. Checking jig against real providers** (`jig-core::conform`, `fixtures/`)
- Recordings are reduced to **masked structural templates**. Values that change between runs, such as ids, timestamps, token counts and where text chunks split, are masked out. The structure is kept.
- Offline `cargo test` checks that jig's rendered output matches those templates (T1) and that recorded requests match the request template (T2).
- Other SDKs, such as tongs, can check their own requests against jig's templates using the same tools.

**4. Fixture upkeep** (`xtask`)
- `xtask record` re-captures the scenarios for each provider: single text, tool call, tool result then final answer, thinking, and parallel tool calls. It then re-derives the templates.
- `xtask staleness` flags recordings older than 90 days by default, as a reminder to refresh them.

**5. The loop underneath** (`steploop`)
- `steploop` is a generic single-threaded loop that never awaits and has no async runtime. The logic and I/O planning are pure functions, and a small reactor performs the actual system calls.
- The server and the recorder are built the same way: a pure core, a pure I/O step, a host, and a handle that runs all three on one OS thread.
- Tap/replay can re-run any real session deterministically, which makes failures reproducible.

## How it fits together

```
 official client ──► jig record ──► real backend      (online, manual, needs credentials)
                         │
                         ▼
         fixtures/<dialect>/<scenario>/   recordings → masked templates   (xtask record / derive)
                         │
                         ▼
   cargo test: jig's rendered output == templates     (offline conformance)
                         │
                         ▼
 your code / SDK ──► FakeLlm (script) ──► faithful SSE replies   (offline end-to-end tests)
```

In short, the recording side shows what the real providers send, the conformance tests prove jig sends the same format, and the server lets you script conversations for your own tests. The recording side needs credentials and network; the other two run offline.

## Documentation

Reference:

- [The fake LLM server](docs/reference/server.md): routes, HTTP behaviour, the in-process `FakeLlm` and `serve_request` APIs, and running the `jig` binary.
- [Script file format](docs/reference/script-file.md): `fixed`, `sequence`, `phases`, `reference_delivery`, replies and turns, HTTP errors, and rule closures.
- [The recorder](docs/reference/recorder.md): `jig record`, the `Recorder` API and its once/pump modes. The capture workflow and `jig record` flags are in [`crates/jig-record/README.md`](crates/jig-record/README.md).

How-to:

- [Refresh the recorded fixtures](docs/how-to/refresh-fixtures.md)

Explanation:

- [Record and conform](docs/explanation/record-and-conform.md): why fixtures come from real traffic, masking, and the conformance tests.
- [The sans-IO shell](docs/explanation/sans-io-shell.md): the no-await loop, its design decisions, and the results.
- [Repository layout](docs/explanation/repository-layout.md): the crates and what each one holds.
