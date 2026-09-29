# Repository layout

```
jig/
├── Cargo.toml              # [workspace] + thin [package] jig (the binary)
├── README.md               # overview: what jig offers and how the pieces fit
├── src/
│   └── main.rs             # thin glue: serve a script (FakeLlm::start) or run one `record` capture
├── docs/
│   ├── explanation/        # design rationale (this file; record-and-conform; sans-io-shell)
│   ├── how-to/             # operator procedures (refresh-fixtures)
│   ├── reference/          # server routes and API, script file format, recorder API
│   └── plans/              # briefs for planned work (the sans-IO pilot)
├── fixtures/               # recordings + conformance artifacts per dialect; Temper script files
└── crates/
    ├── steploop/           # the no-await loop: reactor, run/tap/replay, TCP/TLS stages, HTTP/1 planners
    ├── jig-core/           # dialect-agnostic logic, no async: Reply, Script → Plan + Rule, request views
    │   ├── parse/          # SSE → canonical Reply parsers (openai/anthropic/codex)
    │   ├── render/         # canonical Reply → SSE renderers
    │   └── conform/        # masking policy + structural-template derivation (P2)
    ├── jig-server/         # the embeddable service API: pure provider core + FakeLlm on a steploop loop
    ├── jig-record/         # passthrough recorder: pure relay core + Recorder on a steploop loop
    │   └── examples/       # capture harnesses (capture, codex_capture, openai_capture)
    └── xtask/              # developer task runner: `record` + `derive` + `staleness`
```

## Crates

- `crates/steploop` — the generic loop, with no jig types: the reactor (an
  owned wrapper over `polling`), the `run` driver, tap and replay, and pure
  TCP, TLS client (feature `tls`) and HTTP/1 server and client planners.
- `crates/jig-core` — dialect-agnostic, async-free core: `Reply`/`Turn`/`Usage`/
  `StopReason`, `Script` (split into a data `Plan` and an optional `Rule`
  closure), request views, the SSE renderers and parsers, the conformance
  masking and templates, and the `ScriptFile` file format.
- `crates/jig-server` — the embeddable service API: the pure provider core
  (`Provider`, `serve_request`), its I/O step (`ServerIo`), its host
  (`FakeLlmHost`), and `FakeLlm`, which runs the three on one loop thread.
- `crates/jig-record` — the passthrough recorder, layered the same way: pure
  routing, redaction and fixture writer, the relay core (`RecorderCore`), its
  I/O step (`RecorderIo`), its host (`RecorderHost`), and the `Recorder`
  handle. The capture harnesses are its examples.
- `crates/xtask` — the developer task runner: the `record` orchestrator (expands
  the scenario matrix and runs each cell's capture harness), `derive` (templates
  from recordings) and the offline `staleness` check.
- `src/main.rs` — thin glue binary: serve a script (`FakeLlm::start`) or run one
  passthrough capture (`record`, via `jig_record::record_once`).

`steploop` is generic (no jig types). The two servers are layered the same way:
a pure core, a pure I/O step composing steploop's planners, a host for what may
not enter pure code, and a handle that runs the three on one OS thread (see
[the sans-IO shell](sans-io-shell.md)):

| | jig-server | jig-record |
| --- | --- | --- |
| core | `provider::Provider` | `relay::RecorderCore` |
| I/O step | `io::ServerIo` | `io::RecorderIo` |
| host | `host::FakeLlmHost` | `host::RecorderHost` |
| handle | `FakeLlm` | `Recorder` |

`fixtures/<dialect>/<scenario>/` holds the committed recordings and the derived
`*.template.json` / `drive-shape.json` conformance artifacts (see
[record-and-conform](record-and-conform.md)).
