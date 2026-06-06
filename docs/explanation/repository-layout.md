# Repository layout

```
jig/
├── Cargo.toml              # [workspace] + thin [package] jig (the binary)
├── README.md               # what it is, how to run, the three routes
├── bootstrap.md            # this file
├── src/
│   └── main.rs             # thin glue: load script, FakeLlm::start, print base_url, block
└── crates/
    ├── jig-core/           # dialect-agnostic logic, no async
    └── jig-server/         # the embeddable service API
```
