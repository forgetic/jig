# jig's domain

Provisional, 2026-10-07. The design of jig's domain layer: the core an
application's engine composes, the contract its connectors meet, what
its root does, where its agents run, and how all of it is tested. jig as
a whole, and why it is a kit, is `../README.md`; the client's domain is
`../client/domain/`, designed separately. The mechanics are those of
skein's `programming-model.md`.

## 1. Reading order

1. **core.md:** the model: the five primitives, decisions and the store,
   the owner keeping the value, projects and goals, plans and
   coordinators, applications and their systems, parties, and what jig
   promises. Read it first.
2. **tasks.md:** the hub: tasks, batches, lifecycle, holds, inboxes and
   wake policies, proposals and escalations.
3. **authority.md:** what a task may do: authority as a value, budgets
   and spend, checks, proposals, requirements and their verdicts.
4. **connectors.md:** the contract every connector meets, and what
   crosses between it and the core: jig's API toward applications.
5. **engine.md:** the core as a whole: its children, its vocabulary
   toward the root, commits and the store, the restart script, agents'
   runs and the engine's tools, the fleet, briefs, notes, views,
   accounts.
6. **root.md:** what an application's root does: the one shape, the
   journal, routing, translation, putting outputs together, and what it
   never does.
7. **people.md:** parties: identity, roles, requests, inboxes, chats,
   person tasks, notifications.
8. **hosts.md:** agents and their hosts: the contract, the three shapes,
   the agent's two kinds, the one host for runs wherever their agents
   run, workspaces, and what the core takes from smith.
9. **testing.md:** the worlds, the conformance world and its referee.

`../examples.md` is the example application, `ops`, which each document
uses for its examples.

## 2. Structure

```
an application's engine
<application>-domain            the root: the application's (root.md)
├── jig-core                    the core (engine.md)
│   ├── jig-core-tasks          the hub (tasks.md)
│   ├── jig-core-authority      policy (authority.md)
│   ├── jig-core-people         parties (people.md)
│   ├── jig-core-fleet          hosts, placement, attempts
│   ├── jig-core-accounts       LLM accounts
│   ├── jig-core-brief          briefs
│   ├── jig-core-notes          notes
│   └── jig-core-views          live streams
├── <connector>…                the application's (connectors.md)
├── jig-host                    hosted runs on the engine's slots, if it runs agents (hosts.md)
├── jig-inline-agent            their agent: smith's domains in this process
└── …                           the application's own child domains

an application's worker, if it has workers
<application>-worker            the worker's root: the application's
├── jig-host                    hosted runs (hosts.md)
├── smith-host-domain           their agent: processes, smith's
└── <workspace>                 the application's

skein-lib's journal             the commit barrier every root holds (root.md, section 4)
```

The crate graph is the role graph: no jig crate depends on an
application's; the core and its children depend on skein-lib alone; the
hub depends on skein-lib alone too, and only the inline agent and the
charter's translation link smith; nothing in jig names a system.

## 3. Conventions

- **A bare file name** names a document of this directory: `tasks.md`.
  jig's other documents are named by their path from here
  (`../README.md`, `../examples.md`).
- **Code cites a section** at module or type level, as `domain/tasks.md`,
  6.2, never stamped on every field.
- **skein's foundation documents** are named by their own file names:
  `programming-model.md`, `testing-strategy.md`, `notes.md`. skein's
  design documents are named with skein's name: skein's `lib.md`.
- **smith's documents,** in smith's `docs/design/domain/`, are named with
  smith's name: smith's `run.md`.
- **An application** appears only as an example, and says so: `ops`
  most often, temper where a second example helps.
- **Each document** says in one page what it is, then its structure, its
  parts, what it owes and is owed below the domain, its world, and what
  is open.

## 4. Names

| Name | What it is |
|---|---|
| application | a program built on jig: its roots, its connectors, its workspaces, its client's objects |
| deployment | one engine, its store, its hosts, its configuration; an id made at its first start |
| engine | the application's process where work is decided: its root, jig's core, its connectors |
| root | the application's top-level domain in a process (root.md) |
| core | jig's mid-level domain inside the engine (engine.md) |
| journal | skein-lib's container that numbers commits and holds outputs until durable (root.md, section 4) |
| decision | what one event changes and asks for, gathered into one commit |
| hand-off | a question the core asks a connector within one decision, through the root |
| the door | the journal's way out for outputs that decide nothing |
| project | resources with roles, homes, a policy, parties' roles, notes (core.md, 6.1) |
| task | a unit of work someone asked for (core.md, 3.1) |
| goal | a tracked task: a priority, projections, goals' topics, a notes scope (core.md, 6.2) |
| plan | the subtree of tasks one task delegated (core.md, 7.1) |
| batch | the tasks one decision makes, checked whole (tasks.md, section 4) |
| input | an ended task's result a new task names |
| closing | a task settling before it ends: its run, its delegates, its effects, its releases |
| executor | who carries a task out: an agent, a procedure, a party |
| charter | how an agent executor is set up, and which hosts may run it |
| result contract | what counts as done: a report, a verdict, a connector's result, a delivery, a choice |
| chat | an agent task whose requester is a person (people.md, section 7) |
| coordinator | an agent whose charter is to delegate and supervise |
| standing procedure | a procedure subscribed to topics that makes tasks as news arrives (connectors.md, 6.6) |
| run, attempt | one activation of an agent task on a host; one try at it, fenced by a number that only grows |
| tries | the failed attempts counted per class, which a release resets |
| transcript, turn | an agent task's conversation, kept turn by turn; one LLM turn, committed as it ends |
| call | a run's tool call, named by the run, decided once |
| brief | the context a run is given for why it runs |
| message, inbox | what a task hears once started; its durable, bounded store of them |
| wake policy | which messages wake a task, and how they batch |
| subscription, topic | a task's standing interest; a connector's subject |
| reference | a task another may message |
| party | a person or a service that takes part (people.md) |
| provider | one of the application's ways of signing in |
| a party's role | what a party may do in a project: owner, maintainer, member, observer, or the policy's own |
| a resource's role | owned, fork, or context, in a project |
| authority | what a task may do (authority.md, section 3) |
| grant | an effect or a read of a connector's kind on a pattern of resources |
| fit | whether a delegate's authority goes under its creator's, given what it has left |
| funder | who a task's budget was reserved from: a task, a party's pool, a project's period |
| price | what a priced effect costs at most, in the deployment's unit |
| effective authority | a task's authority within the rules, policy and adoption access as they stand when it decides |
| proposal, escalation | an action beyond its proposer's authority; a held task; each waiting for a holder |
| rules, policy | the deployment's, a project's: the root of authority |
| requirement, judge, verdict | facts an effect needs; the connector that judges them; met, wait or refuse |
| guarded, observed | a requirement the effect's own system checks as it applies it; one judged only when the effect is decided, within a freshness |
| connector | an external system as the engine sees it: the application's |
| resource | what has an identity on a system, named by a connector's number and a path |
| hold, pool slot | a resource a task writes alone; one of a pool's counted slots (tasks.md, section 6) |
| writer slot | the one run that may write a held resource at a time |
| effect, outbox, key | what the application does on a system; its entries waiting to be made; what finds them again |
| recovery class | how an effect of a kind whose outcome is uncertain is resolved: keyed, conditional, idempotent, unrecoverable (connectors.md, 4.3) |
| retry deadline | when an uncertain attempt may be tried again, committed with the attempt |
| procedure | engine code as an executor, a connector's or the core's |
| projection, home | what a connector writes of a goal for people to read; where, per project |
| owned, participating | what the application writes alone; what it writes among others |
| drift | a change the application did not make to what it owns and relies on |
| host, hub | where a run runs: the engine's own slots, or a worker; `jig-host`, the one hub hosting runs on either (hosts.md, section 6) |
| inline agent | smith's domains run in the engine's process, the agent capability of the engine's hub (hosts.md, 5.2) |
| workspace, item | what a run works in; what a connector names for it to hold |
| workstream | the key a workspace is cached under: the holding task's number |
| delivery | a run's hand-over through its workspace, made by the application's workspace |
| saved state | unfinished work a run's host saved, which the next run starts from |
| token | a name the core holds for a value a connector keeps (core.md, section 5) |
| conformance world | the world every application runs its domain on, with jig's referee (testing.md, section 5) |
| test connector | jig's connector for its own worlds |
| smith | the agent, and the kit it is built with, in its own repository |
| session | an agent's conversation with an LLM: smith's |

## 5. Decisions this design takes

Questions `../README.md` left open, and others met while writing, with
where each is answered:

- **Payloads the core orders but does not read:** the owner keeps the
  value; the core holds tokens and sizes, and the root puts outputs
  together (core.md, section 5; root.md, section 7).
- **Requirements across connectors:** requirements are judged by the
  connector whose facts they are, as verdicts the core asks for through
  the root, whichever connector makes the effect (authority.md,
  section 10; connectors.md, section 7).
- **Scarce resources:** holds are exclusive or pooled, and a taken one
  refuses or waits, as its connector says; a task takes every hold at
  once or waits with none (tasks.md, section 6).
- **Parties that are not people:** a party is a person or a service; a
  service signs in with a credential the deployment issues, holds roles,
  and makes requests, but no chat (people.md, section 3).
- **Sign-in providers:** the application's; the core knows an identity as
  a provider and a subject (people.md, section 3).
- **Hosting runs inside the engine:** the same hub as on a worker, a
  child of the application's root, whose slots the fleet places on like
  a worker's, with the inline agent as its agent (hosts.md, sections 5
  and 6; engine.md, section 8).
- **The worker:** jig ships the hub; the worker's root and its
  workspaces are the application's (hosts.md, sections 4 and 7).
- **Costs beyond LLMs:** a connector may price kinds of effect, charged
  at the decision (authority.md, section 7).
- **An effect waiting for a verdict:** never committed; the asker asks
  again (connectors.md, 4.5).
- **Restart:** a script the core runs, step by step, through the root
  (engine.md, section 6; root.md, section 8).

Decisions taken after the design's first review (2026-10-07):

- **Once, by recovery class:** each kind of effect is keyed, conditional,
  idempotent or unrecoverable, as its system allows, and the promise of
  once is stated per class; an uncertain unrecoverable effect holds its
  task for a person (connectors.md, 4.3; core.md, section 11).
- **Retry deadlines are absolute,** committed with each attempt before it
  is sent, so restarts never postpone an uncertain effect for ever
  (connectors.md, 4.3).
- **Guarded and observed requirements:** a guarded one holds when its
  effect is applied, an observed one when it is decided, within a
  freshness; a policy may require a guard (authority.md, section 10;
  connectors.md, section 7).
- **A hard ceiling on recorded spend:** a run reserves each completion's
  maximum before making it, a priced effect is charged its maximum; only
  turns lost with a worker escape it, within a stated bound
  (authority.md, section 7). It asks a change of smith's `run.md`,
  section 9 (hosts.md, section 8).
- **Standing work renews by period:** a standing task's allotment of
  tasks and spend is carved again each period (authority.md, section 7).
- **Pools that shrink** keep their holders and admit nobody until they
  drain; the promise is about admission (tasks.md, 6.6).
- **Effective authority:** a narrowing of rules, policy or access reaches
  every later decision, existing tasks' included; committed effects are
  made; pending proposals are judged at acceptance (authority.md,
  section 6).
- **Committed turns survive;** what a worker loses before a commit is
  done again by the next attempt (hosts.md, section 1).
- **What the conformance world catches** is stated as what its scenarios
  reach, with fakes reporting what they observed independently of the
  root's translation (root.md, section 1; testing.md, section 5).
- **One host:** runs are hosted by one hub, `jig-host`, in the engine and
  on workers alike, its agent a capability of two kinds (processes, or
  the inline agent in the engine), its link to the core its root's. The
  run's lifecycle is built and tested once (hosts.md, sections 1 and 4
  to 6).

## 6. What is open

Each document lists its own; the ones that reach across:

- the deployment's unit of spend, and costs over time (authority.md);
- generating a root's boilerplate rather than copying it (root.md);
- several roots in one process (root.md);
- fairness for runs on the engine's loop (hosts.md);
- how fresh a verdict's facts must be (connectors.md);
- how many commits the focused suite crashes at (testing.md).
