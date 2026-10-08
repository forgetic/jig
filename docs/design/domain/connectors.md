# Connectors

Provisional, 2026-10-07. The contract an external system meets to be
driven by an application on jig: what a connector is, what crosses
between it and the core through the application's root, and what every
connector does: resources and holds, effects and the outbox, facts and
topics, procedures, requirements' verdicts, projections, reads and brief
sections, workspaces, drift and adoption. It is the fifth primitive of
core.md (3.5) in depth, and jig's API toward applications: connectors
are the application's, and this is what each owes and is owed. What is
still open is listed in section 16.

## 1. In one page

- **A connector is a subtree of the application's engine domain,** a
  child of its root, in a closed set the root matches on. It knows its
  own system and nothing of the others, nor of the core's types; the
  core knows it by number (core.md, 3.5).
- **One vocabulary crosses** (section 2), the same for every connector,
  in jig's terms on the core's side: resource names, effect
  descriptions, verdicts, topics' news, procedure steps. The root
  translates each connector's vocabulary to it and back.
- **The owner keeps the value** (core.md, section 5). A connector keeps
  what is in its own terms (an effect's typed request, a procedure's
  parameters and state, a brief section, a read's answer, a result)
  under a task's number, a call's name or a token, and hands it over when
  the root puts an output together.
- **Resources** are named as paths, which authority's grants cover by
  pattern. A connector says which kinds take holds, exclusive or pooled,
  and whether a taken hold refuses or waits.
- **Effects** are committed into the outbox with the decision that asks
  for them, and made after the commit, keyed. Each kind declares a
  recovery class, which says how an effect whose outcome is uncertain is
  resolved and how strong its promise of once is (4.3). A connector
  describes each effect for authority, priced at its maximum when it
  costs.
- **Facts are read afresh; events are hints.** A connector keeps a
  working set of what its system owns of the live work, and publishes
  what changed on topics, classified for each subscriber.
- **Procedures are the mechanics,** as executors of tasks: pure step
  machines, level-triggered, their state committed with each step.
- **Requirements are judged by their connector** (section 7): asked
  about an effect's exact state, it answers met, wait or refuse from its
  own facts, whichever connector makes the effect. A guarded requirement
  is checked again by the effect's own system as it applies the effect;
  an observed one holds only when the effect is decided, within its
  freshness.
- **What the application does there belongs to it.** A connector records
  what it made by key, writes only to what is owned (or as a
  participant, or through its own mechanics into what is shared), and
  treats any other change to what it relies on as drift.
- **Adopted as it is.** A system is plugged in with whatever access the
  application is given; nothing on it is required.

## 2. Where a connector sits

```
application root            routes between the core and every connector
├── core                    jig's core
└── <connector>             a connector's top: its resources, holds' kinds, topics, procedures' routing
    ├── …                   its own children, as its system needs
```

- **A connector's top is a child of the root,** and may have children of
  its own (programming-model.md, 4.5). Its top routes between them, as
  the root does between the core and the connectors.
- **What crosses** between the root and a connector, in the connector's
  vocabulary, translated by the root to and from the core's:

  | Down, root to connector | Up, connector to root |
  |---|---|
  | a live task names these resources; it no longer does | each resource's role and kind of hold; unknown; not adopted |
  | holds taken, handed down or released; a writer slot taken or freed | a pool's slots changed; a resource read afresh after a lost attempt |
  | describe this effect, from an agent's call or a step | its description: kind, resources, purpose, condition, form, price |
  | keep this effect under this key, or under this proposal; drop it | — (part of the decision) |
  | make this outbox entry (the journal released it) | made, with what it made; failed; uncertain |
  | judge this requirement for this effect's state | met; wait, for what; refuse, on which fact |
  | a procedure task: made with its parameters; activated; a message; closing | a step's decision: effects, batches, amendments, proposals, messages up, a hold, a result |
  | project this goal: its state, its plan, its milestones | the projection's effects |
  | release what this closing task held | the release's effects |
  | subscribe this task to this topic; unsubscribe | news for these subscribers, classified |
  | serve this read for a run | its answer |
  | gather this section for a brief, within this budget; cut it to a size; hand it over | ready, with its size; the section |
  | name what a run of this task needs prepared; hand it over | its workspace items, with their size; the items |
  | what a run of this task left on its resources | taken: facts updated, procedures stepped |
  | adopt this resource into this project | adopted, with what may be done there and the parties it knows; refused |
  | restart: restore these records; read afresh; settle the outbox | done |

- **Tasks are named by number.** The connector keeps the numbers of the
  tasks it serves as plain values; it never sees a task. What it projects
  of a goal, the root gives it, in the connector's own terms.
- **Its protocol is its own.** The root passes its requests down to the
  application's protocol layer and their answers up; its system's hints
  (webhooks, a stream of alerts) arrive as events no request caused.
- **Its store records are its own,** saved and erased as part of the
  decisions it takes part in, restored at a restart (engine.md, 5.6).

## 3. Resources

### 3.1 Names

- **A path of segments,** the connector's choice, stable for the
  resource's life: for `ops`'s infrastructure, `env`, the environment,
  then `service` and its name, or `pool` and its name. Names are bytes,
  compared segment by segment, and every name carries its connector's
  number.
- **Patterns** cover what begins with them, which is how authority grants
  effects on many resources at once (authority.md, section 4).
- **Kinds of effect and read** are the connector's, numbered, each with
  the resource kinds it applies to, and the order among them that
  authority holds as data.
- **Names after a task:** a connector may define resources named after
  the task that makes them, under a prefix of the deployment's, which a
  batch names symbolically (authority.md, section 4).

### 3.2 Roles

Each resource a project adopts has a role (core.md, 6.1): owned, or
adopted to write into; a fork; context. Each object the application
deals with is owned or participating (core.md, 8.2). A connector keeps
both with the project, and every effect is checked against them before
any grant: no effect writes to a context resource, and an effect on a
participating object is a participant's write.

### 3.3 Holds

The core keeps holds and their queues (tasks.md, section 6); the
connector says what they apply to:

- **which kinds of resource take holds,** and whether each is
  *exclusive* (one holder: a service being remediated, a branch being
  written) or *pooled* (counted slots: a pool's environments);
- **for a pooled resource, its slots,** from its facts or its
  configuration, told to the core whenever they change. A pool that
  shrinks below its holders keeps them, and the connector says which
  holder's allocation is gone, which is drift (tasks.md, 6.6);
- **whether a taken hold refuses or waits** (tasks.md, 6.2);
- **what a release does** to the resource, per way its task ended (4.6).

Shared resources take no hold: writing into them is ordered by the
connector's own mechanics, such as a landing queue.

### 3.4 Live resources

The resources live tasks name are the connector's live resources: what
its working set holds (section 5) and what it reads afresh at a restart.
When no live task names a resource, it leaves the working set.

## 4. Effects and the outbox

### 4.1 Effects

An effect is a typed request the connector defines, in its own terms,
which it keeps. What the core gets is its **description:**

- **its kind and its resources,** for grants and roles;
- **its purpose,** from which its key is derived (4.4);
- **its condition,** where it has one: the state it was decided against
  that the system can check as it applies it (a rollback only from the
  version decided on, a merge at exactly a head);
- **its form:** a **creation** (an environment, a pull request, a
  comment), a **transition** (a rollback, a merge, a scale to a size), or
  a **set** (a body, a label, a silence's end);
- **its recovery class,** which its kind declares (4.3);
- **its price,** if its kind is priced: its maximum, in the deployment's
  unit (authority.md, section 7);
- **the state its requirements are judged at:** what the judges are
  asked about (section 7).

### 4.2 The outbox

- **An entry per effect,** committed with the decision that asks for it.
  Its connector holds the entries not yet made, which the store keeps
  too, as the connector's records.
- **Made after the commit,** when the journal releases the request to
  make it: in commit order per resource and one at a time on each, in
  parallel across resources within the connector's limits and request
  budget.
- **Each attempt is committed before it is sent,** with its absolute
  retry deadline (4.3), so no restart forgets when an attempt went out.
- **Its outcome is committed in turn:** made, with what it made; failed,
  typed; or uncertain. A task closing waits until every entry it asked
  for has settled (tasks.md, 5.1).
- **Retries are the connector's:** a transient failure is tried again
  after a backoff, within its budget; a failure for good (forbidden, a
  conflict, the object gone) is news for the task that asked, and holds
  it if it was closing. A projection's or a release's failure for good is
  news for the goal, or the closing task, it was made for.
- **Withdrawn** when the task that asked for an entry is cancelled before
  the entry went out. An entry that went out is never withdrawn: what may
  have been made counts.

### 4.3 Uncertainty and recovery classes

- **Only what provably never reached the system did nothing.** Any other
  failure (a timeout, a reset after sending, an answer that does not
  decode) is uncertain: the write may still take effect, even after the
  connector has tried again.
- **Each kind of effect declares its recovery class,** from what its
  system offers, and the connector's world shows the class holds
  (section 15):
  - *keyed:* the system keeps the effect's key, or an operation's id,
    and refuses or returns a second request with the same one. An
    uncertain entry is looked for by its key, and tried again with the
    same key if not found; a late copy finds the first. **At most
    once.**
  - *conditional:* the system applies the effect only from the state it
    was decided against (a rollback only from the version decided on, a
    merge only at the head decided on), and the state it leads to can be
    told from any other's, from what the system keeps, which no other
    hand can erase (a pull request closed is still found; a branch
    deleted is not). An uncertain entry is looked for by that
    state, and tried again under the same condition; a late copy fails
    its condition. **At most once in effect.**
  - *idempotent:* a set, which may be applied twice to the same final
    state. It is written again. Where the system offers a version to
    check, the set carries it, so a late copy cannot land over a newer
    write; where it does not, the kind says a late copy may.
    **The same final state, possibly more than once.**
  - *unrecoverable:* none of the above, such as a restart through an
    interface that gives no operation's id. An uncertain entry is never
    tried again by the engine: its task is held, saying so, for a person
    to decide whether to make it again (tasks.md, 5.5). **At most once,
    by holding.**
- **Retry deadlines are absolute.** Each attempt to make an entry is
  committed, with its deadline (the wall time it is sent, plus the
  system's lifetime for a write, plus a margin for the clock), before it
  is sent. An uncertain entry that was not found is tried again once its
  deadline has passed, and only by committing a new attempt. A restart
  keeps the deadlines it loads, rather than counting a lifetime from its
  own start, so restarting never postpones an entry for ever. A clock
  that moved back only delays a retry; the margin covers a clock that
  moved forward by as much as the deployment configures.
- **A copy lands within its write's lifetime, or never.** Every class's
  promise rests on it: an attempt is retried only once the lifetime of
  the one before has passed, so no copy of an earlier attempt lands after
  a later one is sent. A fake system that delivers copies later breaks
  this assumption, not the connector. A system whose writes cannot be
  bounded so has no class but unrecoverable.

### 4.4 Keys

- **Committed with the entry,** and derived from what the effect is for,
  never from when it was asked, so asking again for the same thing finds
  the same key:
  - an agent's effect: its task, its attempt and the call that asked
    (engine.md, 7.3);
  - a procedure's: its task and the purpose within it (provisioning its
    environment; rolling back to a version; merging at a head);
  - a projection's: its goal and what it writes;
  - a release's: the closing task and the resource.
- **The deployment's,** too: every key carries the deployment's
  identity, so two deployments on one system, or a store made anew, never
  find each other's objects. A key the connector writes itself (a marker
  in a body, an operation's id) carries the deployment's id. A key that
  is a name the system keeps (a branch, an environment's name) carries
  the deployment's prefix (section 12), which is the deployment's alone
  on that system, so the name stays readable and the grants that name it
  by pattern (authority.md, section 4) stay as they are.
- **Carried on what it creates,** where the system keeps it, so that a
  creation can be found by its key alone. The connector records every
  object it made, by key, in the store: the ownership of section 11.

### 4.5 Effects asked by agents

An agent asks for an effect with its `effect` tool (engine.md, 7.3),
naming one of the kinds the connector offers to agents. Its input
reaches the root decoded, as the connector's typed request; the
connector describes it; the core checks it (authority.md, 8.2); and, in
the same decision, the connector keeps it under its key, or under the
proposal's number if it was proposed, or drops it. A check that answers
wait commits nothing: the call is answered so, and the run may ask again
or delegate to a procedure that waits. What the connector's procedures
own (provisioning, remediation, a change's way to landing) is not
offered to agents: a run that wants it done delegates it to the
procedure.

### 4.6 Releases

When a task that held resources closes, its connector releases them
with effects of its own, after every other effect of the task has
settled (tasks.md, 6.5): per kind of resource and per way the task
ended, what becomes of it. An environment a task provisioned is torn
down; a branch a landed change was pushed to is deleted; a failed one's
is kept for a person to read until its tree's root closes. A release's
effects are keyed by the task and the resource, and its failure for good
is news for the closing task.

## 5. Facts and events

### 5.1 The working set

- **What the system owns of the live work:** for `ops`'s observability,
  the services' health and error rates; for its infrastructure, the
  services' versions and replicas, the environments' states, the pools'
  slots; for a forge, pull requests' heads and CI on them. A working set,
  not a copy: it grows with the live resources, never with the system's
  history.
- **Kept up** by reading what changed, sooner when a hint arrives, with
  polling as the backstop, at a cost that follows the rate of change.
- **Read afresh** before a decision depends on a fact: a procedure's step
  reads what its decision needs, and a judge reads what its verdict
  needs (section 7).

### 5.2 Events

- **Hints, never state.** A webhook, an alert, or a change keeping up
  found, tells the connector that something may have changed; it reads
  what it needs and updates its working set.
- **Echoes dropped.** What the application itself did comes back as
  events too; the connector drops those it can tell are its own (an
  effect it made, by its key; a run's write, by its writer slot,
  section 11) and reads the rest.
- **Then three things follow:** procedures whose facts changed step;
  holds whose resources drifted are checked (section 11); news is
  published on the topics it touches.

### 5.3 Topics

- **A topic is a subject a connector names:** a service's alerts, its
  health, an environment's state, the landings on a branch.
- **Subscribers are tasks,** by number, and their number per topic is
  bounded. A task subscribes through its tool, through its creator, or
  through the core (a goal's topics, 5.4).
- **Each event is classified per subscriber:** *wakes*, *kept* or
  *dropped*, by the connector's judgement and without an LLM: an alert
  on a service a watch covers wakes it; one it already has open news of
  merges; one below the watch's threshold is kept. The class travels
  with the news into the task's inbox, whose wake policy may lower it,
  never raise it (tasks.md, 8.3).

### 5.4 Goals' topics

A connector may offer topics for goals: what a goal's subtree should
hear of the resources it works on, such as the landings that overlap a
goal's changes, or the alerts on the services an incident's
remediations touched. The core subscribes each goal to them as its
subtree names resources (core.md, 6.2).

## 6. Procedures

### 6.1 What a procedure is

A step machine a connector owns, one per task whose executor it is:
remediate, provision, a change, a standing watch. Its parameters come
with the batch that makes its task, in the connector's terms, and the
connector keeps them under the task's number. Its state is a plain
value, kept in the store with the connector's records and committed with
each step, including the effects it has asked for that are not yet
settled and the wall times of what it waits for. It knows its task's
number, its delegates by number, and the facts it reads. A projection
(section 8) is not a procedure: no task asked for it.

### 6.2 When it steps

- when its task becomes active;
- when a message its owner lets through reaches its task: a delegate's
  result, an amendment, a cancel, an answer, news it subscribed to;
- when a fact it reads changes, which its connector knows from its
  working set;
- when a deadline its state names passes: a stall, a backoff, the end of
  what its task was asked to keep. The connector's top arms one deadline
  per procedure task, from the wall time its state keeps; the procedure
  arms none.

### 6.3 What a step decides

One decision, committed with its new state:

- effects, described and kept for the outbox (4.1);
- batches of delegates: agents for what needs judgement (a triage, a
  diagnosis, a repair), people for what needs a decision;
- amendments and cancels of its delegates;
- proposals, for what its task's authority does not cover, while the
  rest of the step commits;
- messages up: a question, an escalation, news its requester should
  have;
- a hold, when it needs a decision it cannot make;
- its result, which ends its task: in jig's terms (a report, a verdict),
  or a connector's result it keeps, with words summarising it (tasks.md,
  section 3).

Every effect and batch is checked against its task's authority, as any
decision is (authority.md, section 8); a procedure's authority is its
task's, given by its creator.

### 6.4 Level-triggered

A step decides from the facts as they are and its own state, never from
the event that woke it, so stepping twice on the same facts decides the
same, and a step lost to a restart is made again from what is read. It
never asks for an effect while one for the same purpose is outstanding
in its state, so deciding the same twice asks for nothing twice. What it
waits for (a service healthy, an environment ready, a verdict met) is a
condition on the facts, armed with a deadline past which it stalls.

### 6.5 Bounded

Each procedure declares its limits: remediations, retries, the stall of
each wait. Past one, it holds its task, saying why, and its requester
hears (tasks.md, 5.5).

### 6.6 Standing procedures

A procedure may stand: subscribed to topics, it makes tasks as news
arrives, and ends only when cancelled. `ops`'s watch is one: woken by a
batch of alerts, it makes a triage task for them, and an incident when a
triage says so. A standing procedure's tasks are its delegates, within
its authority, and its requester is usually the deployment or a person
on call.

## 7. Requirements and verdicts

- **A connector defines requirements** on its own facts, each with the
  parameters it takes: observability's *healthy replica elsewhere*, *load
  below a level for a time*; a forge's *CI passed at this head*, *up to
  date with the base*. The policy names them as judges of other kinds of
  effect (authority.md, section 10), and the judging connector keeps
  their parameters as its own configuration.
- **Guarded or observed.** A requirement is *guarded* when the effect's
  own system checks it as it applies the effect, as the effect's
  condition (4.1): a branch's head, a service's version. It holds when
  the effect is applied. Every other requirement is *observed:* judged
  when the effect is decided, by whichever connector owns its facts,
  within a freshness the policy gives it, and it may stop holding before
  the effect is applied, as load may rise before a scale-down. The
  policy may mark a requirement as one that must be guarded; an effect
  kind whose system cannot guard it is then refused (authority.md,
  section 10).
- **Asked for a verdict,** the judge is given the requirement, the
  effect's resource names and the state the effect names. It finds the
  subject of its facts from those names, by its own configuration, and
  answers from its working set, in the same step:
  - *met:* its facts, read within the requirement's freshness, hold;
  - *wait:* its facts are unknown, older than the freshness, or pending,
    saying what it waits for; it reads afresh, and its news on the
    subject lets the asker ask again;
  - *refuse:* a fact failed, saying which.
- **Judged twice,** as the mechanics in the procedure that waits for its
  requirements, and as the rule at the effect (authority.md, section 10).
- **Pinned, where guarded.** A guarded verdict is for the exact state the
  effect names (a version, a head), which the effect's condition holds
  its system to as it applies it. An observed verdict promises only what
  its judge saw, within the freshness.

## 8. Projections

What a connector writes for people to read of a goal, such as an issue
on a forge or an incident page, is a projection: neither a task nor a
procedure, but a mechanism of the connector's, keyed to the goal.

- **Fed by the root:** the goal's state, its plan as its subtree's tasks
  and their phases, and its milestones as the tree's history records
  them, in the connector's own terms, whenever they change: a change to
  any task of the subtree, not only to the goal's own. Milestones come
  as they are recorded, each with an identity of its own, so the
  connector keeps what it has written and adds to it; the plan comes
  whole, within the tree's limits (tasks.md, section 11). The last
  feed, as the goal closes, is its closing state.
- **Where:** in the project's home for that connector (core.md, 6.1).
  A project with no home there has no projection there.
- **Its effects** have keys of their own (4.4), are checked against the
  authority the project's policy gives its goals' projections, and are
  made through the outbox like any other.
- **Its state** (what it last wrote, by a digest) is kept with the goal,
  and outlives it until its last write, made as the goal closes, has
  settled.

## 9. Reads and brief sections

- **Reads an agent may call** (engine.md, 7.3): typed, each with an
  answer bounded in bytes and charged to the connector's request budget:
  a service's logs in a window, a metric's series, a pull request's
  diff. The call reaches the root decoded as the connector's request; the
  core checks the read grant (authority.md, 8.5); the connector serves
  it; its answer goes out through the journal's door for outputs that
  decide nothing (root.md, section 3). The core never holds it.
- **Sections a brief carries** for the resources a task names: a
  service's recent errors, an environment's state, a pull request
  against its base. The brief child domain plans them (engine.md,
  section 9) by kind and budget; the connector gathers each, keeps it,
  and says its size; asked to cut it to a smaller size, it cuts it by its
  own rules and says the new size; when the brief is complete, it hands
  the section to the root, which puts the assignment together (root.md,
  section 7).
- **A connector's result** reaches a dependent's brief the same way: the
  brief asks the connector for its section on that ended task's result.
- **Neither is state.** What a run reads is not kept by the connector
  beyond its answer, and a section beyond the brief it was gathered for.

## 10. Workspaces and what runs leave

- **What a run needs prepared** comes from the resources its task names.
  For each such resource, the connector that owns it says what the host
  is to prepare, in its own terms: a repository's checkout, where it
  starts and whether it may be written; nothing, for a resource reached
  through tools. These workspace items are handed to the root with the
  assignment (root.md, section 7), and the application's workspace child
  on the host prepares them (hosts.md, section 7).
- **Context resources** of the project may be added read-only, as the
  connector says.
- **What a run left** on a connector's resources, as its host reports it
  in the run's answer (the heads it pushed, the state it saved), reaches
  that connector through the root: its writer slot's provisional moves
  are confirmed (section 11), its procedures step.
- **Saved state.** A run that parks, fails or is cancelled with
  unfinished work has it saved by its host where the connector says; the
  task keeps which resources have saved state (tasks.md, section 3), and
  the next run's workspace items start from it.

## 11. Ownership and drift

- **Owned objects are recorded by key** (4.4), so ownership is the
  store's, whatever the system shows.
- **Projections are written, never read back** (core.md, 8.1).
- **The application's own moves.** A held resource moves under the
  application's own hand in two ways: an effect the connector made,
  known by its key and its outcome, and a run's write, which the engine
  hears of only with the run's answer. While a resource's writer slot is
  taken, a move by the application's identity that the connector can
  tell is the writer's (a branch moving forward) is taken as the
  writer's, provisionally, and confirmed by what the run's answer
  reports, or, if its attempt is settled as lost, by a fresh read then.
  A procedure decides nothing on a resource while its writer slot is
  taken.
- **Drift is a change the application did not make** to something it
  owns and relies on: a service restarted by another hand, an environment
  deleted, a branch moved by another identity or backwards, an object
  closed or retargeted. It holds the task that relies on it, saying what
  changed and when, and the client shows it; nothing is written over.
- **What is not drift:** a fact a decision reads is simply read again;
  a change to what others own is news.
- **Participating objects** are those others write too. What others
  write there is read into the inbox of the task that deals with the
  object, as news on its topic; what the application writes there is a
  participant's, possibly a proposal for a person to accept first.

## 12. Adopting a system

- **Adoption** adds a resource to a project with a role, by a party
  whose role allows it (people.md, section 5). The connector reads what
  the application may do there and keeps it with the project, and the
  project's ceiling follows from it (authority.md, section 6). It may
  also say which parties it knows there, with their permission, from
  which the project's policy may seed roles (people.md, section 4).
- **Nothing is required** of the system beyond the access the
  application is given. What is missing narrows what may be done there
  or slows it down, and the client says so: a system without hints is
  kept up by polling alone.
- **Provisioning** a system's objects (the system's own setup, not a
  task's resources) is an optional set of effects a party may ask for,
  never a step of adoption.
- **One deployment's.** The prefix under which a connector names what it
  creates is the deployment's; adoption refuses a resource where another
  deployment's objects already live under that prefix. Objects under the
  prefix that the store records no making of are another deployment's,
  or those of a store made anew's predecessor: the resource is refused,
  saying which, until a person chooses another prefix or clears them.
- **What may be done is read again** when the system refuses something
  the connector believed it could, and the project's ceiling narrows
  with it, for every later decision, existing tasks' included
  (authority.md, section 6).

## 13. Load

Each connector keeps its system's request budget:

- **Fresh reads first,** verdicts' and effects' own reads among them,
  then effects, then keeping up, then slow passes, with a share of each
  window kept for the last two.
- **Load follows change, never history:** what a connector asks of its
  system per pass grows with what changed and with the live resources,
  never with the system's past, and its worlds count it.
- **A refusal for the rate** stops every call to that system until its
  reset.

## 14. Restart

At a restart (engine.md, section 6), a connector is given, when the core
asks for each step:

1. **its records:** its live resources, its procedures' states and
   parameters, its outbox, its projections' states, what it made by key,
   and its own state, loaded from its live ranges;
2. **a fresh read:** it reads afresh what its system owns of its live
   resources, within its budget, before any of its procedures steps;
3. **its outbox to settle:** it looks for every entry not committed as
   made, by its key or the state it leads to (4.3).

Then, as decisions open, it steps each of its procedures once, on what
it read. Nothing it held in memory before the restart is needed.

## 15. The world

Each connector has its worlds: its subtree's, with its system's fake
below it and the root scripted above it; and the application's, where it
runs with the core and every other connector on jig's conformance world
(testing.md, section 5). What every connector's world checks, beyond its
own stories: keyed effects made once across restarts at drawn moments,
uncertain entries resolved as their recovery class says, with copies
arriving late, after their deadline and after a retry, and targets that
another hand reached too; deadlines that survive restarts made before
they pass; procedures that decide the same when stepped twice on the
same facts; verdicts that never say met on facts older than their
freshness; facts that change between a verdict and its effect; pools
that shrink while held and while waited for; topics whose news reaches
every subscriber once, classified; values it keeps for the root handed
over once, or dropped when the core drops their token; and a load that
follows change, the system preloaded with ten times the history making
the same calls while idle.

## 16. Open questions

- **Connectors added without a build:** the set is closed in the code,
  as the programming model's rules make it. A connector whose behaviour
  is data (a generic HTTP system) would be one connector of that kind,
  not a way around the closed set.
- **Classifying news with an LLM:** whether a cheap model should judge
  relevance where a connector's rules do not tell (tasks.md,
  section 13).
- **Freshness defaults:** how fresh each kind of observed requirement
  must be by default, before a policy says.
