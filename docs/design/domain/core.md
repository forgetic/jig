# The model

Provisional, 2026-10-07. What jig's domain is beneath its parts: an
engine core that runs work done by agents, procedures and people, keeps
it in its own store, and drives external systems through an
application's connectors. Every other document in this directory builds
on it, and it is the first to read (README.md gives the order and the
names). The mechanics are those of skein's `programming-model.md`. What
is still open is listed in section 12.

## 1. In one page

- **A generic agentic engine.** jig runs work: it keeps what was asked,
  who does it and what came of it; it runs agents on hosts, steps
  procedures, asks people, and routes to each what it needs to hear. It
  is to be as flexible as a coding agent at a terminal, with what such an
  agent lacks: work that outlives processes and machines, many agents at
  once, rules no agent can loosen, and effects on other systems made
  once.
- **Five primitives** (section 3): the **task**, what is asked; the
  **executor**, who does it: an agent, a procedure or a person; the
  **message**, everything a task hears once it has started;
  **authority**, what a task may do, narrowed whenever it delegates; and
  the **connector**, an external system. Everything else is built from
  them: plans, coordinators, chats, goals, reviews, incidents.
- **Structure belongs to the work.** A plan is the tasks one task has
  delegated, and a coordinator is an agent whose charter is to delegate
  and supervise (section 7). Agents make structure with tools, as a
  coding agent spawns sub-agents; the core makes it durable, schedules
  it, and holds it to its authority.
- **The core owns its state.** Projects, tasks, their inboxes and
  results, transcripts, proposals, notes and the outbox live in the
  application's store, which only its engine reads and writes. The engine
  holds bounded working sets, rebuilt from the store and the connectors
  after a restart. A connector's system keeps what is the system's.
- **A decision commits once** (section 4): its change of state and the
  effects it asks for, in one commit. Nothing leaves the engine before
  the commit it follows from is durable. Effects are made from the
  outbox, keyed, so a write repeated after an uncertain failure finds
  what the first made.
- **The owner keeps the value** (section 5). The core holds what it acts
  on, and nothing of a connector's vocabulary: it names a connector's
  values by token and size, and the application's root puts each output
  together from its owners' parts.
- **External systems are the application's connectors.** Each gives
  resources, effects, events, procedures, requirements' verdicts, reads
  and brief sections, and the core knows none of them but by number
  (connectors.md). The application's root knows them all.
- **What the application does on a system belongs to it** (section 8).
  It writes only to what it owns, and takes part as one participant in
  what others own. Anything else that changes is drift, found by reading
  afresh and reconciled, never coordinated with.
- **People are parties** (section 9). They talk to agents, set goals,
  accept proposals, answer questions, steer and watch, through a client.
- **Mechanics are code, judgement is an LLM's.** Procedures carry out
  what needs no judgement. An LLM is woken for what does.
- **Bounded, and complete in its domain.** What the engine holds grows
  with the work in progress, never with history. Every entity has a
  limit, refused at the entrance. A world of domains and fakes runs
  everything the core does, with no protocol and no io
  (programming-model.md, section 4).

## 2. The system

```
people    a client: chats, goals, proposals to accept, questions to answer, watching
engine    the application's process: its root, jig's core, its connectors; the store's only client
store     the application's durable storage: jig's records and its connectors'
hosts     where agents run: inside the engine, or on workers that dial it
agents    smith: LLM work, one run at a time per host slot
systems   what the application's connectors drive
```

- **One engine per deployment.** A deployment is one engine, its store,
  its hosts and its configuration, with an id made at its first start.
  The engine is the only writer of the store and the only client of
  every connector's system, so it never needs a compare-and-swap against
  itself. Scaling out is a later problem.
- **The engine decides; hosts host; agents think.** Agents reach the
  engine only through tools their host relays (hosts.md).
- **The store is assumed.** It keeps durably what the engine gives it,
  commits several changes as one, applies commits in order, and answers
  bounded reads (engine.md, section 5).
- **People reach the engine through a client** of the application's,
  built on jig's client (people.md).

## 3. The five primitives

### 3.1 Task

A unit of work someone asked for (tasks.md):

- **its spec,** in words and in typed parameters, and its **result
  contract**, which says what counts as done: a report; a verdict from a
  closed list, with a contract per verdict; a connector's result, which
  its procedure produces; a person's choice;
- **its requester:** the task, the person or the deployment that asked
  for it. Requesters link tasks into a tree, rooted at a party or at the
  deployment;
- **its dependencies:** the tasks it starts after. A dependency means
  only "after it is done", and its result reaches the task: an agent's
  brief carries it. Dependencies link tasks into a graph without cycles;
- **its executor** (3.2), **its authority** (3.4) and **its wake
  policy** (3.3);
- **its write holds:** the resources it writes alone (tasks.md,
  section 6);
- **its lifecycle:** waiting, active, closing, then done, failed or
  cancelled; held for a decision at any point before it ends;
- **its result,** delivered to its requester as a message, and kept.

Tasks are created in **batches.** A batch, with the dependencies among
its tasks, is checked whole and made at once, so that a plan is never
half made.

### 3.2 Executor

Who carries a task out. A task has one:

- **An agent:** an LLM, set up by a charter: instructions, models,
  tools, the resources its workspace holds, the outcome it must produce.
  Its transcript belongs to its task and is kept turn by turn in the
  store. It is activated as a run on a host, which ends by parking or
  finishing; between runs, nothing of it is live (engine.md, section 7).
- **A procedure:** engine code, pure: given the task's state, what it has
  heard and the facts it reads afresh, it says which effects to make,
  which tasks to create, amend or cancel, and whether it is done.
  Procedures belong to connectors (connectors.md, section 6), or to the
  core, such as a recurring task (tasks.md, section 10). They cost no LLM
  turns.
- **A person:** a party who answers through a client: a choice, a
  decision, an approval, an answer (people.md, section 8).

All three can delegate, each within its authority: an agent with its
tools, a procedure in its code, a person through the client.

### 3.3 Message

Everything a task hears once it has started:

- results and questions from the tasks it delegated;
- amendments, cancels, answers and decisions from its requester;
- a person's words;
- news from what it subscribed to: a connector's events, another task's
  state, a timer.

What waits for its decision, a proposal or an escalation from below, is
not a message but an entry of its own, which always wakes it (tasks.md,
section 9).

Each task has an inbox, durable in the store, and a **wake policy**,
which decides when its executor runs for what has arrived: a run for an
agent, a step for a procedure, a notification for a person. A wake costs
an agent an LLM turn, so news is kept, unless its connector finds it is
not the task's concern, and only some of it wakes. What does not wake a
task waits in its inbox, and its next run hears it (tasks.md, section 8).

### 3.4 Authority

What a task may do (authority.md):

- the tools its agent has, and the effects and reads it may make, on
  which resources: restart which services, write which branches, create
  what where;
- what it may spend: LLM completions and priced effects, in the
  deployment's unit, and time;
- what it may delegate: which kinds of task, how many, how deep, with
  what authority.

**Delegation only narrows it:** a task gives its delegates at most what
it has, and what it gives of its budget is no longer its own to spend.
The root of every tree is the deployment's rules and the project's
policy, which no task loosens.

**Beyond its authority, a task proposes.** The action becomes a proposal
to whoever holds the authority, up the tree: a task above it, or a
person. They accept it, and it is made with their authority, or reject
it, and the task hears why. Acceptance, escalation and widening a plan's
reach are all this one mechanism.

### 3.5 Connector

An external system, as the engine sees it (connectors.md):

- **resources:** what has an identity there (a service, an environment,
  a repository, a branch), named by paths, which a task names and a
  workspace may hold. A resource a task writes is held by that task
  alone while it writes, or waits its turn for a pool's slot;
- **effects:** what the application does there, made through the outbox
  (section 4), and priced when they cost;
- **events:** what changed there, delivered to the tasks that
  subscribed. They are hints: the state they point at is read afresh;
- **procedures:** the mechanics of working there, as executors (3.2);
- **requirements:** the facts an effect needs, judged by the connector
  whose facts they are, as a verdict;
- **reads and brief sections:** what an agent may read of it, and what a
  brief carries about it.

The set of connectors is closed in each application: each is a child of
its root, which knows them all. The core knows them by number, and its
tasks, inboxes and authority know none of them.

## 4. Decisions, the store and the outbox

- **A decision commits once.** A run's answer, a procedure's step, a
  person's request, a tool call: each is decided against the engine's
  state, and what it changes in every part it touched (tasks made, a
  result kept, an inbox read, a proposal accepted, an outbox entry
  added) and the effects it asks for go to the store in one commit.
- **Nothing leaves before its commit.** An effect is made, a run is
  assigned, a host's answer is acknowledged and a person is answered only
  once the commit it follows from is durable. The engine's state may run
  ahead of the store by the commits in flight, which are bounded; if the
  engine dies meanwhile, nothing anyone saw depended on what was lost,
  and a restart starts from the last commit.
- **The journal holds it.** The application's root gathers each
  decision's writes and outputs into one decision of skein-lib's journal,
  which numbers the commit and releases the outputs once it is durable
  (root.md). The core decides what each output waits for; the journal
  keeps it waiting.
- **The outbox makes each effect, keyed.** An effect committed is an
  entry in its connector's outbox, with a key derived from what the
  effect is for, never from when it was asked. Its connector makes it; a
  creation carries its key where the system keeps it, so a write repeated
  after an uncertain failure finds what the first made (connectors.md,
  section 4).
- **Facts are read afresh.** What a connector's system owns is read again
  before a decision that depends on it, as level-triggered state. Events
  are hints that something changed, never the state itself.
- **A restart reads the store,** then has each connector read afresh what
  its system owns of the live work, and look for what uncertain effects
  may have made, before deciding anything new. The core runs it as a
  script (engine.md, section 6).
- **A commit that fails stops the engine.** The store is assumed to do
  what the engine needs; when it cannot, the engine stops rather than act
  on state it could not keep, and a restart starts from the last commit.

## 5. The owner keeps the value

The core and the application's connectors are siblings under the root,
and siblings share no types (programming-model.md, 4.5). So:

- **The core holds what it acts on,** in its own vocabulary: tasks,
  inboxes, authority, budgets, holds, transcripts as bytes it never reads
  inside, its tools' records.
- **What it orders but does not read, it names.** A brief section a
  connector gathers, the answer to a connector's read, a procedure's
  parameters, a connector's result, what a run left on a connector's
  resources, a workspace's items: each stays with the connector it
  belongs to, keyed by the task's number, a run's attempt, a call's name
  or a token. The core holds the key and, where it budgets, the declared
  size.
- **The root puts outputs together.** An assignment, a call's answer, a
  brief: each is made, in the step the core decides it, from the core's
  part and the parts each connector hands over, and goes into the
  journal whole (root.md, section 7).
- **Values arrive typed,** decoded by the application's protocol layer
  into the vocabulary of whoever owns them: an agent's call for a
  connector's read reaches the root already as that connector's request.

This is skein's binding rule (programming-model.md, 4.2: bindings are
echoed tokens) applied between siblings: the core echoes what it does
not own.

## 6. Projects and goals

### 6.1 Projects

A project is a body of work people steer, kept in the store:

- **its resources,** each adopted from a connector with a role: *owned*
  or adopted to write into; a *fork* of what others own, where the
  application owns its copy and takes part upstream; or *context*, read
  and never written (connectors.md, section 12). A project's resources
  may span connectors and systems;
- **its homes:** for each connector that projects goals, where it does
  (a repository whose issues show goals, a page that shows incidents);
- **its policy:** the most any task in the project may do, the
  requirements its effects need, its spend per period (authority.md,
  section 6);
- **its parties and their roles** (people.md, section 4);
- **its notes** (engine.md, section 10).

Every task belongs to one project, which its delegates inherit.

### 6.2 Goals

A goal is a task people track: the task a person asks for, or that a
chat proposes and a person accepts, marked so. It is a role a task takes,
not a sixth primitive. A goal a chat proposes is requested by the person
who accepts it, not by the chat, so it outlives the conversation it came
from (tasks.md, section 9). A tracked task gets:

- **a priority** among the project's goals, which connectors use to order
  what competes for one resource (a landing queue, a pool's slots);
- **projections:** what each connector that projects goals writes of it
  for people to read (connectors.md, section 8);
- **news** of what its subtree's work changed, through the topics its
  connectors offer for goals, which the core subscribes it to;
- **a scope for notes,** which its subtree reads and writes.

Any task may be tracked; most are not. A chat, a triage or a repair is
not a goal; an incident, or "give the payments team a staging
environment", may be.

## 7. Plans and coordinators

### 7.1 What a plan is

A plan is three things, kept apart:

1. **The strategy:** why this approach, in what order, what was dropped
   and why. It is text, and it is the LLM's: its transcript, its notes, a
   goal's projection. It changes freely.
2. **The structure:** tasks and their dependencies. It is the engine's
   state, and the engine runs it.
3. **The authority** to run it: the budget, the kinds and number of
   tasks, the resources. It is the task's authority (3.4).

So **a plan is the subtree of tasks one task has delegated.** Revising it
is creating, amending or cancelling tasks, each with its reason, which
the store keeps as the plan's history. Planning everything up front is
one batch (3.1): checked whole and, beyond its creator's authority, one
proposal for a person to accept.

### 7.2 What a coordinator does

A coordinator is an agent whose charter is to break a goal down,
delegate, watch its inbox, revise, and report. The core knows nothing
particular of it: it is an agent's task with delegates, woken by what
its wake policy lets through. It may be the chat a person talks to, or a
fresh agent the chat delegates the goal to, which keeps a long
conversation out of the supervision. The choice is the charter's.

A coordinator woken rarely and far apart starts fresh by default, from a
brief: its goal, the plan's state, its inbox and its notes. A chat
resumes its transcript, which the store keeps whole. Either is its
charter's choice (engine.md, 7.4).

### 7.3 Shapes of work

The same primitives make each of these:

1. **Done alone.** A person asks a chat a question, and it answers.
2. **Delegated once.** A chat's agent creates one task, hears its
   result, and tells the person.
3. **Handed to a procedure.** An agent decides what to do and delegates
   the doing to a connector's procedure: a restart verified by health, a
   change landed.
4. **A static plan.** Tasks with dependencies and no LLM over them.
5. **Coordinated.** An agent supervises and revises, for the goals that
   need it.
6. **Standing.** A procedure subscribed to a topic makes tasks as news
   arrives: a watch that triages alerts.
7. **Recurring.** A timer makes tasks from a template (tasks.md,
   section 10).

### 7.4 Two levels of delegation

An agent delegates in two ways, and chooses between them:

- **sub-agents within its run** (smith's `run.md`, 5.3): cheap,
  ephemeral, sharing its workspace and its budget, and ended with the
  call that asked for them;
- **tasks:** durable and scheduled, with an executor, a workspace and an
  authority of their own, surviving restarts.

An agent reaches the engine through tools its host relays: delegate a
batch of tasks; message; amend, cancel and release its delegates; decide
what they propose; propose; subscribe; make an effect; read a connector;
note and recall; wait; finish. Each is checked against its task's
authority (engine.md, 7.3).

## 8. Applications and external systems

### 8.1 What belongs to the application

- **The rule.** What the application does on an external system belongs
  to it: what it creates, and the changes it makes to what it owns. It
  writes only to what it owns, with two exceptions its connectors define:
  writing into shared resources through their own mechanics (landing
  into a shared branch, scaling a shared service within a policy), and
  taking part as one participant in objects others own (8.2).
- **Ownership is the store's.** Each connector records every object it
  made, with the key it made it under. On the system, ownership may also
  show (an author, a prefix, a label), but the store's record is what
  counts.
- **Projections are written, never read back.** What a connector writes
  for people to read is a projection of the store's state: keyed, made
  through the outbox, and never parsed back as state, beyond finding its
  own creations by their keys.
- **What the system owns is read afresh** before each decision that
  depends on it (section 4).

### 8.2 Owned and participating

Each object the application deals with is one of two kinds:

- **Owned,** the default: the application is its only writer. What comes
  back from it is only the system's facts.
- **Participating:** others write too. The connector reads what they
  write into the inbox of the task that deals with the object, and writes
  as one participant among several. A write that goes out may need a
  person's acceptance.

### 8.3 Plugging in

- **Systems are adopted as they are.** Making their objects is an
  optional convenience above that, never something the core assumes.
- **Nothing is required of a system** beyond the access the application
  is given. Policy is the enforcement; whatever protection a system has
  is a second net.
- **What the application may do there is read at adoption,** and the
  project's policy keeps within it (connectors.md, section 12).

### 8.4 Drift

Another party may change what the application relies on: someone
restarts a service by hand, deletes an environment, pushes to a branch
the application owns. No workflow is built on this, but it must be safe:

- facts are read afresh before each decision; effects carry the state
  they were decided on where the system can check it;
- a change the application did not make to what others own, such as a
  landing it did not make, is news like any other;
- a change to an object the application owns holds the task that relies
  on it, and the client says why. Nothing is written over
  (connectors.md, section 11).

## 9. People

People are parties, as tasks are. Each has an inbox, and creates,
messages, amends, cancels, releases and accepts within the authority
their role in a project gives them (people.md). A service may be a party
too, signed in with a credential the application gives it.

- **A chat** is an agent's task whose requester is a person: each of
  their words wakes it, and it resumes its transcript. It is how a person
  usually starts work: the chat answers, delegates, or proposes a goal.
- **A party's inbox** holds what waits for them: questions from the
  tasks they asked for, proposals they may accept, held tasks escalated
  to them, tasks whose executor they are, results.
- **The client is generic:** tasks as a tree, conversations, what waits
  for a party, and live runs to watch or stop. It knows the five
  primitives, not the shapes of work built from them. An application adds
  views of its own connectors' objects.

## 10. A goal, as tasks

In `ops`, jig's example application (`../examples.md`), alerts fire on
the checkout service in production. P is the person on call:

```
P, the person on call
└─ W   watch the shop's services         observability's watch, a procedure subscribed to alerts
   ├─ T1  triage alerts 7–9              agent in the engine, a cheap model: noise
   ├─ T2  triage alert 12                agent: incident
   └─ I   incident: checkout errors      agent, an incident lead's charter; tracked
      ├─ R1 roll back checkout           infrastructure's remediate; holds the checkout service
      │                                  in production: a proposal P accepted
      └─ Q  why did 4.2 fail?            person: a developer's answer, for I's report
```

- **W is standing** (7.3): a procedure, not an agent. A storm of alerts
  wakes it once; it makes a triage task per batch.
- **T1 and T2 are cheap,** and end with verdicts. Only T2's makes I.
- **I is a goal,** tracked: people follow it in the client, its priority
  orders its remediations against other incidents' for the same
  service, and its notes' scope keeps what was learnt.
- **R1 is beyond I's authority** in production, so it is a proposal. It
  passes over W, a procedure, to P, who accepts it with P's authority.
- **R1 is a procedure:** it holds the service, makes the rollback, and
  waits until the service is healthy, a fact the observability connector
  judges (connectors.md, section 7).
- **Q is a person task:** I asks a developer a question, and the answer
  reaches its report.
- **Had P closed W** while I ran, I would be cancelled with it; had P
  made I their own (tasks.md, section 7), it would go on.

## 11. What jig promises

The referees of jig's worlds hold the core, and every application's
wiring, to these, seen from outside (testing.md):

- **Authority holds.** No effect is made, no task created and no budget
  spent beyond what the deployment's rules, the project's policy and the
  chain of authority above the task allow, except by a proposal a holder
  of that authority accepted. No effect is made whose requirements'
  verdicts were not met.
- **Once.** Every keyed effect is made at most once, across restarts
  included; every task in a batch is made, or none is.
- **Nothing ahead of its commit.** Nothing a party was answered, a host
  was assigned or acknowledged, or a system was asked to do, depended on
  state the store did not keep. What people watch live may run ahead of
  it, as views are best effort.
- **Order.** A task starts only after its dependencies are done and
  closed; one run at most writes a resource at a time, and a pool never
  has more holders than its slots; an effect lands exactly the state its
  decision saw, where the system can check it; a task ends only after its
  delegates.
- **Nothing is lost.** A result reaches its requester; a person's words
  reach the task they were written to, or that task ends; a proposal is
  decided, withdrawn, or still waiting where a holder can see it.
- **Nothing is written over.** No effect writes to an object the
  application does not own except as a participant or through its
  connector's own mechanics, and none overwrites a change it did not
  make.
- **Bounded.** What the engine holds stays within its limits, whatever
  the history in the store; every story ends within a bound.

## 12. Open questions

- **Agents as identities** that serve many tasks, such as a service's
  standing operator. The model allows them; a long-lived task serves
  meanwhile.
- **Procedures written by agents,** deterministic workflows like a coding
  agent's scripts. Until they are wanted, procedures are Rust, owned by
  connectors or the core.
- **Wake policies:** what wakes a task, and whether a cheap model should
  triage news before a coordinator wakes. Tuned from use (tasks.md,
  section 13).
- **Scaling out:** one engine per deployment until it binds.
