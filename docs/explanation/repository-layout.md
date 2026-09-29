# Repository layout

```
jig/
├── Cargo.toml              # [workspace] + thin [package] jig (the binary)
├── README.md               # what it is, how to run, the three routes
├── src/
│   └── main.rs             # thin glue: serve a script (FakeLlm::start) or run one `record` capture
├── docs/
│   ├── explanation/        # design rationale (this file; record-and-conform; sans-io-shell)
│   ├── how-to/             # operator procedures (refresh-fixtures)
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
