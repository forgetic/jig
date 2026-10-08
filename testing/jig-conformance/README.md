# Conformance

The application engine on independent neighbours, with jig's referee checking
its promises (`domain/testing.md`, sections 5–6).

An application implements `Application` in its ordinary world crate. It gives:

- Its concrete root, configuration, events, deliveries, store keys and rows.
- Construction, step, release, readiness, timers and iteration reclamation.
- Independent systems, scripted hosts and parties, plus their boundary
  translations. `observed` maps their evidence and durable rows into referee
  observations; it never reads the root's state.
- Scenario policy and each effect kind's recovery class. Policy changes follow
  authenticated requests and durable acceptance, independently of the root.
- Its restart loads and row restoration, plus a conservative checked heap bound.

`Harness` owns commit application, cold restarts, simulated time and the referee.
Its ordered fake store holds and fails acknowledgements and pages loads slowly.
The harness retains the outside ledgers across a crash. `Cut::AfterCommit` arms
one cut before that durable acknowledgement reaches the root; `Cut::Random`
draws one cut from the seed. Sweeping means constructing a new world for each
commit of the baseline story, including a fresh seed for its peers and systems.

`ScenarioApplication` binds the eight shared scenarios to the application's
kinds and resources. Its actions use peer scripts and durable records; its
ending checks use the same outside evidence. A missing kind returns `None`
from `configuration`, so the caller can report the absent scenario. `run` keeps
the referee in every story iteration. Applications also write their own stories
with `Harness::send`, `advance`, `crash` and the fake store's faults.

The testing application in `tests/conformance` shows these bindings for two
connectors, one engine domain and independent remote or permanent engine hosts.
Its focused suite cuts around claim, answer, effect and initial admission;
its fuzzy suite sweeps each shared story's commits and seeded random cuts.
The referee tests deliberately corrupt root and connector boundaries, with a
faithful control through those same boundaries.

The referee checks atomic snapshots, released prerequisites, independent spend,
policy and judge facts, effect copies, ownership, holders and delivery bounds.
Durable attempts include their absolute retry deadlines; repeating the same
attempt cannot change one. External cold loads report their position in the
application's declared sequence and restart resets that position alone.
