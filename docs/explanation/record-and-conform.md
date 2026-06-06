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
            └─  │ a client       │  official CLI/SDK  *and*  pi_agent_rust (P6)
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

## Two drivers, two roles

The proxy does not care who drives it, so two kinds of client do, with strict
roles that keep the oracle property honest:

| Driver | Role | Provides |
| --- | --- | --- |
| **Official clients** (Codex / Claude Code / OpenAI–DeepSeek SDK) | **`authoritative`** — the spec | Real response framing *and* request grammar, including each provider's real tool-call encoding |
| **pi SDK** (`pi_agent_rust`, used directly) | **`subject`** — measured against the spec | Full control of tools/prompts/loop for deterministic shapes; its own requests, validated against the authoritative contract (P6, #17) |

**Rule:** jig's response contract is anchored to the **official** recordings,
never to the pi SDK. The `client` label and `role` are free-form data in
`meta.json`; the recorder hardcodes no specific consumer, which is why the same
recorder serves both roles with no code change.

## The offline oracle (P6, #17)

The `subject` driver doubles as an **offline oracle**: with jig proven faithful
by the dialect work (P3/P4), driving the pi SDK against jig — with **no network
and no credentials** — and asserting it parses jig and completes its agent loop
turns jig into a fast, deterministic check that the SDK speaks each dialect.

`crates/jig-oracle` is that check. Its integration test points a
`pi_agent_rust` provider's `base_url` at an in-process `jig_server::FakeLlm`,
streams a scripted reply, and asserts the SDK decodes jig's SSE into its
canonical event model and reaches `Done` — for all three dialects, both for a
single text reply and for a tool-call → tool-result → final loop. The pi SDK is
consumed **directly** (no smith). Codex validates that its bearer is a JWT
carrying a `chatgpt_account_id` claim before sending, so the test mints a
synthetic unsigned JWT locally (the shape the SDK's own tests use); nothing
leaves the machine. It runs in the default, network-free `cargo test`.

This is the *oracle* half of P6. The pi-SDK **recording** track — capturing
`subject` recordings and the T3 (request-validation) / T4 (cross-driver)
conformance checks — is **not** here: recording needs live provider credentials,
and T3/T4 compare against the structural `*.template.json` artifacts that P2
(#14) derives, which are out of scope for an offline test. The recorder already
supports the `subject` role with no code change (see "Two drivers, two roles"),
so that track is additive on top of P2.

## The fixture taxonomy

Each recording is written into the taxonomy from issue #13:

```
fixtures/<dialect>/<scenario>/
  recordings/
    <client>/                 # e.g. openai-sdk (authoritative) / pi-sdk (subject)
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

## The scenario matrix

Per dialect, the minimal matrix is: (a) single text turn, (b) single tool-call
turn, (c) tool-result → final turn, (d) thinking + text (Anthropic / Codex), and
(e) parallel tool calls. `xtask record` holds this matrix declaratively (see
`crates/xtask/src/matrix.rs`) so adding a dialect, scenario, or client is a
one-line edit and needs no orchestrator change. Selection flags
(`--dialect` / `--scenario` / `--client`) refresh a single cell without
re-recording everything.

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
- Not a dependency on any private tooling — the second driver is `pi_agent_rust`
  consumed directly.

## References

- Issue #13 — the full implementation plan and design (repeatability, secrets,
  the docs deliverable this file is part of).
- `crates/jig-record/` — the recorder (proxy, redaction, fixture writer).
- `crates/xtask/` — the `record` orchestrator and the `staleness` check.
- [How-to: refresh the fixtures](../how-to/refresh-fixtures.md).
