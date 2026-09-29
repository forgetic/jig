# The sans-IO shell: design and rationale

Status: **M0 design**, 2026-09-29. It is written before any code, and a
results section ([§11](#11-results-m4)) is added at M4. This document is the
reference for porting temper's shell to the same model later. It records the
decisions and also the reasoning and alternatives behind them, so they can be
re-examined when circumstances change.

Brief: [docs/plans/sans-io-pilot-brief.md](../plans/sans-io-pilot-brief.md).

## Contents

1. [Summary](#1-summary)
2. [Context: why this pilot exists](#2-context-why-this-pilot-exists)
3. [The target programming model](#3-the-target-programming-model)
4. [Decisions](#4-decisions)
5. [Architecture](#5-architecture)
6. [Behaviour changes](#6-behaviour-changes-relative-to-ca1edfd)
7. [Testing strategy](#7-testing-strategy)
8. [Using jig from temper without I/O](#8-using-jig-from-temper-without-io)
9. [Milestones and work split](#9-milestones-and-work-split)
10. [Questions for temper, and how the pilot answers them](#10-questions-for-temper-and-how-the-pilot-answers-them)
11. [Results (M4)](#11-results-m4)
12. [Sources](#12-sources)

## 1. Summary

jig's shell (`jig-server`, `jig-record`'s proxy and pump, `jig-runtime`) is
rewritten from async/await on skein into a single-threaded loop that never
awaits:

```text
loop {
    reactor.poll(events, timeout = min(core.deadline(), io.deadline()) - now)  // the only blocking call
    now = clock()                                                                // read once per iteration
    io.reap(now, events)  → completions                                          // pure
    repeat until quiet: core.step(now, completions) → requests                   // pure business logic
                        host.handle(host requests)  → completions                // embedder: logs, rule closures
                        io.step(now, requests)      → actions                    // pure I/O planning
    reactor.perform(actions) → events for the next iteration                     // thin, one event per action
}
```

The main decisions:

- **The reactor is a small wrapper we own, over the `polling` crate** (smol-rs;
  it is the same crate skein's `EpollReactor` wraps). We use oneshot readiness,
  so arming interest is a request with exactly one completion. We don't use mio,
  and we don't use skein's reactor as it stands. The wrapper is meant to move
  into skein once temper adopts the model. See [§4.1](#41-the-reactor-an-owned-wrapper-over-polling).
- **There is no byte-level simulated world.** Deterministic testing happens at
  two levels. Business logic is tested with scripted completions, and later
  with seeded vocabulary-level fault injection. Each I/O planner is tested
  directly with scripted events. The thin real-I/O remainder is tested over
  real loopback sockets. Any real run can be replayed deterministically from a
  tap of the pure steps' inputs. See [§4.2](#42-the-testing-layer-no-byte-level-simulated-world).
- **Layered reuse.** jig-core and a pure *provider core* at the HTTP-message
  level can be used from temper tests with no I/O at all. See [§8](#8-using-jig-from-temper-without-io).
- **Backward compatibility is not a goal.** The owner prefers adherence to the
  model over API stability, and temper will be adapted.
- **skein and `jig-runtime` leave jig's dependency tree.**

## 2. Context: why this pilot exists

**temper** (`~/src/rust/temper`) is an LLM-based software factory. Its agent
tier is being refactored into a single event loop with all business logic in
pure step functions. The alignment review
(`~/src/rust/temper/docs/plans/agent-single-loop/ALIGNMENT_REPORT.md`,
especially its Addendum) found:

- **The core side is aligned.** It has pure machines, owned-value inputs and
  outputs, ids instead of pointers, time and entropy as data, and exactly one
  completion per operation.
- **The shell side is not.** temper's executor runs on skein's async runtime
  with a task per operation, awaits in every primitive and a blocking thread
  pool, and HTTP/TLS/SSE are hidden inside async dependencies.
  - It needed 18+ waker-hygiene fixes.
  - A scheduler bug (ai/skein#3, a stale task id) caused a production hang.
  - About 40% of `temper-agent-exec` exists only because of async: queue
    wakers, runtime bootstrap, guards.

The owner decided on a reactor-based design with no await and two top-level
sans-IO step functions. The shell design is the uncertain part, so it is
piloted on jig first. jig is small, its core (`jig-core`) is already pure, and
its shell exercises the hardest part of temper's shell: TCP, HTTP/1, SSE
streaming and an HTTPS client.

### What the C port taught us

A C11 port of jig (`~/src/c/jig`, see its `docs/comparison.md`) was built with
the target model: a raw io_uring loop around a pure core with no allocation. It
matched Rust byte for byte. What carries over:

- **Wins.**
  - The core is deterministic and testable without sockets: fake kernels, and a
    STOP injected at 720 points.
  - Serving is concurrent, so an idle client no longer stalls the server.
  - Cancellation is explicit.
  - It was 1.3–3× faster with 1.7–3× less CPU, from less work per request.
- **Costs to avoid.**
  - The event-driven code was **4.4× the async code**. The server's
    connection state machine was 713 lines against about 50 async lines; the
    recorder had about 20 phases in 1,358 lines against about 340.
  - The "one completion per op" guarantee rested on flags spread across three
    modules (`rec.c`, `rec_loop.c` and the TLS session) and was checked only
    by asserts.
  - Connection-slot state was spread across five fields.
- **Costs that came from the C brief, not from the model:** no allocation,
  everything through io_uring (which forced its own DNS resolver and process
  spawning), and byte-exact serde behaviour. Rust does not inherit these.

The Rust design therefore uses:

- ordinary allocation;
- one enum per connection state, with the state's data inside its variant;
- one owner for each invariant;
- small byte-stage planners composed in a fixed order.

Size is tracked and reported at M4.

### What is being replaced (at `ca1edfd`)

| Crate / file | Lines (wc) | Role |
|---|---:|---|
| `jig-server/src/lib.rs` | 131 | `FakeLlm`: thread + skein runtime + `Notify` shutdown |
| `jig-server/src/server.rs` | 363 | accept loop, request reader, SSE/error/404 writers |
| `jig-record/src/proxy.rs` | 410 | request reader, HTTPS forward, streamed capture (≈75 of it tests) |
| `jig-record/src/pump.rs` | 158 | concurrent accept, one task per connection |
| `jig-record/src/lib.rs` (async part) | ≈60 | `record_once`, `record_once_blocking` |
| `jig-runtime` | 185 | `block_on` with a `Cx`, `read_some` |

## 3. The target programming model

Primary sources are `~/desired-rust-programming-model/`:
- `explanation`: the Rust step-function architecture, ownership, the borrow
  checker, and the house rules.
- `model/harness/src/{engine,main,slab}.rs`: a compiling example.
- `vsr/docs/io-design.md` §1, §3 and §4, and `vsr/include/vsr-io.h`: the C
  design this follows (executor seam, a library with no loop of its own,
  planners, one blocking wait).

### 3.1 The loop

- **One loop per thread, and it is the only loop in the program.** The one
  blocking call is `reactor.poll`. There is no thread pool, no per-connection
  thread or task, and no `async`/`.await`.
- **Two pure step functions:** the business *core* and the *I/O step*
  (`reap` and `step`). Neither makes syscalls, reads a clock, draws
  randomness, takes locks or spawns threads. They are tested by feeding inputs
  and asserting on outputs.
- **`perform` does what it's told.** It executes planned non-blocking syscalls
  and reports what happened. It makes no decisions.
- **Deadlines are state.** Each step exposes its earliest deadline, and the
  loop passes the minimum to `poll`. There are no timer tasks and no timer
  threads.
- **Cross-thread signals are reactor events**, never shared state that the
  loop polls.
- **The loop steps to quiescence.** The core and the I/O step alternate until
  no new completions appear. Only then does the loop perform and block, as
  VSR's `poll` → `prepare` does.

### 3.2 House rules

From the explanation, §9. These apply to the core and to every I/O planner:

1. The core and the planners never do I/O, read a clock, spawn threads, sleep
   or log. Time arrives as `now`.
2. Types crossing a step boundary contain no references and no lifetime
   parameters. They hold owned values (`String`, `Vec<u8>`), `Copy` ids, or
   `Arc<T>` for immutable shared data. Buffers move in with a request and move
   back with its completion.
3. Entities refer to each other by id, never by stored references.
4. State is grouped into sub-structs. Helpers take the sub-struct they touch
   (`&mut Conns`, `&mut Conn`), not `&mut self`.
5. Never mutate a collection while iterating over it: collect ids first, then
   act.
6. Every started operation gets exactly one completion. Unknown or late
   completions are ignored, never unwrapped.
7. No `Rc`, `RefCell`, `Mutex`, `async`, trait-object callbacks or lifetime
   parameters in pure code. Prefer enums and `match` over `dyn` ("no vtables,
   no callbacks", from VSR). If one seems necessary, change the data flow
   instead.
8. Every behaviour gets a test that drives the step functions with scripted
   inputs. Those tests use no threads, sleeps or network.
9. Arenas and no-malloc are out of scope. Ordinary allocation is fine
   (explanation §8.5).

Additions made by this design:

10. **Deterministic iteration.** Pure state that is iterated uses `BTreeMap` or
    `Vec`, never `HashMap`. Replay must reproduce outputs exactly, and
    `HashMap` iteration order is randomized.
11. **Steps drain their input vectors and append to their output vectors.**
    Vectors are owned by the loop and reused across iterations, allocated
    once.
12. **No `unwrap()` in step functions**, except directly after a check that
    makes it infallible.

## 4. Decisions

### 4.1 The reactor: an owned wrapper over `polling`

The owner asked this to be examined in depth, since skein is ours and fully
under our control. Research is summarized below; the agent reports are
preserved in this section.

#### Why skein was adopted in the first place

- temper moved from tokio to asupersync 0.3.1 on 2026-06-11 (temper commit
  `087ff969`). The reasons are recorded only in a retrospective written after
  the switch (`git show 11c53ef1^:asupersync-followup.md` in temper):
  - one crate replaced hyper, axum, tower, reqwest and url/idna/icu (the old
    lockfile no longer built on the toolchain);
  - alignment with the pi-sdk/smith stack;
  - a clock that can be swapped at runtime;
  - an *option* on `LabRuntime` for simulating the whole shell: "The option is
    what retroactively justifies the runtime choice."
- The same retrospective concedes: "the reasoning improvements in the codebase
  (determinism, replayable machines, race-free-by-construction daemon core)
  came from the sans-IO machine discipline, not from the runtime. The same
  architecture over tokio would have delivered most of it."
- skein was forked from asupersync on 2026-06-12 for its license (a rider from
  0.2.5 onward) and to stay on the stable toolchain (0.3.3+ needs nightly).
- jig moved from tokio to skein on 2026-06-13 (PR #37) "so no tokio remains
  anywhere in the dependency tree".
- **mio was never evaluated.** It first appears in documents from 2026-09-28/29.

Every stated reason concerns the **async layer**: `Cx`, obligations,
`LabRuntime`, the swappable clock, and a single async I/O crate. A design with
no await uses none of it. Time becomes plain data. temper uses `LabRuntime` 25
times and `LabReactor` zero times.

#### skein's reactor on its own merits

At `50c7719`; nothing in `runtime/reactor/` has changed since June.

- **Shape.** `Reactor` is a `Send + Sync` trait with `&self` methods:
  `register(&dyn Source, Token, Interest)`, `modify`, `deregister`, `poll`,
  `wake`. `create_reactor()` returns an `Arc<dyn Reactor>`. A `Source` is any
  `AsRawFd`, so plain `std::net` sockets and a pidfd work.
- **Backends.** `EpollReactor` is a thin wrapper over the `polling` crate and
  is oneshot by default. There are also `KqueueReactor`, `IoUringReactor`
  (readiness through re-armed `PollAdd`s) and `LabReactor`.
- **Gaps for this design:**

| Gap | Relevance | Fix size |
|---|---|---|
| The reactor is not feature-gated, so depending on it compiles all of skein: 69 crates, ≈41 s clean (mio: 3 crates, ≈2 s) | Both jig and temper, until skein is pruned | Medium–large to extract |
| No non-blocking connect, no pipe types, no non-async net types | The recorder's upstream (jig); child pipes and clients (temper) | Medium |
| `Events::default()` has capacity 0 and silently drops every event; with oneshot the registration is lost for good (`mod.rs:290-294`) | Both | Small |
| Its own fd→token map requires deregistering before closing, or a reused fd fails with `AlreadyExists` (`epoll.rs:228-233`) | Both | Small |
| io_uring backend: `poll` returns 0 immediately when nothing is registered, so a loop spins; it holds the ring mutex across the wait; it ignores `EDGE_TRIGGERED`. kqueue is always oneshot. The backends disagree on triggering | Only if io_uring or macOS is used | Small to document, medium to fix |
| `LabReactor` simulates readiness only. `register` ignores the source; faults surface only as an errored event and never reach a `read()`. It cannot produce partial reads or writes, EAGAIN, slow peers or a mid-stream disconnect, and it cannot drive code that makes real syscalls | Testing (see [§4.2](#42-the-testing-layer-no-byte-level-simulated-world)) | Large |
| `Arc<dyn Reactor>` and `&dyn Source` | House rule 7 prefers enums and static dispatch | Small |

- **Quality.** 151 reactor unit tests pass. CI runs only Linux and never
  enables io_uring. The known multi-worker hang (ai/skein#5) comes from the
  runtime holding its I/O driver lock across the blocking poll, not from
  `EpollReactor`. It does not affect a single-threaded loop.

#### mio

mio is tokio's foundation and very widely used. It is always edge-triggered,
with a `Registry` and a separate `Waker`. It has non-blocking
`TcpStream::connect`, `unix::pipe` (from `ChildStdout`), and `SourceFd` for any
fd. It has no simulation.

#### The decisive point: owning the thin layer

The owner pointed out that once temper is fully ported, skein's async layers
can be deleted and only the reactor remains, so the build-weight argument is
temporary. That is correct. With build weight set aside, the choice is
**"own a thin wrapper over `polling`" versus "depend on mio"**, and the owned
wrapper wins:

1. **Oneshot readiness fits the model.** Arming read interest yields exactly
   one readiness event and must then be re-armed. So "arm" is an operation
   with exactly one completion, the same shape as an io_uring `POLL_ADD`.
   mio's edge-triggered readiness is sticky state that a planner would have to
   mirror, draining until EAGAIN.
2. **Control is real, and cheap here.** The kernel-facing code is third-party
   either way (`polling` or mio). What we own is the wrapper, and that is where
   the model's choices live: ids, resource ownership, one completion per
   action, and signals as events.
3. **The wrapper owns the fds.** `polling` 3's `Poller::add` is `unsafe`
   because a source must be deleted before its fd is closed. Our wrapper holds
   every fd, so it always deletes before dropping. The one `unsafe` call is
   sound by construction and contained in one place. That also rules out
   skein's fd-reuse hazard.
4. **Dependencies are minimal.** `polling` pulls in only `cfg-if` and `rustix`.
   `socket2` provides non-blocking connect.
5. **The path into skein.** When temper adopts this model, the loop crate
   (wrapper, loop driver and planners) moves into skein as its core, and the
   async layers are deleted around it. skein then becomes the owned toolkit of
   reactor, loop and sans-IO codecs. Its sans-IO h1 codec and h2
   implementation are assets there; the skein prune notes of June 2026 already
   call the h2 code "genuinely sans-IO … scarce". The owner's control goal is
   met from the other direction.

**Not chosen:**

- **skein's reactor as it stands:** the build weight until the prune, the dyn
  plumbing, the sharp edges, and a lab reactor that would be dead weight.
- **"skein after fixes":** extracting the reactor and adding net and pipe
  types amounts to rebuilding mio inside skein, and it still leaves the
  simulation to us.
- **mio:** its edge-triggered semantics fit the model less well. It remains a
  one-file swap behind the wrapper if `polling` ever disappoints.

### 4.2 The testing layer: no byte-level simulated world

The brief asked for "a simulated reactor/world for deterministic tests of the
I/O step". After discussion with the owner, **we do not build a byte-level
simulated world** (virtual sockets, a network model, fault injection on
bytes). This matches the conclusion of the June skein analysis (temper memory
`skein-prune-map`): the lab "simulates at *readiness* level, not byte level",
and "the right adoption shape for temper [is] … fake executors at the executor
boundary".

The reasoning, by where bugs live:

- **Business logic and how completions interleave.** Retries, timeouts,
  cancellation, streams cut short, dispatch churn and the races temper's git
  history fought all live here. This is the right level for FoundationDB-style
  seeded exploration: a fake executor at the *vocabulary* boundary (temper's
  `IoRequest`/`IoCompletion`) that reorders, delays, fails and truncates
  completions from a seed. jig's provider core ([§8](#8-using-jig-from-temper-without-io))
  becomes the model responder inside it.
- **Transport code.** In this design it is already made of pure planners, so
  it is tested by feeding scripted events directly:
  - bytes split at every offset;
  - short writes, `WouldBlock`, EOF mid-head and mid-body, errors;
  - readiness arriving late.

  A simulated world would mostly re-test the codecs indirectly and at much
  greater cost.
- **The thin real-I/O remainder:** `perform` plus real kernel semantics
  (oneshot re-arm, hang-ups, `SO_ERROR` after connect, RST on close). A fake
  here would encode our own assumptions about epoll. Real loopback sockets
  test the real semantics cheaply and nearly deterministically: a slow reader
  (small `SO_RCVBUF`, not reading), a disconnect mid-SSE, shutdown mid-stream,
  and a local TLS upstream with a bad certificate. Beyond that, the owner's
  end-to-end chaos testing with real processes, machines and connections
  covers the rest.
- **Reproducibility is the one thing real-I/O testing lacks, and replay
  provides it.** Both steps are pure, so a *tap* that records `(now, inputs)`
  during any real run lets a failure be replayed deterministically at the
  layer where it happened. That includes runs under chaos. temper already has
  this for its core (`LoopTap`); the loop crate provides it for the I/O step
  ([§5.8](#58-tap-and-replay)).
- **Where a byte-level world does pay off:** distributed protocols whose
  product is correctness under partitions and crashes, such as VSR. That is
  not jig and not temper's shell.

The consequence for the reactor choice: `LabReactor`, the one feature skein
had that mio lacks, is not needed. It would have tested at the wrong level.

### 4.3 Ids: never-reused `u64`s, allocated by the pure side

- Every OS resource the I/O step uses (listener, connection, upstream
  connection) is named by a `SockId(u64)`. The **pure I/O step allocates the
  ids** from a counter it owns. The reactor's table maps `SockId` to the owned
  fd. So the shell never invents a number the pure side has to learn, and
  replay needs only the recorded events.
- Ids are **never reused**; a `u64` counter does not wrap in practice. A late
  or unknown event (for an id already forgotten) misses its lookup and is
  ignored (house rule 6). No generation counter is needed. temper's `IoId`
  made the same choice.
- Pure state is keyed by `BTreeMap<SockId, Conn>` for deterministic iteration
  (rule 10). A generational slab (`model/harness/src/slab.rs`) would be an
  optimization, not needed at jig's scale.
- Higher-level ids (`ReqId` for an HTTP exchange, `FetchId` for an upstream
  request) are newtypes allocated the same way by whichever step starts the
  operation.

### 4.4 Time

- `Time(u64)` is monotonic nanoseconds since the loop started. It is `Copy`,
  `Ord`, and plain data. `Duration` is `std::time::Duration`.
- The loop reads `Instant::now()` once per iteration, after `poll` returns,
  and converts it. The poll timeout uses the previous iteration's `now`, which
  is stale only by the time `perform` took, a few microseconds.
- Wall-clock time (the recorder's capture date) is supplied by the binary at
  startup, as today.

### 4.5 TLS: rustls `ClientConnection` (buffered API), ring provider

- rustls 0.23 is already in the dependency graph. Its **buffered
  `ClientConnection`** is sans-IO: `read_tls(&mut &[u8])`,
  `process_new_packets()`, `reader()`, `writer()`,
  `write_tls(&mut Vec<u8>)`. It is driven as a byte stage between the TCP
  buffers and the HTTP planner ([§5.5](#55-planners-byte-stages-composed-in-a-fixed-order)).
- `UnbufferedClientConnection` was rejected. It saves copies but pushes buffer
  management and a larger state machine (`ReadTraffic`, `EncodeTlsData`,
  `TransmitTlsData`, …) onto us, and the house rules allow allocation.
  rustls's own mio examples use the buffered API.
- **Crypto provider:** `ring`, already in the lockfile. **Trust:**
  `webpki-roots` by default, as today (`with_webpki_roots()`). The recorder
  config can replace the root store for tests.
- **Documented exceptions to purity inside rustls:**
  - **Entropy.** rustls draws randomness (client random, key shares) from the
    OS inside the planner.
  - **Wall clock.** It reads the wall clock to check certificate validity.

  So a TLS stage is deterministic in its *control flow* but not in its bytes,
  and a tap replay of a TLS connection is not byte-exact. The path to full
  determinism is known and deferred: a custom `CryptoProvider` with a seeded
  RNG plus custom key-exchange groups, and a `TimeProvider` fed from the loop.
  temper's replay matters at the vocabulary boundary, where this does not
  apply.

### 4.6 HTTP codec: `httparse` plus our own framing

- **Heads** (the server's request heads and the client's response heads) are
  parsed with `httparse`. It is tiny, has no dependencies, is battle-tested
  (hyper uses it), keeps header order and case, and is incremental
  (`Status::Partial`).
- skein's `Http1ClientCodec` was rejected because it would keep skein in the
  tree.
- **Bodies and framing** are small pure functions of our own:
  - Content-Length;
  - the chunked encoder (for jig's SSE responses);
  - a chunked decoder with split-point tests, used by temper's future model
    client rather than by jig;
  - read-until-EOF.
- **Strictness changes** ([§6](#6-behaviour-changes-relative-to-ca1edfd)):
  - A malformed head gets `400`.
  - A request `Transfer-Encoding` gets `501`. Today it is silently misread as
    an empty body.
  - An oversized head or body gets `413`.

  For well-formed requests, parsing must give exactly today's results: method,
  path with the query stripped for routing, header names and values trimmed
  as today, body by the last `Content-Length`.
- **Responses are written verbatim.** The core supplies the status, reason
  phrase and an ordered header list, including the framing headers. The
  planner writes the head exactly as given and frames the body as instructed.
  That keeps jig's bytes identical (header order included) without the
  planner guessing.

### 4.7 DNS: a synchronous `Resolve` action, a documented exception

- The recorder resolves its upstream host through `Action::Resolve`, which
  `perform` executes with `ToSocketAddrs`, i.e. `getaddrinfo`. That call
  **blocks the loop thread** for the duration of the lookup.
- Accepted for jig because the recorder is a manual tool with a handful of
  connections, and resolution happens once per upstream exchange.
- Connecting tries each resolved address in order, as std's `connect` does.
- **Not recommended for temper as it stands.** Options for temper:
  - resolve at startup;
  - a resolver thread whose results arrive as events (a signal plus a queue;
    not a thread pool, but it is a thread);
  - a sans-IO stub resolver, like the C port's.

  The action/event shape stays the same in all three, so switching later
  changes only `perform`.
- The server needs no DNS: it binds `127.0.0.1:0`.

### 4.8 Files

jig reads script and fixture files only at startup. Recordings are written
after a capture completes. That happens synchronously on the caller's thread
in the handle's `stop`/`wait`, or on the loop thread as a host action after
the loop has finished. Files never go through the loop.

For temper: small synchronous file operations are fine inline, with large
walks and greps chunked across iterations (the Addendum's recommendation).

### 4.9 Cross-thread signals, and the request log

- **Signals.** A `Signal` is a non-blocking `UnixStream::pair()`. The handle
  keeps a `SignalSender` (`Send`, cloneable) that writes one byte. The read
  end is a reactor resource the loop owns. `perform` keeps it armed and
  drains it, and each wake is delivered as `Event::Signal { id }`. No shared
  flag is polled, and no libc eventfd is needed. `FakeLlm::drop` and
  `Recorder::stop` use it.
- **The request log.** The provider core emits `HostReq::Record(RecordedRequest)`.
  `FakeLlm`'s host appends it to an `Arc<Mutex<Vec<_>>>` that `requests()`
  reads. That is the one lock in the design, and it sits at the thread
  boundary in shell code, never in pure code. The alternative, a query
  message answered by the loop, would block the caller on the loop and add
  a round trip for no benefit.

### 4.10 Scripts become data; rule closures are answered by the embedder

`Script` today violates rule 7 twice:

- sequence cursors are `Mutex<usize>` behind `&self`;
- `Script::Rule` and `ActionRule` hold `Box<dyn Fn>`, as do the lowered forms
  of script-file phases and reference delivery.

We change the data flow instead:

- **Script files and reference delivery become data variants**, with plain
  cursors and `&mut self`:
  `Script::{Fixed(ScriptAction), Sequence(..), Phases(..), ReferenceDelivery(..), Rule(Rule)}`.
- **Rule closures never enter pure code.** `Script::split(self) -> (Plan, Option<Rule>)`:
  - `Plan` is pure data. A rule script yields `Plan::External`.
  - The closure (`Rule(Box<dyn FnMut(&RequestView) -> ScriptAction + Send>)`)
    stays with the embedder.
  - The provider core asks with `HostReq::Decide { req, view }` and receives
    `Comp::Decision { req, action }` in the same iteration.
- **Who answers `Decide`:**
  - `FakeLlm`'s host calls the closure on the loop thread, as today.
  - A direct or in-process test answers inline.
  - Closures may now be `FnMut` without `Sync`. temper's `Fn + Send + Sync`
    closures still fit, and state can live in the closure instead of in
    `AtomicUsize` counters.
- **Kept:** the constructors (`rule`, `action_rule`, `sequence`,
  `action_sequence`, `fixed_action`). `next_reply` is removed. `next_action`
  becomes `&mut self` and remains a convenience for direct users; it calls
  the rule itself.
- **As built (wave 1):** `Script::Fixed` keeps holding a `Reply`, beside
  `FixedAction(ScriptAction)`, because the integration tests (and temper)
  construct `Script::Fixed(reply)` and must pass unchanged. Reply and action
  sequences share one `Sequence` variant, and reply and action rules one
  `Rule` variant. `ReferenceDelivery` holds its `ReferenceDeliverySpec`, and
  `Phases` holds one `Sequence` per phase. The full shape is
  `Script::{Fixed(Reply), FixedAction(..), Sequence(..), Phases(..), ReferenceDelivery(..), Rule(..)}`,
  with `Plan::{Fixed(ScriptAction), Sequence(..), Phases(..), ReferenceDelivery(..), External}`.

### 4.11 Static traits for the loop, and why not `dyn`

The loop crate defines three **statically dispatched** traits: `Core`,
`IoStep` and `Host`, with associated types ([§5.3](#53-the-loop-driver-and-its-traits)).

- **Why traits at all:** they are what lets one generic `run` drive any
  core/I/O pair, jig's today and temper's later.
- **Why this is allowed:**
  - Nothing is stored behind a vtable.
  - No callback is passed into pure code.
  - `Host` is the one place impure code (logs, locks, rule closures) meets
    the loop. It is shell code by definition, the Rust counterpart of VSR's
    `vsr_io_run` hooks, "the only callbacks in the header".

### 4.12 Concurrency and shutdown

- **Concurrency.** Both servers serve connections concurrently. The owner
  approved this, and external code that depended on one connection at a time
  will be changed. Script cursors advance in the order requests *complete*. A
  single client making sequential requests sees no difference.
- **Stop.** `FakeLlm::drop` raises the stop signal. The core turns it into
  `Shutdown { grace }` (the core decides, the I/O step executes). Then:
  1. The listener is closed.
  2. Connections that have not produced a request are closed.
  3. Responses being written get `grace` (default 1 s) to finish, then are
     closed.
  4. The loop exits when the core is done and the I/O step is idle.
  5. `drop` joins the thread.
- **Contrast with today.** A stalled client used to block `drop` forever.
  Now it is bounded by `grace`.
- **Recorder.** In once mode it shuts itself down after the first capture. In
  pump mode it runs until `stop()`.

### 4.13 Backward compatibility

Not a goal. The owner prefers adherence to the model, and temper will be
adapted. [§6](#6-behaviour-changes-relative-to-ca1edfd) lists every API change
so temper's bump is mechanical.

## 5. Architecture

### 5.1 Layers

| Layer | What | Pure? | Used without real I/O by |
|---|---|---|---|
| L0 `jig-core` | Script (data), Reply, renderers, SSE parsers, request views, conformance | yes | temper today (`late_stream_jig.rs`, `coding_request_oracle.rs`) |
| L1 provider core (`jig_server::provider`) | HTTP request message in → response (status, headers, SSE body) + request records + rule decisions out. Owns the `Plan` | yes | temper model-level tests ([§8](#8-using-jig-from-temper-without-io)) |
| L2 I/O planners (`steploop::{tcp, tls, http1}`) | listener/accept, connection byte pipe, HTTP/1 server and client, TLS client stage | yes | their own scripted-event tests |
| L3 loop + reactor (`steploop::{run, reactor}`) | the only loop, `perform`, signals, tap | no (the shell) | — |
| L4 handles (`FakeLlm`, `Recorder`) | L1 + L2 + L3 on one OS thread | no | real-socket and separate-process tests |

### 5.2 Crate map after the rework

```text
crates/
├── steploop/        NEW, generic (no jig types); destined for skein
│   ├── time.rs      Time, Deadlines<K> (min-heap with lazy deletion)
│   ├── sys.rs       SockId, Action, Event, Interest, Readiness, IoError, SignalId
│   ├── reactor.rs   Reactor: owns fds + polling::Poller; poll(); perform(); signals
│   ├── run.rs       Core, IoStep, Host traits; run(); Tap
│   ├── tcp.rs       Listener planner, Conn byte pipe (reads, writes, arm, backpressure, close)
│   ├── tls.rs       TLS client stage over rustls ClientConnection (feature "tls")
│   └── http1/       codec (httparse heads, framing, chunked), server planner, client planner
├── jig-core/        L0 (Script becomes data; see §4.10)
├── jig-server/      provider (L1, pure), io (server planner config), FakeLlm (L4)
├── jig-record/      pure: route, redact, fixture, upstream head; recorder core (pure); Recorder (L4)
└── xtask/           unchanged apart from the recorder API
(jig-runtime deleted; skein removed from the workspace)
```

The dependencies added are `polling` 3, `socket2` 0.6, `httparse` 1,
`rustls` 0.23 (ring), `rustls-pki-types` and `webpki-roots` 1, with `rcgen`
as a dev-dependency. `steploop` declares `rust-version = "1.85"` (temper's
MSRV), so clippy's `incompatible_msrv` lint catches newer std APIs.

### 5.3 The loop driver and its traits

```rust
/// Business logic. Pure.
pub trait Core {
    type Comp;    // completions and events from the I/O step (and host answers)
    type IoReq;   // requests to the I/O step
    type HostReq; // requests to the embedder: records, captures, decisions
    /// Drain `comps`; append to `io` and `host`. May be called with `comps` empty
    /// (time passed): expire deadlines here.
    fn step(&mut self, now: Time, comps: &mut Vec<Self::Comp>,
            io: &mut Vec<Self::IoReq>, host: &mut Vec<Self::HostReq>);
    fn deadline(&self) -> Option<Time>;
    fn done(&self) -> bool;
}

/// I/O planning. Pure.
pub trait IoStep {
    type Comp;
    type Req;
    /// Drain reactor events; update planner state; append completions for the core.
    fn reap(&mut self, now: Time, events: &mut Vec<Event>, comps: &mut Vec<Self::Comp>);
    /// Drain core requests; plan actions for every resource that needs progress.
    /// Completions that need no syscall (e.g. a request for an unknown id) go to `comps`.
    fn step(&mut self, now: Time, reqs: &mut Vec<Self::Req>,
            comps: &mut Vec<Self::Comp>, actions: &mut Vec<Action>);
    fn deadline(&self) -> Option<Time>;
    /// Nothing open and nothing in flight: safe to exit.
    fn idle(&self) -> bool;
}

/// The embedder. Shell code: may lock, log, call closures.
pub trait Host<C: Core> {
    fn handle(&mut self, now: Time, reqs: &mut Vec<C::HostReq>, comps: &mut Vec<C::Comp>);
}

pub fn run<C, I, H>(core: &mut C, io: &mut I, host: &mut H,
                    reactor: &mut Reactor, mut tap: Option<&mut Tap<C::Comp>>) -> io::Result<()>
where C: Core, I: IoStep<Comp = C::Comp, Req = C::IoReq>, H: Host<C>,
{
    // Allocated once and reused: steps drain their inputs and append outputs.
    let (mut events, mut comps, mut io_reqs, mut host_reqs, mut actions) = Default::default();
    let start = Instant::now();
    let mut now = Time::ZERO;
    loop {
        // Block until readiness, a signal, or the earliest deadline. With
        // perform results pending, only collect readiness (timeout 0).
        let timeout = if events.is_empty() { until(min(core.deadline(), io.deadline()), now) }
                      else { Some(Duration::ZERO) };
        reactor.poll(&mut events, timeout)?;                  // THE blocking call
        now = Time::since(start);                              // read once per iteration
        if let Some(t) = tap.as_deref_mut() { t.events(now, &events); }
        io.reap(now, &mut events, &mut comps);
        loop {                                                  // step to quiescence
            core.step(now, &mut comps, &mut io_reqs, &mut host_reqs);
            host.handle(now, &mut host_reqs, &mut comps);
            if let Some(t) = tap.as_deref_mut() { t.host(now, &comps); }   // host answers are inputs too
            io.step(now, &mut io_reqs, &mut comps, &mut actions);
            if comps.is_empty() { break; }
        }
        if core.done() && io.idle() && actions.is_empty() { return Ok(()); }
        reactor.perform(&mut actions, &mut events);           // one Event per Action
    }
}
```

`perform` is synchronous: every action's event is available in the very next
`reap`, and no step call happens while an action is "in flight". That keeps
planner state small. There is no "buffer is in the kernel" state, unlike
io_uring and the C port, where a SEND's buffer is pinned across iterations.
The only asynchronous completion is `Arm` → `Ready`.

### 5.4 The syscall vocabulary: `Action` and `Event`

```rust
pub struct SockId(pub u64);                      // Copy, Ord; allocated by the pure side

pub enum Action {
    Accept   { listener: SockId, new: SockId },  // accept one connection, name it `new`
    Connect  { sock: SockId, addr: SocketAddr }, // non-blocking connect (socket2)
    FinishConnect { sock: SockId },              // after writable readiness: check SO_ERROR
    Read     { sock: SockId, max: usize },
    Write    { sock: SockId, data: Vec<u8> },    // buffer moves in...
    Arm      { sock: SockId, interest: Interest }, // oneshot readiness
    Close    { sock: SockId },
    Resolve  { query: u64, host: String, port: u16 }, // blocking getaddrinfo (§4.7)
}

pub enum Event {
    Accepted  { listener: SockId, new: SockId, result: Result<SocketAddr, IoError> },
    Connected { sock: SockId, result: Result<Progress, IoError> }, // Done | InProgress
    Read      { sock: SockId, result: Result<Vec<u8>, IoError> },  // Ok(empty) = EOF
    Wrote     { sock: SockId, data: Vec<u8>, result: Result<usize, IoError> }, // ...and moves back
    Ready     { sock: SockId, readiness: Readiness },
    Closed    { sock: SockId },
    Resolved  { query: u64, result: Result<Vec<SocketAddr>, IoError> },
    Signal    { signal: SignalId },
}

pub struct IoError { pub kind: io::ErrorKind, pub os: Option<i32> } // Clone, Eq; WouldBlock is a kind
```

**The contract**, owned by `Reactor` and checked in its tests:

1. **Exactly one event per action.** Each event names the action's id. A
   `WouldBlock` is an ordinary `Err` result.
2. **Arms.** An `Arm` completes with one `Ready`, or with the `Closed` of its
   socket if that comes first. At most one arm is pending per socket; the TCP
   stage merges read and write interest before arming.
3. **Unknown ids.** An action on an unknown `SockId` completes with
   `IoError { kind: NotFound }`. It never panics.
4. **Buffers.** `Write`'s buffer always comes back in `Wrote`, with the count
   written, so the planner keeps the remainder.
5. **Signals** are the reactor's own resources. It re-arms and drains them
   itself, and they never appear in actions.

`Resolve` lives here rather than in a separate executor because it is the one
blocking action and deserves to be visible in the same vocabulary. Replacing
it later (for example with a resolver thread) changes only `perform`.

### 5.5 Planners: byte stages composed in a fixed order

Each connection is a struct of **stages** that transform buffers. A
connection's `pump` runs the stages in a fixed order inside `reap` and `step`.
This is the "pipeline of byte transformers" the brief asked about. It needs
no framework: stages are plain structs, and the composition is one function
per connection kind, with an enum (`Link::Plain | Link::Tls(..)`) where a stage
is optional.

```text
socket ──Read──▶ tcp.inbound ─▶ [tls: read_tls → process → reader] ─▶ plain_in ─▶ http parser ─▶ Comp::Request / FetchEvent
socket ◀─Write── tcp.outbound ◀─ [tls: writer → write_tls]         ◀─ plain_out ◀─ http writer ◀─ Req::Respond / Fetch
```

- **`tcp::Conn`: the connection lifecycle.**
  - It tracks the read, write and arm in flight, `want_read`, EOF, error, and
    closing.
  - It issues `Read` while the upper stage wants bytes and the inbound buffer
    is below its limit. After `WouldBlock` it arms read.
  - Writes: it writes the outbound buffer; a short write keeps the remainder,
    and `WouldBlock` arms write.
  - It merges interests into a single `Arm`.
  - `Close` waits until nothing is in flight. Because `perform` is
    synchronous, that is at most one round.
  - **Backpressure within a connection lives here.** It doesn't read what the
    upper stage hasn't consumed, and the upper stage sees the outbound
    queue's length.
- **`tcp::Listener`.** It accepts until `WouldBlock`, then arms read. It stops
  accepting at the connection cap, and the kernel backlog absorbs the rest.
  It closes on shutdown.
- **`tls::ClientStage`.** It wraps `rustls::ClientConnection` between
  `tcp.inbound`/`tcp.outbound` and `plain_in`/`plain_out`. Handshake progress
  and failures surface as stage state. A handshake failure is a
  connection-level error the HTTP client reports as `FetchEvent::Failed`.
- **`http1::server`.** Per connection:
  1. Read the head (httparse, `Partial` → read more), then the
     Content-Length body.
  2. Emit `Comp::Request { req, request }`.
  3. Wait for the core's response, which is either:
     - **structured** (`Respond { req, response }`, head written verbatim,
       body framed as `Full` or `Chunked(parts)`), or
     - **raw** (`RawStart`/`RawBytes`/`RawEnd`: bytes relayed verbatim, the
       recorder's case).
  4. Flush, then close (always `Connection: close`: one request per
     connection).

  Also:
  - Malformed head → `400`, oversized → `413`, request `Transfer-Encoding` →
    `501`.
  - EOF before the head ends → close without a response. EOF mid-body → serve
    the partial body, as today.
  - Handles shutdown with grace.
  - Limits: head 64 KiB, body 64 MiB, 256 connections. These are generous
    configuration defaults.
- **`http1::client`.** A fetch goes through these stages:
  1. `Resolve`.
  2. `Connect`, trying each address in turn.
  3. Optional TLS.
  4. Write the request head and body.
  5. Parse the response head, emitting `Head { status, headers, raw }`.
  6. Stream the body as `Body(bytes)` until EOF (`Connection: close` is sent
     upstream), then `End`.
  7. Failures at any stage give one `Failed { error }`.

  **Cross-connection backpressure is credit from the core:** at most one
  `Body` is outstanding per fetch, and the core returns `Ack { fetch }` once
  it has relayed it. That is stop-and-wait, VSR's stream-window idea with a
  window of 1. The pilot doesn't need a decoded-body mode (the recorder wants
  raw bytes). The chunked decoder exists as a tested pure function for
  temper's model client.

### 5.6 jig-server

- **Provider core (L1, pure):**

  ```rust
  pub enum Comp    { Request { req: ReqId, request: http1::Request }, Decision { req: ReqId, action: ScriptAction }, Stop }
  pub enum IoReq   { Respond { req: ReqId, response: http1::Response }, Shutdown { grace: Duration } }
  pub enum HostReq { Record(RecordedRequest), Decide { req: ReqId, view: RequestView } }
  ```

  Per request, it:
  1. strips the query for routing;
  2. projects the view for dialect routes;
  3. emits `Record` (before responding, as today);
  4. takes the next action from the `Plan`, or asks with `Decide` for
     `Plan::External`;
  5. renders:
     - `Reply` → SSE (the current headers, one chunk);
     - `HttpError` → the current error format;
     - `StreamError`/`AbortStream` → `501` as today;
     - an unknown path → `404` with `Content-Length: 0`.

  All response bytes must stay identical to `ca1edfd`'s. A convenience
  `serve_request(&mut Provider, Option<&mut Rule>, Request) -> (Response, RecordedRequest)`
  wraps one step for in-process users.
- **`FakeLlm` (L4).** `start(script)`:
  1. binds a `std::net::TcpListener` on the caller's thread, so `base_url` is
     valid at return;
  2. splits the script;
  3. spawns one OS thread that builds the `Reactor`, adopts the listener and
     runs `run(provider, server_io, host)`.

  The host appends records to the shared log and answers `Decide` with the
  rule closure. `requests()` clones the log. `Drop` raises the signal and
  joins.

### 5.7 jig-record

- **Recorder core (pure).** It receives:
  - client `Request`s from the server planner;
  - `FetchEvent`s from the client planner;
  - `Stop`.

  Per client request:
  1. Route by path (`route.rs`).
  2. Unroutable → structured `204` (preflight).
  3. Routable → `Fetch { fetch, upstream: Upstream { host, port, tls }, head, body }`,
     with the head built by today's `build_upstream_request_head` (Host
     rewritten, `Accept-Encoding: identity`, `Connection: close`).
  4. Relay upstream bytes to the client verbatim (`RawStart` with the raw
     response head, then `RawBytes`, then `RawEnd`), acknowledging each
     `Body`.
  5. Capture status, headers and the raw body. The fixture's `response.sse`
     keeps upstream chunk framing, exactly as today.
  6. On `End`, emit `HostReq::Captured(Exchange)` and a log line.

  **Modes:** `Once` shuts down after the first capture; `Pump` runs until
  `Stop`.
- **`Recorder` (L4).** It replaces `bind`, `proxy_once`, `handle_connection`,
  `record_once`, `record_once_blocking` and `CapturePump`:

  ```rust
  let rec = Recorder::start(RecorderConfig { mode, upstream_host, upstream_addr, roots })?;
  rec.base_url();
  rec.next_capture(timeout) -> io::Result<Exchange>   // once-mode users (temper's oracle test)
  rec.stop() -> Vec<Exchange>                          // pump-mode users (capture examples)
  ```

  - `upstream_addr` (connect here instead of resolving) and `roots` (trust
    these instead of webpki) are the test hooks. The upstream `Host`/SNI stays
    the route's or override's name.
  - The binary's `jig record` becomes `Recorder::start` in once mode plus
    writing the fixture.
  - The capture examples use pump mode.

### 5.8 Tap and replay

`Tap<Comp>` records, per iteration, the `now`, the reactor events, and the
completions the host fed back (rule decisions). Everything else the pure side
receives is derived from those.

`replay(core, io, tap)` feeds a fresh core and I/O step the same inputs and
returns the actions and host requests. A test asserts that they equal those of
the original run. Plain-TCP exchanges replay byte-exactly. TLS exchanges do not
([§4.5](#45-tls-rustls-clientconnection-buffered-api-ring-provider)).

This is the mechanism that makes real-I/O and chaos failures reproducible.

### 5.9 Ownership and buffer flow

A request's bytes change owner in this order, with no shared pointers:

1. `Vec<u8>` returned by `perform` in `Event::Read`
2. moved into `tcp.inbound`
3. parsed in place, with the body split off into `http1::Request.body` (moved)
4. `Comp::Request` moved into the provider
5. `RecordedRequest` built from clones (the log and the response both need
   the data)
6. the rendered body `String` moved into `Response`
7. serialized into `tcp.outbound`
8. moved into `Action::Write`, moved back in `Event::Wrote` with the count
9. dropped after close

Expected borrow-checker friction, following the explanation's §6 and §7 cases:

- **Helpers taking `&mut self` while a connection is borrowed.** Avoided by
  grouping state into `Conns` and `Ids` sub-structs and passing the parts.
- **Iterating connections while closing them.** Avoided by collecting ids
  first, then acting.
- **The TLS stage borrowing `tcp.inbound` while the HTTP stage reads
  `plain_in`.** Avoided by the stages being separate fields of the connection
  struct, which gives disjoint field borrows in one function body.

M4 records what actually came up.

## 6. Behaviour changes relative to `ca1edfd`

**Unchanged, byte for byte:**
- status lines, header order, SSE framing (one chunk), error bodies, `404`,
  the recorder's `204` preflight;
- the forwarded upstream head;
- raw response capture and the fixture files.

**Changed:**

1. **Concurrency.** Connections are served concurrently, so an idle or stalled
   client no longer blocks others. Script cursors advance in request
   completion order.
2. **Strict heads.** A malformed head gets `400`, a request
   `Transfer-Encoding` gets `501`, and a head or body over the limits gets
   `413`. Previously these were read leniently or misread.
3. **Shutdown.** `FakeLlm::drop` is bounded by a grace period (default 1 s)
   instead of waiting forever for a stalled client.
4. **Scripts.**
   - Rule closures are `FnMut + Send`.
   - `Script::Fixed` still holds a `Reply`, as before ([§4.10](#410-scripts-become-data-rule-closures-are-answered-by-the-embedder)).
   - `next_action` takes `&mut self`.
   - `next_reply` is removed.
   - Script files and reference delivery are data variants.
5. **Recorder API.** `bind`, `proxy_once`, `handle_connection`,
   `record_once`, `record_once_blocking` and `CapturePump` are replaced by
   `Recorder` ([§5.7](#57-jig-record)). Every recorder mode is concurrent.
   Error texts come from rustls and std directly.
6. **Crates and dependencies.** `jig-runtime` is deleted. There is no skein
   dependency. jig depends on `steploop`.

temper's bump then needs:
- `script.next_reply(view)` inside `Script::rule` closures (four
  `live_manifest` fakes that wrap a file script) →
  `Script::action_rule` with `script.next_action(view)`, which also stops
  HTTP errors in those scripts from degrading to empty replies;
- `jig_request_oracle.rs` switched to `Recorder`;
- `late_stream_jig.rs` using `&mut script`;
- the `jig-runtime` dev-dependency dropped.

## 7. Testing strategy

| Layer | Technique | Examples (mapped to the brief's deliverables) |
|---|---|---|
| L0/L1 cores | scripted completions → assert requests | every script kind; `Decide` round trip; `404`; HTTP errors; record-before-respond; recorder routing, preflight, capture assembly, `Ack` credit, once vs pump, stop |
| L2 planners | scripted `Event`s → assert `Action`s and completions; split points exhaustively | partial reads at every offset; short writes; `WouldBlock` then `Ready`; EOF mid-head and mid-body; one arm per socket; close waits for in-flight actions; backpressure; the TLS stage against an in-memory rustls server (ciphertext shuttled by the test), including handshake failure; client head parse across splits |
| L3 reactor | real fds, single thread | oneshot arms fire once; `Close` completes a pending arm; unknown ids; signal wake from another thread; non-blocking connect to a closed port |
| L4 real sockets | loopback, real threads for peers | the existing `jig-server/tests/*` unchanged; concurrent clients with an idle one; slow reader (small `SO_RCVBUF`); client disconnect mid-SSE; `drop` during a stalled response returns within grace; recorder against a local rustls upstream (rcgen CA): happy path, bad certificate, preflight, idle pooled connection, upstream EOF mid-stream |
| Replay | tap a real plain-TCP exchange → replay → identical outputs | the "record/replay test of one full exchange" |
| Round trip | recorder in front of a jig `FakeLlm` upstream (TLS via the test hook) → fixture → `derive` → serve | the capture pipeline end to end, offline |

No test sleeps to wait for a condition. Real-socket tests use blocking std
clients with timeouts. Tests at L0–L2 use no threads, sockets or clocks.

## 8. Using jig from temper without I/O

The owner's goal is for temper tests at the business-logic layer to simulate
LLM interactions deterministically with no I/O. How temper uses jig today (57
`FakeLlm::start` sites):

- mostly an in-process agent over real loopback HTTP through tongs providers;
- some separate-process runs (live manifests, benchmarks);
- scripts as data files;
- two hand-written servers built from jig-core pieces, needed for in-band
  stream errors and header capture;
- the jig-record oracle;
- run replays captured against jig (WP2.1).

temper's core vocabulary (WP2.1): the machine emits
`IoRequest::ModelStream { id, provider, request: ModelRequest }` and receives
`ModelEvent*` plus one terminal (`ModelStreamEnded`,
`Failed{Model(Provider{http_status..})}` or `Cancelled`). `ScriptedExecutor`
answers requests from rules in virtual time.

Seams, from the top down:

1. **Model vocabulary** (recommended for business-logic tests). A jig-backed
   responder in temper's `ScriptedExecutor` works like this:
   1. Build the provider's wire body from the `ModelRequest` with tongs' pure
      request builder.
   2. Call `jig_server::serve_request(provider, rule, request)`, where the
      request is a `POST /chat/completions` (or the dialect's route).
   3. A non-2xx response becomes `Failed { Provider { http_status, body } }`.
   4. Otherwise, feed the SSE body (de-chunked by the pure decoder) through
      tongs' pure SSE parsing, producing `ModelStreamEvent`s.

   This exercises tongs' request building and SSE parsing and jig's
   script/render with no sockets and no simulated world. It is the thing a
   seeded vocabulary-level fault injector wraps. It needs a small tongs PR
   making the pure parts public (already planned in the shell track). The
   adapter lives in temper, because it speaks temper's vocabulary.
2. **HTTP message.** The provider core directly (`Request` → `Response`). It
   replaces the hand-written servers once jig grows `StreamError`,
   `AbortStream` and header capture, which are small follow-ups made easy by
   the core seeing whole messages.
3. **Bytes.** Not a simulated world ([§4.2](#42-the-testing-layer-no-byte-level-simulated-world)).
   temper's own HTTP/TLS/SSE planners are tested with scripted events, as
   jig's are.
4. **Real sockets.** `FakeLlm`, for separate-process and real-client tests.

## 9. Milestones and work split

Work is done by parallel sub-agents in separate git worktrees, coordinated
from the main session. At most three heavy builds run at once, and disk and
memory are watched. Each milestone lands as one or more PRs on `ai/jig`.

- **M0.** This document, and the brief committed alongside it.
- **Wave 1, in parallel, starting from a crate skeleton committed first:**
  - **A. `steploop` foundation:**
    - `time`, `sys` and the reactor (with signals);
    - `run`, the traits, `Tap` and `Deadlines`;
    - tests: the reactor contract, and an echo server over real sockets.
  - **B. Scripts and provider core:**
    - the `Script` redesign in jig-core ([§4.10](#410-scripts-become-data-rule-closures-are-answered-by-the-embedder));
    - the pure provider core and `serve_request` with scripted tests;
    - the old async server keeps compiling through a transitional
      `Mutex<Script>`, deleted in wave 3.
  - **C. `steploop::http1::codec`:** head parse and serialization,
    Content-Length, chunked encode and decode, exhaustive split-point tests.
- **Wave 2:**
  - **D.** `tcp` stages and the `http1::server` planner (depends on A and C).
  - **E.** The `tls` stage and the `http1::client` planner with the `Ack`
    credit (depends on A and C).
- **Wave 3:**
  - **F. The `FakeLlm` port.** The async server is deleted and the existing
    tests pass unchanged. It adds the new real-socket tests and the replay
    test.
  - **G. The `Recorder` port.** It covers the binary, the examples and xtask,
    adds the local-TLS upstream tests and the round-trip test, and deletes
    `jig-runtime` and skein.
- **M4.** Results ([§11](#11-results-m4)): line counts before and after,
  borrow-checker notes, answers to [§10](#10-questions-for-temper-and-how-the-pilot-answers-them),
  and recommendations for temper. Also updates the README and the repository
  layout doc.

## 10. Questions for temper, and how the pilot answers them

These are the brief's six questions. The expected answers here are
hypotheses, confirmed or revised at M4.

1. **Does the two-step shape stay simple as protocols stack
   (TCP → TLS → HTTP → SSE)?** Expected: yes. Each layer is a byte stage with
   buffers in and out, composed in a fixed order per connection kind
   ([§5.5](#55-planners-byte-stages-composed-in-a-fixed-order)). No generic
   pipeline framework is needed. We'll measure planner sizes.
2. **Where does backpressure live?** Within a connection, in the I/O step:
   don't read what hasn't been consumed, and let the outbound queue's length
   be visible. Across connections (relaying), in the core, as credit (`Ack`).
3. **How are per-connection states identified and reclaimed?** Never-reused
   `u64` ids allocated by the pure side, with `BTreeMap` state. A stale event
   misses its lookup and is ignored. No generational slab is needed.
4. **Is `perform` really thin?** Target: under 250 lines for the whole
   reactor, including signals and `Resolve`. Reported at M4.
5. **How good is the simulated world as a test tool?** Superseded by
   [§4.2](#42-the-testing-layer-no-byte-level-simulated-world). M4 reports on
   scripted planner tests, real-socket tests and replay instead.
6. **skein's reactor or mio for temper?** Neither as it stands: an owned
   oneshot wrapper over `polling`, moving into skein when temper adopts it
   ([§4.1](#41-the-reactor-an-owned-wrapper-over-polling)).

## 11. Results (M4)

*To be written at M4.*

## 12. Sources

- `~/desired-rust-programming-model/`: `explanation`, `model/harness`, and
  VSR's `docs/io-design.md` and `include/vsr-io.h`.
- `~/src/c/jig`: `DESIGN.md`, `docs/comparison.md`,
  `docs/experiment-log.md`, `src/io/loop.c`, `src/io/rec_loop.c`,
  `include/jig.h`.
- temper: `docs/plans/agent-single-loop/ALIGNMENT_REPORT.md` (and its
  Addendum), the retrospective `git show 11c53ef1^:asupersync-followup.md`,
  and temper's project memory note `skein-prune-map`.
- skein at `50c7719`: `crates/skein/src/runtime/reactor/{mod,epoll,lab,io_uring,kqueue,source}.rs`.
- mio 1.2.2 and polling 3.11 sources.
