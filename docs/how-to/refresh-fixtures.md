# How to refresh the recorded fixtures

This is the operator procedure for re-recording `jig`'s fixtures from real
provider traffic with `xtask record`, and for checking how stale the committed
fixtures are. For *why* the pipeline is shaped this way, read the
[record-and-conform explanation](../explanation/record-and-conform.md).

Recording is **manual and online**: each capture proxies a real client ↔ real
backend exchange, so it needs a live credential on the client side and network
access. It is deliberately **not** part of `cargo test` — the default test suite
stays offline and green.

## Prerequisites

1. **The official clients installed** for the dialects you intend to refresh:
   - OpenAI/DeepSeek: any client that lets you set the base URL — the OpenAI or
     DeepSeek SDK, or plain `curl`.
   - Anthropic: the Claude Code CLI (or the Anthropic SDK).
   - Codex: the Codex CLI.
2. **Credentials**, mirroring the `forgejo-mcp.env` pattern — a **gitignored**
   env file holds the API key the client reads. The recorder is a transparent
   proxy, so the key lives on the client, not in jig:

   ```sh
   # crates/jig-record/record.env  (gitignored; never commit)
   OPENAI_API_KEY=sk-...
   # DEEPSEEK_API_KEY / ANTHROPIC_API_KEY as needed
   ```

   `*.secrets.env` and `crates/jig-record/record.env` are already in
   `.gitignore`. OAuth-based clients log in out of band:
   - **Codex:** `codex login` once (browser OAuth, cached in
     `~/.codex/auth.json`, auto-refreshed).
   - **Anthropic via the pi-SDK driver:** `pi /login anthropic` (subscription
     OAuth; wired up in P6, #17).

## The one-shot refresh

`xtask record` is the **primary** entry point. It expands the scenario matrix
and, for each (dialect, scenario, client) cell, **drives the right capture
harness end to end** — the official client for authoritative cells (a
deterministic OpenAI/DeepSeek request, the Claude Code CLI, or `codex exec`) and
the pi-SDK driver for subject cells — then re-derives the templates from the new
authoritative captures. One command refreshes the selected fixtures and leaves
the tree in sync; you do not start a recorder and drive a client by hand.

Start with a dry run to see exactly which driver runs for each cell, and the
concrete command it will spawn, without spawning anything:

```sh
cargo run -p xtask -- record --dialect openai --dry-run
```

Then record one dialect, or everything:

```sh
# One dialect (all its scenarios and clients):
cargo run -p xtask -- record --dialect openai --upstream-host api.deepseek.com

# The whole matrix (explicit opt-in; a bare `record` is refused):
cargo run -p xtask -- record --all --upstream-host api.deepseek.com
```

`record` dispatches per `(dialect, client)`:

- **openai-sdk** (authoritative) — a self-contained `openai_capture` harness
  issues the scenario's chat-completions request straight at the recorder, so no
  external client is needed. Export the bearer it forwards (`DEEPSEEK_API_KEY`
  when `--upstream-host api.deepseek.com` is set, else `OPENAI_API_KEY`).
- **claude-code** (authoritative) — drives the `claude` CLI via the `capture`
  harness with the per-scenario prompt/marker/tools baked into the matrix.
- **codex** (authoritative) — drives `codex exec` via the `codex_capture`
  harness, enabling reasoning for `thinking-text` automatically.
- **pi-sdk** (subject) — drives `pi_agent_rust` directly via the
  `pi_subject_record` harness, using the real credentials in
  `~/.pi/agent/auth.json`.

After the authoritative captures succeed, `record` runs the equivalent of
`xtask derive` automatically, so the committed `*.template.json` /
`drive-shape.json` stay in sync. Pass `--no-derive` to capture without touching
the templates (e.g. when iterating on a single recording).

Re-runs are **idempotent**: a recording overwrites the four files in its
`recordings/<client>/` directory in place, so refreshing a scenario replaces it
rather than accumulating duplicates.

### Refresh just one cell

The selection flags compose, so you can refresh a single fixture without
re-recording the rest:

```sh
cargo run -p xtask -- record \
  --dialect anthropic --scenario tool-call --client claude-code
```

### Pointing a dialect at a compatible backend

The openai dialect can be recorded against any OpenAI-compatible backend
(DeepSeek, a gateway) by overriding the upstream host:

```sh
cargo run -p xtask -- record --dialect openai --upstream-host api.deepseek.com
```

### Provenance stamping

Every recording's `meta.json` is stamped with a **capture date** and the
**recorder's git sha** so a refresh is reproducible and auditable. Both are
discovered automatically (today's date in UTC; `git rev-parse --short HEAD`), or
pinned explicitly:

```sh
cargo run -p xtask -- record --dialect openai \
  --captured 2026-06-06 --recorder-sha "$(git rev-parse --short HEAD)"
```

### How the client is driven

You no longer drive the client by hand: `xtask record` runs the right harness
for each cell automatically (see the dispatch list above). Under the hood each
harness stands up the passthrough recorder on a loopback `base_url`
(`http://127.0.0.1:PORT`), drives its client through it against the real backend,
forwards one exchange, captures it, and exits. A complete chat-completions
capture ends in the `[DONE]` SSE terminator; an Anthropic capture in
`message_stop`; a Codex capture in `response.completed`.

The individual harnesses are still runnable on their own — as **debugging
tools** when a single cell misbehaves and you want to iterate on a prompt or
inspect the raw exchange. Those lower-level commands are documented in
[Lower-level harnesses (debugging)](#lower-level-harnesses-debugging) below; the
one-shot `xtask record` path above is the supported way to refresh fixtures.

### The `parallel-tool-calls` scenario (#30)

`parallel-tool-calls` captures **two** tool calls emitted in a single assistant
turn — the renderers and parsers already handle multiple indexed tool calls, and
this scenario pins that behaviour against real provider traffic. The shape is
elicited by the prompt; there is no `tool_choice` knob on the subject SDK, so the
prompt must name two distinct inputs and ask for both calls in one turn.

- **openai / DeepSeek** — drive the recorder with a two-city request under
  `tool_choice: required`. DeepSeek reliably returns two `get_weather` calls
  (`index: 0` Paris, `index: 1` London) ending in `finish_reason: tool_calls`:

  ```sh
  # Start the recorder (prints a loopback base_url), then curl it:
  jig record --client openai-sdk --scenario parallel-tool-calls \
    --upstream-host api.deepseek.com --client-version curl-deepseek \
    --captured "$(date -u +%F)" --recorder-sha "$(git rev-parse --short HEAD)"
  curl -s "$BASE/chat/completions" -H "Authorization: Bearer $DEEPSEEK_API_KEY" \
    -H 'Content-Type: application/json' -d '{
      "model":"deepseek-v4-flash","stream":true,"stream_options":{"include_usage":true},
      "tools":[{"type":"function","function":{"name":"get_weather",
        "description":"Get current weather for a city",
        "parameters":{"type":"object","properties":{"city":{"type":"string"}},
        "required":["city"]}}}],
      "tool_choice":"required",
      "messages":[{"role":"user","content":"Get the current weather for both Paris and London. Call the get_weather tool once for each city, in the same turn."}]}'
  ```

- **anthropic / Claude Code** — ask for two independent shell commands in one
  turn; Claude Code batches them into two `tool_use` blocks. Drive a tool whose
  args are self-contained values (`Bash` `command`), not absolute paths, so the
  committed template stays portable:

  ```sh
  cargo run -p jig-record --example capture -- \
    --scenario parallel-tool-calls --capture-index 0 --match-body JIGPAR91 \
    --model claude-sonnet-4-5 --captured "$(date -u +%F)" \
    --recorder-sha "$(git rev-parse --short HEAD)" \
    --claude-home /tmp/jig-claude-home --allowed-tools Bash \
    -- "JIGPAR91 Run two shell commands in parallel using two tool calls in the same turn: first 'echo hello' and second 'echo world'. Issue both Bash tool calls together in one turn."
  ```

- **codex** — the authoritative cell is a **reviewed, documented skip**: the only
  official driver is the Codex CLI, which is not available in every capture
  environment. `CODEX_SCENARIOS` in `crates/xtask/src/matrix.rs` deliberately
  omits `parallel-tool-calls`; once a Codex capture can be produced, add the slug
  there and capture as for the other codex scenarios.

The pi-SDK **subject** side records `parallel-tool-calls` with the same two-city
prompt (`Scenario::ParallelToolCalls` in `subject.rs`) for `openai` and `codex`;
the `anthropic` subject cell is reviewed-missing (subscription-OAuth blocker, same
as the other anthropic subject cells). After capturing, run `xtask derive` and
the offline T1–T4 conformance as usual.

## Lower-level harnesses (debugging)

`xtask record` drives each of these harnesses for you. Run them directly only
when you need to **debug a single cell** — iterate on a prompt, pick a different
`--capture-index`, or inspect a raw exchange — outside the orchestrator. They
write the same four-file recording in place, so a manual capture followed by
`xtask derive` is equivalent to the one-shot path for that cell.

### OpenAI/DeepSeek via the self-contained `openai_capture`

The chat-completions request is a single deterministic exchange with no external
CLI to drive, so its harness issues the request itself. It reads the bearer from
the environment (`DEEPSEEK_API_KEY` with `--upstream-host api.deepseek.com`, else
`OPENAI_API_KEY`) and never writes it to a fixture:

```sh
DEEPSEEK_API_KEY=sk-... cargo run -p jig-record --example openai_capture -- \
  --scenario tool-call --upstream-host api.deepseek.com \
  --captured "$(date -u +%F)" --recorder-sha "$(git rev-parse --short HEAD)"
```

### Anthropic via the `claude` CLI

Claude Code is an *agentic* driver: a single `claude -p` run pre-opens a pool of
connections and fires several `POST /v1/messages` (the tool-call turn, the
tool-result→final turn, plus internal housekeeping like session-title
generation). The `capture` example handles this — it accepts connections
**concurrently**, records every routable exchange, and commits the one selected
by `--match-body` (a unique marker in the prompt) and `--capture-index`:

```sh
cargo run -p jig-record --example capture -- \
  --scenario tool-result-final --capture-index 1 --match-body JIGTOOL42 \
  --model claude-sonnet-4-5 --captured "$(date -u +%F)" \
  --recorder-sha "$(git rev-parse --short HEAD)" \
  --claude-home /tmp/jig-claude-home --allowed-tools Write \
  -- "JIGTOOL42 Create a file named greeting.txt containing exactly: bar"
```

`--claude-home` points at an **isolated** `HOME` holding only
`~/.claude/.credentials.json` (the subscription OAuth), so the captured request
is the faithful Claude Code wire shape — the mandatory "You are Claude Code …"
system block and the `anthropic-*` / `X-Stainless-*` identity headers — without
this machine's local skills, MCP servers, or project `CLAUDE.md` bloating the
body. The example runs `claude` in an empty scratch dir, feeds the prompt on
stdin, and redacts the bearer, the `X-Claude-Code-Session-Id`, and the
`metadata.user_id` before anything is written. An Anthropic capture ends in the
`message_stop` SSE event.

### Codex via the `codex exec` CLI

Codex is `responses`-only and, like Claude Code, *agentic* — a single
`codex exec` run issues several `POST /backend-api/codex/responses` (the
tool-call turn, then the tool-result→final turn). The `codex_capture` example is
its counterpart of `capture`: it accepts connections **concurrently**, records
every routable exchange, and commits the one selected by `--match-body` and
`--capture-index`. Codex uses its own auth (`~/.codex/auth.json`, ChatGPT
OAuth) — there is no API key to thread through — so the example points it at the
recorder with a custom `jig` model provider whose `base_url` is the loopback
recorder and whose `wire_api` is `responses`:

```sh
# tool-result-final (the second responses POST in the run):
cargo run -p jig-record --example codex_capture -- \
  --scenario tool-result-final --capture-index 1 --match-body JIGEXEC55 \
  --captured "$(date -u +%F)" --recorder-sha "$(git rev-parse --short HEAD)" \
  -- "JIGEXEC55 Run the shell command: echo hello-from-codex using the \
      exec_command tool, then tell me what it printed."

# thinking-text needs reasoning enabled (off by default in exec):
cargo run -p jig-record --example codex_capture -- \
  --scenario thinking-text --match-body JIGTHINK77 \
  --captured "$(date -u +%F)" --recorder-sha "$(git rev-parse --short HEAD)" \
  --config model_reasoning_effort=high --config model_reasoning_summary=detailed \
  -- "JIGTHINK77 Think step by step about what 17 times 23 is, then reply \
      with just the number. Do not use any tools."
```

The example runs `codex exec --dangerously-bypass-approvals-and-sandbox
--skip-git-repo-check` in a scratch dir (so the agent never touches the repo)
and confirms empirically that Codex appends `/responses` to the provider
`base_url` — the routable path is `/backend-api/codex/responses`. Drive a
**function** tool (e.g. `exec_command`) for the `tool-call` / `tool-result-final`
scenarios: Codex's `apply_patch` is a *custom* tool (`custom_tool_call` items
with a freeform `input`), which the responses parser does not fold into a
canonical tool call. Before anything is written the recorder redacts the bearer,
`chatgpt-account-id`, the Codex session/identity headers (`session-id`,
`thread-id`, `x-client-request-id`, `x-codex-window-id`, `x-codex-turn-metadata`),
and the body's `client_metadata.x-codex-installation-id`. A Codex capture ends in
the `response.completed` event (there is **no** `[DONE]` sentinel).

### The pi-SDK `subject` recordings (P6, #17)

The second driver is `pi_agent_rust` used **directly** (no smith) as the
`subject` measured against the authoritative contract. `xtask record --client
pi-sdk` (or any selection that includes the subject cells) drives it for you
through the `pi_subject_record` example, capturing one `subject` recording per
`(dialect, scenario)` against the **real** backends using the credentials in
`~/.pi/agent/auth.json` (which you may use). To debug a single cell outside the
orchestrator, run the example directly:

```sh
# One subject cell, the same code xtask record drives:
cargo run -p jig-oracle --example pi_subject_record -- \
  --dialect anthropic --scenario tool-call \
  --captured "$(date -u +%F)" --recorder-sha "$(git rev-parse --short HEAD)"
```

The same capture logic is also still reachable as an `#[ignore]`d integration
test — handy when you want the whole-matrix loop or to run it under the test
harness:

```sh
# Record every (dialect, scenario) subject cell:
cargo test -p jig-oracle --test pi_subject_record \
  record_all_subject_fixtures -- --ignored --nocapture

# Refresh one cell (e.g. after a finding):
JIG_DIALECT=anthropic JIG_SCENARIO=tool-call \
  cargo test -p jig-oracle --test pi_subject_record \
  record_one_subject_fixture -- --ignored --nocapture --exact
```

For each cell the harness binds the recorder, resolves the dialect bearer, builds
a pi-SDK provider with `base_url` at the recorder, and drives one completion to
the real backend. Bearer resolution per dialect:

- **OpenAI/DeepSeek** — the `deepseek` API key, a standard bearer; recorded
  against `api.deepseek.com`.
- **Codex** — the `openai-codex` OAuth access **JWT** (which carries the
  `chatgpt_account_id` claim the SDK's codex provider extracts itself). Bearer
  resolution only — no special headers.
- **Anthropic** — the **subscription OAuth** workaround duplicated (with
  attribution) from smith in `crates/jig-oracle/tests/support/anthropic_oauth.rs`:
  it reads/refreshes the `anthropic` OAuth token (dual schema), sends the Claude
  Code identity headers, and sets the mandatory first `system` block
  (`You are Claude Code, …`) — without it the request is rejected with a `429`.
  The token endpoint is itself rate-limited, so if a refresh returns `429` wait a
  few minutes and retry.

The recorder redacts every bearer/identity value at capture time, so the
committed `recordings/pi-sdk/` are safe. A **non-2xx** subject capture is a
*finding*, not a fixture: it is still written (with its real status) and surfaced,
but never derived from. `xtask derive` is **not** run for `subject` recordings —
templates are anchored to the *authoritative* client only. The offline
**T3/T4** checks then validate the committed subject recordings:

```sh
cargo test -p jig-core --test pi_sdk_conformance
```

T3 reduces the subject `request.json` to its request grammar and asserts it is
conformant with the authoritative `request.template.json` grammar; a reviewed,
benign divergence (a spec-valid optional field the official sample omitted) is
recorded in that test's `REVIEWED_T3_FINDINGS` allowlist, so an *unreviewed*
divergence still fails. T4 checks the subject reply grammar against the
authoritative response template (best-effort).

## Deriving the templates

`xtask record` runs this step **automatically** after a successful refresh, so
you normally do not invoke it by hand. Run `derive` on its own only when you
recorded with `--no-derive`, drove a lower-level harness directly, or changed the
masking policy. It reduces the captured authoritative recordings to the committed
conformance artifacts and is **offline and deterministic**:

```sh
cargo run -p xtask -- derive
```

For every `<dialect>/<scenario>` with an `authoritative` recording it (re)writes
`response.template.json`, `request.template.json`, and `drive-shape.json` at the
scenario root, masking volatile values per the policy in
`crates/jig-core/src/conform/`. The dialect is chosen from the recording's
request path (`/chat/completions` → OpenAI, `/v1/messages` → Anthropic,
`/backend-api/codex/responses` → Codex), so each capture is reduced with the
right parser, renderer, and stream terminator (`[DONE]` for OpenAI,
`message_stop` for Anthropic, `response.completed` for Codex). Re-running it over
unchanged recordings produces byte-identical files, so a clean `git diff` after
`derive` means the captures and templates are in sync. The offline T1/T2
conformance tests — one per dialect, e.g.
`cargo test -p jig-core --test openai_conformance`,
`cargo test -p jig-core --test anthropic_conformance`, and
`cargo test -p jig-core --test codex_conformance` — assert jig reproduces these
templates exactly. See the
[record-and-conform design](../explanation/record-and-conform.md#deriving-templates-the-masking-policy)
for what is masked and why.

## Checking staleness

`xtask staleness` walks `fixtures/` **offline** and reports each recording's
capture age, flagging anything past the threshold (default 90 days):

```sh
cargo run -p xtask -- staleness
cargo run -p xtask -- staleness --max-age-days 60
```

It is **non-fatal** by default — a nudge to re-record. To gate a CI job on
freshness, add `--fail-on-stale` (which exits non-zero if any fixture is stale):

```sh
cargo run -p xtask -- staleness --fail-on-stale
```

## Redaction guarantees

Nothing secret is ever written to `fixtures/`. The recorder redacts at capture
time, *before* anything touches disk:

- `authorization`, `proxy-authorization`, `x-api-key`, `api-key`, `cookie`,
  `set-cookie`, the OAuth account headers (`openai-organization`,
  `chatgpt-account-id`, the `x-oauth-*` / `x-stainless-account*` families),
  Claude Code's per-session `x-claude-code-session-id`, and Codex's session /
  window identity headers (`session-id`, `thread-id`, `x-client-request-id`,
  `x-codex-window-id`, `x-codex-turn-metadata`) have their **values** replaced
  with the stable placeholder `REDACTED`.
- Identity carried in the request **body** is redacted too: Claude Code's
  `metadata.user_id` (and any nested `account_uuid` / `device_id` / `session_id`)
  and Codex's `client_metadata.x-codex-installation-id` collapse to `REDACTED`,
  so a body-carried account/installation id never reaches a fixture. A
  same-named *schema* property (Codex ships a tool whose argument schema has a
  `session_id` property — an object, not an identity value) is left intact, so
  the captured wire shape stays faithful.
- Header and JSON **names** are preserved, so a fixture still records *which*
  headers and keys the client sent and what scheme was used — only the
  credential/identity value is gone.
- The credential is still forwarded **on the wire** to the real upstream;
  redaction applies only to the captured copy.

After a refresh, review the diff before committing — confirm no real key,
cookie, or account id appears anywhere under `fixtures/`. The redactor and the
fixture writer are unit-tested (`cargo test -p jig-record`) to enforce this, but
the human review on each refresh is the backstop.

## After recording

1. `git diff fixtures/` — confirm the captures look right and no secret leaked.
2. `cargo run -p xtask -- derive` — `xtask record` already re-derives after a
   successful refresh, so this is only needed if you recorded with `--no-derive`,
   drove a lower-level harness by hand, or changed the masking policy. Re-running
   it over unchanged recordings is a no-op (byte-identical output).
3. `cargo test --workspace` — the offline conformance half (incl. T1/T2) must
   stay green.
4. Commit the refreshed fixtures and templates with the capture date in the
   message.
