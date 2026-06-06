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
  `set-cookie`, and the OAuth account headers (`openai-organization`,
  `chatgpt-account-id`, the `x-oauth-*` / `x-stainless-account*` families) have
  their **values** replaced with the stable placeholder `REDACTED`.
- Header **names** are preserved, so a fixture still records *which* headers the
  client sent and what scheme was used — only the credential is gone.
- The credential is still forwarded **on the wire** to the real upstream;
  redaction applies only to the captured copy.

After a refresh, review the diff before committing — confirm no real key,
cookie, or account id appears anywhere under `fixtures/`. The redactor and the
fixture writer are unit-tested (`cargo test -p jig-record`) to enforce this, but
the human review on each refresh is the backstop.

## After recording

1. `git diff fixtures/` — confirm the captures look right and no secret leaked.
2. `cargo test --workspace` — the offline conformance half must stay green.
3. Commit the refreshed fixtures with the capture date in the message.
