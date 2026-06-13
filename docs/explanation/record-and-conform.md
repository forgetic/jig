# Record and conform: why jig's fixtures come from real traffic

This is the **design explanation** for how `jig` stays faithful to the real
providers. It distills the plan in issue #13 into the rationale a reader needs to
understand *why* the recorder, the fixture taxonomy, and the conformance tests
are shaped the way they are. For the step-by-step refresh procedure, see the
[how-to](../how-to/refresh-fixtures.md).

## The problem

`jig` impersonates an LLM provider at the *transport and framing* level so a
client SDK parses its replies and drives its agent loop without real
credentials, network, or token spend. The risk is drift: if jig's wire output is
hand-authored from one reading of a provider, it is faithful to *that reading*,
not to the provider. Providers also evolve. So jig anchors its output to
**recordings of real interactions** between **official provider clients** and the
**real backends**, derives committed fixtures from those recordings, and asserts
jig speaks **exactly** like them. The exercise is **repeatable** so fixtures can
be refreshed as providers change — that repeatability is what P5 (#19) delivers.

## The key principle: format-faithful, content-irrelevant

The *content* of a conversation does not matter — real models are
non-deterministic. What must match **exactly** is the **conversation format**:

- **Invariant (must match):** header names and value *formats*, response framing
  (`content-type: text/event-stream`, chunking), JSON keys + types + nesting, SSE
  `event:`/`data:` names and ordering, and the `[DONE]` terminator.
- **Volatile (masked before comparing):** ids (`chatcmpl-…`, `msg_…`, `call_…`),
  timestamps / `created`, token counts, fingerprints, nonces, volatile headers
  (`date`, request-ids, `cf-*`, `set-cookie`, `server`), the arbitrary chunk
  boundaries of text deltas, and version-volatile client-identity values.

Conformance is therefore **structural**, not byte-equality.

## Two halves: record (online) and conform (offline)

```
            ┌─ real backend (api.openai.com / api.anthropic.com / chatgpt.com)
            │        ▲ TLS (real creds, from a gitignored secrets file)
   record   │   ┌────┴───────────┐
   (online,  │  │ jig record     │  passthrough proxy: tees the exchange verbatim
    manual,  │  │  (per dialect) │  → a client/role-tagged raw recording
    creds)   │  └────┬───────────┘
            │        │ http (the base_url the client is pointed at)
            │   ┌────┴───────────┐
            └─  │ a client       │  an official CLI/SDK
                └────────────────┘
                     │ redact secrets, derive templates
        fixtures/<dialect>/<scenario>/{recordings/, *.template.json, drive-shape.json}  ← committed
                     │
   conform   ┌───────┴────────┐
   (offline,  │ cargo test     │  render with jig → strip → == template (and request checks)
    no creds) │                │
            └────────────────┘
```

- **record** is **online**, needs credentials, and is run by hand (or on a
  schedule). It is never part of `cargo test`.
- **conform** is **offline** — no credentials, no egress — and *is* part of the
  default `cargo test`. This preserves the project invariant of a green,
  network-free test suite.

`xtask record` is the orchestrator for the online half;
[`xtask staleness`](#staleness-a-nudge-not-a-gate) reports how old the committed
captures are.

## The recorder is a driver-agnostic passthrough proxy

`jig record` (crate `crates/jig-record`) binds a loopback port, routes the
incoming request by **path → dialect** (the same route table the server uses),
forwards it over **HTTPS** to the real upstream, and streams the response back
**unbuffered** so SSE timing and framing are preserved. As the bytes flow it
**tees** the request (method, path, headers, body) and the full response (status,
headers, the raw SSE byte stream) to disk. Secrets are **redacted at capture
time** — `authorization`, `x-api-key`, OAuth account headers, and cookies become
a stable `REDACTED` placeholder — so nothing secret is ever written to a fixture.

No TLS interception or CA cert is needed: the client speaks plain HTTP to the
proxy and the proxy speaks HTTPS upstream.

## Drivers and roles

The proxy does not care who drives it. Every recording carries a `role`:

| Driver | Role | Provides |
| --- | --- | --- |
| **Official clients** (Codex / Claude Code / OpenAI–DeepSeek SDK) | **`authoritative`** — the spec | Real response framing *and* request grammar, including each provider's real tool-call encoding |
| An SDK under test | **`subject`** — measured against the spec | Its own requests, which can be validated against the authoritative contract |

**Rule:** jig's response contract is anchored to the **official** recordings,
never to an SDK under test. The `client` label and `role` are free-form data in
`meta.json`; the recorder hardcodes no specific consumer, which is why the same
recorder serves both roles with no code change. SDKs that want to prove
themselves against jig do so from their own repositories, by driving their
providers against an in-process `jig_server::FakeLlm` (offline) or through the
recorder (online, as `subject` recordings).

For the subject leg jig provides the building blocks; the SDK's repo owns the
harness and the committed recordings:

- `jig_record::CapturePump` — the recorder on its own runtime thread, so the
  SDK can drive a request through it synchronously;
- `jig_record::build_recording` + `Recording::write` — redact and persist a
  `role: subject` recording under the SDK's own fixture tree;
- `jig_core::conform::grammar` — reduce the subject's recorded request body to
  its wire-grammar skeleton and check it is conformant with the authoritative
  `request.template.json` (`grammar_findings` empty = the SDK invents no wire
  structure the official client does not use);
- `jig_core::fixtures_root()` — locate jig's authoritative templates from a
  path-dependency consumer.

[tongs](https://github.com/forgetic/tongs) is the worked example: an
online `#[ignore]` harness records tongs' own requests against the real
backends, and its offline `cargo test` validates every committed subject
recording against jig's templates (T3 request grammar, T4 reply-shape
consistency).

## The fixture taxonomy

Each recording is written into the taxonomy from issue #13:

```
fixtures/<dialect>/<scenario>/
  recordings/
    <client>/                 # e.g. openai-sdk (authoritative)
      request.json            # method, path, redacted headers, body
      response.headers        # status + redacted headers
      response.sse            # the full SSE byte stream, exactly as received
      meta.json               # client, role, dialect, scenario, model, captured date, recorder sha
  response.template.json      # derived from authoritative recordings (the masked skeleton)
  request.template.json       # derived from authoritative = the spec the SDK must meet
  drive-shape.json            # the turn shape used to drive jig in the T1 conformance test
```

`response.sse` is the verbatim stream (for human reference and debugging); the
`*.template.json` artifacts are the masked structural skeletons the conformance
tests compare against. Template derivation is the job of P2/P3/P4; P5 makes
producing the underlying recordings a one-command operation.

The OpenAI/DeepSeek `/chat/completions` slice of this has **landed** (P2, #14):
`fixtures/openai/{single-text,tool-call,tool-result-final}/` carry real DeepSeek
captures and their derived templates, and `cargo test` runs the T1/T2 checks over
them offline. See [Deriving templates](#deriving-templates-the-masking-policy)
below.

## Deriving templates: the masking policy

Derivation turns the verbatim recordings into the masked `*.template.json` +
`drive-shape.json` skeletons. It is **offline and deterministic** — a pure
function of the recording bytes — so re-deriving over the same recordings
produces byte-identical artifacts (`cargo run -p xtask -- derive`).

The reduction lives in `crates/jig-core/src/conform/` and is committed as
**data + code**, not ad-hoc string-munging:

- The **`parse_openai_sse`** parser (the inverse of `render_openai`) folds the
  fragmented, id-tagged SSE chunks back into the canonical `Reply`, coalescing
  arbitrary text-delta chunk boundaries — so the `Reply` *is* the
  chunk-boundary-independent body skeleton.
- The **masking policy** (`conform/mask.rs`) is the reviewable list of what is
  volatile: body keys (`id`, `created`, the served `model`, `system_fingerprint`,
  every `*_tokens` count) collapse to a stable `<MASKED>` sentinel; a **header
  allowlist** keeps `content-type` (the framing-contract signal), masks the
  volatile-but-present headers (`date`, `server`, `set-cookie`, request/trace
  ids, the `cf-*` / `x-amz-cf-*` edge families), and drops the rest. The policy
  is designed to extend to the version-volatile client-identity values P3/P6 need
  for Anthropic and Codex (see #13).
- The **templates** are then: `response.template.json` (masked canonical reply +
  framing invariants + header view), `request.template.json` (masked request,
  requested `model` kept), and `drive-shape.json` (the un-masked canonical reply
  jig is driven with in T1).

The two conformance properties, asserted offline over `fixtures/openai/*` by
`crates/jig-core/tests/openai_conformance.rs`:

- **T1** — drive jig with `drive-shape.json` → `render_openai` → strip (parse →
  mask) → must equal `response.template.json`.
- **T2** — the authoritative `request.json` → strip → must equal
  `request.template.json`.

A failure prints a readable structural diff (the JSON path that diverged), not a
wall of two blobs.

## The scenario matrix

Per dialect, the minimal matrix is: (a) single text turn, (b) single tool-call
turn, (c) tool-result → final turn, (d) thinking + text (Anthropic / Codex), and
(e) parallel tool calls — **two** tool calls emitted in one assistant turn.
`xtask record` holds this matrix declaratively (see `crates/xtask/src/matrix.rs`)
so adding a dialect, scenario, or client is a one-line edit and needs no
orchestrator change. Selection flags (`--dialect` / `--scenario` / `--client`)
refresh a single cell without re-recording everything.

Scenario (e), `parallel-tool-calls` (issue #30), is captured authoritatively for
the **openai** dialect (DeepSeek emits two `get_weather` calls for two cities
under `tool_choice: required`) and the **anthropic** dialect (Claude Code batches
two independent `Bash` calls into two `tool_use` blocks in one turn). The
**codex** authoritative cell is a reviewed, documented skip: the only official
driver is the Codex CLI, and it is not available in every capture environment, so
`CODEX_SCENARIOS` deliberately omits `parallel-tool-calls` until a Codex capture
can be produced.

## Staleness: a nudge, not a gate

Every `meta.json` records the **capture date**. `xtask staleness` walks
`fixtures/` offline, computes each recording's age against today, and flags any
older than a threshold (default 90 days). It is **non-fatal** by default — a
reminder to re-record as providers drift — but `--fail-on-stale` lets a CI job
opt into gating on it. The age computation is a pure function of the capture date
and a reference date, so it is unit-tested deterministically without a clock.

## What this is not

- Not content fidelity, token counting, or model-behaviour emulation — only the
  *format* matters.
- Not a dependency of CI on live credentials — recording is manual and offline;
  CI runs only the offline conformance half.

## References

- Issue #13 — the full implementation plan and design (repeatability, secrets,
  the docs deliverable this file is part of).
- `crates/jig-record/` — the recorder (proxy, redaction, fixture writer).
- `crates/xtask/` — the `record` orchestrator and the `staleness` check.
- [How-to: refresh the fixtures](../how-to/refresh-fixtures.md).
