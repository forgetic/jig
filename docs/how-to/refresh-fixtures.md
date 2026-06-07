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

`xtask record` expands the scenario matrix and drives `jig record` once per
(dialect, scenario, client). Start with a dry run to see exactly what it will do
without spawning anything:

```sh
cargo run -p xtask -- record --dialect openai --dry-run
```

Then record one dialect, or everything:

```sh
# One dialect (all its scenarios and clients):
cargo run -p xtask -- record --dialect openai

# The whole matrix (explicit opt-in; a bare `record` is refused):
cargo run -p xtask -- record --all
```

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

### Driving the client

For each invocation the orchestrator prints which client to drive and starts
`jig record`, which prints a loopback `base_url` (`http://127.0.0.1:PORT`). Point
the official client at that base URL and run a trivial task that produces the
scenario's shape (e.g. "create file foo.txt with bar" for a tool-call). The
recorder forwards one exchange to the real backend, captures it, and exits.
A complete chat-completions capture ends in the `[DONE]` SSE terminator.

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

## Deriving the templates

After recording (or any time you change the masking policy), reduce the captured
recordings to the committed conformance artifacts. This is **offline and
deterministic**:

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
2. `cargo run -p xtask -- derive` — re-derive the templates from the new captures.
3. `cargo test --workspace` — the offline conformance half (incl. T1/T2) must
   stay green.
4. Commit the refreshed fixtures and templates with the capture date in the
   message.
