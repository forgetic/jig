# Agent guidance for jig

- **jig is a kit for agentic applications;** temper is its first
  application. Read [README.md](README.md), then
  [docs/design/README.md](docs/design/README.md).
- **While jig is built inside temper's repository** (under `jig/`):
  - temper's `docs/development/workflow.md` applies to jig. Its checks
    and test budgets cover jig's crates, which are members of temper's
    workspace.
  - **Nothing under `jig/` depends on or names temper:** no dependency on
    a temper crate, and no citation of temper's documents or code. temper
    may appear in jig's documents only as an example of an application,
    and says so. A check in the gate enforces the dependency rule once
    jig has crates.
  - **Everything under `jig/` is written as it will be in jig's own
    repository:** paths relative to `jig/`, crates named `jig-*`, and
    citations in the form they will keep.

  This item is removed when jig moves.
- **jig follows skein's foundation documents:** `programming-model.md`,
  `testing-strategy.md` and `notes.md`. Read them before writing code.
  Citations use those file names and their sections.
- **jig's design is `docs/design/`.** Inside jig, a bare file name names
  one of its documents (`README.md`, 6.2). skein's design documents are
  named with skein's name (skein's `lib.md`). Code cites a section at
  module or type level, never stamped on every field.
- **Every crate of jig is step code under the strict subset** of
  programming-model.md, section 10, except jig's test kits, which are
  ordinary Rust like every world. jig defines no traits and no generic
  items in step code. A mechanism that must be generic, such as the
  journal (`README.md`, 6.2), belongs in skein-lib, and goes there before
  its jig consumer.
- **jig owns every invariant it promises.** An application's root only
  routes and translates (`README.md`, section 6). A change that would
  make an application's root decide or order something is a change to
  jig's design first.
- **The core never depends on smith.** Only jig's agent hosts and the
  client's view of conversations do, so an application without agents
  never links smith (`README.md`, section 9).
- **No system is named in jig's code:** no forge, repository, branch, pull
  request, issue or git. Systems are an application's connectors.
- **Work locally,** on isolated branches merged with `--ff-only`. Do not
  push or publish unless the session explicitly authorises it.
