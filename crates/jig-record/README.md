# jig-record

A passthrough **recorder**: it captures a real client ↔ real backend
interaction to client/role-tagged on-disk fixtures, with secrets redacted. This
is the P1 capture substrate the rest of the fixture pipeline (#13) derives from.
It only **captures** — no parsing or template derivation happens here (that is
P2, #14).

## How it works

```
official client ──HTTP──▶ jig record (127.0.0.1:0) ──HTTPS──▶ real backend
                              │
                              └─▶ fixtures/<dialect>/<scenario>/recordings/<client>/
```

- **Routes by path → dialect** using the same route table as `jig-server`
  (`/chat/completions`, `/v1/messages`, `/backend-api/codex/responses`), then
  forwards over **HTTPS** to that dialect's real upstream.
- **Streams the response back unbuffered**: every byte read from the upstream is
  written to the client *and* appended to the capture before the next read, so
  SSE timing and framing are preserved (no re-chunking, no buffering).
- **Redacts at capture time**: `authorization`, `x-api-key`, OAuth account
  headers, cookies, … → the stable placeholder `REDACTED`. Nothing secret is
  ever written under `fixtures/`.

## Fixture layout

Each capture writes four files (taxonomy from #13):

```
fixtures/<dialect>/<scenario>/recordings/<client>/
  request.json       method, path, redacted headers, body
  response.headers   status + redacted headers
  response.sse       the full SSE byte stream, exactly as received
  meta.json          free-form client label, role, version, model, date, sha
```

`meta.json` carries a free-form `client` label (`openai-sdk`, `curl`, …) and a
`role` (`authoritative` for official clients, `subject` for an SDK under test).
jig hardcodes no specific consumer, so the same recorder can serve any
SDK-under-test `subject` recordings with no change.

## Recording is manual

A real capture needs a live API key and network, so it is **not** part of
`cargo test` — the default suite stays green and network-free, covering the
redactor, the fixture writer, routing, and request assembly with unit tests.

To record against a real backend:

```sh
# 1. Provide the official client's API key (gitignored).
cp crates/jig-record/record.env.example crates/jig-record/record.env
$EDITOR crates/jig-record/record.env
source crates/jig-record/record.env

# 2. Start one capture. It prints the loopback base_url and waits for one request.
JIG_CAPTURE_DATE=$(date +%F) \
  cargo run -- record --client openai-sdk --scenario single-text &

# 3. Drive an official client through the proxy via its base-url knob, e.g.
OPENAI_BASE_URL="$base_url" python -c 'import openai; ...'
# or DeepSeek (OpenAI-compatible):
cargo run -- record --client curl --scenario single-text \
  --upstream-host api.deepseek.com
```

`jig record` flags: `--client` and `--scenario` are required; `--role`
(`authoritative` default), `--client-version`, `--fixtures-root` (default
`fixtures`), `--upstream-host`, `--captured` (or `$JIG_CAPTURE_DATE`), and
`--recorder-sha` (or `$JIG_RECORDER_SHA`, else `git rev-parse --short HEAD`) are
optional.

The scenario matrix to capture first (per #18): single text turn; single
tool-call turn; tool-result → final.
