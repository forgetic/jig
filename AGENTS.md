# Agent entry point

This repo follows Diátaxis so that each document has a clear job and agents can
load only the context relevant to their task.

## Useful docs

- [Repository layout](docs/explanation/repository-layout.md)
- [Record and conform: why jig's fixtures come from real traffic](docs/explanation/record-and-conform.md)
- [How to refresh the recorded fixtures](docs/how-to/refresh-fixtures.md)
- [The sans-IO shell: design and rationale](docs/explanation/sans-io-shell.md)
  (the no-await loop, `steploop`, decisions and their reasoning)
- [Pilot brief for the sans-IO shell](docs/plans/sans-io-pilot-brief.md)
- Reference: [the fake LLM server](docs/reference/server.md),
  [the script file format](docs/reference/script-file.md),
  [the recorder](docs/reference/recorder.md)

## Checking changes locally

- `~/.local/bin/jig-ci-1.85` is the local stand-in for CI. It replicates the
  Forgejo runner's rustc/clippy 1.85 (fmt, build, clippy with `-D warnings`,
  tests). Run it from a jig checkout.
- Run heavy cargo commands under a memory cap, e.g.
  `systemd-run --user --scope -q -p MemoryMax=4G -p MemorySwapMax=0 -- jig-ci-1.85`.
  The machine has no swap, and parallel builds have frozen it.
