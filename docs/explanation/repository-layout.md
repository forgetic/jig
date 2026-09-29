# Repository layout

```
jig/
├── Cargo.toml              # [workspace] + thin [package] jig (the binary)
├── README.md               # what it is, how to run, the three routes
├── src/
│   └── main.rs             # thin glue: serve a script (FakeLlm::start) or run one `record` capture
├── docs/
│   ├── explanation/        # design rationale (this file; record-and-conform)
│   └── how-to/             # operator procedures (refresh-fixtures)
└── crates/
    ├── steploop/           # the no-await loop: reactor, run/tap/replay, TCP/TLS stages, HTTP/1 planners
    ├── jig-core/           # dialect-agnostic logic, no async
    │   ├── parse/          # SSE → canonical Reply parsers (openai/anthropic/codex)
    │   ├── render/         # canonical Reply → SSE renderers
    │   └── conform/        # masking policy + structural-template derivation (P2)
    ├── jig-server/         # the embeddable service API: pure provider core + FakeLlm on a steploop loop
    ├── jig-record/         # passthrough recorder: pure relay core + Recorder on a steploop loop
    └── xtask/              # developer task runner: `record` + `derive` + `staleness`
```

`steploop` is generic (no jig types). The two servers are layered the same way:
a pure core, a pure I/O step composing steploop's planners, a host for what may
not enter pure code, and a handle that runs the three on one OS thread (see
[the sans-IO shell](sans-io-shell.md)).

`fixtures/<dialect>/<scenario>/` holds the committed recordings and the derived
`*.template.json` / `drive-shape.json` conformance artifacts (see
[record-and-conform](record-and-conform.md)).
