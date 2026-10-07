# The application's root

Provisional, 2026-10-07. What an application's engine root is and does:
the top-level domain every application on jig writes, which composes
jig's core with the application's connectors and its own child domains.
It says what the root carries, the one shape every root has, what jig
asks of skein-lib's journal, how the root routes, translates and puts
outputs together, its part in restart and in effects, and what it must
never do. The core is engine.md; connectors are connectors.md. What is
still open is listed in section 14.

## 1. In one page

- **The application owns the root.** skein's model nests step machines,
  and the top one is always the application's (programming-model.md,
  4.5). The root composes the core, the application's connectors, the
  host inside the engine if it has one, and its own child domains. It
  alone faces the application's protocol layer.
- **The root routes and translates; it never decides or orders.**
  Everything that makes jig's promises true (what commits together, what
  waits for a commit, the order of a restart, the check on every effect)
  is decided by the core or kept by the journal. The root carries it, in
  a shape that is hard to get wrong.
- **One shape** (section 3): admission before any child changes; one
  decision gathered from every child the event reaches; at most one
  commit; outputs out only through the journal.
- **The journal is skein-lib's** (section 4): a generic container that
  numbers commits, holds outputs until their commit is durable, and
  releases them in order. It is the root's only way out.
- **The owner keeps the value** (section 7). The root puts each output
  together from its owners' parts in the step it is decided: an
  assignment from the core's part and the connectors' sections and
  items, a call's answer from the core's part and a connector's.
- **Boilerplate, not judgement.** What is left to the application is
  which child an event goes to and the small total functions between
  vocabularies. A mistake there is an ordinary bug, found by the
  application's stories; it cannot break a promise silently, because
  every write and every output passes through the journal, and jig's
  conformance world crashes the application's domain at every commit
  (testing.md, section 5).
- **Copied, not invented.** `ops`'s root is the reference every root is
  copied from (section 12).

## 2. What a root is

```
<application>-domain        the root: the journal, routing, translation, putting outputs together
├── core                    jig's core (engine.md)
├── <connector>…            the application's connectors (connectors.md)
├── local host              jig's host inside the engine, if the application runs agents there (hosts.md)
└── …                       the application's own child domains
```

- **Its state:** its children's states, the journal, and nothing else
  that outlives a step but what a child asked it to carry back (a token
  to resume). It has no records of its own in the store.
- **Its vocabulary** is what crosses the domain boundary: the store's
  commits and loads, the workers' channel, the clients' requests, each
  connector's protocol, the accounts' OAuth, timers. It is the union of
  its children's, wrapped: an exhaustive enum per direction, with one
  variant per child.
- **Its children are siblings,** sharing no types (programming-model.md,
  4.5). The core knows each connector by a number the root gives it in
  configuration; each connector knows nothing of the core.

## 3. The one shape

```
step(root, event):
    room = the worst case of this event's route                  (section 10)
    journal.takes(room)?  else: answer busy, through the door    ← before any child changes
    d = journal.decision()
    route the event to its child; while a child asks for a hand-off,
        route it to the child that answers, and its answer back   (section 5)
    every write any child asked for, wrapped    → d.write
    every held output, put together             → d.hold        (section 7)
    every output that decides nothing           → journal.now
    journal.accept(d)                           → at most one commit, to the store

iterate, after the step:
    store answers commit n  → journal.committed(n)
    store fails commit n    → journal.failed(n): nothing after it leaves; the engine stops
    journal.release(out)    → held outputs whose commit is durable, in order, within a bound;
                              those addressed to a child are fed back to it as events
```

- **No outbound queue.** The root's entry points take the journal and no
  other way out: what leaves the domain is what `iterate` takes from the
  journal. A root cannot send a held output early without writing code
  to get around the journal.
- **Every request says what it is.** The core and the connectors mark
  each request as part of the decision, held, or now (engine.md, 4.2).
  The root keeps the mark: a held output goes to `d.hold`, a now one to
  `journal.now`. The door for outputs that decide nothing is one call,
  so review finds every use.
- **Admission is all or nothing.** The room is reserved before the
  event reaches a child; an event the journal cannot take changes
  nothing, and its sender is answered busy, or keeps it, as engine.md,
  5.1 says.
- **One decision per event.** Hand-offs complete within the step;
  nothing of a decision is left for a later step but what a child holds
  in its own state, saying so (a brief gathering, a load awaited).
- **Released outputs addressed to a child** (the core's resumption after
  a claim commits, a connector's entry to make) are fed back to it as
  events by `iterate`, each as one more step of this shape.

## 4. The journal

The journal is a container of skein-lib's (skein's `lib.md`), generic
over the writes and outputs it holds, as lib's queue is over its items.
It inspects nothing it holds. What jig asks of it:

- **Admission by counts:** `takes(room)` answers whether a decision with
  at most so many writes and held outputs fits within the commits in
  flight, the writes per commit and the outputs held; nothing changes
  when it does not.
- **A decision:** a value, `#[must_use]` and not `Clone`, that gathers
  one decision's writes and held outputs, each refused with ownership
  returned past what it reserved; going past what was reserved is the
  root's bug, and fails stop.
- **At most one commit per decision,** numbered in order; a decision
  with no write makes no commit, and its held outputs wait for the last
  commit made.
- **Tags:** each held output carries the number of the commit it follows,
  so it is released only once that commit is durable.
- **The door:** `now(output)` leaves at once, in order with other now
  outputs, untagged.
- **Release in order,** a bounded number per call, so a step hearing the
  store does not release an unbounded group at once.
- **A failed commit:** nothing tagged with it or after it is released,
  and the journal reports stop.
- **Its worst case,** priced from its capacities, like every lib
  container.
- **The subset:** generic types and lib's own containers, no traits
  defined, no closures (programming-model.md, 10.2).

## 5. Routing

- **From the protocol layer:** each event goes to the child it is for:
  the store's pages and restores to the core or to the connector whose
  range it is; the workers' channel to the core, but for what a run's
  answer says it left on resources, which goes to the connectors that
  own them; a run's call to the core, or, for a connector's read or
  effect, to that connector first (section 9); clients' requests to the
  core, and those for an application's own objects to its children; a
  connector's system's events and answers to that connector; a timer to
  the child that armed it.
- **Hand-offs between children,** within one step: what the core asks of
  a connector (describe, judge, keep, drop, gather, cut, hand over, step,
  project, release, subscribe, restore, read afresh, settle) goes to the
  connector the core names by number, and its answer back; what a
  connector says up (a step's decision, news, an outcome, a pool's slots,
  drift) goes to the core.
- **To the host inside the engine:** the core's assignments, relays and
  cancels for runs placed on the engine's own slots go to the local host
  child, and what its runs say goes back to the core as a worker's would
  (hosts.md, section 5).
- **Nothing is routed by content** beyond the variant: which child an
  event is for is in its type.

## 6. Translation

- **Small total functions,** one per pair of vocabularies, matching
  exhaustively, with no `_` arm, so a change on either side breaks the
  build in one place.
- **The core's connector vocabulary** (engine.md, 4.1) is translated to
  each connector's own and back: a connector's number, its resources'
  names as paths, its effect descriptions, its verdicts, its news.
- **The store's vocabulary.** The root's `Key`, `Record`, `Write` and
  `Range` wrap the core's and each connector's: one variant per child.
  Its keys order first by child, then by the child's own order, so a
  child's ranges stay contiguous. The core's live families and each
  connector's are ranges of their own, which the restart script loads
  (section 8).
- **Configuration** is split the same way: the root gives each child its
  part, and the core each connector's number.

## 7. Putting outputs together

What leaves the engine is often made of parts that different children
own (core.md, section 5). The root puts each together in the step it is
decided:

- **An assignment:** the core's part (the run, its attempt, its charter,
  its core sections, its transcript, its calls) names the connectors'
  sections and workspace items by token; the root asks each connector to
  hand over what it keeps under those tokens, and wraps the whole in the
  application's assignment, which the journal holds until the claim
  commits.
- **A call's answer:** the core's part (status, the effect's key, why it
  was refused) and, for an effect made, the connector's part (what it
  made), kept under the call's key; the root asks again when the call is
  asked again.
- **A read's answer** is the connector's alone, and goes out through the
  door.
- **A connector's result in a dependent's brief** is a section like any
  other.
- **A token the core drops** (a brief abandoned, a call withdrawn) is
  dropped by its owner too: the root passes the drop on in the same step.

The parts are taken in the step the output is decided, so nothing waits
in the root between steps; each owner hands over its part once.

## 8. Restart, from the root

The core runs restart as a script (engine.md, section 6). The root:

1. loads the core's live ranges and restores them into the core, then
   says done;
2. for each connector the core names, asks it for its live ranges, loads
   them, restores them into it, then says done;
3. tells the core when the fleet's claims have been given, and passes on
   workers' hellos as they dial in;
4. asks each connector to read afresh, and says done when each has;
5. asks each connector to settle its outbox, and says done when each
   has;
6. lets the core open decisions.

The root does only the step it is asked for, and the core asks for the
next only once this one is done, so the order is the core's.

## 9. Effects and verdicts, from the root

- **From an agent's call:** the call reaches the root as the connector's
  typed request; the root gives it to the connector, which describes it
  and keeps it under a token; the description goes to the core; the
  core's check asks for verdicts, which the root routes to each judge it
  names and back; the core's answer (allow, propose, wait, refuse) goes
  to the connector, which keeps the effect under its key or the
  proposal's number, or drops it. All within one step, one decision.
- **From a procedure's step:** the step's decision comes up from the
  connector with its effects described; the root routes their checks as
  above, and the core's answers back to the connector, before the step's
  decision is complete.
- **Across connectors:** the judge of a requirement may be any connector;
  the root routes by the number the core names, never deciding which.
- **Made after the commit:** the request to make an outbox entry is a
  held output addressed to its connector, released once its commit is
  durable and fed back to the connector by `iterate`.

## 10. Worst cases and limits

- **Each child declares its worst case:** its limits, its memory, its
  `MAX_OUT`, and, per kind of event, the most it writes and outputs.
- **The root adds them up along each route** (programming-model.md, 4.5):
  the room an event needs is the sum over the children its route can
  reach, hand-offs included, which is what the root asks the journal for
  (section 3).
- **The root's own worst case** is the sum of its children's and the
  journal's.
- **The application's limits** are configuration, given to each child as
  its own; the conformance world runs every story under tiny limits.

## 11. What a root never does

- decide anything of the core's or a connector's: what to commit, what
  waits, whether an effect may be made, which judge to ask, the order of
  a restart;
- send an output any way but through the journal;
- hold anything between steps but its children and the journal;
- read or rewrite a value it carries between children, beyond wrapping
  it;
- give one child another child's type;
- route by content beyond the variant.

## 12. The reference root

`ops`'s root (`../examples.md`) is the reference: written to be copied,
commented as such. Its crate has one module per part of this document:

```
<application>-domain/src/
├── lib.rs          the tree, re-exports
├── boundary.rs     the root's vocabulary: events in, requests out, one variant per child
├── store.rs        Key, Record, Write, Range: the children's wrapped
├── domain.rs       Domain, step, fire, release: the one shape (section 3)
├── route.rs        which child each event and each hand-off goes to (section 5)
├── translate.rs    the small total functions (section 6)
├── assemble.rs     outputs put together from their owners' parts (section 7)
├── restart.rs      the script's steps, as the core asks (section 8)
├── limits.rs       the children's limits, and their sum (section 10)
└── tests.rs
```

A new application copies it, replaces `ops`'s connectors with its own,
and lets the compiler list the arms to fill.

## 13. The world

The root has no world of its own beyond its application's: the
application's worlds run its whole domain on jig's conformance world
(testing.md, section 5), whose referee checks, from outside, that the
root kept every promise: nothing a party, a host or a system saw depended
on a commit that was lost; every keyed effect made once across a crash
at every commit; no effect made without its verdicts; a restart's steps
in the core's order. Its own stories are the application's.

## 14. Open questions

- **Generating the boilerplate:** whether the root's routing and
  wrapping, which have one shape, are best copied and filled by agents,
  as now, or generated from a short description, which the subset's
  rules on macros would confine to a build step.
- **Several roots in one process:** an engine, a worker and agents in
  one process for tests and small deployments (smith's `host.md`,
  section 9) composes several roots under one more; whether that top is
  jig's or the application's.
