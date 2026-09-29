# Pilot brief: convert jig's shell to a no-await, two-step sans-IO loop

This brief is self-contained. It is written for a fresh Claude Code session
working in `/home/free/src/rust/jig` (repo `ai/jig` on
`https://git.ekanayaka.io`). It explains why the pilot exists, the exact
programming model to reach, what jig looks like today, the relevant facts
about our runtime dependencies, the constraints, and what to hand back.

## 1. Why this pilot exists

The main project, **temper** (`/home/free/src/rust/temper`), is an
LLM-based software factory. Its agent tier is being refactored into a single
event loop with all business logic in pure step functions. An alignment review
(`/home/free/src/rust/temper/docs/plans/agent-single-loop/ALIGNMENT_REPORT.md`,
read it, especially the Addendum) found:

- **The core side is aligned.** Pure machines, owned-value inputs and outputs,
  ids not pointers, time and entropy as data, exactly one completion per
  operation.
- **The shell side is not.** temper's executor (`crates/temper-agent-exec`)
  runs on skein's async runtime: a task per operation, awaits in every
  primitive, a blocking thread pool, and HTTP/TLS/SSE hidden inside async
  dependencies. It needed 18+ waker-hygiene fixes, and a skein scheduler bug
  (ai/skein#3) caused a production hang.

The owner's decision is to move to a **clean reactor-based, no-await design
with two top-level sans-IO step functions**: one for business logic, one for
driving I/O. The one blocking call is a single reactor poll per iteration,
and there is no thread pool. The shell design is the uncertain part, so it is
**piloted here on jig first**. jig is small, its core is already pure, and its
shell exercises exactly the hardest part of temper's shell: TCP, HTTP/1, SSE
streaming, and an HTTPS client (TLS).

The pilot's output is a validated pattern, and ideally a small reusable crate,
that temper's "shell track" (SH0–SH7 in the report) will adopt.

## 2. The target programming model

Primary sources (read them): `/home/free/desired-rust-programming-model/`
- `explanation` is the Rust step-function architecture: ownership, the borrow
  checker, the annotated harness, and the house rules.
- `model/harness/src/{engine.rs,main.rs,slab.rs}` is the compiling example of
  a core, a shell and a generational slab.
- `vsr/docs/io-design.md` §1, §3 and §4, plus `vsr/include/vsr-io.h`, cover the
  C design this follows: an executor seam, a loop-less library, planners, and
  one blocking wait.

### The loop (one per thread; the only loop in the program)
```text
loop {
    events  = reactor.poll(timeout = min(core.deadline(), io.deadline()) - now)   // ONLY blocking call
    now     = clock()                                   // read once per iteration
    inputs  = io.reap(now, &events)                     // IO step: readiness + bytes → typed completions (pure)
    reqs    = core.step(now, inputs)                    // business step (pure)
    actions = io.step(now, reqs)                        // IO step: requests → planned non-blocking actions (pure)
    perform(actions)                                    // thin: non-blocking read/write/accept/connect, interest changes
}
```
- **Two step functions.** Both `core.step` and `io.step`/`io.reap` are pure:
  no syscalls, no clock reads, no randomness, no locks, no threads, no
  `async`/`.await`. They are testable by feeding events and asserting on the
  outputs.
- **`perform` is dumb.** It executes planned non-blocking syscalls and reports
  what happened (bytes written, `EAGAIN`, EOF, errors) back as the next
  iteration's I/O events. It makes no decisions. Keep it tiny.
- **Deadlines are state.** Each step exposes its earliest deadline, and the
  loop passes the minimum to `poll`. There are no timer tasks and no timer
  threads.
- **Cross-thread signals are reactor events.** `FakeLlm`'s `Drop` runs on
  another thread and must stop the loop; do it with `reactor.wake()` (or an
  eventfd or pipe), and never with shared-state polling.
- **Stepping to quiescence.** Loop `core.step` ↔ `io.step` until no new work
  appears before blocking, like VSR's `poll` → `prepare`.

### House rules (from the explanation, §9)
- The core and the I/O planners never do I/O, read a clock, spawn threads,
  sleep or log. Time arrives as `now`.
- Types crossing the step boundary contain no references and no lifetime
  parameters. They hold owned values (`String`, `Vec<u8>`, `bytes::Bytes`),
  `Copy` ids, or `Arc<T>` for immutable shared data. Buffers move in with a
  request and move back with its completion.
- Entities refer to each other by id: connection ids, request ids, generational
  slab handles. Never by stored references.
- Group state into sub-structs. Helpers take the sub-struct they touch
  (`&mut Conns`, `&mut Conn`), not `&mut self`.
- Never mutate a collection while iterating it: collect ids first, then act.
- Every started operation gets exactly one completion. Unknown or late
  completions are ignored, never unwrapped.
- No `Rc`, `RefCell`, `Mutex`, `async`, trait-object callbacks, or lifetime
  parameters in pure code. Prefer enums and `match` over `dyn` ("no vtables,
  no callbacks", from VSR). If one seems necessary, change the data flow
  instead.
- Every behaviour gets a test that drives the step functions with scripted
  events. No threads, sleeps or network in those tests.
- Arenas and no-malloc are out of scope: ordinary allocation is fine in Rust
  (explanation §8.5).

## 3. jig today (at `ca1edfd`)

| Crate | LOC | Role | Async? |
|---|---|---|---|
| `jig-core` | ~7.2k | Scripts, dialect parse/render (SSE ↔ canonical `Reply`), conformance | **Pure** (serde only) |
| `jig-server` | ~1.9k | `FakeLlm` embeddable fake provider. It binds `127.0.0.1:0` and spawns one OS thread with a single-threaded skein runtime; `serve` is the accept loop and `handle_connection` reads a request, routes it by path to a dialect, and writes SSE or an HTTP error | async (about 24 `.await`s, skein `TcpListener`/`TcpStream`, `Select` for shutdown) |
| `jig-record` | ~2.2k | Passthrough recorder. `pump.rs` accepts connections concurrently, one skein task each. `proxy.rs` forwards each routable request over **HTTPS** (`skein::tls::TlsConnector`, rustls underneath), streams the response back unbuffered and captures it. `redact.rs` and `fixture.rs` are pure | async (about 29 `.await`s) |
| `jig-runtime` | 166 | skein runtime bootstrap (`block_on` with a `Cx`) plus the `read_some` helper | async |
| `xtask` | ~2.2k | record, derive and staleness developer tasks | – |

Read `README.md`, `AGENTS.md`, `docs/explanation/repository-layout.md` and
`docs/explanation/record-and-conform.md` first.

## 4. Runtime facts you can build on (verified)

skein is our async runtime, a fork of asupersync, pinned at rev `50c7719`.
Source: `~/.cargo/git/checkouts/skein-*/50c7719/crates/skein/src`. You do NOT
need its async layer:
- **An await-free reactor.** `skein::runtime::reactor::Reactor`
  (`runtime/reactor/mod.rs`) offers `register(&dyn Source, Token, Interest)`,
  `modify`, `deregister`, `poll(&mut Events, Option<Duration>)` and `wake()`.
  Backends are `EpollReactor`, `IoUringReactor` and `KqueueReactor`, plus
  **`LabReactor`**, a deterministic simulation with `FaultConfig`. Use
  `create_reactor()` for the platform default.
- **Pure codecs.** The `codec::Decoder` trait; `http/h1/client.rs`
  `Http1ClientCodec` (`decode(&mut BytesMut) -> Option<Response>`); and
  `http/h1/codec.rs` `Http1Codec`. Check that they handle streamed and chunked
  bodies; if not, `httparse` plus a small body decoder is fine.
- **TLS.** skein's TLS is rustls (0.23 is already in the dependency graph)
  `ClientConnection`, which is sans-IO: `read_tls`, `write_tls`,
  `process_new_packets`, `reader()`, `writer()`. Drive it from the I/O step
  using byte buffers. rustls also has an explicit `UnbufferedClientConnection`
  API; pick one and justify it.
- **Alternative.** `mio` 1.x (in temper's lockfile) is a smaller, battle-tested
  reactor with the same shape but no lab simulation. **Decide between skein's
  reactor and mio, and justify it.** The criteria are simplicity, the quality
  of the simulation or test story, and the dependency surface.

Known skein issues: ai/skein#3 (stale-wake scheduler bug, fixed in ai/skein#4)
lives in the *async* scheduler, which the pilot won't use. There is also a
multi-worker reactor hang noted in skein#4 that doesn't affect single-threaded
reactor use.

## 5. Constraints

**Keep the public API temper uses stable.** temper pins jig at `ca1edfd` as a
git dev-dependency, so nothing breaks until temper bumps it, but the bump must
be a drop-in. temper imports:
- `jig_server::FakeLlm`: `start(Script) -> io::Result<FakeLlm>`, `base_url()`,
  `requests()`, and `Drop` that shuts down and joins its thread. It is used in
  about 41 places.
- `jig_core::{Reply, Script, Turn, StopReason, RequestView, ScriptFile, ScriptAction, HttpError, Dialect, ViewMessage, render::frames_to_body}`.
  jig-core is pure; leave it alone unless the conversion needs something.
- `jig_record::{bind, proxy_once, Route, ClientRequest, UpstreamResponse}` and
  `jig_runtime::block_on` (in one temper test). You may redesign these, but
  list every signature change so temper can adapt, or keep thin compatibility
  shims.

**Behaviour must be identical at the transport and framing level:** SSE
framing, status lines, headers, chunking, error responses and the 404
behaviour. The existing tests in `crates/jig-server/tests/*` and jig-record's
tests are the oracle and must pass unchanged. Add tests; don't weaken them.

**One OS thread per engine is fine.** `FakeLlm` owning one OS thread that runs
the loop is allowed ("one engine per thread, share nothing"). What's banned is
a thread pool, per-connection threads or tasks, and any await.

**Files and DNS.** jig only reads script and fixture files at start-up.
Synchronous file reads outside the loop are fine. The recorder's upstream DNS
may be resolved synchronously *before* connecting, as a documented exception,
or through a planner. Pick one and justify it.

**Repo process:**
- Follow jig's `AGENTS.md` and the Forgejo CI in `.forgejo/workflows/ci.yml`.
- Use branches and PRs on `ai/jig`. Push with
  `git -c credential.helper=/home/free/.local/state/agent-refactor/fj-cred-helper.sh push`.
  API creds are in `~/.pi/agent/secrets/forgejo-mcp.env`; never print them.
- End commits with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  End PR bodies with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
- **Do not bump jig in temper.** The temper session does that after the pilot.
- **The host is shared** with temper's builds and its only CI runner:
  - Run builds and tests with `nice -n 10` and `CARGO_BUILD_JOBS=4`.
  - The disk is tight: a kache disk guard runs, so don't leave large `target/`
    dirs around.
  - Use `/srv/...` for scratch, not `/tmp`: it's a small tmpfs, wiped on
    reboot.

## 6. Deliverables

1. **A small reusable loop crate** inside jig for now (suggested:
   `crates/jig-loop`, or a generic name such as `steploop`). It must not depend
   on jig-specific types, because temper will adopt it or copy it. Contents:
   - the loop driver: poll → reap → step to quiescence → prepare → perform;
   - a deadline heap;
   - the `IoStep` planner interface, and the typed action and event records
     `perform` consumes and produces;
   - a **simulated reactor/world** (`LabReactor`-based or your own) for
     deterministic tests of the I/O step.
2. **Planners (pure):**
   - TCP listener and accept;
   - TCP connection read and write, with partial writes, EAGAIN, EOF and
     backpressure;
   - HTTP/1 server: request parsing, plus response writing with streamed SSE;
   - TLS client (rustls);
   - HTTP/1 client, for the recorder's upstream;
   - the connection lifecycle and cancellation or shutdown.
3. **jig-server and jig-record rewritten on the loop,** with zero `async`,
   `.await` or `spawn` and no thread pool. `jig-runtime` is removed or reduced
   to a thin compatibility shim. `FakeLlm` keeps its API and behaviour.
4. **Tests.**
   - All existing tests pass unchanged.
   - New deterministic tests drive the I/O step and planners against the
     simulated world, including partial reads and writes, a slow client, a
     client disconnect mid-SSE, TLS handshake failure and shutdown during a
     stream.
   - A record/replay test of one full exchange.
5. **Write-up: `docs/explanation/sans-io-shell.md`.** It covers:
   - the final API shapes, with a short annotated example;
   - how ownership and buffers flow (moves in and out), and which borrow-checker
     issues came up and how they were solved;
   - the decisions: skein reactor vs mio; TLS API choice; HTTP codec choice;
     DNS; files; how `perform` stays thin;
   - what felt awkward or costly compared with async, and LOC before and after;
   - **concrete recommendations for temper's shell track.** Covering a TLS/HTTP
     client planner for model calls (temper uses tongs' providers; tongs has
     "Pure: request building" and "Pure: SSE folding" sections that can be
     exposed), a process planner (pidfd plus non-blocking pipes plus a
     TERM→KILL ladder), and file operations.

## 7. Suggested milestones (one PR each is fine)

- **M0 (design doc, no code).** Choose the reactor, sketch the `IoStep` and
  `perform` records, the id and slab scheme, and the test world. Ask the owner
  if a decision is truly theirs; otherwise decide and document it.
- **M1.** The loop crate, the deadline heap and the simulated world, proven on
  a toy echo server with deterministic tests.
- **M2.** TCP plus HTTP/1 server plus SSE planners. Port `jig-server` and keep
  `FakeLlm`'s API. The existing server tests pass.
- **M3.** TLS plus HTTP/1 client planners. Port `jig-record` (the concurrent
  accept plus upstream proxy). The recorder tests pass. Delete async
  `jig-runtime`.
- **M4.** Write-up, cleanup, and a LOC and complexity comparison.

## 8. Questions the pilot must answer for temper

1. Does the two-step shape stay simple as protocols stack (TCP → TLS → HTTP →
   SSE)? Or do planners need composition helpers (a pipeline of byte
   transformers)?
2. Where does backpressure live: the I/O step, or the core?
3. How are per-connection states identified and reclaimed: a generational slab,
   and what happens to stale events?
4. Is `perform` really thin, and how many lines is it?
5. How good is the simulated world as a test tool: fault injection, and
   determinism under replay?
6. skein reactor or mio for temper?
