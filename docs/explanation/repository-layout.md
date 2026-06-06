# Repository layout

```
jig/
├── Cargo.toml              # [workspace] + thin [package] jig (the binary)
├── README.md               # what it is, how to run, the three routes
├── bootstrap.md            # this file
├── src/
│   └── main.rs             # thin glue: serve a script (FakeLlm::start) or run one `record` capture
├── docs/
│   ├── explanation/        # design rationale (this file; record-and-conform)
│   └── how-to/             # operator procedures (refresh-fixtures)
└── crates/
    ├── jig-core/           # dialect-agnostic logic, no async
    │   ├── parse/          # SSE → canonical Reply parsers (openai/anthropic/codex)
    │   ├── render/         # canonical Reply → SSE renderers
    │   └── conform/        # masking policy + structural-template derivation (P2)
    ├── jig-server/         # the embeddable service API
    ├── jig-record/         # passthrough recorder: capture real interactions to redacted fixtures
    ├── jig-oracle/         # offline pi-SDK ↔ jig oracle test (drives pi_agent_rust directly)
    └── xtask/              # developer task runner: `record` + `derive` + `staleness`
```

`fixtures/<dialect>/<scenario>/` holds the committed recordings and the derived
`*.template.json` / `drive-shape.json` conformance artifacts (see
[record-and-conform](record-and-conform.md)).
