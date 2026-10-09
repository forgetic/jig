# jig

jig is a kit for agentic applications, written in Rust on skein. It
provides the rails that an application's work runs on: tasks done by
agents, procedures and people; authority that only narrows as tasks
delegate; effects on external systems made once; and state that outlives
processes and machines. An application brings its connectors to external
systems, its own domain logic and its client's own objects. It owns the
top of every process, and composes jig's parts beneath it.

temper, a software factory, is jig's first application.

Everyone who builds jig, or builds on it, reads skein's foundation first:

- `programming-model.md`: how the code is written, in skein, in jig and
  in every application;
- `testing-strategy.md`: how it is tested;
- `notes.md`: the questions still open about both.

jig's design starts at [docs/design/README.md](docs/design/README.md).

## In one page

- **A kit, not a framework.** jig has no application trait, no plug-in
  points and no callbacks. Nothing in jig calls into an application. An
  application's root domain calls jig's parts by name and routes between
  them and its own.
- **Rails, not boilerplate.** The code an application writes around jig
  only routes and translates, and has one shape in every application.
  Whatever is hard to get right is jig's: the commit barrier, restart,
  authority, and effects made once. jig's conformance world and referee
  check, in the application's own worlds, that its wiring kept them.
- **Built when pulled,** as skein is. temper's needs decide what jig
  holds and in what order; a second, small application keeps jig from
  being temper's in disguise.

## Where jig is

jig is a repository of its own; temper is its first application. See
[AGENTS.md](AGENTS.md).
