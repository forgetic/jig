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
   the host inside the engine, the worker host, workspaces, and what the
   core takes from smith.
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
├── jig-local-host              agents in the engine, if any (hosts.md)
└── …                           the application's own child domains

an application's worker, if it has workers
<application>-worker            the worker's root: the application's
├── jig-worker-host             hosted runs (hosts.md)
├── smith-host-domain           agent processes, smith's
└── <workspace>                 the application's

skein-lib's journal             the commit barrier every root holds (root.md, section 4)
```

The crate graph is the role graph: no jig crate depends on an
application's; the core and its children depend on skein-lib alone; the
hosts depend on smith's domains; nothing in jig names a system.

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
| price | what a priced effect costs, in the deployment's unit |
| proposal, escalation | an action beyond its proposer's authority; a held task; each waiting for a holder |
| rules, policy | the deployment's, a project's: the root of authority |
| requirement, judge, verdict | facts an effect needs; the connector that judges them; met, wait or refuse |
| connector | an external system as the engine sees it: the application's |
| resource | what has an identity on a system, named by a connector's number and a path |
| hold, pool slot | a resource a task writes alone; one of a pool's counted slots (tasks.md, section 6) |
| writer slot | the one run that may write a held resource at a time |
| effect, outbox, key | what the application does on a system; its entries waiting to be made; what finds them again |
| procedure | engine code as an executor, a connector's or the core's |
| projection, home | what a connector writes of a goal for people to read; where, per project |
| owned, participating | what the application writes alone; what it writes among others |
| drift | a change the application did not make to what it owns and relies on |
| host | where a run runs: the local host in the engine, or a worker |
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
- **The host inside the engine:** jig's local host, a child of the
  application's root, whose slots the fleet places on like a worker's
  (hosts.md, section 5; engine.md, section 8).
- **The worker:** jig ships the worker host; the worker's root and its
  workspaces are the application's (hosts.md, sections 4 and 7).
- **Costs beyond LLMs:** a connector may price kinds of effect, charged
  at the decision (authority.md, section 7).
- **An effect waiting for a verdict:** never committed; the asker asks
  again (connectors.md, 4.5).
- **Restart:** a script the core runs, step by step, through the root
  (engine.md, section 6; root.md, section 8).

## 6. What is open

Each document lists its own; the ones that reach across:

- the deployment's unit of spend, and costs over time (authority.md);
- generating a root's boilerplate rather than copying it (root.md);
- several roots in one process (root.md);
- fairness for runs on the engine's loop (hosts.md);
- how fresh a verdict's facts must be (connectors.md);
- how many commits the focused suite crashes at (testing.md).
