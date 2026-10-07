# jig's design

Provisional, 2026-10-06. What jig is, how an application is built around
it, and what keeps the application's wiring from breaking what jig
promises. This document comes first. The documents it lists in section 12
go deeper into each part. The mechanics are those of skein's
`programming-model.md`. What is still open is listed in section 14.

## 1. In one page

- **A kit for agentic applications.** jig runs work done by agents,
  procedures and people, keeps that work in its own store, and drives
  external systems through connectors. It aims to be as flexible as a
  coding agent at a terminal, and adds what such an agent lacks:
  - work that outlives processes and machines;
  - many agents at once;
  - rules no agent can loosen;
  - effects on other systems made once.
- **Five primitives** (`domain/core.md`):
  - the **task**: what is asked;
  - the **executor**: who does it, which is an agent, a procedure or a
    person;
  - the **message**: everything a task hears once it has started;
  - **authority**: what a task may do, narrowed whenever it delegates;
  - the **connector**: an external system.

  Everything else is built from them: plans, coordinators, chats, goals,
  reviews.
- **The application owns the root.** skein's model nests step machines
  inside one another, and the top one is always the application's. The
  application's root domain composes jig's core with the application's
  connectors and its own domain logic, and translates between them
  (programming-model.md, 4.5). Extension is composition: no traits, no
  generic items, no callbacks. That is what makes jig a kit and not a
  framework.
- **jig gives a mid-level core.** The core is the parent of tasks,
  authority, people, the fleet, accounts, briefs, notes and views. It
  routes among them and keeps the engine's numbers, its runs and their
  calls, and the order of restart. The root's code grows with the
  application's connectors, not with jig's internals.
- **Rails, not boilerplate** (section 6). The root routes and translates;
  it never decides or orders. The rails that keep this true:
  - the commit barrier is a generic container in skein-lib, the
    journal, and it is the root's only way out;
  - the core runs restart and effects as a script;
  - every part declares its worst cases, and the journal asserts them;
  - a conformance world crashes the application's domain at every
    commit;
  - a reference root is the template every application copies.
- **Connectors meet the core through one vocabulary** (section 7):
  resources as paths, effects keyed, topics, procedures' steps,
  requirements as verdicts. A connector keeps its own state per task,
  keyed by the task's number. The core never sees a connector's types.
- **The store, the hosts and the client follow the same pattern.**
  - The store holds jig's records and the application's, in one commit.
  - Agents are optional. An application may have none, run them inside
    its engine, or run them on workers, in workspaces of its own kinds.
  - The client domain covers the primitives, and the application adds
    its own kinds of object. It runs on any platform.

  In each case the application's root composes jig's part with its own,
  in its domain and in its protocol layer.
- **The core never depends on smith.** Only jig's agent hosts, and the
  client's view of conversations, link smith. An application without
  agents never does.
- **Flexibility and orthogonality over convenience.** An application
  writes some predictable code to wire jig in. Agents write such code
  well, and getting it wrong cannot break a promise silently.

## 2. What jig is for

- **What an application gets** is what is hard to get right, held to the
  promises of `domain/core.md`:
  - authority holds;
  - every effect made once, as far as its system allows: keyed and
    conditional effects at most once, others held for a person when
    their outcome is uncertain;
  - recorded spend never passes a budget;
  - nothing leaves the engine before the commit it follows from;
  - order: dependencies first, one writer per resource;
  - nothing is lost, nothing is written over;
  - what the engine holds is bounded.

  jig's referee checks these promises in the application's own worlds,
  with the application's wiring and its systems' fakes (6.6).
- **What an application brings:**
  - its connectors: the systems it drives, their procedures, the reads
    they give agents;
  - its charters: what its agents are told to be;
  - its policy;
  - its client's own objects.

  For example, temper brings a forge connector whose procedure lands
  changes, and charters for coders and reviewers.
- **What jig is not:**
  - a workflow catalog: structure is the work's, made by agents with
    tools;
  - an agent: that is smith;
  - io: that is skein.

## 3. The system

```
people    a client: the application's, around jig's client domain
engine    where work is decided: the application's process, around jig's core
store     skein-kv inside the engine: jig's records and the application's, committed together
workers   if any: execution hosts, the application's process, around jig's worker host
agents    if any: smith's, inside the engine or one run per agent process on a worker
systems   what the application's connectors drive
```

- **One engine per deployment.** It is the store's only writer and the
  only client of every connector's system (`domain/core.md`).
- **The engine decides, hosts host, agents think.** A host is the engine
  itself or a worker that dials it (section 9). Agents reach the engine
  only through tools their host relays.
- **Every process is the application's.** The application writes the
  root, the protocol layer, `iterate` and `main`, as with any service on
  skein. jig supplies the parts beneath its roots, and the pieces of its
  protocol layers that speak jig's vocabularies: the store's encoding of
  jig's records, the engine's side of the workers' channel when it has
  workers, and the client's wire documents.

## 4. The application owns the root

```
application root    routing between its children, translation; its store, channel and web vocabularies
├── core            jig: tasks, authority, people, fleet, accounts, brief, notes, views, and their routing
├── <connector>     the application's: one subtree per system it drives
├── …
└── …               the application's own child domains, if it has any
```

- **Control.** The application chooses its connectors, what it adds
  around the core, its limits, and its entrances. The core is the top of
  a subtree of its own, as a connector is.
- **Composition only.** Step code defines no traits or generic items, and
  uses no `dyn` or closures (programming-model.md, 10.3). The root
  therefore matches on what the core and each connector ask for, and
  calls the one each request goes to.
- **Change arrives on purpose.** Matches are exhaustive, with no `_`
  arm, so a new variant in jig breaks every application's build in one
  place. A change in jig reaches an application through a new pin, and
  the compiler lists what the application must decide.
- **What the core never knows:** any connector's types, the application's
  records, and any wire format.
- **An escape hatch.** The core's children are public crates. An
  application may compose them without the core. If it does, it takes on
  the invariants the core keeps, and the conformance world (6.6) still
  applies to it.

## 5. The core

- **What it holds:**
  - the routing among its children, for what arrives: a run's turn,
    call or answer; a person's request; a task due; a timer;
  - the deployment's numbers;
  - the runs in flight, and the calls committed for each since its last
    turn;
  - the gathering of a run's brief;
  - the restart script (6.4).
- **Its vocabulary toward the root:**
  - the store: its own records, saved, erased, loaded and restored, as
    every child's are (`domain/engine.md`);
  - agents' hosts: the workers' channel, the engine's side, or the host
    inside the engine (section 9);
  - people's requests, and the replies to them;
  - LLM accounts' refreshes;
  - the connectors' vocabulary (section 7).
- **Connectors are numbered.** The core knows from its configuration that
  connector `k` exists and that its resources are paths under its
  prefix, and nothing more.
- **Its children** are each a child domain of their own, with their own
  vocabulary, limits and worlds:
  - **tasks**, the hub: batches, lifecycle, inboxes, wakes, proposals;
  - **authority**: policy as data, checks and budgets;
  - **people**: identities, roles, requests, inboxes, chats;
  - **fleet**: agents' hosts, slots, placement, attempts;
  - **accounts**: the LLM credentials;
  - **brief**: a run's context, in typed sections within a byte budget;
  - **notes**: what agents learn, in scopes;
  - **views**: live streams to watchers.
- **Agents are optional.** An application without them configures no
  charters. The batch check then refuses tasks with agent executors, and
  the fleet and accounts stay idle.

## 6. The rails

### 6.1 The barrier

A child domain decides immediately. Its state changes as it decides, and
it emits its writes and its outward requests at once. It never waits for
durability; durability is its parent's job. For each call to its entry
point, the root does these things:

1. **Admission:** before any child changes, it checks that there is room
   for one more commit. If there is not, it answers busy.
2. **Gathering:** every write from every child the event touched goes
   into one numbered commit.
3. **Holding:** every output that someone outside could act on is tagged
   with that commit. That covers an effect, an assignment, an
   acknowledgement, and an answer to a person or to a tool call. The
   output is held until the store says the commit is durable.
4. **Releasing:** held outputs leave in their order. After a failed
   commit, nothing leaves, and the engine stops.

The order of a restart, and the authority check on every effect, belong
to the same set of duties.

Together, these make "once" and "nothing ahead of its commit" true.
Connectors are the application's children, so their writes and outputs
pass through the application's root, and that root is on this path
whatever jig does. Its mistakes pass every test of expected behaviour and
show only when the engine crashes:

- **An output that leaves early:** an effect made before the commit of
  its outbox entry, made but never recorded;
- **A decision split in two:** a call's record in one commit and its
  outbox entry in the next;
- **Admission after a change:** a connector changed, then no room found
  to commit what it changed;
- **Restart out of order:** the core deciding before a connector has
  restored its outbox, and asking again for an effect already in flight;
- **An effect that skips authority:** a procedure's effect reaching its
  connector's outbox without the core's check.

The rails below make each of these hard to write, loud when written, and
caught by a test.

### 6.2 The journal, in skein-lib

The barrier cannot live in the core: the core may not hold the
application's records and outputs, since its code may not be generic.
The barrier is not policy either. It is a data structure, as a queue is.
So it goes into skein-lib, generic over the writes and outputs it holds,
as lib's containers are generic over what they hold. programming-model.md,
10.2 allows exactly this: lib may use generic types, and application code
does not hand-roll data structures.

- **What it does:**
  - **admission by counts:** commits in flight, writes per commit,
    outputs held;
  - **a decision:** a value that gathers one decision's writes and
    outputs; it is `#[must_use]` and not `Clone`;
  - **at most one numbered commit per decision;**
  - **tags:** each output carries the commit it follows, or the last
    commit made if the decision made none;
  - **release:** outputs leave in order as the store answers, within a
    bound per step;
  - **a failed commit:** nothing after it is released, and the journal
    says stop;
  - **its worst case,** priced like every lib container.
- **What it does not do:** inspect what it holds. Whoever owns the bytes
  checks byte bounds before handing them over. The deployment's record
  and numbers are the core's.
- **It keeps to the subset.** It uses generic types and lib's own
  containers, and nothing else of what lib alone may use. It defines no
  traits and takes no closures. Its design is a section of skein's
  `lib.md`.

### 6.3 One shape for every root

The root's entry points have no outbound queue of their own. What leaves
the domain is taken from the journal by the service's `iterate`, so the
journal is the only way out:

```
step(root, event):
    journal.takes(room for this event)?  else: busy     ← before any child changes
    d = journal.decision()
    route the event through the children: each write and each outward request goes into d
    journal.accept(d)                                   → at most one commit out
store answers n → journal.committed(n) → the outputs held for n leave, in order
```

Outputs that decide nothing leave through one explicit door of the
journal, untagged, so review finds every use. These are facts streamed
to watchers and answers to a run's read tools.

### 6.4 Ordering belongs to the core

- **Restart is a script.** The core asks the root for one step at a time:
  1. the core's own live ranges, loaded;
  2. connector `k` restored: the root asks the connector for its live
     ranges, loads them and feeds them back;
  3. runs adopted, as workers dial in;
  4. connectors reading afresh what their systems own of the live work;
  5. the outbox settled: every entry that may have been made is looked
     for by its key before it is made again;
  6. decisions open.

  The root does the step it is asked for, and the core asks for the next
  one only after hearing that this one is done.
- **Effects follow one loop:**
  1. a connector proposes an effect, from a procedure's step or from an
     agent's call;
  2. the core admits it against authority;
  3. the connector records the outbox entry, in the same decision;
  4. the connector makes the effect only when the journal releases it.
- **Write holds:** the core keeps holds and writer slots, in tasks. A
  connector tells the core when it has read a resource afresh after a
  lost attempt.

### 6.5 Mistakes are loud

- **Worst cases are declared.** Each child declares the most it writes
  and outputs per event, and the root adds them up along the route
  (programming-model.md, 4.5).
- **The journal asserts.** A decision that exceeds what it reserved stops
  the engine (fail-stop), rather than leaving state ahead of the store.

### 6.6 Mistakes are caught: the conformance world

- **What jig ships:**
  - the fakes for its own peers: the store on skein-kv's in-memory mode,
    scripted workers and scripted people;
  - the referee for `domain/core.md`'s promises;
  - the faults: crashes at every commit, commits held, commits failed.
- **What the application adds:** its whole domain and the fakes of its
  systems.
- **Generic in the world, concrete in the domain.** A world is ordinary
  Rust (programming-model.md, 10.2), so the conformance world may be
  generic over the application. The application's domain stays concrete.
- **The referee looks from outside.** Before a fake system saw any
  effect, that effect's record was durable. No run was assigned without
  a durable claim.No person was answered ahead of their commit. Every
  effect was made as its recovery class allows.

### 6.7 A reference root

jig's example application, `ops` (`examples.md`), is a small system for
running services in production. It is chosen because it shares none of
temper's shapes of work:
- two connectors, observability and infrastructure;
- a stream of events, mostly noise;
- effects on shared and scarce resources that cost money;
- its agents inside its engine.

It is complete: a root, its connectors, their fake, a client and worlds.
Its root is the template every application copies. With
exhaustive matches and one shape (6.3), roots look alike, and agents fill
in their arms.

### 6.8 What is left to the application

Which child an event goes to, and the small total functions between
vocabularies. A mistake there is an ordinary bug in the application, and
its own stories find it. It cannot break jig's promises silently,
because every write and every output still passes through the journal,
and the conformance world crashes the application at every commit.

## 7. Connectors

- **The vocabulary between the root and a connector is jig's API**
  (`domain/connectors.md`):
  - resources named as paths of bytes, with roles: owned, participating,
    context;
  - write holds, and one writer at a time;
  - effects keyed per connector, made from the outbox, with uncertainty
    after a failure or a restart;
  - effect and read kinds as numbers, whose order authority holds as
    data;
  - topics, and news for subscribers, classified as waking, kept or
    dropped;
  - procedures' steps and their decisions;
  - projections: what a connector writes for people to read;
  - brief sections and reads for agents;
  - workspaces: what a worker prepares for a run (section 9);
  - drift, and adoption.
- **Requirements are verdicts.** Facts such as CI at a head or a
  reviewer's approval are a connector's. The connector that makes an
  effect judges its own facts and gives a verdict: met, wait, or refuse
  with a reason. The core holds one rule: no effect without its verdict.
- **What the core does not act on, it does not hold.** A connector keeps
  its own state per task, keyed by the task's number, and the root joins
  the two. What the core orders but does not read (a brief section, a
  read's answer, a connector's result) it names by token and size, and
  the root puts outputs together from their owners' parts
  (`domain/core.md`, section 5).
- **The set is closed in each application.** The application's root
  knows every connector and matches on them exhaustively. Nothing below
  the root knows any connector but itself.
- **A missing place in the vocabulary is jig's to add,** since no root
  can add one. A second application tests the vocabulary for that reason.

## 8. The store

- **jig's part:**
  - the core's records and keys;
  - their encoding, ordered as the keys are and versioned;
  - transcripts as files, per session;
  - the secret records: sign-ins, refresh tokens.
- **The application's part:**
  - its connectors' records: outboxes, procedures' states, what was made
    by key, and each connector's own state;
  - the root's keys, records and writes, which wrap both jig's records
    and its own;
  - the store's protocol layer, which composes jig's encoding with its
    own.
- **One commit spans both,** through the journal. The live records of
  both are loaded by the core's restart script (6.4). skein-kv is
  underneath, inside the engine (`domain/engine.md`, section 5).

## 9. Agents and their hosts

- **smith is the agent.** Nothing in smith assumes code, repositories or
  a workspace, so the same agent serves every application, told apart by
  its charters.
- **The core never depends on smith.** It keeps turns opaque, and reads
  only a turn's size, its spend and the last message it read. Charters
  are put into smith's form by the protocol layer, not by the domain.
- **What the core needs is a host for each run, not a worker.** The
  core's contract with a run is:
  - a start: the charter, the brief, the transcript, credential grants,
    an attempt number;
  - turns sent up, committed and acknowledged;
  - calls relayed and answered once, and messages relayed;
  - an answer at the end;
  - cancellation, and graces for a host that is lost.

  smith's host contract carries the same, as frames over a channel or as
  calls inside one process (smith's `host.md`, sections 3 and 9).
- **Three shapes of application:**

  ```
  no agents           procedures and people only             no host, no smith, no LLM accounts
  agents in-engine    smith's domain inside the engine       no workspace; tools are host tools only
  agents on workers   worker processes, agent processes      workspaces, commands, many machines
  ```

  1. **No agents,** such as an approval or operations pipeline. Nothing
     links smith (section 5).
  2. **Agents in the engine,** such as a researcher, a triager or a chat
     assistant, whose tools are the engine's tools and the connectors'
     reads. The engine's root composes smith's domain in its one-process
     form, and the fleet places runs on the engine's own slots. The costs:
     - no process containment, which is fine when nothing runs
       commands;
     - the engine's worst case includes its runs;
     - LLM traffic goes through the engine's protocol layer, on the
       engine's loop.
  3. **Agents on workers,** when runs execute commands, hold workspaces,
     or need many machines, as temper's do. Each run is its own agent
     process, so stopping a run ends its process tree.

  An application may mix the second and third shapes: a charter says
  which hosts may run it.
- **jig's worker host,** for the third shape:
  - slots, attempts, turns kept until acknowledged, and graces;
  - agent processes, supervised through smith's host domain;
  - the worker's side of the channel to the engine.
- **Workspaces are the application's.** A connector says which of its
  resources a run needs prepared. A connector whose resources are not
  files gives a run tools instead. The worker's root is the
  application's: it composes jig's worker host with the application's
  kinds of workspace (for temper, git checkouts).
- **Host tools:** the engine's tools are jig's, and each connector's
  reads are declared by the application.

## 10. The client

- **jig's part:**
  - the client domain over the primitives: tasks as a tree,
    conversations, a person's inbox, proposals, escalations, questions,
    person tasks, results and live runs, with the pending keyed requests,
    pages and streams;
  - the wire documents for these;
  - their views, conversations among them, rendered with smith's
    transcript crate, which is the client's only use of smith;
  - shells: the browser's, and a native one on skein's HTTP client for
    Linux and the worlds;
  - the fake person, who reads the view tree by role, name and text.
- **The application's part:**
  - its own kinds of object, as children of its client root;
  - their wire documents and their views.

  The client root composes these with jig's client domain, under the
  same rule as the engine's: the root routes, and jig's client domain
  keeps the invariants of pending requests and pages.
- **Any client, on any platform:** a browser through wasm32, a phone, a
  desktop application on Linux, macOS or Windows, a command-line tool, or
  another service.
  - **Only the shell is the platform's.** The wire, the protocol layer,
    the domain and the view are step crates, built for the host and for
    wasm32.
  - **A shell can sit on any HTTP stack.** The client's protocol layer
    sits above HTTP: it gives the shell a method, a path and a body, and
    gets back whole bodies and whole stream events. A shell may use the
    browser's fetch, the operating system's own HTTP stack, or skein's
    HTTP client, whose io is Linux's. Each shell is small.
  - **The view is optional.** The view tree suits graphical clients. A
    command-line tool may want a text view of its own. A service may use
    the domain and the protocol only: keyed requests sent again until
    durable, streams and paging.
- **Clients that are not people.** Every party today is a person who
  signed in, with a role in a project. A service acting through a client
  needs an identity and a role too: a service is a party of its own kind
  (`domain/people.md`, section 3).
- **The web protocol** carries jig's documents and the application's
  over one connection, composed in each end's protocol layer. Sign-in
  providers are the application's.

## 11. Building an application on jig

| Part | Written by |
|---|---|
| the engine's root domain: routing, translations, its vocabularies | the application, copied from the reference root |
| the core and its children | jig |
| connectors: their domains, protocol layers and fakes | the application |
| the journal | skein (skein-lib) |
| the store's encoding: of jig's records / of the application's, and their composition | jig / the application |
| the engine's protocol layer, `iterate` and `main` | the application, from jig's and skein's pieces |
| agents' hosts: the worker host, the in-engine host / the worker's root, kinds of workspace, `main` (none without agents) | jig / the application |
| the agent / its charters | smith / the application |
| the client domain, views, wire and shells / its own objects and its client root | jig / the application |
| the conformance world, the referee, scripted workers and people, the fake person / its worlds and its systems' fakes | jig / the application |

Before code, an application writes its connectors' vocabularies and
limits, and the worst cases of its root. Then, in order:

1. its connectors' domains;
2. its root;
3. its worlds, on jig's conformance world;
4. its protocol layers, its `iterate` and `main`.

What it finds missing in jig along the way is added to jig, with the
application as its first user.

## 12. Documents

- **`domain/`:** jig's domain layer: the model, tasks, authority,
  connectors, the core, the application's root, parties, hosts, and
  testing. Its `README.md` gives the reading order and the names, and
  the decisions it took on what this document left open.
- **`client/domain/`:** the client's domain, its views and its wire, to
  be designed.
- **examples.md:** the example application, `ops`, and the reference
  root.
- **Below the domain,** to come with the protocol layers: the store's
  encoding of jig's records, the workers' channel, the client's protocol.

## 13. Conventions

- **A bare file name** names a document in this directory;
  `domain/core.md` names one of the domain's.
- **skein's foundation documents** are named by their own file names:
  `programming-model.md`, `testing-strategy.md`, `notes.md`. skein's
  design documents are named with skein's name: skein's `lib.md`.
- **smith's documents** are named with smith's name: smith's `run.md`.
- **An application** appears only as an example, and says so.
- **Each document** says in one page what it is. Then it gives its
  structure, its parts, what it owes and is owed below the domain, its
  world, and what is open.

## 14. Open questions

The domain's design took decisions on most of what this document left
open (`domain/README.md`, section 5). What remains here:

- **The client's pages:** how jig's pages show an application's objects,
  for example as one kind of card per family (`client/domain/`).
- **Versioning:** how jig is released and pinned, and how a change to its
  vocabulary reaches every application.
